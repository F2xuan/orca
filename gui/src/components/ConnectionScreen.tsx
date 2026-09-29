import { createSignal, createEffect, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "../lib/i18n";

interface ConnectionScreenProps {
  status: string; // "connecting" | "disconnected" | "stopped"
  onRetry: () => void;
}

export default function ConnectionScreen(props: ConnectionScreenProps) {
  const [starting, setStarting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [daemonPath, setDaemonPath] = createSignal<string | null>(null);

  const fetchDaemonInfo = async () => {
    try {
      const info = (await invoke("get_daemon_info")) as {
        binary_path: string;
        port: number;
        config_path: string;
      };
      setDaemonPath(info.binary_path);
    } catch {
      // Command may not be available yet
    }
  };

  // Fetch daemon info when disconnected
  createEffect(() => {
    if (props.status !== "connecting") {
      fetchDaemonInfo();
    }
  });

  const startDaemon = async () => {
    setStarting(true);
    setError(null);
    try {
      await invoke("start_daemon");
      // Give it a moment then retry
      setTimeout(() => {
        props.onRetry();
        setStarting(false);
      }, 500);
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  return (
    <div class="connection-screen">
      <img src="/icon.png" class="connection-icon" alt="Orca" />

      <Show when={props.status === "connecting"}>
        <div class="connection-spinner" />
        <h2 class="connection-title">{t("Starting Orca...")}</h2>
        <p class="connection-subtitle">
          {t("Connecting to the Orca daemon and Docker runtime.")}
        </p>
      </Show>

      <Show when={props.status !== "connecting"}>
        <h2 class="connection-title">{t("Orca daemon is not running")}</h2>
        <p class="connection-subtitle">
          {t("The Orca daemon manages your containers and must be running for Orca Desktop to work.")}
        </p>

        <Show when={error()}>
          <div class="connection-error">
            <div style={{ "font-weight": "600", "margin-bottom": "6px" }}>
              {t("Failed to start daemon")}
            </div>
            <div style={{ "margin-bottom": "8px" }}>{error()}</div>
            <div style={{ "margin-top": "8px", "line-height": "1.8", "font-size": "12px" }}>
              <p style={{ "margin-bottom": "8px" }}>
                {t("The daemon binary")} (<code>orca-daemon</code>) {t("was not found. To fix this:")}
              </p>
              <ol style={{ margin: "0", "padding-left": "18px" }}>
                <li>
                  <a href="https://github.com/edvin/orca/actions" target="_blank" rel="noopener noreferrer" style={{ color: "#58a6ff" }}>
                    {t("Download orca-daemon")}
                  </a>{" "}
                  {t("from GitHub Actions artifacts")}
                </li>
                <li>
                  {t("Place it in your PATH or next to the Orca Desktop app")}
                </li>
                <li>
                  {t("Click Retry Connection below")}
                </li>
              </ol>
              <p style={{ "margin-top": "8px", color: "#8b949e" }}>
                {t("Or start it manually:")} <code>orca-daemon</code>
              </p>
            </div>
          </div>
        </Show>

        <div class="connection-actions">
          <button
            class="btn btn-primary"
            onClick={startDaemon}
            disabled={starting()}
          >
            {starting() ? t("Starting...") : t("Start Daemon")}
          </button>
          <button class="btn" onClick={() => props.onRetry()}>
            {t("Retry Connection")}
          </button>
        </div>

        <Show when={daemonPath()}>
          <div class="connection-hint">
            {t("Looking for daemon at: {path}", { path: daemonPath()! })}
          </div>
        </Show>
      </Show>
    </div>
  );
}
