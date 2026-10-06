import { createSignal, onMount, Show, type JSX } from "solid-js";
import { t } from "../lib/i18n";
import {
  daemonErrorMessage,
  daemonGet,
  daemonPost,
  daemonPut,
} from "../lib/daemonClient";
import { showToast } from "./Toast";
import { confirmDanger } from "./ConfirmDialog";
import { logError } from "../lib/activityStore";
import Dropdown from "./Dropdown";
import Spinner from "./Spinner";

/**
 * Docker Engine settings — the `daemon.json` editor.
 *
 * Mirrors Docker Desktop's *Settings → Docker Engine* pane, except the common
 * settings are form fields rather than a raw JSON blob (the raw editor is still
 * available below for anything not covered).
 *
 * Two things drive the design:
 *
 * 1. `dockerd` reads this file **only at startup**, so saving is never "done" on
 *    its own — the panel keeps a restart prompt visible until the running engine
 *    actually reports the saved values.
 * 2. The backend merges rather than replaces, so keys this form doesn't know
 *    about (Docker Desktop's `builder`, `features`, hand-added entries) survive.
 */

interface EngineConfigResponse {
  config: Record<string, unknown>;
  path: string;
  exists: boolean;
  running: {
    mirrors: string[];
    log_driver: string | null;
    storage_driver: string | null;
  } | null;
  restart_required: boolean;
  platform: string;
}

interface EngineForm {
  mirrors: string;
  insecure: string;
  dns: string;
  mtu: string;
  bip: string;
  logDriver: string;
  logMaxSize: string;
  logMaxFile: string;
  maxDownloads: string;
  maxUploads: string;
  experimental: boolean;
  liveRestore: boolean;
  userlandProxy: boolean;
}

const EMPTY_FORM: EngineForm = {
  mirrors: "",
  insecure: "",
  dns: "",
  mtu: "",
  bip: "",
  logDriver: "",
  logMaxSize: "",
  logMaxFile: "",
  maxDownloads: "",
  maxUploads: "",
  experimental: false,
  liveRestore: false,
  userlandProxy: false,
};

const LOG_DRIVERS = [
  "json-file",
  "local",
  "journald",
  "syslog",
  "fluentd",
  "gelf",
  "awslogs",
  "splunk",
  "gcplogs",
  "none",
];

/** A labelled form control with optional hint text underneath. */
function Field(props: { label: string; hint?: string; children: JSX.Element }) {
  return (
    <div style={{ display: "flex", "flex-direction": "column", gap: "4px", flex: "1", "min-width": "0" }}>
      <label class="settings-label">{props.label}</label>
      {props.children}
      <Show when={props.hint}>
        <span class="settings-description">{props.hint}</span>
      </Show>
    </div>
  );
}

/** A titled group of fields. */
function Group(props: { title: string; description?: string; children: JSX.Element }) {
  return (
    <div class="card" style={{ display: "flex", "flex-direction": "column", gap: "12px" }}>
      <div>
        <div class="settings-label">{props.title}</div>
        <Show when={props.description}>
          <div class="settings-description">{props.description}</div>
        </Show>
      </div>
      {props.children}
    </div>
  );
}

function Toggle(props: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label
      style={{
        display: "flex",
        "align-items": "flex-start",
        gap: "8px",
        cursor: "pointer",
        "user-select": "none",
      }}
    >
      <input
        type="checkbox"
        checked={props.checked}
        onChange={(e) => props.onChange(e.currentTarget.checked)}
        style={{ "margin-top": "3px" }}
      />
      <span style={{ display: "flex", "flex-direction": "column", gap: "2px" }}>
        <span class="settings-label">{props.label}</span>
        <Show when={props.hint}>
          <span class="settings-description">{props.hint}</span>
        </Show>
      </span>
    </label>
  );
}

