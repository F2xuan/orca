//! Agent tool definitions for AI-driven container management.
//!
//! Provides a catalog of tools that can be exposed via MCP (Model Context Protocol)
//! or OpenAI-compatible function calling APIs.

use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub category: String,
}

/// Fallback tier for a tool that is missing from [`TOOL_RISKS`].
///
/// The *most* dangerous tier on purpose: an unclassified tool is one nobody has
/// thought about, and the safe assumption about a tool nobody has thought about
/// is that it can destroy things.
fn default_tool_risk() -> ToolRisk {
    ToolRisk::Destructive
}

/// How much damage a tool can do when an agent calls it with no human in the
/// loop.
///
/// The tiers exist so the daemon can default to the safe end of the range.
/// Agent tools are reachable by an LLM that may be following instructions
/// embedded in the very data it was asked to inspect — a container's log lines,
/// a page it fetched, a commit message. Read-only by default means a successful
/// prompt injection is still embarrassing, but cannot delete the user's data.
///
/// The tiers are ordered and cumulative: permitting `Write` also permits
/// `Read`. A policy that allowed writes while denying reads would be incoherent,
/// so it is not representable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    /// Observational only. Safe to run unattended.
    Read,
    /// Changes a running system, but in a way the user can undo: start, stop,
    /// restart, pull, create, redeploy, scale.
    Write,
    /// Can destroy data, or is equivalent to handing over a shell. Not
    /// recoverable by the agent that caused it.
    Destructive,
}

impl ToolRisk {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
        }
    }

    /// Whether a policy whose ceiling is `allowed` permits this tier.
    pub fn is_permitted_by(self, allowed: ToolRisk) -> bool {
        self <= allowed
    }
}

/// The single source of truth for tool risk.
///
/// Kept as a table rather than a field on each `ToolDefinition` literal so the
/// 42 tool sites stay untouched, and so the completeness tests in this module
/// can assert the table covers the catalog *exactly* — in both directions. A
/// newly added tool therefore cannot ship unclassified, and a renamed one
/// cannot leave a stale row behind.
const TOOL_RISKS: &[(&str, ToolRisk)] = &[
    // --- Containers ---
    ("list_containers", ToolRisk::Read),
    ("inspect_container", ToolRisk::Read),
    ("container_logs", ToolRisk::Read),
    ("container_stats", ToolRisk::Read),
    ("diagnose_container", ToolRisk::Read),
    ("system_health", ToolRisk::Read),
    ("environment_status", ToolRisk::Read),
    ("check_port_availability", ToolRisk::Read),
    ("start_container", ToolRisk::Write),
    ("stop_container", ToolRisk::Write),
    ("restart_container", ToolRisk::Write),
    ("create_and_run_container", ToolRisk::Write),
    // Runs an arbitrary command inside the container. Tiered with the
    // destructive operations, not the recoverable ones: the command can delete
    // anything the container can write, and the agent cannot undo it.
    ("exec_in_container", ToolRisk::Destructive),
    ("remove_container", ToolRisk::Destructive),
    // --- Images ---
    ("list_images", ToolRisk::Read),
    ("pull_image", ToolRisk::Write),
    ("remove_image", ToolRisk::Destructive),
    // Pruning deletes images that are not merely unused but *untagged*, which
    // includes build cache the user may be relying on for the next build.
    ("prune_images", ToolRisk::Destructive),
    // --- Stacks ---
    ("list_stacks", ToolRisk::Read),
    ("compose_up", ToolRisk::Write),
    // `compose down` removes the containers. Named volumes survive, but
    // container-local state does not, and an agent that tears down a stack
    // unattended is a bad default regardless.
    ("compose_down", ToolRisk::Destructive),
    // --- Volumes / networks ---
    ("list_volumes", ToolRisk::Read),
    ("list_networks", ToolRisk::Read),
    ("create_network", ToolRisk::Write),
    // --- Templates ---
    ("list_templates", ToolRisk::Read),
    ("deploy_template", ToolRisk::Write),
    // --- Kubernetes ---
    ("k8s_status", ToolRisk::Read),
    ("k8s_list_pods", ToolRisk::Read),
    ("k8s_list_deployments", ToolRisk::Read),
    ("k8s_list_services", ToolRisk::Read),
    ("k8s_list_namespaces", ToolRisk::Read),
    ("k8s_list_events", ToolRisk::Read),
    ("k8s_list_configmaps", ToolRisk::Read),
    // Names only, never values — see the tool description.
    ("k8s_list_secrets", ToolRisk::Read),
    ("k8s_list_ingresses", ToolRisk::Read),
    ("k8s_helm_list", ToolRisk::Read),
    ("k8s_pod_logs", ToolRisk::Read),
    ("k8s_get_yaml", ToolRisk::Read),
    ("k8s_apply_yaml", ToolRisk::Write),
    ("k8s_restart_deployment", ToolRisk::Write),
    ("k8s_scale_deployment", ToolRisk::Write),
    ("k8s_delete_pod", ToolRisk::Destructive),
];

