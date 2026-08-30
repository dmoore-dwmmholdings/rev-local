import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { RunDetail } from './RunDetail';
import type { RunView } from './ipc';

function run(overrides: Partial<RunView> = {}): RunView {
  return {
    run_id: 12,
    change: 'abc1234',
    status: 'done',
    engine: 'claude',
    depth: 'standard',
    verdict: 'request_changes',
    tokens: 189_000,
    tokens_known: true,
    stages: {
      started_at: '2026-08-30T01:00:00Z',
      finished_at: '2026-08-30T01:04:00Z',
      elapsed_secs: 240,
      per_stage_unavailable: 'stage transitions are not recorded, so only start and end are known',
    },
    truncated: false,
    omitted_files: [],
    findings: [
      {
        id: 1,
        severity: 'high',
        category: 'security',
        title: 'SQL injection in find_user',
        file: 'src/db.rs',
        line_start: 4,
        anchorable: true,
      },
    ],
    transcript_bytes: 2_400_000,
    targets: [
      { target: 'github', sent: 3, pending: 0, awaiting_approval: 0, failed: 0, retryable: false },
      { target: 'andare', sent: 0, pending: 0, awaiting_approval: 0, failed: 1, retryable: true },
    ],
    ...overrides,
  };
}

const noop = vi.fn();

describe('run detail', () => {
  it('anchors a finding to its file and line', () => {
    render(<RunDetail run={run()} transcript={null} onExpandTranscript={noop} onRetry={noop} />);

    expect(screen.getByText('src/db.rs:4')).toBeDefined();
  });

  it('shows an unanchorable finding rather than dropping it', () => {
    // §18. A review that found something outside the changed lines has still
    // found something; omitting it would hide a result.
    render(
      <RunDetail
        run={run({
          findings: [
            {
              id: 2,
              severity: 'medium',
              category: 'correctness',
              title: 'something with no file',
              anchorable: false,
            },
          ],
        })}
        transcript={null}
        onExpandTranscript={noop}
        onRetry={noop}
      />,
    );

    expect(screen.getByText('something with no file')).toBeDefined();
    expect(screen.getByText(/not anchored/)).toBeDefined();
  });

  it('names the files a truncated review did not read', () => {
    // "58 files omitted" cannot be checked by the person reading it; a list can.
    render(
      <RunDetail
        run={run({ truncated: true, omitted_files: ['src/huge.rs', 'vendor/blob.rs'] })}
        transcript={null}
        onExpandTranscript={noop}
        onRetry={noop}
      />,
    );

    expect(screen.getByRole('status').textContent).toMatch(/not the whole picture/);
    expect(screen.getByText('src/huge.rs')).toBeDefined();
    expect(screen.getByText('vendor/blob.rs')).toBeDefined();
  });

  it('lists the three caveats separately, because they have different fixes', () => {
    const { container } = render(
      <RunDetail
        run={run({
          truncated: true,
          omitted_files: ['a.rs'],
          tokens_known: false,
          degraded: 'repaired once',
        })}
        transcript={null}
        onExpandTranscript={noop}
        onRetry={noop}
      />,
    );

    const items = container.querySelectorAll('.caveats > ul > li');
    expect(items.length).toBe(3);
  });

  it('says nothing is wrong when nothing is', () => {
    // Hedging a clean run would train people to ignore the banner.
    render(<RunDetail run={run()} transcript={null} onExpandTranscript={noop} onRetry={noop} />);

    expect(screen.queryByText(/not the whole picture/)).toBeNull();
  });

  it('enables retry only for a target that failed', () => {
    render(<RunDetail run={run()} transcript={null} onExpandTranscript={noop} onRetry={noop} />);

    const buttons = screen.getAllByText('retry');
    // github sent 3 and failed 0; andare failed 1.
    expect((buttons[0] as HTMLButtonElement).disabled).toBe(true);
    expect((buttons[1] as HTMLButtonElement).disabled).toBe(false);
  });

  it('does not fetch the transcript until it is expanded', () => {
    // The property that makes a megabyte log survivable. Collapsed-in-the-DOM is
    // not enough: by then it has already crossed the boundary.
    const onExpand = vi.fn();
    render(
      <RunDetail run={run()} transcript={null} onExpandTranscript={onExpand} onRetry={noop} />,
    );

    expect(onExpand).not.toHaveBeenCalled();
    expect(screen.getByText(/show transcript \(2\.3 MB\)/)).toBeDefined();

    screen.getByText(/show transcript/).click();
    expect(onExpand).toHaveBeenCalledOnce();
  });

  it('says why there is no per-stage timeline', () => {
    // §15 asks for one. Start and end with no explanation looks like a timeline
    // that failed to load.
    render(<RunDetail run={run()} transcript={null} onExpandTranscript={noop} onRetry={noop} />);

    expect(screen.getByText(/stage transitions are not recorded/)).toBeDefined();
  });

  it('distinguishes a run with no findings from one that failed to load', () => {
    render(
      <RunDetail run={run({ findings: [] })} transcript={null} onExpandTranscript={noop} onRetry={noop} />,
    );

    expect(screen.getByText(/ran and reported nothing/)).toBeDefined();
  });
});
