import { createSignal, onMount, For, Show } from "solid-js";
import { t } from "../lib/i18n";
import { invoke } from "@tauri-apps/api/core";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import type { GatewayStatus, GatewayRoute, Container, ComposeProject, StackLinkGroup, TraefikStatus } from "../lib/types";
import { useRefresh } from "../lib/useRefresh";
import { showToast } from "../components/Toast";
import { confirmDanger } from "../components/ConfirmDialog";
import { logError } from "../lib/activityStore";
import { SkeletonCard } from "../components/Skeleton";
import { safeHref } from "../lib/sanitize";

interface GatewayPageProps {
  onNavigate?: (target: string) => void;
}

/** Map common Docker / Caddy errors to user-friendly messages. */
function friendlyStartError(raw: string): string {
  if (raw.includes("port is already allocated")) {
    const port = raw.match(/port\s+(\d+)/i)?.[1] ?? "";
    return `Port ${port} is already in use. Change the port in the Configuration tab or stop the conflicting service.`;
  }
  if (raw.includes("No such image") || raw.includes("not found") || raw.includes("pull")) {
    return "Could not download caddy:2-alpine. Check your internet connection.";
  }
  if (raw.includes("admin API") || raw.includes("not responding")) {
    return t("Gateway started but Caddy is not responding. Check Settings > About > Daemon Log.");
  }
  return raw;
}

type GatewayTab = "routes" | "configuration" | "settings";

