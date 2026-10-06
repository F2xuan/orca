//! A short-TTL snapshot cache for the list endpoints.
//!
//! # Why a cache at all
//!
//! Six GUI surfaces ask the daemon for the container list — the sidebar, the
//! command palette, the containers page, the container detail page, the gateway
//! page and the templates page — and the sidebar polls the container list *and*
//! the image list every five seconds regardless of whether anything changed.
//! Every one of those calls was its own Docker API round trip.
//!
//! # The shape: `{ data, err, updated_at }`
//!
//! Borrowed from Komodo's resource state. Keeping `err` *beside* the data
//! rather than instead of it is the point: a transient Docker hiccup must not
//! blank a list the user is reading, and the caller still needs to know the
//! value is old.
//!
//! # Why the TTL is short
//!
//! Two seconds. The cache exists to collapse *concurrent* demand, not to avoid
//! work: six surfaces loading at once become one Docker call, and the sidebar's
//! five-second poll usually arrives after the entry has already expired, so it
//! still sees fresh data. A long TTL would trade a real correctness problem
//! (a list that does not reflect an action the user just took) for a saving
//! nobody can perceive.
//!
//! Mutations therefore invalidate explicitly, and the TTL is the backstop for
//! any site that forgets. That ordering matters: forgetting an invalidation
//! costs at most one TTL of staleness rather than a permanently wrong list.
//!
//! # Clock injection
//!
//! Freshness is decided against an injected clock, so every TTL rule below is
//! tested without sleeping — the same reason [`orca_core::alert::AlertEngine`]
//! takes timestamps instead of reading the clock.

use std::fmt::Display;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a cached list is served before another fetch is allowed.
///
/// See the module comment for why this is two seconds and not thirty.
pub const DEFAULT_TTL: Duration = Duration::from_secs(2);

/// A source of "now", so tests can move time without sleeping.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// One resource's cached state: the last good value, the last failure, and when
/// the value was taken.
#[derive(Debug)]
pub struct Snapshot<T> {
    /// The last successful value, if there has ever been one.
    ///
    /// `Arc` so handing it to a response body is a refcount bump rather than a
    /// deep copy of a container list.
    pub data: Option<Arc<T>>,
    /// The most recent failure, if the most recent attempt failed.
    ///
    /// Cleared by the next success, so `err.is_some()` always means "the value
    /// beside me is older than the last attempt".
    pub err: Option<Arc<str>>,
    /// When `data` was taken. `None` until the first success.
    pub updated_at: Option<Instant>,
}

/// Every field is an `Arc`, an `Option`, or a `Copy` instant, so cloning a
/// snapshot is cheap and must **not** require `T: Clone` — a caller should be
/// able to hand out an `Arc<T>` for a type that cannot be cloned.
impl<T> Clone for Snapshot<T> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            err: self.err.clone(),
            updated_at: self.updated_at,
        }
    }
}

impl<T> Default for Snapshot<T> {
    fn default() -> Self {
        Self {
            data: None,
            err: None,
            updated_at: None,
        }
    }
}

impl<T> Snapshot<T> {
    /// Whether this snapshot may be served without fetching again.
    ///
    /// A cache with no data is never fresh, however recently it was *attempted*
    /// — `updated_at` tracks the value, not the attempt, so a failing cache
    /// retries on every request instead of serving an empty list for a TTL.
    pub fn is_fresh(&self, ttl: Duration, now: Instant) -> bool {
        match self.updated_at {
            Some(updated_at) => now.saturating_duration_since(updated_at) < ttl,
            None => false,
        }
    }

    /// How old the value is, if there is one.
    pub fn age(&self, now: Instant) -> Option<Duration> {
        self.updated_at.map(|at| now.saturating_duration_since(at))
    }
}

/// A cache for one resource type.
pub struct ResourceCache<T> {
    snapshot: Mutex<Snapshot<T>>,
    /// Held across a refresh. This is what collapses concurrent demand: the
    /// losers of the race wait, then find the winner's value already fresh
    /// rather than each fetching their own copy.
    ///
    /// Serialising refreshes rather than sharing one future keeps this simple
    /// and still bounds Docker to one call per TTL — the latency cost falls
    /// only on callers who arrived during a refresh, who would otherwise have
    /// queued on the same socket anyway.
    refresh: tokio::sync::Mutex<()>,
    ttl: Duration,
    clock: Clock,
}

impl<T> ResourceCache<T> {
    pub fn new(ttl: Duration) -> Self {
        Self::with_clock(ttl, Arc::new(Instant::now))
    }

