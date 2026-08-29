// The typed edge of the Tauri boundary (RL-1101, SPEC §15).
//
// These types mirror `revlocal-tauri`'s `UiEvent` and `IpcError`, which are
// deliberately separate from the daemon's own enums: the daemon is free to change
// shape for its own reasons, and this is a wire format the front end is written
// against. Keeping the mirror explicit means a mismatch is a TypeScript error
// here rather than a field that silently reads `undefined`.

/** The one channel every run event arrives on. */
export const RUN_EVENT = 'revlocal://run-event';

export type UiEvent =
  | { kind: 'stage_changed'; run_id: number; from: string; to: string }
  | { kind: 'interrupted'; run_id: number; stuck_in: string }
  | { kind: 're_enqueued'; previous_run_id: number; run_id: number; attempt: number }
  | { kind: 'given_up'; run_id: number; reason: string };

/** Errors cross the boundary as data, so the UI can branch rather than display. */
export type IpcError =
  | { error: 'daemon_unavailable'; remediation: string }
  | { error: 'no_such_repo'; repo_id: number }
  | { error: 'no_such_run'; run_id: number }
  | { error: 'store'; detail: string; remediation: string };

/** What a run event means, in one line. */
export function describe(event: UiEvent): string {
  switch (event.kind) {
    case 'stage_changed':
      return `${event.from} → ${event.to}`;
    case 'interrupted':
      return `stuck in ${event.stuck_in}`;
    case 're_enqueued':
      return `attempt ${event.attempt}, was run ${event.previous_run_id}`;
    case 'given_up':
      return event.reason;
  }
}

/** Which events deserve visual weight. */
export function severityOf(event: UiEvent): 'normal' | 'warn' | 'bad' {
  switch (event.kind) {
    case 'stage_changed':
      return 'normal';
    case 're_enqueued':
      return 'warn';
    // §18: a run that stopped being reviewed is the thing an operator most needs
    // to notice, and the thing least likely to announce itself.
    case 'interrupted':
    case 'given_up':
      return 'bad';
  }
}

type TauriGlobal = {
  event?: { listen: (name: string, handler: (msg: { payload: unknown }) => void) => Promise<() => void> };
  core?: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
};

function tauri(): TauriGlobal | undefined {
  return (globalThis as { __TAURI__?: TauriGlobal }).__TAURI__;
}

/** Whether the page is running inside the app rather than a plain browser. */
export function inTauri(): boolean {
  return tauri()?.event !== undefined;
}

/** Subscribe to run events. Returns an unsubscribe function. */
export async function onRunEvent(handler: (event: UiEvent) => void): Promise<() => void> {
  const api = tauri()?.event;
  if (!api) return () => {};
  return api.listen(RUN_EVENT, (msg) => handler(msg.payload as UiEvent));
}

/** Invoke a command. Rejects with an `IpcError`, not a string. */
export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const api = tauri()?.core;
  if (!api) throw { error: 'daemon_unavailable', remediation: 'open this in the rev-local app' } satisfies IpcError;
  return api.invoke(command, args) as Promise<T>;
}
