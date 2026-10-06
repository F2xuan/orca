/**
 * The daemon's alert log, as the GUI sees it.
 *
 * Fetched on demand — when the notification bell opens, or the activity page
 * mounts — rather than polled. That matches how the other daemon-interrogating
 * surfaces in this round were built: a background poller costs the user
 * something (here, a request every few seconds for a list that changes rarely)
 * and buys nothing that opening the panel does not.
 */
import { createSignal } from "solid-js";
import { daemonGet, daemonPost, isNotFound } from "./daemonClient";
import type { Alert, AlertData, AlertsResponse, SeverityLevel } from "./types";

const [alerts, setAlerts] = createSignal<Alert[]>([]);
const [openCount, setOpenCount] = createSignal(0);
const [error, setError] = createSignal<string | null>(null);

/**
 * The alert log, newest first — including resolved ones.
 *
 * Deliberately not filtered to open alerts. The daemon's only alert source
 * today is a build failure, which is recorded already terminal, so an
 * open-only list would always be empty and this panel would look broken while
 * working exactly as designed. Open rows are marked and can be dismissed.
 */
export function getDaemonAlerts(): Alert[] {
  return alerts();
}

/** How many alerts are open, whatever is currently listed. */
export function getDaemonOpenCount(): number {
  return openCount();
}

/**
 * Record the open count pushed over the event stream.
 *
 * A push is how the badge lights up while the panel is closed — the list is
 * only fetched when it opens. The value is validated rather than trusted: it
 * arrives as `unknown` off the wire, and a `NaN` in the badge renders as
 * "NaN" rather than as nothing.
 */
export function noteOpenCount(count: unknown): void {
  if (typeof count === "number" && Number.isInteger(count) && count >= 0) {
    setOpenCount(count);
  }
}

/** The last fetch failure, or `null`. */
export function getDaemonAlertsError(): string | null {
  return error();
}

/**
 * Re-read the daemon's open alerts.
 *
 * Failures are recorded rather than thrown: this is called from a UI event
 * handler, and a rejected promise there would surface as an unhandled rejection
 * instead of a message the user can see.
 */
export async function refreshAlerts(): Promise<void> {
  try {
    const data = await daemonGet<AlertsResponse>("/alerts");
    setAlerts(data.alerts);
    setOpenCount(data.open_count);
    setError(null);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

/** Dismiss an open alert, then re-read. */
export async function resolveAlert(id: string): Promise<void> {
  try {
    await daemonPost(`/alerts/${encodeURIComponent(id)}/resolve`);
  } catch (e) {
    // "No open alert with id ..." is not a failure worth showing: someone else
    // dismissed it, or two clicks raced. Anything else is surfaced.
    if (!isNotFound(e)) {
      setError(e instanceof Error ? e.message : String(e));
      return;
    }
  }
  // Re-read rather than dropping the row locally: the daemon's log is the
  // source of truth, and resolving may have closed more than this one row.
  await refreshAlerts();
}

/** A one-line description of an alert, for a list row. */
export function alertSummary(alert: Alert): string {
  return alertText(alert.data);
}

/**
 * Mirrors `AlertData::summary()` in `crates/orca-core/src/alert.rs`.
 *
 * Duplicated deliberately for now: it is the part of alert rendering that
 * cannot be shared without the type/codegen step noted in `types.ts`. If it
 * drifts, the list shows a stale phrasing — the alert's state is unaffected.
 */
function alertText(data: AlertData): string {
  switch (data.kind) {
    case "container_exited":
      return `container ${data.name} exited with code ${data.exit_code}`;
    case "container_unhealthy":
      return `container ${data.name} is unhealthy`;
    case "image_update_available":
      return `a newer ${data.image} is available`;
    case "build_failed":
      return data.error
        ? `build ${data.id} failed: ${data.error}`
        : `build ${data.id} failed`;
    case "stack_state_change":
      return `stack ${data.name} is now ${data.state}`;
    case "host_disk_high":
      return `disk ${data.mount} is ${data.used_pct}% full`;
    case "custom":
      return data.message;
    default:
      // A kind this build predates — a newer daemon.
      return "unrecognised alert";
  }
}

/**
 * Map a daemon severity onto the severity classes the activity list styles.
 *
 * `critical` shares `error`'s styling on purpose: both mean "there is a problem
 * now", and a fourth colour would have to be justified to the eye.
 */
export function alertLevelClass(level: SeverityLevel): string {
  switch (level) {
    case "critical":
    case "error":
      return "error";
    case "warning":
      return "warning";
    case "ok":
      return "success";
    default:
      return "info";
  }
}
