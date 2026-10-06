//! Periodic evaluation of system state into alerts.
//!
//! # Why this polls when `registry.rs` deliberately does not
//!
//! They are different kinds of source. A registry check is a *request against
//! someone else's rate-limited service*, so it stays on demand. Container and
//! disk state are *local reads of truth that changes on its own*, and there is
//! no event that says "the disk is still 94% full" — an event stream can only
//! tell you about transitions it happened to observe.
//!
//! That distinction is not cosmetic, it is what makes the two-strike
//! de-bouncer correct. Feeding `ContainerDied` events into a two-strike
//! condition would mean a container that dies **once** and stays dead never
//! reaches the second strike, so the one case that most deserves an alert —
//! it is down and not coming back — would produce silence. Observing current
//! state makes "still down" accrue a strike on the next tick, and makes
//! "running again" a real recovery observation.
//!
//! The evaluation itself is a pure function of an observation list and is
//! tested without Docker; [`run`] is the thin loop that produces those
//! observations.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use orca_core::event::{Event, EventKind};
use orca_core::alert::{Alert, AlertData, AlertEngine, SeverityLevel, Threshold, Transition};
use orca_core::runtime::{Container, ContainerRuntime, ContainerState};

use crate::alerts;
use crate::state::AppState;

/// How often system state is re-read.
///
/// Thirty seconds because two-strike confirmation needs *repeated* observation
/// of the same state, so a container condition takes two rounds (60s) to fire.
/// Metrics are unaffected — they fire on the first breach and de-bounce by
/// hysteresis. Shorter would spend Docker API calls on a signal that changes
/// slowly; longer would make the second strike feel arbitrary.
pub const INTERVAL: Duration = Duration::from_secs(30);

/// Disk percentage at which the alert opens. It clears five points below this,
/// because a filesystem hovering at exactly the threshold is not two events.
pub const DISK_FIRE_AT_PCT: f64 = 90.0;

/// Hysteresis width in percentage points, matching the engine's default but
/// stated here so the alert's behaviour is readable from this file.
pub const DISK_HYSTERESIS_PCT: f64 = 5.0;

/// The label for the filesystem `get_system_resources` measures.
///
/// That function runs `df -k /`, so this is the root filesystem — labelled `/`
/// rather than "disk" so the alert does not imply it covers every mount.
pub const ROOT_MOUNT: &str = "/";

/// A container reduced to what alerting needs.
///
/// Deliberately not the whole `Container`: the decision below is easier to test
/// and to read when it cannot reach for fields it does not use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerObservation {
    pub id: String,
    pub name: String,
    /// Actually running, not merely existing.
    pub running: bool,
    /// The exit code, when it could be determined.
    pub exit_code: Option<i64>,
    pub oom_killed: bool,
}

/// What should be done about one container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerVerdict {
    /// Faulty: worth a strike.
    Failed,
    /// Fine: clears strikes and closes anything open.
    Healthy,
    /// Not enough information to say either way.
    Unknown,
}

/// The alert target for a container.
///
/// One function so a producer and the zombie-closer cannot disagree about what
/// they are naming — a mismatch there would silently leak open alerts forever.
pub fn container_target(id: &str) -> String {
    format!("container:{id}")
}

/// Reduce a container's state to a verdict.
///
/// The `Unknown` arm is the point of this function. A container that is Exited
/// but whose inspect failed has a real state and an unknown cause: calling it
/// `Failed` would cry wolf on every deliberate `docker stop` we could not read,
/// and calling it `Healthy` would silently close a real crash. So it is neither
/// — no strike, and no closure either (see [`close_vanished`]).
pub fn verdict(observation: &ContainerObservation) -> ContainerVerdict {
    if observation.running {
        return ContainerVerdict::Healthy;
    }
    match observation.exit_code {
        // A deliberate stop is not a fault. This is also what closes an alert
        // when someone stops a container that had been crashing.
        Some(0) => ContainerVerdict::Healthy,
        Some(_) => ContainerVerdict::Failed,
        // OOM is unambiguous even without an exit code.
        None if observation.oom_killed => ContainerVerdict::Failed,
        None => ContainerVerdict::Unknown,
    }
}

