//! CortexManager — instance registry for multiple concurrent Cortex instances.
//!
//! Supports booting/shutting down multiple Cortex instances. The first one
//! booted becomes the default (for backwards compatibility with singleton usage).

use crate::cortex::{Cortex, CortexError};
use crate::daemon_bridge::DaemonBridge;
use crate::model::MentalModel;
use cel_accessibility::AccessibilityTree;
use cel_context::ContextMerger;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Manages multiple Cortex instances.
pub struct CortexManager {
    /// Active cortex instances by ID.
    instances: RwLock<HashMap<String, Arc<Cortex>>>,
    /// ID of the default (first-booted) instance.
    default_id: RwLock<Option<String>>,
    /// Counter for auto-generating IDs.
    next_id: AtomicU32,
}

impl CortexManager {
    /// Create a new empty manager.
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            default_id: RwLock::new(None),
            next_id: AtomicU32::new(1),
        }
    }

    /// Boot a new Cortex instance.
    ///
    /// If `id` is None, an auto-generated ID is used.
    /// The `merger` provides context from the accessibility tree and other streams.
    /// When `bridge` is `Some`, Cortex-observed events (`url_changed`,
    /// `app_focused`, `window_opened`) are forwarded to it — the host wraps its
    /// daemon IPC client so the daemon's rule matcher sees browser navigation
    /// the daemon cannot observe on its own.
    ///
    /// `configure` is a host-supplied hook run on the fresh Cortex *before*
    /// `boot()` — the point at which adapters must be registered (the
    /// tick-loop lock is uncontested then). Hosts pass
    /// `cel_adapters::register_default_adapters` here so this engine crate
    /// stays adapter-agnostic (it cannot depend on the adapter crates, which
    /// already depend on it). Pass a no-op closure (`|_| {}`) to register
    /// nothing.
    pub async fn boot_cortex(
        &self,
        id: Option<String>,
        merger: ContextMerger,
        observer: Box<dyn AccessibilityTree>,
        bridge: Option<Arc<dyn DaemonBridge>>,
        configure: impl FnOnce(&mut Cortex),
    ) -> Result<String, CortexError> {
        let id = id.unwrap_or_else(|| {
            let n = self.next_id.fetch_add(1, Ordering::Relaxed);
            format!("cortex-{}", n)
        });

        {
            let instances = self.instances.read().await;
            if instances.contains_key(&id) {
                return Err(CortexError::AlreadyRunning(id));
            }
        }

        // Production manager: the user's Cortex is meant to drive their real
        // machine, so opt into native input. Eval/test contexts should never
        // route through CortexManager.
        let mut cortex = Cortex::new(id.clone()).with_native_input_unsafe();
        if let Some(bridge) = bridge {
            cortex = cortex.with_daemon_bridge(bridge);
        }
        // Host adapter wiring. Must run before boot(): register_adapter takes
        // the adapters write-lock uncontended only before the tick loop spawns.
        configure(&mut cortex);
        cortex.boot(merger, observer).await?;
        let cortex = Arc::new(cortex);

        {
            let mut instances = self.instances.write().await;
            instances.insert(id.clone(), Arc::clone(&cortex));
        }

        // Set as default if first instance
        {
            let mut default = self.default_id.write().await;
            if default.is_none() {
                *default = Some(id.clone());
            }
        }

        info!(cortex_id = %id, "Cortex instance booted");
        Ok(id)
    }

    /// Shutdown a specific Cortex instance.
    pub async fn shutdown_cortex(&self, id: &str) -> Result<(), CortexError> {
        let cortex = {
            let mut instances = self.instances.write().await;
            instances.remove(id)
        };

        match cortex {
            Some(c) => {
                c.shutdown();

                // If this was the default, promote the next one
                let mut default = self.default_id.write().await;
                if default.as_deref() == Some(id) {
                    let instances = self.instances.read().await;
                    *default = instances.keys().next().cloned();
                }

                debug!(cortex_id = %id, "Cortex instance shut down");
                Ok(())
            }
            None => Err(CortexError::NotRunning(id.to_string())),
        }
    }

    /// Shutdown all Cortex instances.
    pub async fn shutdown_all(&self) {
        let instances: Vec<(String, Arc<Cortex>)> = {
            let mut map = self.instances.write().await;
            map.drain().collect()
        };

        for (id, cortex) in instances {
            cortex.shutdown();
            debug!(cortex_id = %id, "Cortex instance shut down");
        }

        let mut default = self.default_id.write().await;
        *default = None;

        info!("All Cortex instances shut down");
    }

    /// Get the default (first-booted) Cortex instance.
    pub async fn get_default(&self) -> Option<Arc<Cortex>> {
        let default_id = self.default_id.read().await;
        match default_id.as_deref() {
            Some(id) => {
                let instances = self.instances.read().await;
                instances.get(id).cloned()
            }
            None => None,
        }
    }

    /// Get a specific Cortex instance by ID.
    pub async fn get_by_id(&self, id: &str) -> Option<Arc<Cortex>> {
        let instances = self.instances.read().await;
        instances.get(id).cloned()
    }

    /// Check if any Cortex instance is active.
    pub async fn is_active(&self) -> bool {
        !self.instances.read().await.is_empty()
    }

    /// List all active Cortex instance IDs.
    pub async fn list_ids(&self) -> Vec<String> {
        self.instances.read().await.keys().cloned().collect()
    }

    /// Get the default Cortex's mental model for reading.
    pub async fn read_model(&self) -> Option<Arc<RwLock<MentalModel>>> {
        let cortex = self.get_default().await?;
        Some(cortex.model())
    }
}

impl Default for CortexManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_new() {
        let manager = CortexManager::new();
        // Can't easily test async methods in sync test, but verify construction
        assert_eq!(manager.next_id.load(Ordering::Relaxed), 1);
    }
}
