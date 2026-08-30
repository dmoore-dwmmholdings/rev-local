import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { Nav, SCREENS, UNBUILT } from './Nav';

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

  it('does not navigate to an unbuilt screen', () => {
    // A disabled tab that still fired would land somebody on a blank view and
    // leave them thinking the screen is broken rather than absent.
    const onSelect = vi.fn();
    render(<Nav screen="dashboard" onSelect={onSelect} />);

    screen.getByText('Settings').click();
    expect(onSelect).not.toHaveBeenCalled();
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
