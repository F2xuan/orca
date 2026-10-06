//! Event system for real-time updates to the GUI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub timestamp: String,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventKind {
    // Machine events
    MachineStarted {
        name: String,
    },
    MachineStopped {
        name: String,
    },
    MachineError {
        name: String,
        error: String,
    },

    // Container events
    ContainerCreated {
        id: String,
        name: String,
    },
    ContainerStarted {
        id: String,
        name: String,
    },
    ContainerStopped {
        id: String,
        name: String,
    },
    ContainerRemoved {
        id: String,
        name: String,
    },
    ContainerDied {
        id: String,
        name: String,
        exit_code: i32,
        #[serde(default)]
        oom_killed: bool,
    },

    // Image events
    ImagePulled {
        id: String,
        reference: String,
    },
    ImageRemoved {
        id: String,
    },

    // Volume events
    VolumeCreated {
        name: String,
    },
    VolumeRemoved {
        name: String,
    },

    // Build events
    BuildStarted {
        id: String,
        tag: String,
    },
    BuildCompleted {
        id: String,
        tag: String,
        success: bool,
    },

    /// Forward-compat catch-all for events emitted by newer daemons that
    /// older clients don't recognize. Keeps deserialization non-fatal
    /// across version skew.
    // Alert events
    /// An alert opened, escalated, or resolved.
    ///
    /// Carries the authoritative open count and nothing else. The bell's badge
    /// needs a number to light up; the detail list already has a source
    /// (`GET /alerts`), so putting a second serialisation of `Alert` on this
    /// wire would only be one more thing to keep in sync.
    AlertChanged {
        open_count: usize,
    },

    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GUI reads `kind.type` and `kind.data`, so an alert event has to
    /// arrive in exactly that shape or the badge silently never updates.
    #[test]
    fn an_alert_event_carries_the_open_count_in_the_data_field() {
        let event = Event {
            timestamp: "2026-01-01T00:00:00+00:00".into(),
            kind: EventKind::AlertChanged { open_count: 3 },
        };
        let value = serde_json::to_value(&event).expect("serialises");
        assert_eq!(value["kind"]["type"], "AlertChanged");
        assert_eq!(value["kind"]["data"]["open_count"], 3);
        assert_eq!(value["timestamp"], "2026-01-01T00:00:00+00:00");

        let back: Event = serde_json::from_value(value).expect("round trips");
        match back.kind {
            EventKind::AlertChanged { open_count } => assert_eq!(open_count, 3),
            other => panic!("expected an alert event, got {other:?}"),
        }
    }

    /// A variant this build does not know must not fail the whole stream.
    ///
    /// Only the **unit** shape is covered. `#[serde(other)]` cannot capture the
    /// `data` field under adjacent tagging, so an unknown variant *carrying a
    /// payload* fails to decode rather than becoming `Unknown` — the annotation
    /// reads like forward compatibility but is narrower than it looks.
    ///
    /// This is currently inert: nothing deserialises an `Event`. The Docker
    /// listener only constructs them, the SSE stream only serialises, and the
    /// GUI bridge passes untyped JSON through. It would bite the first consumer
    /// that decodes events from a *newer* daemon. The two real fixes are to tag
    /// internally (`tag = "type"` with the fields inline) — a wire change that
    /// would break `kind.data` for every existing event — or to hand-write a
    /// `Deserialize` over a shadow enum. Both are recorded in the triage report
    /// §21 rather than chosen here.
    #[test]
    fn an_unknown_event_decodes_only_without_a_payload() {
        let unit_shaped = serde_json::json!({
            "timestamp": "2026-01-01T00:00:00+00:00",
            "kind": { "type": "SomethingFromTheFuture" }
        });
        let event: Event = serde_json::from_value(unit_shaped).expect("decodes");
        assert!(matches!(event.kind, EventKind::Unknown));

        // The limitation, pinned by a test so it cannot be mistaken for working
        // forward compatibility. If this assertion starts failing, `Unknown` has
        // gained payload capture and the note above is out of date.
        let with_payload = serde_json::json!({
            "timestamp": "2026-01-01T00:00:00+00:00",
            "kind": { "type": "SomethingFromTheFuture", "data": { "x": 1 } }
        });
        assert!(
            serde_json::from_value::<Event>(with_payload).is_err(),
            "an unknown payload-carrying variant does not decode today"
        );
    }

    #[test]
    fn event_kind_serializes_as_tagged_enum() {
        let kind = EventKind::ContainerStarted {
            id: "abc123".into(),
            name: "web-1".into(),
        };
        let json = serde_json::to_value(&kind).unwrap();

        // serde(tag = "type", content = "data") produces {"type": "...", "data": {...}}
        assert_eq!(json["type"], "ContainerStarted");
        assert_eq!(json["data"]["id"], "abc123");
        assert_eq!(json["data"]["name"], "web-1");
    }

    #[test]
    fn event_kind_machine_started_serialization() {
        let kind = EventKind::MachineStarted { name: "default".into() };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "MachineStarted");
        assert_eq!(json["data"]["name"], "default");
    }

    #[test]
    fn event_kind_container_died_includes_exit_code() {
        let kind = EventKind::ContainerDied {
            id: "def456".into(),
            name: "worker-1".into(),
            exit_code: 137,
            oom_killed: true,
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "ContainerDied");
        assert_eq!(json["data"]["exit_code"], 137);
        assert_eq!(json["data"]["oom_killed"], true);
    }

    #[test]
    fn event_kind_container_died_defaults_oom_killed() {
        let json = r#"{"type":"ContainerDied","data":{"id":"def456","name":"worker-1","exit_code":137}}"#;
        let kind: EventKind = serde_json::from_str(json).unwrap();
        let restored = serde_json::to_value(kind).unwrap();
        assert_eq!(restored["data"]["oom_killed"], false);
    }

    #[test]
    fn event_kind_roundtrip() {
        let original = EventKind::ImagePulled {
            id: "sha256:abc".into(),
            reference: "nginx:latest".into(),
        };
        let json_str = serde_json::to_string(&original).unwrap();
        let restored: EventKind = serde_json::from_str(&json_str).unwrap();

        // Verify via re-serialization (PartialEq not derived)
        let json_restored = serde_json::to_string(&restored).unwrap();
        assert_eq!(json_str, json_restored);
    }

    #[test]
    fn event_struct_serializes_with_timestamp() {
        let event = Event {
            timestamp: "2025-06-01T12:00:00Z".into(),
            kind: EventKind::VolumeCreated {
                name: "myvolume".into(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["timestamp"], "2025-06-01T12:00:00Z");
        assert_eq!(json["kind"]["type"], "VolumeCreated");
        assert_eq!(json["kind"]["data"]["name"], "myvolume");
    }

    #[test]
    fn event_kind_machine_error_has_error_field() {
        let kind = EventKind::MachineError {
            name: "vm1".into(),
            error: "out of memory".into(),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "MachineError");
        assert_eq!(json["data"]["error"], "out of memory");
    }
}