export default function EngineSettings() {
  const [form, setForm] = createSignal<EngineForm>({ ...EMPTY_FORM });
  const [info, setInfo] = createSignal<EngineConfigResponse | null>(null);
  const [raw, setRaw] = createSignal("");
  const [showRaw, setShowRaw] = createSignal(false);
  const [loading, setLoading] = createSignal(true);
  const [saving, setSaving] = createSignal(false);
  const [restarting, setRestarting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const set = <K extends keyof EngineForm>(key: K, value: EngineForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  /** One entry per line — the same editing model Docker Desktop uses. */
  const lines = (v: string) => v.split("\n").map((s) => s.trim()).filter(Boolean);

  const refresh = async () => {
    setLoading(true);
    try {
      const r = await daemonGet<EngineConfigResponse>("/environment/engine-config");
      const c = r?.config ?? {};
      const asLines = (v: unknown) => (Array.isArray(v) ? v.join("\n") : "");
      const asStr = (v: unknown) => (v === null || v === undefined ? "" : String(v));
      const lo = (c["log-opts"] ?? {}) as Record<string, unknown>;

      setForm({
        mirrors: asLines(c["registry-mirrors"]),
        insecure: asLines(c["insecure-registries"]),
        dns: asLines(c.dns),
        mtu: asStr(c.mtu),
        bip: asStr(c.bip),
        logDriver: asStr(c["log-driver"]),
        logMaxSize: asStr(lo["max-size"]),
        logMaxFile: asStr(lo["max-file"]),
        maxDownloads: asStr(c["max-concurrent-downloads"]),
        maxUploads: asStr(c["max-concurrent-uploads"]),
        experimental: c.experimental === true,
        liveRestore: c["live-restore"] === true,
        userlandProxy: c["userland-proxy"] === true,
      });
      setRaw(JSON.stringify(c, null, 2));
      setInfo(r);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
    setLoading(false);
  };

  onMount(() => {
    void refresh();
  });

  /**
   * Catch bad numbers locally. The daemon validates too, but without this an
   * empty or mistyped field would be sent as `NaN` → `null` → "remove this key",
   * silently deleting a setting instead of reporting a problem.
   */
  const formError = (): string | null => {
    const f = form();
    const checkInt = (v: string, label: string, lo: number, hi: number) => {
      if (!v.trim()) return null;
      const n = Number(v);
      if (!Number.isInteger(n) || n < lo || n > hi) {
        return t("{field} must be a whole number between {lo} and {hi}", { field: label, lo, hi });
      }
      return null;
    };
    return (
      checkInt(f.mtu, t("MTU"), 576, 65535) ??
      checkInt(f.maxDownloads, t("Max concurrent downloads"), 1, 64) ??
      checkInt(f.maxUploads, t("Max concurrent uploads"), 1, 64)
    );
  };

  const numOrNull = (v: string) => (v.trim() === "" ? null : Number(v));

  const save = async () => {
    const bad = formError();
    if (bad) {
      showToast(bad, "error");
      return;
    }
    const f = form();
    // Every managed key is always sent; `null` / `[]` means "remove it". That is
    // what makes this form the single source of truth for the settings it owns,
    // while leaving every other key in the file untouched.
    const patch: Record<string, unknown> = {
      "registry-mirrors": lines(f.mirrors),
      "insecure-registries": lines(f.insecure),
      dns: lines(f.dns),
      mtu: numOrNull(f.mtu),
      bip: f.bip.trim() || null,
      "log-driver": f.logDriver || null,
      "log-opts": {
        "max-size": f.logMaxSize.trim() || null,
        "max-file": f.logMaxFile.trim() || null,
      },
      "max-concurrent-downloads": numOrNull(f.maxDownloads),
      "max-concurrent-uploads": numOrNull(f.maxUploads),
      experimental: f.experimental,
      "live-restore": f.liveRestore,
      "userland-proxy": f.userlandProxy,
    };

    setSaving(true);
    try {
      await daemonPut("/environment/engine-config", patch);
      showToast(t("Engine settings saved. Restart Docker to apply."), "success");
      await refresh();
    } catch (e) {
      logError(`Failed to save engine settings: ${daemonErrorMessage(e)}`, "Docker Engine");
      showToast(t("Failed to save engine settings: {error}", { error: daemonErrorMessage(e) }), "error");
    }
    setSaving(false);
  };

  const applyRaw = async () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw());
    } catch (e) {
      showToast(t("Invalid JSON: {error}", { error: daemonErrorMessage(e) }), "error");
      return;
    }
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      showToast(t("The configuration must be a JSON object."), "error");
      return;
    }

    const go = await confirmDanger(
      t("Replace the whole configuration?"),
      t("This overwrites every key in the file, including ones this form does not manage. A backup is kept next to it."),
    );
    if (!go) return;

    setSaving(true);
    try {
      await daemonPut("/environment/engine-config/raw", { config: parsed });
      showToast(t("Engine settings saved. Restart Docker to apply."), "success");
      await refresh();
    } catch (e) {
      logError(`Failed to save raw engine settings: ${daemonErrorMessage(e)}`, "Docker Engine");
      showToast(t("Failed to save engine settings: {error}", { error: daemonErrorMessage(e) }), "error");
    }
    setSaving(false);
  };

  const restart = async () => {
    const go = await confirmDanger(
      t("Restart Docker?"),
      t("Docker will restart so the engine settings take effect. Every running container stops; only containers with a restart policy come back automatically."),
    );
    if (!go) return;

    setRestarting(true);
    try {
      await daemonPost("/environment/restart-engine");
      showToast(t("Docker is restarting. This can take up to a minute."), "info");
      // Poll until the engine reports the on-disk config as live so the banner
      // clears itself instead of staying wrong until the next reload.
      for (let i = 0; i < 40; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        try {
          const r = await daemonGet<EngineConfigResponse>("/environment/engine-config");
          if (r && r.restart_required === false) break;
        } catch {
          // Engine still down — keep waiting.
        }
      }
      await refresh();
    } catch (e) {
      logError(`Failed to restart Docker: ${e}`, "Docker Engine");
      showToast(t("Failed to restart Docker: {error}", { error: daemonErrorMessage(e) }), "error");
    }
    setRestarting(false);
  };

  const isLinux = () => info()?.platform === "linux";
  const runningMirrors = () => info()?.running?.mirrors ?? [];

  return (
    <div class="settings-section">
      <h2 class="settings-section-title">{t("Docker Engine")}</h2>

      <div class="settings-description">
        {t("These settings live in the engine configuration file and apply to every container on this machine.")}
      </div>

      <div class="settings-description">
        {t("Configuration file")}:{" "}
        <span class="mono" style={{ color: "#8b949e" }}>{info()?.path || "—"}</span>
      </div>

      <Show when={error()}>
        <div class="card" style={{ "border-color": "#f85149" }}>
          <div style={{ color: "#f85149", "font-size": "13px" }}>{error()}</div>
        </div>
      </Show>

      <Show when={info()?.restart_required}>
        <div class="card" style={{ "border-color": "#d29922", background: "#d2992211" }}>
          <div style={{ color: "#d29922", "font-size": "13px", "font-weight": "600" }}>
            {t("Restart required")}
          </div>
          <div class="settings-description" style={{ "margin-top": "4px" }}>
            {t("The engine reads this file only at startup, so the settings below are not active yet.")}
          </div>
          <button
            class="btn btn-primary"
            style={{ "margin-top": "8px", "align-self": "flex-start" }}
            disabled={restarting()}
            onClick={() => void restart()}
          >
            {restarting() ? t("Restarting Docker...") : t("Restart Docker")}
          </button>
        </div>
      </Show>

      <Show when={runningMirrors().length > 0}>
        <div class="settings-description">
          {t("Active in the running engine: {list}", { list: runningMirrors().join(", ") })}
        </div>
      </Show>

      <Show when={loading()}>
        <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
          <Spinner />
          <span class="settings-description">{t("Loading...")}</span>
        </div>
      </Show>

      <Show when={!loading()}>
        <Group
          title={t("Image sources")}
          description={t("How the engine reaches image registries.")}
        >
          <Field
            label={t("Registry mirrors")}
            hint={t("One URL per line. Speeds up pulls and works around an unreachable Docker Hub.")}
          >
            <textarea
              class="form-input mono"
              rows="3"
              placeholder="https://docker.m.daocloud.io"
              value={form().mirrors}
              onInput={(e) => set("mirrors", e.currentTarget.value)}
            />
          </Field>
          <Field
            label={t("Insecure registries")}
            hint={t("One per line, as host:port without a scheme (e.g. registry.local:5000). Use this for registries with a self-signed certificate.")}
          >
            <textarea
              class="form-input mono"
              rows="2"
              placeholder="registry.local:5000"
              value={form().insecure}
              onInput={(e) => set("insecure", e.currentTarget.value)}
            />
          </Field>
        </Group>

        <Group title={t("Network")} description={t("Container networking defaults.")}>
          <Field
            label={t("DNS servers")}
            hint={t("One IP per line. Useful when the host's DNS is not reachable from inside containers.")}
          >
            <textarea
              class="form-input mono"
              rows="2"
              placeholder="8.8.8.8"
              value={form().dns}
              onInput={(e) => set("dns", e.currentTarget.value)}
            />
          </Field>
          <div class="form-row">
            <Field
              label={t("MTU")}
              hint={t("Lower this (e.g. 1400) when a VPN makes large packets fail.")}
            >
              <input
                class="form-input"
                type="number"
                placeholder="1500"
                value={form().mtu}
                onInput={(e) => set("mtu", e.currentTarget.value)}
              />
            </Field>
            <Show when={isLinux()}>
              <Field label={t("Bridge IP (bip)")} hint={t("CIDR for the default bridge, e.g. 172.30.0.1/24.")}>
                <input
                  class="form-input mono"
                  placeholder="172.30.0.1/24"
                  value={form().bip}
                  onInput={(e) => set("bip", e.currentTarget.value)}
                />
              </Field>
            </Show>
          </div>
        </Group>

        <Group
          title={t("Logging")}
          description={t("Container log handling. Without a size limit, logs can fill the disk.")}
        >
          <Field label={t("Log driver")}>
            <Dropdown
              value={form().logDriver}
              placeholder={t("Default (json-file)")}
              options={LOG_DRIVERS.map((d) => ({ value: d, label: d }))}
              onChange={(v) => set("logDriver", v)}
            />
          </Field>
          <div class="form-row">
            <Field label={t("Max size per file")} hint={t("e.g. 10m")}>
              <input
                class="form-input mono"
                placeholder="10m"
                value={form().logMaxSize}
                onInput={(e) => set("logMaxSize", e.currentTarget.value)}
              />
            </Field>
            <Field label={t("Max files")} hint={t("e.g. 3")}>
              <input
                class="form-input mono"
                placeholder="3"
                value={form().logMaxFile}
                onInput={(e) => set("logMaxFile", e.currentTarget.value)}
              />
            </Field>
          </div>
        </Group>

        <Group title={t("Concurrency")} description={t("How many layers the engine transfers at once.")}>
          <div class="form-row">
            <Field label={t("Max concurrent downloads")}>
              <input
                class="form-input"
                type="number"
                placeholder="3"
                value={form().maxDownloads}
                onInput={(e) => set("maxDownloads", e.currentTarget.value)}
              />
            </Field>
            <Field label={t("Max concurrent uploads")}>
              <input
                class="form-input"
                type="number"
                placeholder="5"
                value={form().maxUploads}
                onInput={(e) => set("maxUploads", e.currentTarget.value)}
              />
            </Field>
          </div>
        </Group>

        <Group title={t("Advanced")}>
          <Toggle
            label={t("Experimental features")}
            hint={t("Enables experimental engine features. Some tools require this.")}
            checked={form().experimental}
            onChange={(v) => set("experimental", v)}
          />
          <Show when={isLinux()}>
            <Toggle
              label={t("Live restore")}
              hint={t("Keeps containers running while the daemon restarts. Linux only.")}
              checked={form().liveRestore}
              onChange={(v) => set("liveRestore", v)}
            />
          </Show>
          <Toggle
            label={t("Userland proxy")}
            hint={t("Disable to avoid an extra process per published port; requires hairpin NAT to work.")}
            checked={form().userlandProxy}
            onChange={(v) => set("userlandProxy", v)}
          />
        </Group>

        <Show when={showRaw()}>
          <Group
            title={t("Raw configuration")}
            description={t("The complete file. Anything not covered above can be edited here.")}
          >
            <textarea
              class="form-input mono"
              rows="14"
              style={{ "font-size": "12px", "line-height": "1.5" }}
              value={raw()}
              onInput={(e) => setRaw(e.currentTarget.value)}
            />
            <div>
              <button class="btn" disabled={saving()} onClick={() => void applyRaw()}>
                {t("Apply raw JSON")}
              </button>
            </div>
          </Group>
        </Show>

        <div class="settings-divider" />

        <div style={{ display: "flex", gap: "8px", "align-items": "center", "flex-wrap": "wrap" }}>
          <button class="btn btn-primary" disabled={saving()} onClick={() => void save()}>
            {saving() ? t("Saving...") : t("Save")}
          </button>
          <button class="btn" disabled={loading() || restarting()} onClick={() => void restart()}>
            {restarting() ? t("Restarting Docker...") : t("Restart Docker")}
          </button>
          <button class="btn" disabled={loading()} onClick={() => void refresh()}>
            {t("Reload")}
          </button>
          <button class="btn" onClick={() => setShowRaw((v) => !v)}>
            {showRaw() ? t("Hide raw JSON") : t("Show raw JSON")}
          </button>
        </div>

        <div class="settings-description">
          {t("Saving writes the file but does not restart the engine. Restart Docker for the change to take effect.")}
        </div>
      </Show>
    </div>
  );
}
