import { useState } from 'react';
import type { ApprovalsView, QueuedAction } from './ipc';

/**
 * What a payload says, read out of the payload itself (§12.4).
 *
 * There is no second renderer. Dispatch sends `payload_json` verbatim, and this
 * reads fields out of that same string — so a preview cannot disagree with what
 * is sent, because there is nothing for it to disagree with. A UI that
 * *reproduced* the target's rendering would be a second implementation, and the
 * one somebody approved against would be the one that never runs.
 *
 * Arrangement is all this adds: which field reads as a title, which as a body.
 * The raw payload sits underneath so anybody can check.
 */
function preview(payloadJson: string): { title?: string; body?: string } {
  try {
    const p = JSON.parse(payloadJson) as Record<string, unknown>;
    const str = (k: string) => (typeof p[k] === 'string' ? (p[k] as string) : undefined);
    return {
      title: str('title') ?? str('summary') ?? str('subject'),
      body: str('body') ?? str('body_md') ?? str('text') ?? str('comment'),
    };
  } catch {
    // A payload that will not parse is still the payload that would be sent.
    // Showing nothing here is right — the raw block below shows it whole.
    return {};
  }
}

function Action({
  action,
  onApprove,
  onReject,
  onEdit,
}: {
  action: QueuedAction;
  onApprove: (a: QueuedAction) => void;
  onReject: (a: QueuedAction, suppress: boolean) => void;
  onEdit: (a: QueuedAction) => void;
}) {
  const [showRaw, setShowRaw] = useState(false);
  const { title, body } = preview(action.payload_json);

  return (
    <li className="queued">
      <header className="queued-head">
        {/* §15: every outbound action names its target explicitly. The heading
            is the naming — not a tooltip, not the confirmation alone. */}
        <strong>
          {action.capability} → {action.target}
        </strong>
        <span className="tag">{action.risk} risk</span>
        <span className="spacer" />
        <span className="dim">run #{action.run_id}</span>
      </header>

      <div className="preview">
        {title && <p className="preview-title">{title}</p>}
        {body && <p className="preview-body">{body}</p>}
        {!title && !body && (
          <p className="dim">This payload has no title or body field; see the raw payload.</p>
        )}
      </div>

      <button className="link" onClick={() => setShowRaw(!showRaw)}>
        {showRaw ? 'hide' : 'show'} the exact payload
      </button>
      {showRaw && <pre className="payload">{action.payload_json}</pre>}

      <div className="queued-actions">
        <button onClick={() => onApprove(action)}>Approve</button>
        <button onClick={() => onReject(action, false)}>Reject</button>
        {/* Disabled rather than hidden where there is no finding: a suppression
            with nothing to suppress is a row that can never match anything. */}
        <button
          disabled={!action.has_finding}
          title={
            action.has_finding
              ? 'Reject, and stop proposing this finding'
              : 'this action carries no finding to suppress'
          }
          onClick={() => onReject(action, true)}
        >
          Reject &amp; suppress
        </button>
        <button onClick={() => onEdit(action)}>Edit body…</button>
      </div>
    </li>
  );
}

/** §15 screen 5. */
export function Approvals({
  view,
  onApprove,
  onApproveRun,
  onReject,
  onEdit,
}: {
  view: ApprovalsView | null;
  onApprove: (a: QueuedAction) => void;
  onApproveRun: (runId: number, count: number) => void;
  onReject: (a: QueuedAction, suppress: boolean) => void;
  onEdit: (a: QueuedAction) => void;
}) {
  if (!view) return <p className="empty">Loading the inbox.</p>;

  if (view.waiting.length === 0) {
    // Said explicitly. An empty list rendered as nothing is indistinguishable
    // from a screen that failed to load.
    return <p className="empty">Nothing is waiting for approval.</p>;
  }

  // Grouped by run, because "approve all for this run" needs a visible scope —
  // a button whose blast radius is not on screen is a button pressed blind.
  const runs: number[] = [];
  for (const a of view.waiting) if (!runs.includes(a.run_id)) runs.push(a.run_id);

  return (
    <section className="approvals">
      {runs.map((runId) => {
        const forRun = view.waiting.filter((a) => a.run_id === runId);
        return (
          <div key={runId} className="run-group">
            <header className="run-group-head">
              <h3>run #{runId}</h3>
              <span className="dim">
                {forRun.length} action{forRun.length === 1 ? '' : 's'} waiting
              </span>
              <span className="spacer" />
              <button onClick={() => onApproveRun(runId, forRun.length)}>
                Approve all {forRun.length} for this run
              </button>
            </header>

            <ul className="queued-list">
              {forRun.map((a) => (
                <Action
                  key={a.id}
                  action={a}
                  onApprove={onApprove}
                  onReject={onReject}
                  onEdit={onEdit}
                />
              ))}
            </ul>
          </div>
        );
      })}
    </section>
  );
}