/// Whether a `ContainerState` means "running as far as alerting is concerned".
///
/// `Created` has never started and `Paused` was suspended on purpose, so
/// neither is a fault. `Restarting` is *not* included: it is mid-crash-loop,
/// and its previous run's exit code is what decides.
fn is_running(state: ContainerState) -> bool {
    matches!(
        state,
        ContainerState::Running | ContainerState::Created | ContainerState::Paused
    )
}

fn failed_data(observation: &ContainerObservation) -> AlertData {
    AlertData::ContainerExited {
        id: observation.id.clone(),
        name: observation.name.clone(),
        // The alert carries what we know; an unknown code is reported as -1
        // rather than as a fabricated 0, which would read as a clean exit.
        exit_code: observation.exit_code.unwrap_or(-1) as i32,
    }
}

fn healthy_data(observation: &ContainerObservation) -> AlertData {
    // Only ever reached on the healthy path, where the engine ignores `data`,
    // but it still has to be something.
    AlertData::ContainerExited {
        id: observation.id.clone(),
        name: observation.name.clone(),
        exit_code: observation.exit_code.unwrap_or(0) as i32,
    }
}

/// The severity for a failed container: an OOM kill is worse than a plain
/// non-zero exit, because it says the container ran out of a resource rather
/// than exiting on a condition it could handle.
fn failure_level(observation: &ContainerObservation) -> SeverityLevel {
    if observation.oom_killed {
        SeverityLevel::Critical
    } else {
        SeverityLevel::Error
    }
}

/// Feed one round of container observations into the engine.
///
/// Returns the transitions to persist. Containers with an `Unknown` verdict are
/// skipped entirely — they neither strike nor clear.
pub fn observe_containers(
    engine: &mut AlertEngine,
    observations: &[ContainerObservation],
    ts: &str,
) -> Vec<Transition> {
    let mut transitions = Vec::new();
    for observation in observations {
        let (healthy, level, data) = match verdict(observation) {
            ContainerVerdict::Unknown => continue,
            ContainerVerdict::Healthy => (true, SeverityLevel::Error, healthy_data(observation)),
            ContainerVerdict::Failed => {
                (false, failure_level(observation), failed_data(observation))
            }
        };
        let target = container_target(&observation.id);
        // An id is generated for every observation even though it is only used
        // when an alert actually opens. Collision-free ids are worth one
        // counter read per container per tick.
        let id = alerts::new_id("alert");
        if let Some(transition) =
            engine.observe_condition(&target, healthy, level, data, ts, &id)
        {
            transitions.push(transition);
        }
    }
    transitions
}

/// Close alerts whose subject is no longer present at all.
///
/// `present` holds the targets of every container the listing returned,
/// including ones this round could not judge. That distinction is the whole
/// point: a container we could not inspect is *not* a container that is gone,
/// and closing its alert on a transient inspect failure would hide a live
/// problem.
///
/// This is the "only close things" rule — it never opens an alert, so a daemon
/// restart cannot resurrect a fault from a stale listing.
pub fn close_vanished(
    engine: &mut AlertEngine,
    open: &[Alert],
    present: &HashSet<String>,
    ts: &str,
) -> Vec<Transition> {
    let mut transitions = Vec::new();
    for alert in open {
        if !alert.target.starts_with("container:") || present.contains(&alert.target) {
            continue;
        }
        // A healthy observation both closes the alert and clears the strike
        // count, so a future container reusing this id starts clean.
        let id = alerts::new_id("alert");
        if let Some(transition) = engine.observe_condition(
            &alert.target,
            true,
            SeverityLevel::Ok,
            alert.data.clone(),
            ts,
            &id,
        ) {
            transitions.push(transition);
        }
    }
    transitions
}

