//! Alert lifecycle and de-bouncing.
//!
//! Orca's existing feedback is a toast: transient, unqueryable, and gone. An
//! alert here is instead a **row with a lifecycle** — opened, possibly
//! escalated, eventually resolved — so "is something wrong right now" and "what
//! has gone wrong" are answered by the same table. The mechanism is borrowed
//! from Komodo's monitor; the *parameters* deliberately are not (see below).
//!
//! # Why three de-bounce mechanisms and not one cooldown
//!
//! They suppress three different things, and collapsing them into a single
//! timer gets each of them wrong:
//!
//! 1. **Two-strike confirmation** for state changes. A container that is
//!    `restarting` for half a second is not news. Requiring a *second*
//!    consecutive bad observation removes the single-blip class of false alarm
//!    entirely, at the cost of one evaluation of latency. A single blip also
//!    must not *arm* anything — the counter resets on recovery, or two unrelated
//!    blips minutes apart would add up to one false alarm.
//! 2. **Hysteresis** for measurements. A disk at 90.0% crossing to 89.9% and
//!    back is not two events. The clear line sits *below* the fire line, so the
//!    value has to genuinely come down before the alert closes. The band is an
//!    absolute percentage, not a ratio: it means the same thing at 12% and at
//!    97%, which is what a percentage threshold wants.
//! 3. **One-shot suppression** for occurrences with no continuous condition —
//!    "a newer image exists". There is no level to compare, so the only correct
//!    behaviour is to notify once per episode and re-arm only when the
//!    condition *clears*. That is what makes "update available → user deals with
//!    it → a new update appears" notify twice rather than never again.
//!
//! # Severity only rises
//!
//! Within one open alert the level is monotonically non-decreasing. If something
//! is already `Error` and a later observation calls it `Warning`, the existing
//! alert is not downgraded: the operator has already been told how bad it is,
//! and silently softening that is worse than saying nothing. Recovery is the
//! only way down, and recovery is a distinct transition.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Consecutive bad observations required before a state alert opens.
///
/// Two, not three: one evaluation of latency, and it removes the single-blip
/// class outright. Raising it trades detection latency for fewer alarms, and at
/// this granularity two is the point where the trade stops paying.
pub const STRIKES_REQUIRED: u32 = 2;

/// Default gap between the fire line and the clear line, in absolute
/// percentage points.
pub const DEFAULT_HYSTERESIS: f64 = 5.0;

/// How bad an alert is.
///
/// `Ok` is not "fine, ignore this" — it is reserved for *recovery
/// notifications*, which are sent as `Ok` copies of an alert that has just
/// closed. Without it, "the disk is full" and "the disk is fine again" would
/// have to share a level, and a notification list could not distinguish them.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityLevel {
    Ok,
    Warning,
    Error,
    Critical,
}

impl SeverityLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }
}

/// What an alert is about.
///
/// Six variants carry everything Orca can detect today; `Unknown` exists so an
/// older reader can still load an alert log written by a newer daemon rather
/// than failing to parse the whole file.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlertData {
    ContainerExited {
        id: String,
        name: String,
        exit_code: i32,
    },
    ContainerUnhealthy {
        id: String,
        name: String,
    },
    ImageUpdateAvailable {
        image: String,
    },
    BuildFailed {
        id: String,
        /// Why it failed, when the builder said. An alert that only says "it
        /// failed" sends the reader hunting for the reason.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts-export", ts(optional))]
        error: Option<String>,
    },
    StackStateChange {
        name: String,
        state: String,
    },
    HostDiskHigh {
        mount: String,
        used_pct: u8,
    },
    Custom {
        message: String,
    },
    /// Reached by *reading*, not by writing: a log entry whose kind this build
    /// does not know parses to here, and is then re-serialised to clients as
    /// `"unknown"`. So it is on the wire and must stay in the generated
    /// TypeScript — `#[serde(other)]` constrains deserialisation only. §31.
    #[serde(other)]
    Unknown,
}

impl AlertData {
    /// A short, user-facing label. Kept here rather than in the UI so a log
    /// remains readable from the CLI and the API too.
    pub fn summary(&self) -> String {
        match self {
            Self::ContainerExited { name, exit_code, .. } => {
                format!("container '{name}' exited ({exit_code})")
            }
            Self::ContainerUnhealthy { name, .. } => format!("container '{name}' is unhealthy"),
            Self::ImageUpdateAvailable { image } => format!("a newer '{image}' is available"),
            Self::BuildFailed { id, error } => match error {
                Some(error) => format!("build {id} failed: {error}"),
                None => format!("build {id} failed"),
            },
            Self::StackStateChange { name, state } => format!("stack '{name}' is {state}"),
            Self::HostDiskHigh { mount, used_pct } => {
                format!("disk {mount} is {used_pct}% full")
            }
            Self::Custom { message } => message.clone(),
            Self::Unknown => "unknown alert".to_string(),
        }
    }
}

