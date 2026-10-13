/**
 * Unit tests for the shared poll store.
 *
 * Run from `gui/` with `npm run test:unit`.
 *
 * This is the one piece of the GUI that can actually be executed in this
 * environment. The store is pure logic, and both of its DOM touches
 * (`document.hidden` and the `visibilitychange` listener) are guarded by
 * `typeof document !== "undefined"`, so it runs under plain Node.
 *
 * The bundling step in the script exists because the source uses extensionless
 * imports, which Node's ESM resolver rejects; esbuild resolves them, and the
 * whole thing is bundled so the result can live outside the source tree.
 *
 * Teardown note: every test disposes its root through `t.after` as well as
 * explicitly where it needs to observe the stopped state. Without that, an
 * assertion failing before `dispose()` left the store's interval alive and the
 * run hung instead of reporting a failure.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRoot } from "solid-js";
import { usePoller, __pollerStats, __hostSwitch, type Polled } from "../src/lib/pollStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/** Let pending promises (and one macrotask) settle. */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

test("two subscribers share one request, one timer, and the fastest interval", async (t) => {
  let calls = 0;
  const gate = deferred<unknown>();
  const fetcher = () => {
    calls += 1;
    return gate.promise;
  };

  const dispose = createRoot((dispose) => {
    const slow: Polled<unknown> = usePoller("shared", fetcher, 15_000);
    usePoller("shared", fetcher, 10_000);

    assert.equal(calls, 1, "the second subscriber reuses the in-flight request");
    const stats = __pollerStats().find((s) => s.key === "shared");
    assert.ok(stats, "the key is registered");
    assert.equal(stats.subscribers, 2);
    assert.equal(stats.intervalMs, 10_000, "the fastest requested interval wins");
    assert.equal(stats.running, true, "one timer serves both subscribers");
    assert.equal(slow.data(), undefined, "no value until the fetch resolves");
    return dispose;
  });

  let finished = false;
  const finish = () => {
    if (!finished) {
      finished = true;
      dispose();
    }
  };
  t.after(finish);

  gate.resolve({ ok: true });
  await settle();
  assert.equal(
    __pollerStats().find((s) => s.key === "shared")?.running,
    true,
    "still polling while subscribed",
  );

  finish();
  const after = __pollerStats().find((s) => s.key === "shared");
  assert.equal(after?.running, false, "the timer stops when the last subscriber leaves");
  assert.equal(calls, 1);
});

test("a refresh while one is in flight joins it instead of stacking", async (t) => {
  let calls = 0;
  let release: ((value: unknown) => void) | null = null;
  const fetcher = () => {
    calls += 1;
    return new Promise<unknown>((resolve) => {
      release = resolve;
    });
  };

  let poll!: Polled<unknown>;
  const dispose = createRoot((d) => {
    poll = usePoller("flight", fetcher, 60_000);
    return d;
  });
  t.after(dispose);
  assert.equal(calls, 1, "subscribing fetches immediately");

  const first = poll.refresh();
  const second = poll.refresh();
  assert.equal(calls, 1, "both refreshes join the request already in flight");

  release?.({ done: true });
  await Promise.all([first, second]);
  assert.equal(calls, 1);

  const third = poll.refresh();
  assert.equal(calls, 2, "a refresh after it settled issues a new request");
  release?.({ done: true });
  await third;
});

test("a failed refresh keeps the last good value and sets the error", async (t) => {
  let fail = false;
  const fetcher = async () => {
    if (fail) throw new Error("daemon down");
    return { n: 1 };
  };

  let poll!: Polled<{ n: number }>;
  const dispose = createRoot((d) => {
    poll = usePoller("failing", fetcher, 60_000);
    return d;
  });
  t.after(dispose);

  await settle();
  assert.deepEqual(poll.data(), { n: 1 });
  assert.equal(poll.error(), undefined);

  fail = true;
  await poll.refresh();
  assert.deepEqual(poll.data(), { n: 1 }, "the last good value survives a failed refresh");
  assert.equal(poll.error(), "daemon down");

  fail = false;
  await poll.refresh();
  assert.equal(poll.error(), undefined, "a later success clears the error");
});

test("no polling while hidden, and a refresh on return", async (t) => {
  let calls = 0;
  const fetcher = async () => {
    calls += 1;
    return calls;
  };

  const listeners: Record<string, (() => void)[]> = {};
  let hidden = true;
  // The store reads `document.hidden` and registers its listeners here; a stub
  // drives both. This is the only test that installs a document — the store binds
  // its listeners once per module — so it is also the only place where the binding
  // itself is observable.
  (globalThis as unknown as { document: unknown }).document = {
    get hidden() {
      return hidden;
    },
    addEventListener: (type: string, fn: () => void) => {
      (listeners[type] ??= []).push(fn);
    },
  };

  const dispose = createRoot((d) => {
    usePoller("visibility", fetcher, 5);
    return d;
  });
  t.after(() => {
    dispose();
    delete (globalThis as unknown as { document?: unknown }).document;
  });

  await settle();
  assert.equal(calls, 1, "the first fetch happens even while hidden");
  assert.equal(
    listeners["orca-host-switch"]?.length ?? 0,
    1,
    "the host-switch listener is bound when the store is first subscribed",
  );

  await new Promise((resolve) => setTimeout(resolve, 40));
  assert.equal(calls, 1, "timer ticks while hidden do not fetch");

  hidden = false;
  for (const fn of listeners["visibilitychange"] ?? []) fn();
  // Deliberately not an exact count: the visibility handler fetches and the next
  // timer tick may land too. Either way the value is refreshed promptly, and
  // single-flight collapses them when they overlap.
  await settle();
  assert.ok(calls >= 2, "becoming visible refreshes without waiting for a tick");

  const before = calls;
  await new Promise((resolve) => setTimeout(resolve, 40));
  assert.ok(calls > before, "polling resumes once visible");
});

test("a host switch drops the answer from the host that was left", async (t) => {
  let calls = 0;
  const gates: ((value: unknown) => void)[] = [];
  const fetcher = () => {
    calls += 1;
    return new Promise<unknown>((resolve) => {
      gates.push(resolve);
    });
  };

  let poll!: Polled<unknown>;
  const dispose = createRoot((d) => {
    poll = usePoller("host-switch", fetcher, 60_000);
    return d;
  });
  t.after(dispose);
  assert.equal(calls, 1, "subscribing fetches immediately");

  // The switch lands while that request is still in flight.
  __hostSwitch();

  // The refresh that accompanies a switch has to issue a real request rather than
  // joining the abandoned one: joining it would leave the previous host's data on
  // screen until the next poll tick, which reads as "switching did nothing".
  const refresh = poll.refresh();
  assert.equal(calls, 2, "the post-switch refresh does not join the stale request");

  // The host that was left finally answers. Its value describes the wrong machine.
  gates[0]({ host: "left" });
  await settle();
  assert.equal(poll.data(), undefined, "the previous host's answer is discarded");

  // The replacement's slot must have survived that abandoned request settling.
  gates[1]({ host: "entered" });
  await refresh;
  await settle();
  assert.deepEqual(poll.data(), { host: "entered" });
});
