use std::sync::Arc;

use tokio::sync::{Mutex, Notify, RwLock, broadcast};

use orca_backend_common::BollardRuntime;
use orca_backend_common::k8s::K3sManager;
use orca_core::config::OrcaConfig;
use orca_core::event::Event;
use orca_core::image::Image;
use orca_core::runtime::Container;

use crate::cache::{DEFAULT_TTL, ResourceCache};

/// Shared application state for the daemon.
///
/// The runtime is wrapped in RwLock so it can be hot-swapped when
/// reconnecting to Docker without restarting the daemon.
pub struct AppState {
    pub config: Mutex<OrcaConfig>,
    pub runtime: RwLock<Arc<BollardRuntime>>,
    pub k8s: Arc<K3sManager>,
    pub events_tx: broadcast::Sender<Event>,
    /// API authentication token. Empty string means auth is disabled (--no-auth).
    pub api_token: String,
    /// Fired when the runtime is hot-swapped so background tasks can
    /// re-subscribe against the new runtime immediately instead of
    /// polling.
    pub swap_notify: Arc<Notify>,
    /// Short-TTL caches for the two most-polled list endpoints. See
    /// [`crate::cache`] for why these exist and why the TTL is this short.
    pub containers: ResourceCache<Vec<Container>>,
    pub images: ResourceCache<Vec<Image>>,
}

impl AppState {
    pub fn new(
        config: OrcaConfig,
        runtime: Arc<BollardRuntime>,
        k8s: Arc<K3sManager>,
        events_tx: broadcast::Sender<Event>,
        api_token: String,
    ) -> Self {
        Self {
            config: Mutex::new(config),
            runtime: RwLock::new(runtime),
            k8s,
            events_tx,
            api_token,
            swap_notify: Arc::new(Notify::new()),
            containers: ResourceCache::new(DEFAULT_TTL),
            images: ResourceCache::new(DEFAULT_TTL),
        }
    }

    /// Get the current runtime (read lock — fast, concurrent).
    pub async fn rt(&self) -> Arc<BollardRuntime> {
        self.runtime.read().await.clone()
    }

    /// Swap the runtime with a new connection (write lock — exclusive).
    pub async fn swap_runtime(&self, new_runtime: Arc<BollardRuntime>) {
        let mut guard = self.runtime.write().await;
        *guard = new_runtime;
        drop(guard);
        // The BuildKit history cache holds records read from the *previous*
        // engine. Its TTL would keep serving them for up to 30s after the
        // switch, which the GUI presents as immediate.
        crate::build_manager::invalidate_buildkit_cache();
        // Every cached list belongs to the engine that was just replaced.
        // Serving them for a TTL would show the *old* engine's containers after
        // a reconnect the user was already told had succeeded.
        self.containers.invalidate();
        self.images.invalidate();
        // Wake any task that was waiting on the old subscription so it can
        // re-subscribe against the new runtime immediately.
        self.swap_notify.notify_waiters();
        tracing::info!("Runtime connection hot-swapped successfully");
    }
}
