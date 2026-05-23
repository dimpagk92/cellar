//! SQLite-backed rules and watchlists store for the Cellar daemon.
//!
//! Replaces `cel_act_gateway::traits::StaticRules` and
//! `cellar_types::InMemoryWatchlists` in the production daemon. Implements
//! both [`RuleSource`] and [`WatchlistLookup`] so the gateway and the
//! matcher consumer task can share a single store via `Arc`.
//!
//! # Hot reload
//!
//! The store keeps two in-memory snapshots that are kept in sync with the
//! database on every mutation:
//!
//! - `rules: RwLock<Vec<Rule>>` — what [`RuleSource::snapshot`] returns.
//! - `watchlists: RwLock<HashMap<String, BTreeSet<String>>>` — what
//!   [`WatchlistLookup::contains`] consults.
//!
//! Mutations (`create_rule`, `delete_watchlist_item`, etc.) take the DB
//! mutex, run the SQL, then update the in-memory snapshot atomically.
//! Read paths only ever touch the in-memory snapshots — no DB lock — so
//! the matcher's hot path stays uncontended.
//!
//! The matcher and gateway each hold their own clone of `Arc<SqliteRulesStore>`,
//! so a write through any pathway is immediately visible to all readers
//! without an explicit reload signal.
//!
//! # Scope (v1 Phase 2 Slice 2a)
//!
//! - Rules and watchlists CRUD.
//! - One SQLite file (or `:memory:` for tests).
//! - Schema version 1 baked in; the [`schema_version`] tracking is in place
//!   for forward migrations but no migrations exist yet.
//!
//! Not in this slice (separate follow-ups):
//!
//! - IPC `rules.*` / `watchlists.*` methods (Slice 2b — wires this store
//!   behind the locked IPC surface).
//! - Cooldown enforcement (Phase 2 plan item).
//! - Cross-process change notification (only relevant if a CLI ever writes
//!   directly to the DB while the daemon runs).

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use cel_act_gateway::{CooldownPersistence, RuleSource};
use cellar_types::matcher::WatchlistLookup;
use cellar_types::{Rule, Watchlist, WebhookConfig};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

/// Errors from the rules store.
///
/// Use [`Self::is_unique_constraint_violation`] for "row already exists"
/// detection (duplicate rule id, duplicate watchlist name). Helpers like
/// these keep callers from having to import `rusqlite` just to classify
/// errors.
#[derive(Debug, Error)]
pub enum RulesStoreError {
    /// Underlying SQLite operation failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem error (e.g. creating the parent directory for the DB file).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Serialization failure marshalling a rule / watchlist to or from JSON.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Schema is at a version this build doesn't recognize.
    #[error("unsupported schema version {found}; this build supports up to {supported}")]
    UnsupportedSchemaVersion {
        /// Version found in the database.
        found: i64,
        /// Highest version this build's migrations know how to produce.
        supported: i64,
    },
}

impl RulesStoreError {
    /// True if this error reflects a uniqueness violation — primary key or
    /// UNIQUE constraint. Used by callers to distinguish "you tried to
    /// insert a row that's already there" from other failures.
    pub fn is_unique_constraint_violation(&self) -> bool {
        matches!(
            self,
            RulesStoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation
        )
    }
}

/// Current schema version. Increment when adding a forward migration in
/// [`SqliteRulesStore::run_migrations`].
///
/// v1: rules + watchlists + watchlist_items.
/// v2: webhooks (one row per `WebhookConfig`, JSON payload).
/// v3: rule_cooldowns — per-rule last-fire timestamps for cooldown
///     persistence across daemon restarts (see
///     [`cel_act_gateway::CooldownPersistence`]).
const SCHEMA_VERSION: i64 = 3;

/// SQLite-backed store. Implements [`RuleSource`] + [`WatchlistLookup`].
///
/// Construct via [`Self::open`] (file-backed) or [`Self::in_memory`] (tests).
/// Wrap in `Arc` immediately — the daemon holds one `Arc<SqliteRulesStore>`
/// and clones it into the gateway and the matcher task.
pub struct SqliteRulesStore {
    db: Mutex<Connection>,
    rules: RwLock<Vec<Rule>>,
    watchlists: RwLock<HashMap<String, BTreeSet<String>>>,
    webhooks: RwLock<HashMap<String, WebhookConfig>>,
}

