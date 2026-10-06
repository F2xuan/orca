//! Persistence for long-running operation records.
//!
//! See `orca_core::operation` for what these records mean and why they exist.
//! This module owns the file format and the read-modify-write cycle.
//!
//! Two properties the implementation has to preserve:
//!
//! * **A record is on disk before the work starts.** Everything that makes an
//!   operation record useful after a crash depends on the "running" row already
//!   existing when the process dies. Recording at the end only would reproduce
//!   the problem this replaces.
//! * **The file is never left half-written.** Writes go to a temp file and are
//!   renamed into place, so a crash mid-write leaves the previous complete
//!   history rather than a truncated JSON document that fails to parse and
//!   silently discards every record.
//!
//! Cross-process locking is not used: the daemon refuses to start if another
//! instance already holds the port (`main.rs`), so it is the only writer of this
//! file. Config persistence needs a file lock because the CLI writes
//! `config.json` too; nothing outside the daemon writes operation history.

use std::path::PathBuf;

use orca_core::operation::{
    self, OperationActor, OperationKind, OperationRecord, OperationStatus,
};

/// Serialises every read-modify-write cycle so two concurrent operations cannot
/// clobber each other's records.
static OPERATIONS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Test-only override for the platform data directory. Always `None` in
/// production.
///
/// A `OnceLock` rather than a `#[cfg(test)] static` so that the read path is
/// identical in both builds — the code under test is the code that ships. And
/// not an environment variable, because a test calling `std::env::set_var`
/// mutates process-global state that other threads may be reading, which edition
/// 2024 declares undefined.
static DATA_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Point operation storage at `dir`, once. Returns `Err` if already set.
///
/// Exists so a test can exercise the real read-modify-write path against a temp
/// directory instead of the user's data directory. It would also be the hook for
/// a `--data-dir` flag, if one is ever added.
#[cfg(test)]
pub fn set_data_dir(dir: PathBuf) -> Result<(), PathBuf> {
    DATA_DIR_OVERRIDE.set(dir)
}

