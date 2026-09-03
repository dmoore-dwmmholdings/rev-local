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

// --- the autopilot (RL-1501, RL-1506) ---------------------------------------

/** The channel the background loop reports itself on. */
export const AUTOPILOT_EVENT = 'revlocal://autopilot';

/**
 * What the background loop is doing.
 *
 * `last_line` is a whole sentence, written by the daemon, and is meant to be
 * rendered as-is. Reassembling it here from the counts would be a second opinion
 * about what happened, and the two would disagree the first time either changed.
 */
export type Autopilot = {
  /** Whether the loop is switched on. */
  enabled: boolean;
  /** Whether a pass is executing right now. */
  ticking: boolean;
  /** Seconds between passes. */
  interval_secs: number;
  /** When the last pass started, RFC 3339. */
  last_tick_at: string | null;
  /** What the last pass did, in one sentence. */
  last_line: string;
  /** Why the last pass could not run at all. */
  last_error: string | null;
  /** Everything the last pass did not do, and why. */
  notes: string[];
};

export function fetchAutopilot(): Promise<Autopilot> {
  return invoke<Autopilot>('autopilot_status');
}

export function setAutopilot(enabled: boolean): Promise<Autopilot> {
  return invoke<Autopilot>('set_autopilot', { enabled });
}

export function autopilotNow(): Promise<void> {
  return invoke<void>('autopilot_now');
}

/** Subscribe to autopilot status. Returns an unsubscribe function. */
export async function onAutopilot(handler: (state: Autopilot) => void): Promise<() => void> {
  const api = tauri()?.event;
  if (!api) return () => {};
  return api.listen(AUTOPILOT_EVENT, (msg) => handler(msg.payload as Autopilot));
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
  summary?: string;
  error?: string;
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
  // Filing an issue is high risk by §12.3's own list, so this mode asks about
  // every issue rev-local would ever create. Saying only "low risk runs" led to
  // an inbox of 3 and a tracker of 0 for a week — the label now says so.
  auto_low_ask_high: 'Auto (low risk) — asks before filing any issue',
  auto: 'Auto — files issues without asking',
};

export function fetchDashboard(): Promise<Dashboard> {
  return invoke<Dashboard>('dashboard');
}

/** Release the kill switch (§12.1: stopping has to be reversible). */
export function resume(): Promise<void> {
  return invoke<void>('resume');
}

export function setMode(mode: Mode): Promise<void> {
  return invoke<void>('set_mode', { mode });
}

// --- the queue (SPEC §15 screen 1) -------------------------------------------

/** One run as the queue panel shows it. `status` is the run's, not its trigger's. */
export type QueueItem = {
  run_id: number;
  repo: string;
  repo_id: number;
  change: string;
  title?: string;
  status: string;
  trigger: string;
  created_at: string;
  started_at?: string;
  finished_at?: string;
  verdict?: string;
  error?: string;
};

export type QueueStatus = {
  /** A review is executing — read from the runs, not from a local flag. */
  running: boolean;
  /** This window is working through the queue. */
  draining: boolean;
  paused: boolean;
  queued_total: number;
  /**
   * How many of those cannot run because their repository's checkout is gone.
   *
   * Optional because a status serialised before RL-1554 has no such field.
   */
  queued_blocked?: number;
  active: QueueItem[];
  /** The head of the queue, in the order it will run. */
  waiting: QueueItem[];
  /** Waiting runs past the ones listed. */
  waiting_hidden: number;
  recent: QueueItem[];
};

export function fetchQueueStatus(): Promise<QueueStatus> {
  return invoke<QueueStatus>('queue_status');
}

export function startQueuedRuns(): Promise<void> {
  return invoke<void>('start_queued_runs');
}

/**
 * A timestamp as an operator reads it: how long ago, then the clock time.
 *
 * The raw RFC 3339 string is what the backend stores and the wrong thing to put
 * in a table — "2026-09-01T04:12:44.918273Z" takes a column and a second of
 * arithmetic to answer "is this recent?", which is the only question being asked
 * of it.
 */
