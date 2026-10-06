/**
 * Direct HTTP access to the local daemon.
 *
 * The GUI has two routes to the daemon: Tauri commands, which perform the
 * request in Rust, and this one, which calls `127.0.0.1:9477/api/v1` straight
 * from the webview (the pattern `ImagesPage` already used for the build SSE
 * stream).
 *
 * The reason this module exists rather than "just keep adding Tauri commands"
 * is the error envelope. A Tauri command returns `Result<T, String>`, so the
 * machine-readable `code` the daemon now attaches to every error cannot survive
 * that boundary — the client is back to matching on human-readable English,
 * which the daemon is explicitly free to reword. Here the code is a typed
 * field.
 *
 * New surfaces use this. Existing ones can migrate one at a time; a single
 * sweep of all ~361 `invoke` call sites is not something the current gates can
 * verify, so it is deliberately not attempted wholesale.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ErrorCode } from "./types";

/** The daemon's error envelope. `error` is prose — never branch on it. */
export interface DaemonErrorEnvelope {
  error: string;
  code: ErrorCode;
}

/**
 * An error the daemon reported, or a transport failure.
 *
 * `code === "unknown"` means the response carried no usable envelope (an
 * unmatched route, a proxy error, a body that is not JSON) or the request never
 * arrived. Callers must treat that as "something went wrong", not as a specific
 * cause.
 */
export class DaemonError extends Error {
  readonly code: ErrorCode | "unknown";
  /** HTTP status, or 0 when the request never reached the daemon. */
  readonly status: number;

  constructor(message: string, code: ErrorCode | "unknown", status: number) {
    super(message);
    this.name = "DaemonError";
    this.code = code;
    this.status = status;
  }
}

/**
 * How long to wait for the daemon before giving up.
 *
 * Matches the Tauri layer's `authed_client()` (5s to connect, 30s overall), so
 * migrating a call site does not quietly remove its timeout.
 */
const REQUEST_TIMEOUT_MS = 30_000;

interface DaemonBase {
  url: string;
  token: string;
}

/**
 * The daemon's base URL and token, resolved once and cached.
 *
 * Cached because every request needs it, and re-crossing the Tauri boundary per
 * request would add a hop to the very path that exists to remove one.
 */
let cachedBase: Promise<DaemonBase> | null = null;

export function daemonBase(): Promise<DaemonBase> {
  if (!cachedBase) {
    cachedBase = invoke<DaemonBase>("get_daemon_base").catch((e) => {
      // A failure must not be cached: the daemon may simply not have started
      // yet, and a poisoned cache would break every later request for the
      // lifetime of the window.
      cachedBase = null;
      throw new DaemonError(`cannot reach the daemon: ${String(e)}`, "unknown", 0);
    });
  }
  return cachedBase;
}

/**
 * Drop the cached base URL and token.
 *
 * For a daemon restart: the port stays the same but the token can change, and a
 * stale token would turn every later request into a 401.
 */
export function resetDaemonBase(): void {
  cachedBase = null;
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const base = await daemonBase();

  // `fetch` applies no timeout of its own, and the Tauri layer this replaces
  // used 5s to connect and 30s overall. Without an equivalent, a hung daemon
  // leaves the caller waiting forever instead of reporting a failure — and
  // "forever" is indistinguishable from "still loading".
  const controller = new AbortController();
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, REQUEST_TIMEOUT_MS);
  // A caller's own signal (cancel on unmount) still wins.
  const forwardAbort = () => controller.abort();
  signal?.addEventListener("abort", forwardAbort, { once: true });

  let response: Response;
  let text: string;
  try {
    response = await fetch(`${base.url}${path}`, {
      method,
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${base.token}`,
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: controller.signal,
    });
    // Read inside the timeout too: the Rust client's budget covered the whole
    // request, not just the headers.
    text = response.status === 204 ? "" : await response.text();
  } catch (e) {
    if (timedOut) {
      throw new DaemonError(
        `the daemon did not answer within ${REQUEST_TIMEOUT_MS / 1000}s`,
        "timeout",
        0,
      );
    }
    // A transport failure is not the daemon saying no. Status 0 keeps the two
    // distinguishable; a caller-initiated abort also lands here.
    throw new DaemonError(e instanceof Error ? e.message : String(e), "unknown", 0);
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", forwardAbort);
  }

  if (response.status === 204) return undefined as T;

  if (!response.ok) {
    let envelope: Partial<DaemonErrorEnvelope> | null = null;
    try {
      envelope = JSON.parse(text) as Partial<DaemonErrorEnvelope>;
    } catch {
      // Not JSON — fall through to the status-only error below.
    }
    // Only a string code is trusted. A malformed envelope must not be able to
    // invent a plausible-looking code that callers then branch on.
    const code: ErrorCode | "unknown" =
      typeof envelope?.code === "string" ? (envelope.code as ErrorCode) : "unknown";
    const message =
      typeof envelope?.error === "string" && envelope.error.length > 0
        ? envelope.error
        : `HTTP ${response.status}`;
    throw new DaemonError(message, code, response.status);
  }

  if (text.length === 0) return undefined as T;
  return JSON.parse(text) as T;
}

export function daemonGet<T>(path: string, signal?: AbortSignal): Promise<T> {
  return request<T>("GET", path, undefined, signal);
}

export function daemonPost<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  return request<T>("POST", path, body, signal);
}

export function daemonPut<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  return request<T>("PUT", path, body, signal);
}

export function daemonDelete<T>(path: string, signal?: AbortSignal): Promise<T> {
  return request<T>("DELETE", path, undefined, signal);
}

/**
 * A daemon error's message, for a toast or a log line.
 *
 * `String(error)` on a `DaemonError` prepends "Error: ", and the Tauri path
 * this replaces wrapped the daemon's text in its own prefix. This keeps the
 * daemon's own (already redacted) sentence as the thing the user reads.
 */
export function daemonErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Whether an error carries a specific daemon code.
 *
 * This is the whole point of the `code` field. An unknown code from a newer
 * daemon simply fails every comparison rather than throwing, so a client that
 * does not know about a code treats it as an unhandled failure — which is the
 * safe reading.
 */
export function hasCode(error: unknown, code: ErrorCode): boolean {
  return error instanceof DaemonError && error.code === code;
}

/** The addressed resource does not exist (the daemon sent `not_found`). */
export function isNotFound(error: unknown): boolean {
  return hasCode(error, "not_found");
}
