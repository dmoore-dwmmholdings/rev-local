import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Approvals } from './Approvals';
import type { QueuedAction } from './ipc';

function action(overrides: Partial<QueuedAction> = {}): QueuedAction {
  return {
    id: 1,
    run_id: 10,
    target: 'github',
    capability: 'post_review',
    risk: 'high',
    payload_json: JSON.stringify({
      title: 'SQL injection in find_user',
      body: 'name is interpolated straight into the query.',
    }),
    held_by: {
      reason: 'high risk, and the global autonomy is `auto_low_ask_high`',
      remedy: 'set the global mode to `auto`',
    },
    has_finding: true,
    ...overrides,
  };
}

const noop = vi.fn();

describe('approvals', () => {
  it('says which setting is holding an item, not only how risky it is', () => {
    // REVL-199. The class on its own is a label. A repository already set to
    // `autonomy = auto` sat here with its issues waiting, and nothing on the
    // screen said the *global* half of `min(global, repo)` was the one that
    // applied — so the reasonable conclusion was that the setting had not taken.
    render(
      <Approvals
        view={{ waiting: [action()] }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    expect(screen.getByText(/global autonomy/)).toBeTruthy();
    expect(screen.getByText(/set the global mode/)).toBeTruthy();
  });

  it('shows how long an item has left, and marks it when nearly up', () => {
    // RL-1541, found on the live install: three real findings had waited 70 of
    // their 72 hours and the inbox said nothing. Two hours later the daemon
    // discards them, and nothing on screen gave anybody a reason to hurry.
    render(
      <Approvals
        view={{
          waiting: [
            action({ id: 1, deadline: '2h left' }),
            action({ id: 2, run_id: 11, deadline: 'under an hour left' }),
            action({ id: 3, run_id: 12, deadline: null }),
          ],
        }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    expect(screen.getByText('2h left')).toBeDefined();
    // The urgent one is marked, not merely listed — the whole failure was that
    // a doomed item looked like every other row.
    const soon = screen.getByText('under an hour left');
    expect(soon.className).toContain('tag-off');
    expect(screen.getByText('2h left').className).not.toContain('tag-off');
  });

  it('renders the preview out of the payload that would be sent', () => {
    // Criterion 1, and the reason there is no second renderer: dispatch sends
    // `payload_json` verbatim and this reads fields out of that same string, so
    // there is nothing for a preview to disagree with.
    const a = action();
    render(
      <Approvals
        view={{ waiting: [a] }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    const payload = JSON.parse(a.payload_json) as { title: string; body: string };
    expect(screen.getByText(payload.title)).toBeDefined();
    expect(screen.getByText(payload.body)).toBeDefined();
  });

  it('offers the exact payload for checking', () => {
    // Arrangement is all the preview adds. The payload is available whole so
    // nobody has to trust the arrangement.
    const a = action();
    render(
      <Approvals view={{ waiting: [a] }} onApprove={noop} onApproveRun={noop} onReject={noop} onEdit={noop} />,
    );

    // `fireEvent` rather than `.click()`: a bare DOM click fires the handler
    // but leaves React's state update unflushed, so the assertion below would
    // read the pre-click DOM and fail for a reason that is not the subject.
    fireEvent.click(screen.getByText(/show the exact payload/));
    expect(screen.getByText(a.payload_json)).toBeDefined();
  });

  it('names the target in the heading, not only in a confirmation', () => {
    // §15: every outbound action names its target explicitly. Somebody scanning
    // the inbox must see where each thing goes without pressing anything.
    render(
      <Approvals view={{ waiting: [action()] }} onApprove={noop} onApproveRun={noop} onReject={noop} onEdit={noop} />,
    );

    expect(screen.getByText(/post_review → github/)).toBeDefined();
  });

  it('shows approve-all with its count, scoped to one run', () => {
    // A button whose blast radius is not on screen is a button pressed blind.
    render(
      <Approvals
        view={{ waiting: [action({ id: 1 }), action({ id: 2, target: 'andare' }), action({ id: 3, run_id: 11 })] }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    expect(screen.getByText('Approve all 2 for this run')).toBeDefined();
    expect(screen.getByText('Approve all 1 for this run')).toBeDefined();
  });

  it('passes the count to the caller so the confirmation can name it', () => {
    const onApproveRun = vi.fn();
    render(
      <Approvals
        view={{ waiting: [action({ id: 1 }), action({ id: 2 })] }}
        onApprove={noop}
        onApproveRun={onApproveRun}
        onReject={noop}
        onEdit={noop}
      />,
    );

    screen.getByText('Approve all 2 for this run').click();
    expect(onApproveRun).toHaveBeenCalledWith(10, 2);
  });

  it('disables suppress where there is no finding to suppress', () => {
    // A suppression with no fingerprint and no glob can never match anything.
    render(
      <Approvals
        view={{ waiting: [action({ has_finding: false })] }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    const button = screen.getByText(/Reject & suppress/) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  it('still shows a payload that will not parse', () => {
    // It is still the payload that would be sent. Hiding it because the preview
    // cannot arrange it would hide the thing being approved.
    render(
      <Approvals
        view={{ waiting: [action({ payload_json: 'not json at all' })] }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    expect(screen.getByText(/no title or body field/)).toBeDefined();
    // `fireEvent` rather than `.click()`: a bare DOM click fires the handler
    // but leaves React's state update unflushed, so the assertion below would
    // read the pre-click DOM and fail for a reason that is not the subject.
    fireEvent.click(screen.getByText(/show the exact payload/));
    expect(screen.getByText('not json at all')).toBeDefined();
  });

  it('says the inbox is empty rather than rendering nothing', () => {
    render(
      <Approvals view={{ waiting: [] }} onApprove={noop} onApproveRun={noop} onReject={noop} onEdit={noop} />,
    );

    expect(screen.getByText(/Nothing is waiting for approval/)).toBeDefined();
  });
});

describe('an action that cannot be sent', () => {
  it('refuses the approval and says why', () => {
    // The payload is a string, and dispatch is where it used to be parsed first.
    // Approving one of these marked it failed and did nothing else (RL-1516).
    render(
      <Approvals
        view={{
          waiting: [
            action({
              unsendable: 'this cannot be sent as it stands — it is not an Andare issue',
            }),
          ],
        }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    expect(screen.getByRole('alert').textContent).toMatch(/cannot be sent/);
    expect(screen.getByRole('button', { name: 'Approve' }).hasAttribute('disabled')).toBe(true);
  });

  it('leaves the two ways out available', () => {
    // Reject and Edit are how somebody resolves this. Disabling them would leave
    // the action stuck in the inbox with no move at all.
    render(
      <Approvals
        view={{ waiting: [action({ unsendable: 'nope' })] }}
        onApprove={noop}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    expect(screen.getByRole('button', { name: 'Reject' }).hasAttribute('disabled')).toBe(false);
    expect(screen.getByRole('button', { name: /Edit body/ }).hasAttribute('disabled')).toBe(false);
  });

  it('approves normally when the payload is fine', () => {
    const onApprove = vi.fn();
    render(
      <Approvals
        view={{ waiting: [action()] }}
        onApprove={onApprove}
        onApproveRun={noop}
        onReject={noop}
        onEdit={noop}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
    expect(onApprove).toHaveBeenCalled();
  });
});
