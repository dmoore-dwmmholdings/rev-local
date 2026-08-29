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
        "db", "publish", "targets", "review", "repo", "pause", "resume", "kill",
    ];

    /// Command groups §14 names that are not built yet, and what each waits on.
    ///
    /// An entry is a claim, not a placeholder: naming the blocker is what keeps
    /// this from becoming a list nobody revisits.
    const NOT_YET: &[(&str, &str)] = &[
        (
            "doctor",
            "RL-1202 — needs the engine and MCP probes assembled",
        ),
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
        ("approvals", "RL-803 built the inbox; this is its front end"),
        (
            "hooks",
            "RL-1004 built the installer; this is its front end",
        ),
        (
            "webhook",
            "RL-1005 and RL-1006 built the listener and tunnels",
        ),
        ("budget", "RL-805 built the ledger; this reads it"),
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
