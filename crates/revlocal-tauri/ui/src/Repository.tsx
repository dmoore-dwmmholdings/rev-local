import { useEffect, useRef, useState } from 'react';
import {
  ago,
  EMPTY_TREE,
  exactly,
  toneOf,
  type BranchList,
  type RepositoryView,
  type ReviewCommit,
  type ReviewRequest,
  type StartedReview,
  type TriggerStatus,
  type Watching,
} from './ipc';

/**
 * §15 screen 2 — one repository: what it watches, what can trigger it, what it
 * has run, what it has spent, and its config.
 *
 * Two things here are decisions rather than layout.
 *
 * **Four indicators, not one.** The four triggers fail for unrelated reasons —
 * the network, a file on disk, a missing secret, and never. A single rolled-up
 * light would show amber for a repository whose hooks are dead and whose polling
 * is fine, and send somebody looking in the wrong place.
 *
 * **The config is edited as the document it is.** A typed form would be a second
 * spelling of `RepoConfig`, updated by hand every time a field is added, and the
 * field somebody could not reach would be invisible rather than obviously
 * missing. §13.2's JSON is what rev-local reads, so it is what gets edited.
 */

/** How each state reads, and how much weight it deserves. */
const STATE_LABEL: Record<TriggerStatus['state'], string> = {
  active: 'live',
  off: 'off',
  broken: 'not working',
  not_applicable: 'n/a',
};

function Trigger({ status }: { status: TriggerStatus }) {
  return (
    <li className={`trigger trigger-${status.state}`}>
      <span className={`dot dot-${status.state}`} aria-hidden="true" />
      <strong>{status.trigger}</strong>
      <span className="tag">{STATE_LABEL[status.state]}</span>
      {/* The reason travels with the light. An indicator with no explanation is
          one somebody has to guess at, and the guess is usually "it is fine". */}
      <span className="dim">{status.detail}</span>
    </li>
  );
}

