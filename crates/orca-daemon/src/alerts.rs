//! Alert persistence: an append-only JSONL log, folded on read.
//!
//! # Why append-only, when `config.json` is read-modify-write
//!
//! `config.json` is loaded, mutated, and rewritten by several paths, and Orca's
//! own audit (H18) records that as a source of lost updates. For alerts the
//! same shape would be worse: every new alert would rewrite the whole file, so
//! two concurrent evaluations could each drop the other's row, and a crash
//! mid-rewrite would lose **all** history rather than one line.
//!
//! Appending one JSON object per line removes both failure modes. Recording an
//! alert never reads the file, so there is nothing to race over; and a torn
//! final line — the only damage a crash can do — costs exactly that line.
//! Malformed lines are therefore *skipped*, not treated as corruption: losing
//! the last alert is acceptable, losing the log is not.
//!
//! The file is an event log of alert *states*, not a table of alerts. A resolve
//! is another line for the same id, and reading folds to the last line per id.
//! That is what keeps a close honest about history: the open row with its real
//! severity stays on disk.

use std::collections::HashMap;
use std::path::PathBuf;

use orca_core::alert::{Alert, AlertData, SeverityLevel};

/// How many distinct alerts are kept when the log is compacted.
///
/// Compaction runs at startup, so the bound applies to what is loaded into
/// memory and rewritten — the live file between restarts is only bounded by how
/// many alerts a session produces.
pub const MAX_ALERTS: usize = 500;

/// Test-only override, same shape and same reasoning as `operations.rs`: a
/// `OnceLock` rather than an environment variable, because a test calling
/// `std::env::set_var` mutates process-global state other threads may read.
static DATA_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Point alert storage at `dir`, once. Returns `Err` if already set.
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

fn alerts_dir() -> PathBuf {
    data_dir().join("orca").join("alerts")
}

fn log_path() -> PathBuf {
    alerts_dir().join("alerts.jsonl")
}

/// A unique alert id: millisecond timestamp plus 16 random bits, matching the
/// build-record and operation-id schemes so ids sort roughly by time and stay
/// filename-safe.
pub(crate) fn new_id(prefix: &str) -> String {
    use rand::RngCore;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // 64 random bits for the same reason as `operations::new_id`: the alert log
    // folds by id, so a collision would merge two alerts into one.
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}-{millis:x}{:016x}", u64::from_be_bytes(bytes))
}

/// Record a failed build.
///
/// Stored as an *already-resolved* point event: the failure has happened and has
/// no ongoing state, so it must answer "what has gone wrong" without sitting in
/// the open count forever — that is what would make the open count useless.
pub fn record_build_failure(id: &str, error: Option<&str>) {
    let alert = Alert::event(
        new_id("build"),
        SeverityLevel::Error,
        format!("build:{id}"),
        AlertData::BuildFailed {
            id: id.to_string(),
            error: error.map(str::to_string),
        },
        &chrono::Utc::now().to_rfc3339(),
    );
    tracing::info!("alert recorded: {}", alert.data.summary());
    append(&alert);
}

/// Close an open alert by hand, returning whether one was found.
///
/// Appends the resolved state rather than rewriting the open row, so the log
/// keeps both that the alert fired and that it was dismissed.
pub fn resolve(id: &str) -> bool {
    let Some(mut alert) = list().into_iter().find(|a| a.id == id && a.is_open()) else {
        return false;
    };
    alert.resolved = true;
    alert.resolved_ts = Some(chrono::Utc::now().to_rfc3339());
    append(&alert);
    true
}

/// Append one alert state to the log.
///
/// Uses a single `O_APPEND` write of one line. `O_APPEND` is what makes
/// concurrent appends safe without a lock: the kernel attaches each write to
/// the end atomically, so two writers cannot interleave halves of a line.
pub fn append(alert: &Alert) {
    use std::io::Write;

    let dir = alerts_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("alerts: cannot create {}: {e}", dir.display());
        return;
    }

    let mut line = match serde_json::to_string(alert) {
        Ok(line) => line,
        Err(e) => {
            tracing::warn!("alerts: cannot serialise alert {}: {e}", alert.id);
            return;
        }
    };
    line.push('\n');

    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(log_path()) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(line.as_bytes()) {
                tracing::warn!("alerts: cannot append: {e}");
            }
        }
        Err(e) => tracing::warn!("alerts: cannot open log: {e}"),
    }
}

/// Every alert state in the log, in file order.
///
/// Malformed lines are skipped with a warning. This deliberately tolerates a
/// half-written trailing line rather than failing the read.
pub fn load_states() -> Vec<Alert> {
    let path = log_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    let mut states = Vec::new();
    let mut skipped = 0usize;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Alert>(line) {
            Ok(alert) => states.push(alert),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        // Almost always one torn final line from a crash mid-append.
        tracing::warn!("alerts: skipped {skipped} unreadable line(s) in the alert log");
    }
    states
}

