//! The kill switch (RL-804, SPEC §12.1).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.
//!
//! # Multi-threaded runtimes throughout (RL-1303)
//!
//! Every test here kills a real process, and a killed process can leave a child
//! holding the pipes. Under the default current-thread runtime a blocked drain
//! starves the timer, so `tokio::time::timeout` never fires and the test **hangs**
//! rather than failing — which stops every test binary queued behind it and turns
//! one bad assertion into a run that reports nothing.
//!
//! That is not hypothetical. It is what the Windows leg did four times before
//! SPEC §8.5's Job Object landed: forty-five minutes per round, each producing a
//! log whose last line was a test name.
//!
//! A regression here must be reportable.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Capability, CapabilitySet, Change, ChangeId, ChangeKind, Depth, DiffStat,
    EngineKind, PublishAction, PublishActionId, PublishActionStatus, PublishReceipt, Repo, RepoId,
    RepoKind, RiskClass, Run, RunId, RunStatus, TargetHealth, Timestamp, TriggerSource, Usage,
};
use revlocal_daemon::{
    cancels, may_dispatch, process_is_alive, reap, switch_detail, KillSwitch, PauseReport,
    CANCELLABLE,
};
use revlocal_publish::{PublishError, PublishQueue, PublishTarget, QueueConfig};
use revlocal_store::{
    open, ChangeStore, Pool, PublishActionStore, RepoStore, RunStore, SettingStore,
};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

async fn seeded() -> Result<(TempDir, Pool, RunId), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let pool = open(&dir.path().join("rev-local.db")).await?;

    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "rev-local".to_owned(),
            kind: RepoKind::Git,
            local_path: None,
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    let change = ChangeStore::new(&pool)
        .upsert(&Change {
            id: ChangeId::new(0),
            repo_id: repo.id,
            kind: ChangeKind::Commit,
            external_id: "deadbeef".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: None,
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: at(1),
        })
        .await?;

    let run = RunStore::new(&pool)
        .insert(&Run {
            id: RunId::new(0),
            change_id: change.id,
            attempt: 1,
            status: RunStatus::Publishing,
            engine: EngineKind::Mock,
            depth: Depth::Standard,
            trigger: TriggerSource::Manual,
            skip_reason: None,
            error: None,
            degraded: None,
            usage: Usage::default(),
            started_at: Some(at(2)),
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(2),
        })
        .await?;

    Ok((dir, pool, run.id))
}

struct CountingTarget {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl PublishTarget for CountingTarget {
    fn id(&self) -> &str {
        "andare"
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::CreateIssue]))
    }

    async fn execute(&self, _action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PublishReceipt {
            external_ref: Some("REVL-1".to_owned()),
            response_json: None,
            deduplicated: false,
        })
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::CreateIssue]),
            detail: None,
        })
    }
}

fn pending_action(run_id: RunId, key: &str) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(0),
        run_id,
        finding_id: None,
        target: "andare".to_owned(),
        capability: Capability::CreateIssue,
        risk: RiskClass::High,
        idempotency_key: key.to_owned(),
        payload_json: "{}".to_owned(),
        status: PublishActionStatus::Pending,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(3),
        sent_at: None,
    }
}

// --- criterion 1: a running engine is cancelled quickly -------------------

