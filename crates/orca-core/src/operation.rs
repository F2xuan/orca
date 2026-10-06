//! First-class records for long-running operations.
//!
//! Before this existed, a `docker compose up`, an image pull, or a template
//! deploy was a request you waited on and then a sentence in a toast. If the
//! client went away — a closed tab, a reload, a dropped connection — the
//! outcome was simply lost, and nothing on disk said the operation had ever
//! been attempted.
//!
//! An [`OperationRecord`] fixes that by being written *before* the work starts
//! and updated when it ends, so the operation outlives the request that
//! triggered it. Two consequences worth stating plainly:
//!
//! * History is now queryable (`GET /operations`), which is what makes a
//!   failed background pull diagnosable after the fact.
//! * The actor is recorded, which turns "something pulled an image at 03:14"
//!   into "the scheduler did", or — more to the point — "an AI agent did".
//!
//! What this deliberately is **not**: a job queue, and not a cancellation
//! mechanism. There is no way to stop a recorded operation through this type,
//! and no scheduling. Recording is a side-channel over the existing execution
//! model, so it cannot change how (or whether) an operation runs.

use serde::{Deserialize, Serialize};

/// How many records are kept. Old ones are dropped from the end.
///
/// Sized so that a busy daemon keeps a useful recent window without the file
/// growing without bound — the failure mode of an unbounded operation log is a
/// config directory that only ever grows.
pub const MAX_RETENTION: usize = 200;

/// The kind of work an operation performs.
///
/// An enum rather than a free-form string so a client can switch on it, with
/// `Unknown` for forward compatibility: a newer daemon may record a kind this
/// build has never heard of, and an older client must still be able to list the
/// history rather than fail to parse it. (Same convention as `BuildStatus` and
/// `DeployStatus`.)
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    PullImage,
    PruneImages,
    ComposeUp,
    ComposeDown,
    DeployTemplate,
    /// Cloning or downloading a build context from a URL.
    ImportContext,
    /// Cloning and building an image from a repository.
    BuildFromRepo,
    /// A mutating agent tool with no more specific kind above.
    ///
    /// The long tail of agent tools is better described by `target` (which
    /// carries the tool name) than by a kind variant per tool — and adding one
    /// variant per tool would make `OperationKind` grow every time the agent
    /// gains a capability.
    AgentTool,
    #[serde(other)]
    Unknown,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PullImage => "pull_image",
            Self::PruneImages => "prune_images",
            Self::ComposeUp => "compose_up",
            Self::ComposeDown => "compose_down",
            Self::DeployTemplate => "deploy_template",
            Self::ImportContext => "import_context",
            Self::BuildFromRepo => "build_from_repo",
            Self::AgentTool => "agent_tool",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Running,
    Success,
    Failed,
    /// Ended because the daemon restarted, not because it errored. Distinct
    /// from `Failed` so a restart does not look like a bug in the operation.
    Interrupted,
    #[serde(other)]
    Unknown,
}

impl OperationStatus {
    /// Whether no further transition is expected. `Running` and `Unknown` are
    /// the only non-terminal states.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Success | Self::Failed | Self::Interrupted)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
}

/// Who asked for the operation.
///
/// Derived from the *code path* that started it, never from a request header.
/// A client-supplied actor would be a claim, and the interesting question this
/// field answers — "was this a person or an automated actor?" — deserves an
/// answer the caller cannot forge. It is still not an *identity*: Orca's API
/// token is a single shared secret, so two humans are indistinguishable and
/// both read as `User`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationActor {
    /// A human-driven API call: the GUI, curl, or the CLI.
    User,
    /// An AI agent tool invocation.
    Agent,
    /// The built-in scheduler.
    Scheduler,
    /// An inbound webhook (auto-deploy).
    Webhook,
    #[serde(other)]
    Unknown,
}

impl OperationActor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Scheduler => "scheduler",
            Self::Webhook => "webhook",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String,
    pub kind: OperationKind,
    /// What the operation acted on: an image reference, a stack name, a
    /// template id. Free-form on purpose — a field that can reject its input is
    /// a field that stops the record from being written at all.
    pub target: String,
    pub actor: OperationActor,
    pub status: OperationStatus,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub error: Option<String>,
}

/// The `/operations` response body.
///
/// A type rather than a `serde_json::json!` literal, so its key names are
/// compiler-checked and generated into TypeScript instead of existing as string
/// literals in two languages. Same change as `AlertsResponse` (§30).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct OperationsResponse {
    pub count: usize,
    pub operations: Vec<OperationRecord>,
}

