import { createSignal, createEffect, untrack, onMount, onCleanup, Show, For } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { useSystemHealth } from "../lib/pollStore";
import { showToast } from "./Toast";
import { t } from "../lib/i18n";
import { getAlertEvents, getUnreadCount, markAllRead, clearEvents } from "../lib/activityStore";
import {
  getDaemonAlerts,
  getDaemonOpenCount,
  getDaemonAlertsError,
  refreshAlerts,
  resolveAlert,
  alertSummary,
  alertLevelClass,
} from "../lib/daemonAlerts";
import { openAiWindow } from "./AiAssistant";
import type { RemoteHost, ActiveHost } from "../lib/types";

interface TitlebarProps {
  daemonStatus: string;
  onNavigate?: (page: string) => void;
}

function relativeTime(date: Date): string {
  const now = Date.now();
  const diff = now - date.getTime();
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return t("just now");
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("{minutes}m ago", { minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("{hours}h ago", { hours });
  const days = Math.floor(hours / 24);
  return t("{days}d ago", { days });
}

export default function Titlebar(props: TitlebarProps) {
  const [maximized, setMaximized] = createSignal(false);
  const [dockerConnected, setDockerConnected] = createSignal<boolean | null>(null);
  const [warningCount, setWarningCount] = createSignal(0);
  const [runtimeInfo, setRuntimeInfo] = createSignal<string | null>(null);
  const [bellOpen, setBellOpen] = createSignal(false);

  /**
   * Unread activity plus *open* daemon alerts.
   *
   * Both mean "something wants your attention": an unread activity entry is a
   * notification you have not seen, an open daemon alert is a problem that is
   * still true. Counting the whole alert list here — as this did — counted
   * dismissed rows too, so the badge never went down when one was dismissed,
   * and it never went up for an alert the user had not fetched the list to see.
   */
  const badgeCount = () => getUnreadCount() + getDaemonOpenCount();
  const [hostMenuOpen, setHostMenuOpen] = createSignal(false);
  const [remoteHosts, setRemoteHosts] = createSignal<RemoteHost[]>([]);
  const [activeHost, setActiveHost] = createSignal<ActiveHost>({ id: null, name: "Local", url: "", is_remote: false });

  const loadHosts = async () => {
    try {
      const hosts = (await invoke("list_remote_hosts")) as RemoteHost[];
      setRemoteHosts(hosts);
      const active = (await invoke("get_active_host")) as ActiveHost;
      setActiveHost(active);
    } catch {}
  };

  const switchToHost = async (id: string | null) => {
    try {
      await invoke("switch_host", { id });
      const active = (await invoke("get_active_host")) as ActiveHost;
      setActiveHost(active);
      setHostMenuOpen(false);
      showToast(t("Switched to {name}", { name: active.name }), "success");
      // Trigger a full refresh + chart reset across the app
      document.dispatchEvent(new CustomEvent("orca-host-switch"));
      document.dispatchEvent(new CustomEvent("orca-refresh"));
    } catch (e) {
      showToast(t("Failed to switch host: {error}", { error: String(e) }), "error");
    }
  };

  // Shared with the status bar: one timer and one request for `system_health`
  // instead of one per component (they used to poll it at 10s and at 15s).
  const healthPoll = useSystemHealth(10_000);

  createEffect(() => {
    if (healthPoll.error()) {
      // Daemon not reachable — docker status unknown. Checking the error first
      // matters: on a failed refresh the poll keeps the last good value, and the
      // titlebar is the subscriber that must *clear* rather than show it stale.
      setDockerConnected(null);
      setWarningCount(0);
      return;
    }
    const health = healthPoll.data();
    if (!health) return;
    // `untrack`: this effect writes `dockerConnected`, so reading it as a
    // dependency would make the effect re-run on its own write. The value is
    // only needed to compare against the previous poll.
    const prevConnected = untrack(dockerConnected);
    setDockerConnected(health.docker_connected);
    setWarningCount(health.warnings.length);
    setRuntimeInfo(health.docker_version ? `Docker ${health.docker_version}` : null);

    // Detect reconnection
    if (prevConnected === false && health.docker_connected) {
      showToast(t("Docker connection restored"), "success");
    }
  });

  onMount(() => {
    loadHosts();

    // Reload hosts when settings change
    const onRefresh = () => loadHosts();
    document.addEventListener("orca-refresh", onRefresh);
    onCleanup(() => document.removeEventListener("orca-refresh", onRefresh));

    // Close dropdowns when clicking outside
    const handleClickOutside = (e: MouseEvent) => {
      if (bellOpen()) {
        const bell = document.querySelector(".notification-bell");
        if (bell && !bell.contains(e.target as Node)) {
          setBellOpen(false);
        }
      }
      if (hostMenuOpen()) {
        const hostSel = document.querySelector(".host-selector");
        if (hostSel && !hostSel.contains(e.target as Node)) {
          setHostMenuOpen(false);
        }
      }
    };
    document.addEventListener("mousedown", handleClickOutside);

    onCleanup(() => {
      document.removeEventListener("mousedown", handleClickOutside);
    });
  });

  const minimize = async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().minimize();
    } catch (e) {
      console.error("Minimize failed:", e);
    }
  };

  const toggleMaximize = async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const win = getCurrentWindow();
      if (await win.isMaximized()) {
        await win.unmaximize();
        setMaximized(false);
      } else {
        await win.maximize();
        setMaximized(true);
      }
    } catch (e) {
      console.error("Maximize toggle failed:", e);
    }
  };

  const close = async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().hide();
    } catch (e) {
      console.error("Close/hide failed:", e);
    }
  };

  const statusColor = () => {
    // Docker connection status takes priority when daemon is running
    if (props.daemonStatus === "running") {
      if (dockerConnected() === false) return "#f85149";
      return "#3fb950";
    }
    switch (props.daemonStatus) {
      case "stopped": return "#f85149";
      default: return "#848d97";
    }
  };

  const statusText = () => {
    if (props.daemonStatus === "running") {
      if (dockerConnected() === false) return t("disconnected");
      return t("running");
    }
    return t(props.daemonStatus);
  };

  return (
    <div class="titlebar" data-tauri-drag-region>
      <div class="titlebar-left" data-tauri-drag-region>
        <img src="/icon.png" class="titlebar-icon" alt="" />
        <span class="titlebar-title" data-tauri-drag-region>Orca Desktop</span>
        <div class="titlebar-status" onClick={() => props.onNavigate?.("environment")} title={t("System Health")}>
          <span class="titlebar-status-dot" style={{ background: statusColor() }} />
          <span class="titlebar-status-text">{statusText()}</span>
          <Show when={runtimeInfo() && dockerConnected()}>
            <span class="titlebar-runtime">{runtimeInfo()}</span>
          </Show>
          <Show when={dockerConnected() === false && props.daemonStatus === "running"}>
            <span class="titlebar-reconnecting" title={t("Click to check System Health")}>{t("No runtime")}</span>
          </Show>
          <Show when={warningCount() > 0}>
            <span class="titlebar-warning-badge" title={t("{count} warnings", { count: warningCount() })}>
              {warningCount()}
            </span>
          </Show>
        </div>
          <div class="host-selector" data-tauri-drag-region-exclude>
            <button
              class={`host-selector-btn ${activeHost().is_remote ? "host-remote" : ""}`}
              onClick={() => { setHostMenuOpen(!hostMenuOpen()); loadHosts(); }}
              title={activeHost().is_remote ? t("Connected to {url}", { url: activeHost().url }) : t("Local daemon")}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <rect x="2" y="2" width="20" height="8" rx="2" ry="2"/><rect x="2" y="14" width="20" height="8" rx="2" ry="2"/><line x1="6" y1="6" x2="6.01" y2="6"/><line x1="6" y1="18" x2="6.01" y2="18"/>
              </svg>
              <span>{activeHost().name}</span>
              <svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M6 9l6 6 6-6"/></svg>
            </button>
            <Show when={hostMenuOpen()}>
              <div class="host-selector-dropdown">
                <div
                  class={`host-selector-item ${!activeHost().is_remote ? "active" : ""}`}
                  onClick={() => switchToHost(null)}
                >
                  <span class="host-dot" style={{ background: "#3fb950" }} />
                  <span>{t("Local")}</span>
                </div>
                <For each={remoteHosts()}>
                  {(host) => (
                    <div
                      class={`host-selector-item ${activeHost().id === host.id ? "active" : ""}`}
                      onClick={() => switchToHost(host.id)}
                    >
                      <span class="host-dot" style={{ background: activeHost().id === host.id ? "#58a6ff" : "#8b949e" }} />
                      <span>{host.name}</span>
                    </div>
                  )}
                </For>
                <div class="host-selector-divider" />
                <div
                  class="host-selector-item"
                  onClick={() => { setHostMenuOpen(false); props.onNavigate?.("settings:remote-hosts"); }}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
                  <span>{t("Manage Hosts...")}</span>
                </div>
              </div>
            </Show>
          </div>
      </div>
      <button
        class="titlebar-search"
        onClick={() => {
          // Dispatch a plain custom event — synthetic KeyboardEvents are
          // unreliable on macOS WebKit (Cmd vs Ctrl), and some listeners
          // ignore untrusted events. The App-level keydown handler listens
          // for `orca-open-command-palette` as well.
          document.dispatchEvent(new CustomEvent("orca-open-command-palette"));
        }}
        data-tauri-drag-region-exclude
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style={{ opacity: 0.5 }}><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
        <span>{t("Search...")}</span>
        <span class="titlebar-search-shortcut">⌘K</span>
      </button>
      <div class="titlebar-controls">
        <button
          class="notification-btn"
          title={t("AI Assistant")}
          onClick={openAiWindow}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="m12 3-1.9 5.8a2 2 0 0 1-1.287 1.288L3 12l5.8 1.9a2 2 0 0 1 1.288 1.287L12 21l1.9-5.8a2 2 0 0 1 1.287-1.288L21 12l-5.8-1.9a2 2 0 0 1-1.288-1.287Z" />
            <path d="M4 3v2" />
            <path d="M3 4h2" />
            <path d="M20 19v2" />
            <path d="M19 20h2" />
          </svg>
        </button>
        <div class="notification-bell">
          <button
            class="notification-btn"
            title={t("Notifications")}
            onClick={() => {
              const opening = !bellOpen();
              setBellOpen(opening);
              if (opening) {
                markAllRead();
                // Fetched when the panel opens rather than polled: it changes
                // rarely, and a poller would cost a request every few seconds
                // to keep a closed panel up to date.
                void refreshAlerts();
              }
            }}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/></svg>
            <Show when={badgeCount() > 0}>
              <span class="notification-badge">
                {badgeCount() > 99 ? "99+" : badgeCount()}
              </span>
            </Show>
          </button>
          <Show when={bellOpen()}>
            <div class="notification-dropdown">
              <div class="notification-dropdown-header">
                <span>{t("Notifications")}</span>
                <span style={{ color: "#8b949e", "font-weight": "400" }}>
                  {t("{count} alerts", {
                    count: getAlertEvents().length + getDaemonAlerts().length,
                  })}
                </span>
              </div>
              <div class="notification-dropdown-body">
                <Show when={getDaemonAlerts().length > 0}>
                  <div
                    style={{
                      padding: "6px 12px",
                      "font-size": "11px",
                      "text-transform": "uppercase",
                      "letter-spacing": "0.04em",
                      color: "#8b949e",
                    }}
                  >
                    {t("Daemon alerts")}
                  </div>
                  <For each={getDaemonAlerts().slice(0, 10)}>
                    {(alert) => (
                      <div class="activity-event">
                        <div
                          class={`activity-event-icon activity-icon-${alertLevelClass(alert.level)}`}
                        >
                          {alert.level === "error" || alert.level === "critical" ? "\u2717" : "\u26A0"}
                        </div>
                        <div class="activity-event-body">
                          <div class="activity-event-title">{alertSummary(alert)}</div>
                          <div class="activity-event-time">
                            {relativeTime(new Date(alert.ts))}
                          </div>
                        </div>
                        <Show when={!alert.resolved}>
                          <button
                            class="notification-view-all"
                            title={t("Dismiss")}
                            onClick={() => void resolveAlert(alert.id)}
                          >
                            {t("Dismiss")}
                          </button>
                        </Show>
                      </div>
                    )}
                  </For>
                </Show>
                <Show when={getDaemonAlertsError()}>
                  <div style={{ padding: "8px 12px", color: "#8b949e", "font-size": "12px" }}>
                    {getDaemonAlertsError()}
                  </div>
                </Show>
                <Show
                  when={getAlertEvents().length > 0}
                  fallback={
                    <Show when={getDaemonAlerts().length === 0}>
                      <div style={{ padding: "20px", "text-align": "center", color: "#8b949e", "font-size": "12px" }}>
                        {t("No errors or warnings")}
                      </div>
                    </Show>
                  }
                >
                  <For each={getAlertEvents().slice(0, 10)}>
                    {(event) => (
                      <div
                        class="activity-event activity-event-clickable"
                        onClick={() => { setBellOpen(false); props.onNavigate?.("activity"); }}
                        title={t("Click to view in Activity")}
                      >
                        <div class={`activity-event-icon activity-icon-${event.severity}`}>
                          {event.severity === "error" ? "\u2717" : "\u26A0"}
                        </div>
                        <div class="activity-event-body">
                          <div class="activity-event-title">{event.title}</div>
                          <Show when={event.detail}>
                            <div class="activity-event-detail">{event.detail}</div>
                          </Show>
                          <div class="activity-event-time">{relativeTime(event.timestamp)}</div>
                        </div>
                      </div>
                    )}
                  </For>
                </Show>
              </div>
              <div class="notification-dropdown-footer">
                <button
                  class="notification-view-all"
                  onClick={() => {
                    setBellOpen(false);
                    if (props.onNavigate) props.onNavigate("activity");
                  }}
                >
                  {t("View All Activity")}
                </button>
                <Show when={getAlertEvents().length > 0}>
                  <button
                    class="notification-view-all notification-clear-btn"
                    onClick={() => {
                      clearEvents();
                      setBellOpen(false);
                    }}
                  >
                    {t("Clear")}
                  </button>
                </Show>
              </div>
            </div>
          </Show>
        </div>
        <button class="titlebar-btn" onClick={minimize} title={t("Minimize")}>
          <svg width="10" height="1" viewBox="0 0 10 1"><rect width="10" height="1" fill="currentColor"/></svg>
        </button>
        <button class="titlebar-btn" onClick={toggleMaximize} title={maximized() ? t("Restore") : t("Maximize")}>
          <Show when={maximized()} fallback={
            <svg width="10" height="10" viewBox="0 0 10 10"><rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" stroke-width="1"/></svg>
          }>
            <svg width="10" height="10" viewBox="0 0 10 10"><path d="M2 0h8v8h-2v2H0V2h2V0zm1 1v1h5v5h1V1H3zM1 3v6h6V3H1z" fill="currentColor"/></svg>
          </Show>
        </button>
        <button class="titlebar-btn titlebar-btn-close" onClick={close} title={t("Close")}>
          <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1 0l4 4L9 0l1 1-4 4 4 4-1 1-4-4-4 4L0 9l4-4L0 1z" fill="currentColor"/></svg>
        </button>
      </div>
    </div>
  );
}
