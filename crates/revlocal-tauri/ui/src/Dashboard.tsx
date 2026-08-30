import { MODES, MODE_LABELS, type BudgetBar, type Mode, type RepoCard } from './ipc';

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

/** §15 screen 1. */
export function Dashboard({
  dashboard,
  onMode,
  onOpenRun,
  onOpenRepo,
}: {
  dashboard: { repos: RepoCard[]; mode: string; paused: boolean } | null;
  onMode: (next: Mode) => void;
  onOpenRun: (runId: number) => void;
  onOpenRepo: (repoId: number) => void;
}) {
  if (!dashboard) {
    return <p className="empty">Loading the dashboard.</p>;
  }

  return (
    <section className="dashboard">
      <div className="dashboard-head">
        <ModeSelector mode={dashboard.mode} onChange={onMode} />
        {dashboard.paused && (
          <p className="banner" role="status">
            Paused. Nothing is being reviewed and publish actions are held.
          </p>
        )}
      </div>

      {dashboard.repos.length === 0 ? (
        <p className="empty">
          No repositories yet. Add one with <code>revlocal repo add</code>.
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
    </section>
  );
}
