import {
  FINDING_STATES,
  SEVERITIES,
  type FindingFilter,
  type FindingRow,
  type FindingsView,
} from './ipc';

/**
 * §15 screen 4 — findings across every repository.
 *
 * The filter values live above this component and every change re-reads from the
 * daemon. That looks like more work than filtering the array in hand, and it is
 * the point: this is the one screen that can be large, and a browser-side filter
 * has to fetch everything first. Where the filtering happens decides what the
 * screen costs.
 */

/** A `<select>` that means "no filter" when empty. */
function Choice({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: readonly string[];
  onChange: (next: string) => void;
}) {
  return (
    <label className="filter">
      {label}
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        {/* Named rather than blank. An empty first option reads as "not chosen
            yet"; "any severity" says what showing everything means. */}
        <option value="">any {label.toLowerCase()}</option>
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    </label>
  );
}

export function Findings({
  view,
  filter,
  onFilter,
  onOpenRun,
  onSuppress,
  onFile,
}: {
  view: FindingsView | null;
  filter: FindingFilter;
  onFilter: (next: FindingFilter) => void;
  onOpenRun: (runId: number) => void;
  onSuppress: (row: FindingRow) => void;
  onFile: (row: FindingRow) => void;
}) {
  // The dropdown offers what the data has. Rendered from the last successful
  // read, so the categories stay put while a filtered read is in flight rather
  // than emptying and refilling under the cursor.
  const categories = view?.categories ?? [];

  const set = (patch: Partial<FindingFilter>) => onFilter({ ...filter, ...patch });

  return (
    <section className="findings">
      <div className="filters" role="group" aria-label="filters">
        <Choice
          label="Severity"
          value={filter.min_severity ?? ''}
          options={SEVERITIES}
          onChange={(v) => set({ min_severity: v || undefined })}
        />
        <Choice
          label="Category"
          value={filter.category ?? ''}
          options={categories}
          onChange={(v) => set({ category: v || undefined })}
        />
        <Choice
          label="State"
          value={filter.state ?? ''}
          options={FINDING_STATES}
          onChange={(v) => set({ state: v || undefined })}
        />
        <span className="spacer" />
        <button className="link" onClick={() => onFilter({})}>
          clear filters
        </button>
      </div>

      {/* Severity filtering is "this and worse", which is not what a lone
          dropdown labelled "high" looks like it does. Said once, in words. */}
      <p className="note">
        Severity selects that level <em>and worse</em>. Filters combine: choosing two
        narrows the table.
      </p>

      {!view ? (
        <p className="empty">Loading findings.</p>
      ) : (
        <>
          <p className="count">
            {/* Both numbers. A count of what is shown, with no sense of what was
                hidden, is how somebody concludes a filter found everything. */}
            Showing {view.rows.length} of {view.total_before_filter} finding
            {view.total_before_filter === 1 ? '' : 's'}.
            {view.truncated && (
              <strong className="warn-text">
                {' '}
                Older runs were not scanned — this is not the whole history.
              </strong>
            )}
          </p>

          {view.rows.length === 0 ? (
            <p className="empty">
              {view.total_before_filter === 0
                ? 'No findings have been recorded yet.'
                : 'No findings match these filters.'}
            </p>
          ) : (
            <table className="findings-table">
              <thead>
                <tr>
                  <th>Severity</th>
                  <th>Repository</th>
                  <th>Category</th>
                  <th>Finding</th>
                  <th>State</th>
                  <th>Run</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {view.rows.map((row) => (
                  <tr key={row.id} className={`sev-${row.severity}`}>
                    <td>
                      <span className="tag">{row.severity}</span>
                    </td>
                    <td>{row.repo}</td>
                    <td>{row.category}</td>
                    <td>
                      {row.title}
                      {row.file && <div className="dim mono">{row.file}</div>}
                    </td>
                    <td>{row.state}</td>
                    <td>
                      <button className="link" onClick={() => onOpenRun(row.run_id)}>
                        #{row.run_id}
                      </button>
                    </td>
                    <td className="row-actions">
                      {/* Already suppressed is disabled rather than hidden: a
                          button that vanishes leaves somebody wondering whether
                          they clicked it or imagined it. */}
                      <button
                        disabled={row.state === 'suppressed'}
                        title={
                          row.state === 'suppressed'
                            ? 'already suppressed'
                            : 'Stop proposing this finding in this repository'
                        }
                        onClick={() => onSuppress(row)}
                      >
                        Suppress
                      </button>
                      <button
                        title="File this to Andare — risk-gated like any other publish"
                        onClick={() => onFile(row)}
                      >
                        File to Andare
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </section>
  );
}
