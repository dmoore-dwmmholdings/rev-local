import {
  ago,
  exactly,
  toneOf,
  MODES,
  MODE_LABELS,
  type Autopilot,
  type BudgetBar,
  type Mode,
  type QueueItem,
  type QueueStatus,
  type RepoCard,
} from './ipc';

/**
 * One budget bar (SPEC §15, §18).
 *
 * Draws the fraction it knows and says the numbers beside it. A percentage alone
 * cannot say "of what", and somebody deciding whether to widen a ceiling needs
 * both halves.
 *
 * When the token count is a lower bound — some run today reported no count — the
 * bar is drawn differently and labelled. §18: a partial total must not render as
 * a total, and a bar is the easiest place in a UI to lose that distinction.
 */
function Budget({ budget }: { budget: BudgetBar }) {
  const fraction = (used: number, limit: number) =>
    limit === 0 ? 0 : Math.min(1, used / limit);

  return (
    <div className="budget">
      <div className="budget-row">
        <span className="budget-label">runs</span>
        <span className="meter" role="img" aria-label={`${budget.runs} of ${budget.runs_limit} runs`}>
          <span className="meter-fill" style={{ width: `${fraction(budget.runs, budget.runs_limit) * 100}%` }} />
        </span>
        <span className="budget-figure">
          {budget.runs}
          {budget.runs_limit > 0 && ` / ${budget.runs_limit}`}
        </span>
      </div>

      <div className="budget-row">
        <span className="budget-label">tokens</span>
        <span
          className={budget.tokens_known ? 'meter' : 'meter meter-partial'}
          role="img"
          aria-label={`${budget.tokens} of ${budget.tokens_limit} tokens${
            budget.tokens_known ? '' : ', at least — a run today reported no count'
          }`}
        >
          <span
            className="meter-fill"
            style={{ width: `${fraction(budget.tokens, budget.tokens_limit) * 100}%` }}
          />
        </span>
        <span className="budget-figure">
          {budget.tokens_known ? '' : '≥ '}
          {budget.tokens.toLocaleString()}
          {budget.tokens_limit > 0 && ` / ${budget.tokens_limit.toLocaleString()}`}
        </span>
      </div>

      {!budget.tokens_known && (
        <p className="hedge">a run today reported no token count, so this is a lower bound</p>
      )}
    </div>
  );
}

/** One repository's card: health, last run, queue depth, today's budget. */
function Card({
  card,
  onOpenRun,
  onOpenRepo,
}: {
  card: RepoCard;
  onOpenRun: (runId: number) => void;
  onOpenRepo: (repoId: number) => void;
}) {
  const { repo, last_run: lastRun, queue_depth: queued, budget } = card;

  return (
    <li className="card">
      <header className="card-head">
        <span className={`dot ${repo.health.health === 'healthy' ? 'ok' : 'bad'}`} />
        {/* The name is the way into §15's screen 2. A card that showed a
            repository but could not open it would leave the only route to its
            config and triggers being the command line. */}
        <h3>
          <button className="link" onClick={() => onOpenRepo(repo.id)}>
            {repo.repo}
          </button>
        </h3>
        <span className="spacer" />
        {/* Autonomy is on the card because it is the setting that decides
            whether this repository writes to anybody else's systems. */}
        <span className="tag">{repo.autonomy}</span>
        {!repo.enabled && <span className="tag tag-off">disabled</span>}
      </header>

      <dl className="card-facts">
        <dt>last run</dt>
        <dd>
          {/* Said, never omitted: a card with no line about runs reads as one
              whose runs failed to load. */}
          {lastRun ? (
            <button className="link" onClick={() => onOpenRun(lastRun.run_id)}>
              #{lastRun.run_id} {lastRun.status}
              {lastRun.verdict ? ` (${lastRun.verdict})` : ''}
            </button>
          ) : (
            'none yet'
          )}
        </dd>
        <dt>queued</dt>
        <dd>{queued}</dd>
        <dt>engine</dt>
        <dd>
          {repo.kind} · {repo.engine}
        </dd>
      </dl>

      <Budget budget={budget} />
    </li>
  );
}

/**
 * The global autonomy ceiling (§12.2).
 *
 * Widening it is confirmed and narrowing it is not. §15 requires a destructive or
 * outbound action to name its target, and the asymmetry is the point: turning
 * autonomy *up* is what lets rev-local write to somebody else's systems, while
 * turning it down can only ever stop it.
 */
function ModeSelector({
  mode,
  onChange,
}: {
  mode: string;
  onChange: (next: Mode) => void;
}) {
  function choose(next: Mode) {
    if (next === mode) return;

    const widening = MODES.indexOf(next) > MODES.indexOf(mode as Mode);
    if (widening) {
      const ok = window.confirm(
        `Widen autonomy to "${MODE_LABELS[next]}"?\n\n` +
          'This raises the ceiling for every repository. A repository set lower ' +
          'stays lower; one set higher is currently held down by this.',
      );
      if (!ok) return;
    }
    onChange(next);
  }

  return (
    <label className="mode">
      <span>mode</span>
      <select value={mode} onChange={(e) => choose(e.target.value as Mode)}>
        {MODES.map((m) => (
          <option key={m} value={m}>
            {MODE_LABELS[m]}
          </option>
        ))}
      </select>
    </label>
  );
}

