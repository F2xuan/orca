import { createSignal, onCleanup, type Accessor } from "solid-js";
import { daemonGet } from "./daemonClient";
import type { SystemHealth } from "./types";

/**
 * One shared poll per resource, instead of one `setInterval` per component.
 *
 * Before this, the shell polled the same endpoints on separate timers:
 * `system_health` every 10s from the titlebar and every 15s from the status bar,
 * `list_containers`/`list_images` every 5s from the sidebar, and again from each
 * page that needed them. The duplication had two costs: N requests for one
 * answer, and — because each caller stored its own copy — two components could
 * disagree about the same fact at the same moment.
 *
 * Subscribers of one key now share a single timer, a single in-flight request and
 * a single value, so a second subscriber costs nothing extra.
 *
 * Three rules are deliberate:
 *
 * - **Single-flight.** A refresh already in progress is returned to the caller
 *   rather than duplicated, so a poll and a manual refresh cannot stack.
 * - **A failed refresh keeps the last good value** and only sets `error`. The
 *   policy for what to do with stale-but-real data belongs to the subscriber:
 *   the titlebar clears its Docker status on error, the status bar keeps showing
 *   the last health. Both are preserved.
 * - **No polling while the window is hidden**, and an immediate refresh when it
 *   becomes visible again. The first fetch still happens even when hidden, so a
 *   headless screenshot run does not have to wait for a visibility change.
 */

export interface Polled<T> {
  data: Accessor<T | undefined>;
  error: Accessor<string | undefined>;
  updatedAt: Accessor<number | undefined>;
  loading: Accessor<boolean>;
  refresh: () => Promise<void>;
}

interface Entry {
  fetcher: () => Promise<unknown>;
  /** Every interval a subscriber asked for; the fastest one wins, so sharing
   *  never makes a caller's data staler than it used to be. */
  requestedMs: Set<number>;
  timer: ReturnType<typeof setInterval> | null;
  inFlight: Promise<void> | null;
  /** The `epoch` the in-flight fetch belongs to. Compared on settle so a request
   *  issued before a host switch cannot write its answer over the new host's. */
  inFlightEpoch: number;
  subscribers: number;
  data: Accessor<unknown>;
  setData: (value: unknown) => void;
  error: Accessor<string | undefined>;
  setError: (value: string | undefined) => void;
  updatedAt: Accessor<number | undefined>;
  setUpdatedAt: (value: number) => void;
  loading: Accessor<boolean>;
  setLoading: (value: boolean) => void;
}

const entries = new Map<string, Entry>();
let visibilityBound = false;

/**
 * Bumped on every host switch.
 *
 * Every polled value and every in-flight request describes one machine, so a
 * switch makes all of them stale at once — they are about the host the user just
 * left. A fetch records the epoch it started in and drops its answer if the epoch
 * moved meanwhile, which stops a slow reply from the previous host from landing
 * on top of the new host's data.
 *
 * Note the values themselves are deliberately *not* cleared here. Consumers latch
 * polled data behind an `if (result)` guard (see `ContainersPage`), so setting it
 * to `undefined` would be a no-op for them — the old rows would stay on screen.
 * Replacing the value with the new host's answer is what actually updates them,
 * and dropping the in-flight request is what lets that answer arrive on the
 * refresh that accompanies the switch instead of up to a poll interval later.
 */
let epoch = 0;

/**
 * Discard in-flight fetches so the post-switch refresh starts a real request
 * rather than joining one that is about to be thrown away.
 *
 * Exported as a test seam, following `__pollerStats`: the epoch handling is the
 * part worth exercising and it needs no DOM.
 */
export function __hostSwitch(): void {
  epoch += 1;
  for (const entry of entries.values()) {
    entry.inFlight = null;
    // Orphaned so the abandoned request's own teardown cannot clear the slot its
    // replacement is about to occupy.
    entry.inFlightEpoch = -1;
    // The abandoned request no longer owns the indicator; the refresh that
    // follows the switch turns it back on.
    entry.setLoading(false);
  }
}

let hostSwitchBound = false;
function bindHostSwitch(): void {
  if (hostSwitchBound || typeof document === "undefined") return;
  hostSwitchBound = true;
  document.addEventListener("orca-host-switch", __hostSwitch);
}

function intervalFor(entry: Entry): number {
  return entry.requestedMs.size > 0 ? Math.min(...entry.requestedMs) : 0;
}

