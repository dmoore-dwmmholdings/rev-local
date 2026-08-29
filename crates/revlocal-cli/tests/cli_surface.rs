//! The CLI surface (RL-1201, SPEC §14).
//!
//! §14 says this surface **is** the acceptance-test API. That makes two things
//! contractual rather than incidental: which commands exist, and what the exit
//! code means.
//!
//! This suite is deliberately honest about the surface being incomplete. §14 lists
//! sixteen command groups and five are implemented. A test that only checked the
//! five would pass while the surface stayed a third built, and would say nothing
//! about the eleven — so the list of what is missing is written down here and
//! checked from the spec, which means a command landing without being ticked off
//! fails, and so does a command being quietly dropped from §14.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

mod cli_surface {
    use std::path::PathBuf;
    use std::process::Command;

    use revlocal_cli::Exit;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// The command groups §14 lists, in its order.
    ///
    /// Read from the spec rather than transcribed, so a command added to §14 turns
    /// up here without anybody remembering to copy it.
    fn spec_commands() -> Result<Vec<String>, String> {
        let spec = std::fs::read_to_string(workspace_root().join("SPEC.md"))
            .map_err(|e| format!("reading SPEC.md: {e}"))?;

        let section = spec
            .split("## 14. CLI surface")
            .nth(1)
            .ok_or("SPEC.md has no §14")?;
        let block = section
            .split("```")
            .nth(1)
            .ok_or("§14 has no command block")?;

        let mut groups: Vec<String> = Vec::new();
        let mut push = |group: &str| {
            let group = group.trim().to_owned();
            if !group.is_empty() && !groups.contains(&group) {
                groups.push(group);
            }
        };

        for line in block.lines() {
            let line = line.split('#').next().unwrap_or(line).trim();
            let Some(rest) = line.strip_prefix("revlocal ") else {
                continue;
            };

            // §14 uses `|` for two different things, and telling them apart is the
            // whole of this parser.
            //
            //   revlocal pause | resume | kill --hard      three top-level commands
            //   revlocal repo list | show <name> | ...     one command, three subs
            //
            // The tell is how many words precede the first `|`. One word means the
            // alternatives are siblings of it; more means they are alternatives
            // *within* it. Getting this wrong is not hypothetical — the first
            // version read only `pause` and then reported `resume` and `kill` as
            // commands §14 does not list.
            let mut segments = rest.split('|');
            let Some(first) = segments.next() else {
                continue;
            };
            let head: Vec<&str> = first.split_whitespace().collect();
            let Some(group) = head.first() else {
                continue;
            };
            push(group);

            if head.len() == 1 {
                for sibling in segments {
                    if let Some(name) = sibling.split_whitespace().next() {
                        push(name);
                    }
                }
            }
        }
        Ok(groups)
    }

    /// Command groups that exist today.
    const IMPLEMENTED: &[&str] = &[
        "db",
        "publish",
        "targets",
        "review",
        "repo",
        "pause",
        "resume",
        "kill",
        "doctor",
        "hooks",
        "approvals",
        "budget",
    ];

    /// Command groups §14 names that are not built yet, and what each waits on.
    ///
    /// An entry is a claim, not a placeholder: naming the blocker is what keeps
    /// this from becoming a list nobody revisits.
    const NOT_YET: &[(&str, &str)] = &[
        (
            "watch",
            "needs the daemon main loop; the pieces exist, nothing runs them",
        ),
        (
            "backfill",
            "RL-1007 built the scheduler; this is its front end",
        ),
        ("runs", "needs the run store surfaced, which RL-1201 does"),
        ("findings", "same, plus suppression writes"),
        (
            "webhook",
            "RL-1005 and RL-1006 built the listener and tunnels",
        ),
    ];

    fn binary() -> PathBuf {
        // The test binary lives beside the CLI binary cargo just built.
        let mut path = std::env::current_exe().unwrap_or_default();
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.join(if cfg!(windows) {
            "revlocal.exe"
        } else {
            "revlocal"
        })
    }

