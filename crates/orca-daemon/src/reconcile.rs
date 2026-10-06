//! Startup reconciliation.
//!
//! Converge state that a previous process left "in progress" into a terminal
//! state — once, before the HTTP server starts accepting requests.
//!
//! Two rules keep this safe to run on every start:
//!
//! * **It only ever closes things.** Nothing here creates a resource or deletes
//!   user data. The failure mode of a reconciler bug must be "too little was
//!   cleaned up", never "the user's history is gone" — a reconciler runs with
//!   no user watching and no way to ask.
//! * **"In progress at startup" is self-evidently stale.** The tasks that own
//!   those operations live in this process, which has not started any yet, so
//!   interpreting the state needs no heartbeat table, lock file, or clean
//!   shutdown marker. (A marker would in fact be worse: it would make
//!   reconciliation depend on the previous process having died politely.)
//!
//! The pattern comes from Komodo's startup reconcile pass; the borrowing notes
//! record which parts of Komodo's version were deliberately left out and why.

use crate::alerts;
use crate::build_manager;
use crate::operations;

/// What a reconciliation pass changed. Returned rather than only logged, so
/// startup can report it and tests can assert on it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Builds left `InProgress` by a dead process, now marked `Failed`.
    pub builds_interrupted: usize,
    /// Operations left `Running` by a dead process, now marked `Interrupted`.
    pub operations_interrupted: usize,
    /// Superseded alert-log rows dropped by compaction.
    pub alert_rows_dropped: usize,
}

impl ReconcileReport {
    /// True when nothing was stale — the normal case.
    pub fn is_clean(&self) -> bool {
        *self == Self::default()
    }
}

/// Reconcile everything that can be left mid-flight across a restart.
pub async fn reconcile_on_startup() -> ReconcileReport {
    let builds_interrupted = build_manager::interrupt_in_progress_builds().await;
    let operations_interrupted = operations::interrupt_running_operations().await;
    // The alert log is append-only, so superseded rows accumulate. Compacting
    // once at startup bounds it without making a live append read the file —
    // which is the property that makes appends race-free in the first place.
    let alert_rows_dropped = alerts::compact();

    // Deliberately *not* reconciled, so the next reader does not have to
    // re-derive it:
    //
    // * `deploy_history` (`OrcaConfig`) cannot go stale: `DeployStatus` has no
    //   in-progress variant, and every `DeployRecord` is pushed only *after*
    //   the redeploy call has returned, ok or err.
    // * `ProjectStatus` is derived from Docker compose labels on each read and
    //   never persisted, so it self-heals.
    // * Agent grants and alert lifecycles do not exist yet. When batch 2's
    //   alerts land they belong in this function, under the same "only close
    //   things" rule.
    //
    // Komodo also reconciles service/stack state by *deploying* it into the
    // desired shape at startup. That is intentionally not copied: Orca has no
    // declarative desired-state store, and a reconciler that starts containers
    // at boot would resurrect workloads the user stopped on purpose.

    ReconcileReport {
        builds_interrupted,
        operations_interrupted,
        alert_rows_dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_is_clean() {
        assert!(ReconcileReport::default().is_clean());
    }

    #[test]
    fn any_change_makes_the_report_dirty() {
        let report = ReconcileReport {
            builds_interrupted: 3,
            operations_interrupted: 0,
            alert_rows_dropped: 0,
        };
        assert!(!report.is_clean());

        let report = ReconcileReport {
            builds_interrupted: 0,
            operations_interrupted: 1,
            alert_rows_dropped: 0,
        };
        assert!(!report.is_clean());

        let report = ReconcileReport {
            builds_interrupted: 0,
            operations_interrupted: 0,
            alert_rows_dropped: 2,
        };
        assert!(!report.is_clean());
    }
}
