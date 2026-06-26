//! Embedded SQL migrations.
//!
//! Migrations are bundled into the binary via `include_str!` so applications do
//! not need to ship migration files separately. Each migration runs in a
//! transaction; the schema version is tracked in `_cel_memory_schema_version`.

use rusqlite::{params, Connection};

use crate::error::SqliteMemoryError;

/// One migration step.
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "001_initial.sql",
    sql: include_str!("../migrations/001_initial.sql"),
}];

/// Highest schema version this build can apply.
pub const LATEST_VERSION: i64 = 1;

/// Run all pending migrations against the connection. Idempotent: a
/// connection that already at `LATEST_VERSION` becomes a no-op.
pub fn run(conn: &mut Connection) -> Result<(), SqliteMemoryError> {
    // Create the schema-tracking table if it doesn't exist. Standalone
    // statement so we know it executes even on a fresh DB.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _cel_memory_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
        [],
    )?;

    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM _cel_memory_schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        tracing::info!(version = m.version, name = m.name, "applying migration");
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)
            .map_err(|e| SqliteMemoryError::Migration {
                name: m.name.to_string(),
                source: e,
            })?;
        tx.execute(
            "INSERT INTO _cel_memory_schema_version(version, applied_at) VALUES(?, ?)",
            params![m.version, chrono::Utc::now().timestamp_millis()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_with_vec() -> Connection {
        // Register the extension BEFORE opening so the new connection
        // picks it up. Auto-extensions only affect connections opened
        // after registration; calling multiple times is safe — SQLite
        // dedupes by function pointer.
        crate::vec_extension::register();
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn schema_version_starts_at_zero() {
        let mut conn = open_with_vec();
        run(&mut conn).unwrap();
        let v: i64 = conn
            .query_row(
                "SELECT MAX(version) FROM _cel_memory_schema_version",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, LATEST_VERSION);
    }

    #[test]
    fn running_twice_is_idempotent() {
        let mut conn = open_with_vec();
        run(&mut conn).unwrap();
        run(&mut conn).unwrap();
        let v: i64 = conn
            .query_row("SELECT COUNT(*) FROM _cel_memory_schema_version", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, LATEST_VERSION); // each migration recorded once
    }
}
