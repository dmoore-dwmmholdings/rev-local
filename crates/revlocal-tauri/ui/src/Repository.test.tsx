import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { Repository } from './Repository';
import { EMPTY_TREE, type BranchList, type RepositoryView, type ReviewCommit, type TriggerStatus } from './ipc';

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

describe('starting a review by hand', () => {
  const branchList: BranchList = {
    branches: [
      { name: 'feature/widgets', head: 'aaaaaaaaaaaa1111', current: true },
      { name: 'main', head: 'bbbbbbbbbbbb2222', current: false },
    ],
    suggested_base: 'main',
    current: 'feature/widgets',
  };

  const commits: ReviewCommit[] = [
    {
      sha: 'cccccccccccc3333',
      subject: 'widen the widget',
      author: 'Sam',
      authored_at: '2026-01-01T01:00:00Z',
    },
  ];

  function wire(overrides: { onReview?: ReturnType<typeof vi.fn> } = {}) {
    const onReview =
      overrides.onReview ??
      vi.fn().mockResolvedValue({ run_id: 42, status: 'queued', scope: 'branch feature/widgets' });
    return {
      onReview,
      onLoadBranches: vi.fn().mockResolvedValue(branchList),
      onLoadCommits: vi.fn().mockResolvedValue(commits),
    };
  }

  it('opens on the checked-out branch, compared against the obvious base', async () => {
    // Opening on whichever branch sorted first would make the common case —
    // "review what I am working on" — a two-step.
    const props = wire();
    mount(view(), props);

    await waitFor(() => expect(screen.getByRole('button', { name: 'Review branch' })).toBeDefined());
    const selects = screen.getAllByRole('combobox');
    expect((selects[0] as HTMLSelectElement).value).toBe('feature/widgets');
    expect((selects[1] as HTMLSelectElement).value).toBe('main');
  });

  it('reviews a branch against its base, by resolved sha rather than by name', async () => {
    // The sha is captured when the screen loaded. A branch that advances between
    // the click and the run must not change what was asked for.
    const props = wire();
    mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review branch' }));
    fireEvent.click(screen.getByRole('button', { name: 'Review branch' }));

    await waitFor(() =>
      expect(props.onReview).toHaveBeenCalledWith(1, {
        branch: 'feature/widgets',
        revision: 'aaaaaaaaaaaa1111',
        base: 'main',
      }),
    );
  });

  it('reviews the whole repository as a diff against the empty tree', async () => {
    // Expressed as a base rather than as a mode, so truncation and the omitted
    // file report apply to it like any other review.
    const props = wire();
    mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review whole repository' }));
    fireEvent.click(screen.getByRole('button', { name: 'Review whole repository' }));

    await waitFor(() =>
      expect(props.onReview).toHaveBeenCalledWith(1, {
        branch: 'feature/widgets',
        revision: 'aaaaaaaaaaaa1111',
        base: EMPTY_TREE,
      }),
    );
  });

  it('reviews one commit with no base at all', async () => {
    const props = wire();
    mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review' }));
    fireEvent.click(screen.getByRole('button', { name: 'Review' }));

    await waitFor(() =>
      expect(props.onReview).toHaveBeenCalledWith(1, {
        branch: 'feature/widgets',
        revision: 'cccccccccccc3333',
      }),
    );
  });

  it('names the run it queued instead of leaving the screen unchanged', async () => {
    // The review runs in the background, so the only evidence it started is what
    // this line says. Without it the button looks like it did nothing.
    const props = wire();
    mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review branch' }));
    fireEvent.click(screen.getByRole('button', { name: 'Review branch' }));

    await waitFor(() => expect(screen.getByRole('status').textContent).toMatch(/Queued run #42/));
  });

  it('says why a branch cannot be compared rather than only disabling the button', async () => {
    // A control that is simply dead teaches nothing about why.
    const props = wire();
    props.onLoadBranches.mockResolvedValue({
      branches: [{ name: 'main', head: 'bbbb', current: true }],
      current: 'main',
    } satisfies BranchList);
    mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review branch' }));
    expect((screen.getByRole('button', { name: 'Review branch' }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/only one branch/)).toBeDefined();
  });

  it('reports a failure to start beside the controls', async () => {
    const onReview = vi.fn().mockRejectedValue(new Error('no local checkout to review'));
    const props = wire({ onReview });
    mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review branch' }));
    fireEvent.click(screen.getByRole('button', { name: 'Review branch' }));

    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toMatch(/no local checkout/),
    );
  });

  it('is not offered for a repository kind it cannot review by hand', async () => {
    // Subversion has no local branches to list, and a panel that could list
    // nothing and start nothing would sit there claiming to be loading.
    const props = wire();
    mount(
      view({
        repo: { ...view().repo, kind: 'svn' },
        watching: { kind: 'paths', paths: ['trunk'] },
      }),
      props,
    );

    await waitFor(() => screen.getByText(/Watched paths/));
    expect(screen.queryByText('Review now')).toBeNull();
    expect(props.onLoadBranches).not.toHaveBeenCalled();
  });

  it('loads branches once per repository, not once per render', async () => {
    // The effect used to depend on the callback props. A caller passing an inline
    // arrow handed it a new identity every render, and it fetched, set state,
    // re-rendered and fetched again — forever.
    const props = wire();
    const { rerender } = mount(view(), props);

    await waitFor(() => screen.getByRole('button', { name: 'Review branch' }));
    rerender(
      <Repository
        view={view()}
        onOpenRun={noop}
        onSave={saveOk}
        onLoadBranches={(id) => props.onLoadBranches(id)}
        onLoadCommits={(id, branch) => props.onLoadCommits(id, branch)}
        onReview={(id, request) => props.onReview(id, request)}
      />,
    );

    await waitFor(() => screen.getByRole('button', { name: 'Review branch' }));
    expect(props.onLoadBranches).toHaveBeenCalledTimes(1);
  });
});

// --- where findings go (RL-1503) --------------------------------------------

describe('the filing configuration', () => {
  it('shows the project key from the configuration', () => {
    mount(view({ config_json: '{\n  "andare_project": "REVL"\n}' }));

    expect((screen.getByLabelText('andare project key') as HTMLInputElement).value).toBe('REVL');
  });

  it('says plainly that nothing is filed while it is empty', () => {
    // Three repositories ran for a week with `{}`, and the only place that said
    // why was a report nobody was shown.
    mount(view({ config_json: '{}' }));

    expect(screen.getByText(/Nothing is filed while this is empty/)).toBeDefined();
  });

  it('writes the key into the configuration the Save button sends', async () => {
    // One draft and one Save. A form that saved on its own would discard whatever
    // was half-typed in the editor below it.
    const onSave = vi.fn((_: string) => Promise.resolve());
    mount(view({ config_json: '{}' }), { onSave });

    fireEvent.change(screen.getByLabelText('andare project key'), { target: { value: 'revl' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));

    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(JSON.parse(onSave.mock.calls[0]?.[0] ?? '')).toEqual({ andare_project: 'REVL' });
  });

  it('keeps every other setting when the key changes', () => {
    // Editing one field must not be a way to silently reset the rest.
    const onSave = vi.fn((_: string) => Promise.resolve());
    mount(view({ config_json: '{"poll_interval_secs": 300}' }), { onSave });

    fireEvent.change(screen.getByLabelText('andare project key'), { target: { value: 'ENG' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));

    expect(JSON.parse(onSave.mock.calls[0]?.[0] ?? '')).toEqual({
      poll_interval_secs: 300,
      andare_project: 'ENG',
    });
  });

  it('says the editor must be fixed rather than guessing at broken JSON', () => {
    mount(view({ config_json: '{ not json' }));

    expect(screen.getByText(/does not parse/)).toBeDefined();
    expect(screen.queryByLabelText('andare project key')).toBeNull();
  });
});
