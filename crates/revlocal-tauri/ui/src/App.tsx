import { useEffect, useState } from 'react';
import { describe, inTauri, invoke, onRunEvent, severityOf, type UiEvent } from './ipc';

/** One row in the activity feed, with the moment it arrived. */
type Entry = { event: UiEvent; at: Date; seq: number };

/** Read something a rejected promise threw, without assuming it is an Error. */
function messageOf(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'remediation' in error) {
    return String((error as { remediation: unknown }).remediation);
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

export function App() {
  const [entries, setEntries] = useState<Entry[]>([]);
  const [connected, setConnected] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let seq = 0;
    let unsubscribe: (() => void) | undefined;
    let cancelled = false;

    // Two different failures, and they had the same symptom until this was split.
    //
    // Not being in the app at all is one thing. Being in the app and having the
    // subscription *rejected* — a capability Tauri did not grant — is another,
    // and it used to render as "open this from the app rather than a browser":
    // the one diagnosis guaranteed to be wrong for somebody already looking at
    // the app. §18 — an error must say what to do, and that one sent people to
    // do the thing they had already done.
    if (!inTauri()) {
      setNotice('Not connected. Open this window from the rev-local app rather than a browser.');
      return;
    }

    // §15: live updates come from events, not from polling the database. There is
    // no interval here and no fetch — if this component ever grows one, that rule
    // has been broken.
    onRunEvent((event) => {
      seq += 1;
      setEntries((previous) => [{ event, at: new Date(), seq }, ...previous].slice(0, 500));
    })
      .then((off) => {
        // An effect that has already been cleaned up must still release the
        // subscription it asked for, or a remount leaks one listener per mount.
        if (cancelled) {
          off();
          return;
        }
        unsubscribe = off;
        setConnected(true);
      })
      .catch((error: unknown) => {
        setNotice(`Live updates are not arriving — ${messageOf(error)}`);
      });

    return () => {
      cancelled = true;
      unsubscribe?.();
    };
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
      setNotice(`Could not engage the kill switch — ${messageOf(error)}`);
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
              : 'Not receiving events. See the message above.'}
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