export function ago(iso: string | undefined | null, now = Date.now()): string {
  if (!iso) return '—';
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return iso;
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 45) return 'just now';
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

/** The clock time, for the title attribute where the exact moment matters. */
export function exactly(iso: string | undefined | null): string {
  if (!iso) return 'not recorded';
  const at = Date.parse(iso);
  return Number.isNaN(at) ? iso : new Date(at).toLocaleString();
}

/**
 * How a run's status should read, and how much weight it deserves.
 *
 * `done` is not the same as `interrupted`, and a table that renders both in the
 * same grey is one somebody has to read every row of to find the failure.
 */
export const RUN_TONE: Record<string, 'ok' | 'warn' | 'bad' | 'active' | 'idle'> = {
  queued: 'idle',
  preparing: 'active',
  reviewing: 'active',
  synthesizing: 'active',
  publishing: 'active',
  done: 'ok',
  skipped: 'idle',
  interrupted: 'bad',
  failed: 'bad',
  cancelled: 'warn',
};

export function toneOf(status: string): 'ok' | 'warn' | 'bad' | 'active' | 'idle' {
  return RUN_TONE[status] ?? 'idle';
}

// --- run detail (RL-1107, SPEC §15 screen 3) --------------------------------

export type AnchoredFinding = {
  id: number;
  severity: string;
  category: string;
  title: string;
  file?: string;
  line_start?: number;
  line_end?: number;
  /** False when this finding cannot be placed against the diff (§18). */
  anchorable: boolean;
  /**
   * What the finding says, in the engine's own words.
   *
   * Optional because a view serialised before RL-1552 has no such field, and a
   * screen that throws on last week's JSON is worse than one that shows less.
   */
  body?: string;
  /** How to make it happen, when the engine gave one. */
  failure_scenario?: string;
  /** What to change, when the engine proposed something. */
  suggested_fix?: string;
};

export type Stages = {
  started_at?: string;
  finished_at?: string;
  elapsed_secs?: number;
  /** Why there is no per-stage breakdown. §15 asks for one; nothing records it. */
  per_stage_unavailable: string;
};

export type TargetLine = {
  target: string;
  sent: number;
  pending: number;
  awaiting_approval: number;
  failed: number;
  /** Only a failed target can be retried. */
  retryable: boolean;
};

export type RunView = {
  run_id: number;
  change: string;
  status: string;
  engine: string;
  depth: string;
  verdict?: string;
  summary?: string;
  error?: string;
  /** What the engine said, beside the code `error` carries (RL-1512). */
  error_detail?: string;
  degraded?: string;
  tokens: number;
  tokens_known: boolean;
  stages: Stages;
  truncated: boolean;
  /** Names, not a count: "58 files omitted" cannot be checked and a list can. */
  omitted_files: string[];
  findings: AnchoredFinding[];
  /** Size only. The text is fetched separately, on expand. */
  transcript_bytes: number;
  targets: TargetLine[];
};

export function fetchRun(runId: number): Promise<RunView> {
  return invoke<RunView>('get_run', { runId });
}

export function fetchTranscript(runId: number): Promise<string> {
  return invoke<string>('get_transcript', { runId });
}

export function retryTarget(runId: number, target: string): Promise<void> {
  return invoke<void>('retry_target', { runId, target });
}

// --- starting a review by hand (SPEC §15 screen 2) ---------------------------

/**
 * Git's empty tree. Reviewing a commit against it is what "the whole repository"
 * means expressed as a diff, so a whole-repository review needs no separate mode
 * on either side of the boundary.
 */
export const EMPTY_TREE = '4b825dc642cb6eb9a060e54bf8d69288fbee4904';

export type ReviewBranch = { name: string; head: string; current: boolean };
export type BranchList = {
  branches: ReviewBranch[];
  suggested_base?: string;
  current?: string;
};
export type ReviewCommit = { sha: string; subject: string; author: string; authored_at: string };