/**
 * One run in the queue panel, as a row.
 *
 * The status carries the colour rather than the whole row: a table where every
 * row is coloured says nothing, and the thing worth spotting while scanning is
 * the one run that failed.
 */
function QueueRow({
  item,
  when,
  onOpenRun,
}: {
  item: QueueItem;
  /** Which timestamp this row is about — they mean different things per section. */
  when: 'created' | 'started' | 'finished';
  onOpenRun: (runId: number) => void;
}) {
  const at =
    when === 'created' ? item.created_at : when === 'started' ? item.started_at : item.finished_at;

  return (
    <tr>
      <td>
        <button className="link" onClick={() => onOpenRun(item.run_id)}>
          #{item.run_id}
        </button>
      </td>
      <td>{item.repo}</td>
      <td className="queue-change">
        <span className="mono">{item.change.slice(0, 12)}</span>
        {item.title && <span className="dim"> {item.title}</span>}
      </td>
      <td>
        <span className={`tag run-${toneOf(item.status)}`}>{item.status}</span>
        {item.verdict && <span className="dim"> {item.verdict}</span>}
      </td>
      <td>{item.trigger}</td>
      <td className="dim" title={exactly(at)}>
        {ago(at)}
      </td>
    </tr>
  );
}

function QueueTable({
  items,
  when,
  onOpenRun,
}: {
  items: QueueItem[];
  when: 'created' | 'started' | 'finished';
  onOpenRun: (runId: number) => void;
}) {
  const heading = when === 'created' ? 'Queued' : when === 'started' ? 'Started' : 'Finished';

  return (
    <table className="queue-table">
      <thead>
        <tr>
          <th>Run</th>
          <th>Repository</th>
          <th>Change</th>
          <th>Status</th>
          <th>Trigger</th>
          <th>{heading}</th>
        </tr>
      </thead>
      <tbody>
        {items.map((item) => (
          <QueueRow key={item.run_id} item={item} when={when} onOpenRun={onOpenRun} />
        ))}
      </tbody>
    </table>
  );
}

/**
 * The queue, as three answers rather than one list (§15 screen 1).
 *
 * "What is running", "what is waiting" and "what just finished" are different
 * questions, and a single table of runs answers none of them without being read
 * row by row. Each section says so in words when it is empty, because a section
 * that disappears when empty is indistinguishable from one that failed to load.
 *
 * `running` comes from the runs themselves, not from whether this window started
 * them: a review kicked off from the command line, or by a previous session of
 * this app, is still a review that is running.
 */
function Queue({
  queue,
  paused,
  onStart,
  onOpenRun,
}: {
  queue: QueueStatus | null;
  paused: boolean;
  onStart: () => void;
  onOpenRun: (runId: number) => void;
}) {
  if (!queue) {
    return (
      <section className="queue">
        <h2>Queue</h2>
        <p className="dim">Reading the queue…</p>
      </section>
    );
  }

  const idle = !queue.running && queue.queued_total === 0;

  return (
    <section className="queue">
      <header className="queue-head">
        <h2>Queue</h2>
        <span className={queue.running ? 'tag run-active' : 'tag'}>
          {queue.active.length} running
        </span>
        <span className="tag">{queue.queued_total} waiting</span>
        <span className="spacer" />
        {/* Busy is read from the runs, not from whether this window pressed the
            button. A window-local flag is cleared after the last run's event has
            already been delivered, so a queue that stopped with work still in it
            left this button disabled and claiming to be working — with nothing
            left to arrive that would correct it. */}
        <button
          onClick={onStart}
          disabled={paused || queue.running || queue.queued_total === 0}
          title={
            paused
              ? 'The kill switch is engaged — release it before starting queued work'
              : queue.running
                ? 'A review is already running'
                : queue.queued_total === 0
                  ? 'Nothing is waiting to run'
                  : 'Work through the queued reviews'
          }
        >
          {queue.running ? 'Reviewing…' : 'Start queued reviews'}
        </button>
      </header>

      {/* The kill switch is announced once, at the top of the screen. What is
          added here is what it means for the queue specifically. */}
      {paused && (
        <p className="warn-text">
          Held by the kill switch: {queue.queued_total} queued review
          {queue.queued_total === 1 ? '' : 's'} will not start until it is released.
        </p>
      )}

      <h3>Running now</h3>
      {queue.active.length === 0 ? (
        <p className="dim">
          {idle ? 'Nothing is running and nothing is waiting.' : 'Nothing is running.'}
        </p>
      ) : (
        <QueueTable items={queue.active} when="started" onOpenRun={onOpenRun} />
      )}

      <h3>Waiting</h3>
      {queue.waiting.length === 0 ? (
        <p className="dim">No reviews are waiting to run.</p>
      ) : (
        <>
          <QueueTable items={queue.waiting} when="created" onOpenRun={onOpenRun} />
          {/* §18: a list that is a page of a queue must not read as the queue. */}
          {queue.waiting_hidden > 0 && (
            <p className="dim">
              Showing the next {queue.waiting.length} of {queue.queued_total}, in the order they
              will run — {queue.waiting_hidden} more are waiting.
            </p>
          )}
        </>
      )}

      <h3>Recently finished</h3>
      {queue.recent.length === 0 ? (
        <p className="dim">No review has finished yet.</p>
      ) : (
        <QueueTable items={queue.recent} when="finished" onOpenRun={onOpenRun} />
      )}
    </section>
  );
}