/// Feed the root filesystem's usage into the engine.
///
/// The threshold is the alert: the existing `> 90.0` warning inside
/// `check_system_health` is a string in a list, so it cannot be de-bounced,
/// persisted, or recovered — it simply says the same thing on every poll.
pub fn observe_disk(
    engine: &mut AlertEngine,
    used_pct: f64,
    ts: &str,
) -> Option<Transition> {
    let mount = ROOT_MOUNT.to_string();
    // Above 95% the situation is no longer "worth watching".
    let level = if used_pct >= 95.0 {
        SeverityLevel::Critical
    } else {
        SeverityLevel::Warning
    };
    let target = format!("disk:{mount}");
    let id = alerts::new_id("alert");
    engine.observe_metric(
        &target,
        used_pct,
        Threshold::with_hysteresis(DISK_FIRE_AT_PCT, DISK_HYSTERESIS_PCT),
        level,
        AlertData::HostDiskHigh {
            mount,
            used_pct: used_pct.round().clamp(0.0, 100.0) as u8,
        },
        ts,
        &id,
    )
}

/// Build the observation list from a container listing, inspecting the ones
/// that are not running.
///
/// The list summary carries state but not the exit code, so a non-running
/// container has to be inspected to tell a crash from a deliberate stop.
/// Only non-running ones are inspected, so the cost is bounded by how many
/// containers are stopped rather than by how many exist.
pub async fn observe(
    runtime: &orca_backend_common::BollardRuntime,
) -> (Vec<ContainerObservation>, HashSet<String>) {
    let containers = match runtime.list_containers(true).await {
        Ok(containers) => containers,
        Err(e) => {
            tracing::warn!("monitor: cannot list containers: {e:#}");
            // An empty list here would be read as "every container vanished",
            // so return a marker the caller can distinguish. `None`-style
            // handling is done by the caller checking `present.is_empty()`.
            return (Vec::new(), HashSet::new());
        }
    };

    let mut observations = Vec::with_capacity(containers.len());
    let mut present = HashSet::with_capacity(containers.len());
    for container in containers {
        present.insert(container_target(&container.id));
        observations.push(observation_for(runtime, container).await);
    }
    (observations, present)
}

async fn observation_for(
    runtime: &orca_backend_common::BollardRuntime,
    container: Container,
) -> ContainerObservation {
    let running = is_running(container.state);
    let mut observation = ContainerObservation {
        id: container.id.clone(),
        name: container.name.clone(),
        running,
        exit_code: container.exit_code,
        oom_killed: container.oom_killed.unwrap_or(false),
    };

    if !running && observation.exit_code.is_none() {
        match runtime.inspect_container(&container.id).await {
            Ok(detail) => {
                observation.exit_code = detail.exit_code;
                observation.oom_killed = detail.oom_killed.unwrap_or(false);
            }
            Err(e) => {
                // Leaves the verdict `Unknown`: no strike, and no closure.
                tracing::debug!(
                    container = %container.id,
                    "monitor: cannot inspect a stopped container, skipping this round: {e:#}"
                );
            }
        }
    }
    observation
}

/// Run the monitor loop.
///
/// Never returns; spawned once at startup. Failures inside a round are logged
/// and the next round retries, because a monitor that dies on the first Docker
/// hiccup is worse than one that misses a tick.
pub async fn run(state: Arc<AppState>, interval: Duration) {
    let mut engine = AlertEngine::new();
    loop {
        let ts = chrono::Utc::now().to_rfc3339();
        let (observations, present) = {
            let runtime = state.rt().await;
            observe(&runtime).await
        };

        // An empty listing is almost always a failed list rather than every
        // container having been deleted at once. Closing every container alert
        // on that basis is exactly the kind of confident wrong action this
        // module is written to avoid.
        if !present.is_empty() {
            let open = alerts::open_alerts();
            let mut transitions = observe_containers(&mut engine, &observations, &ts);
            transitions.extend(close_vanished(&mut engine, &open, &present, &ts));

            // A failed disk read is skipped rather than treated as 0%: zero is
            // below the clear threshold, so passing it on would close an open
            // disk alert and report the disk as fixed.
            if let Some(used_pct) = root_disk_used_pct().await
                && let Some(transition) = observe_disk(&mut engine, used_pct, &ts)
            {
                transitions.push(transition);
            }

            for transition in &transitions {
                alerts::append(transition.persisted());
                // Openings and escalations are worth a log line; a recovery is
                // normal operation, and the resolved row is written either way.
                if let Transition::Open(alert) | Transition::Escalate(alert) = transition {
                    tracing::info!(alert = %alert.id, "{}", alert.data.summary());
                }
            }

            // Pushed *after* the batch is persisted, and the count is read back
            // from the log rather than from the engine, so it is exactly the
            // count `GET /alerts` would report. A bell that lights up on its own
            // is the difference between an alert and a log line.
            if !transitions.is_empty() {
                let _ = state.events_tx.send(Event {
                    timestamp: ts.clone(),
                    kind: EventKind::AlertChanged {
                        open_count: alerts::open_alerts().len(),
                    },
                });
                // A send error means nobody is subscribed, which is normal.
            }
        }

        tokio::time::sleep(interval).await;
    }
}

