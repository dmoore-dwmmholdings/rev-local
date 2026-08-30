import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { Dashboard } from './Dashboard';
import type { RepoCard } from './ipc';

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
    render(<Dashboard dashboard={{ repos: [card()], mode: 'dry_run', paused: false }} onMode={noop} onOpenRun={noop} />);

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
      />,
    );

    expect(screen.getByText(/lower bound/)).toBeDefined();
    expect(screen.getByText(/≥/)).toBeDefined();
    expect(container.querySelector('.meter-partial')).not.toBeNull();
  });

  it('does not hedge a fully measured count', () => {
    // The other half: hedging everything would train people to ignore the hedge.
    const { container } = render(
      <Dashboard dashboard={{ repos: [card()], mode: 'dry_run', paused: false }} onMode={noop} onOpenRun={noop} />,
    );

    expect(screen.queryByText(/lower bound/)).toBeNull();
    expect(container.querySelector('.meter-partial')).toBeNull();
  });

  it('shows the kill switch state as a banner when paused', () => {
    // §15: the switch is reachable from every screen, and a screen that does not
    // say it is engaged is worse than one without the switch.
    render(<Dashboard dashboard={{ repos: [card()], mode: 'off', paused: true }} onMode={noop} onOpenRun={noop} />);

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
      />,
    );

    expect(screen.getByText('dry_run')).toBeDefined();
    expect(screen.getByText('auto')).toBeDefined();
  });

  it('says so when there are no repositories at all', () => {
    render(<Dashboard dashboard={{ repos: [], mode: 'off', paused: false }} onMode={noop} onOpenRun={noop} />);

    expect(screen.getByText(/No repositories yet/)).toBeDefined();
  });

  it('confirms widening autonomy and does not confirm narrowing it', () => {
    // The asymmetry is deliberate: turning autonomy up is what lets rev-local
    // write to somebody else's systems; turning it down can only ever stop it.
    const onMode = vi.fn();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);

    const { container } = render(
      <Dashboard dashboard={{ repos: [card()], mode: 'dry_run', paused: false }} onMode={onMode} onOpenRun={noop} />,
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
});
