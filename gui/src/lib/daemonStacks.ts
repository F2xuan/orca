import { daemonPost } from "./daemonClient";

/**
 * Stack actions, as a table instead of a string handed to `invoke`.
 *
 * These used to be `invoke(action, { name })` where `action` arrived as a string
 * from the call site. That shape is why a grep for `invoke("compose_down")` found
 * only one of its two callers — the other went through a dispatcher — and why
 * `tsc` could not see the coupling either. Naming every action in one table makes
 * each of them explicit, greppable and type-checked, and it is what let the last
 * six commands of the stack surface come out of the Tauri layer.
 */
export type StackAction =
  | "start_stack"
  | "stop_stack"
  | "restart_stack"
  | "compose_up"
  | "compose_down"
  | "compose_pull";

/** Every action is a POST with no body, matching the commands they replace. */
const STACK_ACTION_PATHS: Record<StackAction, (name: string) => string> = {
  start_stack: (name) => `/stacks/${encodeURIComponent(name)}/start`,
  stop_stack: (name) => `/stacks/${encodeURIComponent(name)}/stop`,
  restart_stack: (name) => `/stacks/${encodeURIComponent(name)}/restart`,
  compose_up: (name) => `/stacks/${encodeURIComponent(name)}/up`,
  compose_down: (name) => `/stacks/${encodeURIComponent(name)}/down`,
  compose_pull: (name) => `/stacks/${encodeURIComponent(name)}/pull`,
};

/**
 * Run a stack action.
 *
 * Start/stop/restart answer `204 No Content` when every service was acted on, or
 * `207 Multi-Status` with `{"partial_errors": [...]}` when some were not. The
 * command this replaces only looked at `status.is_success()`, so 207 counted as
 * success and a partial failure was reported to the user as "succeeded". Here the
 * partial case is raised as an error instead: the callers already have a catch
 * that logs and toasts, so the failure becomes visible rather than silent.
 *
 * The compose actions (up/down/pull) answer 200 with the compose output, which
 * includes its own `success` field; that is left alone, and the callers keep
 * reading it as they did.
 */
export async function runStackAction(action: StackAction, name: string): Promise<unknown> {
  const result = await daemonPost<Record<string, unknown>>(STACK_ACTION_PATHS[action](name));
  const partial = result?.partial_errors;
  if (Array.isArray(partial) && partial.length > 0) {
    throw new Error(`${action} did not complete for every service: ${partial.join("; ")}`);
  }
  return result;
}