    #[test]
    fn every_command_in_spec_14_is_implemented_or_listed() -> Result<(), String> {
        // Criterion 1, stated honestly. The surface is a third built; this makes
        // the other two thirds visible instead of leaving them to be rediscovered
        // when M14 tries to close.
        let spec = spec_commands()?;
        assert!(
            spec.len() > 10,
            "only {} commands parsed from §14; the parser has stopped matching",
            spec.len()
        );

        let unaccounted: Vec<&String> = spec
            .iter()
            .filter(|group| {
                !IMPLEMENTED.contains(&group.as_str())
                    && !NOT_YET.iter().any(|(name, _)| name == group)
            })
            .collect();

        assert!(
            unaccounted.is_empty(),
            "§14 lists commands this suite does not account for. Implement them, or \
             add them to NOT_YET with what they wait on:\n  {unaccounted:?}"
        );
        Ok(())
    }

    #[test]
    fn nothing_claims_to_be_implemented_that_spec_14_does_not_list() -> Result<(), String> {
        // The other direction. A command that exists and is not in §14 is a
        // surface nobody specified and nobody will test.
        let spec = spec_commands()?;
        let stray: Vec<&&str> = IMPLEMENTED
            .iter()
            .filter(|group| !spec.iter().any(|listed| listed == *group))
            .collect();

        assert!(
            stray.is_empty(),
            "these commands exist but §14 does not list them: {stray:?}"
        );
        Ok(())
    }

    #[test]
    fn the_not_yet_list_says_what_each_command_waits_on() {
        // A list of missing things with no reasons is a list nobody revisits.
        for (command, blocker) in NOT_YET {
            assert!(
                blocker.len() > 15,
                "`{command}` is listed as not built with no reason: {blocker:?}"
            );
        }
    }

    #[test]
    fn exit_codes_are_the_ones_spec_14_names() {
        // §14: 0 ok, 1 error, 2 usage, 3 blocked-by-budget, 4 awaiting-approval.
        assert_eq!(Exit::Ok.code(), 0);
        assert_eq!(Exit::Error.code(), 1);
        assert_eq!(Exit::Usage.code(), 2);
        assert_eq!(Exit::BlockedByBudget.code(), 3);
        assert_eq!(Exit::AwaitingApproval.code(), 4);

        // Distinct, or a caller cannot branch on them.
        let codes: std::collections::BTreeSet<u8> = Exit::ALL.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), Exit::ALL.len());
    }

    #[test]
    fn only_a_plain_error_is_worth_retrying() {
        // The question a CI job actually asks. Collapsing 3 and 4 into 1 would make
        // a job waiting for approval indistinguishable from one that failed — and
        // the usual response to a failure, retrying, is exactly wrong for both.
        assert!(Exit::Error.is_worth_retrying());
        assert!(!Exit::Ok.is_worth_retrying());
        assert!(!Exit::Usage.is_worth_retrying());
        assert!(!Exit::BlockedByBudget.is_worth_retrying());
        assert!(!Exit::AwaitingApproval.is_worth_retrying());
    }

    #[test]
    fn every_exit_code_explains_itself() {
        // Criterion 3 says the codes are documented. A number with no sentence is
        // documented in the sense that it appears in a list.
        for exit in Exit::ALL {
            let description = exit.describe();
            assert!(description.len() > 15, "{exit:?}: {description:?}");
        }
        // The two that surprise people say why retrying will not help.
        assert!(Exit::BlockedByBudget.describe().contains("will not help"));
        assert!(Exit::AwaitingApproval.describe().contains("will not help"));
    }

    #[test]
    fn help_is_coherent_for_someone_who_has_not_read_the_spec() -> Result<(), String> {
        // Criterion 4. `--help` is where somebody meets this tool, and a help text
        // that assumes the spec is a help text for people who do not need it.
        let output = Command::new(binary())
            .arg("--help")
            .output()
            .map_err(|e| format!("running revlocal --help: {e}"))?;

        assert!(output.status.success(), "--help must exit 0");
        let help = String::from_utf8_lossy(&output.stdout);

        // It says what the tool is for.
        assert!(
            help.to_lowercase().contains("review"),
            "--help does not say what rev-local does:\n{help}"
        );
        // The exit codes are in it, so a script author does not need §14.
        for exit in Exit::ALL {
            assert!(
                help.contains(&format!("  {}  ", exit.code())),
                "--help does not document exit code {}:\n{help}",
                exit.code()
            );
        }
        assert!(
            help.contains("--json"),
            "--help does not mention --json, which every command accepts:\n{help}"
        );
        Ok(())
    }

    #[test]
    fn an_unknown_command_exits_two_not_one() -> Result<(), String> {
        // §14's code 2 is "you typed it wrong", and clap already uses it. Asserted
        // because it is contract rather than coincidence: a caller distinguishing
        // "my invocation is wrong" from "the run failed" depends on it.
        let output = Command::new(binary())
            .arg("not-a-command")
            .output()
            .map_err(|e| format!("running revlocal: {e}"))?;

        assert_eq!(
            output.status.code(),
            Some(i32::from(Exit::Usage.code())),
            "an unknown command must exit {} (usage), not {}",
            Exit::Usage.code(),
            Exit::Error.code()
        );
        Ok(())
    }
}