/** What was queued, and enough to say so: `scope` is the request in words. */
export type StartedReview = { run_id: number; status: string; scope: string };

/** `base` turns a commit review into a range: a branch's own work, or everything. */
export type ReviewRequest = {
  branch?: string;
  revision: string;
  base?: string;
};

export function fetchReviewBranches(repoId: number): Promise<BranchList> {
  return invoke<BranchList>('review_branches', { repoId });
}

export function fetchReviewCommits(repoId: number, branch: string): Promise<ReviewCommit[]> {
  return invoke<ReviewCommit[]>('review_commits', { repoId, branch });
}

export function startReview(repoId: number, request: ReviewRequest): Promise<StartedReview> {
  return invoke<StartedReview>('start_review', {
    repoId,
    branch: request.branch ?? null,
    revision: request.revision,
    base: request.base ?? null,
  });
}

// --- approvals (RL-1109, SPEC §12.4, §15 screen 5) --------------------------

export type QueuedAction = {
  id: number;
  run_id: number;
  target: string;
  capability: string;
  risk: string;
  /** The payload that would be sent, verbatim. Not a rendering of it. */
  payload_json: string;
  /** Why this cannot be sent as it stands, when it cannot (RL-1516). */
  unsendable?: string;
  /** How long before it is discarded, in words; absent when it waits forever. */
  deadline?: string | null;
  has_finding: boolean;
};

export type ApprovalsView = { waiting: QueuedAction[] };

export function fetchApprovals(): Promise<ApprovalsView> {
  return invoke<ApprovalsView>('list_approvals');
}
/**
 * Approve one action. Resolves with a delivery note, or `''` when it was sent.
 *
 * Approving and delivering are separate: an approval that could not be delivered
 * is still an approval, and the queue redelivers it. Rejecting the whole call
 * because Andare is unconfigured used to report "could not approve", which sent
 * people to look at the approval instead of at Settings.
 */
export function approveAction(id: number): Promise<string> {
  return invoke<string>('approve_action', { id });
}
export function approveRun(runId: number): Promise<string> {
  return invoke<string>('approve_run', { runId });
}
export function rejectAction(id: number, suppress: boolean): Promise<void> {
  return invoke<void>('reject_action', { id, suppress });
}
export function editPayload(id: number, payloadJson: string): Promise<void> {
  return invoke<void>('edit_payload', { id, payloadJson });
}

/** Which screen a capture harness asked for, or "" (RL-1102, §16.4). */
export function fetchInitialScreen(): Promise<string> {
  return invoke<string>('initial_screen');
}

/** Which repository a capture harness asked for, or 0 (RL-1102, §16.4). */
export function fetchInitialRepo(): Promise<number> {
  return invoke<number>('initial_repo');
}

/**
 * The step a scripted flow capture is on, or "" (RL-1103, §16.4).
 *
 * Read from the driver's own labels file, so the caption on a frame and the state
 * the app was in when it was taken come from one write.
 */
export function fetchFlowStep(): Promise<string> {
  return invoke<string>('flow_step');
}

/** Which onboarding step a capture harness asked for, or "" (RL-1102, §16.4). */
export function fetchInitialOnboardingStep(): Promise<string> {
  return invoke<string>('initial_onboarding_step');
}

/** Which run a capture harness asked for, or 0 (RL-1102, §16.4). */
export function fetchInitialRun(): Promise<number> {
  return invoke<number>('initial_run');
}

// --- findings (RL-1108, SPEC §15 screen 4) ----------------------------------

/** §10.1's severities, worst first — the order the filter means by "and worse". */
export const SEVERITIES = ['critical', 'high', 'medium', 'low', 'info'] as const;
export type Severity = (typeof SEVERITIES)[number];

/** §5's finding states. */
export const FINDING_STATES = ['open', 'published', 'suppressed', 'superseded'] as const;

