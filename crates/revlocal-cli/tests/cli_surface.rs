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
        "runs",
        "findings",
        "watch",
        "backfill",
    ];

    /// Command groups §14 names that are not built yet, and what each waits on.
    ///
    /// An entry is a claim, not a placeholder: naming the blocker is what keeps
    /// this from becoming a list nobody revisits.
    const NOT_YET: &[(&str, &str)] = &[(
        "webhook",
        "RL-1005 and RL-1006 built the listener and tunnels",
    )];

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

// --- repo add | list | remove | set (RL-1201, §14) -------------------------

mod repo_commands {
    use revlocal_cli::repo::{add, remove, render_write, set};
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
    async fn a_new_repository_defaults_to_doing_nothing_unattended() -> Result<(), String> {
        // The default that matters most. A repository added a moment ago has never
        // been reviewed and nobody has seen its findings — the first thing it does
        // should not be to publish them.
        let (pool, _dir) = store().await?;
        let report = add(
            &pool,
            "/work/acme-api",
            "git",
            None,
            "claude",
            "dry_run",
            now(),
        )
        .await
        .map_err(|e| e.to_string())?;

        assert_eq!(report.name, "acme-api", "the name is derived from the path");
        assert!(
            report.detail.contains("nothing is published"),
            "{}",
            report.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_duplicate_name_is_refused_rather_than_merged() -> Result<(), String> {
        // §5 makes `repo.name` unique, and the name is what hooks send and what
        // findings are fingerprinted against — so a second one would silently merge
        // two repositories' history.
        let (pool, _dir) = store().await?;
        add(
            &pool,
            "/work/acme-api",
            "git",
            None,
            "claude",
            "dry_run",
            now(),
        )
        .await
        .map_err(|e| e.to_string())?;

        let second = add(
            &pool,
            "/elsewhere/acme-api",
            "git",
            None,
            "claude",
            "dry_run",
            now(),
        )
        .await;
        let error = second.err().map(|e| e.to_string()).unwrap_or_default();

        assert!(error.contains("already configured"), "{error}");
        assert!(
            error.contains("repo remove acme-api"),
            "must offer a way out: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_value_names_the_ones_that_exist() -> Result<(), String> {
        // A message saying only "invalid engine" makes somebody go and find the
        // list. The list is three words long.
        let (pool, _dir) = store().await?;
        let error = add(&pool, "/w/x", "git", None, "nonsense", "dry_run", now())
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(error.contains("claude"), "{error}");
        assert!(error.contains("codex"), "{error}");
        assert!(error.contains("mock"), "{error}");
        Ok(())
    }

    #[tokio::test]
    async fn a_url_is_stored_as_a_remote_and_a_path_as_a_path() -> Result<(), String> {
        // They mean different things to every adapter downstream, and guessing
        // wrong makes a local review try to fetch.
        let (pool, _dir) = store().await?;
        add(
            &pool,
            "https://github.com/acme/api.git",
            "github",
            None,
            "claude",
            "dry_run",
            now(),
        )
        .await
        .map_err(|e| e.to_string())?;

        let repos = revlocal_store::RepoStore::new(&pool)
            .list()
            .await
            .map_err(|e| e.to_string())?;
        let stored = repos.first().ok_or("the repository must exist")?;

        assert_eq!(stored.name, "api", "`.git` is trimmed from a derived name");
        assert!(stored.remote_url.is_some());
        assert!(stored.local_path.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn set_changes_what_it_names_and_says_what_it_changed() -> Result<(), String> {
        let (pool, _dir) = store().await?;
        add(&pool, "/w/acme", "git", None, "claude", "dry_run", now())
            .await
            .map_err(|e| e.to_string())?;

        let report = set(
            &pool,
            "acme",
            &["engine=codex".to_owned(), "autonomy=auto".to_owned()],
            now(),
        )
        .await
        .map_err(|e| e.to_string())?;

        assert!(report.detail.contains("engine=codex"), "{}", report.detail);
        assert!(report.detail.contains("autonomy=auto"), "{}", report.detail);

        let repos = revlocal_store::RepoStore::new(&pool)
            .list()
            .await
            .map_err(|e| e.to_string())?;
        let stored = repos.first().ok_or("must exist")?;
        assert_eq!(stored.engine, revlocal_core::EngineKind::Codex);
        assert_eq!(stored.autonomy, revlocal_core::AutonomyMode::Auto);
        Ok(())
    }

    #[tokio::test]
    async fn set_rejects_the_whole_change_when_one_pair_is_wrong() -> Result<(), String> {
        // Applying the good half of a bad command leaves the repository in a state
        // nobody asked for, and the user cannot tell which half took.
        let (pool, _dir) = store().await?;
        add(&pool, "/w/acme", "git", None, "claude", "dry_run", now())
            .await
            .map_err(|e| e.to_string())?;

        let failed = set(
            &pool,
            "acme",
            &["engine=codex".to_owned(), "autonomy=nonsense".to_owned()],
            now(),
        )
        .await;
        assert!(failed.is_err());

        let repos = revlocal_store::RepoStore::new(&pool)
            .list()
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(
            repos.first().ok_or("must exist")?.engine,
            revlocal_core::EngineKind::Claude,
            "the valid half must not have been applied"
        );
        Ok(())
    }

    #[tokio::test]
    async fn removing_says_what_it_did_not_remove() -> Result<(), String> {
        // Hooks live in the working copy, not the database. Somebody who removes a
        // repository and finds their commits still firing a hook deserves to have
        // been told.
        let (pool, _dir) = store().await?;
        add(&pool, "/w/acme", "git", None, "claude", "dry_run", now())
            .await
            .map_err(|e| e.to_string())?;

        let report = remove(&pool, "acme").await.map_err(|e| e.to_string())?;
        assert!(report.detail.contains("hooks"), "{}", report.detail);
        assert!(
            report.detail.contains("hooks uninstall"),
            "must name the command: {}",
            report.detail
        );

        assert!(remove(&pool, "acme").await.is_err(), "it is gone");
        Ok(())
    }

    #[tokio::test]
    async fn a_write_report_round_trips_as_json() -> Result<(), String> {
        let (pool, _dir) = store().await?;
        let report = add(&pool, "/w/acme", "git", None, "claude", "dry_run", now())
            .await
            .map_err(|e| e.to_string())?;
        let json = render_write(&report, true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        assert_eq!(parsed["action"], "add");
        assert_eq!(parsed["name"], "acme");
        assert!(parsed["repo_id"].is_number());
        Ok(())
    }
}

// --- runs and findings (RL-1201, §14) --------------------------------------

mod runs_and_findings {
    use revlocal_cli::inspect::{parse_severity, parse_status, run_detail, runs};
    use revlocal_core::{RepoId, RunStatus};

    #[test]
    fn an_unknown_status_names_every_one_that_exists() {
        let error = parse_status("nonsense")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        // All ten, because somebody guessing "running" should be shown "reviewing"
        // rather than sent to the spec.
        for status in ["queued", "reviewing", "done", "failed", "cancelled"] {
            assert!(error.contains(status), "{status} missing from: {error}");
        }
        assert_eq!(parse_status("reviewing").ok(), Some(RunStatus::Reviewing));
    }

    #[test]
    fn an_unknown_severity_names_every_one_that_exists() {
        let error = parse_severity("nope")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        for severity in ["info", "low", "medium", "high", "critical"] {
            assert!(error.contains(severity), "{severity} missing from: {error}");
        }
    }

    #[tokio::test]
    async fn a_missing_run_is_told_apart_from_a_broken_database() -> Result<(), String> {
        // The remedies are opposite. A store failure means `db migrate`; a missing
        // id means the database is fine and the id is not, and offering `db
        // migrate` sends somebody to fix something that is not broken.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let pool = revlocal_store::open(&dir.path().join("rl.db"))
            .await
            .map_err(|e| e.to_string())?;

        let error = run_detail(&pool, revlocal_core::RunId::new(999))
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(error.contains("no run with id 999"), "{error}");
        assert!(
            error.contains("runs list"),
            "must point somewhere useful: {error}"
        );
        assert!(
            !error.contains("db migrate"),
            "the database is not what is wrong: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_empty_list_says_so_and_reports_what_it_matched() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let pool = revlocal_store::open(&dir.path().join("rl.db"))
            .await
            .map_err(|e| e.to_string())?;

        let report = runs(&pool, Some(RepoId::new(1)), None, 20)
            .await
            .map_err(|e| e.to_string())?;

        assert!(report.runs.is_empty());
        assert_eq!(report.matched, 0);
        assert!(report.render_human().contains("No runs match"));
        Ok(())
    }

    #[test]
    fn a_truncated_list_says_how_many_it_did_not_show() {
        // §18. A list showing the first twenty of nine hundred, without saying so,
        // reads as nine hundred being twenty.
        let report = revlocal_cli::inspect::RunsReport {
            runs: Vec::new(),
            matched: 900,
            limit: 20,
        };
        // With rows present the header names both numbers; this asserts the
        // arithmetic that produces it rather than the empty case above.
        assert!(report.matched > u32::try_from(report.runs.len()).unwrap_or(0));

        let with_rows = revlocal_cli::inspect::RunsReport {
            runs: vec![revlocal_cli::inspect::RunRow {
                id: 1,
                change_id: 1,
                attempt: 1,
                status: "done".to_owned(),
                engine: "mock".to_owned(),
                verdict: Some("approve".to_owned()),
                skip_reason: None,
                degraded: None,
                error: None,
            }],
            matched: 900,
            limit: 20,
        };
        let human = with_rows.render_human();
        assert!(human.contains("showing 1 of 900"), "{human}");
        assert!(human.contains("raise --limit"), "{human}");
    }

    #[test]
    fn the_three_reasons_a_run_is_not_what_it_looks_like_are_shown_while_scanning() {
        // skip_reason, degraded and error each answer "why is this not what I
        // expected". Burying them in `runs show` means nobody sees them while
        // scanning a list, which is when the question actually gets asked.
        let report = revlocal_cli::inspect::RunsReport {
            runs: vec![revlocal_cli::inspect::RunRow {
                id: 7,
                change_id: 3,
                attempt: 2,
                status: "done".to_owned(),
                engine: "claude".to_owned(),
                verdict: None,
                skip_reason: Some("ignored_paths".to_owned()),
                degraded: Some("output salvaged from a fenced block".to_owned()),
                error: Some("interrupted".to_owned()),
            }],
            matched: 1,
            limit: 20,
        };

        let human = report.render_human();
        assert!(human.contains("skipped: ignored_paths"), "{human}");
        assert!(human.contains("degraded: output salvaged"), "{human}");
        assert!(human.contains("error: interrupted"), "{human}");
    }
}

// --- watch (RL-1201, §4.2, §7) ---------------------------------------------

mod watch_loop {
    use revlocal_cli::watch::{render, tick};
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

    async fn add_repo(pool: &Pool, name: &str, path: &str, enabled: bool) -> Result<(), String> {
        let at = now();
        revlocal_store::RepoStore::new(pool)
            .insert(&revlocal_core::Repo {
                id: revlocal_core::RepoId::new(0),
                name: name.to_owned(),
                kind: revlocal_core::RepoKind::Git,
                local_path: Some(path.to_owned()),
                remote_url: None,
                default_branch: Some("main".to_owned()),
                engine: revlocal_core::EngineKind::Mock,
                autonomy: revlocal_core::AutonomyMode::DryRun,
                enabled,
                config_json: "{}".to_owned(),
                created_at: at,
                updated_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn the_kill_switch_stops_the_loop_and_says_how_to_undo_it() -> Result<(), String> {
        // §12.1, checked at the level that actually enforces it. The scheduler
        // orders the check first; this asserts `watch` honours the answer.
        let (pool, _dir) = store().await?;
        add_repo(&pool, "acme", "/nonexistent", true).await?;
        revlocal_store::SettingStore::new(&pool)
            .set_paused(true, now())
            .await
            .map_err(|e| e.to_string())?;

        let report = tick(&pool, now()).await.map_err(|e| e.to_string())?;

        assert!(report.paused);
        assert!(report.passes.is_empty(), "nothing may run while paused");
        let idle = report.idle.clone().unwrap_or_default();
        assert!(idle.contains("kill switch"), "{idle}");
        assert!(
            idle.contains("revlocal resume"),
            "must say how to undo it: {idle}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_disabled_repository_is_not_polled() -> Result<(), String> {
        // `enabled` is the per-repo switch §13.2 gives, and it has to mean
        // something before the scheduler is asked anything.
        let (pool, _dir) = store().await?;
        add_repo(&pool, "off", "/nonexistent", false).await?;

        let report = tick(&pool, now()).await.map_err(|e| e.to_string())?;
        assert_eq!(report.repos, 0);
        assert!(report.passes.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn one_broken_repository_does_not_stop_the_others() -> Result<(), String> {
        // The property that decides whether a daemon is usable with eleven
        // repositories. An unreachable remote is recorded against the repository
        // that has it, and the rest are still polled.
        let (pool, _dir) = store().await?;
        add_repo(&pool, "broken", "/definitely/not/a/repo", true).await?;
        add_repo(&pool, "also-broken", "/nor/this/one", true).await?;

        let report = tick(&pool, now()).await.map_err(|e| e.to_string())?;

        assert_eq!(report.passes.len(), 2, "both were attempted");
        assert!(
            report.passes.iter().all(|p| p.error.is_some()),
            "each failure is recorded against its own repository"
        );

        let human = report.render_human();
        assert!(human.contains("broken — FAILED"), "{human}");
        assert!(human.contains("also-broken — FAILED"), "{human}");
        Ok(())
    }

    #[tokio::test]
    async fn a_tick_says_reviews_are_not_running_yet() -> Result<(), String> {
        // §18. A `watch` that silently reviewed nothing would be indistinguishable
        // from one whose repositories are quiet — which is the failure this whole
        // project keeps documenting.
        let (pool, _dir) = store().await?;
        add_repo(&pool, "acme", "/nonexistent", true).await?;

        let human = tick(&pool, now())
            .await
            .map_err(|e| e.to_string())?
            .render_human();

        assert!(human.contains("Discovery only"), "{human}");
        assert!(
            human.contains("revlocal review"),
            "must name what does work: {human}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_empty_install_ticks_without_complaining() -> Result<(), String> {
        // Running `watch` before adding a repository is how somebody checks it
        // works. It should say what it saw, not fail.
        let (pool, _dir) = store().await?;
        let report = tick(&pool, now()).await.map_err(|e| e.to_string())?;

        assert_eq!(report.repos, 0);
        assert!(
            report.idle.is_none(),
            "an empty install is not an error state"
        );
        assert!(report.render_human().contains("nothing due"));
        Ok(())
    }

    #[tokio::test]
    async fn the_json_omits_what_is_absent_rather_than_nulling_it() -> Result<(), String> {
        let (pool, _dir) = store().await?;
        let report = tick(&pool, now()).await.map_err(|e| e.to_string())?;
        let json = render(&report, true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        assert!(parsed["repos"].is_number());
        assert!(parsed["passes"].is_array());
        assert!(parsed["paused"].is_boolean());
        // Present means "something to say", the same rule the other reports follow.
        assert!(parsed.get("idle").is_none(), "{json}");
        Ok(())
    }
}

// --- backfill (RL-1201, §7.4) ----------------------------------------------

mod backfill_command {
    use revlocal_cli::backfill::{render, BackfillReport, ENUMERATION_CAP};

    fn report(items: usize, excluded: usize, truncated: bool) -> BackfillReport {
        BackfillReport {
            repo: "acme".to_owned(),
            scope: "backfill:commits:main".to_owned(),
            resumed_from: None,
            items: (0..items).map(|i| format!("sha{i} commit {i}")).collect(),
            excluded_by_limit: excluded,
            executed: false,
            truncated_enumeration: truncated,
        }
    }

    #[test]
    fn what_the_limit_excluded_is_stated() {
        // §18, and the bug this test exists for. `--limit 2` against four
        // candidates reported "2 change(s) to review" and nothing else, because
        // the limit was passed to enumeration *and* to planning — so planning
        // never saw the two it was excluding. That is the "showing 50 of 3,000"
        // failure, produced by the code written to report it.
        let human = report(2, 2, false).render_human();

        assert!(human.contains("2 change(s) to review"), "{human}");
        assert!(
            human.contains("2 more match --since and were excluded by --limit"),
            "{human}"
        );
    }

    #[test]
    fn a_capped_enumeration_says_its_own_count_is_a_lower_bound() {
        // §18 one level up. If enumeration itself stopped early, the excluded
        // count is not a total either, and printing it as one would be the same
        // mistake with an extra step.
        let capped = report(20, 9_980, true);
        assert!(!capped.counts_are_complete());

        let human = capped.render_human();
        assert!(human.contains("at least"), "{human}");
        assert!(human.contains(&ENUMERATION_CAP.to_string()), "{human}");

        // And an uncapped one does not hedge.
        assert!(!report(2, 2, false).render_human().contains("at least"));
    }

    #[test]
    fn a_plan_says_it_enqueued_nothing_rather_than_implying_it_did() {
        // Same rule as `watch`: a command that lists work and silently does none
        // of it is indistinguishable from one that did it all.
        let human = report(3, 0, false).render_human();

        assert!(human.contains("Nothing was enqueued"), "{human}");
        assert!(
            human.contains("revlocal review"),
            "must name what works: {human}"
        );
    }

    #[test]
    fn resuming_names_where_it_resumed_from() {
        // §7.4's separate `backfill:` cursor exists so an interrupted run picks up
        // where it stopped. Saying which change that was is what lets somebody
        // check the claim.
        let mut resumed = report(2, 0, false);
        resumed.resumed_from = Some("deadbeef".to_owned());

        let human = resumed.render_human();
        assert!(
            human.contains("resuming backfill:commits:main after deadbeef"),
            "{human}"
        );
    }

    #[test]
    fn the_json_omits_an_absent_resume_rather_than_nulling_it() -> Result<(), String> {
        let json = render(&report(1, 0, false), true).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        assert!(parsed.get("resumed_from").is_none(), "{json}");
        assert_eq!(parsed["executed"], false);
        assert_eq!(parsed["truncated_enumeration"], false);
        Ok(())
    }
}