// §12.1 gives three seconds from kill switch to a stopped engine, on every
// platform. This test was `#[cfg(unix)]` for four CI rounds because it did not
// fail on Windows — it **hung**, past the ten-second timeout it already wraps the
// engine in, and a hung binary stops every test queued behind it.
//
// The cause was §8.5's missing Job Object: Windows has no process-group kill, so
// terminating a `.cmd` shim killed `cmd.exe` and left the `node` grandchild
// holding the pipes. The gate is removed now that the Job Object is implemented,
// which is what it said it was waiting for.
//
// It runs on Windows here for the first time, and this is the assertion that
// decides whether the Job Object works: nothing else in the suite kills a process
// that has a live grandchild.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_cancels_a_running_engine_within_three_seconds() {
    if std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        println!("SKIPPED (node not installed, nothing verified): kill_switch_cancels...");
        return;
    }

    let switch = KillSwitch::new();
    let token = switch.token().clone();

    let invocation = revlocal_engine::Invocation {
        program: revlocal_engine::mock_engine_program().display().to_string(),
        args: vec![],
        stdin: None,
    };
    let mut env = std::collections::BTreeMap::new();
    env.insert("MOCK_ENGINE_MODE".to_owned(), "hang".to_owned());
    env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
    env.insert("HOME".to_owned(), std::env::var("HOME").unwrap_or_default());

    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_path_buf();

    let engine = tokio::spawn(async move {
        revlocal_engine::supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &cwd,
            &env,
            // Far longer than the test's patience: if the timeout fired instead of
            // the switch, the assertion below would be measuring the wrong thing.
            Duration::from_secs(300),
            &token,
        )
        .await
    });

    // Let it get going, then pull the switch.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    switch.engage();

    let result = tokio::time::timeout(Duration::from_secs(10), engine)
        .await
        .unwrap_or_else(|e| panic!("the engine task did not finish: {e}"))
        .unwrap_or_else(|e| panic!("the engine task panicked: {e}"))
        .unwrap_or_else(|e| panic!("{e}"));

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "§12.1: cancelled within 3 seconds; took {elapsed:?}"
    );
    assert_eq!(
        result.killed,
        Some(revlocal_engine::KillReason::Cancelled),
        "and killed BY THE SWITCH, not by the timeout — a test that only checked \
         the process died would pass either way"
    );
}

#[test]
fn kill_switch_engaging_is_visible_and_releasing_makes_a_fresh_token() {
    let switch = KillSwitch::new();
    assert!(!switch.is_engaged());

    let watcher = switch.clone();
    switch.engage();
    assert!(switch.is_engaged());
    assert!(
        watcher.is_engaged(),
        "every clone observes the same cancellation, which is what lets one toggle \
         reach every run in flight"
    );

    let resumed = switch.released();
    assert!(!resumed.is_engaged());
    assert!(
        watcher.is_engaged(),
        "a cancelled token cannot be un-cancelled: work already told to stop must \
         not silently un-stop"
    );
}