export type FindingRow = {
  id: number;
  run_id: number;
  repo_id: number;
  repo: string;
  severity: string;
  category: string;
  state: string;
  title: string;
  file?: string;
  /**
   * The line it names, if any.
   *
   * Optional because a view serialised before RL-1553 has no such field.
   */
  line?: number;
  fingerprint: string;
  /**
   * How many runs have seen this problem, including the latest.
   *
   * The table shows one row per problem rather than per run (RL-1563). Optional
   * because a view serialised before that has no such field.
   */
  occurrences?: number;
};

/**
 * What the filter panel sends.
 *
 * Independent optional fields, because they compose: severity *and* category
 * means both. Filtering happens in the daemon — a cross-repository table is the
 * one screen that can be large, and filtering in the browser means fetching it
 * all first and paying for the size on every keystroke.
 */
export type FindingFilter = {
  min_severity?: string;
  category?: string;
  state?: string;
  repo_id?: number;
};

export type FindingsView = {
  rows: FindingRow[];
  /** Every category present before filtering, so the dropdown has no dead ends. */
  categories: string[];
  /** So the screen can say "12 of 340" rather than presenting a slice as a whole. */
  total_before_filter: number;
  /** Whether the scan stopped at its cap (§18). */
  truncated: boolean;
};

export function fetchFindings(filter: FindingFilter): Promise<FindingsView> {
  return invoke<FindingsView>('list_findings', { filter });
}

/** Suppress one finding. Returns its new state. */
export function suppressFinding(id: number): Promise<string> {
  return invoke<string>('suppress_finding', { id });
}

/**
 * File a finding to Andare by hand. Returns the status it was *given*.
 *
 * Not "filed" — under the default mode it is queued for approval, and the caller
 * has to say which happened rather than assume the optimistic one.
 */
export function fileToAndare(id: number): Promise<string> {
  return invoke<string>('file_to_andare', { id });
}

// --- repository (RL-1106, SPEC §15 screen 2) --------------------------------

/** The four ways a change reaches rev-local (§7). */
export const TRIGGERS = ['poll', 'hooks', 'webhook', 'manual'] as const;

/**
 * One trigger's live state.
 *
 * Four cases rather than a boolean, because "off" and "broken" want opposite
 * responses from whoever is reading. A deliberately disabled webhook is fine; one
 * enabled with no secret is silently dropping every delivery.
 */
export type TriggerStatus = {
  trigger: string;
  state: 'active' | 'off' | 'broken' | 'not_applicable';
  /** Why it stands there. Always present — an unexplained light gets guessed at. */
  detail: string;
};

/**
 * What a repository watches, in its own vocabulary (§6.4).
 *
 * Tagged, so SVN paths cannot be rendered under a "branches" heading. An SVN
 * "branch" is a directory, and calling it a branch is where the confusion starts.
 */
export type Watching =
  | { kind: 'branches'; globs: string[] }
  | { kind: 'paths'; paths: string[] };

export type RunLine = {
  run_id: number;
  status: string;
  verdict?: string;
  trigger: string;
  started_at?: string;
};

export type RepositoryView = {
  repo: RepoView;
  watching: Watching;
  /** Exactly four, each computed independently. */
  triggers: TriggerStatus[];
  recent_runs: RunLine[];
  /** Whether there are older runs than the ones listed (§18). */
  more_runs: boolean;
  budget: BudgetBar;
  /** §13.2's document, not a form over it. */
  config_json: string;
  last_run?: LastRun;
};

export function fetchRepository(repoId: number): Promise<RepositoryView> {
  return invoke<RepositoryView>('get_repository', { repoId });
}

/** Save a config. Rejects with the validation error when it will not parse. */
export function saveRepoConfig(repoId: number, configJson: string): Promise<string> {
  return invoke<string>('save_repo_config', { repoId, configJson });
}

// --- settings (RL-1110, SPEC §15 screen 6) ----------------------------------

/**
 * That a secret is configured, and where it comes from. Never what it is.
 *
 * There is no field here that could hold a secret value, which is the property
 * that makes this safe rather than careful: a redacted-on-render secret has
 * already crossed the boundary, and every future renderer has to remember.
 */