/// The `/alerts` response body.
///
/// Built as a type rather than a `serde_json::json!` literal so its key names are
/// checked by the compiler and generated into TypeScript, instead of living as
/// string literals in two languages.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct AlertsResponse {
    pub count: usize,
    /// Open alerts, regardless of the filter that produced `alerts`.
    pub open_count: usize,
    pub alerts: Vec<Alert>,
}

/// One alert row.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    /// When the alert opened (RFC 3339).
    pub ts: String,
    pub level: SeverityLevel,
    /// What the alert is about, e.g. a container id or an image reference.
    /// Stable across an alert's life, and the key the de-bouncers track.
    pub target: String,
    pub data: AlertData,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub resolved_ts: Option<String>,
}

impl Alert {
    /// An alert that is open now.
    pub fn open(
        id: impl Into<String>,
        level: SeverityLevel,
        target: impl Into<String>,
        data: AlertData,
        ts: &str,
    ) -> Self {
        Self {
            id: id.into(),
            ts: ts.to_string(),
            level,
            target: target.into(),
            data,
            resolved: false,
            resolved_ts: None,
        }
    }

    /// An alert for something that already happened and has no ongoing state —
    /// a build failure, a container exit.
    ///
    /// It is stored already terminal, on purpose: "what has gone wrong" is a
    /// question about history, and a point event that needed action long ago
    /// must not sit in the open count forever.
    pub fn event(
        id: impl Into<String>,
        level: SeverityLevel,
        target: impl Into<String>,
        data: AlertData,
        ts: &str,
    ) -> Self {
        Self {
            id: id.into(),
            ts: ts.to_string(),
            level,
            target: target.into(),
            data,
            resolved: true,
            resolved_ts: Some(ts.to_string()),
        }
    }

    pub fn is_open(&self) -> bool {
        !self.resolved
    }

    /// The notification that accompanies a close: a copy at `Ok`.
    ///
    /// The stored row keeps its original severity, because history should record
    /// how bad it got. Only the *notification* says "recovered".
    pub fn as_recovery_notification(&self) -> Self {
        Self {
            level: SeverityLevel::Ok,
            ..self.clone()
        }
    }
}

/// A measured threshold with a hysteresis band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    /// The value at or above which the alert fires.
    pub fire_at: f64,
    /// How far the value must fall *below* `fire_at` before the alert clears.
    pub hysteresis: f64,
}

impl Threshold {
    pub fn new(fire_at: f64) -> Self {
        Self {
            fire_at,
            hysteresis: DEFAULT_HYSTERESIS,
        }
    }

    pub fn with_hysteresis(fire_at: f64, hysteresis: f64) -> Self {
        Self {
            fire_at,
            hysteresis,
        }
    }

    /// Whether the value is bad enough to fire.
    pub fn is_breaching(&self, value: f64) -> bool {
        value >= self.fire_at
    }

    /// Whether the value has come down far enough to clear.
    ///
    /// Strictly below the clear line: at exactly `fire_at - hysteresis` the
    /// alert stays open. The band is meant to require a real retreat, and a
    /// boundary that clears on equality makes the band one increment narrower
    /// than it reads.
    pub fn is_cleared(&self, value: f64) -> bool {
        value < self.fire_at - self.hysteresis
    }
}

/// What an evaluation decided.
///
/// `Recover` carries two rows because a close does two things: it marks the
/// stored alert resolved, and it produces a notification. Those are not the same
/// row — see [`Alert::as_recovery_notification`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// A new open alert.
    Open(Alert),
    /// An already-open alert got worse.
    Escalate(Alert),
    /// The condition cleared.
    Recover { stored: Alert, notification: Alert },
}

impl Transition {
    /// The row to persist.
    pub fn persisted(&self) -> &Alert {
        match self {
            Self::Open(alert) | Self::Escalate(alert) => alert,
            Self::Recover { stored, .. } => stored,
        }
    }

