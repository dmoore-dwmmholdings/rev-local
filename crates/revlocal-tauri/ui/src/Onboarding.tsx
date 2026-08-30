import {
  ENGINES,
  ENGINE_LABELS,
  MODES,
  MODE_LABELS,
  STEPS,
  STEP_TITLES,
  allChecks,
  type Draft,
  type DoctorReport,
  type FirstReview,
  type Step,
} from './ipc';

/**
 * First-run onboarding (RL-1205, §15).
 *
 * Doctor first, because §8.4 says its output "is the first thing the UI shows on
 * a fresh install" — somebody with no engine installed should learn that before
 * being asked to choose one. A failing check does **not** block the walk: the
 * mock engine works on any machine, and stopping somebody at step one because
 * they have not installed Claude Code yet would teach them nothing about whether
 * rev-local works.
 *
 * The walk ends at a review whose result is on screen, not at a configured
 * repository. Somebody who has added a repository and seen nothing does not yet
 * know whether any of it works.
 */

export function Onboarding({
  step,
  draft,
  doctor,
  review,
  busy,
  error,
  onDraft,
  onBack,
  onNext,
  onFinish,
}: {
  step: Step;
  draft: Draft;
  doctor: DoctorReport | null;
  review: FirstReview | null;
  busy: boolean;
  error: string | null;
  onDraft: (next: Draft) => void;
  onBack: () => void;
  onNext: () => void;
  onFinish: () => void;
}) {
  const index = STEPS.indexOf(step);
  const blocked = step === 'add_repo' && draft.path.trim() === '';

  return (
    <section className="onboarding">
      <ol className="steps" aria-label="steps">
        {STEPS.map((s, i) => (
          <li
            key={s}
            className={i === index ? 'step step-current' : i < index ? 'step step-done' : 'step'}
            aria-current={s === step ? 'step' : undefined}
          >
            {STEP_TITLES[s]}
          </li>
        ))}
      </ol>

      {error && (
        <p className="config-error" role="alert">
          {error}
        </p>
      )}

      {step === 'check' && (
        <div className="step-body">
          <p className="dim">
            What rev-local found on this machine. Nothing here has to pass — the mock
            engine works anywhere, and you can install a real one later.
          </p>
          {!doctor ? (
            <p className="empty">Checking…</p>
          ) : (
            <ul className="checks">
              {allChecks(doctor).map((c) => (
                <li key={c.name} className={`check check-${c.health}`}>
                  <span className="tag">{c.health === 'not_needed' ? 'n/a' : c.health}</span>
                  <strong>{c.name}</strong>
                  <span className="dim">{c.detail}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {step === 'add_repo' && (
        <div className="step-body">
          <label className="filter">
            Repository path
            <input
              className="mono"
              aria-label="repository path"
              value={draft.path}
              placeholder="/home/you/projects/acme"
              onChange={(e) => onDraft({ ...draft, path: e.target.value })}
            />
          </label>
          <label className="filter">
            Name
            <input
              aria-label="repository name"
              value={draft.name}
              placeholder="derived from the path"
              onChange={(e) => onDraft({ ...draft, name: e.target.value })}
            />
          </label>
          {/* Said, not just disabled: a control that is simply dead teaches
              nothing about why. */}
          {blocked && <p className="dim">Choose a repository directory or URL to continue.</p>}
        </div>
      )}

      {step === 'pick_engine' && (
        <div className="step-body">
          <p className="dim">
            Which engine reviews this repository. This is per repository, not global — a
            noisy repository can use a cheaper engine than one that matters.
          </p>
          <label className="filter">
            Engine
            <select
              aria-label="engine"
              value={draft.engine}
              onChange={(e) => onDraft({ ...draft, engine: e.target.value })}
            >
              {ENGINES.map((e) => (
                <option key={e} value={e}>
                  {ENGINE_LABELS[e]}
                </option>
              ))}
            </select>
          </label>
        </div>
      )}

      {step === 'pick_autonomy' && (
        <div className="step-body">
          <p className="dim">
            How much rev-local may do without asking. It starts at dry run: it reviews
            and publishes nothing, so you can see what it produces before it reaches
            anybody else.
          </p>
          <label className="filter">
            Autonomy
            <select
              aria-label="autonomy"
              value={draft.autonomy}
              onChange={(e) => onDraft({ ...draft, autonomy: e.target.value })}
            >
              {MODES.map((m) => (
                <option key={m} value={m}>
                  {MODE_LABELS[m]}
                </option>
              ))}
            </select>
          </label>
          {draft.autonomy === 'auto' && (
            // Allowed, and worth a sentence: this is the only mode that writes to
            // somebody else's systems unattended, on a repository nobody has seen
            // a finding from yet.
            <p className="warn-text">
              rev-local will publish findings from this repository without asking, before
              you have seen any of them.
            </p>
          )}
        </div>
      )}

      {step === 'first_review' && (
        <div className="step-body">
          {!review ? (
            <p className="empty">
              {busy ? 'Reviewing one change…' : 'Ready to review one change.'}
            </p>
          ) : (
            <>
              <p>
                Reviewed <strong>{review.repo}</strong> — {review.verdict ?? 'no verdict'},{' '}
                {review.findings} finding{review.findings === 1 ? '' : 's'} (run #{review.run_id}).
              </p>
              {review.caveat && (
                // §18. A rehearsal that reads like a real review is the worst
                // possible first impression: everything after it gets judged
                // against findings that were invented.
                <p className="warn-text">{review.caveat}</p>
              )}
            </>
          )}
        </div>
      )}

      <div className="queued-actions">
        <button onClick={onBack} disabled={index === 0 || busy}>
          Back
        </button>
        {step === 'first_review' ? (
          review ? (
            <button onClick={onFinish}>Done</button>
          ) : (
            <button onClick={onNext} disabled={busy}>
              {busy ? 'Reviewing…' : 'Review one change'}
            </button>
          )
        ) : (
          <button onClick={onNext} disabled={blocked || busy}>
            {busy ? 'Working…' : 'Continue'}
          </button>
        )}
      </div>
    </section>
  );
}