export type SecretPresence = {
  header: string;
  source: string;
  /** A keychain entry's name — not a secret, and the useful half. */
  keychain_entry?: string;
  advice?: string;
};

export type ServerPanel = {
  id: string;
  transport: string;
  endpoint: string;
  secrets: SecretPresence[];
  tools: string[];
  summary: string;
  contacted: boolean;
  error?: string;
};

export type BoundRow = {
  capability: string;
  tool: string;
  /** ADR 0015: "you told us to" and "we worked it out" are different answers. */
  from_override: boolean;
};

export type UnmappedRow = {
  capability: string;
  /** What was looked for. */
  candidates: string[];
  /** What the server has instead — the list an override picks from. */
  available: string[];
  explanation: string;
};

export type TargetPanel = {
  target: string;
  server: string;
  bound: BoundRow[];
  unmapped: UnmappedRow[];
  /** When false the mapping is unknown, not unmapped. */
  server_contacted: boolean;
};

export type DoctorCheck = {
  name: string;
  health: 'ok' | 'warn' | 'fail' | 'not_needed';
  detail: string;
  remediation?: string;
};

export type DoctorReport = {
  prerequisites: DoctorCheck[];
  engines: DoctorCheck[];
  targets: DoctorCheck[];
  platform: DoctorCheck[];
  /**
   * What the install itself is doing — autopilot, checkouts, approvals.
   *
   * Optional because a report serialised before RL-1551 has no such field, and a
   * screen that throws on last week's JSON is worse than one that shows less.
   */
  install?: DoctorCheck[];
};

export type Limits = {
  daily_tokens_per_repo: number;
  daily_runs_per_repo: number;
  daily_cost_usd_per_repo: number;
  on_exhausted: string;
  transcript_retention_days: number;
};

export type SettingsView = {
  doctor: DoctorReport;
  servers: ServerPanel[];
  targets: TargetPanel[];
  limits: Limits;
  config_path: string;
  overrides_path: string;
  target_errors: string[];
};

/** Every check in report order — the four groups are presentation, not meaning. */
export function allChecks(report: DoctorReport): DoctorCheck[] {
  return [
    ...report.prerequisites,
    ...report.engines,
    ...report.targets,
    ...report.platform,
    // Last, because it is the section somebody scrolls to when the ones above
    // are all green and nothing is happening anyway (RL-1551).
    ...(report.install ?? []),
  ];
}

/** How many capabilities are unmapped across every target. */
export function unmappedCount(view: SettingsView): number {
  return view.targets.reduce((total, t) => total + t.unmapped.length, 0);
}

// --- starting at login (RL-1517) --------------------------------------------

/**
 * Whether rev-local starts when you log in.
 *
 * `unsupported` is not `disabled`. "You have not turned it on" and "turning it
 * on does nothing here" are different things to tell somebody, and only one of
 * them deserves a switch.
 */
export type StartupStatus = 'enabled' | 'disabled' | 'unsupported';

export function fetchStartup(): Promise<StartupStatus> {
  return invoke<StartupStatus>('startup_status');
}

export function setStartup(enabled: boolean): Promise<StartupStatus> {
  return invoke<StartupStatus>('set_startup', { enabled });
}

export function fetchSettings(): Promise<SettingsView> {
  return invoke<SettingsView>('settings');
}

/** Re-run doctor and return the settings with its fresh output. */
export function runDoctor(): Promise<SettingsView> {
  return invoke<SettingsView>('run_doctor');
}

/** Configure the built-in Andare and Trama HTTP endpoints with Keychain bearers. */
export function configureMcp(suiteBearer: string): Promise<void> {
  return invoke<void>('configure_mcp', { suiteBearer });
}

export function setOverride(
  target: string,
  capability: string,
  tool: string,
  argsJson: string,
): Promise<void> {
  return invoke<void>('set_override', { target, capability, tool, argsJson });
}

export function clearOverride(target: string, capability: string): Promise<void> {
  return invoke<void>('clear_override', { target, capability });
}