    /// The row to notify with, if this transition is worth telling someone about.
    pub fn notification(&self) -> &Alert {
        match self {
            Self::Open(alert) | Self::Escalate(alert) => alert,
            Self::Recover { notification, .. } => notification,
        }
    }
}

#[derive(Debug, Default)]
struct ConditionState {
    /// Consecutive bad observations since the last healthy one.
    breaches: u32,
    /// The alert currently open for this target, if any.
    open: Option<Alert>,
}

#[derive(Debug, Default)]
struct MetricState {
    firing: bool,
    open: Option<Alert>,
}

#[derive(Debug, Default)]
struct OnceState {
    open: Option<Alert>,
}

/// Tracks per-target de-bounce state and decides when to open, escalate, and
/// close alerts.
///
/// Pure and clock-free: callers pass timestamps and ids. That is what makes
/// every timing rule above testable without sleeping, and keeps the engine
/// usable from the daemon, the CLI, and tests identically.
#[derive(Debug, Default)]
pub struct AlertEngine {
    conditions: HashMap<String, ConditionState>,
    metrics: HashMap<String, MetricState>,
    /// Kept separate from `conditions` so a target string used by two
    /// mechanisms cannot make them share one alert slot.
    once: HashMap<String, OnceState>,
    /// Targets that have already been notified for their current episode.
    notified_once: HashSet<String>,
}