function Watched({ watching }: { watching: Watching }) {
  // §6.4: two vocabularies, and the heading changes with them. Rendering SVN
  // paths under "branches" would suggest a filter that is not being applied.
  if (watching.kind === 'paths') {
    return (
      <section className="watched">
        <h3>Watched paths</h3>
        <p className="dim">
          Subversion has no branches — these are repository paths, watched by polling.
        </p>
        <ul className="globs">
          {watching.paths.map((p) => (
            <li key={p} className="mono">
              {p}
            </li>
          ))}
        </ul>
      </section>
    );
  }

  return (
    <section className="watched">
      <h3>Watched branches</h3>
      {watching.globs.length === 0 ? (
        // An empty list is a real configuration and a surprising one. Said in
        // words, because an empty <ul> reads as a section that failed to load.
        <p className="empty">No branch globs are configured, so nothing matches.</p>
      ) : (
        <ul className="globs">
          {watching.globs.map((g) => (
            <li key={g} className="mono">
              {g}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Whether this repository kind can have a review started against a local branch. */
function reviewableByHand(kind: string): boolean {
  return kind === 'git' || kind === 'github';
}

/** Read something a rejected promise threw, without assuming it is an Error. */
function messageOf(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  return String((error as { message?: string })?.message ?? error);
}


/**
 * The one configuration field that decides whether anything is ever filed
 * (RL-1503).
 *
 * `andare_project` has always been settable — in the raw JSON box below, if you
 * knew the key existed. Nothing named it anywhere in the app, so three
 * repositories ran for a week with `{}` and every finding was held with
 * "no `andare_project` set" in a report nobody was shown.
 *
 * It edits the same draft the textarea does rather than saving on its own, so
 * there is one Save button and one validation path. A form that wrote directly
 * would silently discard whatever was half-typed below it.
 */
function FilingConfig({
  draft,
  onChange,
}: {
  draft: string;
  onChange: (next: string) => void;
}) {
  let parsed: Record<string, unknown> | null = null;
  try {
    const value: unknown = JSON.parse(draft || '{}');
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
      parsed = value as Record<string, unknown>;
    }
  } catch {
    parsed = null;
  }

  // While the JSON below does not parse there is nothing coherent to edit, and
  // guessing would overwrite what is being typed.
  if (!parsed) {
    return (
      <section className="config">
        <h3>Where findings go</h3>
        <p className="dim">
          Fix the configuration below first — it does not parse, so this cannot show
          what is set.
        </p>
      </section>
    );
  }

  const project = typeof parsed.andare_project === 'string' ? parsed.andare_project : '';
  const severity =
    typeof parsed.andare_min_severity === 'string' ? parsed.andare_min_severity : 'medium';

  function set(key: string, value: string | null) {
    const next = { ...(parsed as Record<string, unknown>) };
    if (value === null || value === '') {
      delete next[key];
    } else {
      next[key] = value;
    }
    onChange(`${JSON.stringify(next, null, 2)}\n`);
  }

  return (
    <section className="config">
      <h3>Where findings go</h3>
      <label className="field">
        <span>Andare project key</span>
        <input
          value={project}
          placeholder="e.g. REVL"
          aria-label="andare project key"
          onChange={(e) => set('andare_project', e.target.value.trim().toUpperCase())}
        />
      </label>
      <label className="field">
        <span>File findings at or above</span>
        <select
          aria-label="minimum severity to file"
          value={severity}
          onChange={(e) => set('andare_min_severity', e.target.value)}
        >
          {['critical', 'high', 'medium', 'low', 'info'].map((level) => (
            <option key={level} value={level}>
              {level}
            </option>
          ))}
        </select>
      </label>
      {project === '' && (
        <p className="hedge">
          Nothing is filed while this is empty. Reviews still run and findings are
          still stored — they just stay here.
        </p>
      )}
      <p className="dim">Saved with the configuration below.</p>
    </section>
  );
}

/**
 * Start a review now, without waiting for a trigger (§15 screen 2).
 *
 * Three scopes, because they answer three different questions and collapsing
 * them would make two of the answers wrong:
 *
 * - **A branch** against what it forked from. This is the one people mean by
 *   "review my branch": the branch's own work, not everything that has landed on
 *   the base since it was cut.
 * - **A single commit**, for going back to something specific.
 * - **The whole repository**, expressed as a diff against the empty tree, so
 *   depth selection and truncation apply to it like any other review — a
 *   whole-repository review that only saw part of the tree still says so.
 *
 * Starting a review queues it and returns. The engine takes as long as it takes;
 * a button that stayed pressed until it finished would look like a hang, and the
 * run it created is exactly the thing worth looking at in the meantime.
 */
function ReviewNow({
  branches,
  branch,
  base,
  commits,
  starting,
  started,
  error,
  onBranch,
  onBase,
  onReview,
  onOpenRun,
}: {
  branches: BranchList | null;
  branch: string;
  base: string;
  commits: ReviewCommit[] | null;
  starting: boolean;
  started: StartedReview | null;
  error: string | null;
  onBranch: (next: string) => void;
  onBase: (next: string) => void;
  onReview: (request: ReviewRequest) => void;
  onOpenRun: (runId: number) => void;
}) {
  const head = branches?.branches.find((item) => item.name === branch);
  // A branch cannot be reviewed against itself, and a repository with one branch
  // has nothing to compare it to. Both are said rather than left as a control
  // that does nothing when pressed.
  const comparable = base !== '' && base !== branch;

  return (
    <section className="review-now">
      <h3>Review now</h3>
      <p className="dim">
        Start a review whenever you want one, without waiting for a trigger. It is queued
        immediately and runs in the background — this window stays usable, and the run appears in
        live activity on the dashboard.
      </p>

      {branches === null ? (
        <p className="dim">Reading branches…</p>
      ) : branches.branches.length === 0 ? (
        <p className="empty">This repository has no local branches to review.</p>
      ) : (
        <>
          <div className="review-controls">
            <label className="filter">
              Branch
              <select value={branch} onChange={(event) => onBranch(event.target.value)}>
                {branches.branches.map((item) => (
                  <option key={item.name} value={item.name}>
                    {item.name}
                    {item.current ? ' (checked out)' : ''}
                  </option>
                ))}
              </select>
            </label>

            <label className="filter">
              Compared against
              <select value={base} onChange={(event) => onBase(event.target.value)}>
                <option value="">— nothing; review its head commit only —</option>
                {branches.branches
                  .filter((item) => item.name !== branch)
                  .map((item) => (
                    <option key={item.name} value={item.name}>
                      {item.name}
                    </option>
                  ))}
              </select>
            </label>

            <div className="review-actions">
              <button
                disabled={starting || !head || !comparable}
                title={
                  comparable
                    ? `Review everything on ${branch} since it diverged from ${base}`
                    : 'Choose a different branch to compare against'
                }
                onClick={() => head && onReview({ branch, revision: head.head, base })}
              >
                Review branch
              </button>
              <button
                disabled={starting || !head}
                title={`Review every tracked file on ${branch}, not just a change`}
                onClick={() =>
                  head && onReview({ branch, revision: head.head, base: EMPTY_TREE })
                }
              >
                Review whole repository
              </button>
            </div>
          </div>

          {/* Said rather than left to be inferred from a disabled button. */}
          {!comparable && (
            <p className="dim">
              {branches.branches.length < 2
                ? 'There is only one branch, so there is nothing to compare it against — a whole-repository review is the alternative.'
                : 'Choose a branch to compare against to review a branch\u2019s own work.'}
            </p>
          )}

          {/* Brief, but not nothing: the buttons going grey is the only other
              signal, and a grey button reads as "not allowed" as easily as
              "working". */}
          {starting && <p className="dim">Queueing the review…</p>}
          {started && (
            <p className="review-started" role="status">
              Queued run #{started.run_id} — {started.scope}.{' '}
              <button className="link" onClick={() => onOpenRun(started.run_id)}>
                open it
              </button>
            </p>
          )}
          {error && (
            <p className="config-error" role="alert">
              {error}
            </p>
          )}

          <h4>Or review one commit</h4>
          {commits === null ? (
            <p className="dim">Reading commits on {branch}…</p>
          ) : commits.length === 0 ? (
            <p className="empty">No commits on {branch}.</p>
          ) : (
            <table className="commits">
              <thead>
                <tr>
                  <th>Commit</th>
                  <th>Subject</th>
                  <th>Author</th>
                  <th>Authored</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {commits.map((commit) => (
                  <tr key={commit.sha}>
                    <td className="mono">{commit.sha.slice(0, 12)}</td>
                    <td>{commit.subject || '(no commit subject)'}</td>
                    <td className="dim">{commit.author}</td>
                    <td className="dim" title={exactly(commit.authored_at)}>
                      {ago(commit.authored_at)}
                    </td>
                    <td className="row-actions">
                      <button
                        disabled={starting}
                        onClick={() => onReview({ branch, revision: commit.sha })}
                      >
                        Review
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {/* §18: "the last hundred" and "all there have ever been" must not look
              the same. */}
          {commits !== null && commits.length === 100 && (
            <p className="dim">The most recent 100 commits on {branch}.</p>
          )}
        </>
      )}
    </section>
  );
}

export function Repository({
  view,
  onOpenRun,
  onSave,
  onLoadBranches,
  onLoadCommits,
  onReview,
}: {
  view: RepositoryView | null;
  onOpenRun: (runId: number) => void;
  /** Resolves when saved; rejects with the validation error to show inline. */
  onSave: (configJson: string) => Promise<void>;
  onLoadBranches?: (repoId: number) => Promise<BranchList>;
  onLoadCommits?: (repoId: number, branch: string) => Promise<ReviewCommit[]>;
  /** Resolves once the run exists — not once the review has finished. */
  onReview?: (repoId: number, request: ReviewRequest) => Promise<StartedReview>;
}) {
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [branches, setBranches] = useState<BranchList | null>(null);
  const [branch, setBranch] = useState('');
  const [base, setBase] = useState('');
  const [commits, setCommits] = useState<ReviewCommit[] | null>(null);
  const [reviewError, setReviewError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [started, setStarted] = useState<StartedReview | null>(null);

  // The callbacks are held in a ref rather than named in the effect's dependency
  // list. A caller that passes an inline arrow — or leaves the prop off and takes
  // the default — hands this component a new function identity on every render,
  // and an effect that depended on it would load branches, set state, re-render,
  // and load branches again, forever. The effect depends on the repository,
  // because the repository is the only thing that changes what it should fetch.
  const load = useRef({ onLoadBranches, onLoadCommits });
  load.current = { onLoadBranches, onLoadCommits };

  // Reset the editor when the repository changes, so an unsaved draft cannot be
  // saved onto a different repository than the one it was typed against.
  useEffect(() => {
    setDraft(view?.config_json ?? '');
    setError(null);
    setSaved(false);
  }, [view?.repo.id, view?.config_json]);

  const repoId = view?.repo.id;
  const repoKind = view?.repo.kind;

  useEffect(() => {
    if (repoId === undefined || !repoKind || !reviewableByHand(repoKind)) return;
    const fetchBranches = load.current.onLoadBranches;
    const fetchCommits = load.current.onLoadCommits;
    if (!fetchBranches) return;

    // Nothing from the previous repository may stay on screen while the next
    // one loads: a branch list belonging to another repository is worse than no
    // branch list, because it can be clicked.
    let current = true;
    setBranches(null);
    setCommits(null);
    setStarted(null);
    setReviewError(null);

    fetchBranches(repoId)
      .then((loaded) => {
        if (!current) return;
        setBranches(loaded);
        const initial = loaded.current ?? loaded.branches[0]?.name ?? '';
        setBranch(initial);
        setBase(loaded.suggested_base ?? '');
        if (!initial || !fetchCommits) {
          setCommits([]);
          return;
        }
        return fetchCommits(repoId, initial).then((rows) => {
          if (current) setCommits(rows);
        });
      })
      .catch((error: unknown) => {
        if (current) setReviewError(messageOf(error));
      });

    return () => {
      current = false;
    };
  }, [repoId, repoKind]);

  if (!view) return <p className="empty">Loading the repository.</p>;

  const selectedRepoId = view.repo.id;

  async function save() {
    setSaved(false);
    try {
      await onSave(draft);
      setError(null);
      setSaved(true);
    } catch (e: unknown) {
      // Shown inline, beside the thing that is wrong — not in the app-wide
      // notice bar, where it would be a paragraph away from the text it is
      // about. The message carries a line and column, which is the half of an
      // error that makes it fixable.
      setError(typeof e === 'string' ? e : String((e as { message?: string })?.message ?? e));
      setSaved(false);
    }
  }

  const budget = view.budget;

  async function selectBranch(next: string) {
    setBranch(next);
    // A branch cannot be compared against itself, and the base list hides the
    // selected branch — leaving it set would show an empty base box while the
    // state still held a name.
    if (next === base) setBase('');
    setCommits(null);
    setReviewError(null);
    if (!onLoadCommits) {
      setCommits([]);
      return;
    }
    try {
      setCommits(await onLoadCommits(selectedRepoId, next));
    } catch (error: unknown) {
      setReviewError(messageOf(error));
    }
  }

  async function review(request: ReviewRequest) {
    if (!onReview) return;
    setStarting(true);
    setReviewError(null);
    setStarted(null);
    try {
      setStarted(await onReview(selectedRepoId, request));
    } catch (error: unknown) {
      setReviewError(messageOf(error));
    } finally {
      setStarting(false);
    }
  }

  return (
    <section className="repository">
      <header className="repo-head">
        <h2>{view.repo.repo}</h2>
        <span className="tag">{view.repo.kind}</span>
        <span className="tag">{view.repo.engine}</span>
        <span className={view.repo.enabled ? 'tag' : 'tag tag-off'}>
          {view.repo.enabled ? view.repo.autonomy : 'disabled'}
        </span>
        <span className="spacer" />
        {view.last_run ? (
          <button className="link" onClick={() => onOpenRun(view.last_run!.run_id)}>
            last run #{view.last_run.run_id} {view.last_run.status}
          </button>
        ) : (
          <span className="dim">no runs yet</span>
        )}
      </header>

      <section className="triggers-panel">
        <h3>Triggers</h3>
        <ul className="trigger-list" aria-label="triggers">
          {view.triggers.map((t) => (
            <Trigger key={t.trigger} status={t} />
          ))}
        </ul>
      </section>

      <Watched watching={view.watching} />

      {/* Rendered only when the screen was actually given the means to start a
          review. A panel that can list nothing and start nothing is worse than
          no panel: it sits there saying "Reading branches…" forever. */}
      {reviewableByHand(view.repo.kind) && onLoadBranches && onReview && (
        <ReviewNow
          branches={branches}
          branch={branch}
          base={base}
          commits={commits}
          starting={starting}
          started={started}
          error={reviewError}
          onBranch={selectBranch}
          onBase={setBase}
          onReview={review}
          onOpenRun={onOpenRun}
        />
      )}

      <section className="repo-budget">
        <h3>Today</h3>
        <p>
          {budget.runs} / {budget.runs_limit} runs ·{' '}
          {/* §18: a lower bound is not a total, and says so where it is shown. */}
          {budget.tokens_known ? '' : '≥ '}
          {budget.tokens.toLocaleString()} / {budget.tokens_limit.toLocaleString()} tokens
        </p>
        {!budget.tokens_known && (
          <p className="warn-text">
            A run today reported no token count, so this is a lower bound.
          </p>
        )}
      </section>

      <section className="recent">
        <h3>Recent runs</h3>
        {view.recent_runs.length === 0 ? (
          <p className="empty">Nothing has run for this repository yet.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Run</th>
                <th>Trigger</th>
                <th>Status</th>
                <th>Verdict</th>
                <th>Started</th>
              </tr>
            </thead>
            <tbody>
              {view.recent_runs.map((r) => (
                <tr key={r.run_id}>
                  <td>
                    <button className="link" onClick={() => onOpenRun(r.run_id)}>
                      #{r.run_id}
                    </button>
                  </td>
                  <td>{r.trigger}</td>
                  <td className={`run-status run-${toneOf(r.status)}`}>{r.status}</td>
                  <td>{r.verdict ?? '—'}</td>
                  <td className="dim" title={exactly(r.started_at)}>
                    {r.started_at ? ago(r.started_at) : 'not started'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {view.more_runs && (
          // §18: "the last ten" and "all ten there have ever been" must not look
          // the same.
          <p className="dim">Older runs exist — this is the most recent {view.recent_runs.length}.</p>
        )}
      </section>

      <FilingConfig
        draft={draft}
        onChange={(next) => {
          setDraft(next);
          setError(null);
          setSaved(false);
        }}
      />

      <section className="config">
        <h3>Configuration</h3>
        <p className="dim">
          §13.2 as rev-local reads it. It is validated before anything is stored, so a
          config that will not parse cannot be saved.
        </p>
        <textarea
          className="config-editor mono"
          aria-label="repository configuration"
          spellCheck={false}
          rows={16}
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            // The old error belonged to the old text. Leaving it up would have
            // somebody reading a line and column that no longer point anywhere.
            setError(null);
            setSaved(false);
          }}
        />
        {error && (
          <p className="config-error" role="alert">
            {error}
          </p>
        )}
        {saved && <p className="config-saved">Saved.</p>}
        <div className="queued-actions">
          <button onClick={save} disabled={draft === view.config_json}>
            Save configuration
          </button>
          <button
            className="link"
            disabled={draft === view.config_json}
            onClick={() => {
              setDraft(view.config_json);
              setError(null);
              setSaved(false);
            }}
          >
            revert
          </button>
        </div>
      </section>
    </section>
  );
}