// --- notifications and the tray (RL-1111, SPEC §15) -------------------------

/**
 * Why rev-local wants to interrupt somebody.
 *
 * Sent as-is to the daemon, which decides whether to show it. Deliberately *not*
 * filtered here: a front end that dropped medium-severity findings before asking
 * would be a second copy of the rule, and the copy that drifts is the one nobody
 * is testing.
 */
export type NotifyReason =
  | { kind: 'finding'; severity: string; fingerprint: string; title: string; repo: string }
  | { kind: 'approval'; action_id: number; target: string; capability: string };

/**
 * A finding, as a reason.
 *
 * The **fingerprint** is what makes two runs over one unfixed bug a single
 * notification (§10.3). Sending the title instead would look identical until a
 * finding was reworded, at which point somebody gets told twice about a thing
 * they already fixed nothing about.
 */
export function reasonForFinding(
  finding: { severity: string; fingerprint: string; title: string },
  repo: string,
): NotifyReason {
  return {
    kind: 'finding',
    severity: finding.severity,
    fingerprint: finding.fingerprint,
    title: finding.title,
    repo,
  };
}

/** A queued action, as a reason. Identified by its id: two actions are two decisions. */
export function reasonForApproval(action: QueuedAction): NotifyReason {
  return {
    kind: 'approval',
    action_id: action.id,
    target: action.target,
    capability: action.capability,
  };
}

/** What the daemon decided about one reason. */
export type NotifyDecision =
  | { decision: 'show'; title: string; body: string }
  | { decision: 'summarise'; title: string; body: string; suppressed: number }
  | { decision: 'suppressed'; reason: string };

export function notify(reason: NotifyReason): Promise<NotifyDecision> {
  return invoke<NotifyDecision>('notify', { reason });
}

/** Bring the tray tooltip in line with the paused state. Returns the tooltip. */
export function refreshTray(): Promise<string> {
  return invoke<string>('refresh_tray');
}

// --- onboarding (RL-1205, SPEC §15) -----------------------------------------

/** §15's five steps, in order. */
export const STEPS = ['check', 'add_repo', 'pick_engine', 'pick_autonomy', 'first_review'] as const;
export type Step = (typeof STEPS)[number];

export const STEP_TITLES: Record<Step, string> = {
  check: 'Check what is installed',
  add_repo: 'Choose a repository',
  pick_engine: 'Choose an engine',
  pick_autonomy: 'Choose how much it may do',
  first_review: 'Review one change',
};

/** §8.4's engines. `mock` spends nothing and invents its findings. */
export const ENGINES = ['mock', 'claude', 'codex'] as const;

export const ENGINE_LABELS: Record<string, string> = {
  mock: 'Mock — spends nothing, invents its findings (a rehearsal)',
  claude: 'Claude Code',
  codex: 'Codex',
};

/**
 * What onboarding is building.
 *
 * `autonomy` starts at `dry_run` and never at `auto`: a repository added a moment
 * ago has never been reviewed and nobody has seen a finding from it, so the one
 * thing this flow must not do is leave it able to publish.
 */
export type Draft = {
  path: string;
  name: string;
  kind: string;
  engine: string;
  autonomy: string;
};

export function emptyDraft(): Draft {
  return { path: '', name: '', kind: 'git', engine: 'mock', autonomy: 'dry_run' };
}

export type FirstReview = {
  run_id: number;
  repo: string;
  status: string;
  verdict?: string;
  findings: number;
  engine: string;
  /** Present when the mock ran — said out loud, never implied (§18). */
  caveat?: string;
};

export function fetchIsFirstRun(): Promise<boolean> {
  return invoke<boolean>('is_first_run');
}

export function onboardAddRepo(draft: Draft): Promise<{ name: string }> {
  return invoke<{ name: string }>('onboard_add_repo', { draft });
}

export function onboardFirstReview(repo: string): Promise<FirstReview> {
  return invoke<FirstReview>('onboard_first_review', { repo });
}
