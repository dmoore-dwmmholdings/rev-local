import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Dashboard } from './Dashboard';
import type { Autopilot, QueueItem, QueueStatus, RepoCard } from './ipc';

/** A card with everything present, which tests then vary one field of. */
function card(overrides: Partial<RepoCard> = {}): RepoCard {
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
        poll_interval_secs: 120,
        next_poll_in_secs: 60,
        consecutive_failures: 0,
        last_error: null,
        notes: [],
      },
    },
    last_run: { run_id: 7, status: 'done', verdict: 'comment' },
    queue_depth: 2,
    budget: {
      runs: 37,
      runs_limit: 200,
      tokens: 189_000,
      tokens_limit: 2_000_000,
      tokens_known: true,
    },
    ...overrides,
  };
}

const noop = vi.fn();

describe('dashboard', () => {
  it('shows all four regions of a card', () => {
    render(<Dashboard dashboard={{ repos: [card()], mode: 'dry_run', paused: false }} onMode={noop} onOpenRun={noop} onOpenRepo={noop} />);

    expect(screen.getByText('acme')).toBeDefined();
    expect(screen.getByText(/#7 done \(comment\)/)).toBeDefined();
    expect(screen.getByText('2')).toBeDefined();
    expect(screen.getByText(/37/)).toBeDefined();
  });

  it('says a repository has no runs rather than omitting the line', () => {
    // A card with no line about runs reads as one whose runs failed to load.
    render(
      <Dashboard
        dashboard={{ repos: [card({ last_run: undefined })], mode: 'dry_run', paused: false }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );

    expect(screen.getByText('none yet')).toBeDefined();
  });

  it('renders a lower-bound token count differently from a total', () => {
    // §18, and the assertion this file exists for. A bar is the easiest place in
    // a UI to lose the distinction between "spent this much" and "spent at least
    // this much", and losing it makes a partial figure look authoritative.
    const { container } = render(
      <Dashboard
        dashboard={{
          repos: [card({ budget: { ...card().budget, tokens_known: false } })],
          mode: 'dry_run',
          paused: false,
        }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );

    expect(screen.getByText(/lower bound/)).toBeDefined();
    expect(screen.getByText(/≥/)).toBeDefined();
    expect(container.querySelector('.meter-partial')).not.toBeNull();
  });

  it('does not hedge a fully measured count', () => {
    // The other half: hedging everything would train people to ignore the hedge.
    const { container } = render(
      <Dashboard dashboard={{ repos: [card()], mode: 'dry_run', paused: false }} onMode={noop} onOpenRun={noop} onOpenRepo={noop} />,
    );

    expect(screen.queryByText(/lower bound/)).toBeNull();
    expect(container.querySelector('.meter-partial')).toBeNull();
  });

  it('shows the kill switch state as a banner when paused', () => {
    // §15: the switch is reachable from every screen, and a screen that does not
    // say it is engaged is worse than one without the switch.
    render(<Dashboard dashboard={{ repos: [card()], mode: 'off', paused: true }} onMode={noop} onOpenRun={noop} onOpenRepo={noop} />);

    expect(screen.getByRole('status').textContent).toMatch(/Paused/);
  });

  it('names the autonomy on each card', () => {
    // It is the setting that decides whether this repository writes to anybody
    // else's systems, so it belongs where somebody scanning cards will see it.
    render(
      <Dashboard
        dashboard={{ repos: [card(), card({ repo: { ...card().repo, id: 2, repo: 'widgets', autonomy: 'auto' } })], mode: 'auto', paused: false }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );

    expect(screen.getByText('dry_run')).toBeDefined();
    expect(screen.getByText('auto')).toBeDefined();
  });

  it('says so when there are no repositories at all', () => {
    render(<Dashboard dashboard={{ repos: [], mode: 'off', paused: false }} onMode={noop} onOpenRun={noop} onOpenRepo={noop} />);

    expect(screen.getByText(/No repositories yet/)).toBeDefined();
  });

  it('confirms widening autonomy and does not confirm narrowing it', () => {
    // The asymmetry is deliberate: turning autonomy up is what lets rev-local
    // write to somebody else's systems; turning it down can only ever stop it.
    const onMode = vi.fn();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);

    const { container } = render(
      <Dashboard dashboard={{ repos: [card()], mode: 'dry_run', paused: false }} onMode={onMode} onOpenRun={noop} onOpenRepo={noop} />,
    );
    const select = container.querySelector('select');
    if (!select) throw new Error('no mode selector');

    // Narrowing: dry_run -> off.
    select.value = 'off';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    expect(confirm).not.toHaveBeenCalled();
    expect(onMode).toHaveBeenCalledWith('off');

    // Widening: dry_run -> auto.
    select.value = 'auto';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    expect(confirm).toHaveBeenCalled();

    confirm.mockRestore();
  });

  it('opens the repository screen from the card name', () => {
    // §15 screen 1 → screen 2. A card that showed a repository but could not
    // open it would leave the command line as the only route to its config.
    const onOpenRepo = vi.fn();
    render(
      <Dashboard
        dashboard={{ repos: [card()], mode: 'dry_run', paused: false }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={onOpenRepo}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'acme' }));

    expect(onOpenRepo).toHaveBeenCalledWith(1);
  });
});

/** A queue with nothing in it, which tests then vary one field of. */
function queue(overrides: Partial<QueueStatus> = {}): QueueStatus {
  return {
    running: false,
    draining: false,
    paused: false,
    queued_total: 0,
    active: [],
    waiting: [],
    waiting_hidden: 0,
    recent: [],
    ...overrides,
  };
}

function item(overrides: Partial<QueueItem> = {}): QueueItem {
  return {
    run_id: 11,
    repo: 'acme',
    repo_id: 1,
    change: 'abcdef0123456789',
    status: 'queued',
    trigger: 'manual',
    created_at: new Date().toISOString(),
    ...overrides,
  };
}

describe('the queue panel', () => {
  const dash = { repos: [card()], mode: 'dry_run', paused: false };

  function mount(q: QueueStatus | null, props: { paused?: boolean; onStartQueue?: () => void; onOpenRun?: (id: number) => void } = {}) {
    return render(
      <Dashboard
        dashboard={{ ...dash, paused: props.paused ?? false }}
        queue={q}
        onStartQueue={props.onStartQueue ?? noop}
        onMode={noop}
        onOpenRun={props.onOpenRun ?? noop}
        onOpenRepo={noop}
      />,
    );
  }

  it('says each section is empty rather than omitting it', () => {
    // A heading that disappears with its rows is indistinguishable from one that
    // failed to load, and "is anything running?" is the question this panel is on
    // screen to answer.
    mount(queue());

    expect(screen.getByText(/Nothing is running and nothing is waiting/)).toBeDefined();
    expect(screen.getByText(/No reviews are waiting to run/)).toBeDefined();
    expect(screen.getByText(/No review has finished yet/)).toBeDefined();
  });

  it('reports a run in flight even when this window did not start it', () => {
    // `running` is read from the runs. A review started from the command line, or
    // by a previous session, is still a review that is running.
    mount(queue({ running: true, active: [item({ status: 'reviewing' })] }));

    expect(screen.getByText('1 running')).toBeDefined();
    expect(screen.getByText('reviewing')).toBeDefined();
  });

  it('labels a run with its status, not with its trigger', () => {
    // The panel used to put the trigger in the status column, which told somebody
    // watching a run what had started it and nothing about what it was doing.
    const { container } = mount(queue({ running: true, active: [item({ status: 'synthesizing', trigger: 'poll' })] }));

    // Scoped to the table: the header's "1 running" count carries the same tone.
    expect(container.querySelector('.queue-table .run-active')?.textContent).toBe('synthesizing');
    expect(screen.getByText('poll')).toBeDefined();
  });

  it('says how many waiting runs it is not showing', () => {
    // §18: a page of a queue must not read as the queue.
    mount(queue({ queued_total: 30, waiting: [item()], waiting_hidden: 29 }));

    expect(screen.getByText(/Showing the next 1 of 30/)).toBeDefined();
    expect(screen.getByText(/29 more are waiting/)).toBeDefined();
  });

  it('cannot start queued work while the kill switch is engaged', () => {
    const onStartQueue = vi.fn();
    mount(queue({ queued_total: 3, waiting: [item()], paused: true }), { paused: true, onStartQueue });

    const button = screen.getByRole('button', { name: /Start queued reviews/ });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/Held by the kill switch/)).toBeDefined();
  });

  it('will not offer to start an empty queue', () => {
    mount(queue());

    expect((screen.getByRole('button', { name: /Start queued reviews/ }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('starts queued work, and reports busy from the runs rather than the click', () => {
    // A window-local "I pressed it" flag is cleared after the last run's event has
    // been delivered, so a queue that stopped with work still in it would leave
    // this button disabled and claiming to work, with nothing left to correct it.
    const onStartQueue = vi.fn();
    const { rerender } = mount(queue({ queued_total: 1, waiting: [item()] }), { onStartQueue });

    fireEvent.click(screen.getByRole('button', { name: /Start queued reviews/ }));
    expect(onStartQueue).toHaveBeenCalled();

    rerender(
      <Dashboard
        dashboard={dash}
        queue={queue({ queued_total: 1, running: true, active: [item({ run_id: 9, status: 'reviewing' })] })}
        onStartQueue={onStartQueue}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );
    expect((screen.getByRole('button', { name: /Reviewing…/ }) as HTMLButtonElement).disabled).toBe(true);

    // And it comes back the moment the runs say nothing is active — even if this
    // window's own drain flag is still set.
    rerender(
      <Dashboard
        dashboard={dash}
        queue={queue({ queued_total: 1, waiting: [item()], draining: true })}
        onStartQueue={onStartQueue}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );
    expect((screen.getByRole('button', { name: /Start queued reviews/ }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('opens a run from any of the three sections', () => {
    const onOpenRun = vi.fn();
    mount(
      queue({
        running: true,
        active: [item({ run_id: 1, status: 'reviewing' })],
        waiting: [item({ run_id: 2 })],
        recent: [item({ run_id: 3, status: 'done', finished_at: new Date().toISOString() })],
      }),
      { onOpenRun },
    );

    fireEvent.click(screen.getByRole('button', { name: '#3' }));
    expect(onOpenRun).toHaveBeenCalledWith(3);
  });

  it('says it is still reading rather than showing an empty queue', () => {
    // `null` is "not read yet" and is not the same as "nothing queued". Rendering
    // them the same way says "nothing is running" before anything has been asked.
    mount(null);

    expect(screen.getByText(/Reading the queue/)).toBeDefined();
    expect(screen.queryByText(/No reviews are waiting/)).toBeNull();
  });
});

// --- the autopilot panel (RL-1501, RL-1506) ---------------------------------

describe('autopilot panel', () => {
  function state(overrides: Partial<Autopilot> = {}): Autopilot {
    return {
      enabled: true,
      ticking: false,
      interval_secs: 60,
      last_tick_at: new Date().toISOString(),
      last_line: 'Found 2 new change(s), reviewed 2, filed 1.',
      last_error: null,
      notes: [],
      ...overrides,
    };
  }

  function mount(
    autopilot: Autopilot | null,
    mode = 'auto',
    handlers = {},
    queue: QueueStatus | null = null,
  ) {
    render(
      <Dashboard
        dashboard={{ repos: [card()], mode, paused: false }}
        autopilot={autopilot}
        queue={queue}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
        {...handlers}
      />,
    );
  }

  it('says what an idle switch is idle about', () => {
    // RL-1548. Everything on this panel was gated behind `enabled`, so an
    // install with the switch off said the word "Off" and nothing more — while
    // 51 runs sat queued on the live machine and nobody had ever turned it on.
    // "It doesn't want to process automatically" was the report, and this was
    // the whole of the app's answer.
    mount(state({ enabled: false }), 'auto', {}, queue({ queued_total: 51 }));

    expect(screen.getByText(/51 changes waiting/)).toBeDefined();
    expect(screen.getByText(/Nothing reviews them until this is on/)).toBeDefined();
  });

  it('stays quiet when the switch is off and nothing is waiting', () => {
    // A fresh install has an empty queue, and a warning about nothing is how a
    // panel teaches somebody to stop reading it.
    mount(state({ enabled: false }), 'auto', {}, queue({ queued_total: 0 }));

    expect(screen.queryByText(/Nothing reviews them until this is on/)).toBeNull();
  });

  it('says whether it is on, and how often it looks', () => {
    // The first question anybody has about this app, and until RL-1506 no screen
    // answered it.
    mount(state());

    expect(screen.getByText(/On — checking every 60 seconds/)).toBeDefined();
  });

  it('shows the last pass in the daemon’s own words', () => {
    // Not reassembled from counts here: two renderings of one pass disagree the
    // first time either changes.
    mount(state());

    expect(screen.getByText(/Found 2 new change\(s\), reviewed 2, filed 1\./)).toBeDefined();
  });

  it('warns that findings will wait when the mode cannot file', () => {
    // Filing an issue is high risk by §12.3's list, so every mode below `auto`
    // holds every issue. Somebody who switched the loop on and got an inbox
    // instead of a tracker has to be told why.
    mount(state(), 'auto_low_ask_high');

    expect(screen.getByText(/wait for your approval/)).toBeDefined();
  });

  it('does not warn about approvals when the mode files on its own', () => {
    mount(state(), 'auto');

    expect(screen.queryByText(/wait for your approval/)).toBeNull();
  });

  it('shows the notes a pass left, so held work is not silent', () => {
    // §18: work that did not happen is reported with its reason.
    mount(state({ notes: ['acme: no `andare_project` set'] }));

    expect(screen.getByText(/no `andare_project` set/)).toBeDefined();
  });

  it('reports a pass that could not run at all as an alert', () => {
    mount(state({ last_error: 'could not open the database' }));

    expect(screen.getByRole('alert').textContent).toMatch(/could not open the database/);
  });

  it('says nothing has happened yet rather than showing a stale line', () => {
    mount(state({ last_tick_at: null, last_line: '' }));

    expect(screen.getByText(/Waiting for the first pass/)).toBeDefined();
  });

  it('can be switched off, and asks the app rather than deciding itself', () => {
    const onToggleAutopilot = vi.fn();
    mount(state(), 'auto', { onToggleAutopilot });

    fireEvent.click(screen.getByRole('checkbox'));
    expect(onToggleAutopilot).toHaveBeenCalledWith(false);
  });

  it('will not ask for a second pass while one is running', () => {
    mount(state({ ticking: true }));

    expect(screen.getByRole('button', { name: /checking/ }).hasAttribute('disabled')).toBe(true);
  });

  it('renders nothing at all before the status has been read', () => {
    // `null` is "not asked yet". Drawing an "Off" switch then would be a lie
    // somebody acts on by clicking it.
    mount(null);

    expect(screen.queryByLabelText('autopilot')).toBeNull();
  });
});

describe('the kill switch', () => {
  it('offers a way back out of paused', () => {
    // §12.1 makes stopping reversible. The banner stated the condition and gave
    // no way out of it, and the only resume was the command line (RL-1522).
    render(
      <Dashboard
        dashboard={{ repos: [card()], mode: 'auto', paused: true }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );

    expect(screen.getByRole('status').textContent).toMatch(/Paused/);
    expect(screen.getByRole('button', { name: 'Resume' })).toBeDefined();
  });

  it('asks the app to resume rather than deciding for itself', () => {
    const onResume = vi.fn();
    render(
      <Dashboard
        dashboard={{ repos: [card()], mode: 'auto', paused: true }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
        onResume={onResume}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Resume' }));
    expect(onResume).toHaveBeenCalled();
  });

  it('shows no resume button when nothing is paused', () => {
    render(
      <Dashboard
        dashboard={{ repos: [card()], mode: 'auto', paused: false }}
        onMode={noop}
        onOpenRun={noop}
        onOpenRepo={noop}
      />,
    );

    expect(screen.queryByRole('button', { name: 'Resume' })).toBeNull();
  });
});