/// Fold the log to one row per alert id, newest first.
///
/// The *last* state for an id wins, which is what makes a resolve row close the
/// open row that preceded it. Ordering is by timestamp descending with the file
/// order as the tiebreak, so two alerts sharing a timestamp keep the order they
/// were written in rather than shuffling between reads.
pub fn fold(states: &[Alert]) -> Vec<Alert> {
    let mut latest: HashMap<&str, (usize, &Alert)> = HashMap::new();
    for (index, alert) in states.iter().enumerate() {
        latest.insert(alert.id.as_str(), (index, alert));
    }

    let mut folded: Vec<(usize, &Alert)> = latest.into_values().collect();
    // Stable sort by timestamp descending; file order breaks ties.
    folded.sort_by(|(index_a, a), (index_b, b)| b.ts.cmp(&a.ts).then(index_b.cmp(index_a)));
    folded.into_iter().map(|(_, alert)| alert.clone()).collect()
}

/// The current state of every alert, newest first.
pub fn list() -> Vec<Alert> {
    fold(&load_states())
}

/// Only the alerts that are still open.
pub fn open_alerts() -> Vec<Alert> {
    list().into_iter().filter(Alert::is_open).collect()
}

/// Rewrite the log as just the folded state, dropping superseded rows.
///
/// Runs at startup rather than on every append: an append must stay a single
/// write with no read, and a log that is compacted once per session is bounded
/// by `MAX_ALERTS` plus one session's traffic instead of growing forever.
///
/// The rewrite is atomic (temp file + rename), because this is the one
/// operation that *can* destroy history: a crash mid-truncate would leave an
/// empty or partial log.
pub fn compact() -> usize {
    let states = load_states();
    let before = states.len();
    let mut folded = fold(&states);

    // Keep the newest `MAX_ALERTS`. Open alerts are what a badge and a list
    // actually need, so they are never the ones dropped.
    if folded.len() > MAX_ALERTS {
        // `fold` is newest-first, so truncation would drop the oldest — but an
        // open alert's original timestamp can be old, so partition first.
        let (open, closed): (Vec<Alert>, Vec<Alert>) =
            folded.into_iter().partition(Alert::is_open);
        let room = MAX_ALERTS.saturating_sub(open.len());
        let mut kept = open;
        kept.extend(closed.into_iter().take(room));
        folded = kept;
    }

    save_atomically(&folded);
    before.saturating_sub(folded.len())
}