    pub fn with_clock(ttl: Duration, clock: Clock) -> Self {
        Self {
            snapshot: Mutex::new(Snapshot::default()),
            refresh: tokio::sync::Mutex::new(()),
            ttl,
            clock,
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    /// Drop the cached value so the next call fetches.
    ///
    /// Called by mutations. The `err` is cleared with the value: an error
    /// recorded against data that is now gone would read as "this failed"
    /// about a resource nobody has tried to load since.
    pub fn invalidate(&self) {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *snapshot = Snapshot::default();
    }
}

impl<T: Send + Sync + 'static> ResourceCache<T> {
    /// Serve the cached value if it is fresh, otherwise fetch one.
    ///
    /// On failure the last good `data` is **kept** and `err` is set, so a caller
    /// with a value to show can keep showing it while knowing it is stale.
    pub async fn get_or_refresh<F, Fut, E>(&self, fetch: F) -> Snapshot<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: Display,
    {
        if let Some(fresh) = self.fresh_snapshot() {
            return fresh;
        }

        let _refreshing = self.refresh.lock().await;

        // Re-check: someone may have refreshed while this task waited for the
        // lock, which is the entire point of holding it across the fetch.
        if let Some(fresh) = self.fresh_snapshot() {
            return fresh;
        }

        let now = self.now();
        match fetch().await {
            Ok(value) => {
                let mut snapshot = self.lock();
                snapshot.data = Some(Arc::new(value));
                snapshot.err = None;
                snapshot.updated_at = Some(now);
                snapshot.clone()
            }
            Err(e) => {
                let message: Arc<str> = Arc::from(e.to_string().as_str());
                let mut snapshot = self.lock();
                // `data` deliberately untouched, and `updated_at` deliberately
                // not moved: the value we are holding is exactly as old as it
                // was, and saying otherwise would make a failed refresh look
                // like a successful one.
                snapshot.err = Some(message.clone());

                // Logged *here*, not at the call site. A caller that still has a
                // value to serve returns it and would otherwise swallow the
                // failure completely: the list silently goes stale and nothing
                // anywhere says so.
                match snapshot.age(now) {
                    Some(age) => tracing::warn!(
                        error = %message,
                        stale_for_ms = age.as_millis(),
                        "resource refresh failed; serving the last good value"
                    ),
                    None => tracing::warn!(
                        error = %message,
                        "resource refresh failed; nothing cached to serve"
                    ),
                }
                snapshot.clone()
            }
        }
    }

    /// The current snapshot without fetching. Test-only: production code goes
    /// through `get_or_refresh`, and a peek nobody calls would be speculation.
    #[cfg(test)]
    fn peek(&self) -> Snapshot<T> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Snapshot<T>> {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fresh_snapshot(&self) -> Option<Snapshot<T>> {
        let snapshot = self.lock();
        snapshot
            .is_fresh(self.ttl, self.now())
            .then(|| snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    /// A clock the tests move by hand, so no test sleeps.
    struct FakeClock {
        base: Instant,
        millis: AtomicU64,
    }

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                millis: AtomicU64::new(0),
            })
        }

        fn advance(&self, millis: u64) {
            self.millis.fetch_add(millis, Ordering::SeqCst);
        }

        fn clock(self: &Arc<Self>) -> Clock {
            let this = self.clone();
            Arc::new(move || this.base + Duration::from_millis(this.millis.load(Ordering::SeqCst)))
        }
    }

    /// A fetch that counts its calls and can be told to fail.
    struct Fetcher {
        calls: AtomicUsize,
        next: Mutex<Option<String>>,
    }

