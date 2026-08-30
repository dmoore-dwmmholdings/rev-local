import { useCallback, useEffect, useState } from 'react';
import {
  describe,
  approveAction,
  approveRun,
  editPayload,
  fetchApprovals,
  fetchDashboard,
  fetchFindings,
  emptyDraft,
  fetchInitialOnboardingStep,
  fetchInitialRepo,
  fetchIsFirstRun,
  fetchInitialRun,
  fetchInitialScreen,
  fetchRepository,
  fetchSettings,
  notify,
  onboardAddRepo,
  onboardFirstReview,
  reasonForApproval,
  refreshTray,
  fetchRun,
  fetchTranscript,
  fileToAndare,
  rejectAction,
  retryTarget,
  runDoctor,
  saveRepoConfig,
  setOverride,
  clearOverride,
  suppressFinding,
  inTauri,
  invoke,
  onRunEvent,
  setMode,
  severityOf,
  type Dashboard as DashboardData,
  type ApprovalsView as ApprovalsData,
  type FindingFilter,
  type FindingRow,
  type FindingsView as FindingsData,
  type QueuedAction,
  type RepositoryView as RepositoryData,
  type RunView as RunViewData,
  type SettingsView as SettingsData,
  type Draft,
  type FirstReview,
  type Step,
  STEPS,
  type Mode,
  type UiEvent,
} from './ipc';
import { Dashboard } from './Dashboard';
import { RunDetail } from './RunDetail';
import { Nav, initialScreen, type Screen } from './Nav';
import { Approvals } from './Approvals';
import { Findings } from './Findings';
import { Repository } from './Repository';
import { Settings } from './Settings';
import { Onboarding } from './Onboarding';

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
  const [dashboard, setDashboard] = useState<DashboardData | null>(null);
  const [screen, setScreen] = useState<Screen>('dashboard');
  const [run, setRun] = useState<RunViewData | null>(null);
  const [transcript, setTranscript] = useState<string | null>(null);
  const [approvals, setApprovals] = useState<ApprovalsData | null>(null);
  const [findings, setFindings] = useState<FindingsData | null>(null);
  const [filter, setFilter] = useState<FindingFilter>({});
  const [repository, setRepository] = useState<RepositoryData | null>(null);
  const [repoId, setRepoId] = useState<number | null>(null);
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [doctorRunning, setDoctorRunning] = useState(false);
  const [onboarding, setOnboarding] = useState(false);
  const [step, setStep] = useState<Step>('check');
  const [draft, setDraft] = useState<Draft>(emptyDraft());
  const [firstReview, setFirstReview] = useState<FirstReview | null>(null);
  const [onboardBusy, setOnboardBusy] = useState(false);
  const [onboardError, setOnboardError] = useState<string | null>(null);

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

  // §15: live updates come from events, not polling. There is no interval here —
  // the dashboard is re-read when an event says something changed, which is the
  // only thing that can change it.
  const reload = useCallback(() => {
    if (!inTauri()) return;
    fetchDashboard()
      .then(setDashboard)
      .catch((error: unknown) => setNotice(`Could not load the dashboard — ${messageOf(error)}`));
  }, []);

  // Asked once, on mount. Only a capture harness ever sets it.
  useEffect(() => {
    if (!inTauri()) return;
    fetchInitialScreen()
      .then((wanted) => {
        if (wanted) setScreen(initialScreen(wanted));
      })
      .catch(() => {});
    // The repository screen is about *a* repository, so a capture of it needs
    // one chosen. Only a harness sets this.
    fetchInitialRepo()
      .then((id) => {
        if (id > 0) setRepoId(id);
      })
      .catch(() => {});
    fetchInitialRun()
      .then((id) => {
        if (id > 0) openRun(id);
      })
      .catch(() => {
        // Not worth a notice. Failing to read an optional capture hint should
        // leave somebody on the dashboard, not staring at an error.
      });
  }, []);

  useEffect(reload, [reload]);
  useEffect(() => {
    if (entries.length > 0) reload();
  }, [entries.length, reload]);

  async function changeMode(next: Mode) {
    try {
      await setMode(next);
      reload();
    } catch (error: unknown) {
      setNotice(`Could not change the mode — ${messageOf(error)}`);
    }
  }

  // Opening a run switches screens and clears the previous transcript: showing
  // one run's log under another run's heading is the kind of wrong that looks
  // right.
  function openRun(runId: number) {
    setTranscript(null);
    setRun(null);
    setScreen('run');
    fetchRun(runId)
      .then(setRun)
      .catch((error: unknown) => setNotice(`Could not load run ${runId} — ${messageOf(error)}`));
  }

  function loadTranscript() {
    if (!run) return;
    fetchTranscript(run.run_id)
      .then(setTranscript)
      .catch((error: unknown) => setNotice(`Could not read the transcript — ${messageOf(error)}`));
  }

  async function retry(target: string) {
    if (!run) return;
    // §15: an outbound action names its target explicitly.
    const ok = window.confirm(
      `Re-queue this run's failed actions for ${target}?\n\n` +
        'Only failed actions are affected. Anything already delivered stays delivered.',
    );
    if (!ok) return;

    try {
      await retryTarget(run.run_id, target);
      openRun(run.run_id);
    } catch (error: unknown) {
      setNotice(`Could not retry ${target} — ${messageOf(error)}`);
    }
  }

  const reloadApprovals = useCallback(() => {
    if (!inTauri()) return;
    fetchApprovals()
      .then(setApprovals)
      .catch((error: unknown) => setNotice(`Could not load the inbox — ${messageOf(error)}`));
  }, []);

  // The inbox is read when its screen is opened, not on a timer. §15: live
  // updates come from events, and an inbox nobody is looking at does not need
  // refreshing.
  useEffect(() => {
    if (screen === 'approvals') reloadApprovals();
  }, [screen, reloadApprovals]);

  // §12.4's five actions. Every one that sends something names where it goes:
  // "are you sure?" tells nobody anything, and the target is the fact that
  // decides whether somebody should say yes.
  async function approveOne(action: QueuedAction) {
    const ok = window.confirm(
      `Send this ${action.capability} to ${action.target}?\n\n` +
        'It is dispatched as shown. An edit after approving is refused — the ' +
        'payload is fingerprinted now and checked again when it is sent.',
    );
    if (!ok) return;
    try {
      await approveAction(action.id);
      reloadApprovals();
    } catch (error: unknown) {
      setNotice(`Could not approve #${action.id} — ${messageOf(error)}`);
    }
  }

  async function approveWholeRun(runId: number, count: number) {
    // The count is in the confirmation because it is the blast radius, and a
    // number nobody saw is a number nobody agreed to.
    const ok = window.confirm(
      `Approve all ${count} queued action(s) for run #${runId}?\n\n` +
        'Each is dispatched as it stands. This does not affect other runs.',
    );
    if (!ok) return;
    try {
      await approveRun(runId);
      reloadApprovals();
    } catch (error: unknown) {
      setNotice(`Could not approve run #${runId} — ${messageOf(error)}`);
    }
  }

  async function rejectOne(action: QueuedAction, suppress: boolean) {
    const ok = window.confirm(
      suppress
        ? `Reject this ${action.capability} to ${action.target}, and stop proposing this finding?\n\n` +
            'Suppressing is the wider choice: the finding will not be raised again ' +
            'for any future run, not just this one.'
        : `Reject this ${action.capability} to ${action.target}?\n\n` +
            'Nothing is sent. The finding may be proposed again on a later run.',
    );
    if (!ok) return;
    try {
      await rejectAction(action.id, suppress);
      reloadApprovals();
    } catch (error: unknown) {
      setNotice(`Could not reject #${action.id} — ${messageOf(error)}`);
    }
  }

  async function editOne(action: QueuedAction) {
    const edited = window.prompt(
      `Edit the payload sent to ${action.target}. It is dispatched exactly as left here.`,
      action.payload_json,
    );
    if (edited === null || edited === action.payload_json) return;
    try {
      // Edit first, approve second — never together. §12.4's protection is that
      // the digest recorded at approval is re-checked at dispatch, so editing
      // after approving would invalidate the approval. That is the protection
      // working, and the order here is what keeps it from firing on our own edit.
      await editPayload(action.id, edited);
      reloadApprovals();
      setNotice('Payload edited. Approve it when you are ready — it is still queued.');
    } catch (error: unknown) {
      setNotice(`Could not edit #${action.id} — ${messageOf(error)}`);
    }
  }

  // Re-read whenever the filter changes, because the daemon does the filtering.
  // §15: no polling — this fires on a filter change and on nothing else.
  const reloadFindings = useCallback(
    (next: FindingFilter) => {
      if (!inTauri()) return;
      fetchFindings(next)
        .then(setFindings)
        .catch((error: unknown) => setNotice(`Could not load findings — ${messageOf(error)}`));
    },
    [],
  );

  useEffect(() => {
    if (screen === 'findings') reloadFindings(filter);
  }, [screen, filter, reloadFindings]);

  async function suppressOne(row: FindingRow) {
    // §15: a destructive action names its scope. Suppressing from here is scoped
    // to this repository — saying so is the difference between somebody agreeing
    // to silence a rule here and silencing it everywhere.
    const ok = window.confirm(
      `Stop proposing "${row.title}" in ${row.repo}?\n\n` +
        'It will not be raised again for this repository. Other repositories are ' +
        'unaffected — use `revlocal findings suppress` for a global suppression.',
    );
    if (!ok) return;
    try {
      await suppressFinding(row.id);
      reloadFindings(filter);
    } catch (error: unknown) {
      setNotice(`Could not suppress #${row.id} — ${messageOf(error)}`);
    }
  }

  async function fileOne(row: FindingRow) {
    const ok = window.confirm(
      `File "${row.title}" to Andare?\n\n` +
        'Creating an issue is high risk, so this obeys the repository\u2019s autonomy ' +
        'mode like any other publish — it may wait for approval rather than being sent.',
    );
    if (!ok) return;
    try {
      const status = await fileToAndare(row.id);
      // Reporting the status rather than "filed". Under the default mode it is
      // queued, and telling somebody it was sent would be a lie they act on.
      setNotice(
        status === 'awaiting_approval'
          ? 'Queued for approval — see the Approvals screen.'
          : status === 'skipped_dry_run'
            ? 'Recorded and not sent: this repository is in dry run.'
            : `Queued to send (${status}).`,
      );
      reloadFindings(filter);
    } catch (error: unknown) {
      setNotice(`Could not file #${row.id} — ${messageOf(error)}`);
    }
  }

  // Read when the screen opens or the repository changes, and after a save.
  // §15: no polling.
  const reloadRepository = useCallback((id: number | null) => {
    if (!inTauri() || id === null) return;
    fetchRepository(id)
      .then(setRepository)
      .catch((error: unknown) => setNotice(`Could not load the repository — ${messageOf(error)}`));
  }, []);

  useEffect(() => {
    if (screen === 'repository') reloadRepository(repoId);
  }, [screen, repoId, reloadRepository]);

  // Opening a repository clears the previous one: showing one repository's
  // config under another's name is the kind of wrong that looks right.
  function openRepository(id: number) {
    setRepository(null);
    setRepoId(id);
    setScreen('repository');
  }

  async function saveConfig(configJson: string) {
    if (repoId === null) throw new Error('no repository is open');
    // Deliberately not caught here. The editor shows the validation error
    // inline, beside the text it is about — an app-wide notice would put a line
    // and column a paragraph away from the line and column.
    await saveRepoConfig(repoId, configJson);
    reloadRepository(repoId);
  }

  // Read when the screen opens. Contacting every MCP server is a real cost, so
  // it happens on demand rather than on a timer — and §15 forbids polling anyway.
  const reloadSettings = useCallback(() => {
    if (!inTauri()) return;
    fetchSettings()
      .then(setSettings)
      .catch((error: unknown) => setNotice(`Could not load settings — ${messageOf(error)}`));
  }, []);

  useEffect(() => {
    if (screen === 'settings') reloadSettings();
  }, [screen, reloadSettings]);

  async function rerunDoctor() {
    setDoctorRunning(true);
    try {
      // The whole view comes back, not just the report: doctor checks the same
      // engines and targets the rest of the screen describes, and refreshing
      // half of it would leave two answers about one machine on screen.
      setSettings(await runDoctor());
    } catch (error: unknown) {
      setNotice(`Could not run doctor — ${messageOf(error)}`);
    } finally {
      setDoctorRunning(false);
    }
  }

  async function mapCapability(target: string, capability: string, tool: string) {
    // §15: an action that changes what rev-local will send names what it changes.
    const ok = window.confirm(
      `Bind ${target}'s "${capability}" to the tool "${tool}"?\n\n` +
        'rev-local will call that tool whenever it performs this capability. The ' +
        "tool's own required arguments are checked now, not at the first publish.",
    );
    if (!ok) return;
    try {
      // Empty arguments: the server's schema decides what is required, and it
      // refuses the binding if this is not enough. Better a refusal here than a
      // publish that silently did not happen.
      await setOverride(target, capability, tool, '{}');
      reloadSettings();
    } catch (error: unknown) {
      setNotice(`Could not map ${capability} — ${messageOf(error)}`);
    }
  }

  async function unmapCapability(target: string, capability: string) {
    const ok = window.confirm(
      `Remove your manual binding for ${target}'s "${capability}"?\n\n` +
        'rev-local goes back to resolving it against the tools the server exposes, ' +
        'which may leave it unmapped.',
    );
    if (!ok) return;
    try {
      await clearOverride(target, capability);
      reloadSettings();
    } catch (error: unknown) {
      setNotice(`Could not unmap ${capability} — ${messageOf(error)}`);
    }
  }

  // Tell somebody when something is waiting for them, and keep the tray honest.
  //
  // Driven by run events, not a timer: §15 forbids polling, and a new approval
  // can only appear because a run progressed. The daemon decides what is worth
  // showing and rate-limits it — sending an approval it has already shown is
  // harmless, because it is keyed by action id.
  const announce = useCallback(() => {
    if (!inTauri()) return;
    refreshTray().catch(() => {
      // A tray that could not be updated is not worth a notice in the window.
      // The window is the place somebody would be told, and they are looking at it.
    });
    fetchApprovals()
      .then((inbox) => {
        for (const action of inbox.waiting) {
          notify(reasonForApproval(action)).catch(() => {
            // Notifications are best-effort by nature: the OS may have denied
            // permission, and failing a review over that would be worse.
          });
        }
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (entries.length > 0) announce();
  }, [entries.length, announce]);

  // Offered on a fresh install, and re-runnable from Settings at any time.
  // Onboarding that can only happen once is a thing people are afraid to leave,
  // and the second repository deserves the same walk as the first.
  useEffect(() => {
    if (!inTauri()) return;
    // A harness asking for a step wins over the first-run check: it is capturing
    // one frame of the flow, not being onboarded.
    fetchInitialOnboardingStep()
      .then((wanted) => {
        if (STEPS.includes(wanted as Step)) {
          startOnboarding();
          setStep(wanted as Step);
          return;
        }
        return fetchIsFirstRun().then((first) => {
          if (first) startOnboarding();
        });
      })
      .catch(() => {
        // Not worth a notice: failing to *offer* onboarding leaves somebody on
        // the dashboard, which is where they would have ended up anyway.
      });
    // Asked once, on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function startOnboarding() {
    setStep('check');
    setDraft(emptyDraft());
    setFirstReview(null);
    setOnboardError(null);
    setOnboarding(true);
    // §8.4: doctor's output "is the first thing the UI shows on a fresh install".
    rerunDoctor().catch(() => {});
  }

  async function advanceOnboarding() {
    setOnboardError(null);
    const index = STEPS.indexOf(step);

    // Leaving the autonomy step is where the repository is actually created:
    // everything before it is a draft, so somebody who abandons the walk halfway
    // does not find a repository they never finished adding, polling a path they
    // were still choosing.
    if (step === 'pick_autonomy') {
      setOnboardBusy(true);
      try {
        const added = await onboardAddRepo(draft);
        setDraft({ ...draft, name: added.name });
        setStep('first_review');
      } catch (error: unknown) {
        setOnboardError(messageOf(error));
        return;
      } finally {
        setOnboardBusy(false);
      }
      return;
    }

    if (step === 'first_review') {
      setOnboardBusy(true);
      try {
        setFirstReview(await onboardFirstReview(draft.name));
      } catch (error: unknown) {
        setOnboardError(messageOf(error));
      } finally {
        setOnboardBusy(false);
      }
      return;
    }

    const next = STEPS[index + 1];
    if (next) setStep(next);
  }

  function finishOnboarding() {
    setOnboarding(false);
    reload();
    if (firstReview) openRun(firstReview.run_id);
  }

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
      // §12.1: the tray is the only part of the app on screen when the window is
      // hidden, so it has to say "paused" the moment this happens rather than at
      // the next run event — which, by construction, is not coming.
      await refreshTray().catch(() => '');
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

      {!onboarding && <Nav screen={screen} onSelect={setScreen} />}

      <main>
        {onboarding && (
          <Onboarding
            step={step}
            draft={draft}
            doctor={settings?.doctor ?? null}
            review={firstReview}
            busy={onboardBusy || doctorRunning}
            error={onboardError}
            onDraft={setDraft}
            onBack={() => {
              const index = STEPS.indexOf(step);
              const previous = STEPS[index - 1];
              if (previous) setStep(previous);
            }}
            onNext={advanceOnboarding}
            onFinish={finishOnboarding}
          />
        )}

        {!onboarding && screen === 'dashboard' && (
          <Dashboard
            dashboard={dashboard}
            onMode={changeMode}
            onOpenRun={openRun}
            onOpenRepo={openRepository}
          />
        )}

        {!onboarding && screen === 'approvals' && (
          <Approvals
            view={approvals}
            onApprove={approveOne}
            onApproveRun={approveWholeRun}
            onReject={rejectOne}
            onEdit={editOne}
          />
        )}

        {!onboarding && screen === 'repository' &&
          (repoId === null ? (
            // A screen about "a repository" with none chosen is not an error, and
            // an empty panel would read as one. It says where to choose.
            <p className="empty">Choose a repository from the dashboard.</p>
          ) : (
            <Repository view={repository} onOpenRun={openRun} onSave={saveConfig} />
          ))}

        {!onboarding && screen === 'findings' && (
          <Findings
            view={findings}
            filter={filter}
            onFilter={setFilter}
            onOpenRun={openRun}
            onSuppress={suppressOne}
            onFile={fileOne}
          />
        )}

        {!onboarding && screen === 'settings' && (
          <Settings
            view={settings}
            busy={doctorRunning}
            onRunDoctor={rerunDoctor}
            onMap={mapCapability}
            onUnmap={unmapCapability}
            onRunOnboarding={startOnboarding}
          />
        )}

        {!onboarding && screen === 'run' && (
          <RunDetail
            run={run}
            transcript={transcript}
            onExpandTranscript={loadTranscript}
            onRetry={retry}
          />
        )}

        {/* §15 puts live activity on the dashboard. It used to render under every
            screen, which put "Nothing has run yet" directly beneath a table of
            findings from run #1 — two true-sounding statements contradicting each
            other, which is worse than either alone. Found by reading a capture. */}
        {!onboarding && screen === 'dashboard' && (
          <>
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
          </>
        )}
      </main>
    </>
  );
}
