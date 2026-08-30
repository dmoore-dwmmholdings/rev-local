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

// --- dashboard (RL-1105, SPEC §15 screen 1) ---------------------------------

/** One repository's polling health, as `revlocal repo show` reports it. */
export type HealthReport = {
  repo: string;
  health: string;
  poll_interval_secs: number;
  next_poll_in_secs: number;
  consecutive_failures: number;
  last_error: string | null;
  notes: string[];
};

export type RepoView = {
  id: number;
  repo: string;
  kind: string;
  engine: string;
  autonomy: string;
  enabled: boolean;
  local_path?: string;
  health: HealthReport;
};

export type LastRun = {
  run_id: number;
  status: string;
  verdict?: string;
  finished_at?: string;
};

/**
 * Today's spend beside today's ceiling.
 *
 * Both numbers, never a percentage: a bar that knows only "62%" cannot say 62%
 * *of what*, and somebody deciding whether to widen a budget needs both.
 */
export type BudgetBar = {
  runs: number;
  runs_limit: number;
  tokens: number;
  tokens_limit: number;
  /** When false the token figure is a lower bound, not a total (§18, RL-409). */
  tokens_known: boolean;
};

export type RepoCard = {
  repo: RepoView;
  last_run?: LastRun;
  queue_depth: number;
  budget: BudgetBar;
};

export type Dashboard = {
  repos: RepoCard[];
  mode: string;
  paused: boolean;
};

/** §12.2's four autonomy levels, widest last. */
export const MODES = ['off', 'dry_run', 'auto_low_ask_high', 'auto'] as const;
export type Mode = (typeof MODES)[number];

/** How each mode reads to somebody choosing one. */
export const MODE_LABELS: Record<string, string> = {
  off: 'Off — nothing runs',
  dry_run: 'Dry run — review, publish nothing',
  auto_low_ask_high: 'Auto (low risk) — ask before anything high-risk',
  auto: 'Auto — publish without asking',
};

export function fetchDashboard(): Promise<Dashboard> {
  return invoke<Dashboard>('dashboard');
}

export function setMode(mode: Mode): Promise<void> {
  return invoke<void>('set_mode', { mode });
}