impl std::fmt::Debug for SqliteRulesStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rules = self.rules.read().map(|r| r.len()).unwrap_or(0);
        let watchlists = self.watchlists.read().map(|w| w.len()).unwrap_or(0);
        let webhooks = self.webhooks.read().map(|w| w.len()).unwrap_or(0);
        f.debug_struct("SqliteRulesStore")
            .field("rules_cached", &rules)
            .field("watchlists_cached", &watchlists)
            .field("webhooks_cached", &webhooks)
            .finish()
    }
}

impl SqliteRulesStore {
    /// Open a file-backed store. The file is created if missing, and
    /// schema is initialised / migrated to the current version.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, RulesStoreError> {
        let conn = Connection::open(path.as_ref())?;
        Self::from_conn(conn)
    }

    /// In-memory store for tests. Each call yields a fresh, isolated DB.
    pub fn in_memory() -> Result<Arc<Self>, RulesStoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_conn(conn)
    }

    fn from_conn(conn: Connection) -> Result<Arc<Self>, RulesStoreError> {
        // Enable foreign keys (off by default in SQLite).
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        Self::run_migrations(&conn)?;

        let rules = Self::load_rules(&conn)?;
        let watchlists = Self::load_watchlists(&conn)?;
        let webhooks = Self::load_webhooks(&conn)?;
        tracing::debug!(
            rules = rules.len(),
            watchlists = watchlists.len(),
            webhooks = webhooks.len(),
            "rules store opened"
        );

        Ok(Arc::new(Self {
            db: Mutex::new(conn),
            rules: RwLock::new(rules),
            watchlists: RwLock::new(watchlists),
            webhooks: RwLock::new(webhooks),
        }))
    }

    fn run_migrations(conn: &Connection) -> Result<(), RulesStoreError> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            )",
            [],
        )?;

        let current: Option<i64> = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .optional()?
            .flatten();

        if let Some(v) = current {
            if v > SCHEMA_VERSION {
                return Err(RulesStoreError::UnsupportedSchemaVersion {
                    found: v,
                    supported: SCHEMA_VERSION,
                });
            }
            if v == SCHEMA_VERSION {
                return Ok(());
            }
        }
        let from = current.unwrap_or(0);

        // v0 → v1: rules + watchlists + watchlist_items.
        if from < 1 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS rules (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    enabled INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_rules_kind_enabled ON rules(kind, enabled);

                CREATE TABLE IF NOT EXISTS watchlists (
                    name TEXT PRIMARY KEY,
                    description TEXT,
                    updated_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS watchlist_items (
                    name TEXT NOT NULL,
                    item TEXT NOT NULL,
                    PRIMARY KEY (name, item),
                    FOREIGN KEY (name) REFERENCES watchlists(name) ON DELETE CASCADE
                );
                "#,
            )?;
        }

        // v1 → v2: webhooks (one JSON row per `WebhookConfig`).
        if from < 2 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS webhooks (
                    id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                "#,
            )?;
        }

        // v2 → v3: rule_cooldowns — per-rule last-fire timestamp so
        // cooldown windows survive daemon restarts. `last_fired_at` is
        // RFC3339 UTC (wall clock) — see CooldownPersistence trait docs
        // for the clock-skew semantics.
        if from < 3 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS rule_cooldowns (
                    rule_id TEXT PRIMARY KEY,
                    last_fired_at TEXT NOT NULL
                );
                "#,
            )?;
        }

        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;
        Ok(())
    }

    fn load_rules(conn: &Connection) -> Result<Vec<Rule>, RulesStoreError> {
        let mut stmt = conn.prepare("SELECT payload FROM rules ORDER BY created_at ASC, id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let payload = row?;
            let rule: Rule = serde_json::from_str(&payload)?;
            out.push(rule);
        }
        Ok(out)
    }

    fn load_watchlists(
        conn: &Connection,
    ) -> Result<HashMap<String, BTreeSet<String>>, RulesStoreError> {
        let mut stmt = conn.prepare("SELECT name, item FROM watchlist_items")?;
        let rows = stmt.query_map([], |row| {
            let name: String = row.get(0)?;
            let item: String = row.get(1)?;
            Ok((name, item))
        })?;
        let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
        for row in rows {
            let (name, item) = row?;
            out.entry(name).or_default().insert(item);
        }
        // Also include watchlists with no items yet, so callers can see them.
        let mut names = conn.prepare("SELECT name FROM watchlists")?;
        let name_rows = names.query_map([], |r| r.get::<_, String>(0))?;
        for n in name_rows {
            out.entry(n?).or_default();
        }
        Ok(out)
    }

    fn load_webhooks(conn: &Connection) -> Result<HashMap<String, WebhookConfig>, RulesStoreError> {
        let mut stmt = conn.prepare("SELECT payload FROM webhooks ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out: HashMap<String, WebhookConfig> = HashMap::new();
        for row in rows {
            let payload = row?;
            let cfg: WebhookConfig = serde_json::from_str(&payload)?;
            out.insert(cfg.id.clone(), cfg);
        }
        Ok(out)
    }

    // ────────────────── Rules CRUD ──────────────────

    /// Insert a new rule. Errors if the id already exists.
    pub fn create_rule(&self, rule: Rule) -> Result<(), RulesStoreError> {
        let payload = serde_json::to_string(&rule)?;
        let kind = serde_json::to_value(rule.kind)?
            .as_str()
            .unwrap_or("watcher")
            .to_string();
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        conn.execute(
            "INSERT INTO rules (id, name, kind, enabled, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &rule.id,
                &rule.name,
                &kind,
                rule.enabled as i64,
                &payload,
                rule.created_at.to_rfc3339(),
            ],
        )?;
        drop(conn);

        let mut rules = self.rules.write().expect("rules cache poisoned");
        rules.push(rule);
        Ok(())
    }

    /// Replace an existing rule by id. Returns `false` if the id doesn't
    /// exist — callers turn that into their own typed "not found" error.
    pub fn update_rule(&self, rule: Rule) -> Result<bool, RulesStoreError> {
        let payload = serde_json::to_string(&rule)?;
        let kind = serde_json::to_value(rule.kind)?
            .as_str()
            .unwrap_or("watcher")
            .to_string();
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let updated = conn.execute(
            "UPDATE rules SET name=?2, kind=?3, enabled=?4, payload=?5 WHERE id=?1",
            params![&rule.id, &rule.name, &kind, rule.enabled as i64, &payload,],
        )?;
        drop(conn);

        if updated == 0 {
            return Ok(false);
        }

        let mut rules = self.rules.write().expect("rules cache poisoned");
        if let Some(slot) = rules.iter_mut().find(|r| r.id == rule.id) {
            *slot = rule;
        }
        Ok(true)
    }

    /// Toggle a rule's `enabled` flag. Returns false if the id doesn't exist.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let updated = conn.execute(
            "UPDATE rules SET enabled=?2, payload=json_set(payload, '$.enabled', json(?3)) WHERE id=?1",
            params![id, enabled as i64, if enabled { "true" } else { "false" }],
        )?;
        drop(conn);
        if updated == 0 {
            return Ok(false);
        }
        let mut rules = self.rules.write().expect("rules cache poisoned");
        if let Some(r) = rules.iter_mut().find(|r| r.id == id) {
            r.enabled = enabled;
        }
        Ok(true)
    }

    /// Delete a rule by id. Returns false if the id doesn't exist.
    pub fn delete_rule(&self, id: &str) -> Result<bool, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let deleted = conn.execute("DELETE FROM rules WHERE id=?1", params![id])?;
        drop(conn);
        if deleted == 0 {
            return Ok(false);
        }
        let mut rules = self.rules.write().expect("rules cache poisoned");
        rules.retain(|r| r.id != id);
        Ok(true)
    }

    /// Look up a rule by id (cheap — reads the in-memory snapshot).
    pub fn get_rule(&self, id: &str) -> Option<Rule> {
        let rules = self.rules.read().expect("rules cache poisoned");
        rules.iter().find(|r| r.id == id).cloned()
    }

    /// List all rules (cheap — clones the in-memory snapshot).
    pub fn list_rules(&self) -> Vec<Rule> {
        self.rules.read().expect("rules cache poisoned").clone()
    }

    // ────────────────── Watchlists CRUD ──────────────────

    /// Create an empty watchlist with an optional description.
    pub fn create_watchlist(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<(), RulesStoreError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        conn.execute(
            "INSERT INTO watchlists (name, description, updated_at) VALUES (?1, ?2, ?3)",
            params![name, description, now],
        )?;
        drop(conn);
        self.watchlists
            .write()
            .expect("watchlists cache poisoned")
            .entry(name.to_string())
            .or_default();
        Ok(())
    }

    /// Drop a watchlist and all its items. Returns false if name unknown.
    pub fn delete_watchlist(&self, name: &str) -> Result<bool, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let deleted = conn.execute("DELETE FROM watchlists WHERE name=?1", params![name])?;
        drop(conn);
        if deleted == 0 {
            return Ok(false);
        }
        self.watchlists
            .write()
            .expect("watchlists cache poisoned")
            .remove(name);
        Ok(true)
    }

    /// Add an item to a watchlist. The watchlist must exist.
    pub fn add_watchlist_item(&self, name: &str, item: &str) -> Result<(), RulesStoreError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        // Touch updated_at.
        conn.execute(
            "UPDATE watchlists SET updated_at=?2 WHERE name=?1",
            params![name, now],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO watchlist_items (name, item) VALUES (?1, ?2)",
            params![name, item],
        )?;
        drop(conn);
        self.watchlists
            .write()
            .expect("watchlists cache poisoned")
            .entry(name.to_string())
            .or_default()
            .insert(item.to_string());
        Ok(())
    }

    /// Replace a watchlist's items atomically. The watchlist is created if
    /// it doesn't exist. Returns the number of items in the resulting list
    /// (useful for diff logging by callers; the in-memory cache is updated
    /// before this call returns).
    pub fn set_watchlist_items(
        &self,
        name: &str,
        items: &[String],
    ) -> Result<usize, RulesStoreError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.db.lock().expect("rules store db mutex poisoned");
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO watchlists (name, description, updated_at) VALUES (?1, NULL, ?2)
             ON CONFLICT(name) DO UPDATE SET updated_at=excluded.updated_at",
            params![name, &now],
        )?;
        tx.execute("DELETE FROM watchlist_items WHERE name=?1", params![name])?;
        for item in items {
            tx.execute(
                "INSERT OR IGNORE INTO watchlist_items (name, item) VALUES (?1, ?2)",
                params![name, item],
            )?;
        }
        tx.commit()?;
        drop(conn);

        let set: BTreeSet<String> = items.iter().cloned().collect();
        let count = set.len();
        self.watchlists
            .write()
            .expect("watchlists cache poisoned")
            .insert(name.to_string(), set);
        Ok(count)
    }

    /// Remove an item from a watchlist. Returns false if item absent.
    pub fn remove_watchlist_item(&self, name: &str, item: &str) -> Result<bool, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let deleted = conn.execute(
            "DELETE FROM watchlist_items WHERE name=?1 AND item=?2",
            params![name, item],
        )?;
        drop(conn);
        if deleted == 0 {
            return Ok(false);
        }
        if let Some(set) = self
            .watchlists
            .write()
            .expect("watchlists cache poisoned")
            .get_mut(name)
        {
            set.remove(item);
        }
        Ok(true)
    }

    /// Cheap existence check (reads the in-memory cache only).
    pub fn has_watchlist(&self, name: &str) -> bool {
        self.watchlists
            .read()
            .expect("watchlists cache poisoned")
            .contains_key(name)
    }

    /// Fetch one watchlist with its current items.
    pub fn get_watchlist(&self, name: &str) -> Result<Option<Watchlist>, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let row = conn
            .query_row(
                "SELECT description, updated_at FROM watchlists WHERE name=?1",
                params![name],
                |r| {
                    let description: Option<String> = r.get(0)?;
                    let updated_at: String = r.get(1)?;
                    Ok((description, updated_at))
                },
            )
            .optional()?;
        let Some((description, updated_at_str)) = row else {
            return Ok(None);
        };
        let updated_at = parse_rfc3339(&updated_at_str);
        drop(conn);

        let items = self
            .watchlists
            .read()
            .expect("watchlists cache poisoned")
            .get(name)
            .cloned()
            .unwrap_or_default();
        Ok(Some(Watchlist {
            name: name.to_string(),
            description,
            items,
            updated_at,
        }))
    }

    /// List every watchlist (names + items).
    pub fn list_watchlists(&self) -> Result<Vec<Watchlist>, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let mut stmt =
            conn.prepare("SELECT name, description, updated_at FROM watchlists ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let description: Option<String> = r.get(1)?;
            let updated_at: String = r.get(2)?;
            Ok((name, description, updated_at))
        })?;
        let mut metas: Vec<(String, Option<String>, DateTime<Utc>)> = Vec::new();
        for row in rows {
            let (n, d, ts) = row?;
            metas.push((n, d, parse_rfc3339(&ts)));
        }
        drop(stmt);
        drop(conn);

        let items_snapshot = self
            .watchlists
            .read()
            .expect("watchlists cache poisoned")
            .clone();
        Ok(metas
            .into_iter()
            .map(|(name, description, updated_at)| {
                let items = items_snapshot.get(&name).cloned().unwrap_or_default();
                Watchlist {
                    name,
                    description,
                    items,
                    updated_at,
                }
            })
            .collect())
    }
}

