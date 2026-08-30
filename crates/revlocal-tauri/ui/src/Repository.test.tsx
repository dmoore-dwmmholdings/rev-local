import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { Repository } from './Repository';
import type { RepositoryView, TriggerStatus } from './ipc';

function trigger(
  name: string,
  state: TriggerStatus['state'],
  detail = 'because',
): TriggerStatus {
  return { trigger: name, state, detail };
}

function view(overrides: Partial<RepositoryView> = {}): RepositoryView {
  return {
    repo: {
      id: 1,
      repo: 'acme',
      kind: 'git',
      engine: 'claude',
      autonomy: 'dry_run',
      enabled: true,
      health: {
        repo: 'acme',
        health: 'healthy',
        poll_interval_secs: 300,
        next_poll_in_secs: 120,
        consecutive_failures: 0,
        last_error: null,
        notes: [],
      },
    },
    watching: { kind: 'branches', globs: ['main', 'release/*'] },
    triggers: [
      trigger('poll', 'active', 'every 300s; next in about 120s'),
      trigger('hooks', 'off', 'no rev-local hook is installed'),
      trigger('webhook', 'broken', 'enabled with no secret'),
      trigger('manual', 'active', 'always available'),
    ],
    recent_runs: [
      { run_id: 10, status: 'done', verdict: 'approve', trigger: 'poll', started_at: '2026-01-01T01:00:00Z' },
    ],
    more_runs: false,
    budget: { runs: 3, runs_limit: 200, tokens: 1000, tokens_limit: 2000000, tokens_known: true },
    config_json: '{\n  "poll_interval_secs": 300\n}',
    last_run: { run_id: 10, status: 'done', verdict: 'approve' },
    ...overrides,
  };
}

const noop = vi.fn();
const saveOk = () => Promise.resolve();

function mount(v: RepositoryView | null, props: Partial<Parameters<typeof Repository>[0]> = {}) {
  return render(<Repository view={v} onOpenRun={noop} onSave={saveOk} {...props} />);
}

describe('repository', () => {
  it('shows all four triggers with independent states', () => {
    // The acceptance criterion. One rolled-up light would show amber for a
    // repository whose hooks are dead and whose polling is fine, and send
    // somebody looking in the wrong place.
    mount(view());

    // Scoped to the trigger list: "poll" is also a value in the recent-runs
    // table, and a bare text query would pass on the wrong element.
    const list = screen.getByLabelText('triggers');
    for (const name of ['poll', 'hooks', 'webhook', 'manual']) {
      expect(within(list).getByText(name)).toBeTruthy();
    }
    expect(within(list).getAllByRole('listitem')).toHaveLength(4);
    // A broken webhook has not dimmed the poller.
    expect(screen.getByText(/every 300s/)).toBeTruthy();
    expect(screen.getByText(/enabled with no secret/)).toBeTruthy();
  });

  it('carries the reason beside every indicator', () => {
    // An unexplained light gets guessed at, and the guess is usually "fine".
    mount(view({ triggers: [trigger('poll', 'broken', 'connection refused')] }));

    expect(screen.getByText(/connection refused/)).toBeTruthy();
  });

  it('renders SVN as watched paths, not branches', () => {
    // Criterion 3. An SVN "branch" is a directory; calling it a branch is where
    // the confusion starts, and a branch heading suggests a filter that is not
    // being applied.
    mount(
      view({
        repo: { ...view().repo, kind: 'svn' },
        watching: { kind: 'paths', paths: ['trunk', 'branches/*'] },
        triggers: [
          trigger('poll', 'active'),
          trigger('hooks', 'not_applicable', 'SVN hooks run on the server'),
          trigger('webhook', 'not_applicable', 'an SVN repository has none'),
          trigger('manual', 'active'),
        ],
      }),
    );

    expect(screen.getByText(/watched paths/i)).toBeTruthy();
    expect(screen.queryByText(/watched branches/i)).toBeNull();
    expect(screen.getByText('trunk')).toBeTruthy();
    expect(screen.getAllByText(/n\/a/).length).toBe(2);
  });

  it('shows a validation error inline and does not clear the draft', async () => {
    // Criterion 2. Inline beside the text it is about — an app-wide notice puts
    // a line and column a paragraph away from the line and column. And the bad
    // text stays, because retyping it from memory is not a fix.
    const onSave = vi.fn().mockRejectedValue('line 1, column 24: invalid type: string');
    mount(view(), { onSave });

    const editor = screen.getByLabelText(/repository configuration/i);
    fireEvent.change(editor, { target: { value: '{"poll_interval_secs": "soon"}' } });
    fireEvent.click(screen.getByRole('button', { name: /save configuration/i }));

    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
    expect(screen.getByRole('alert').textContent).toContain('line 1, column 24');
    expect((editor as HTMLTextAreaElement).value).toBe('{"poll_interval_secs": "soon"}');
  });

  it('clears a stale error as soon as the text changes', async () => {
    // A line and column that no longer point anywhere is worse than no error.
    const onSave = vi.fn().mockRejectedValue('line 1, column 24: bad');
    mount(view(), { onSave });

    const editor = screen.getByLabelText(/repository configuration/i);
    fireEvent.change(editor, { target: { value: 'nonsense' } });
    fireEvent.click(screen.getByRole('button', { name: /save configuration/i }));
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());

    fireEvent.change(editor, { target: { value: '{}' } });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('cannot save an unchanged config', () => {
    // Saving what is already stored is a write with no purpose, and an enabled
    // button says otherwise.
    mount(view());

    const save = screen.getByRole('button', { name: /save configuration/i });
    expect((save as HTMLButtonElement).disabled).toBe(true);
  });

  it('confirms a save rather than leaving the screen unchanged', async () => {
    mount(view());

    const editor = screen.getByLabelText(/repository configuration/i);
    fireEvent.change(editor, { target: { value: '{"poll_interval_secs": 900}' } });
    fireEvent.click(screen.getByRole('button', { name: /save configuration/i }));

    await waitFor(() => expect(screen.getByText(/^saved\.$/i)).toBeTruthy());
  });

  it('says an empty branch list matches nothing', () => {
    // A real configuration and a surprising one. An empty list rendered as
    // nothing reads as a section that failed to load.
    mount(view({ watching: { kind: 'branches', globs: [] } }));

    expect(screen.getByText(/matches/i)).toBeTruthy();
  });

  it('says when older runs exist', () => {
    // §18: "the last ten" and "all ten there have ever been" must not look alike.
    mount(view({ more_runs: true }));

    expect(screen.getByText(/older runs exist/i)).toBeTruthy();
  });

  it('hedges a token figure that is only a lower bound', () => {
    mount(view({ budget: { ...view().budget, tokens_known: false } }));

    expect(screen.getByText(/lower bound/i)).toBeTruthy();
  });

  it('jumps to a run from the recent list', () => {
    const onOpenRun = vi.fn();
    mount(view(), { onOpenRun });

    fireEvent.click(screen.getByRole('button', { name: '#10' }));

    expect(onOpenRun).toHaveBeenCalledWith(10);
  });

  it('says it is loading rather than rendering an empty repository', () => {
    mount(null);

    expect(screen.getByText(/loading the repository/i)).toBeTruthy();
  });
});
