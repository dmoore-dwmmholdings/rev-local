import { useEffect, useState } from 'react';
import { describe, inTauri, invoke, onRunEvent, severityOf, type UiEvent } from './ipc';

/** One row in the activity feed, with the moment it arrived. */
type Entry = { event: UiEvent; at: Date; seq: number };

export function App() {
  const [entries, setEntries] = useState<Entry[]>([]);
  const [connected, setConnected] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let seq = 0;
    let unsubscribe: (() => void) | undefined;

    // §15: live updates come from events, not from polling the database. There is
    // no interval here and no fetch — if this component ever grows one, that rule
    // has been broken.
    void onRunEvent((event) => {
      seq += 1;
      setEntries((previous) => [{ event, at: new Date(), seq }, ...previous].slice(0, 500));
    }).then((off) => {
      unsubscribe = off;
      setConnected(inTauri());
    });

    return () => unsubscribe?.();
  }, []);

  async function killSwitch() {
    // §15: a destructive action names its target. There is exactly one target
    // here — everything — and the confirmation says so rather than asking "are
    // you sure?", which tells nobody anything.
    const ok = window.confirm(
      'Stop every running review and hold all pending publish actions?\n\n' +
        'Runs in flight are cancelled. Nothing already published is undone.',
    );
    if (!ok) return;

    try {
      await invoke('kill_switch');
      setNotice('Kill switch engaged. Reviews stopped; pending actions held.');
    } catch (error) {
      const remediation =
        typeof error === 'object' && error !== null && 'remediation' in error
          ? String((error as { remediation: unknown }).remediation)
          : null;
      setNotice(remediation ? `Could not engage the kill switch — ${remediation}` : 'Could not engage the kill switch.');
    }
  }

  return (
    <>
      <header>
        <span className={connected ? 'dot ok' : 'dot idle'} title={connected ? 'connected to the daemon' : 'not connected'} />
        <h1>rev-local</h1>
        <span className="spacer" />
        <button className="danger" onClick={killSwitch} title="Stop everything now (SPEC §12.1)">
          Kill switch
        </button>
      </header>

      {notice && (
        <div className="notice" role="status">
          {notice}
          <button className="link" onClick={() => setNotice(null)}>dismiss</button>
        </div>
      )}

      <main>
        <p className="note">
          Live activity. Events arrive over <code>revlocal://run-event</code> — this
          screen never polls the database.
        </p>

        {entries.length === 0 ? (
          <p className="empty">
            {connected
              ? 'Waiting for the daemon. Nothing has run yet.'
              : 'Not connected. Open this window from the rev-local app rather than a browser.'}
          </p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Run</th>
                <th>Event</th>
                <th>Detail</th>
                <th>At</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((entry) => (
                <tr key={entry.seq} className={severityOf(entry.event)}>
                  <td className="num">{entry.event.run_id}</td>
                  <td className="kind">{entry.event.kind.replace(/_/g, ' ')}</td>
                  <td>{describe(entry.event)}</td>
                  <td className="num">{entry.at.toLocaleTimeString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </main>
    </>
  );
}