fn parse_rfc3339(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ────────────────── Webhooks CRUD ──────────────────

impl SqliteRulesStore {
    /// Insert a new webhook config. Errors with a unique-constraint
    /// violation if the id already exists.
    pub fn create_webhook(&self, cfg: WebhookConfig) -> Result<(), RulesStoreError> {
        let payload = serde_json::to_string(&cfg)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        conn.execute(
            "INSERT INTO webhooks (id, payload, updated_at) VALUES (?1, ?2, ?3)",
            params![&cfg.id, &payload, &now],
        )?;
        drop(conn);
        self.webhooks
            .write()
            .expect("webhooks cache poisoned")
            .insert(cfg.id.clone(), cfg);
        Ok(())
    }

    /// Replace an existing webhook config by id. Returns `false` if the id
    /// doesn't exist.
    pub fn update_webhook(&self, cfg: WebhookConfig) -> Result<bool, RulesStoreError> {
        let payload = serde_json::to_string(&cfg)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let updated = conn.execute(
            "UPDATE webhooks SET payload=?2, updated_at=?3 WHERE id=?1",
            params![&cfg.id, &payload, &now],
        )?;
        drop(conn);
        if updated == 0 {
            return Ok(false);
        }
        self.webhooks
            .write()
            .expect("webhooks cache poisoned")
            .insert(cfg.id.clone(), cfg);
        Ok(true)
    }

    /// Delete a webhook config by id. Returns `false` if the id doesn't exist.
    pub fn delete_webhook(&self, id: &str) -> Result<bool, RulesStoreError> {
        let conn = self.db.lock().expect("rules store db mutex poisoned");
        let deleted = conn.execute("DELETE FROM webhooks WHERE id=?1", params![id])?;
        drop(conn);
        if deleted == 0 {
            return Ok(false);
        }
        self.webhooks
            .write()
            .expect("webhooks cache poisoned")
            .remove(id);
        Ok(true)
    }

    /// Look up a webhook config (in-memory cache read).
    pub fn get_webhook(&self, id: &str) -> Option<WebhookConfig> {
        self.webhooks
            .read()
            .expect("webhooks cache poisoned")
            .get(id)
            .cloned()
    }

    /// All configured webhooks (in-memory cache snapshot).
    pub fn list_webhooks(&self) -> Vec<WebhookConfig> {
        let mut out: Vec<WebhookConfig> = self
            .webhooks
            .read()
            .expect("webhooks cache poisoned")
            .values()
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Snapshot the webhook table for the daemon's startup wiring of
    /// `cellar_webhook::WebhookService` — same shape the service expects.
    pub fn webhooks_snapshot(&self) -> HashMap<String, WebhookConfig> {
        self.webhooks
            .read()
            .expect("webhooks cache poisoned")
            .clone()
    }
}

// ────────────────── Trait impls (gateway + matcher integration) ──────────────────

impl RuleSource for SqliteRulesStore {
    fn snapshot(&self) -> Vec<Rule> {
        self.list_rules()
    }
}

impl WatchlistLookup for SqliteRulesStore {
    fn contains(&self, watchlist_name: &str, item: &str) -> bool {
        self.watchlists
            .read()
            .expect("watchlists cache poisoned")
            .get(watchlist_name)
            .is_some_and(|set| set.contains(item))
    }
}

impl CooldownPersistence for SqliteRulesStore {
    /// Load every `(rule_id, last_fired_at)` row from the `rule_cooldowns`
    /// table. Returns an empty vec on IO or parse error — the cooldown
    /// tracker degrades to in-memory behaviour on failure rather than
    /// blocking daemon boot.
    fn load_all(&self) -> Vec<(String, DateTime<Utc>)> {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(_) => {
                tracing::error!("rules-store db mutex poisoned in CooldownPersistence::load_all");
                return Vec::new();
            }
        };
        let mut stmt = match conn.prepare("SELECT rule_id, last_fired_at FROM rule_cooldowns") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "rule_cooldowns prepare failed; skipping rehydrate");
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let ts_str: String = row.get(1)?;
            Ok((id, ts_str))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "rule_cooldowns query failed; skipping rehydrate");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for row in rows {
            match row {
                Ok((id, ts_str)) => match DateTime::parse_from_rfc3339(&ts_str) {
                    Ok(ts) => out.push((id, ts.with_timezone(&Utc))),
                    Err(e) => {
                        tracing::warn!(
                            rule_id = %id,
                            error = %e,
                            "rule_cooldowns row has unparseable last_fired_at; dropping"
                        );
                    }
                },
                Err(e) => tracing::warn!(error = %e, "rule_cooldowns row error; dropping"),
            }
        }
        out
    }

    /// Insert-or-replace `(rule_id, ts)` in the `rule_cooldowns` table.
    /// Logs at `warn` on IO error; cooldown is best-effort so we don't
    /// propagate failures up to the matcher hot path.
    fn upsert(&self, rule_id: &str, ts: DateTime<Utc>) {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(_) => {
                tracing::error!("rules-store db mutex poisoned in CooldownPersistence::upsert");
                return;
            }
        };
        let result = conn.execute(
            "INSERT INTO rule_cooldowns (rule_id, last_fired_at) VALUES (?1, ?2)
             ON CONFLICT(rule_id) DO UPDATE SET last_fired_at = excluded.last_fired_at",
            params![rule_id, ts.to_rfc3339()],
        );
        if let Err(e) = result {
            tracing::warn!(
                rule_id = %rule_id,
                error = %e,
                "rule_cooldowns upsert failed; in-memory state still correct"
            );
        }
    }

    /// Delete every `rule_cooldowns` row whose `last_fired_at < cutoff`.
    /// Called from `CooldownTracker::gc`. Errors are logged and swallowed.
    fn delete_older_than(&self, cutoff: DateTime<Utc>) {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(_) => {
                tracing::error!(
                    "rules-store db mutex poisoned in CooldownPersistence::delete_older_than"
                );
                return;
            }
        };
        let result = conn.execute(
            "DELETE FROM rule_cooldowns WHERE last_fired_at < ?1",
            params![cutoff.to_rfc3339()],
        );
        if let Err(e) = result {
            tracing::warn!(error = %e, "rule_cooldowns gc delete failed");
        }
    }
}