// --- criterion 2: the publish queue is held, not drained -----------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_holds_pending_actions_and_sends_them_on_resume() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let store = PublishActionStore::new(&pool);
    let settings = SettingStore::new(&pool);

    for key in ["a", "b", "c"] {
        store
            .insert(&pending_action(run, key))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    settings
        .set_paused(true, at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let calls = Arc::new(AtomicUsize::new(0));
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(CountingTarget {
        calls: Arc::clone(&calls),
    }));

    // The daemon asks before dispatching. Paused means it does not.
    let paused = settings.is_paused().await.unwrap_or_else(|e| panic!("{e}"));
    assert!(paused);
    if may_dispatch(paused) {
        queue
            .dispatch_pending(at(5))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing went out");

    let still_pending = store
        .list_pending(at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        still_pending.len(),
        3,
        "§12.1 HOLDS the publish queue. Draining it would throw away actions that \
         were already decided — some by a human in the approvals inbox — because \
         somebody hit pause. Pause means stop, not undo"
    );

    // Resume.
    settings
        .set_paused(false, at(6))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let paused = settings.is_paused().await.unwrap_or_else(|e| panic!("{e}"));
    if may_dispatch(paused) {
        queue
            .dispatch_pending(at(7))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "and everything held goes out on resume"
    );
}

// --- criterion 3: paused survives a restart ------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_paused_state_survives_a_restart() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("rev-local.db");

    {
        let pool = open(&path).await.unwrap_or_else(|e| panic!("{e}"));
        SettingStore::new(&pool)
            .set_paused(true, at(4))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        pool.close().await;
    }

    // A different process, opening the same database.
    let pool = open(&path).await.unwrap_or_else(|e| panic!("{e}"));
    assert!(
        SettingStore::new(&pool)
            .is_paused()
            .await
            .unwrap_or_else(|e| panic!("{e}")),
        "if paused lived in memory, restarting a paused daemon would resume it — \
         and the most likely reason a paused daemon restarts is that something \
         went wrong, which is the worst moment to start writing to other people's \
         systems again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_a_fresh_database_is_not_paused() {
    let (_dir, pool, _run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !SettingStore::new(&pool)
            .is_paused()
            .await
            .unwrap_or_else(|e| panic!("{e}")),
        "a first start must not look like somebody had stopped it"
    );
}

// --- what a pause cancels -------------------------------------------------

#[test]
fn kill_switch_cancels_runs_in_flight_and_leaves_finished_ones_alone() {
    for status in CANCELLABLE {
        assert!(cancels(status), "{status:?} is in flight");
    }

    for status in [
        RunStatus::Done,
        RunStatus::Failed,
        RunStatus::Skipped,
        RunStatus::Cancelled,
    ] {
        assert!(
            !cancels(status),
            "{status:?} is finished; rewriting it would falsify history"
        );
    }
}

#[test]
fn kill_switch_the_report_names_what_is_waiting_not_only_what_stopped() {
    let report = PauseReport {
        runs_cancelled: vec![RunId::new(1), RunId::new(2)],
        actions_held: 5,
    };

    let summary = report.summary();
    assert!(summary.contains("cancelled 2 run(s)"), "{summary}");
    assert!(
        summary.contains("5 publish action(s) held"),
        "somebody who has just hit a kill switch needs to know what is waiting, or \
         resuming later comes as a surprise: {summary}"
    );
    assert!(summary.contains("sent on resume"), "{summary}");

    let detail = switch_detail(true, &report, at(4));
    assert_eq!(detail["engaged"], true);
    assert_eq!(detail["actions_held"], 5);
    assert_eq!(detail["runs_cancelled"], serde_json::json!([1, 2]));
}

// --- criterion 4: kill --hard leaves no orphan ---------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_hard_reaps_a_recorded_pid_and_leaves_nothing_behind() {
    // A real process rev-local could have started, on a run that has finished.
    // Null stdio, not inherited. RL-601 learned this the hard way: a child that
    // holds the harness's stderr open keeps the test binary alive long after the
    // test finished, and the run looks like a hang rather than a failure.
    let mut command = tokio::process::Command::new("sleep");
    command
        .arg("120")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group, exactly as RL-405 spawns an engine — so this test
    // exercises the group path rather than the fallback.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("spawning a victim: {e}"));
    let pid = child.id().unwrap_or_else(|| panic!("no pid"));

    assert!(
        process_is_alive(pid),
        "the probe must see a process that is definitely running, or the reap \
         below proves nothing"
    );

    assert!(reap(pid), "an orphan that is alive is signalled");

    // Give the kernel a moment to finish reaping, then confirm.
    for _ in 0..50 {
        if !process_is_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = child.wait().await;

    assert!(
        !process_is_alive(pid),
        "§12.1: no orphan engine process remains after kill --hard"
    );
}

#[test]
fn kill_switch_never_signals_pid_zero_or_one() {
    // Not a style point. On POSIX `kill(0, sig)` means "every process in the
    // caller's process group", so reaping a stored pid of 0 would have rev-local
    // killing itself and everything it had spawned. This test hung the harness
    // before the guard existed, because that is exactly what happened.
    assert!(!process_is_alive(0));
    assert!(
        !reap(0),
        "0 is the caller's own process group, not an orphan"
    );
    assert!(!reap(1), "1 is init");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_reaping_a_pid_that_is_already_gone_is_not_a_failure() {
    // A real pid that has definitely exited.
    let mut child = tokio::process::Command::new("true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    let pid = child.id().unwrap_or_else(|| panic!("no pid"));
    let _ = child.wait().await;

    assert!(
        !reap(pid),
        "a pid that is already gone is the normal case, and reporting it as a \
         failure would make --hard look broken every time it had nothing to do"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_orphan_candidates_are_pids_on_runs_that_have_finished() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let runs = RunStore::new(&pool);

    runs.set_engine_pid(run, Some(4242))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // Still running: not an orphan candidate.
    assert!(
        runs.orphan_pids()
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .is_empty(),
        "a pid on an active run is the engine doing its job"
    );

    runs.transition(run, RunStatus::Publishing, RunStatus::Cancelled)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let candidates = runs.orphan_pids().await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        candidates,
        vec![(run, 4242)],
        "a pid recorded against a run that has since finished is exactly what \
         --hard is looking for"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_a_cleared_pid_is_not_a_candidate() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let runs = RunStore::new(&pool);

    runs.set_engine_pid(run, Some(4242))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    runs.set_engine_pid(run, None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    runs.transition(run, RunStatus::Publishing, RunStatus::Done)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(
        runs.orphan_pids()
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .is_empty(),
        "an engine that exited cleanly cleared its pid, and re-signalling a reused \
         pid would kill a stranger's process"
    );
}