    impl Fetcher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                next: Mutex::new(None),
            })
        }

        fn fail_with(&self, message: &str) {
            *self.next.lock().unwrap() = Some(message.to_string());
        }

        fn succeed(&self) {
            *self.next.lock().unwrap() = None;
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        async fn fetch(&self, value: u32) -> Result<u32, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Yield so concurrent callers genuinely interleave.
            tokio::task::yield_now().await;
            match self.next.lock().unwrap().clone() {
                Some(message) => Err(message),
                None => Ok(value),
            }
        }
    }

    fn cache(ttl: Duration) -> (ResourceCache<u32>, Arc<FakeClock>) {
        let clock = FakeClock::new();
        (
            ResourceCache::with_clock(ttl, clock.clock()),
            clock,
        )
    }

    #[tokio::test]
    async fn the_first_call_fetches_and_the_second_does_not() {
        let (cache, _clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();

        let first = cache.get_or_refresh(|| fetcher.fetch(7)).await;
        assert_eq!(first.data.as_deref(), Some(&7));
        assert!(first.err.is_none());
        assert!(first.updated_at.is_some());

        let second = cache.get_or_refresh(|| fetcher.fetch(8)).await;
        assert_eq!(second.data.as_deref(), Some(&7), "the cached value is served");
        assert_eq!(fetcher.calls(), 1, "and no second fetch happened");
    }

    #[tokio::test]
    async fn the_value_is_refetched_once_the_ttl_expires() {
        let (cache, clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();

        cache.get_or_refresh(|| fetcher.fetch(1)).await;
        clock.advance(1_999);
        cache.get_or_refresh(|| fetcher.fetch(2)).await;
        assert_eq!(fetcher.calls(), 1, "still inside the TTL");

        clock.advance(2);
        let after = cache.get_or_refresh(|| fetcher.fetch(3)).await;
        assert_eq!(fetcher.calls(), 2);
        assert_eq!(after.data.as_deref(), Some(&3));
    }

    #[tokio::test]
    async fn the_ttl_boundary_is_exclusive() {
        let (cache, clock) = cache(Duration::from_secs(1));
        let fetcher = Fetcher::new();
        cache.get_or_refresh(|| fetcher.fetch(1)).await;

        // Exactly at the TTL the entry counts as expired: serving it would make
        // the boundary depend on sub-millisecond timing.
        clock.advance(1_000);
        cache.get_or_refresh(|| fetcher.fetch(2)).await;
        assert_eq!(fetcher.calls(), 2);
    }

    /// The reason the refresh lock is held across the fetch rather than only
    /// around the bookkeeping.
    #[tokio::test]
    async fn concurrent_callers_collapse_into_one_fetch() {
        let (cache, _clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();

        let calls: Vec<_> = (0..8)
            .map(|i| {
                let cache = &cache;
                let fetcher = fetcher.clone();
                async move { cache.get_or_refresh(|| fetcher.fetch(i)).await }
            })
            .collect();
        let snapshots = futures::future::join_all(calls).await;

        assert_eq!(fetcher.calls(), 1, "eight concurrent callers, one Docker call");
        for snapshot in snapshots {
            assert!(
                snapshot.data.is_some(),
                "every caller gets an answer, including the ones that waited"
            );
        }
    }

    #[tokio::test]
    async fn a_failure_keeps_the_last_good_value_and_records_the_error() {
        let (cache, clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();

        cache.get_or_refresh(|| fetcher.fetch(5)).await;
        clock.advance(3_000);
        fetcher.fail_with("docker is not running");

        let snapshot = cache.get_or_refresh(|| fetcher.fetch(6)).await;
        assert_eq!(
            snapshot.data.as_deref(),
            Some(&5),
            "a transient failure must not blank a list the user is reading"
        );
        assert_eq!(snapshot.err.as_deref(), Some("docker is not running"));
        assert_eq!(
            snapshot.updated_at,
            cache.peek().updated_at,
            "a failed refresh does not make the value look newer"
        );
    }

    #[tokio::test]
    async fn a_failure_with_no_previous_value_has_nothing_to_serve() {
        let (cache, _clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();
        fetcher.fail_with("cannot connect");

        let snapshot = cache.get_or_refresh(|| fetcher.fetch(1)).await;
        assert!(snapshot.data.is_none());
        assert_eq!(snapshot.err.as_deref(), Some("cannot connect"));
    }

    /// Without this a failing backend would serve an empty list for a whole TTL,
    /// which reads as "you have no containers".
    #[tokio::test]
    async fn a_cache_with_no_value_is_never_fresh() {
        let (cache, clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();
        fetcher.fail_with("down");

        cache.get_or_refresh(|| fetcher.fetch(1)).await;
        cache.get_or_refresh(|| fetcher.fetch(2)).await;
        assert_eq!(
            fetcher.calls(),
            2,
            "no value means every request retries, however recent the attempt"
        );
        assert!(!cache.peek().is_fresh(Duration::from_secs(2), clock.base));
    }

    #[tokio::test]
    async fn a_later_success_clears_the_error() {
        let (cache, clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();
        fetcher.fail_with("down");
        cache.get_or_refresh(|| fetcher.fetch(1)).await;

        fetcher.succeed();
        clock.advance(3_000);
        let snapshot = cache.get_or_refresh(|| fetcher.fetch(2)).await;
        assert!(snapshot.err.is_none(), "the error belonged to the old attempt");
        assert_eq!(snapshot.data.as_deref(), Some(&2));
    }

    #[tokio::test]
    async fn invalidate_forces_a_refetch() {
        let (cache, _clock) = cache(Duration::from_secs(60));
        let fetcher = Fetcher::new();
        cache.get_or_refresh(|| fetcher.fetch(1)).await;
        cache.get_or_refresh(|| fetcher.fetch(2)).await;
        assert_eq!(fetcher.calls(), 1);

        cache.invalidate();
        let snapshot = cache.get_or_refresh(|| fetcher.fetch(3)).await;
        assert_eq!(fetcher.calls(), 2, "a mutation must be visible immediately");
        assert_eq!(snapshot.data.as_deref(), Some(&3));
    }

    #[tokio::test]
    async fn invalidate_clears_a_recorded_error_too() {
        let (cache, _clock) = cache(Duration::from_secs(60));
        let fetcher = Fetcher::new();
        fetcher.fail_with("down");
        cache.get_or_refresh(|| fetcher.fetch(1)).await;
        assert!(cache.peek().err.is_some());

        cache.invalidate();
        let snapshot = cache.peek();
        assert!(snapshot.err.is_none(), "the error was about the value that is gone");
        assert!(snapshot.data.is_none());
    }

    #[tokio::test]
    async fn age_reports_how_stale_the_value_is() {
        let (cache, clock) = cache(Duration::from_secs(2));
        let fetcher = Fetcher::new();

        assert!(cache.peek().age(clock.base).is_none(), "no value, no age");
        cache.get_or_refresh(|| fetcher.fetch(1)).await;
        clock.advance(500);
        let snapshot = cache.peek();
        assert_eq!(snapshot.age(clock.base + Duration::from_millis(500)), Some(Duration::from_millis(500)));
    }
}