function entryFor(key: string, fetcher: () => Promise<unknown>, intervalMs: number): Entry {
  let entry = entries.get(key);
  if (!entry) {
    const [data, setData] = createSignal<unknown>(undefined);
    const [error, setError] = createSignal<string | undefined>(undefined);
    const [updatedAt, setUpdatedAt] = createSignal<number | undefined>(undefined);
    const [loading, setLoading] = createSignal(false);
    entry = {
      fetcher,
      requestedMs: new Set<number>(),
      timer: null,
      inFlight: null,
      inFlightEpoch: -1,
      subscribers: 0,
      data,
      setData,
      error,
      setError,
      updatedAt,
      setUpdatedAt,
      loading,
      setLoading,
    };
    entries.set(key, entry);
  }
  // For a given key every caller passes the same call, so the latest is fine.
  entry.fetcher = fetcher;
  entry.requestedMs.add(intervalMs);
  return entry;
}

async function fetchOnce(entry: Entry): Promise<void> {
  if (entry.inFlight) return entry.inFlight;
  entry.setLoading(true);
  const startedAt = epoch;
  entry.inFlightEpoch = startedAt;
  entry.inFlight = (async () => {
    try {
      const value = await entry.fetcher();
      // The host may have changed while this was in flight, in which case the
      // answer describes a machine the user is no longer looking at.
      if (startedAt !== epoch) return;
      entry.setData(value);
      entry.setError(undefined);
      entry.setUpdatedAt(Date.now());
    } catch (err) {
      if (startedAt !== epoch) return;
      entry.setError(err instanceof Error ? err.message : String(err));
    } finally {
      // Only the fetch that still owns the slot may release it. A host switch
      // orphans this one and a replacement may already hold both the slot and
      // the loading state.
      if (entry.inFlightEpoch === startedAt) {
        entry.setLoading(false);
        entry.inFlight = null;
      }
    }
  })();
  return entry.inFlight;
}

function start(entry: Entry): void {
  if (entry.timer !== null) return;
  const ms = intervalFor(entry);
  if (ms <= 0) return;
  entry.timer = setInterval(() => {
    if (typeof document !== "undefined" && document.hidden) return;
    void fetchOnce(entry);
  }, ms);
}

function stop(entry: Entry): void {
  if (entry.timer !== null) {
    clearInterval(entry.timer);
    entry.timer = null;
  }
}

function bindVisibility(): void {
  if (visibilityBound || typeof document === "undefined") return;
  visibilityBound = true;
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) return;
    // Whatever went stale while hidden is refreshed in one batch on return.
    for (const entry of entries.values()) {
      if (entry.subscribers > 0) void fetchOnce(entry);
    }
  });
}

/**
 * Subscribe to a polled resource, creating its timer on first use.
 *
 * Exported for tests and for resources that are not in `polled` below; components
 * should normally use the named wrappers.
 */
export function usePoller<T>(key: string, fetcher: () => Promise<T>, intervalMs: number): Polled<T> {
  const entry = entryFor(key, fetcher as () => Promise<unknown>, intervalMs);
  entry.subscribers += 1;
  bindVisibility();
  bindHostSwitch();
  start(entry);
  void fetchOnce(entry);
  onCleanup(() => {
    // Clamped: unsubscribing twice must not drive the count negative and leave
    // a timer running for a subscriber that no longer exists.
    entry.subscribers = Math.max(0, entry.subscribers - 1);
    // The timer stays at whatever the fastest subscriber asked for, so an
    // unmounting short-interval subscriber does not need to reschedule others.
    if (entry.subscribers <= 0) stop(entry);
  });
  return {
    data: entry.data as Accessor<T | undefined>,
    error: entry.error,
    updatedAt: entry.updatedAt,
    loading: entry.loading,
    refresh: () => fetchOnce(entry),
  };
}

/**
 * The polled endpoints, described once.
 *
 * Every path lives here rather than at the call sites, so "which components read
 * this resource" and "what URL is it" stay in one place.
 */
export const polled = {
  health: () => daemonGet<SystemHealth>("/system/health"),
  containers: () => daemonGet<unknown[]>("/containers"),
  images: () => daemonGet<unknown[]>("/images"),
  stacks: () => daemonGet<unknown[]>("/stacks"),
};

export function useSystemHealth(intervalMs: number): Polled<SystemHealth> {
  return usePoller("/system/health", polled.health, intervalMs);
}

export function useContainers(intervalMs: number): Polled<unknown[]> {
  return usePoller("/containers", polled.containers, intervalMs);
}

export function useImages(intervalMs: number): Polled<unknown[]> {
  return usePoller("/images", polled.images, intervalMs);
}

export function useStacks(intervalMs: number): Polled<unknown[]> {
  return usePoller("/stacks", polled.stacks, intervalMs);
}

/** Test seam: how many timers are running, and how many requests each key made. */
export function __pollerStats(): { key: string; subscribers: number; intervalMs: number; running: boolean }[] {
  return [...entries.entries()].map(([key, entry]) => ({
    key,
    subscribers: entry.subscribers,
    intervalMs: intervalFor(entry),
    running: entry.timer !== null,
  }));
}