export default function GatewayPage(props: GatewayPageProps) {
  const [activeTab, setActiveTab] = createSignal<GatewayTab>("routes");
  const [status, setStatus] = createSignal<GatewayStatus | null>(null);
  const [routes, setRoutes] = createSignal<GatewayRoute[]>([]);
  const [showAdd, setShowAdd] = createSignal(false);
  const [starting, setStarting] = createSignal(false);
  const [startError, setStartError] = createSignal<string | null>(null);

  // Add route form
  const [addHostname, setAddHostname] = createSignal("");
  const [addPath, setAddPath] = createSignal("");
  const [addContainer, setAddContainer] = createSignal("");
  const [addPort, setAddPort] = createSignal("80");
  const [containers, setContainers] = createSignal<Container[]>([]);
  const [adding, setAdding] = createSignal(false);
  const [showContainerPicker, setShowContainerPicker] = createSignal(false);
  const [pickerSearch, setPickerSearch] = createSignal("");
  const [pickerStacks, setPickerStacks] = createSignal<ComposeProject[]>([]);

  // Edit route
  const [editRoute, setEditRoute] = createSignal<GatewayRoute | null>(null);
  const [editHostname, setEditHostname] = createSignal("");
  const [editContainer, setEditContainer] = createSignal("");
  const [editPort, setEditPort] = createSignal("80");
  const [editPath, setEditPath] = createSignal("");
  const [editSaving, setEditSaving] = createSignal(false);

  // Environment links
  const [stackLinks, setStackLinks] = createSignal<StackLinkGroup[]>([]);
  const [selectedEnv, setSelectedEnv] = createSignal("local");
  const [collapsedGroups, setCollapsedGroups] = createSignal<Record<string, boolean>>({});

  // Configuration
  const [cfgDomain, setCfgDomain] = createSignal("localhost");
  const [cfgHttpPort, setCfgHttpPort] = createSignal("80");
  const [cfgHttpsPort, setCfgHttpsPort] = createSignal("443");
  const [cfgTlsMode, setCfgTlsMode] = createSignal<"orca_ca" | "custom">("orca_ca");
  const [cfgCustomCert, setCfgCustomCert] = createSignal("");
  const [cfgCustomKey, setCfgCustomKey] = createSignal("");
  const [cfgSaving, setCfgSaving] = createSignal(false);
  const [cfgLoaded, setCfgLoaded] = createSignal(false);
  const [portConflicts, setPortConflicts] = createSignal<string[]>([]);
  const [checkingPorts, setCheckingPorts] = createSignal(false);
  const [statusLoaded, setStatusLoaded] = createSignal(false);

  // Traefik / K8s integration
  const [traefikStatus, setTraefikStatus] = createSignal<TraefikStatus | null>(null);
  const [traefikMode, setTraefikMode] = createSignal<string>("gateway_only");
  const [traefikHttpPort, setTraefikHttpPort] = createSignal("30080");
  const [traefikHttpsPort, setTraefikHttpsPort] = createSignal("30443");
  const [applyingTraefik, setApplyingTraefik] = createSignal(false);
  // Gate the daemon→form prime so refresh (Ctrl-R / host-switch) doesn't
  // clobber in-progress edits. Reset when the user explicitly saves/cancels.
  const [traefikFormDirty, setTraefikFormDirty] = createSignal(false);
  let traefikFormInitialized = false;

  // Dismissed suggestions
  const [dismissedKeys, setDismissedKeys] = createSignal<string[]>([]);

  const loadConfig = async () => {
    try {
      const config = (await invoke("gateway_get_config")) as any;
      setCfgDomain(config.domain || "localhost");
      setCfgHttpPort(String(config.http_port || 80));
      setCfgHttpsPort(String(config.https_port || 443));
      setCfgTlsMode(config.tls_mode === "custom" ? "custom" : "orca_ca");
      setCfgCustomCert(config.custom_cert || "");
      setCfgCustomKey(config.custom_key || "");
      setCfgLoaded(true);
    } catch { setCfgLoaded(true); }
  };

  const saveConfig = async () => {
    setCfgSaving(true);
    try {
      await invoke("gateway_update_config", {
        domain: cfgDomain(),
        httpPort: parseInt(cfgHttpPort(), 10) || 80,
        httpsPort: parseInt(cfgHttpsPort(), 10) || 443,
        tlsMode: cfgTlsMode(),
        customCert: cfgCustomCert() || null,
        customKey: cfgCustomKey() || null,
      });
      showToast(t("Gateway configuration saved"), "success");
    } catch (e) {
      showToast(t("Failed to save: {error}", { error: String(e) }), "error");
    }
    setCfgSaving(false);
  };

  const checkPorts = async () => {
    setCheckingPorts(true);
    try {
      const httpP = parseInt(cfgHttpPort(), 10) || 80;
      const httpsP = parseInt(cfgHttpsPort(), 10) || 443;
      const result = (await invoke("gateway_check_ports", { httpPort: httpP, httpsPort: httpsP })) as { conflicts: string[] };
      setPortConflicts(result.conflicts || []);
      if (result.conflicts.length === 0) showToast(t("Ports are available"), "success");
    } catch {}
    setCheckingPorts(false);
  };

  const httpConflict = () => portConflicts().find((c) => c.includes("HTTP"));
  const httpsConflict = () => portConflicts().find((c) => c.includes("HTTPS"));

  const fetchStatus = async () => {
    try {
      const s = (await invoke("gateway_status")) as GatewayStatus;
      setStatus(s);
    } catch (e) {
      logError(`Failed to fetch gateway status: ${e}`);
    }
    setStatusLoaded(true);
  };

  const fetchRoutes = async () => {
    try {
      const r = (await invoke("gateway_list_routes")) as GatewayRoute[];
      setRoutes(r);
    } catch (e) {
      logError(`Failed to fetch gateway routes: ${e}`);
    }
  };

  const fetchLinks = async () => {
    try {
      const links = (await invoke("gateway_get_links")) as StackLinkGroup[];
      setStackLinks(links || []);
    } catch {
      // Links may not be available yet
    }
  };

  const fetchDismissed = async () => {
    try {
      const result = (await invoke("gateway_get_dismissed")) as { dismissed: string[] };
      setDismissedKeys(result.dismissed || []);
    } catch {}
  };

  const fetchTraefikStatus = async () => {
    try {
      const ts = (await invoke("gateway_traefik_status")) as TraefikStatus;
      setTraefikStatus(ts);
      // Only prime the form fields on first fetch or when the user hasn't
      // started editing — otherwise a refresh (Ctrl-R / host-switch) would
      // clobber in-progress edits.
      if (!traefikFormInitialized || !traefikFormDirty()) {
        setTraefikMode(ts.mode || "gateway_only");
        setTraefikHttpPort(String(ts.traefik_http_port || 30080));
        setTraefikHttpsPort(String(ts.traefik_https_port || 30443));
        traefikFormInitialized = true;
      }
    } catch { /* K8s may not be running */ }
  };

  const applyTraefikMode = async () => {
    setApplyingTraefik(true);
    try {
      await invoke("gateway_set_traefik_mode", {
        mode: traefikMode(),
        traefikHttpPort: parseInt(traefikHttpPort(), 10) || 30080,
        traefikHttpsPort: parseInt(traefikHttpsPort(), 10) || 30443,
      });
      showToast(t("Traefik integration mode updated"), "success");
      // Successfully persisted — form is no longer dirty, safe to re-prime.
      setTraefikFormDirty(false);
      await fetchTraefikStatus();
    } catch (e) {
      logError(`Failed to set Traefik mode: ${e}`);
      showToast(t("Failed: {error}", { error: String(e) }), "error");
    } finally {
      setApplyingTraefik(false);
    }
  };

  const suggestHostname = (name: string) => {
    let h = name.toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "");
    const parts = h.split("-");
    if (parts.length >= 3 && /^\d+$/.test(parts[parts.length - 1])) {
      h = parts.slice(1, -1).join("-");
    }
    return h;
  };

  // Suggested routes: running containers with exposed ports not yet in the gateway, minus dismissed
  const suggestions = () => {
    const s = status();
    if (!s?.running) return [];
    const routedNames = new Set(routes().map((r) => r.container_name));
    const dismissed = new Set(dismissedKeys());
    return containers().filter((c) => {
      if (c.state !== "Running" || c.ports.length === 0 || routedNames.has(c.name)) return false;
      // Check if all ports for this container are dismissed
      const allDismissed = c.ports.every((p) => dismissed.has(`${c.name}:${p.container_port}`));
      return !allDismissed;
    });
  };

  const fetchContainers = async () => {
    try {
      const [c, s] = await Promise.all([
        invoke("list_containers") as Promise<Container[]>,
        invoke("list_stacks") as Promise<ComposeProject[]>,
      ]);
      setContainers((c || []).filter((x) => x.name !== "orca-gateway"));
      setPickerStacks(s || []);
    } catch {}
  };

  const quickAdd = async (c: Container) => {
    const hostname = suggestHostname(c.name);
    const port = c.ports[0]?.container_port || 80;
    const domain = status()?.domain || "localhost";
    try {
      await invoke("gateway_add_route", { hostname: `${hostname}.${domain}`, containerName: c.name, port });
      showToast(t("Added {hostname}", { hostname: `${hostname}.${domain}` }), "success");
      refresh();
    } catch (e) { showToast(t("Failed: {error}", { error: String(e) }), "error"); }
  };

  const dismissAllPorts = async (container: Container) => {
    const ports = [...new Set(container.ports.map((p) => p.container_port))];
    for (const port of ports) {
      const key = `${container.name}:${port}`;
      if (!dismissedKeys().includes(key)) {
        try {
          await invoke("gateway_dismiss_suggestion", { key });
        } catch {}
      }
    }
    setDismissedKeys((prev) => [
      ...prev,
      ...ports.map((p) => `${container.name}:${p}`).filter((k) => !prev.includes(k)),
    ]);
  };

  const clearDismissed = async () => {
    try {
      await invoke("gateway_clear_dismissed");
      setDismissedKeys([]);
      showToast(t("Dismissed suggestions cleared"), "success");
    } catch (e) {
      showToast(t("Failed to clear: {error}", { error: String(e) }), "error");
    }
  };

  const refresh = () => {
    fetchStatus();
    fetchRoutes();
    fetchLinks();
    fetchContainers();
    fetchTraefikStatus();
    fetchDismissed();
  };

  useRefresh(refresh);
  onMount(() => { refresh(); loadConfig(); });

  const handleStart = async () => {
    setStarting(true);
    setStartError(null);
    try {
      await invoke("gateway_start");
      showToast(t("Gateway started"), "success");
      await refresh();
    } catch (e) {
      const raw = String(e);
      const friendly = friendlyStartError(raw);
      logError(`Failed to start gateway: ${raw}`);
      showToast(t("Failed to start gateway: {error}", { error: friendly }), "error");
      setStartError(friendly);
    }
    setStarting(false);
  };

  const handleStop = async () => {
    try {
      await invoke("gateway_stop");
      showToast(t("Gateway stopped"), "success");
      setStartError(null);
      await refresh();
    } catch (e) {
      logError(`Failed to stop gateway: ${e}`);
      showToast(t("Failed to stop gateway: {error}", { error: String(e) }), "error");
    }
  };

  const openAddDialog = () => {
    setAddHostname("");
    setAddPath("");
    setAddContainer("");
    setAddPort("80");
    fetchContainers();
    setShowAdd(true);
  };

  const handleAddRoute = async (e: Event) => {
    e.preventDefault();
    const hostname = addHostname().trim();
    const containerName = addContainer();
    const port = parseInt(addPort(), 10);
    if (!hostname || !containerName || isNaN(port)) return;

    const s = status();
    const domain = s?.domain || "localhost";
    const fullHostname = hostname.includes(".") ? hostname : `${hostname}.${domain}`;

    setAdding(true);
    try {
      await invoke("gateway_add_route", {
        hostname: fullHostname,
        containerName,
        port,
        path: addPath().trim() || null,
      });
      showToast(t("Route added: {hostname}", { hostname: fullHostname }), "success");
      setShowAdd(false);
      await refresh();
    } catch (e) {
      logError(`Failed to add route: ${e}`);
      showToast(t("Failed to add route: {error}", { error: String(e) }), "error");
    }
    setAdding(false);
  };

  const handleRemoveRoute = async (hostname: string) => {
    if (!(await confirmDanger(t("Remove Route"), t("Remove route for \"{hostname}\"?", { hostname })))) return;
    try {
      await invoke("gateway_remove_route", { hostname });
      showToast(t("Route \"{hostname}\" removed", { hostname }), "success");
      await refresh();
    } catch (e) {
      logError(`Failed to remove route: ${e}`);
      showToast(t("Failed to remove route: {error}", { error: String(e) }), "error");
    }
  };

  const openEditRoute = (route: GatewayRoute) => {
    setEditRoute(route);
    setEditHostname(route.hostname);
    setEditContainer(route.container_name);
    setEditPort(String(route.port));
    setEditPath(route.path || "");
  };

  const handleSaveEdit = async () => {
    const original = editRoute();
    if (!original) return;

    // Validate ALL fields up-front — before any daemon mutation — so we
    // never end up in a half-applied state where the old route has been
    // removed and the new one failed to add.
    const rawHostname = editHostname().trim();
    if (!rawHostname) {
      showToast(t("Hostname is required"), "error");
      return;
    }
    const containerName = editContainer().trim();
    if (!containerName) {
      showToast(t("Container is required"), "error");
      return;
    }
    const port = parseInt(editPort(), 10);
    if (isNaN(port) || port < 1 || port > 65535) {
      showToast(t("Port must be between 1 and 65535"), "error");
      return;
    }
    const domain = status()?.domain || "localhost";
    const newHostname = rawHostname.includes(".") ? rawHostname : `${rawHostname}.${domain}`;
    const pathValue = editPath().trim() || null;

    setEditSaving(true);
    try {
      if (newHostname === original.hostname) {
        // Hostname unchanged — use the in-place update endpoint. It
        // accepts container/port/enabled changes atomically.
        await invoke("gateway_update_route", {
          hostname: original.hostname,
          containerName,
          port,
          enabled: original.enabled,
        });
      } else {
        // Hostname changed — the PUT endpoint keys on hostname so it
        // can't rename. Add under the new hostname FIRST, then remove
        // the old. If the add fails (port conflict, invalid hostname,
        // daemon error), the original route is still intact.
        await invoke("gateway_add_route", {
          hostname: newHostname,
          containerName,
          port,
          path: pathValue,
        });
        try {
          await invoke("gateway_remove_route", { hostname: original.hostname });
        } catch (removeErr) {
          // Both routes exist now — surface a specific warning so the
          // user can clean up manually rather than silently leaving a
          // duplicate.
          logError(`Added new route but failed to remove old "${original.hostname}": ${removeErr}`);
          showToast(t("New route added but failed to remove old \"{hostname}\" — remove it manually", { hostname: original.hostname }), "error");
          setEditRoute(null);
          setEditSaving(false);
          await refresh();
          return;
        }
      }
      showToast(t("Route updated"), "success");
      setEditRoute(null);
      await refresh();
    } catch (e) {
      showToast(t("Failed to update: {error}", { error: String(e) }), "error");
    }
    setEditSaving(false);
  };

  const handleToggleRoute = async (route: GatewayRoute) => {
    try {
      await invoke("gateway_update_route", {
        hostname: route.hostname,
        containerName: route.container_name,
        port: route.port,
        enabled: !route.enabled,
      });
      await refresh();
    } catch (e) {
      logError(`Failed to toggle route: ${e}`);
      showToast(t("Failed to toggle route: {error}", { error: String(e) }), "error");
    }
  };

  const openUrl = (url: string) => {
    const safe = safeHref(url);
    if (!safe) return;
    shellOpen(safe).catch(() => {
      window.open(safe, "_blank", "noopener,noreferrer");
    });
  };

  const landingUrl = () => {
    const s = status();
    if (!s) return "";
    const port = s.https_port;
    return port === 443 ? `https://${s.domain}` : `https://${s.domain}:${port}`;
  };

  const allEnvNames = () => {
    const envs = new Set<string>();
    for (const group of stackLinks()) {
      for (const link of group.links) {
        for (const env of Object.keys(link.urls)) {
          envs.add(env);
        }
      }
    }
    const sorted = Array.from(envs).filter((e) => e !== "local").sort();
    if (envs.has("local")) sorted.unshift("local");
    return sorted;
  };

  const resolveUrl = (env: string, value: string) => {
    if (env === "local" && !value.includes("://")) {
      const s = status();
      const domain = s?.domain || "localhost";
      const port = s?.https_port || 443;
      const hostname = value.includes(".") ? value : `${value}.${domain}`;
      return port === 443 ? `https://${hostname}` : `https://${hostname}:${port}`;
    }
    return value;
  };

  const toggleGroup = (key: string) => {
    setCollapsedGroups((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  let mouseDownOnOverlay = false;
  const handleOverlayMouseDown = (e: MouseEvent) => {
    mouseDownOnOverlay = (e.target as HTMLElement).classList.contains("modal-overlay");
  };
  const handleOverlayClick = (e: MouseEvent) => {
    if (mouseDownOnOverlay && (e.target as HTMLElement).classList.contains("modal-overlay")) {
      setShowAdd(false);
    }
    mouseDownOnOverlay = false;
  };

  const previewUrl = () => {
    const hostname = addHostname().trim();
    if (!hostname) return "";
    const s = status();
    const domain = s?.domain || "localhost";
    const full = hostname.includes(".") ? hostname : `${hostname}.${domain}`;
    const port = s?.https_port || 443;
    const pathPart = addPath().trim();
    const base = port === 443 ? `https://${full}` : `https://${full}:${port}`;
    return pathPart ? `${base}${pathPart}` : base;
  };

  const pickerGrouped = (): { stacks: { name: string; containers: Container[] }[]; standalone: Container[] } => {
    const allContainers = containers();
    const search = pickerSearch().toLowerCase();
    const filtered = search
      ? allContainers.filter((c) => c.name.toLowerCase().includes(search) || c.image.toLowerCase().includes(search))
      : allContainers;

    const stackNames = new Set(pickerStacks().map((s) => s.name));
    const projectContainers = new Map<string, Container[]>();
    const standaloneList: Container[] = [];

    for (const c of filtered) {
      const projectName = c.labels?.["com.docker.compose.project"];
      if (projectName && stackNames.has(projectName)) {
        if (!projectContainers.has(projectName)) {
          projectContainers.set(projectName, []);
        }
        projectContainers.get(projectName)!.push(c);
      } else {
        standaloneList.push(c);
      }
    }

    const groups: { name: string; containers: Container[] }[] = [];
    for (const [name, ctrs] of projectContainers) {
      groups.push({ name, containers: ctrs });
    }
    groups.sort((a, b) => a.name.localeCompare(b.name));

    return { stacks: groups, standalone: standaloneList };
  };

  const selectContainer = (container: Container) => {
    setAddContainer(container.name);
    if (container.ports.length > 0) {
      setAddPort(String(container.ports[0].container_port));
    }
    if (!addHostname().trim()) {
      let suggested = container.name
        .toLowerCase()
        .replace(/[^a-z0-9-]/g, "-")
        .replace(/-+/g, "-")
        .replace(/^-|-$/g, "");
      const parts = suggested.split("-");
      if (parts.length >= 3 && parts[parts.length - 1].match(/^\d+$/)) {
        suggested = parts.slice(1, -1).join("-");
      }
      setAddHostname(suggested);
    }
    setShowContainerPicker(false);
  };

  const formatPortsList = (ports: Container["ports"]): string => {
    if (!ports.length) return "";
    const seen = new Set<number>();
    const result: string[] = [];
    for (const p of ports) {
      if (!seen.has(p.container_port)) {
        seen.add(p.container_port);
        result.push(`:${p.container_port}`);
      }
    }
    return result.join(" ");
  };

  return (
    <div>
      <div class="page-header">
        <h1 class="page-title">{t("Gateway")}</h1>
        <div class="page-actions">
          <Show when={status()?.running}>
            <button class="btn" onClick={() => openUrl(landingUrl())} title={t("Open gateway landing page in browser")}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style={{ "margin-right": "4px", "vertical-align": "-2px" }}>
                <path d="M18 13v6a2 2 0 01-2 2H5a2 2 0 01-2-2V8a2 2 0 012-2h6" /><polyline points="15 3 21 3 21 9" /><line x1="10" y1="14" x2="21" y2="3" />
              </svg>
              {t("Open Gateway")}
            </button>
            <button class="btn" onClick={openAddDialog}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style={{ "margin-right": "4px", "vertical-align": "-2px" }}>
                <line x1="12" y1="5" x2="12" y2="19" /><line x1="5" y1="12" x2="19" y2="12" />
              </svg>
              {t("Add Route")}
            </button>
          </Show>
          <Show
            when={status()?.running}
            fallback={
              <button class="btn btn-primary" onClick={handleStart} disabled={starting()}>
                {starting() ? t("Starting...") : t("Start Gateway")}
              </button>
            }
          >
            <button class="btn" onClick={handleStop} style={{ color: "#f85149", "border-color": "#f85149" }}>
              {t("Stop Gateway")}
            </button>
          </Show>
          <button class="btn" onClick={refresh}>{t("Refresh")}</button>
        </div>
      </div>

      {/* Tab bar */}
      <div class="tab-bar" style={{ "margin-bottom": "24px" }}>
        <button class={`tab-item ${activeTab() === "routes" ? "active" : ""}`} onClick={() => setActiveTab("routes")}>{t("Routes")}</button>
        <button class={`tab-item ${activeTab() === "configuration" ? "active" : ""}`} onClick={() => setActiveTab("configuration")}>{t("Configuration")}</button>
        <button class={`tab-item ${activeTab() === "settings" ? "active" : ""}`} onClick={() => setActiveTab("settings")}>{t("Settings")}</button>
      </div>

      {/* Port conflict warnings (visible across all tabs) */}
      <Show when={(status()?.port_conflicts?.length ?? 0) > 0}>
        <div style={{
          background: "rgba(210, 153, 34, 0.1)",
          border: "1px solid rgba(210, 153, 34, 0.3)",
          "border-radius": "8px",
          padding: "12px 16px",
          "margin-bottom": "16px",
          display: "flex",
          "align-items": "flex-start",
          gap: "10px",
          "font-size": "13px",
          color: "#d29922",
        }}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style={{ "flex-shrink": "0", "margin-top": "1px" }}>
            <path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z" /><line x1="12" y1="9" x2="12" y2="13" /><line x1="12" y1="17" x2="12.01" y2="17" />
          </svg>
          <div>
            <div style={{ "font-weight": "600", "margin-bottom": "4px" }}>{t("Port Conflict Detected")}</div>
            <For each={status()?.port_conflicts ?? []}>
              {(conflict) => <div>{conflict}</div>}
            </For>
            <div style={{ "margin-top": "4px", "font-size": "12px", color: "#8b949e" }}>
              {t("Change the port in the Configuration tab or stop the conflicting service.")}
            </div>
          </div>
        </div>
      </Show>

      {/* Start error banner (visible across all tabs) */}
      <Show when={startError()}>
        <div style={{
          background: "rgba(248, 81, 73, 0.1)",
          border: "1px solid rgba(248, 81, 73, 0.3)",
          "border-radius": "8px",
          padding: "12px 16px",
          "margin-bottom": "16px",
          display: "flex",
          "align-items": "flex-start",
          gap: "10px",
          "font-size": "13px",
          color: "#f85149",
        }}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style={{ "flex-shrink": "0", "margin-top": "1px" }}>
            <circle cx="12" cy="12" r="10" /><line x1="15" y1="9" x2="9" y2="15" /><line x1="9" y1="9" x2="15" y2="15" />
          </svg>
          <div>
            <div style={{ "font-weight": "600", "margin-bottom": "4px" }}>{t("Failed to Start Gateway")}</div>
            <div style={{ color: "#e6edf3" }}>{startError()}</div>
          </div>
          <button
            style={{ "margin-left": "auto", background: "none", border: "none", cursor: "pointer", color: "#8b949e", "flex-shrink": "0" }}
            onClick={() => setStartError(null)}
            title={t("Dismiss")}
          >
            {"\u00d7"}
          </button>
        </div>
      </Show>

      {/* ============ TAB 1: Routes ============ */}
      <Show when={activeTab() === "routes"}>
        {/* Skeleton while status is loading */}
        <Show when={!statusLoaded() && status() === null}>
          <SkeletonCard height="100px" />
          <div style={{ height: "16px" }} />
        </Show>

        {/* Status Card (when running) */}
        <Show when={status()?.running ? status() : undefined}>
          {(s) => (
            <div class="card" style={{ "margin-bottom": "20px" }}>
              <div style={{ display: "grid", "grid-template-columns": "repeat(4, 1fr)", gap: "16px 24px", "font-size": "13px" }}>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("Status")}</span>
                  <span class="card-value" style={{ display: "inline-flex", "align-items": "center", gap: "6px" }}>
                    <span style={{
                      display: "inline-block",
                      width: "8px",
                      height: "8px",
                      "border-radius": "50%",
                      background: "#3fb950",
                    }} />
                    {t("Running")}
                  </span>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("Domain")}</span>
                  <span class="card-value">*.{s().domain}</span>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("TLS")}</span>
                  <span class="card-value">{s().tls_mode === "orca_ca" ? t("Orca CA") : t("Custom")}</span>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("Routes")}</span>
                  <span class="card-value">{s().routes_active} active</span>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("HTTP Port")}</span>
                  <span class="card-value mono">:{s().http_port}</span>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("HTTPS Port")}</span>
                  <span class="card-value mono">:{s().https_port}</span>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                  <span class="card-label">{t("Landing Page")}</span>
                  <span class="card-value">
                    <button
                      class="btn-link"
                      style={{ color: "#58a6ff", background: "none", border: "none", cursor: "pointer", "font-size": "13px", padding: "0" }}
                      onClick={() => openUrl(landingUrl())}
                    >
                      {landingUrl()}
                    </button>
                  </span>
                </div>
                <Show when={s().container_id}>
                  <div style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
                    <span class="card-label">{t("Container ID")}</span>
                    <span class="card-value mono" style={{ "font-size": "11px" }}>{s().container_id?.substring(0, 12)}</span>
                  </div>
                </Show>
              </div>
            </div>
          )}
        </Show>

        {/* Routes Table or Onboarding */}
        <Show
          when={status()?.running}
          fallback={
            <div>
              <div class="card" style={{ "margin-bottom": "16px" }}>
                <div style={{ "margin-bottom": "16px" }}>
                  <div style={{ display: "flex", "align-items": "center", gap: "10px", "margin-bottom": "12px" }}>
                    <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#58a6ff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <circle cx="12" cy="12" r="10" />
                      <line x1="2" y1="12" x2="22" y2="12" />
                      <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
                    </svg>
                    <h2 style={{ color: "#e6edf3", "font-size": "16px", "font-weight": "600", margin: "0" }}>{t("Orca Gateway")}</h2>
                  </div>
                  <p style={{ color: "#8b949e", "font-size": "13px", "line-height": "1.6", margin: "0 0 16px 0" }}>
                    {t("Orca Gateway is a managed reverse proxy that gives your containers clean hostnames with automatic TLS.")}
                  </p>
                  <div style={{
                    background: "#161b22",
                    "border-radius": "8px",
                    padding: "12px 16px",
                    "margin-bottom": "16px",
                    "font-family": "'SF Mono', 'Fira Code', monospace",
                    "font-size": "13px",
                    "line-height": "1.8",
                  }}>
                    <div style={{ color: "#8b949e" }}>{t("Instead of:")} <span style={{ color: "#f85149" }}>http://localhost:8095</span></div>
                    <div style={{ color: "#8b949e" }}>{t("Access at:")} <span style={{ color: "#3fb950" }}>https://webmail.localhost</span></div>
                  </div>
                  <div style={{ display: "flex", "flex-direction": "column", gap: "8px", color: "#c9d1d9", "font-size": "13px" }}>
                    <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
                      <span style={{ color: "#3fb950" }}>&#x2022;</span> {t("Automatic HTTPS via the Orca Certificate Authority")}
                    </div>
                    <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
                      <span style={{ color: "#3fb950" }}>&#x2022;</span> .localhost domains work in all browsers
                    </div>
                    <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
                      <span style={{ color: "#3fb950" }}>&#x2022;</span> {t("Custom domains for teams (*.local.company.dev)")}
                    </div>
                    <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
                      <span style={{ color: "#3fb950" }}>&#x2022;</span> {t("WebSocket, SSE, HTTP/2 proxied transparently")}
                    </div>
                    <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
                      <span style={{ color: "#3fb950" }}>&#x2022;</span> {t("orca.yaml in your repo auto-registers routes")}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: "8px", "margin-top": "20px" }}>
                    <button class="btn btn-primary" onClick={handleStart} disabled={starting()}>
                      {starting() ? "Starting..." : "Start Gateway"}
                    </button>
                  </div>
                </div>
              </div>

              <div class="card">
                <h3 style={{ color: "#e6edf3", "font-size": "14px", "font-weight": "600", margin: "0 0 12px 0" }}>{t("Prerequisites")}</h3>
                <div style={{ display: "flex", "flex-direction": "column", gap: "10px", color: "#c9d1d9", "font-size": "13px" }}>
                  <div style={{ display: "flex", gap: "10px" }}>
                    <span style={{ color: "#58a6ff", "font-weight": "600", "flex-shrink": "0" }}>1.</span>
                    <span>
                      Install the Orca CA certificate for trusted TLS.{" "}
                      <button
                        class="btn-link"
                        style={{ color: "#58a6ff", background: "none", border: "none", cursor: "pointer", "font-size": "13px", padding: "0" }}
                        onClick={() => props.onNavigate?.("settings:certificates")}
                      >
                        {t("Settings → Certificates → Download CA")}
                      </button>
                    </span>
                  </div>
                  <div style={{ display: "flex", gap: "10px" }}>
                    <span style={{ color: "#58a6ff", "font-weight": "600", "flex-shrink": "0" }}>2.</span>
                    <span>{t("Start the Gateway (pulls caddy:2-alpine, ~40MB)")}</span>
                  </div>
                  <div style={{ display: "flex", gap: "10px" }}>
                    <span style={{ color: "#58a6ff", "font-weight": "600", "flex-shrink": "0" }}>3.</span>
                    <span>{t("Expose containers via the container detail page or here")}</span>
                  </div>
                </div>
              </div>
            </div>
          }
        >
          <Show
            when={routes().length > 0}
            fallback={
              <div class="empty">
                <div class="empty-icon">
                  <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round">
                    <circle cx="12" cy="12" r="10" />
                    <line x1="2" y1="12" x2="22" y2="12" />
                    <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
                  </svg>
                </div>
                <p class="empty-title">{t("No routes configured")}</p>
                <p>{t("Add a route to map a hostname to a running container.")}</p>
                <button class="btn btn-primary" onClick={openAddDialog} style={{ "margin-top": "12px" }}>
                  {t("Add Route")}
                </button>
              </div>
            }
          >
            <table class="table">
              <thead>
                <tr>
                  <th>{t("Hostname")}</th>
                  <th>{t("Container")}</th>
                  <th>{t("Port")}</th>
                  <th>{t("Status")}</th>
                  <th style={{ "text-align": "right" }}>{t("Actions")}</th>
                </tr>
              </thead>
              <tbody>
                <For each={routes()}>
                  {(route) => (
                    <tr>
                      <td>
                        <button
                          class="btn-link"
                          style={{ color: "#58a6ff", cursor: "pointer", background: "none", border: "none", "font-size": "13px", padding: "0", "word-break": "break-all" }}
                          onClick={() => route.url && openUrl(route.url)}
                          title={route.url}
                        >
                          {route.hostname}{route.path || ""}
                        </button>
                      </td>
                      <td class="mono" style={{ color: "#c9d1d9", "word-break": "break-all" }}>{route.container_name}</td>
                      <td class="mono">{route.port}</td>
                      <td>
                        <span style={{
                          display: "inline-flex",
                          "align-items": "center",
                          gap: "6px",
                          color: route.enabled ? "#3fb950" : "#8b949e",
                          "font-size": "12px",
                        }}>
                          <span style={{
                            display: "inline-block",
                            width: "6px",
                            height: "6px",
                            "border-radius": "50%",
                            background: route.enabled ? "#3fb950" : "#8b949e",
                          }} />
                          {route.enabled ? t("Active") : t("Disabled")}
                        </span>
                      </td>
                      <td style={{ "text-align": "right" }}>
                        <button
                          class="action-icon"
                          title={route.enabled ? t("Pause route") : t("Resume route")}
                          onClick={() => handleToggleRoute(route)}
                          style={{ "margin-right": "4px", color: route.enabled ? "#3fb950" : "#484f58" }}
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/></svg>
                        </button>
                        <button
                          class="action-icon"
                          title={t("Edit route")}
                          onClick={() => openEditRoute(route)}
                          style={{ "margin-right": "4px" }}
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
                        </button>
                        <button
                          class="action-icon action-icon-delete"
                          title={t("Remove route")}
                          onClick={() => handleRemoveRoute(route.hostname)}
                          style={{ color: "#f85149" }}
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 01-2 2H7a2 2 0 01-2-2V6m3 0V4a2 2 0 012-2h4a2 2 0 012 2v2"/></svg>
                        </button>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </Show>
        </Show>

        {/* Suggested Routes */}
        <Show when={status()?.running && suggestions().length > 0}>
          <div style={{ "margin-top": "20px" }}>
            <h3 style={{ color: "#e6edf3", "font-size": "14px", "font-weight": "600", "margin-bottom": "4px" }}>{t("Suggested Routes")}</h3>
            <p style={{ color: "#6e7681", "font-size": "12px", "margin-bottom": "12px" }}>{t("Containers with exposed ports not yet in the gateway")}</p>
            <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
              <For each={suggestions()}>
                {(container) => (
                  <div style={{
                    padding: "12px 16px",
                    background: "rgba(22,27,34,0.5)",
                    border: "1px solid rgba(255,255,255,0.06)",
                    "border-radius": "10px",
                    display: "flex",
                    "align-items": "center",
                    gap: "12px",
                    "min-height": "48px",
                  }}>
                    <div style={{ flex: "1", "min-width": "0" }}>
                      <div style={{ "font-weight": "600", "font-size": "13px", color: "#e6edf3" }}>{container.name}</div>
                      <div style={{ "font-size": "12px", color: "#8b949e", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{container.image}</div>
                    </div>
                    <div style={{ display: "flex", gap: "4px", "flex-wrap": "wrap", "flex-shrink": "0" }}>
                      <For each={[...new Set(container.ports.map((p) => p.container_port))]}>
                        {(port) => (
                          <span class="mono" style={{
                            color: "#8b949e",
                            "font-size": "11px",
                            background: "#161b22",
                            padding: "1px 6px",
                            "border-radius": "4px",
                            "border": "1px solid #30363d",
                          }}>:{port}</span>
                        )}
                      </For>
                    </div>
                    <button class="btn btn-sm btn-primary" onClick={() => quickAdd(container)} style={{ "flex-shrink": "0", "white-space": "nowrap", "font-size": "12px", padding: "4px 12px" }}>
                      {t("Add to Gateway")}
                    </button>
                    <button
                      title={t("Dismiss suggestion")}
                      onClick={() => dismissAllPorts(container)}
                      style={{
                        background: "none",
                        border: "none",
                        cursor: "pointer",
                        color: "#484f58",
                        "flex-shrink": "0",
                        padding: "4px",
                        "font-size": "16px",
                        "line-height": "1",
                      }}
                    >
                      {"\u00d7"}
                    </button>
                  </div>
                )}
              </For>
            </div>
          </div>
        </Show>

        {/* CA Trust Note */}
        <Show when={status()?.running && status()?.tls_mode === "orca_ca"}>
          <div class="card" style={{ "margin-top": "16px", background: "#161b22", "border-color": "#30363d" }}>
            <div style={{ display: "flex", "align-items": "center", gap: "10px", "font-size": "12px", color: "#8b949e" }}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="#58a6ff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <circle cx="12" cy="12" r="10" /><line x1="12" y1="16" x2="12" y2="12" /><line x1="12" y1="8" x2="12.01" y2="8" />
              </svg>
              <span>
                {t("Install the Orca CA certificate to trust HTTPS connections.")}{" "}
                <button
                  class="btn-link"
                  style={{ color: "#58a6ff", background: "none", border: "none", cursor: "pointer", "font-size": "12px", padding: "0" }}
                  onClick={() => props.onNavigate?.("settings:certificates")}
                >
                  {t("Settings → Certificates")}
                </button>
              </span>
            </div>
          </div>
        </Show>

        {/* Environment Links (read-only) */}
        <Show when={status()?.running && stackLinks().length > 0}>
          <div style={{ "margin-top": "24px" }}>
            <h3 style={{ color: "#e6edf3", "font-size": "14px", "font-weight": "600", "margin-bottom": "12px" }}>{t("Environment Links")}</h3>

            <Show when={allEnvNames().length > 1}>
              <div style={{ display: "flex", gap: "4px", "margin-bottom": "16px", "flex-wrap": "wrap" }}>
                <For each={allEnvNames()}>
                  {(env) => (
                    <button
                      class="btn"
                      style={{
                        "font-size": "12px",
                        padding: "4px 12px",
                        "text-transform": "capitalize",
                        ...(selectedEnv() === env
                          ? { background: "#1f6feb", color: "#fff", "border-color": "#1f6feb" }
                          : {}),
                      }}
                      onClick={() => setSelectedEnv(env)}
                    >
                      {env}
                    </button>
                  )}
                </For>
              </div>
            </Show>

            <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
              <For each={stackLinks()}>
                {(group) => {
                  const groupKey = () => `${group.stack}:${group.group}`;
                  const isCollapsed = () => collapsedGroups()[groupKey()] ?? false;
                  return (
                    <div style={{
                      background: "rgba(22, 27, 34, 0.6)",
                      border: "1px solid rgba(255,255,255,0.06)",
                      "border-radius": "12px",
                      overflow: "hidden",
                    }}>
                      <div style={{
                        display: "flex",
                        "align-items": "center",
                        gap: "8px",
                        padding: "10px 16px",
                        "border-bottom": isCollapsed() ? "none" : "1px solid rgba(255,255,255,0.06)",
                      }}>
                        <button
                          onClick={() => toggleGroup(groupKey())}
                          style={{
                            background: "none",
                            border: "none",
                            color: "#e6edf3",
                            cursor: "pointer",
                            padding: "0",
                            display: "flex",
                            "align-items": "center",
                          }}
                        >
                          <svg
                            width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                            stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
                            style={{ transform: isCollapsed() ? "none" : "rotate(90deg)", transition: "transform 0.15s" }}
                          >
                            <polyline points="9 18 15 12 9 6" />
                          </svg>
                        </button>
                        <span style={{ color: "#6e7681", "font-size": "12px", "flex-shrink": "0" }}>{group.stack}</span>
                        <span style={{ color: "#484f58" }}>/</span>
                        <span style={{ color: "#e6edf3", "font-size": "13px", "font-weight": "600", flex: "1", "min-width": "0" }}>
                          {group.group}
                        </span>
                      </div>

                      <Show when={!isCollapsed()}>
                        <div>
                          <For each={group.links}>
                            {(link) => {
                              const rawUrl = () => link.urls[selectedEnv()] || "";
                              const resolved = () => rawUrl() ? resolveUrl(selectedEnv(), rawUrl()) : "";

                              return (
                                <div style={{
                                  display: "flex",
                                  "align-items": "center",
                                  padding: "8px 16px",
                                  "border-bottom": "1px solid rgba(255,255,255,0.04)",
                                  gap: "8px",
                                }}>
                                  <span style={{
                                    color: "#c9d1d9",
                                    "font-size": "13px",
                                    width: "160px",
                                    "flex-shrink": "0",
                                    overflow: "hidden",
                                    "text-overflow": "ellipsis",
                                    "white-space": "nowrap",
                                  }}>
                                    {link.name}
                                  </span>

                                  <Show
                                    when={resolved()}
                                    fallback={
                                      <span class="mono" style={{ color: "#484f58", "font-size": "12px", flex: "1", "font-style": "italic" }}>
                                        no {selectedEnv()} URL
                                      </span>
                                    }
                                  >
                                    <button
                                      class="btn-link mono"
                                      style={{
                                        color: "#58a6ff",
                                        "font-size": "12px",
                                        flex: "1",
                                        "min-width": "0",
                                        overflow: "hidden",
                                        "text-overflow": "ellipsis",
                                        "white-space": "nowrap",
                                        background: "none",
                                        border: "none",
                                        cursor: "pointer",
                                        padding: "0",
                                        "text-align": "left",
                                      }}
                                      onClick={() => openUrl(resolved())}
                                      title={resolved()}
                                    >
                                      {resolved()}
                                    </button>
                                  </Show>

                                  <Show when={resolved()}>
                                    <button
                                      class="action-icon"
                                      title={t("Open in browser")}
                                      onClick={() => openUrl(resolved())}
                                      style={{ "flex-shrink": "0" }}
                                    >
                                      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <path d="M18 13v6a2 2 0 01-2 2H5a2 2 0 01-2-2V8a2 2 0 012-2h6" />
                                        <polyline points="15 3 21 3 21 9" />
                                        <line x1="10" y1="14" x2="21" y2="3" />
                                      </svg>
                                    </button>
                                  </Show>
                                </div>
                              );
                            }}
                          </For>
                        </div>
                      </Show>
                    </div>
                  );
                }}
              </For>
            </div>

            <p style={{ "font-size": "11px", color: "#6e7681", "margin-top": "10px" }}>
              {t("Environment links are configured in your project's")} <code style={{ background: "#161b22", padding: "2px 6px", "border-radius": "4px", "font-size": "11px" }}>orca.yaml</code>
            </p>
          </div>
        </Show>
      </Show>

      {/* ============ TAB 2: Configuration ============ */}
      <Show when={activeTab() === "configuration"}>
        <Show when={cfgLoaded()} fallback={<SkeletonCard height="200px" />}>
          <div class="card" style={{ padding: "20px" }}>
            <div class="form-group">
              <label class="form-label">{t("Domain")}</label>
              <input class="form-input" type="text" value={cfgDomain()} onInput={(e) => setCfgDomain(e.currentTarget.value)} placeholder="localhost" />
              <p style={{ "font-size": "11px", color: "#6e7681", "margin-top": "4px" }}>{t("Routes will be created as subdomains (e.g., myapp.{domain})", { domain: cfgDomain() })}</p>
            </div>

            <div style={{ display: "grid", "grid-template-columns": "1fr 1fr", gap: "12px" }}>
              <div class="form-group">
                <label class="form-label">{t("HTTP Port")}</label>
                <input class="form-input" type="number" value={cfgHttpPort()} onInput={(e) => setCfgHttpPort(e.currentTarget.value)} min="1" max="65535" style={httpConflict() ? { "border-color": "#d29922" } : undefined} />
                <Show when={httpConflict()}><p style={{ "font-size": "11px", color: "#d29922", "margin-top": "4px" }}>{httpConflict()}</p></Show>
              </div>
              <div class="form-group">
                <label class="form-label">{t("HTTPS Port")}</label>
                <input class="form-input" type="number" value={cfgHttpsPort()} onInput={(e) => setCfgHttpsPort(e.currentTarget.value)} min="1" max="65535" style={httpsConflict() ? { "border-color": "#d29922" } : undefined} />
                <Show when={httpsConflict()}><p style={{ "font-size": "11px", color: "#d29922", "margin-top": "4px" }}>{httpsConflict()}</p></Show>
              </div>
            </div>

            <div style={{ "margin-top": "12px", "margin-bottom": "12px" }}>
              <button class="btn" onClick={checkPorts} disabled={checkingPorts()} style={{ "font-size": "12px" }}>
                {checkingPorts() ? t("Checking...") : t("Check Ports")}
              </button>
            </div>

            <div class="form-group">
              <label class="form-label">{t("TLS Mode")}</label>
              <div style={{ display: "flex", gap: "8px" }}>
                <button class="btn" style={{ background: cfgTlsMode() === "orca_ca" ? "#1f6feb" : undefined, color: cfgTlsMode() === "orca_ca" ? "#fff" : undefined, "border-color": cfgTlsMode() === "orca_ca" ? "#1f6feb" : undefined }} onClick={() => setCfgTlsMode("orca_ca")}>{t("Orca CA (automatic)")}</button>
                <button class="btn" style={{ background: cfgTlsMode() === "custom" ? "#1f6feb" : undefined, color: cfgTlsMode() === "custom" ? "#fff" : undefined, "border-color": cfgTlsMode() === "custom" ? "#1f6feb" : undefined }} onClick={() => setCfgTlsMode("custom")}>{t("Custom Certificate")}</button>
              </div>
            </div>

            <Show when={cfgTlsMode() === "orca_ca"}>
              <div style={{ background: "#161b22", "border-radius": "6px", padding: "10px 14px", "margin-bottom": "12px", display: "flex", "align-items": "center", gap: "8px", "font-size": "12px", color: "#8b949e" }}>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#58a6ff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
                {t("Install the Orca CA certificate for trusted HTTPS.")} <a style={{ color: "#58a6ff", "margin-left": "4px", cursor: "pointer" }} onClick={() => props.onNavigate?.("settings:certificates")}>{t("Go to Certificates")}</a>
              </div>
            </Show>

            <Show when={cfgTlsMode() === "custom"}>
              <div class="form-group">
                <label class="form-label">{t("Certificate PEM")}</label>
                <textarea class="form-input" rows={4} value={cfgCustomCert()} onInput={(e) => setCfgCustomCert(e.currentTarget.value)} placeholder={"-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----"} style={{ "font-family": "monospace", "font-size": "11px" }} />
              </div>
              <div class="form-group">
                <label class="form-label">{t("Private Key PEM")}</label>
                <textarea class="form-input" rows={4} value={cfgCustomKey()} onInput={(e) => setCfgCustomKey(e.currentTarget.value)} placeholder={"-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----"} style={{ "font-family": "monospace", "font-size": "11px" }} />
              </div>
            </Show>

            <div style={{ display: "flex", "align-items": "center", "justify-content": "space-between", "margin-top": "16px" }}>
              <button class="btn btn-primary" onClick={saveConfig} disabled={cfgSaving()}>
                {cfgSaving() ? t("Saving...") : t("Save Configuration")}
              </button>
              <span style={{ "font-size": "11px", color: "#6e7681" }}>{t("Changes may require restarting the gateway")}</span>
            </div>
          </div>
        </Show>

        {/* Kubernetes Integration */}
        <Show when={traefikStatus()?.traefik_detected}>
          <div style={{ "margin-top": "24px" }}>
            <h3 style={{ "font-size": "14px", "font-weight": "600", color: "#e6edf3", "margin-bottom": "16px" }}>{t("Kubernetes Integration")}</h3>
            <div style={{ display: "flex", gap: "12px", "flex-wrap": "wrap", "margin-bottom": "16px" }}>
              <div
                class="card"
                style={{
                  flex: "1", "min-width": "200px", padding: "16px", cursor: "pointer",
                  border: traefikMode() === "gateway_only" ? "1px solid #1f6feb" : "1px solid #30363d",
                  background: traefikMode() === "gateway_only" ? "rgba(31,111,235,0.08)" : "#161b22",
                }}
                onClick={() => { setTraefikMode("gateway_only"); setTraefikFormDirty(true); }}
              >
                <div style={{ display: "flex", "align-items": "center", gap: "8px", "margin-bottom": "8px" }}>
                  <div style={{
                    width: "16px", height: "16px", "border-radius": "50%",
                    border: traefikMode() === "gateway_only" ? "5px solid #1f6feb" : "2px solid #484f58",
                    "box-sizing": "border-box",
                  }} />
                  <span style={{ "font-weight": "600", "font-size": "13px", color: "#e6edf3" }}>{t("Gateway Only")}</span>
                </div>
                <div style={{ "font-size": "12px", color: "#8b949e" }}>
                  {t("Gateway and Traefik run independently. Traefik keeps its default ports.")}
                </div>
              </div>

              <div
                class="card"
                style={{
                  flex: "1", "min-width": "200px", padding: "16px", cursor: "pointer",
                  border: traefikMode() === "separate_ports" ? "1px solid #1f6feb" : "1px solid #30363d",
                  background: traefikMode() === "separate_ports" ? "rgba(31,111,235,0.08)" : "#161b22",
                }}
                onClick={() => { setTraefikMode("separate_ports"); setTraefikFormDirty(true); }}
              >
                <div style={{ display: "flex", "align-items": "center", gap: "8px", "margin-bottom": "8px" }}>
                  <div style={{
                    width: "16px", height: "16px", "border-radius": "50%",
                    border: traefikMode() === "separate_ports" ? "5px solid #1f6feb" : "2px solid #484f58",
                    "box-sizing": "border-box",
                  }} />
                  <span style={{ "font-weight": "600", "font-size": "13px", color: "#e6edf3" }}>{t("Separate Ports")}</span>
                </div>
                <div style={{ "font-size": "12px", color: "#8b949e" }}>
                  {t("Each listens on its own ports. Traefik moves to custom NodePorts.")}
                </div>
              </div>

              <div
                class="card"
                style={{
                  flex: "1", "min-width": "200px", padding: "16px", cursor: "pointer",
                  border: traefikMode() === "gateway_proxies_traefik" ? "1px solid #1f6feb" : "1px solid #30363d",
                  background: traefikMode() === "gateway_proxies_traefik" ? "rgba(31,111,235,0.08)" : "#161b22",
                }}
                onClick={() => { setTraefikMode("gateway_proxies_traefik"); setTraefikFormDirty(true); }}
              >
                <div style={{ display: "flex", "align-items": "center", gap: "8px", "margin-bottom": "8px" }}>
                  <div style={{
                    width: "16px", height: "16px", "border-radius": "50%",
                    border: traefikMode() === "gateway_proxies_traefik" ? "5px solid #1f6feb" : "2px solid #484f58",
                    "box-sizing": "border-box",
                  }} />
                  <span style={{ "font-weight": "600", "font-size": "13px", color: "#e6edf3" }}>{t("Gateway Proxies Traefik")}</span>
                </div>
                <div style={{ "font-size": "12px", color: "#8b949e" }}>
                  {t("Single entry point. K8s ingress hostnames are auto-routed through the gateway.")}
                </div>
              </div>
            </div>

            <Show when={traefikMode() !== "gateway_only"}>
              <div class="card" style={{ padding: "16px", "margin-bottom": "16px" }}>
                <div style={{ display: "flex", gap: "16px", "align-items": "center" }}>
                  <div class="form-group" style={{ "margin-bottom": "0" }}>
                    <label class="form-label" style={{ "font-size": "11px" }}>{t("Traefik HTTP Port")}</label>
                    <input class="form-input" type="number" value={traefikHttpPort()} onInput={(e) => { setTraefikHttpPort(e.currentTarget.value); setTraefikFormDirty(true); }} style={{ width: "100px" }} />
                  </div>
                  <div class="form-group" style={{ "margin-bottom": "0" }}>
                    <label class="form-label" style={{ "font-size": "11px" }}>{t("Traefik HTTPS Port")}</label>
                    <input class="form-input" type="number" value={traefikHttpsPort()} onInput={(e) => { setTraefikHttpsPort(e.currentTarget.value); setTraefikFormDirty(true); }} style={{ width: "100px" }} />
                  </div>
                </div>
              </div>
            </Show>

            <div style={{ display: "flex", "align-items": "center", gap: "12px", "margin-bottom": "16px" }}>
              <button class="btn btn-primary" onClick={applyTraefikMode} disabled={applyingTraefik()}>
                {applyingTraefik() ? t("Applying...") : t("Apply Changes")}
              </button>
              <Show when={traefikStatus()}>
                <span style={{ "font-size": "12px", color: "#8b949e" }}>
                  Traefik: {traefikStatus()!.traefik_reachable ? "reachable" : "not reachable"}
                  {traefikStatus()!.traefik_ports ? ` (${traefikStatus()!.traefik_ports!.type})` : ""}
                </span>
              </Show>
            </div>

            <Show when={traefikMode() === "gateway_proxies_traefik" && (traefikStatus()?.k8s_ingress_hostnames?.length ?? 0) > 0}>
              <div class="card" style={{ padding: "16px" }}>
                <div style={{ "font-size": "12px", "font-weight": "600", color: "#e6edf3", "margin-bottom": "8px" }}>
                  {t("Auto-discovered K8s Ingress Hostnames")}
                </div>
                <table class="table" style={{ "margin-bottom": "0" }}>
                  <thead>
                    <tr>
                      <th>{t("Hostname")}</th>
                      <th>{t("Namespace")}</th>
                      <th>{t("Ingress")}</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    <For each={traefikStatus()!.k8s_ingress_hostnames}>
                      {(entry) => (
                        <tr>
                          <td><code style={{ color: "#79c0ff" }}>{entry.hostname}</code></td>
                          <td>{entry.namespace}</td>
                          <td>{entry.ingress_name}</td>
                          <td>
                            <span style={{
                              background: "#1f3a5f", color: "#58a6ff",
                              padding: "2px 6px", "border-radius": "4px",
                              "font-size": "10px", "font-weight": "600",
                            }}>K8s</span>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            </Show>
          </div>
        </Show>
      </Show>

      {/* ============ TAB 3: Settings ============ */}
      <Show when={activeTab() === "settings"}>
        <div class="card" style={{ padding: "20px" }}>
          <h3 style={{ color: "#e6edf3", "font-size": "14px", "font-weight": "600", margin: "0 0 16px 0" }}>{t("Gateway Settings")}</h3>

          {/* Clear dismissed suggestions */}
          <div style={{ display: "flex", "align-items": "center", "justify-content": "space-between", padding: "12px 0", "border-bottom": "1px solid #21262d" }}>
            <div>
              <div style={{ color: "#e6edf3", "font-size": "13px", "font-weight": "500" }}>{t("Dismissed Suggestions")}</div>
              <div style={{ color: "#8b949e", "font-size": "12px", "margin-top": "2px" }}>
                {dismissedKeys().length === 0
                  ? t("No dismissed suggestions")
                  : `${dismissedKeys().length} suggestion${dismissedKeys().length === 1 ? "" : "s"} dismissed`}
              </div>
            </div>
            <button
              class="btn"
              style={{ "font-size": "12px" }}
              onClick={clearDismissed}
              disabled={dismissedKeys().length === 0}
            >
              {t("Clear Dismissed")}
            </button>
          </div>

          {/* Info note */}
          <div style={{ "margin-top": "16px", background: "#161b22", "border-radius": "6px", padding: "10px 14px", display: "flex", "align-items": "center", gap: "8px", "font-size": "12px", color: "#8b949e" }}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#58a6ff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
            {t("Gateway-specific settings. For general configuration (domain, ports, TLS), use the Configuration tab.")}
          </div>
        </div>
      </Show>

      {/* Add Route Dialog */}
      <Show when={showAdd()}>
        <div class="modal-overlay" onMouseDown={handleOverlayMouseDown} onClick={handleOverlayClick}>
          <div class="modal-dialog">
            <div class="modal-header">
              <h2 class="modal-title">{t("Add Route")}</h2>
              <button class="modal-close" onClick={() => setShowAdd(false)}>
                {"\u00d7"}
              </button>
            </div>
            <form onSubmit={handleAddRoute}>
              <div class="modal-body">
                <p style={{ "font-size": "12px", color: "#8b949e", "margin-bottom": "16px", "line-height": "1.6" }}>
                  {t("Map a hostname to a container's internal port. The Gateway serves it over HTTPS on ports")} {status()?.http_port || 80}/{status()?.https_port || 443}.
                </p>
                <div class="form-group">
                  <label class="form-label">
                    {t("Hostname")} <span style={{ color: "#f85149" }}>*</span>
                  </label>
                  <div style={{ display: "flex", "align-items": "center", gap: "4px" }}>
                    <input
                      class="form-input"
                      type="text"
                      placeholder="myapp"
                      value={addHostname()}
                      onInput={(e) => setAddHostname(e.currentTarget.value)}
                      autofocus
                      style={{ flex: "1" }}
                    />
                    <span style={{ color: "#8b949e", "font-size": "13px", "white-space": "nowrap" }}>
                      .{status()?.domain || "localhost"}
                    </span>
                  </div>
                </div>

                <div class="form-group">
                  <label class="form-label">{t("Path (optional)")}</label>
                  <input
                    class="form-input"
                    type="text"
                    placeholder="/api/*"
                    value={addPath()}
                    onInput={(e) => setAddPath(e.currentTarget.value)}
                  />
                  <p style={{ "font-size": "11px", color: "#6e7681", "margin-top": "4px" }}>
                    Optional. Route a specific path to this container (e.g., /api/*, /ws/*)
                  </p>
                </div>

                <div class="form-group">
                  <label class="form-label">
                    {t("Container")} <span style={{ color: "#f85149" }}>*</span>
                  </label>
                  <button
                    type="button"
                    class="form-input"
                    style={{
                      "text-align": "left",
                      cursor: "pointer",
                      display: "flex",
                      "align-items": "center",
                      "justify-content": "space-between",
                      color: addContainer() ? "#e6edf3" : "#6e7681",
                    }}
                    onClick={() => { setPickerSearch(""); setShowContainerPicker(true); }}
                  >
                    <span>{addContainer() || "Select container..."}</span>
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <polyline points="6 9 12 15 18 9" />
                    </svg>
                  </button>
                </div>

                <div class="form-group">
                  <label class="form-label">
                    {t("Container Port")} <span style={{ color: "#f85149" }}>*</span>
                  </label>
                  <input
                    class="form-input"
                    type="number"
                    placeholder="80"
                    value={addPort()}
                    onInput={(e) => setAddPort(e.currentTarget.value)}
                    min="1"
                    max="65535"
                  />
                  <p style={{ "font-size": "11px", color: "#6e7681", "margin-top": "4px" }}>
                    The port your app listens on inside the container (e.g., 8080, 3000)
                  </p>
                </div>

                <Show when={previewUrl()}>
                  <div style={{ background: "#161b22", "border-radius": "6px", padding: "10px 14px", "margin-top": "12px", "font-size": "12px", "line-height": "1.6" }}>
                    <span style={{ color: "#8b949e" }}>{t("Routing:")}</span>
                    <span class="mono" style={{ color: "#58a6ff" }}>{previewUrl()}</span>
                    <Show when={addContainer() && addPort()}>
                      <span style={{ color: "#6e7681" }}>{" \u2192 "}</span>
                      <span class="mono" style={{ color: "#3fb950" }}>{addContainer()}:{addPort()}</span>
                    </Show>
                  </div>
                </Show>
              </div>

              <div class="modal-footer">
                <button type="button" class="btn" onClick={() => setShowAdd(false)} disabled={adding()}>
                  {t("Cancel")}
                </button>
                <button
                  type="submit"
                  class="btn btn-primary"
                  disabled={adding() || !addHostname().trim() || !addContainer()}
                >
                  {adding() ? t("Adding...") : t("Add Route")}
                </button>
              </div>
            </form>
          </div>
        </div>
      </Show>

      {/* Container Picker Modal */}
      <Show when={showContainerPicker()}>
        <div class="modal-overlay" onMouseDown={(e) => { mouseDownOnOverlay = (e.target as HTMLElement).classList.contains("modal-overlay"); }} onClick={(e) => { if (mouseDownOnOverlay && (e.target as HTMLElement).classList.contains("modal-overlay")) setShowContainerPicker(false); mouseDownOnOverlay = false; }}>
          <div class="modal-dialog" style={{ "max-width": "620px", "max-height": "70vh", display: "flex", "flex-direction": "column" }}>
            <div class="modal-header">
              <h2 class="modal-title">{t("Select Container")}</h2>
              <button class="modal-close" onClick={() => setShowContainerPicker(false)}>{"\u00d7"}</button>
            </div>
            <div style={{ padding: "16px 20px 8px" }}>
              <input
                class="form-input"
                type="text"
                placeholder={t("Search containers...")}
                value={pickerSearch()}
                onInput={(e) => setPickerSearch(e.currentTarget.value)}
                autofocus
                style={{ width: "100%" }}
              />
            </div>
            <div style={{ padding: "0 20px 16px", "overflow-y": "auto", flex: "1" }}>
              <For each={pickerGrouped().stacks}>
                {(group) => (
                  <div style={{ "margin-top": "12px" }}>
                    <div style={{ "font-size": "11px", "font-weight": "600", color: "#8b949e", "text-transform": "uppercase", "letter-spacing": "0.5px", "margin-bottom": "6px", "padding-bottom": "4px", "border-bottom": "1px solid #21262d" }}>
                      {group.name} <span style={{ "font-weight": "400", "text-transform": "none" }}>{t("(stack)")}</span>
                    </div>
                    <div style={{ border: "1px solid #21262d", "border-radius": "6px", overflow: "hidden" }}>
                      <For each={group.containers}>
                        {(c, i) => (
                          <div
                            onClick={() => selectContainer(c)}
                            style={{
                              display: "flex",
                              "align-items": "center",
                              gap: "10px",
                              padding: "8px 12px",
                              cursor: "pointer",
                              "font-size": "13px",
                              "border-top": i() > 0 ? "1px solid #21262d" : "none",
                              background: c.name === addContainer() ? "#1a2233" : "transparent",
                            }}
                            onMouseEnter={(e) => { e.currentTarget.style.background = "#161b22"; }}
                            onMouseLeave={(e) => { e.currentTarget.style.background = c.name === addContainer() ? "#1a2233" : "transparent"; }}
                          >
                            <span style={{ width: "8px", height: "8px", "border-radius": "50%", background: c.state === "Running" ? "#3fb950" : "#484f58", "flex-shrink": "0" }} />
                            <span style={{ "font-weight": "600", color: "#e6edf3", "min-width": "0", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{c.name}</span>
                            <span class="mono" style={{ color: "#6e7681", "font-size": "12px", "min-width": "0", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap", flex: "1" }}>{c.image}</span>
                            <Show when={c.ports.length > 0}>
                              <span class="mono" style={{ color: "#6e7681", "font-size": "11px", "white-space": "nowrap", "flex-shrink": "0" }}>{formatPortsList(c.ports)}</span>
                            </Show>
                          </div>
                        )}
                      </For>
                    </div>
                  </div>
                )}
              </For>

              <Show when={pickerGrouped().standalone.length > 0}>
                <div style={{ "margin-top": "12px" }}>
                  <div style={{ "font-size": "11px", "font-weight": "600", color: "#8b949e", "text-transform": "uppercase", "letter-spacing": "0.5px", "margin-bottom": "6px", "padding-bottom": "4px", "border-bottom": "1px solid #21262d" }}>
                    {t("Standalone")}
                  </div>
                  <div style={{ border: "1px solid #21262d", "border-radius": "6px", overflow: "hidden" }}>
                    <For each={pickerGrouped().standalone}>
                      {(c, i) => (
                        <div
                          onClick={() => selectContainer(c)}
                          style={{
                            display: "flex",
                            "align-items": "center",
                            gap: "10px",
                            padding: "8px 12px",
                            cursor: "pointer",
                            "font-size": "13px",
                            "border-top": i() > 0 ? "1px solid #21262d" : "none",
                            background: c.name === addContainer() ? "#1a2233" : "transparent",
                          }}
                          onMouseEnter={(e) => { e.currentTarget.style.background = "#161b22"; }}
                          onMouseLeave={(e) => { e.currentTarget.style.background = c.name === addContainer() ? "#1a2233" : "transparent"; }}
                        >
                          <span style={{ width: "8px", height: "8px", "border-radius": "50%", background: c.state === "Running" ? "#3fb950" : "#484f58", "flex-shrink": "0" }} />
                          <span style={{ "font-weight": "600", color: "#e6edf3", "min-width": "0", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{c.name}</span>
                          <span class="mono" style={{ color: "#6e7681", "font-size": "12px", "min-width": "0", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap", flex: "1" }}>{c.image}</span>
                          <Show when={c.ports.length > 0}>
                            <span class="mono" style={{ color: "#6e7681", "font-size": "11px", "white-space": "nowrap", "flex-shrink": "0" }}>{formatPortsList(c.ports)}</span>
                          </Show>
                        </div>
                      )}
                    </For>
                  </div>
                </div>
              </Show>

              <Show when={pickerGrouped().stacks.length === 0 && pickerGrouped().standalone.length === 0}>
                <div style={{ "text-align": "center", padding: "24px", color: "#6e7681", "font-size": "13px" }}>
                  {pickerSearch() ? t("No containers match your search") : t("No containers found")}
                </div>
              </Show>
            </div>
            <div class="modal-footer">
              <button type="button" class="btn" onClick={() => setShowContainerPicker(false)}>{t("Cancel")}</button>
            </div>
          </div>
        </div>
      </Show>

      {/* Edit Route Dialog */}
      <Show when={editRoute()}>
        <div class="modal-overlay"
          onMouseDown={(e) => { (e.currentTarget as any).__mdTarget = e.target; }}
          onClick={(e) => { if ((e.currentTarget as any).__mdTarget === e.target && (e.target as HTMLElement).classList.contains("modal-overlay")) setEditRoute(null); }}>
          <div class="modal-content" style={{ "max-width": "480px" }}>
            <div class="modal-header">
              <h2>{t("Edit Route")}</h2>
              <button class="modal-close" onClick={() => setEditRoute(null)}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
              </button>
            </div>
            <div class="modal-body" style={{ overflow: "visible" }}>
              <div class="form-group">
                <label class="form-label">{t("Hostname")}</label>
                <div style={{ display: "flex", "align-items": "center", gap: "4px" }}>
                  <input class="form-input" type="text" value={editHostname()} onInput={(e) => setEditHostname(e.currentTarget.value)} style={{ flex: "1" }} />
                </div>
              </div>
              <div class="form-group">
                <label class="form-label">{t("Container")}</label>
                <input class="form-input" type="text" value={editContainer()} onInput={(e) => setEditContainer(e.currentTarget.value)} />
              </div>
              <div class="form-group">
                <label class="form-label">{t("Container Port")}</label>
                <input class="form-input" type="number" value={editPort()} onInput={(e) => setEditPort(e.currentTarget.value)} min="1" max="65535" />
              </div>
              <div class="form-group">
                <label class="form-label">{t("Path (optional)")}</label>
                <input class="form-input" type="text" value={editPath()} onInput={(e) => setEditPath(e.currentTarget.value)} placeholder="/api/*" />
                <p style={{ "font-size": "11px", color: "#6e7681", "margin-top": "4px" }}>{t("Route a specific path to this container")}</p>
              </div>
            </div>
            <div class="modal-footer">
              <button class="btn" onClick={() => setEditRoute(null)} disabled={editSaving()}>{t("Cancel")}</button>
              <button class="btn btn-primary" onClick={handleSaveEdit} disabled={editSaving() || !editHostname().trim() || !editContainer().trim() || isNaN(parseInt(editPort(), 10))}>
                {editSaving() ? t("Saving...") : t("Save Changes")}
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
