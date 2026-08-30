import { useState } from 'react';
import {
  allChecks,
  unmappedCount,
  type DoctorCheck,
  type SettingsView,
  type ServerPanel,
  type TargetPanel,
  type UnmappedRow,
} from './ipc';

/**
 * §15 screen 6 — engines with doctor inline, MCP servers with their discovered
 * tools, the capability mapping table, budgets and retention.
 *
 * Three things here are decisions.
 *
 * **Unmapped capabilities lead.** §15's criterion is that they are *visible*, and
 * a count somebody has to assemble by reading four panels is not visible. The
 * banner is at the top, says how many, and every row carries the affordance that
 * fixes it.
 *
 * **A secret's value never arrives.** The screen renders presence — which header,
 * from the keychain or from the file — because the IPC type has no field that
 * could carry the value. Nothing here has to remember to redact.
 *
 * **"Not contacted" is not "unmapped".** A server nobody has spoken to has an
 * unknown mapping. Showing four unmapped capabilities for it would send somebody
 * writing overrides for tools that are almost certainly there.
 */

const HEALTH_LABEL: Record<DoctorCheck['health'], string> = {
  ok: 'ok',
  warn: 'warn',
  fail: 'FAIL',
  not_needed: 'n/a',
};

function Check({ check }: { check: DoctorCheck }) {
  return (
    <li className={`check check-${check.health}`}>
      <span className="tag">{HEALTH_LABEL[check.health]}</span>
      <strong>{check.name}</strong>
      <span className="dim">{check.detail}</span>
      {/* §18: an error says what to do next. The remediation is the check's own,
          not a generic line, so it names the command that fixes this one. */}
      {check.remediation && <code className="remedy">{check.remediation}</code>}
    </li>
  );
}

function Server({ server }: { server: ServerPanel }) {
  return (
    <li className="server">
      <header className="server-head">
        <strong>{server.id}</strong>
        <span className="tag">{server.transport}</span>
        <span className="mono dim">{server.endpoint}</span>
      </header>

      <p className={server.error ? 'warn-text' : 'dim'}>{server.summary}</p>

      {server.secrets.length > 0 && (
        <ul className="secrets">
          {server.secrets.map((s) => (
            <li key={s.header}>
              <span className="mono">{s.header}</span>{' '}
              {/* Presence and provenance. The value is not here to show. */}
              {s.source === 'keychain' ? (
                <span className="dim">
                  from the keychain entry <span className="mono">{s.keychain_entry}</span>
                </span>
              ) : (
                <span className="warn-text">set in the config file</span>
              )}
              {s.advice && <div className="dim">{s.advice}</div>}
            </li>
          ))}
        </ul>
      )}

      {server.contacted && server.tools.length > 0 && (
        <details>
          <summary>{server.tools.length} tools</summary>
          <ul className="globs">
            {server.tools.map((t) => (
              <li key={t} className="mono">
                {t}
              </li>
            ))}
          </ul>
        </details>
      )}
    </li>
  );
}