fn data_dir() -> PathBuf {
    DATA_DIR_OVERRIDE
        .get()
        .cloned()
        .unwrap_or_else(|| dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")))
}

fn operations_dir() -> PathBuf {
    data_dir().join("orca").join("operations")
}

fn index_path() -> PathBuf {
    operations_dir().join("index.json")
}

/// Read the history. A missing or unparseable file reads as empty rather than
/// failing, so a corrupt log cannot stop the daemon from starting.
pub fn load() -> Vec<OperationRecord> {
    let path = index_path();
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Write the history atomically, newest first, capped at `MAX_RETENTION`.
fn save(records: &[OperationRecord]) {
    let dir = operations_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("operations: cannot create {}: {e}", dir.display());
        return;
    }

    let json = match serde_json::to_string_pretty(records) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("operations: cannot serialise history: {e}");
            return;
        }
    };

    // Temp file in the same directory so the rename is atomic (a rename across
    // filesystems is a copy, and not atomic).
    let unique = format!(
        "{}.{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let tmp = dir.join(format!(".index.json.tmp.{unique}"));
    let path = index_path();

    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        // Operations name images, stack names, and paths the user chose. Not
        // secret by design, but 0600 costs nothing and keeps the whole data
        // directory uniformly private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    };

    if let Err(e) = write() {
        tracing::warn!("operations: cannot write {}: {e}", tmp.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        tracing::warn!("operations: cannot replace {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Build a unique operation id.
///
/// Millisecond timestamp plus 16 random bits, matching the build-record scheme:
/// two operations started in the same millisecond still get distinct ids, and
/// the id sorts roughly by time, which makes a raw `index.json` readable.
fn new_id() -> String {
    use rand::RngCore;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // 64 random bits, not 16. Every id generated inside the same millisecond
    // shares `millis`, so 16 bits gave ~200 ids in one millisecond a 26% chance
    // of a collision (birthday bound) — enough to make a 200-id loop flaky, and
    // in production enough to merge two records that are keyed by id.
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{millis:x}{:016x}", u64::from_be_bytes(bytes))
}

/// Record the start of an operation and return the (already persisted) record.
///
/// The write happens here, before the caller does any work, so a crash or a
/// `pkill` mid-operation leaves evidence that it was attempted.
pub async fn begin(
    kind: OperationKind,
    target: impl Into<String>,
    actor: OperationActor,
) -> OperationRecord {
    let record = OperationRecord::start(
        new_id(),
        kind,
        target,
        actor,
        &chrono::Utc::now().to_rfc3339(),
    );

    let _guard = OPERATIONS_LOCK.lock().await;
    let mut records = load();
    operation::push_capped(&mut records, record.clone());
    save(&records);
    record
}

/// Move an operation to a terminal state.
///
/// A missing record is reported but not an error: retention may have dropped it
/// (a 200-record window on a busy daemon), and failing the caller for that would
/// turn a bookkeeping limit into a user-visible failure.
pub async fn finish(id: &str, status: OperationStatus, error: Option<String>) {
    let _guard = OPERATIONS_LOCK.lock().await;
    let mut records = load();
    match records.iter_mut().find(|record| record.id == id) {
        Some(record) => {
            record.finish(status, error, &chrono::Utc::now().to_rfc3339());
            save(&records);
        }
        None => tracing::warn!("operations: no record for '{id}' to finish"),
    }
}

/// The recorded history, newest first.
pub async fn list() -> Vec<OperationRecord> {
    let _guard = OPERATIONS_LOCK.lock().await;
    load()
}

/// A classifier that reports every outcome as success.
///
/// Pass this to [`run`] when the work signal failures by returning `Err`.
pub fn always_ok<T>(_: &T) -> Option<String> {
    None
}

/// Run `work` as a recorded operation and return its result unchanged.
///
/// Three behaviours worth stating, because each one is load-bearing:
///
/// * **The record is written before `work` starts.** An interrupt mid-operation
///   must leave evidence that it was attempted, which is only true if the row
///   already exists.
/// * **`classify` inspects the successful value.** Some backends report failure
///   *inside* a successful return — `docker compose up` yields `Ok(output)` with
///   a non-zero `exit_code` — and recording that as success would make the
///   history actively misleading. `Some(error)` records a failure.
/// * **`work` is spawned rather than awaited in place.** If the client
///   disconnects, axum drops the handler future; a spawned task is not dropped
///   with it, so the work runs to completion and closes its own record instead
///   of leaving a `Running` row for the next restart to sweep up.
pub async fn run<T, F, C>(
    kind: OperationKind,
    target: impl Into<String>,
    actor: OperationActor,
    work: F,
    classify: C,
) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    C: Fn(&T) -> Option<String> + Send + 'static,
{
    let record = begin(kind, target, actor).await;
    let id = record.id.clone();
    // Two handles: the task owns one, the panic branch below needs the other.
    let task_id = id.clone();

    let task = tokio::spawn(async move {
        let result = work.await;
        match &result {
            Ok(value) => match classify(value) {
                None => finish(&task_id, OperationStatus::Success, None).await,
                Some(error) => finish(&task_id, OperationStatus::Failed, Some(error)).await,
            },
            Err(e) => {
                // `{:#}` keeps the anyhow cause chain. The record is the only
                // surviving copy of why the operation failed, so flattening it
                // to the outermost message would throw away the diagnosis.
                finish(&task_id, OperationStatus::Failed, Some(format!("{e:#}"))).await
            }
        }
        result
    });

    match task.await {
        Ok(result) => result,
        // A panic never reaches the `finish` above, so close the record here
        // rather than leaving it `Running` forever.
        Err(join_error) => {
            finish(
                &id,
                OperationStatus::Failed,
                Some(format!("operation task panicked: {join_error}")),
            )
            .await;
            anyhow::bail!("operation task failed: {join_error}")
        }
    }
}

/// Close every operation left `Running` by a dead process.
///
/// Called from startup reconciliation under the same "only close things" rule
/// as the build sweep.
pub async fn interrupt_running_operations() -> usize {
    let _guard = OPERATIONS_LOCK.lock().await;
    let mut records = load();
    let interrupted = operation::interrupt_running(&mut records, &chrono::Utc::now().to_rfc3339());
    if interrupted > 0 {
        save(&records);
    }
    interrupted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_hex() {
        // 10,000 rather than a handful: these all land in the same millisecond
        // or two, so with too few random bits this fails *often* instead of
        // rarely, which is what makes it a regression test for the id width
        // rather than a coin flip. (At 16 bits, ~10,000 ids collide with
        // probability ~1; at 64 bits, with probability ~3e-12.)
        let ids: std::collections::HashSet<String> = (0..10_000).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 10_000, "ids must not collide");
        assert!(
            ids.iter().all(|id| id.chars().all(|c| c.is_ascii_hexdigit())),
            "ids stay filename- and URL-safe"
        );
    }

    /// The retention cap is enforced by the shared helper, so this asserts the
    /// daemon module actually routes through it rather than pushing directly.
    #[test]
    fn retention_is_enforced_through_the_shared_helper() {
        let mut records: Vec<OperationRecord> = Vec::new();
        for i in 0..(operation::MAX_RETENTION + 3) {
            operation::push_capped(
                &mut records,
                OperationRecord::start(
                    format!("op{i}"),
                    OperationKind::PullImage,
                    "alpine:3",
                    OperationActor::User,
                    "t",
                ),
            );
        }
        assert_eq!(records.len(), operation::MAX_RETENTION);
        assert_eq!(records[0].id, format!("op{}", operation::MAX_RETENTION + 2));
    }

    /// The single test that touches disk, and therefore the single test that
    /// claims the data directory. Merging them is deliberate: the override is a
    /// process-wide `OnceLock`, so several tests competing for it would make the
    /// suite order-dependent — and the "skip if already claimed" workaround
    /// would silently stop testing anything.
    ///
    /// Not mutating the environment here is also deliberate: edition 2024 makes
    /// `set_var` unsafe precisely because other threads may be reading it.
    #[test]
    fn operations_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("orca-ops-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_data_dir(dir.clone()).expect("exactly one test may claim the operations data dir");

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            // ---- A started operation is on disk before any work happens ----
            // This is the property everything else rests on: if the process dies
            // here, the attempt is still recorded.
            let started = begin(OperationKind::ComposeUp, "mystack", OperationActor::User).await;
            let on_disk = load();
            assert_eq!(on_disk.len(), 1);
            assert_eq!(on_disk[0].id, started.id);
            assert!(on_disk[0].is_running());
            assert_eq!(on_disk[0].target, "mystack");

            // ---- It becomes terminal, and the change is durable ----
            finish(&started.id, OperationStatus::Success, None).await;
            let after = load();
            assert_eq!(after[0].status, OperationStatus::Success);
            assert!(after[0].finished_at.is_some());

            // ---- A restart sweeps only what is still running ----
            let running =
                begin(OperationKind::PruneImages, "dangling", OperationActor::Scheduler).await;
            assert_eq!(interrupt_running_operations().await, 1);
            let swept = load();
            assert_eq!(
                swept.iter().find(|r| r.id == running.id).unwrap().status,
                OperationStatus::Interrupted
            );
            assert_eq!(
                swept.iter().find(|r| r.id == started.id).unwrap().status,
                OperationStatus::Success,
                "an already-finished record must survive the sweep untouched"
            );

            // ---- Newest first, which is what `list` promises the API ----
            assert_eq!(list().await[0].id, running.id);

            // ---- Finishing an unknown id is a no-op, not a failure ----
            // Retention may legitimately have dropped the row, and failing the
            // caller for that would turn a bookkeeping limit into a user-visible
            // error.
            finish("no-such-id", OperationStatus::Failed, Some("x".into())).await;
            assert_eq!(load().len(), 2);

            // ---- `run` passes the value through and records success ----
            let value = run(
                OperationKind::PullImage,
                "alpine:3",
                OperationActor::Agent,
                async { Ok::<_, anyhow::Error>(42u32) },
                always_ok,
            )
            .await
            .unwrap();
            assert_eq!(value, 42);
            let records = list().await;
            assert_eq!(records[0].status, OperationStatus::Success);
            assert_eq!(records[0].actor, OperationActor::Agent);

            // ---- A failure hidden inside a successful value is not a success ----
            // Mimics `ComposeOutput`: `Ok`, but the command exited non-zero.
            let value = run(
                OperationKind::ComposeUp,
                "mystack",
                OperationActor::User,
                async { Ok::<_, anyhow::Error>(7i32) },
                |exit_code| (*exit_code != 0).then(|| format!("compose up exited {exit_code}")),
            )
            .await
            .unwrap();
            assert_eq!(value, 7, "a classified failure is still returned to the caller");
            let records = list().await;
            assert_eq!(records[0].status, OperationStatus::Failed);
            assert_eq!(records[0].error.as_deref(), Some("compose up exited 7"));

            // ---- A propagated error keeps its cause chain ----
            let result: anyhow::Result<()> = run(
                OperationKind::ComposeDown,
                "mystack",
                OperationActor::User,
                async { Err(anyhow::anyhow!("inner: permission denied").context("compose down failed")) },
                always_ok,
            )
            .await;
            assert!(result.is_err());
            let error = list().await[0].error.clone().unwrap_or_default();
            assert!(error.contains("compose down failed"), "outer message kept: {error}");
            assert!(
                error.contains("permission denied"),
                "the cause chain is the only surviving diagnosis: {error}"
            );
        });

        let _ = std::fs::remove_dir_all(&dir);
    }
}
