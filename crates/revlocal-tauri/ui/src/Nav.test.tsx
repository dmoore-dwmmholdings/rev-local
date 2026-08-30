import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { Nav, SCREENS, UNBUILT, initialScreen } from './Nav';

describe('nav', () => {
  it('lists all six screens §15 names', () => {
    // Showing two would make somebody wonder whether the other four are hidden
    // under a menu. Listing all six says what exists and what does not.
    render(<Nav screen="dashboard" onSelect={vi.fn()} />);

    expect(SCREENS.length).toBe(6);
    expect(screen.getAllByRole('button').length).toBe(6);
    // Every one is labelled — a nav of six blank tabs would pass a count.
    for (const label of ['Dashboard', 'Repository', 'Run detail', 'Findings', 'Approvals', 'Settings']) {
      expect(screen.getByText(label)).toBeDefined();
    }
  });

  it('disables the screens that are not built', () => {
    render(<Nav screen="dashboard" onSelect={vi.fn()} />);

    const disabled = screen
      .getAllByRole('button')
      .filter((b) => (b as HTMLButtonElement).disabled).length;

    expect(disabled).toBe(UNBUILT.size);
  });

  it('has every screen §15 names built', () => {
    // This used to assert that clicking `Settings` did nothing, because it was
    // the last one unbuilt. All six exist now, so the honest test is that none
    // of them is disabled — the same fact, stated the way round it is now true.
    render(<Nav screen="dashboard" onSelect={vi.fn()} />);

    expect(UNBUILT.size).toBe(0);
    for (const button of screen.getAllByRole('button')) {
      expect((button as HTMLButtonElement).disabled).toBe(false);
    }
  });

  it('navigates to every screen, including the last one built', () => {
    const onSelect = vi.fn();
    render(<Nav screen="dashboard" onSelect={onSelect} />);

    screen.getByText('Settings').click();
    expect(onSelect).toHaveBeenCalledWith('settings');
  });

  it('navigates to a built one', () => {
    const onSelect = vi.fn();
    render(<Nav screen="dashboard" onSelect={onSelect} />);

    screen.getByText('Run detail').click();
    expect(onSelect).toHaveBeenCalledWith('run');
  });

  it('marks the current screen for assistive tech, not just visually', () => {
    render(<Nav screen="run" onSelect={vi.fn()} />);

    expect(screen.getByText('Run detail').getAttribute('aria-current')).toBe('page');
    expect(screen.getByText('Dashboard').getAttribute('aria-current')).toBeNull();
  });
});

describe('initial screen', () => {
  it('opens the screen a capture harness asked for', () => {
    // §16.4 photographs one screen at a time, and clicking into a webview from
    // the OS is not available — its DOM is not an accessibility tree.
    expect(initialScreen('approvals')).toBe('approvals');
    expect(initialScreen('run')).toBe('run');
  });

  it('falls back to the dashboard rather than showing nothing', () => {
    // A mistyped screen name should not produce a blank window.
    expect(initialScreen('nonsense')).toBe('dashboard');
    expect(initialScreen('')).toBe('dashboard');
  });
});
