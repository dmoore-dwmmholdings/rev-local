import { useState } from 'react';
import type { AnchoredFinding, RunView, TargetLine } from './ipc';

/** Bytes, in a form somebody can judge "is this worth expanding" from. */
function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Everything that makes this run's output less than the whole truth (§18).
 *
 * Three separate causes with three different remedies, listed separately rather
 * than collapsed into one "degraded" badge: a truncated diff, an uncounted token
 * total and salvaged output are not the same problem and do not have the same
 * fix. A single banner would tell somebody something is wrong without telling
 * them which thing.
 */
function Caveats({ run }: { run: RunView }) {
  const caveats: string[] = [];

  if (run.truncated) {
    caveats.push(
      `the diff was reduced, so ${run.omitted_files.length} file(s) were not reviewed`,
    );
  }
  if (!run.tokens_known) {
    caveats.push('the token count is a lower bound — something did not report its usage');
  }
  if (run.degraded) {
    caveats.push(`the output was salvaged rather than parsed cleanly: ${run.degraded}`);
  }

  if (caveats.length === 0) return null;

  return (
    <div className="caveats" role="status">
      <strong>This review is not the whole picture.</strong>
      <ul>
        {caveats.map((c) => (
          <li key={c}>{c}</li>
        ))}
      </ul>
      {/* Names, not a count. §18's point about truncation is that "58 files
          omitted" cannot be checked by the person reading it, and a list can. */}
      {run.truncated && run.omitted_files.length > 0 && (
        <details>
          <summary>files not reviewed ({run.omitted_files.length})</summary>
          <ul className="omitted">
            {run.omitted_files.map((f) => (
              <li key={f}>{f}</li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

/** One finding, with where it sits — or why it has nowhere to sit. */
function Finding({ finding }: { finding: AnchoredFinding }) {
  const where = finding.anchorable
    ? `${finding.file}:${finding.line_start}${
        finding.line_end && finding.line_end !== finding.line_start ? `–${finding.line_end}` : ''
      }`
    : null;

  return (
    <li className={`finding sev-${finding.severity}`}>
      <span className="sev">{finding.severity}</span>
      <span className="finding-title">{finding.title}</span>
      {where ? (
        <span className="anchor">{where}</span>
      ) : (
        // Shown, never dropped. A review that found something outside the changed
        // lines has still found something, and omitting it would hide a result.
        <span className="anchor anchor-none" title="this finding names no line in the diff">
          not anchored to the diff
        </span>
      )}
    </li>
  );
}

/** Per-target publish status, with retry where a retry means something. */
function Targets({
  targets,
  onRetry,
}: {
  targets: TargetLine[];
  onRetry: (target: string) => void;
}) {
  if (targets.length === 0) {
    // Said explicitly: an empty section reads as one that failed to load.
    return <p className="empty">Nothing was published for this run.</p>;
  }

  return (
    <table className="targets">
      <thead>
        <tr>
          <th>target</th>
          <th>sent</th>
          <th>pending</th>
          <th>awaiting</th>
          <th>failed</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {targets.map((t) => (
          <tr key={t.target}>
            <td>{t.target}</td>
            <td>{t.sent}</td>
            <td>{t.pending}</td>
            <td>{t.awaiting_approval}</td>
            <td className={t.failed > 0 ? 'bad' : ''}>{t.failed}</td>
            <td>
              {/* Disabled rather than hidden. A button that vanishes leaves
                  somebody wondering whether they misremember; a visibly disabled
                  one says "nothing to retry here". */}
              <button
                disabled={!t.retryable}
                onClick={() => onRetry(t.target)}
                title={
                  t.retryable
                    ? `Re-queue ${t.failed} failed action(s) for ${t.target}`
                    : 'nothing failed for this target'
                }
              >
                retry
              </button>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** §15 screen 3. */
export function RunDetail({
  run,
  transcript,
  onExpandTranscript,
  onRetry,
}: {
  run: RunView | null;
  transcript: string | null;
  onExpandTranscript: () => void;
  onRetry: (target: string) => void;
}) {
  const [showTranscript, setShowTranscript] = useState(false);

  if (!run) return <p className="empty">Select a run.</p>;

  return (
    <section className="run-detail">
      <header className="run-head">
        <h2>
          run #{run.run_id} · {run.change}
        </h2>
        <span className="tag">{run.status}</span>
        {run.verdict && <span className="tag">{run.verdict}</span>}
        <span className="spacer" />
        <span className="dim">
          {run.engine} · {run.depth} · {run.tokens_known ? '' : '≥ '}
          {run.tokens.toLocaleString()} tokens
        </span>
      </header>

      <Caveats run={run} />

      <dl className="card-facts">
        <dt>started</dt>
        <dd>{run.stages.started_at ?? 'not recorded'}</dd>
        <dt>finished</dt>
        <dd>{run.stages.finished_at ?? 'still running'}</dd>
        <dt>elapsed</dt>
        <dd>{run.stages.elapsed_secs !== undefined ? `${run.stages.elapsed_secs}s` : '—'}</dd>
      </dl>
      {/* §15 asks for a per-stage timeline. Nothing records the transitions, and
          saying so beats drawing one bar and calling it a timeline. */}
      <p className="hedge">{run.stages.per_stage_unavailable}</p>

      <h3>findings ({run.findings.length})</h3>
      {run.findings.length === 0 ? (
        <p className="empty">No findings. The review ran and reported nothing.</p>
      ) : (
        <ul className="findings">
          {run.findings.map((f) => (
            <Finding key={f.id} finding={f} />
          ))}
        </ul>
      )}

      <h3>publishing</h3>
      <Targets targets={run.targets} onRetry={onRetry} />

      <h3>transcript</h3>
      {/* Collapsed by default AND not fetched until expanded. The second is what
          matters: a megabyte already in the payload is a megabyte the screen
          waited for, whatever the DOM does with it afterwards. */}
      {!showTranscript ? (
        <button
          onClick={() => {
            setShowTranscript(true);
            onExpandTranscript();
          }}
        >
          show transcript ({humanBytes(run.transcript_bytes)})
        </button>
      ) : (
        <>
          <button onClick={() => setShowTranscript(false)}>hide transcript</button>
          <pre className="transcript">{transcript ?? 'Loading the transcript…'}</pre>
        </>
      )}
    </section>
  );
}