// `impl<T: RuleSource> RuleSource for Arc<T>` and the same for
// `WatchlistLookup` live in their respective trait-owning crates
// (cel-act-gateway and cellar-types). The daemon's `Arc<SqliteRulesStore>`
// gets the trait impls for free via those blanket impls.
//
// `CooldownPersistence` is object-safe and the daemon holds it as
// `Arc<dyn CooldownPersistence>`, so no blanket impl is needed.

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_types::expression::Operator;
    use cellar_types::rule::{Action, ActionType, RuleKind};
    use cellar_types::Expression;
    use serde_json::json;

    fn sample_rule(id: &str) -> Rule {
        Rule {
            id: id.into(),
            name: format!("rule {id}"),
            nl_original: "test rule".into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            action: Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
            cooldown_seconds: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn open_in_memory_initialises_schema() {
        let s = SqliteRulesStore::in_memory().unwrap();
        assert_eq!(s.list_rules().len(), 0);
        assert_eq!(s.list_watchlists().unwrap().len(), 0);
    }

    #[test]
    fn create_then_snapshot_roundtrips() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_rule(sample_rule("r1")).unwrap();
        let rules: Vec<Rule> = s.snapshot();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "r1");
    }

    #[test]
    fn duplicate_id_errors() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_rule(sample_rule("dup")).unwrap();
        let err = s.create_rule(sample_rule("dup")).unwrap_err();
        assert!(matches!(err, RulesStoreError::Sqlite(_)));
    }

    #[test]
    fn update_rule_replaces_payload() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_rule(sample_rule("r1")).unwrap();
        let mut r = sample_rule("r1");
        r.name = "renamed".into();
        s.update_rule(r).unwrap();
        assert_eq!(s.get_rule("r1").unwrap().name, "renamed");
    }

    #[test]
    fn update_missing_rule_returns_false() {
        let s = SqliteRulesStore::in_memory().unwrap();
        let updated = s.update_rule(sample_rule("ghost")).unwrap();
        assert!(!updated);
    }

    #[test]
    fn set_enabled_toggles_flag() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_rule(sample_rule("r1")).unwrap();
        assert!(s.set_enabled("r1", false).unwrap());
        assert!(!s.get_rule("r1").unwrap().enabled);
        assert!(s.set_enabled("r1", true).unwrap());
        assert!(s.get_rule("r1").unwrap().enabled);
        // Unknown id returns false.
        assert!(!s.set_enabled("ghost", true).unwrap());
    }

    #[test]
    fn delete_rule_removes_from_snapshot() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_rule(sample_rule("r1")).unwrap();
        assert!(s.delete_rule("r1").unwrap());
        assert!(s.snapshot().is_empty());
        // Idempotent: second delete returns false.
        assert!(!s.delete_rule("r1").unwrap());
    }

    #[test]
    fn watchlist_create_add_lookup_remove() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_watchlist("approved_apps", Some("Apps the user trusts"))
            .unwrap();
        s.add_watchlist_item("approved_apps", "com.apple.Safari")
            .unwrap();
        s.add_watchlist_item("approved_apps", "com.slack.Slack")
            .unwrap();

        // Trait impl reads from the live snapshot.
        assert!(s.contains("approved_apps", "com.apple.Safari"));
        assert!(!s.contains("approved_apps", "com.example.malware"));
        assert!(!s.contains("nonexistent_list", "anything"));

        // Remove one item.
        assert!(s
            .remove_watchlist_item("approved_apps", "com.apple.Safari")
            .unwrap());
        assert!(!s.contains("approved_apps", "com.apple.Safari"));
        assert!(s.contains("approved_apps", "com.slack.Slack"));

        // Removing missing item returns false.
        assert!(!s
            .remove_watchlist_item("approved_apps", "com.apple.Safari")
            .unwrap());
    }

    #[test]
    fn watchlist_delete_cascade_removes_items() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_watchlist("temp", None).unwrap();
        s.add_watchlist_item("temp", "x").unwrap();
        s.add_watchlist_item("temp", "y").unwrap();
        assert!(s.delete_watchlist("temp").unwrap());
        // Cache cleared.
        assert!(!s.contains("temp", "x"));
        // Items cascaded out of DB too (verified by reopening).
        let lists = s.list_watchlists().unwrap();
        assert!(lists.is_empty());
    }

    #[test]
    fn set_watchlist_items_replaces_atomically() {
        let s = SqliteRulesStore::in_memory().unwrap();
        // Set into a watchlist that doesn't exist yet → it's created.
        let n = s
            .set_watchlist_items("approved", &["a".into(), "b".into(), "c".into()])
            .unwrap();
        assert_eq!(n, 3);
        assert!(s.contains("approved", "a"));
        assert!(s.contains("approved", "b"));
        assert!(s.contains("approved", "c"));

        // Re-setting replaces the contents (and drops items not in the new list).
        s.set_watchlist_items("approved", &["x".into(), "a".into()])
            .unwrap();
        assert!(s.contains("approved", "x"));
        assert!(s.contains("approved", "a"));
        assert!(!s.contains("approved", "b"));
        assert!(!s.contains("approved", "c"));

        // Duplicates in the input collapse (BTreeSet behaviour).
        let n = s
            .set_watchlist_items("approved", &["a".into(), "a".into(), "a".into()])
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn duplicate_watchlist_item_is_noop() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_watchlist("wl", None).unwrap();
        s.add_watchlist_item("wl", "a").unwrap();
        s.add_watchlist_item("wl", "a").unwrap(); // idempotent
        let wl = s.get_watchlist("wl").unwrap().unwrap();
        assert_eq!(wl.items.len(), 1);
    }

    #[test]
    fn persistence_round_trip_through_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.sqlite");

        let s1 = SqliteRulesStore::open(&path).unwrap();
        s1.create_rule(sample_rule("persisted")).unwrap();
        s1.create_watchlist("ok", Some("trusted")).unwrap();
        s1.add_watchlist_item("ok", "com.apple.Safari").unwrap();
        drop(s1);

        // Reopen — caches should rebuild from the file.
        let s2 = SqliteRulesStore::open(&path).unwrap();
        assert_eq!(s2.list_rules().len(), 1);
        assert_eq!(s2.list_rules()[0].id, "persisted");
        assert!(s2.contains("ok", "com.apple.Safari"));
        assert_eq!(
            s2.get_watchlist("ok")
                .unwrap()
                .unwrap()
                .description
                .as_deref(),
            Some("trusted")
        );
    }

    #[test]
    fn schema_version_recorded_on_open() {
        let s = SqliteRulesStore::in_memory().unwrap();
        let conn = s.db.lock().unwrap();
        let v: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    fn sample_webhook(id: &str) -> WebhookConfig {
        WebhookConfig {
            id: id.into(),
            url: "https://example.com/hook".into(),
            headers: Default::default(),
            secret_header: Some("X-Webhook-Secret".into()),
            secret_value_env: Some("MY_SECRET_ENV".into()),
            timeout_ms: 5000,
        }
    }

    #[test]
    fn webhook_create_get_list_round_trip() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_webhook(sample_webhook("default")).unwrap();
        s.create_webhook(sample_webhook("slack")).unwrap();

        let got = s.get_webhook("default").unwrap();
        assert_eq!(got.url, "https://example.com/hook");

        let list = s.list_webhooks();
        assert_eq!(list.len(), 2);
        // Sorted by id (slack < default? alphabetical → "default" then "slack")
        assert_eq!(list[0].id, "default");
        assert_eq!(list[1].id, "slack");
    }

    #[test]
    fn webhook_create_duplicate_id_errors() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_webhook(sample_webhook("dup")).unwrap();
        let err = s.create_webhook(sample_webhook("dup")).unwrap_err();
        assert!(err.is_unique_constraint_violation());
    }

    #[test]
    fn webhook_update_replaces_payload() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_webhook(sample_webhook("h1")).unwrap();
        let mut updated = sample_webhook("h1");
        updated.url = "https://other.example.com/v2".into();
        assert!(s.update_webhook(updated).unwrap());
        assert_eq!(
            s.get_webhook("h1").unwrap().url,
            "https://other.example.com/v2"
        );
    }

    #[test]
    fn webhook_update_missing_returns_false() {
        let s = SqliteRulesStore::in_memory().unwrap();
        assert!(!s.update_webhook(sample_webhook("ghost")).unwrap());
    }

    #[test]
    fn webhook_delete_removes_from_snapshot() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_webhook(sample_webhook("h1")).unwrap();
        assert!(s.delete_webhook("h1").unwrap());
        assert!(s.get_webhook("h1").is_none());
        assert!(!s.delete_webhook("h1").unwrap()); // idempotent
    }

    #[test]
    fn webhook_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.sqlite");
        let s1 = SqliteRulesStore::open(&path).unwrap();
        s1.create_webhook(sample_webhook("persisted")).unwrap();
        drop(s1);

        let s2 = SqliteRulesStore::open(&path).unwrap();
        let got = s2.get_webhook("persisted").unwrap();
        assert_eq!(got.url, "https://example.com/hook");
    }

    #[test]
    fn webhooks_snapshot_is_what_the_service_expects() {
        let s = SqliteRulesStore::in_memory().unwrap();
        s.create_webhook(sample_webhook("default")).unwrap();
        let snap = s.webhooks_snapshot();
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key("default"));
    }

    #[test]
    fn arc_clone_implements_both_traits() {
        // Confirms the gateway's `Gateway<_, _, Arc<SqliteRulesStore>, Arc<SqliteRulesStore>>`
        // wiring will compile: same `Arc` cloned twice, both clones used as
        // R and W type params. This is the critical integration shape.
        let s: Arc<SqliteRulesStore> = SqliteRulesStore::in_memory().unwrap();
        s.create_rule(sample_rule("r")).unwrap();
        s.create_watchlist("wl", None).unwrap();
        s.add_watchlist_item("wl", "i").unwrap();

        let as_rules: &dyn RuleSource = &s;
        let as_lookup: &dyn WatchlistLookup = &s;
        assert_eq!(as_rules.snapshot().len(), 1);
        assert!(as_lookup.contains("wl", "i"));
    }
}