impl AlertEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every alert currently open, across all three mechanisms.
    ///
    /// The daemon's zombie-closer needs this: to close the alerts whose subject
    /// is gone it must first be able to enumerate what is open, and reconstructing
    /// that from the alert log would read a stale copy.
    pub fn open_alerts(&self) -> Vec<Alert> {
        let mut alerts: Vec<Alert> = self
            .conditions
            .values()
            .filter_map(|state| state.open.clone())
            .chain(self.metrics.values().filter_map(|state| state.open.clone()))
            .chain(self.once.values().filter_map(|state| state.open.clone()))
            .collect();
        // Sorted by id so a caller acting on several does not depend on
        // HashMap iteration order.
        alerts.sort_by(|a, b| a.id.cmp(&b.id));
        alerts
    }

    /// How many alerts are open. Useful for a badge and for tests.
    pub fn open_count(&self) -> usize {
        self.conditions
            .values()
            .filter(|state| state.open.is_some())
            .count()
            + self.metrics.values().filter(|state| state.open.is_some()).count()
            + self.once.values().filter(|state| state.open.is_some()).count()
    }

    /// Evaluate a yes/no condition, with two-strike confirmation.
    ///
    /// Returns `None` while an alert is merely *pending* (one strike) — nothing
    /// is written and nothing is notified, because a single observation is not
    /// yet a fact worth recording.
    pub fn observe_condition(
        &mut self,
        target: &str,
        healthy: bool,
        level: SeverityLevel,
        data: AlertData,
        ts: &str,
        id: &str,
    ) -> Option<Transition> {
        let state = self.conditions.entry(target.to_string()).or_default();

        if healthy {
            // Reset the strike counter *and* close anything open. Resetting
            // matters as much as closing: leaving the count armed would let two
            // unrelated blips minutes apart add up to one false alarm.
            state.breaches = 0;
            return state
                .open
                .take()
                .map(|open| Self::recover(open, ts));
        }

        state.breaches = state.breaches.saturating_add(1);
        if state.breaches < STRIKES_REQUIRED {
            return None;
        }

        match &state.open {
            None => {
                let alert = Alert::open(id, level, target, data, ts);
                state.open = Some(alert.clone());
                Some(Transition::Open(alert))
            }
            // Severity only rises: a lower level on an open alert is ignored
            // rather than downgrading what the operator has already been told.
            Some(open) if level > open.level => {
                let mut escalated = open.clone();
                escalated.level = level;
                escalated.data = data;
                state.open = Some(escalated.clone());
                Some(Transition::Escalate(escalated))
            }
            Some(_) => None,
        }
    }

    /// Evaluate a measurement against a threshold, with hysteresis.
    ///
    /// One parameter over clippy's limit. The real fix is to group the five
    /// fields every observation shares — `target`/`level`/`data`/`ts`/`id` —
    /// into an `Observation` struct, which `observe_condition` and
    /// `observe_once` should take as well. That is a redesign of ~50 call sites,
    /// so it is not being smuggled into this change; this suppression is
    /// recorded here rather than left for someone to rediscover.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_metric(
        &mut self,
        target: &str,
        value: f64,
        threshold: Threshold,
        level: SeverityLevel,
        data: AlertData,
        ts: &str,
        id: &str,
    ) -> Option<Transition> {
        let state = self.metrics.entry(target.to_string()).or_default();

        if !state.firing {
            if !threshold.is_breaching(value) {
                return None;
            }
            state.firing = true;
            let alert = Alert::open(id, level, target, data, ts);
            state.open = Some(alert.clone());
            return Some(Transition::Open(alert));
        }

        // Already firing: only a real retreat below the clear line closes it.
        if threshold.is_cleared(value) {
            state.firing = false;
            return state.open.take().map(|open| Self::recover(open, ts));
        }

        // Still inside the band, or worse.
        match &state.open {
            Some(open) if level > open.level => {
                let mut escalated = open.clone();
                escalated.level = level;
                escalated.data = data;
                state.open = Some(escalated.clone());
                Some(Transition::Escalate(escalated))
            }
            // A raise in reported severity while the alert was closed is not
            // possible here, but a metric with no open alert yet still firing is:
            // re-open rather than silently tracking nothing.
            None => {
                let alert = Alert::open(id, level, target, data, ts);
                state.open = Some(alert.clone());
                Some(Transition::Open(alert))
            }
            Some(_) => None,
        }
    }

    /// Report whether a one-shot condition is currently true.
    ///
    /// Notifies once per episode. It re-arms only when `active` goes false, so
    /// "update available → handled → a new update appears" notifies on both
    /// updates, while the same update is not reported every evaluation.
    pub fn observe_once(
        &mut self,
        target: &str,
        active: bool,
        level: SeverityLevel,
        data: AlertData,
        ts: &str,
        id: &str,
    ) -> Option<Transition> {
        if active {
            if self.notified_once.contains(target) {
                return None;
            }
            self.notified_once.insert(target.to_string());
            let alert = Alert::open(id, level, target, data, ts);
            self.once
                .entry(target.to_string())
                .or_default()
                .open = Some(alert.clone());
            return Some(Transition::Open(alert));
        }

        // Clearing re-arms for a future episode.
        self.notified_once.remove(target);
        let open = self.once.get_mut(target).and_then(|state| state.open.take());
        // A condition that was never notified produces no recovery: telling
        // someone "recovered" from something they were never told about is pure
        // noise.
        open.map(|open| Self::recover(open, ts))
    }

    fn recover(mut open: Alert, ts: &str) -> Transition {
        open.resolved = true;
        open.resolved_ts = Some(ts.to_string());
        let notification = open.as_recovery_notification();
        Transition::Recover {
            stored: open,
            notification,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape that `gui/src/lib/types.ts` declares by hand.
    ///
    /// Those TypeScript types cannot be generated in this environment (see §18
    /// of komodo-borrowings-triage.md), so this is the compensating control: if
    /// a field here is renamed, dropped, or stops being snake_case, this shape
    /// changes and the TypeScript declaration has to change with it. Without
    /// this, `tsc` would happily keep a stale type that no response matches.
    #[test]
    fn the_alert_wire_shape_is_pinned() {
        let alert = Alert::open(
            "a1",
            SeverityLevel::Warning,
            "disk:/",
            AlertData::HostDiskHigh {
                mount: "/".into(),
                used_pct: 92,
            },
            T0,
        );
        let value = serde_json::to_value(&alert).expect("an alert serialises");
        let obj = value.as_object().expect("an object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        // `resolved_ts` is absent until there is one to report.
        assert_eq!(keys, ["data", "id", "level", "resolved", "target", "ts"]);

        assert_eq!(obj["level"], "warning", "levels are snake_case strings");
        assert_eq!(obj["resolved"], false);
        assert_eq!(obj["ts"], T0);

        // Internally tagged: the variant lives in `kind` *beside* its fields.
        // The externally tagged shape would nest them under a variant key, and
        // every `data.kind === "..."` check in the GUI would silently fail.
        let data = obj["data"].as_object().expect("data is an object");
        assert_eq!(data["kind"], "host_disk_high");
        assert_eq!(data["mount"], "/");
        assert_eq!(data["used_pct"].as_u64(), Some(92), "numbers stay numbers");
        assert!(
            data.get("host_disk_high").is_none(),
            "types.ts expects the fields flat, not nested under the variant name"
        );

        // Resolving adds when it ended; it must not overwrite when it started.
        let mut resolved = alert.clone();
        resolved.resolved = true;
        resolved.resolved_ts = Some(T1.to_string());
        let value = serde_json::to_value(&resolved).expect("a resolved alert serialises");
        let obj = value.as_object().expect("an object");
        assert_eq!(obj["resolved_ts"], T1);
        assert_eq!(obj["ts"], T0, "the open time survives resolution");
        assert_eq!(obj["resolved"], true);
    }

    /// A kind this build does not recognise is not a parse error: it becomes the
    /// catch-all variant, and is then serialised back out — so it *is* on the
    /// wire.
    ///
    /// `#[serde(other)]` constrains deserialisation only. An earlier revision
    /// dropped the arm from `AlertData`'s TypeScript on the belief that "the
    /// daemon cannot emit it"; `gui/src/lib/daemonAlerts.ts` already contradicted
    /// that with a `default:` branch for exactly this case. See §31 of
    /// komodo-borrowings-triage.md.
    #[test]
    fn an_unrecognised_kind_round_trips_as_unknown() {
        let data: AlertData = serde_json::from_str(r#"{"kind":"from_the_future","x":1}"#)
            .expect("an unrecognised kind parses to the catch-all variant");
        assert_eq!(data, AlertData::Unknown);
        assert_eq!(
            serde_json::to_value(&data).expect("the catch-all variant serialises"),
            serde_json::json!({ "kind": "unknown" }),
            "the catch-all variant serialises, so a client can receive it"
        );
    }

    const T0: &str = "2026-01-01T00:00:00+00:00";
    const T1: &str = "2026-01-01T00:01:00+00:00";

    fn exited(name: &str) -> AlertData {
        AlertData::ContainerExited {
            id: name.to_string(),
            name: name.to_string(),
            exit_code: 1,
        }
    }

    fn disk(pct: u8) -> AlertData {
        AlertData::HostDiskHigh {
            mount: "/".to_string(),
            used_pct: pct,
        }
    }

    // ---- two-strike confirmation ----

    #[test]
    fn one_bad_observation_is_not_yet_an_alert() {
        let mut engine = AlertEngine::new();
        assert_eq!(
            engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1"),
            None,
            "a single blip must not open an alert"
        );
        assert_eq!(engine.open_count(), 0, "and nothing is written while pending");
    }

    #[test]
    fn the_second_consecutive_observation_opens_the_alert() {
        let mut engine = AlertEngine::new();
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1");
        let transition = engine
            .observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T1, "a2")
            .expect("the second strike should open");
        assert!(matches!(transition, Transition::Open(_)));
        assert_eq!(transition.persisted().level, SeverityLevel::Error);
        assert!(transition.persisted().is_open());
        assert_eq!(transition.persisted().target, "c1");
    }

    /// The subtle half: a blip must not *arm* the counter, or two unrelated
    /// blips minutes apart would add up to one false alarm.
    #[test]
    fn a_recovered_blip_does_not_arm_the_counter() {
        let mut engine = AlertEngine::new();
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1");
        engine.observe_condition("c1", true, SeverityLevel::Error, exited("c1"), T0, "a2");
        assert_eq!(
            engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a3"),
            None,
            "the counter must have been reset by the healthy observation"
        );
        assert!(
            engine
                .observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T1, "a4")
                .is_some()
        );
    }

    #[test]
    fn a_healthy_observation_while_pending_produces_no_recovery() {
        let mut engine = AlertEngine::new();
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1");
        assert_eq!(
            engine.observe_condition("c1", true, SeverityLevel::Error, exited("c1"), T1, "a2"),
            None,
            "there is nothing to recover from"
        );
    }

    #[test]
    fn recovery_marks_the_row_resolved_and_notifies_at_ok() {
        let mut engine = AlertEngine::new();
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1");
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T1, "a2");
        let transition = engine
            .observe_condition("c1", true, SeverityLevel::Error, exited("c1"), T1, "a3")
            .expect("recovery");

        match transition {
            Transition::Recover { stored, notification } => {
                assert!(stored.resolved, "the stored row is closed");
                assert_eq!(stored.resolved_ts.as_deref(), Some(T1));
                // History records how bad it got; only the notification says
                // "recovered".
                assert_eq!(
                    stored.level,
                    SeverityLevel::Error,
                    "the stored row keeps its severity"
                );
                assert_eq!(notification.level, SeverityLevel::Ok);
                assert_eq!(notification.id, stored.id);
            }
            other => panic!("expected recovery, got {other:?}"),
        }
        assert_eq!(engine.open_count(), 0);
    }

    #[test]
    fn a_closed_alert_does_not_close_twice() {
        let mut engine = AlertEngine::new();
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1");
        engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T1, "a2");
        assert!(engine
            .observe_condition("c1", true, SeverityLevel::Error, exited("c1"), T1, "a3")
            .is_some());
        assert_eq!(
            engine.observe_condition("c1", true, SeverityLevel::Error, exited("c1"), T1, "a4"),
            None
        );
    }

    // ---- severity only rises ----

    #[test]
    fn severity_escalates_while_open() {
        let mut engine = AlertEngine::new();
        for _ in 0..2 {
            engine.observe_condition("c1", false, SeverityLevel::Warning, exited("c1"), T0, "a1");
        }
        let transition = engine
            .observe_condition("c1", false, SeverityLevel::Critical, exited("c1"), T1, "a2")
            .expect("severity rose");
        match transition {
            Transition::Escalate(alert) => assert_eq!(alert.level, SeverityLevel::Critical),
            other => panic!("expected escalation, got {other:?}"),
        }
    }

    #[test]
    fn severity_never_downgrades_while_open() {
        let mut engine = AlertEngine::new();
        for _ in 0..2 {
            engine.observe_condition("c1", false, SeverityLevel::Critical, exited("c1"), T0, "a1");
        }
        assert_eq!(
            engine.observe_condition("c1", false, SeverityLevel::Warning, exited("c1"), T1, "a2"),
            None,
            "an open alert must not be silently softened"
        );
        assert_eq!(
            engine.observe_condition("c1", false, SeverityLevel::Critical, exited("c1"), T1, "a3"),
            None,
            "and the same level is not re-notified"
        );
    }

    /// After a recovery the target starts over: the new episode is a fresh
    /// alert at its own severity, not a continuation of the closed one. If it
    /// were a continuation, a `Critical` that recovered could never be reported
    /// as a later `Warning`.
    #[test]
    fn a_new_episode_after_recovery_opens_fresh_at_its_own_severity() {
        let mut engine = AlertEngine::new();
        for _ in 0..2 {
            engine.observe_condition("c1", false, SeverityLevel::Critical, exited("c1"), T0, "a1");
        }
        assert_eq!(engine.open_count(), 1);
        engine.observe_condition("c1", true, SeverityLevel::Critical, exited("c1"), T1, "a2");
        assert_eq!(engine.open_count(), 0);

        // The strike counter restarted too, so one observation is not enough.
        assert_eq!(
            engine.observe_condition("c1", false, SeverityLevel::Warning, exited("c1"), T1, "a3"),
            None
        );
        let transition = engine
            .observe_condition("c1", false, SeverityLevel::Warning, exited("c1"), T1, "a4")
            .expect("a fresh episode opens a new alert");
        match transition {
            Transition::Open(alert) => {
                assert_eq!(alert.level, SeverityLevel::Warning);
                assert!(alert.is_open());
            }
            other => panic!("expected a fresh open, got {other:?}"),
        }
        assert_eq!(engine.open_count(), 1);
    }

    // ---- hysteresis ----

    #[test]
    fn a_metric_fires_at_or_above_the_threshold() {
        let mut engine = AlertEngine::new();
        let threshold = Threshold::new(90.0);
        assert!(engine
            .observe_metric("disk:/", 90.0, threshold, SeverityLevel::Warning, disk(90), T0, "a1")
            .is_some());
    }

    #[test]
    fn a_metric_below_the_threshold_never_fires() {
        let mut engine = AlertEngine::new();
        let threshold = Threshold::new(90.0);
        assert_eq!(
            engine.observe_metric("disk:/", 89.9, threshold, SeverityLevel::Warning, disk(89), T0, "a1"),
            None
        );
        assert_eq!(engine.open_count(), 0);
    }

    /// The point of the band: a value jittering across the fire line must not
    /// produce a stream of open/close events.
    #[test]
    fn a_value_jittering_across_the_fire_line_does_not_flap() {
        let mut engine = AlertEngine::new();
        let threshold = Threshold::new(90.0);
        assert!(engine
            .observe_metric("disk:/", 91.0, threshold, SeverityLevel::Warning, disk(91), T0, "a1")
            .is_some());
        for pct in [89.9, 90.1, 89.5, 90.0, 87.0] {
            assert_eq!(
                engine.observe_metric(
                    "disk:/",
                    pct,
                    threshold,
                    SeverityLevel::Warning,
                    disk(pct as u8),
                    T1,
                    "a2"
                ),
                None,
                "{pct}% is inside the band and must not change anything"
            );
        }
        assert_eq!(engine.open_count(), 1);
    }

    #[test]
    fn the_clear_line_is_strictly_below_the_band() {
        let threshold = Threshold::with_hysteresis(90.0, 5.0);
        assert!(threshold.is_cleared(84.9), "below the clear line clears");
        assert!(
            !threshold.is_cleared(85.0),
            "at exactly the clear line the alert stays open"
        );
        assert!(!threshold.is_cleared(95.0));
    }

    #[test]
    fn a_real_retreat_closes_the_alert() {
        let mut engine = AlertEngine::new();
        let threshold = Threshold::new(90.0);
        engine.observe_metric("disk:/", 95.0, threshold, SeverityLevel::Warning, disk(95), T0, "a1");
        let transition = engine
            .observe_metric("disk:/", 80.0, threshold, SeverityLevel::Warning, disk(80), T1, "a2")
            .expect("a real retreat should close");
        assert!(matches!(transition, Transition::Recover { .. }));
        assert_eq!(engine.open_count(), 0);
    }

    #[test]
    fn a_metric_escalates_while_firing() {
        let mut engine = AlertEngine::new();
        let threshold = Threshold::new(90.0);
        engine.observe_metric("disk:/", 95.0, threshold, SeverityLevel::Warning, disk(95), T0, "a1");
        let transition = engine
            .observe_metric("disk:/", 96.0, threshold, SeverityLevel::Critical, disk(96), T1, "a2")
            .expect("severity rose");
        match transition {
            Transition::Escalate(alert) => assert_eq!(alert.level, SeverityLevel::Critical),
            other => panic!("expected escalation, got {other:?}"),
        }
    }

    #[test]
    fn the_band_defaults_to_five_points() {
        assert_eq!(Threshold::new(90.0).hysteresis, DEFAULT_HYSTERESIS);
        assert_eq!(DEFAULT_HYSTERESIS, 5.0);
    }

    // ---- one-shot suppression ----

    #[test]
    fn a_one_shot_notifies_once_per_episode() {
        let mut engine = AlertEngine::new();
        let data = AlertData::ImageUpdateAvailable {
            image: "alpine:3.20".to_string(),
        };
        assert!(engine
            .observe_once("img:alpine", true, SeverityLevel::Warning, data.clone(), T0, "a1")
            .is_some(), "the first sighting notifies");
        assert_eq!(
            engine.observe_once("img:alpine", true, SeverityLevel::Warning, data.clone(), T1, "a2"),
            None,
            "and the same episode does not notify again"
        );
    }

    /// The behaviour that makes the suppression safe rather than lossy: handling
    /// the update clears the condition, so the *next* update notifies too. A
    /// plain cooldown would swallow it.
    #[test]
    fn a_new_episode_notifies_again() {
        let mut engine = AlertEngine::new();
        let data = AlertData::ImageUpdateAvailable {
            image: "alpine:3.20".to_string(),
        };
        engine.observe_once("img:alpine", true, SeverityLevel::Warning, data.clone(), T0, "a1");
        let cleared = engine
            .observe_once("img:alpine", false, SeverityLevel::Warning, data.clone(), T1, "a2")
            .expect("clearing closes the alert");
        assert!(matches!(cleared, Transition::Recover { .. }));
        assert!(
            engine
                .observe_once("img:alpine", true, SeverityLevel::Warning, data, T1, "a3")
                .is_some(),
            "a later update must notify again"
        );
    }

    #[test]
    fn a_one_shot_that_never_notified_produces_no_recovery() {
        let mut engine = AlertEngine::new();
        let data = AlertData::ImageUpdateAvailable {
            image: "alpine:3.20".to_string(),
        };
        assert_eq!(
            engine.observe_once("img:alpine", false, SeverityLevel::Warning, data, T0, "a1"),
            None,
            "recovering from something never reported is pure noise"
        );
    }

    #[test]
    fn repeated_clears_are_idempotent() {
        let mut engine = AlertEngine::new();
        let data = AlertData::ImageUpdateAvailable {
            image: "alpine:3.20".to_string(),
        };
        engine.observe_once("img:alpine", true, SeverityLevel::Warning, data.clone(), T0, "a1");
        assert!(engine
            .observe_once("img:alpine", false, SeverityLevel::Warning, data.clone(), T1, "a2")
            .is_some());
        assert_eq!(
            engine.observe_once("img:alpine", false, SeverityLevel::Warning, data, T1, "a3"),
            None
        );
    }

    // ---- isolation and encoding ----

    /// Two containers failing at once must each need their *own* second strike.
    /// If the de-bounce state were global, one container's second failure would
    /// open an alert about the other.
    #[test]
    fn targets_do_not_interfere() {
        let mut engine = AlertEngine::new();

        // c1: one strike only.
        assert_eq!(
            engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1"),
            None
        );

        // c2: two strikes, which opens c2 — and only c2.
        assert_eq!(
            engine.observe_condition("c2", false, SeverityLevel::Error, exited("c2"), T0, "a2"),
            None
        );
        assert!(engine
            .observe_condition("c2", false, SeverityLevel::Error, exited("c2"), T0, "a3")
            .is_some());
        assert_eq!(engine.open_count(), 1, "c1 is still only pending");

        // c1 opens on its own second strike, not on c2's.
        let transition = engine
            .observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T1, "a4")
            .expect("c1's own second strike");
        match transition {
            Transition::Open(alert) => assert_eq!(alert.target, "c1"),
            other => panic!("expected c1 to open, got {other:?}"),
        }

        // And c2 recovering leaves c1 open.
        assert!(engine
            .observe_condition("c2", true, SeverityLevel::Error, exited("c2"), T1, "a5")
            .is_some());
        assert_eq!(engine.open_count(), 1, "only c1 remains open");
    }

    #[test]
    fn a_point_event_is_stored_already_resolved() {
        let alert = Alert::event(
            "a1",
            SeverityLevel::Error,
            "build:7",
            AlertData::BuildFailed {
                id: "7".into(),
                error: None,
            },
            T0,
        );
        assert!(!alert.is_open(), "a build failure has no ongoing state");
        assert_eq!(alert.resolved_ts.as_deref(), Some(T0));
    }

    #[test]
    fn severities_and_data_serialize_as_snake_case() {
        assert_eq!(serde_json::to_string(&SeverityLevel::Critical).unwrap(), "\"critical\"");
        let json = serde_json::to_value(AlertData::ImageUpdateAvailable {
            image: "alpine:3.20".into(),
        })
        .unwrap();
        assert_eq!(json["kind"], "image_update_available");
        assert_eq!(json["image"], "alpine:3.20");
    }

    #[test]
    fn severity_ordering_is_the_point() {
        assert!(SeverityLevel::Critical > SeverityLevel::Error);
        assert!(SeverityLevel::Error > SeverityLevel::Warning);
        assert!(SeverityLevel::Warning > SeverityLevel::Ok);
    }

    #[test]
    fn an_unknown_data_kind_does_not_break_the_log() {
        let data: AlertData =
            serde_json::from_str(r#"{"kind":"quantum_flux","degree":9}"#).unwrap();
        assert_eq!(data, AlertData::Unknown);
        assert!(!data.summary().is_empty());
    }

    #[test]
    fn every_summary_is_non_empty_and_names_its_subject() {
        assert!(exited("web").summary().contains("web"));
        assert!(AlertData::ContainerUnhealthy {
            id: "c".into(),
            name: "web".into()
        }
        .summary()
        .contains("web"));
        assert!(AlertData::ImageUpdateAvailable {
            image: "alpine:3.20".into()
        }
        .summary()
        .contains("alpine:3.20"));
        assert!(disk(91).summary().contains("91"));
        assert!(AlertData::Custom {
            message: "hello".into()
        }
        .summary()
        .contains("hello"));
    }

    #[test]
    fn open_count_tracks_open_alerts_across_mechanisms() {
        let mut engine = AlertEngine::new();
        assert_eq!(engine.open_count(), 0);

        for _ in 0..2 {
            engine.observe_condition("c1", false, SeverityLevel::Error, exited("c1"), T0, "a1");
        }
        assert_eq!(engine.open_count(), 1);

        engine.observe_metric(
            "disk:/",
            95.0,
            Threshold::new(90.0),
            SeverityLevel::Warning,
            disk(95),
            T0,
            "a2",
        );
        assert_eq!(engine.open_count(), 2);

        engine.observe_once(
            "img:alpine",
            true,
            SeverityLevel::Warning,
            AlertData::ImageUpdateAvailable {
                image: "alpine:3.20".into(),
            },
            T0,
            "a3",
        );
        assert_eq!(engine.open_count(), 3);

        engine.observe_condition("c1", true, SeverityLevel::Error, exited("c1"), T1, "a4");
        assert_eq!(engine.open_count(), 2);
    }
}