/// The root filesystem's used percentage.
async fn root_disk_used_pct() -> Option<f64> {
    orca_backend_common::environment::root_disk_used_pct().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: &str = "2026-01-01T00:00:00+00:00";
    const T1: &str = "2026-01-01T00:01:00+00:00";
    const T2: &str = "2026-01-01T00:02:00+00:00";

    fn running(id: &str, name: &str) -> ContainerObservation {
        ContainerObservation {
            id: id.into(),
            name: name.into(),
            running: true,
            exit_code: None,
            oom_killed: false,
        }
    }

    fn crashed(id: &str, name: &str, code: i64) -> ContainerObservation {
        ContainerObservation {
            id: id.into(),
            name: name.into(),
            running: false,
            exit_code: Some(code),
            oom_killed: false,
        }
    }

    // ---- verdict: the tri-state decision ----

    #[test]
    fn a_running_container_is_healthy() {
        assert_eq!(verdict(&running("a", "web")), ContainerVerdict::Healthy);
    }

    #[test]
    fn a_non_zero_exit_is_a_failure() {
        assert_eq!(
            verdict(&crashed("a", "web", 137)),
            ContainerVerdict::Failed
        );
    }

    #[test]
    fn a_deliberate_stop_is_not_a_failure() {
        assert_eq!(
            verdict(&crashed("a", "web", 0)),
            ContainerVerdict::Healthy,
            "`docker stop` exits 0; alerting on it would alert on every normal stop"
        );
    }

    #[test]
    fn an_oom_kill_is_a_failure_even_without_an_exit_code() {
        let observation = ContainerObservation {
            oom_killed: true,
            ..crashed("a", "web", 0)
        };
        // exit_code 0 is present here, so use the honest case: none at all.
        let observation = ContainerObservation {
            exit_code: None,
            ..observation
        };
        assert_eq!(verdict(&observation), ContainerVerdict::Failed);
    }

    #[test]
    fn an_unreadable_stopped_container_is_unknown() {
        let observation = ContainerObservation {
            exit_code: None,
            ..crashed("a", "web", 0)
        };
        assert_eq!(
            verdict(&observation),
            ContainerVerdict::Unknown,
            "a real state with an unknown cause is neither healthy nor failed"
        );
    }

    // ---- observe_containers: the two-strike contract ----

    #[test]
    fn a_crash_needs_two_rounds_and_then_opens() {
        let mut engine = AlertEngine::new();
        let states = vec![crashed("a", "web", 137)];

        assert!(
            observe_containers(&mut engine, &states, T0).is_empty(),
            "one round is one strike; nothing is recorded yet"
        );
        let transitions = observe_containers(&mut engine, &states, T1);
        assert_eq!(transitions.len(), 1);
        let alert = transitions[0].persisted();
        assert_eq!(alert.target, "container:a");
        assert_eq!(alert.level, SeverityLevel::Error);
        assert!(!alert.resolved);
    }

    /// The reason this module polls: a container that dies once and stays dead
    /// must still reach the second strike.
    #[test]
    fn a_container_that_stays_dead_still_reaches_two_strikes() {
        let mut engine = AlertEngine::new();
        let states = vec![crashed("a", "web", 1)];
        assert!(observe_containers(&mut engine, &states, T0).is_empty());
        assert!(
            !observe_containers(&mut engine, &states, T1).is_empty(),
            "the second round observes the same state again and confirms it"
        );
    }

    #[test]
    fn a_restart_that_comes_back_healthy_closes_the_alert() {
        let mut engine = AlertEngine::new();
        let dead = vec![crashed("a", "web", 137)];
        observe_containers(&mut engine, &dead, T0);
        observe_containers(&mut engine, &dead, T1);
        assert_eq!(engine.open_count(), 1);

        let up = vec![running("a", "web")];
        let transitions = observe_containers(&mut engine, &up, T2);
        assert_eq!(transitions.len(), 1);
        assert!(
            transitions[0].persisted().resolved,
            "recovery is written as a resolved row"
        );
        assert_eq!(engine.open_count(), 0);
    }

    #[test]
    fn an_oom_kill_escalates_rather_than_reopening() {
        let mut engine = AlertEngine::new();
        let plain = vec![crashed("a", "web", 1)];
        observe_containers(&mut engine, &plain, T0);
        observe_containers(&mut engine, &plain, T1);

        let oom = vec![ContainerObservation {
            oom_killed: true,
            ..crashed("a", "web", 137)
        }];
        let transitions = observe_containers(&mut engine, &oom, T2);
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].persisted().level, SeverityLevel::Critical);
        assert_eq!(
            engine.open_count(),
            1,
            "escalating must not create a second alert for the same container"
        );
    }

    #[test]
    fn an_unknown_verdict_neither_strikes_nor_clears() {
        let mut engine = AlertEngine::new();
        let unknown = vec![ContainerObservation {
            exit_code: None,
            ..crashed("a", "web", 0)
        }];
        assert!(observe_containers(&mut engine, &unknown, T0).is_empty());
        assert!(observe_containers(&mut engine, &unknown, T1).is_empty());
        assert_eq!(engine.open_count(), 0);

        // And it does not clear an alert that is already open either.
        let dead = vec![crashed("b", "db", 1)];
        observe_containers(&mut engine, &dead, T0);
        observe_containers(&mut engine, &dead, T1);
        assert_eq!(engine.open_count(), 1);
        let mixed = vec![crashed("b", "db", 1), unknown[0].clone()];
        observe_containers(&mut engine, &mixed, T2);
        assert_eq!(
            engine.open_count(),
            1,
            "the unknown container must not have closed anything"
        );
    }

    #[test]
    fn two_crashed_containers_are_two_alerts() {
        let mut engine = AlertEngine::new();
        let states = vec![crashed("a", "web", 1), crashed("b", "db", 2)];
        observe_containers(&mut engine, &states, T0);
        let transitions = observe_containers(&mut engine, &states, T1);
        assert_eq!(transitions.len(), 2, "one transition per container");
        assert_eq!(engine.open_count(), 2);
    }

    // ---- close_vanished ----

    #[test]
    fn a_container_absent_from_the_listing_closes_its_alert() {
        let mut engine = AlertEngine::new();
        let states = vec![crashed("a", "web", 1)];
        observe_containers(&mut engine, &states, T0);
        observe_containers(&mut engine, &states, T1);
        let open = vec![engine_open(&engine)];
        assert_eq!(open.len(), 1);

        // `a` is gone from the listing entirely.
        let present = HashSet::from([container_target("other")]);
        let transitions = close_vanished(&mut engine, &open, &present, T2);
        assert_eq!(transitions.len(), 1);
        assert!(transitions[0].persisted().resolved);
        assert_eq!(engine.open_count(), 0);
    }

    #[test]
    fn a_container_that_is_still_listed_keeps_its_alert() {
        let mut engine = AlertEngine::new();
        let states = vec![crashed("a", "web", 1)];
        observe_containers(&mut engine, &states, T0);
        observe_containers(&mut engine, &states, T1);
        let open = vec![engine_open(&engine)];

        let present = HashSet::from([container_target("a")]);
        assert!(
            close_vanished(&mut engine, &open, &present, T2).is_empty(),
            "present but unjudged is not the same as gone"
        );
        assert_eq!(engine.open_count(), 1);
    }

    #[test]
    fn close_vanished_ignores_unrelated_targets() {
        let mut engine = AlertEngine::new();
        // A disk alert, which this function has no business closing.
        observe_disk(&mut engine, 99.0, T0);
        observe_disk(&mut engine, 99.0, T1);
        assert_eq!(engine.open_count(), 1);

        let open = vec![engine_open(&engine)];
        let transitions = close_vanished(&mut engine, &open, &HashSet::new(), T2);
        assert!(
            transitions.is_empty(),
            "only container alerts are this function's to close"
        );
        assert_eq!(engine.open_count(), 1);
    }

    // ---- observe_disk ----

    /// Metrics do **not** use two-strike; hysteresis is their de-bounce. A full
    /// disk should say so on the reading that shows it rather than one interval
    /// later — the delay would buy nothing, because the clear line already
    /// stops a reading that merely hovers at the threshold from flapping.
    #[test]
    fn disk_fires_on_the_first_breach_because_hysteresis_is_its_de_bounce() {
        let mut engine = AlertEngine::new();
        let transition =
            observe_disk(&mut engine, 92.0, T0).expect("the first breach is the alert");
        let alert = transition.persisted();
        assert_eq!(alert.target, "disk:/");
        assert_eq!(alert.level, SeverityLevel::Warning);
        match &alert.data {
            AlertData::HostDiskHigh { mount, used_pct } => {
                assert_eq!(mount, "/");
                assert_eq!(*used_pct, 92);
            }
            other => panic!("expected a disk alert, got {other:?}"),
        }
        // The same value again is not a second event.
        assert!(observe_disk(&mut engine, 92.0, T1).is_none());
        assert_eq!(engine.open_count(), 1);
    }

    #[test]
    fn disk_does_not_alert_below_the_threshold() {
        let mut engine = AlertEngine::new();
        assert!(observe_disk(&mut engine, 89.9, T0).is_none());
        assert!(observe_disk(&mut engine, 89.9, T1).is_none());
        assert_eq!(engine.open_count(), 0);
    }

    #[test]
    fn disk_hysteresis_keeps_a_wobbling_filesystem_quiet() {
        let mut engine = AlertEngine::new();
        observe_disk(&mut engine, 92.0, T0);
        observe_disk(&mut engine, 92.0, T1);
        assert_eq!(engine.open_count(), 1);

        // 87% is below the fire line but above the clear line (90 - 5).
        assert!(
            observe_disk(&mut engine, 87.0, T2).is_none(),
            "dropping just under the threshold is not an event"
        );
        assert_eq!(engine.open_count(), 1, "the alert stays open");

        // Below the clear line it closes.
        let transition = observe_disk(&mut engine, 84.0, T2).expect("a real recovery");
        assert!(transition.persisted().resolved);
        assert_eq!(engine.open_count(), 0);
    }

    #[test]
    fn a_full_disk_is_critical_and_escalates() {
        let mut engine = AlertEngine::new();
        observe_disk(&mut engine, 91.0, T0);
        observe_disk(&mut engine, 91.0, T1);
        assert_eq!(engine.open_count(), 1);

        let transition = observe_disk(&mut engine, 97.0, T2).expect("worse is an escalation");
        assert_eq!(transition.persisted().level, SeverityLevel::Critical);
        assert_eq!(engine.open_count(), 1);
    }

    #[test]
    fn a_disk_percentage_over_100_is_clamped_for_display() {
        let mut engine = AlertEngine::new();
        // `used_pct` is a u8, so a nonsense reading must not wrap around into a
        // small number that reads as "nearly empty".
        let transition = observe_disk(&mut engine, 120.0, T0).expect("still an alert");
        match &transition.persisted().data {
            AlertData::HostDiskHigh { used_pct, .. } => assert_eq!(*used_pct, 100),
            other => panic!("expected a disk alert, got {other:?}"),
        }
    }

    #[test]
    fn the_container_target_namespace_is_stable() {
        // `close_vanished` matches on this prefix; changing it without changing
        // that check would leak open alerts forever.
        assert_eq!(container_target("abc"), "container:abc");
        assert!(container_target("abc").starts_with("container:"));
    }

    /// The single open alert the engine is holding, for the tests above.
    fn engine_open(engine: &AlertEngine) -> Alert {
        engine
            .open_alerts()
            .into_iter()
            .next()
            .expect("an open alert")
    }
}