impl OperationRecord {
    /// A record in the `Running` state, ready to be persisted before the work
    /// begins.
    pub fn start(
        id: impl Into<String>,
        kind: OperationKind,
        target: impl Into<String>,
        actor: OperationActor,
        now: &str,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            target: target.into(),
            actor,
            status: OperationStatus::Running,
            started_at: now.to_string(),
            finished_at: None,
            error: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status == OperationStatus::Running
    }

    /// Move the record to a terminal state.
    ///
    /// A caller that passes a non-terminal status is a bug, so it is reported
    /// rather than silently accepted — but the record is still closed, because
    /// leaving it `Running` would be the worse outcome.
    pub fn finish(&mut self, status: OperationStatus, error: Option<String>, now: &str) {
        if !status.is_terminal() {
            tracing::warn!(
                operation = %self.id,
                status = status.as_str(),
                "operation finished with a non-terminal status; closing as failed"
            );
            self.status = OperationStatus::Failed;
            self.error = Some(match error {
                Some(detail) => format!("{detail} (non-terminal status {:?})", status),
                None => format!("operation reported non-terminal status {:?}", status),
            });
        } else {
            self.status = status;
            self.error = error;
        }
        self.finished_at = Some(now.to_string());
    }

    /// Close a record left `Running` by a process that died.
    ///
    /// Used by startup reconciliation. `Interrupted` rather than `Failed`
    /// because nothing about the operation went wrong — the daemon did.
    pub fn interrupt(&mut self, now: &str) -> bool {
        if !self.is_running() {
            return false;
        }
        self.status = OperationStatus::Interrupted;
        self.finished_at = Some(now.to_string());
        self.error = Some("interrupted: the daemon restarted while this was running".into());
        true
    }
}

/// Insert a record, newest first, dropping the oldest beyond [`MAX_RETENTION`].
///
/// Retention is enforced here rather than at the write site so every caller
/// gets it. An operation log that grows forever is a support ticket waiting to
/// happen, and the cost of the cap is only ever losing the least recent
/// history.
pub fn push_capped(records: &mut Vec<OperationRecord>, record: OperationRecord) {
    records.insert(0, record);
    records.truncate(MAX_RETENTION);
}