fn save_atomically(alerts: &[Alert]) {
    use std::io::Write;

    let dir = alerts_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("alerts: cannot create {}: {e}", dir.display());
        return;
    }

    let mut body = String::new();
    for alert in alerts {
        match serde_json::to_string(alert) {
            Ok(line) => {
                body.push_str(&line);
                body.push('\n');
            }
            Err(e) => tracing::warn!("alerts: cannot serialise alert {}: {e}", alert.id),
        }
    }

    let unique = format!(
        "{}.{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let tmp = dir.join(format!(".alerts.jsonl.tmp.{unique}"));

    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()
    };

    if let Err(e) = write() {
        tracing::warn!("alerts: cannot write {}: {e}", tmp.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, log_path()) {
        tracing::warn!("alerts: cannot replace the alert log: {e}");
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orca_core::alert::{AlertData, SeverityLevel};

    const T0: &str = "2026-01-01T00:00:00+00:00";
    const T1: &str = "2026-01-01T00:01:00+00:00";
    const T2: &str = "2026-01-01T00:02:00+00:00";

    fn data() -> AlertData {
        AlertData::Custom {
            message: "test".into(),
        }
    }

    fn open(id: &str, ts: &str) -> Alert {
        Alert::open(id, SeverityLevel::Error, "target", data(), ts)
    }

    fn resolved(id: &str, ts: &str, resolved_ts: &str) -> Alert {
        let mut alert = open(id, ts);
        alert.resolved = true;
        alert.resolved_ts = Some(resolved_ts.to_string());
        alert
    }

    // ---- fold: pure, so most of the behaviour is tested without disk ----

    /// A resolve row is the *same* alert with `resolved` set, so it repeats the
    /// original `ts` and adds `resolved_ts`. That is what keeps "started at T0,
    /// resolved at T1" recoverable from the log.
    ///
    /// Note the helper signature: `resolved(id, opened_at, resolved_at)`. The
    /// daemon produces resolve rows this way because `AlertEngine::recover`
    /// mutates the open alert in place and never rewrites its `ts`.
    #[test]
    fn the_last_state_for_an_id_wins() {
        let states = vec![open("a1", T0), resolved("a1", T0, T1)];
        let folded = fold(&states);
        assert_eq!(folded.len(), 1);
        assert!(folded[0].resolved, "the resolve row supersedes the open row");
        assert_eq!(folded[0].ts, T0, "when it started is preserved");
        assert_eq!(
            folded[0].resolved_ts.as_deref(),
            Some(T1),
            "and when it ended is recorded"
        );
    }

    #[test]
    fn distinct_alerts_are_all_kept() {
        let states = vec![open("a1", T0), open("a2", T1)];
        assert_eq!(fold(&states).len(), 2);
    }

    #[test]
    fn folded_output_is_newest_first() {
        let states = vec![open("old", T0), open("new", T2), open("mid", T1)];
        let folded = fold(&states);
        let ids: Vec<&str> = folded.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "mid", "old"]);
    }

    #[test]
    fn equal_timestamps_keep_file_order() {
        // Two alerts in the same evaluation share a timestamp; the order must not
        // shuffle between reads.
        let states = vec![open("first", T0), open("second", T0)];
        let folded = fold(&states);
        let ids: Vec<&str> = folded.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["second", "first"]);
    }

    #[test]
    fn folding_an_empty_log_is_empty() {
        assert!(fold(&[]).is_empty());
    }

    #[test]
    fn open_alerts_excludes_resolved_ones() {
        let states = vec![open("a1", T0), resolved("a2", T0, T1)];
        let folded = fold(&states);
        let open_only: Vec<&Alert> = folded.iter().filter(|a| a.is_open()).collect();
        assert_eq!(open_only.len(), 1);
        assert_eq!(open_only[0].id, "a1");
    }

    // ---- one disk test, for the same reason as operations.rs ----

    /// The single test that touches disk, and therefore the single test that
    /// claims the data directory.
    ///
    /// Merged rather than split for the reason this file's own comments give
    /// elsewhere: the override is a process-wide `OnceLock`, so several tests
    /// competing for it would make the suite order-dependent, and the "skip if
    /// already claimed" workaround would silently stop testing anything.
    #[test]
    fn the_alert_log_behaves_under_disk_conditions() {
        let dir = std::env::temp_dir().join(format!("orca-alerts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_data_dir(dir.clone()).expect("exactly one test may claim the alerts data dir");

        // ---- appending is durable and never reads ----
        append(&open("a1", T0));
        append(&open("a2", T1));
        assert_eq!(load_states().len(), 2);
        assert_eq!(list().len(), 2, "two alerts are open");
        assert_eq!(open_alerts().len(), 2);

        // ---- a resolve is one more line for the same id ----
        append(&resolved("a1", T0, T2));
        assert_eq!(load_states().len(), 3, "the log is append-only");
        let folded = list();
        assert_eq!(folded.len(), 2, "but folds to two alerts");
        assert_eq!(open_alerts().len(), 1, "and only a2 is still open");
        let a1 = folded.iter().find(|a| a.id == "a1").unwrap();
        assert!(a1.resolved);
        assert_eq!(a1.ts, T0, "history keeps when the alert started");

        // ---- compaction drops superseded rows and preserves state ----
        assert_eq!(compact(), 1, "the superseded open row is dropped");
        assert_eq!(load_states().len(), 2);
        assert_eq!(open_alerts().len(), 1, "state is unchanged by compaction");
        assert_eq!(compact(), 0, "compacting again is a no-op");

        // ---- a torn final line costs only that line ----
        // This is the whole reason for the format: a crash mid-append must not
        // lose the history.
        let _ = std::fs::remove_file(log_path());
        append(&open("good1", T0));
        append(&open("good2", T1));
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path())
                .unwrap();
            file.write_all(b"{\"id\":\"torn\",\"ts\":\"2026").unwrap();
        }
        assert_eq!(load_states().len(), 2, "both complete rows survive");
        assert_eq!(list().len(), 2);

        // ---- the cap never evicts an open alert for a closed one ----
        let _ = std::fs::remove_file(log_path());
        append(&open("old-open", T0));
        for i in 0..(MAX_ALERTS + 20) {
            append(&resolved(&format!("closed{i}"), T1, T2));
        }
        assert!(compact() > 0, "the log should have been trimmed");
        let remaining = list();
        assert!(remaining.len() <= MAX_ALERTS, "got {}", remaining.len());
        assert!(
            remaining.iter().any(|a| a.id == "old-open" && a.is_open()),
            "an open alert must never be dropped to make room for closed ones"
        );

        // ---- a failed build is recorded already-resolved ----
        // It must not sit in the open count forever: a build failure has no
        // ongoing state to clear.
        let _ = std::fs::remove_file(log_path());
        record_build_failure("build-7", Some("exit 1"));
        let after_build = list();
        assert_eq!(after_build.len(), 1);
        assert!(!after_build[0].is_open(), "a point event is terminal on arrival");
        assert_eq!(after_build[0].target, "build:build-7");
        assert_eq!(after_build[0].level, SeverityLevel::Error);
        assert!(
            after_build[0].data.summary().contains("exit 1"),
            "the reason travels with the alert: {}",
            after_build[0].data.summary()
        );
        assert!(open_alerts().is_empty());

        // ---- resolving by hand closes an open alert, and is idempotent ----
        append(&open("manual", T0));
        assert_eq!(open_alerts().len(), 1);
        assert!(resolve("manual"), "an open alert is dismissed");
        assert!(open_alerts().is_empty());
        assert!(
            list().iter().any(|a| a.id == "manual" && a.resolved),
            "the row recording that it fired is still in the log"
        );
        assert!(!resolve("manual"), "dismissing twice is not an error path");
        assert!(!resolve("never-existed"), "an unknown id is not found");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
