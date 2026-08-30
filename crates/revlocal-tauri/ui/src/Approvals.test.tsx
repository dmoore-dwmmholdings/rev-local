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
    has_finding: true,
    ...overrides,
  };
}

const noop = vi.fn();

describe('approvals', () => {
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