/// Close every `Running` record, returning how many changed.
///
/// Pure, so startup reconciliation can be tested without touching a real file.
pub fn interrupt_running(records: &mut [OperationRecord], now: &str) -> usize {
    // A loop rather than `iter_mut().filter(..).count()`: `filter` hands the
    // closure a `&&mut`, which cannot be borrowed mutably.
    let mut interrupted = 0;
    for record in records.iter_mut() {
        if record.interrupt(now) {
            interrupted += 1;
        }
    }
    interrupted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape that `gui/src/lib/types.ts` declares by hand — the
    /// counterpart of the same test in `alert.rs`, and for the same reason.
    #[test]
    fn the_operation_wire_shape_matches_the_hand_written_typescript() {
        let mut record = running("op1");
        let value = serde_json::to_value(&record).expect("a record serialises");
        let obj = value.as_object().expect("an object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        // `finished_at` and `error` are absent while it is still running, so
        // types.ts must declare them optional rather than nullable.
        assert_eq!(keys, ["actor", "id", "kind", "started_at", "status", "target"]);

        assert_eq!(obj["kind"], "compose_up");
        assert_eq!(obj["status"], "running");
        assert_eq!(obj["actor"], "user");

        record.finish(OperationStatus::Failed, Some("boom".into()), "2026-01-01T00:05:00+00:00");
        let value = serde_json::to_value(&record).expect("a finished record serialises");
        let obj = value.as_object().expect("an object");
        assert_eq!(obj["status"], "failed");
        assert_eq!(obj["error"], "boom");
        assert_eq!(obj["finished_at"], "2026-01-01T00:05:00+00:00");
    }

    fn running(id: &str) -> OperationRecord {
        OperationRecord::start(
            id,
            OperationKind::ComposeUp,
            "mystack",
            OperationActor::User,
            "2026-01-01T00:00:00+00:00",
        )
    }

    #[test]
    fn a_started_record_is_running_and_open() {
        let record = running("a");
        assert_eq!(record.status, OperationStatus::Running);
        assert!(record.is_running());
        assert!(record.finished_at.is_none());
        assert!(record.error.is_none());
        assert_eq!(record.target, "mystack");
    }

    #[test]
    fn finishing_records_status_error_and_time() {
        let mut record = running("a");
        record.finish(
            OperationStatus::Failed,
            Some("compose up failed".into()),
            "2026-01-01T00:01:00+00:00",
        );
        assert_eq!(record.status, OperationStatus::Failed);
        assert_eq!(record.error.as_deref(), Some("compose up failed"));
        assert_eq!(
            record.finished_at.as_deref(),
            Some("2026-01-01T00:01:00+00:00")
        );
        assert!(!record.is_running());
    }

    #[test]
    fn a_successful_finish_carries_no_error() {
        let mut record = running("a");
        record.finish(OperationStatus::Success, None, "2026-01-01T00:01:00+00:00");
        assert_eq!(record.status, OperationStatus::Success);
        assert!(record.error.is_none());
    }

    /// Closing as `Running` would leave the record looking live forever, which
    /// is the exact failure this type exists to prevent.
    #[test]
    fn a_non_terminal_finish_closes_the_record_anyway() {
        let mut record = running("a");
        record.finish(OperationStatus::Running, Some("huh".into()), "2026-01-01T00:01:00+00:00");
        assert_eq!(record.status, OperationStatus::Failed);
        assert!(record.finished_at.is_some(), "record must not be left open");
        assert!(record.error.as_deref().unwrap_or_default().contains("huh"));
    }

    #[test]
    fn only_running_records_are_interrupted() {
        let mut records = vec![
            running("a"),
            {
                let mut done = running("b");
                done.finish(OperationStatus::Success, None, "t");
                done
            },
            {
                let mut failed = running("c");
                failed.finish(OperationStatus::Failed, Some("x".into()), "t");
                failed
            },
            {
                let mut already = running("d");
                already.interrupt("t");
                already
            },
        ];

        assert_eq!(interrupt_running(&mut records, "2026-01-01T01:00:00+00:00"), 1);
        assert_eq!(records[0].status, OperationStatus::Interrupted);
        assert_eq!(records[1].status, OperationStatus::Success, "success is left alone");
        assert_eq!(records[2].status, OperationStatus::Failed, "failure is left alone");
        assert_eq!(records[3].status, OperationStatus::Interrupted, "idempotent");
        assert_eq!(
            records[0].finished_at.as_deref(),
            Some("2026-01-01T01:00:00+00:00")
        );
    }

    #[test]
    fn interrupted_is_distinct_from_failed() {
        // A restart is not a bug in the operation, and the UI should not have to
        // guess by pattern-matching the error text.
        let mut record = running("a");
        record.interrupt("t");
        assert_ne!(record.status, OperationStatus::Failed);
        assert_eq!(record.status.as_str(), "interrupted");
    }

    #[test]
    fn retention_drops_the_oldest() {
        let mut records: Vec<OperationRecord> = Vec::new();
        for i in 0..MAX_RETENTION + 5 {
            push_capped(&mut records, running(&format!("op{i}")));
        }
        assert_eq!(records.len(), MAX_RETENTION);
        // Newest first: the last inserted is at the front …
        assert_eq!(records[0].id, format!("op{}", MAX_RETENTION + 4));
        // … and the five oldest are gone.
        assert!(!records.iter().any(|r| r.id == "op0"));
        assert!(!records.iter().any(|r| r.id == "op4"));
        assert!(records.iter().any(|r| r.id == "op5"));
    }

    #[test]
    fn push_capped_puts_newest_first() {
        let mut records = vec![running("old")];
        push_capped(&mut records, running("new"));
        assert_eq!(records[0].id, "new");
        assert_eq!(records[1].id, "old");
    }

    #[test]
    fn statuses_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&OperationStatus::Interrupted).unwrap(),
            "\"interrupted\""
        );
        assert_eq!(
            serde_json::to_string(&OperationKind::ComposeDown).unwrap(),
            "\"compose_down\""
        );
        assert_eq!(
            serde_json::to_string(&OperationActor::Agent).unwrap(),
            "\"agent\""
        );
    }

    /// An older client must be able to read a log written by a newer daemon.
    #[test]
    fn unknown_variants_deserialize_instead_of_failing() {
        let status: OperationStatus = serde_json::from_str("\"quantum_superposition\"").unwrap();
        assert_eq!(status, OperationStatus::Unknown);
        let kind: OperationKind = serde_json::from_str("\"teleport_container\"").unwrap();
        assert_eq!(kind, OperationKind::Unknown);
        let actor: OperationActor = serde_json::from_str("\"robot\"").unwrap();
        assert_eq!(actor, OperationActor::Unknown);
        // Unknown is not terminal, so a record in that state stays "open" and
        // will be picked up by the next startup sweep rather than being
        // silently treated as finished.
        assert!(!OperationStatus::Unknown.is_terminal());
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let mut record = running("a");
        record.finish(OperationStatus::Success, None, "2026-01-01T00:01:00+00:00");
        let json = serde_json::to_string(&record).unwrap();
        let back: OperationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, record.id);
        assert_eq!(back.kind, record.kind);
        assert_eq!(back.status, record.status);
        assert_eq!(back.actor, record.actor);
        assert_eq!(back.finished_at, record.finished_at);
    }
}
