import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Findings } from './Findings';
import type { FindingRow, FindingsView } from './ipc';

function row(overrides: Partial<FindingRow> = {}): FindingRow {
  return {
    id: 1,
    run_id: 10,
    repo_id: 1,
    repo: 'rev-local',
    severity: 'high',
    category: 'security',
    state: 'open',
    title: 'SQL injection in find_user',
    file: 'src/db.rs',
    fingerprint: 'fp-0',
    ...overrides,
  };
}

function view(overrides: Partial<FindingsView> = {}): FindingsView {
  return {
    rows: [row()],
    categories: ['security', 'tests'],
    total_before_filter: 1,
    truncated: false,
    ...overrides,
  };
}

const noop = vi.fn();

function mount(v: FindingsView | null, props: Partial<Parameters<typeof Findings>[0]> = {}) {
  return render(
    <Findings
      view={v}
      filter={{}}
      onFilter={noop}
      onOpenRun={noop}
      onSuppress={noop}
      onFile={noop}
      {...props}
    />,
  );
}

describe('findings', () => {
  it('sends every filter, so choosing two narrows rather than replaces', () => {
    // The acceptance criterion at the edge where the UI could break it: each
    // control patches the filter object rather than replacing it, so a second
    // choice does not discard the first.
    const onFilter = vi.fn();
    mount(view(), { filter: { min_severity: 'high' }, onFilter });

    fireEvent.change(screen.getByLabelText(/category/i), { target: { value: 'security' } });

    expect(onFilter).toHaveBeenCalledWith({ min_severity: 'high', category: 'security' });
  });

  it('offers only categories the data actually has', () => {
    // A dropdown listing a category nothing has is a dead end — a filter that
    // can only ever produce an empty table.
    mount(view({ categories: ['security'] }));

    const options = screen
      .getByLabelText(/category/i)
      .querySelectorAll('option');
    expect([...options].map((o) => o.textContent)).toEqual(['any category', 'security']);
  });

  it('says how many were hidden, not just how many are shown', () => {
    // §18. A count with no denominator is how somebody concludes a filtered
    // table is the whole story.
    mount(view({ rows: [], total_before_filter: 340 }));

    expect(screen.getByText(/showing 0 of 340 findings/i)).toBeTruthy();
  });

  it('distinguishes an empty database from an empty filter result', () => {
    // Two different situations with one remedy each: record some runs, or widen
    // the filter. Rendering both as "nothing here" sends people to the wrong one.
    const { unmount } = mount(view({ rows: [], total_before_filter: 0 }));
    expect(screen.getByText(/no findings have been recorded/i)).toBeTruthy();
    unmount();

    mount(view({ rows: [], total_before_filter: 12 }));
    expect(screen.getByText(/no findings match these filters/i)).toBeTruthy();
  });

  it('says when the scan stopped short of the whole history', () => {
    mount(view({ truncated: true }));

    expect(screen.getByText(/not the whole history/i)).toBeTruthy();
  });

  it('disables suppress on a finding that is already suppressed', () => {
    // Disabled rather than removed: a button that vanishes leaves somebody
    // unsure whether they clicked it.
    mount(view({ rows: [row({ state: 'suppressed' })] }));

    const button = screen.getByRole('button', { name: 'Suppress' });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it('offers filing to Andare and hands the whole row to the caller', () => {
    // The gate lives in the daemon, not here — this only has to pass along which
    // finding, and the row is what carries the repository the mode belongs to.
    const onFile = vi.fn();
    const only = row();
    mount(view({ rows: [only] }), { onFile });

    fireEvent.click(screen.getByRole('button', { name: /file to andare/i }));

    expect(onFile).toHaveBeenCalledWith(only);
  });

  it('jumps to the run that produced a finding', () => {
    const onOpenRun = vi.fn();
    mount(view(), { onOpenRun });

    fireEvent.click(screen.getByRole('button', { name: '#10' }));

    expect(onOpenRun).toHaveBeenCalledWith(10);
  });

  it('says it is loading rather than rendering an empty table', () => {
    // A null view and an empty one are different facts. Rendering both as an
    // empty table makes a screen that failed to load look like good news.
    mount(null);

    expect(screen.getByText(/loading findings/i)).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
  });
});