/// The risk tier of a catalog tool, or `None` when the name is not in the
/// catalog at all. Callers should treat `None` as a refusal, not as "safe".
pub fn tool_risk(name: &str) -> Option<ToolRisk> {
    TOOL_RISKS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, risk)| *risk)
}

fn json_schema(properties: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": properties,
    })
}

/// All available agent tools.
pub fn tool_catalog() -> Vec<ToolDefinition> {
    vec![
        // === Container Operations ===
        ToolDefinition {
            name: "list_containers".into(),
            description: "List all containers with their status, ports, and resource usage. Use this to get an overview of running workloads.".into(),
            parameters: json_schema(json!({})),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "inspect_container".into(),
            description: "Get detailed information about a container including env vars, mounts, network settings, exit code, and error messages. Essential for diagnosing issues.".into(),
            parameters: json_schema(json!({ "id": { "type": "string", "description": "Container ID or name" } })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "container_logs".into(),
            description: "Get recent log output from a container. Use this to diagnose crashes, errors, and unexpected behavior.".into(),
            parameters: json_schema(json!({
                "id": { "type": "string", "description": "Container ID or name" },
                "tail": { "type": "integer", "description": "Number of recent lines to return", "default": 100 }
            })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "container_stats".into(),
            description: "Get real-time CPU, memory, and network usage for a container. Use to identify resource exhaustion.".into(),
            parameters: json_schema(json!({ "id": { "type": "string", "description": "Container ID or name" } })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "start_container".into(),
            description: "Start a stopped container.".into(),
            parameters: json_schema(json!({ "id": { "type": "string" } })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "stop_container".into(),
            description: "Stop a running container gracefully.".into(),
            parameters: json_schema(json!({ "id": { "type": "string" } })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "restart_container".into(),
            description: "Restart a container (stop then start). Useful for applying config changes or recovering from issues.".into(),
            parameters: json_schema(json!({ "id": { "type": "string" } })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "exec_in_container".into(),
            description: "Execute a command inside a running container and return the output. Use for debugging, checking file contents, testing connectivity, etc.".into(),
            parameters: json_schema(json!({
                "id": { "type": "string", "description": "Container ID or name" },
                "command": { "type": "array", "items": { "type": "string" }, "description": "Command and arguments to execute" }
            })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "create_and_run_container".into(),
            description: "Create and start a new container from an image with specified configuration.".into(),
            parameters: json_schema(json!({
                "image": { "type": "string", "description": "Docker image to use" },
                "name": { "type": "string", "description": "Container name" },
                "ports": { "type": "array", "items": { "type": "string" }, "description": "Port mappings in host:container format" },
                "env": { "type": "object", "description": "Environment variables as key-value pairs" },
                "volumes": { "type": "array", "items": { "type": "string" }, "description": "Volume mounts in host:container format" },
                "memory_limit": { "type": "string", "description": "Memory limit e.g. 512m, 1g" },
                "cpu_limit": { "type": "number", "description": "CPU core limit e.g. 0.5, 2.0" }
            })),
            category: "Containers".into(),
        },
        ToolDefinition {
            name: "remove_container".into(),
            description: "Remove a stopped container.".into(),
            parameters: json_schema(json!({ "id": { "type": "string" }, "force": { "type": "boolean", "default": false } })),
            category: "Containers".into(),
        },

        // === Diagnostics ===
        ToolDefinition {
            name: "diagnose_container".into(),
            description: "Comprehensive diagnosis of a container — returns inspect data, recent logs, resource stats, and exit info in one call. Use this as the first step when investigating any container issue.".into(),
            parameters: json_schema(json!({ "id": { "type": "string", "description": "Container ID or name" } })),
            category: "Diagnostics".into(),
        },
        ToolDefinition {
            name: "check_port_availability".into(),
            description: "Check if a port is available on the host or which container is using it.".into(),
            parameters: json_schema(json!({ "port": { "type": "integer" } })),
            category: "Diagnostics".into(),
        },
        ToolDefinition {
            name: "system_health".into(),
            description: "Get overall system health including Docker connection status, disk usage, memory, CPU, and any warnings.".into(),
            parameters: json_schema(json!({})),
            category: "Diagnostics".into(),
        },
        ToolDefinition {
            name: "environment_status".into(),
            description: "Check the container runtime environment — is Docker/Podman installed and running?".into(),
            parameters: json_schema(json!({})),
            category: "Diagnostics".into(),
        },

        // === Images ===
        ToolDefinition {
            name: "list_images".into(),
            description: "List all container images with their tags, sizes, and creation dates.".into(),
            parameters: json_schema(json!({})),
            category: "Images".into(),
        },
        ToolDefinition {
            name: "pull_image".into(),
            description: "Pull a container image from a registry.".into(),
            parameters: json_schema(json!({ "reference": { "type": "string", "description": "Image reference e.g. nginx:latest, postgres:16" } })),
            category: "Images".into(),
        },
        ToolDefinition {
            name: "remove_image".into(),
            description: "Remove a container image.".into(),
            parameters: json_schema(json!({ "id": { "type": "string" }, "force": { "type": "boolean", "default": false } })),
            category: "Images".into(),
        },
        ToolDefinition {
            name: "prune_images".into(),
            description: "Remove all unused images to free disk space.".into(),
            parameters: json_schema(json!({})),
            category: "Images".into(),
        },

        // === Compose Stacks ===
        ToolDefinition {
            name: "list_stacks".into(),
            description: "List all Docker Compose stacks with their services and status.".into(),
            parameters: json_schema(json!({})),
            category: "Stacks".into(),
        },
        ToolDefinition {
            name: "compose_up".into(),
            description: "Run docker compose up for a stack (starts all services).".into(),
            parameters: json_schema(json!({ "name": { "type": "string", "description": "Stack/project name" } })),
            category: "Stacks".into(),
        },
        ToolDefinition {
            name: "compose_down".into(),
            description: "Run docker compose down for a stack (stops and removes all services).".into(),
            parameters: json_schema(json!({ "name": { "type": "string" } })),
            category: "Stacks".into(),
        },

        // === Volumes & Networks ===
        ToolDefinition {
            name: "list_volumes".into(),
            description: "List all Docker volumes.".into(),
            parameters: json_schema(json!({})),
            category: "Storage".into(),
        },
        ToolDefinition {
            name: "list_networks".into(),
            description: "List all Docker networks.".into(),
            parameters: json_schema(json!({})),
            category: "Networking".into(),
        },
        ToolDefinition {
            name: "create_network".into(),
            description: "Create a new Docker network.".into(),
            parameters: json_schema(json!({
                "name": { "type": "string" },
                "driver": { "type": "string", "default": "bridge" }
            })),
            category: "Networking".into(),
        },

        // === Templates ===
        ToolDefinition {
            name: "list_templates".into(),
            description: "List available one-click app templates (databases, web servers, monitoring tools).".into(),
            parameters: json_schema(json!({})),
            category: "Templates".into(),
        },
        ToolDefinition {
            name: "deploy_template".into(),
            description: "Deploy a pre-configured app template (e.g., PostgreSQL, Redis, Grafana) with one click.".into(),
            parameters: json_schema(json!({
                "id": { "type": "string", "description": "Template ID e.g. postgres, redis, grafana" },
                "name": { "type": "string", "description": "Container name override" },
                "ports": { "type": "array", "items": { "type": "string" }, "description": "Port mapping overrides" },
                "env": { "type": "array", "items": { "type": "string" }, "description": "Env var overrides in KEY=value format" }
            })),
            category: "Templates".into(),
        },

        // === Kubernetes ===
        ToolDefinition {
            name: "k8s_status".into(),
            description: "Get Kubernetes cluster status including version, node info, and pod counts.".into(),
            parameters: json_schema(json!({})),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_pods".into(),
            description: "List Kubernetes pods in a namespace.".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_apply_yaml".into(),
            description: "Apply a Kubernetes YAML manifest to create or update resources.".into(),
            parameters: json_schema(json!({ "yaml": { "type": "string", "description": "YAML manifest content" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_pod_logs".into(),
            description: "Get logs from a Kubernetes pod.".into(),
            parameters: json_schema(json!({
                "namespace": { "type": "string" },
                "name": { "type": "string" },
                "tail": { "type": "integer", "default": 100 }
            })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_namespaces".into(),
            description: "List all Kubernetes namespaces.".into(),
            parameters: json_schema(json!({})),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_deployments".into(),
            description: "List Kubernetes deployments in a namespace.".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_services".into(),
            description: "List Kubernetes services in a namespace.".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_events".into(),
            description: "List recent Kubernetes events in a namespace (useful for debugging).".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_scale_deployment".into(),
            description: "Scale a Kubernetes deployment to a specific number of replicas.".into(),
            parameters: json_schema(json!({
                "namespace": { "type": "string" },
                "name": { "type": "string" },
                "replicas": { "type": "integer" }
            })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_restart_deployment".into(),
            description: "Restart a Kubernetes deployment (rolling restart).".into(),
            parameters: json_schema(json!({
                "namespace": { "type": "string" },
                "name": { "type": "string" }
            })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_delete_pod".into(),
            description: "Delete a Kubernetes pod (it will be recreated by its controller).".into(),
            parameters: json_schema(json!({
                "namespace": { "type": "string" },
                "name": { "type": "string" }
            })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_get_yaml".into(),
            description: "Get the YAML manifest of a Kubernetes resource.".into(),
            parameters: json_schema(json!({
                "kind": { "type": "string", "description": "Resource kind (e.g., pod, deployment, service)" },
                "namespace": { "type": "string" },
                "name": { "type": "string" }
            })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_configmaps".into(),
            description: "List ConfigMaps in a namespace.".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_secrets".into(),
            description: "List Secrets in a namespace (names only, not values).".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_list_ingresses".into(),
            description: "List Ingresses in a namespace.".into(),
            parameters: json_schema(json!({ "namespace": { "type": "string", "default": "default" } })),
            category: "Kubernetes".into(),
        },
        ToolDefinition {
            name: "k8s_helm_list".into(),
            description: "List installed Helm releases.".into(),
            parameters: json_schema(json!({})),
            category: "Kubernetes".into(),
        },
    ]
}

/// The catalog paired with each tool's risk tier.
///
/// Kept separate from [`tool_catalog`] so the 42 definition literals do not
/// each repeat a tier this module already owns, and so callers that only need
/// names and descriptions (the MCP and OpenAI tool listings) are unaffected.
///
/// A tool missing from [`TOOL_RISKS`] gets the most dangerous tier rather than
/// the most permissive one; `every_catalog_tool_has_a_declared_risk` fails the
/// build before that can happen.
pub fn tool_catalog_with_risk() -> Vec<(ToolDefinition, ToolRisk)> {
    tool_catalog()
        .into_iter()
        .map(|tool| {
            let risk = tool_risk(&tool.name).unwrap_or_else(default_tool_risk);
            (tool, risk)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tool_catalog_has_tools() {
        let catalog = tool_catalog();
        assert!(!catalog.is_empty(), "tool catalog should not be empty");
    }

    #[test]
    fn tool_catalog_all_have_names() {
        for tool in tool_catalog() {
            assert!(!tool.name.is_empty(), "every tool must have a non-empty name");
        }
    }

    #[test]
    fn tool_catalog_all_have_descriptions() {
        for tool in tool_catalog() {
            assert!(
                !tool.description.is_empty(),
                "tool '{}' must have a non-empty description",
                tool.name
            );
        }
    }

    #[test]
    fn tool_catalog_unique_names() {
        let catalog = tool_catalog();
        let mut seen = HashSet::new();
        for tool in &catalog {
            assert!(seen.insert(&tool.name), "duplicate tool name: '{}'", tool.name);
        }
    }

    #[test]
    fn tool_catalog_has_diagnose_container() {
        let catalog = tool_catalog();
        assert!(
            catalog.iter().any(|t| t.name == "diagnose_container"),
            "catalog must contain the 'diagnose_container' diagnostic tool"
        );
    }

    /// The point of the risk table is that nothing can be added to the catalog
    /// without someone deciding how dangerous it is. Without this test, a new
    /// tool would silently inherit the `Destructive` fallback — safe, but
    /// permanently unusable and with no hint as to why.
    #[test]
    fn every_catalog_tool_has_a_declared_risk() {
        for tool in tool_catalog() {
            assert!(
                tool_risk(&tool.name).is_some(),
                "tool '{}' is in the catalog but has no entry in TOOL_RISKS",
                tool.name
            );
        }
    }

    /// The other direction: a row left behind after a rename would keep
    /// `tool_risk` answering for a tool that no longer exists, and would let
    /// `every_catalog_tool_has_a_declared_risk` pass while the real tool went
    /// unclassified.
    #[test]
    fn risk_table_has_no_stale_entries() {
        let catalog: HashSet<String> = tool_catalog().into_iter().map(|t| t.name).collect();
        for (name, _) in TOOL_RISKS {
            assert!(
                catalog.contains(*name),
                "TOOL_RISKS classifies '{name}', which is not in the tool catalog"
            );
        }
    }

    #[test]
    fn unknown_tools_have_no_risk_rather_than_a_safe_one() {
        assert_eq!(
            tool_risk("definitely_not_a_tool"),
            None,
            "an unknown name must be distinguishable from a known-safe one, \
             so callers refuse it instead of running it"
        );
    }

    #[test]
    fn tiers_are_cumulative() {
        assert!(ToolRisk::Read.is_permitted_by(ToolRisk::Read));
        assert!(!ToolRisk::Write.is_permitted_by(ToolRisk::Read));
        assert!(!ToolRisk::Destructive.is_permitted_by(ToolRisk::Read));

        assert!(ToolRisk::Read.is_permitted_by(ToolRisk::Write));
        assert!(ToolRisk::Write.is_permitted_by(ToolRisk::Write));
        assert!(!ToolRisk::Destructive.is_permitted_by(ToolRisk::Write));

        assert!(ToolRisk::Destructive.is_permitted_by(ToolRisk::Destructive));
    }

    /// The destructive tier is the one that has to be *right*: a read-only
    /// default is only meaningful if the tools it blocks are the ones that can
    /// lose data. Pin the obvious members so a reclassification has to be
    /// deliberate.
    #[test]
    fn data_destroying_tools_are_tiered_destructive() {
        for name in [
            "remove_container",
            "remove_image",
            "prune_images",
            "compose_down",
            "exec_in_container",
            "k8s_delete_pod",
        ] {
            assert_eq!(
                tool_risk(name),
                Some(ToolRisk::Destructive),
                "'{name}' can destroy data or run arbitrary code and must not be \
                 reachable under a read-only agent policy"
            );
        }
    }

    #[test]
    fn catalog_with_risk_covers_the_catalog_in_order() {
        let catalog = tool_catalog();
        let paired = tool_catalog_with_risk();
        assert_eq!(paired.len(), catalog.len());
        for (plain, (with_risk, _)) in catalog.iter().zip(paired.iter()) {
            assert_eq!(plain.name, with_risk.name, "order must be preserved");
        }
    }
}
