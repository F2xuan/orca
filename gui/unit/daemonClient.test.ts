/**
 * Unit tests for the webview's direct-HTTP route to the daemon.
 *
 * Run from `gui/` with `npm run test:unit`.
 *
 * The thing under test is the cached base URL. `daemonBase()` asks the Rust side
 * once and keeps the answer, while Tauri commands re-read the active host on every
 * call — so a host switch that does not invalidate this cache leaves the two
 * routes pointing at different machines. That is not a subtle failure: the UI keeps
 * rendering the daemon it started on while Rust-side calls follow the switch.
 *
 * `invoke` is one indirection deep (`window.__TAURI_INTERNALS__.invoke`), so the
 * whole module runs under Node with that object stubbed.
 */
import { test } from "node:test";
import assert from "node:assert/strict";

interface DaemonBase {
  url: string;
  token: string;
}

test("a host switch drops the cached daemon base", async (t) => {
  const listeners: Record<string, (() => void)[]> = {};
  const commands: string[] = [];
  let base: DaemonBase = { url: "http://127.0.0.1:9477/api/v1", token: "local-token" };

  (globalThis as unknown as { document: unknown }).document = {
    addEventListener: (type: string, fn: () => void) => {
      (listeners[type] ??= []).push(fn);
    },
  };
  (globalThis as unknown as { window: unknown }).window = {
    __TAURI_INTERNALS__: {
      invoke: async (cmd: string) => {
        commands.push(cmd);
        if (cmd !== "get_daemon_base") throw new Error(`unexpected command: ${cmd}`);
        return base;
      },
    },
  };
  t.after(() => {
    delete (globalThis as unknown as { document?: unknown }).document;
    delete (globalThis as unknown as { window?: unknown }).window;
  });

  // Imported only now: the module binds its host-switch listener at load time, so
  // the stubs having to exist first is part of what this test pins down.
  const client = await import("../src/lib/daemonClient");

  assert.equal(
    listeners["orca-host-switch"]?.length ?? 0,
    1,
    "the module binds exactly one host-switch listener at load",
  );

  // Two readers share one resolution rather than crossing the Tauri boundary each
  // time — which is the whole reason the cache exists.
  assert.deepEqual(await client.daemonBase(), { url: base.url, token: base.token });
  await client.daemonBase();
  assert.deepEqual(commands, ["get_daemon_base"], "the base is resolved once and cached");

  // The user switches to a remote host: the Rust override now points elsewhere.
  base = { url: "http://10.0.0.5:9477/api/v1", token: "remote-token" };
  for (const fn of listeners["orca-host-switch"] ?? []) fn();

  assert.deepEqual(
    await client.daemonBase(),
    { url: "http://10.0.0.5:9477/api/v1", token: "remote-token" },
    "the next read resolves the newly active host, not the one that was cached",
  );
  assert.deepEqual(commands, ["get_daemon_base", "get_daemon_base"]);
});

test("a failed resolution is not cached", async (t) => {
  let attempts = 0;
  (globalThis as unknown as { window: unknown }).window = {
    __TAURI_INTERNALS__: {
      invoke: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error("daemon not up yet");
        return { url: "http://127.0.0.1:9477/api/v1", token: "t" };
      },
    },
  };
  t.after(() => {
    delete (globalThis as unknown as { window?: unknown }).window;
  });

  // The import resolves to the same module instance the test above used, cache and
  // all, so clear what it left behind rather than pretending it is fresh.
  const client = await import("../src/lib/daemonClient");
  client.resetDaemonBase();

  await assert.rejects(() => client.daemonBase());
  assert.deepEqual(
    await client.daemonBase(),
    { url: "http://127.0.0.1:9477/api/v1", token: "t" },
    "a poisoned cache must not outlive the failure that caused it",
  );
});