// --- the operator's emergency controls (RL-1201, §12.1) --------------------

mod control {
    use revlocal_cli::control::{kill_hard, pause, render, resume, status};
    use revlocal_store::Pool;

    async fn store() -> Result<(Pool, tempfile::TempDir), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let pool = revlocal_store::open(&dir.path().join("rl.db"))
            .await
            .map_err(|e| e.to_string())?;

        Ok((pool, dir))
    }

    fn now() -> revlocal_core::Timestamp {
        chrono::Utc::now()
    }

    #[tokio::test]
    async fn a_fresh_install_is_not_paused() -> Result<(), String> {
        // Absent means running. Defaulting the other way would make a first start
        // look like somebody had stopped it.
        let (pool, _dir) = store().await?;
        let report = status(&pool).await.map_err(|e| e.to_string())?;

        assert!(!report.paused);
        assert!(!report.changed, "asking is not doing");
        assert_eq!(report.detail, "running");
        Ok(())
    }

    #[tokio::test]
    async fn pausing_twice_reports_that_nothing_changed() -> Result<(), String> {
        // `paused` and `changed` are separate so a script can tell "I stopped it"
        // from "it was already stopped" — which matters when two operators reach
        // for the switch at once and only one should be writing the incident note.
        let (pool, _dir) = store().await?;

        let first = pause(&pool, now()).await.map_err(|e| e.to_string())?;
        assert!(first.paused && first.changed);

        let second = pause(&pool, now()).await.map_err(|e| e.to_string())?;
        assert!(second.paused, "still paused");
        assert!(!second.changed, "the second pause changed nothing");
        assert!(
            second.detail.contains("already paused"),
            "{}",
            second.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn paused_state_survives_reopening_the_database() -> Result<(), String> {
        // The case RL-804 built this for: somebody pauses because something is
        // wrong, the daemon restarts while they investigate, and it must not
        // quietly start reviewing again.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = dir.path().join("rl.db");

        let pool = revlocal_store::open(&path)
            .await
            .map_err(|e| e.to_string())?;
        pause(&pool, now()).await.map_err(|e| e.to_string())?;
        pool.close().await;

        let reopened = revlocal_store::open(&path)
            .await
            .map_err(|e| e.to_string())?;
        let after = status(&reopened).await.map_err(|e| e.to_string())?;
        assert!(after.paused, "a restart must not release the switch");
        Ok(())
    }

    #[tokio::test]
    async fn resuming_says_the_held_actions_will_be_sent() -> Result<(), String> {
        // Somebody resuming needs to know what is about to happen. "Resumed" alone
        // makes the publish actions a surprise.
        let (pool, _dir) = store().await?;
        pause(&pool, now()).await.map_err(|e| e.to_string())?;

        let report = resume(&pool, now()).await.map_err(|e| e.to_string())?;
        assert!(!report.paused);
        assert!(report.changed);
        assert!(report.detail.contains("will be sent"), "{}", report.detail);
        Ok(())
    }

    #[tokio::test]
    async fn resuming_when_not_paused_changes_nothing() -> Result<(), String> {
        let (pool, _dir) = store().await?;
        let report = resume(&pool, now()).await.map_err(|e| e.to_string())?;

        assert!(!report.paused);
        assert!(!report.changed);
        Ok(())
    }

    #[tokio::test]
    async fn kill_hard_pauses_too_and_says_what_it_costs() -> Result<(), String> {
        // A hard kill is a pause *plus* reaping. Reporting only the reaping would
        // hide that reviewing has also stopped until somebody resumes.
        let (pool, _dir) = store().await?;
        let report = kill_hard(&pool, now()).await.map_err(|e| e.to_string())?;

        assert!(report.paused, "a hard kill also pauses");
        assert_eq!(report.action, "kill");
        assert!(
            report.detail.contains("is lost"),
            "must say output is lost: {}",
            report.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_json_shape_is_stable_and_complete() -> Result<(), String> {
        // §14: --json is the acceptance-test API. Fields present even when zero,
        // so the shape does not change the day they start counting.
        let (pool, _dir) = store().await?;
        let report = pause(&pool, now()).await.map_err(|e| e.to_string())?;
        let json = render(&report, true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        for field in [
            "action",
            "paused",
            "changed",
            "runs_cancelled",
            "actions_held",
            "processes_reaped",
            "detail",
        ] {
            assert!(
                !parsed[field].is_null(),
                "--json is missing `{field}`: {json}"
            );
        }
        assert_eq!(parsed["action"], "pause");
        assert!(parsed["runs_cancelled"].is_array());
        Ok(())
    }
}

// --- hooks (RL-1201, §7.2) -------------------------------------------------

mod hooks_command {
    use std::path::Path;

    use revlocal_cli::hooks::{install, render, uninstall};
    use revlocal_daemon::hooks::HookMode;

    /// A repository with one hook somebody else wrote.
    fn repo_with_their_hook(dir: &Path) -> Result<String, String> {
        let hooks = dir.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).map_err(|e| e.to_string())?;
        let theirs = "#!/bin/sh\n# somebody else wrote this\nnpx lint-staged\nexit 0\n";
        std::fs::write(hooks.join("post-commit"), theirs).map_err(|e| e.to_string())?;
        Ok(theirs.to_owned())
    }

    #[test]
    fn install_says_which_hooks_it_appended_to_rather_than_just_installed() -> Result<(), String> {
        // Somebody putting this into a repository that already has hooks wants to
        // know theirs was appended to and not overwritten — and wants to know it
        // without opening the file.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        repo_with_their_hook(dir.path())?;

        let report = install(dir.path(), "acme-api", HookMode::Reference, 41791, "S")
            .map_err(|e| e.to_string())?;

        assert_eq!(report.hooks.len(), 3, "§7.2's three reference-mode hooks");
        assert!(
            report.detail.contains("not overwritten"),
            "{}",
            report.detail
        );

        let appended: Vec<&str> = report
            .hooks
            .iter()
            .filter(|h| h.action == "appended")
            .map(|h| h.path.as_str())
            .collect();
        assert_eq!(appended.len(), 1, "only theirs existed");
        assert!(appended[0].ends_with("post-commit"));
        Ok(())
    }

    #[test]
    fn uninstall_leaves_their_hook_byte_identical() -> Result<(), String> {
        // The property that makes `install` safe to try. RL-1004 owns it; this
        // asserts the CLI actually exposes it rather than promising it.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let theirs = repo_with_their_hook(dir.path())?;

        install(dir.path(), "acme-api", HookMode::Reference, 41791, "S")
            .map_err(|e| e.to_string())?;
        uninstall(dir.path(), "acme-api", HookMode::Reference).map_err(|e| e.to_string())?;

        let after = std::fs::read_to_string(dir.path().join(".git/hooks/post-commit"))
            .map_err(|e| e.to_string())?;
        assert_eq!(after, theirs, "their hook must come back exactly as it was");

        // And the ones rev-local wrote entirely are gone, not left inert.
        assert!(!dir.path().join(".git/hooks/post-merge").exists());
        Ok(())
    }

    #[test]
    fn uninstalling_what_was_never_installed_answers_rather_than_complains() -> Result<(), String> {
        // Running this to check is how somebody finds out. It should answer.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir.path().join(".git").join("hooks"))
            .map_err(|e| e.to_string())?;

        let report =
            uninstall(dir.path(), "acme-api", HookMode::Reference).map_err(|e| e.to_string())?;

        assert!(report.hooks.iter().all(|h| !h.changed));
        assert!(
            report.detail.contains("nothing changed"),
            "{}",
            report.detail
        );
        Ok(())
    }

    #[test]
    fn bare_mirror_mode_installs_the_one_hook_that_sees_every_push() -> Result<(), String> {
        // §7.2: post-receive on a bare mirror is the only way to see every pushed
        // ref, including deletions.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir.path().join("hooks")).map_err(|e| e.to_string())?;

        let report = install(dir.path(), "acme-api", HookMode::BareMirror, 41791, "S")
            .map_err(|e| e.to_string())?;

        assert_eq!(report.hooks.len(), 1);
        assert!(report.hooks[0].path.ends_with("post-receive"));
        assert_eq!(report.mode, "bare-mirror");
        Ok(())
    }

    #[test]
    fn the_json_names_every_file_and_what_happened_to_it() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        repo_with_their_hook(dir.path())?;
        let report = install(dir.path(), "acme-api", HookMode::Reference, 41791, "S")
            .map_err(|e| e.to_string())?;

        let json = render(&report, true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        assert_eq!(parsed["command"], "install");
        assert_eq!(parsed["mode"], "reference");
        let hooks = parsed["hooks"].as_array().ok_or("hooks must be an array")?;
        assert_eq!(hooks.len(), 3);
        for hook in hooks {
            // Per file, so a script can tell which were touched.
            assert!(hook["path"].is_string());
            assert!(hook["action"].is_string());
            assert!(hook["changed"].is_boolean());
        }
        Ok(())
    }

    #[test]
    fn a_path_that_is_not_a_repository_says_what_to_do() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let error = install(dir.path(), "x", HookMode::Reference, 1, "S")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(
            error.contains("does not look like a git repository"),
            "{error}"
        );
        assert!(error.contains("try:"), "{error}");
        Ok(())
    }
}

// --- approvals and budget (RL-1201, §12.4, §13.1) --------------------------

mod inspect_commands {
    use revlocal_cli::inspect::{approvals, budget, render};
    use revlocal_core::{BudgetSettings, RepoId, Usage};
    #[allow(unused_imports)]
    use revlocal_store::RepoStore;
    use revlocal_store::{BudgetLedgerStore, Pool};

    /// A store with one repository in it.
    ///
    /// `budget_ledger` has a foreign key to `repo`, which is right — a ledger row
    /// for a repository that does not exist is a row nobody can explain — and it
    /// means these tests need a real one rather than a bare id.
    async fn store() -> Result<(Pool, tempfile::TempDir, RepoId), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let pool = revlocal_store::open(&dir.path().join("rl.db"))
            .await
            .map_err(|e| e.to_string())?;

        let now = chrono::Utc::now();
        let repo = revlocal_store::RepoStore::new(&pool)
            .insert(&revlocal_core::Repo {
                id: RepoId::new(0),
                name: "acme-api".to_owned(),
                kind: revlocal_core::RepoKind::Git,
                local_path: None,
                remote_url: None,
                default_branch: Some("main".to_owned()),
                engine: revlocal_core::EngineKind::Mock,
                autonomy: revlocal_core::AutonomyMode::DryRun,
                enabled: true,
                config_json: "{}".to_owned(),
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|e| e.to_string())?;

        Ok((pool, dir, repo.id))
    }

    #[tokio::test]
    async fn an_empty_inbox_says_so_rather_than_printing_nothing() -> Result<(), String> {
        // An empty list rendered as nothing is indistinguishable from a command
        // that failed to read anything.
        let (pool, _dir, _repo_id) = store().await?;
        let report = approvals(&pool).await.map_err(|e| e.to_string())?;

        assert!(report.waiting.is_empty());
        let human = report.render_human();
        assert!(human.contains("Nothing is waiting"), "{human}");
        Ok(())
    }

    #[tokio::test]
    async fn a_day_with_no_runs_is_not_a_day_nobody_measured() -> Result<(), String> {
        // The bug this test exists for, found by running the command rather than
        // reading it. `Usage::default()` means "unmeasured" (RL-409, deliberately),
        // and using it for an absent ledger row made a fresh install report
        // "0 tokens (at least — one run reported no count)" when nothing had run.
        let (pool, _dir, repo_id) = store().await?;
        let report = budget(
            &pool,
            repo_id,
            chrono::Utc::now(),
            &BudgetSettings::default(),
        )
        .await
        .map_err(|e| e.to_string())?;

        assert_eq!(report.runs, 0);
        assert!(report.tokens_known, "nothing ran, so nothing is unmeasured");
        assert!(report.cost_known);
        assert!(report.may_run);

        let human = report.render_human();
        assert!(
            !human.contains("at least"),
            "a day with no runs must not hedge its zero:\n{human}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_day_containing_an_unmeasured_run_does_hedge() -> Result<(), String> {
        // The other half. One run whose tokens nobody counted makes the day's
        // total a lower bound, and printing it as a total would be the exact
        // failure RL-409 fixed.
        let (pool, _dir, repo_id) = store().await?;
        BudgetLedgerStore::new(&pool)
            .add_run(
                repo_id,
                &revlocal_daemon::budgets::day_of(chrono::Utc::now()),
                1,
                &Usage::default(),
            )
            .await
            .map_err(|e| e.to_string())?;

        let report = budget(
            &pool,
            repo_id,
            chrono::Utc::now(),
            &BudgetSettings::default(),
        )
        .await
        .map_err(|e| e.to_string())?;

        assert_eq!(report.runs, 1);
        assert!(!report.tokens_known, "the run reported no count");
        let human = report.render_human();
        assert!(human.contains("at least"), "{human}");
        Ok(())
    }

    #[tokio::test]
    async fn a_measured_run_is_reported_as_a_total() -> Result<(), String> {
        let (pool, _dir, repo_id) = store().await?;
        BudgetLedgerStore::new(&pool)
            .add_run(
                repo_id,
                &revlocal_daemon::budgets::day_of(chrono::Utc::now()),
                1,
                &Usage::measured(1_000, 200).with_cost(0.25),
            )
            .await
            .map_err(|e| e.to_string())?;

        let report = budget(
            &pool,
            repo_id,
            chrono::Utc::now(),
            &BudgetSettings::default(),
        )
        .await
        .map_err(|e| e.to_string())?;

        assert_eq!(report.tokens, 1_200);
        assert!(report.tokens_known);
        assert!(report.cost_known);
        assert!(!report.render_human().contains("at least"));
        Ok(())
    }

    #[tokio::test]
    async fn an_exhausted_budget_says_why_it_is_holding() -> Result<(), String> {
        // §18: "may_run: false" without a reason is the system going quiet.
        let (pool, _dir, repo_id) = store().await?;
        let settings = BudgetSettings {
            daily_runs_per_repo: 1,
            ..BudgetSettings::default()
        };
        BudgetLedgerStore::new(&pool)
            .add_run(
                repo_id,
                &revlocal_daemon::budgets::day_of(chrono::Utc::now()),
                1,
                &Usage::measured(10, 10),
            )
            .await
            .map_err(|e| e.to_string())?;

        let report = budget(&pool, repo_id, chrono::Utc::now(), &settings)
            .await
            .map_err(|e| e.to_string())?;

        assert!(!report.may_run);
        let reason = report.reason.clone().unwrap_or_default();
        assert!(reason.contains("run budget"), "{reason}");
        assert!(report.render_human().contains("Holding:"));

        // And the reason survives into --json, where a script reads it.
        let json = render(&report, report.render_human(), true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert_eq!(parsed["may_run"], false);
        assert!(parsed["reason"].is_string());
        Ok(())
    }

    #[tokio::test]
    async fn a_healthy_budget_omits_the_reason_rather_than_nulling_it() -> Result<(), String> {
        // Its presence means "something is stopping this", which is only true if
        // it is absent when nothing is.
        let (pool, _dir, repo_id) = store().await?;
        let report = budget(
            &pool,
            repo_id,
            chrono::Utc::now(),
            &BudgetSettings::default(),
        )
        .await
        .map_err(|e| e.to_string())?;

        let json = render(&report, report.render_human(), true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert!(parsed.get("reason").is_none(), "{json}");
        Ok(())
    }
}
