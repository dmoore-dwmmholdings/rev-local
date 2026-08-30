import { useEffect, useState } from 'react';
import type { RepositoryView, TriggerStatus, Watching } from './ipc';

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

export function Repository({
  view,
  onOpenRun,
  onSave,
}: {
  view: RepositoryView | null;
  onOpenRun: (runId: number) => void;
  /** Resolves when saved; rejects with the validation error to show inline. */
  onSave: (configJson: string) => Promise<void>;
}) {
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Reset the editor when the repository changes, so an unsaved draft cannot be
  // saved onto a different repository than the one it was typed against.
  useEffect(() => {
    setDraft(view?.config_json ?? '');
    setError(null);
    setSaved(false);
  }, [view?.repo.id, view?.config_json]);

  if (!view) return <p className="empty">Loading the repository.</p>;

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
                  <td>{r.status}</td>
                  <td>{r.verdict ?? '—'}</td>
                  <td className="dim">{r.started_at ?? '—'}</td>
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