/**
 * What the background loop is doing (RL-1506).
 *
 * The first thing on the screen, because it answers the first question: is this
 * thing on. Before it existed the honest answer was "no" — discovery and the
 * queue both waited for a button — and nothing on any screen said so.
 *
 * The sentence comes from the daemon whole. Rebuilding it here from counts would
 * be a second opinion about what happened, and the two would disagree the first
 * time either changed.
 */
function AutopilotPanel({
  autopilot,
  mode,
  onToggle,
  onRunNow,
}: {
  autopilot: Autopilot | null;
  mode: string;
  onToggle: (enabled: boolean) => void;
  onRunNow: () => void;
}) {
  if (!autopilot) {
    return null;
  }

  const minutes = Math.round(autopilot.interval_secs / 60);
  const every =
    autopilot.interval_secs < 90
      ? `every ${autopilot.interval_secs} seconds`
      : `every ${minutes} minutes`;

  return (
    <section className="autopilot" aria-label="autopilot">
      <div className="autopilot-head">
        <label className="autopilot-switch">
          <input
            type="checkbox"
            checked={autopilot.enabled}
            onChange={(e) => onToggle(e.target.checked)}
          />
          <span>{autopilot.enabled ? `On — checking ${every}` : 'Off'}</span>
        </label>
        <button className="link" onClick={onRunNow} disabled={autopilot.ticking}>
          {autopilot.ticking ? 'checking…' : 'check now'}
        </button>
      </div>

      {autopilot.enabled && (
        <p className="autopilot-line">
          {autopilot.last_tick_at ? (
            <>
              {autopilot.last_line}{' '}
              <span className="dim" title={exactly(autopilot.last_tick_at)}>
                {ago(autopilot.last_tick_at)}
              </span>
            </>
          ) : (
            'Waiting for the first pass.'
          )}
        </p>
      )}

      {/* Filing an issue is high risk by §12.3's list, so every mode below `auto`
          holds every issue for a human. Somebody who turned the loop on and got
          an inbox instead of a tracker needs to be told why, here, once. */}
      {autopilot.enabled && mode !== 'auto' && mode !== 'off' && (
        <p className="hedge">
          Findings will wait for your approval — filing an issue counts as high
          risk. Choose “{MODE_LABELS.auto}” above to let it file on its own.
        </p>
      )}

      {autopilot.last_error && (
        <p className="banner bad" role="alert">
          {autopilot.last_error}
        </p>
      )}

      {autopilot.notes.length > 0 && (
        <ul className="autopilot-notes">
          {autopilot.notes.map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** §15 screen 1. */
export function Dashboard({
  dashboard,
  queue = null,
  autopilot = null,
  onStartQueue = () => {},
  onToggleAutopilot = () => {},
  onAutopilotNow = () => {},
  onMode,
  onOpenRun,
  onOpenRepo,
}: {
  dashboard: { repos: RepoCard[]; mode: string; paused: boolean } | null;
  queue?: QueueStatus | null;
  autopilot?: Autopilot | null;
  onStartQueue?: () => void;
  onToggleAutopilot?: (enabled: boolean) => void;
  onAutopilotNow?: () => void;
  onMode: (next: Mode) => void;
  onOpenRun: (runId: number) => void;
  onOpenRepo: (repoId: number) => void;
}) {
  if (!dashboard) {
    return <p className="empty">Loading the dashboard.</p>;
  }

  return (
    <section className="dashboard">
      <AutopilotPanel
        autopilot={autopilot}
        mode={dashboard.mode}
        onToggle={onToggleAutopilot}
        onRunNow={onAutopilotNow}
      />

      <div className="dashboard-head">
        <ModeSelector mode={dashboard.mode} onChange={onMode} />
        <p className="dim">The ceiling for every repository. One set higher is held down to this.</p>
        {dashboard.paused && (
          <p className="banner" role="status">
            Paused. Nothing is being reviewed and publish actions are held.
          </p>
        )}
      </div>

      {dashboard.repos.length === 0 ? (
        <p className="empty">
          No repositories yet. Add one from Settings → Add repository, or with{' '}
          <code>revlocal repo add</code>.
        </p>
      ) : (
        <ul className="cards">
          {dashboard.repos.map((card) => (
            <Card
              key={card.repo.id}
              card={card}
              onOpenRun={onOpenRun}
              onOpenRepo={onOpenRepo}
            />
          ))}
        </ul>
      )}

      <Queue queue={queue} paused={dashboard.paused} onStart={onStartQueue} onOpenRun={onOpenRun} />
    </section>
  );
}