function Unmapped({
  target,
  row,
  onMap,
}: {
  target: string;
  row: UnmappedRow;
  onMap: (target: string, capability: string, tool: string) => void;
}) {
  const [tool, setTool] = useState('');

  return (
    <li className="unmapped">
      <div>
        <strong>{row.capability}</strong> <span className="tag tag-off">unmapped</span>
      </div>
      {/* Both lists, because the fix needs both: what rev-local looked for, and
          what this server actually has to offer. */}
      <p className="dim">
        looked for <span className="mono">{row.candidates.join(', ') || '—'}</span>
      </p>
      <div className="map-row">
        <label>
          bind to
          <select value={tool} onChange={(e) => setTool(e.target.value)} aria-label={`tool for ${row.capability}`}>
            <option value="">choose a tool…</option>
            {row.available.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </label>
        <button disabled={!tool} onClick={() => onMap(target, row.capability, tool)}>
          Map it
        </button>
      </div>
    </li>
  );
}

function Target({
  panel,
  onMap,
  onUnmap,
}: {
  panel: TargetPanel;
  onMap: (target: string, capability: string, tool: string) => void;
  onUnmap: (target: string, capability: string) => void;
}) {
  return (
    <li className="target">
      <header className="server-head">
        <strong>{panel.target}</strong>
        <span className="dim">→ {panel.server}</span>
      </header>

      {!panel.server_contacted ? (
        // Said explicitly, and deliberately not counted as unmapped.
        <p className="dim">
          The mapping is unknown — <span className="mono">{panel.server}</span> has not answered
          yet, so nothing has been asked of it.
        </p>
      ) : (
        <>
          <table className="mapping">
            <thead>
              <tr>
                <th>Capability</th>
                <th>Tool</th>
                <th>Bound by</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {panel.bound.map((b) => (
                <tr key={b.capability}>
                  <td>{b.capability}</td>
                  <td className="mono">{b.tool}</td>
                  <td>{b.from_override ? 'you' : 'resolution'}</td>
                  <td>
                    {b.from_override && (
                      <button className="link" onClick={() => onUnmap(panel.target, b.capability)}>
                        undo
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          {panel.unmapped.length > 0 && (
            <ul className="unmapped-list">
              {panel.unmapped.map((u) => (
                <Unmapped key={u.capability} target={panel.target} row={u} onMap={onMap} />
              ))}
            </ul>
          )}
        </>
      )}
    </li>
  );
}

export function Settings({
  view,
  busy,
  onRunDoctor,
  onMap,
  onUnmap,
  onRunOnboarding,
}: {
  view: SettingsView | null;
  busy: boolean;
  onRunDoctor: () => void;
  onMap: (target: string, capability: string, tool: string) => void;
  onUnmap: (target: string, capability: string) => void;
  onRunOnboarding: () => void;
}) {
  if (!view) return <p className="empty">Loading settings.</p>;

  const unmapped = unmappedCount(view);
  const checks = allChecks(view.doctor);

  return (
    <section className="settings">
      {unmapped > 0 && (
        // §15's criterion, at the top of the screen rather than four panels down.
        <p className="banner banner-warn" role="status">
          {unmapped} capabilit{unmapped === 1 ? 'y is' : 'ies are'} unmapped. rev-local will not
          perform {unmapped === 1 ? 'it' : 'them'} until each is bound to a tool the server has.
        </p>
      )}

      {view.target_errors.map((error) => (
        <p key={error} className="config-error" role="alert">
          {error}
        </p>
      ))}

      {/* Ahead of doctor and the server list, because it is the only part of this
          screen with an action attached. §15 lists what the screen contains, not
          what order it goes in — and a banner saying "2 unmapped" above a fix that
          takes two scrolls to reach is a callout, not an affordance. */}
      <section>
        <h3>Capability mapping</h3>
        <ul className="targets">
          {view.targets.map((t) => (
            <Target key={t.target} panel={t} onMap={onMap} onUnmap={onUnmap} />
          ))}
        </ul>
        <p className="dim">
          Overrides are stored in <span className="mono">{view.overrides_path}</span>.
        </p>
      </section>

      <section>
        <div className="section-head">
          <h3>Engines and prerequisites</h3>
          <button onClick={onRunDoctor} disabled={busy}>
            {busy ? 'Running doctor…' : 'Re-run doctor'}
          </button>
        </div>
        {checks.length === 0 ? (
          // An empty report is "nothing has run", not "everything is fine", and
          // the two must not look alike.
          <p className="empty">Doctor has not run yet.</p>
        ) : (
          <ul className="checks">
            {checks.map((c) => (
              <Check key={c.name} check={c} />
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3>MCP servers</h3>
        {view.servers.length === 0 ? (
          <p className="empty">
            No MCP servers are configured in <span className="mono">{view.config_path}</span>.
          </p>
        ) : (
          <ul className="servers">
            {view.servers.map((s) => (
              <Server key={s.id} server={s} />
            ))}
          </ul>
        )}
      </section>

      <section>
        <div className="section-head">
          <h3>Setup</h3>
          {/* §15's onboarding, re-runnable. One that can only happen once is a
              thing people are afraid to leave — and the second repository
              deserves the same walk as the first. */}
          <button onClick={onRunOnboarding}>Run setup again</button>
        </div>
        <p className="dim">
          Walks through the checks, adding a repository, choosing an engine and an
          autonomy mode, and one review. Nothing already configured is changed.
        </p>
      </section>

      <section>
        <h3>Budgets and retention</h3>
        <dl className="card-facts">
          <dt>runs per repo per day</dt>
          <dd>{view.limits.daily_runs_per_repo}</dd>
          <dt>tokens per repo per day</dt>
          <dd>{view.limits.daily_tokens_per_repo.toLocaleString()}</dd>
          <dt>cost per repo per day</dt>
          {/* Zero means unlimited, which is worth spelling out: a budget screen
              reading "$0" looks like a repository that can spend nothing. */}
          <dd>
            {view.limits.daily_cost_usd_per_repo === 0
              ? 'unlimited'
              : `$${view.limits.daily_cost_usd_per_repo}`}
          </dd>
          <dt>when exhausted</dt>
          <dd>{view.limits.on_exhausted}</dd>
          <dt>transcripts kept</dt>
          <dd>{view.limits.transcript_retention_days} days</dd>
        </dl>
        <p className="dim">
          These come from <span className="mono">{view.config_path}</span>, which is hand-edited.
        </p>
      </section>
    </section>
  );
}
