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

    /// Remove every `open ... close` run, so what is left is commands and flags.
    fn strip_pairs(text: &str, open: char, close: char) -> String {
        let mut out = String::with_capacity(text.len());
        let mut depth = 0usize;
        for ch in text.chars() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth = depth.saturating_sub(1);
            } else if depth == 0 {
                out.push(ch);
            }
        }
        out
    }

    /// Every `group subcommand` pair §14 names, read from the spec.
    ///
    /// [`spec_commands`] deliberately stops at the group, because that is the
    /// question "does this command exist" asks. It is also a weaker question than
    /// it looks: `db` existing says nothing about `db vacuum`, and the group-level
    /// check reported a complete §14 surface while eight specified subcommands did
    /// not exist. This asks the narrower question.
    pub fn spec_subcommands() -> Result<Vec<(String, String)>, String> {
        let spec = std::fs::read_to_string(workspace_root().join("SPEC.md"))
            .map_err(|e| format!("reading SPEC.md: {e}"))?;
        let block = spec
            .split("## 14. CLI surface")
            .nth(1)
            .ok_or("SPEC.md has no §14")?
            .split("```")
            .nth(1)
            .ok_or("§14 has no command block")?;

        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut push = |group: &str, sub: &str| {
            let pair = (group.to_owned(), sub.to_owned());
            if !sub.is_empty() && !pairs.contains(&pair) {
                pairs.push(pair);
            }
        };

        for line in block.lines() {
            let line = line.split('#').next().unwrap_or(line).trim();
            let Some(rest) = line.strip_prefix("revlocal ") else {
                continue;
            };

            // §14 writes `|` three ways, and only one of them separates commands.
            //
            //   revlocal hooks install|uninstall --repo N     subcommands (tight)
            //   revlocal db migrate | vacuum | export         subcommands (spaced)
            //   revlocal repo add ... --kind git|github|svn   a flag's VALUES
            //   revlocal repo add <path|url>                  a placeholder's
            //
            // The first parser read all four the same way and reported `github`,
            // `svn` and `ngrok` as missing subcommands of `repo` and `webhook`.
            //
            // Placeholders go first, since `<id|--run R|--all>` contains both a
            // `|` and something that looks like a flag.
            let rest = strip_pairs(&strip_pairs(rest, '<', '>'), '[', ']');

            // A spaced `|` separates alternatives; whether they are subcommands or
            // sibling commands depends on how many words precede the first one.
            let mut segments = rest.split(" | ");
            let Some(first) = segments.next() else {
                continue;
            };
            let head: Vec<&str> = first.split_whitespace().collect();
            let Some(group) = head.first() else {
                continue;
            };
            // `revlocal pause | resume | kill` — siblings of the group, not
            // subcommands of it. Handled by `spec_commands`.
            if head.len() == 1 {
                continue;
            }

            // Only the word directly after the group can carry a tight
            // alternation; anywhere later it belongs to a flag.
            let Some(second) = head.get(1) else { continue };
            if second.starts_with("--") {
                continue;
            }
            for name in second.split('|') {
                push(group, name);
            }

            for segment in segments {
                if let Some(word) = segment.split_whitespace().next() {
                    if !word.starts_with("--") && !word.contains('=') {
                        push(group, word);
                    }
                }
            }
        }
        Ok(pairs)
    }

    /// Subcommands §14 names that do not exist yet, and what each waits on.
    ///
    /// Every one of these is a front end over machinery that already exists and is
    /// tested — the same shape the rest of §14's surface turned out to be. They are
    /// listed rather than silently absent because a specified command that nobody
    /// is tracking is how a surface stays two-thirds built.
    pub const SUBCOMMANDS_NOT_YET: &[(&str, &str, &str)] = &[(
        "db",
        "export",
        "no export format is settled, and one shipped now is one to support forever",
    )];

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
        "webhook",
    ];

    /// Command groups §14 names that are not built yet, and what each waits on.
    ///
    /// An entry is a claim, not a placeholder: naming the blocker is what keeps
    /// this from becoming a list nobody revisits.
    const NOT_YET: &[(&str, &str)] = &[];

    pub fn binary() -> PathBuf {
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

    /// A real git repository with `commits`, one of which is a lockfile bump.
    fn a_repo_with_a_lockfile_commit(dir: &std::path::Path) -> Result<(), String> {
        let run = |args: &[&str]| -> Result<(), String> {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).into_owned());
            }
            Ok(())
        };
        run(&["init", "--quiet", "-b", "main", "."])?;
        run(&["config", "user.email", "t@e.invalid"])?;
        run(&["config", "user.name", "T"])?;
        std::fs::write(dir.join("a.rs"), "code\n").map_err(|e| e.to_string())?;
        run(&["add", "a.rs"])?;
        run(&["commit", "--quiet", "-m", "add a helper"])?;
        std::fs::write(dir.join("Cargo.lock"), "lock\n").map_err(|e| e.to_string())?;
        run(&["add", "Cargo.lock"])?;
        run(&["commit", "--quiet", "-m", "bump deps"])?;
        Ok(())
    }

    #[tokio::test]
    async fn a_skipped_change_is_recorded_and_its_reason_shown() -> Result<(), String> {
        // §9.4: a skipped change is written down with its reason. A change that
        // vanishes is indistinguishable from one that was never seen, and "why did
        // rev-local ignore my commit?" has an answer only if the reason is shown.
        let (pool, dir) = store().await?;
        let work = dir.path().join("acme");
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        a_repo_with_a_lockfile_commit(&work)?;
        add_repo(&pool, "acme", &work.display().to_string(), true).await?;

        let report = tick(&pool, now()).await.map_err(|e| e.to_string())?;
        let pass = report.passes.first().ok_or("one repository, one pass")?;

        assert_eq!(pass.discovered, 2);
        assert_eq!(pass.recorded, 1, "the lockfile commit is not for reviewing");
        assert_eq!(pass.skipped.len(), 1);
        assert!(
            pass.skipped[0].contains("ignore_globs"),
            "the reason must survive: {:?}",
            pass.skipped
        );

        let human = report.render_human();
        assert!(
            human.contains("2 discovered, 1 recorded, 1 skipped"),
            "{human}"
        );
        assert!(human.contains("skipped:"), "{human}");
        Ok(())
    }

    #[tokio::test]
    async fn the_cursor_advances_so_a_second_pass_finds_nothing() -> Result<(), String> {
        // Without this, every poll rediscovers the whole history forever — which
        // looks like working and costs the same as never advancing.
        let (pool, dir) = store().await?;
        let work = dir.path().join("acme");
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        a_repo_with_a_lockfile_commit(&work)?;
        add_repo(&pool, "acme", &work.display().to_string(), true).await?;

        let first = tick(&pool, now()).await.map_err(|e| e.to_string())?;
        assert_eq!(first.passes[0].discovered, 2);
        assert!(
            first.passes[0].cursor.is_some(),
            "the cursor must have moved"
        );

        let second = tick(&pool, now()).await.map_err(|e| e.to_string())?;
        assert_eq!(
            second.passes[0].discovered, 0,
            "a quiet repository must stay quiet"
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_cursor_advances_past_a_skipped_change() -> Result<(), String> {
        // The lockfile commit is the newest one. If the cursor stopped short of a
        // skipped change, it would be re-decided on every poll forever.
        let (pool, dir) = store().await?;
        let work = dir.path().join("acme");
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        a_repo_with_a_lockfile_commit(&work)?;
        add_repo(&pool, "acme", &work.display().to_string(), true).await?;

        tick(&pool, now()).await.map_err(|e| e.to_string())?;
        let second = tick(&pool, now()).await.map_err(|e| e.to_string())?;

        assert_eq!(second.passes[0].discovered, 0);
        assert!(
            second.passes[0].skipped.is_empty(),
            "a skipped change must not be re-decided: {:?}",
            second.passes[0].skipped
        );
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

// --- webhook (RL-1201, §7.3) -----------------------------------------------

mod webhook_command {
    use revlocal_cli::webhook::{start, status, stop, Listener};
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

    /// Write a config naming `port`, and return its path.
    ///
    /// ADR 0003: a helper returns its failure rather than panicking, so a broken
    /// fixture is reported as a broken fixture and not as the test's subject
    /// failing.
    fn config(dir: &tempfile::TempDir, port: u16) -> Result<std::path::PathBuf, String> {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, format!("[global]\nwebhook_port = {port}\n"))
            .map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// A port nothing is listening on, found by binding and releasing one.
    async fn a_free_port() -> Result<u16, String> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        drop(listener);
        Ok(port)
    }

    async fn add_repo(pool: &Pool, name: &str, config_json: &str) -> Result<(), String> {
        let at = now();
        revlocal_store::RepoStore::new(pool)
            .insert(&revlocal_core::Repo {
                id: revlocal_core::RepoId::new(0),
                name: name.to_owned(),
                kind: revlocal_core::RepoKind::Git,
                local_path: Some("/nowhere".to_owned()),
                remote_url: None,
                default_branch: Some("main".to_owned()),
                engine: revlocal_core::EngineKind::Mock,
                autonomy: revlocal_core::AutonomyMode::DryRun,
                enabled: true,
                config_json: config_json.to_owned(),
                created_at: at,
                updated_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn start_refuses_when_the_port_is_zero_and_names_a_free_one() -> Result<(), String> {
        // §13.1's `webhook_port = 0` disables the listener. Starting anyway would
        // enable a switch that cannot do anything, and picking a port here would
        // make the config file stop explaining the behaviour.
        let (pool, dir) = store().await?;
        let path = config(&dir, 0)?;

        let error = start(&pool, &path, Some("cloudflared"), now())
            .await
            .err()
            .ok_or("port 0 must refuse")?;
        let text = error.to_string();

        assert!(text.contains("webhook_port is 0"), "{text}");
        assert!(text.contains("try: set global.webhook_port = "), "{text}");
        // §18: the remedy must be a value, not a suggestion to go and find one.
        let suggested = text
            .split("webhook_port = ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse::<u16>().ok())
            .ok_or_else(|| format!("no port in: {text}"))?;
        assert!(suggested > 0, "suggested {suggested}");
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_provider_lists_the_ones_that_exist() -> Result<(), String> {
        // The common case is a typo, and it is fixed the moment the right
        // spelling is on screen.
        let (pool, dir) = store().await?;
        let path = config(&dir, a_free_port().await?)?;

        let error = start(&pool, &path, Some("cloudfared"), now())
            .await
            .err()
            .ok_or("a misspelt provider must fail")?;
        let text = error.to_string();

        assert!(text.contains("cloudfared"), "{text}");
        assert!(text.contains("cloudflared"), "{text}");
        assert!(text.contains("ngrok"), "{text}");
        assert!(text.contains("manual"), "{text}");
        Ok(())
    }

    #[tokio::test]
    async fn stop_keeps_the_tunnel_choice() -> Result<(), String> {
        // Stopping is not unconfiguring. Making somebody re-pick their tunnel
        // every time they pause deliveries teaches them to leave it running.
        let (pool, dir) = store().await?;
        let path = config(&dir, a_free_port().await?)?;

        let started = start(&pool, &path, Some("ngrok"), now())
            .await
            .map_err(|e| e.to_string())?;
        assert!(started.enabled && started.changed);

        let stopped = stop(&pool, &path, now()).await.map_err(|e| e.to_string())?;
        assert!(!stopped.enabled);
        assert!(stopped.changed, "the first stop changed something");
        assert_eq!(stopped.tunnel.as_deref(), Some("ngrok"));

        // And starting again without --tunnel does not lose it.
        let restarted = start(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(restarted.tunnel.as_deref(), Some("ngrok"));
        Ok(())
    }

    #[tokio::test]
    async fn a_second_stop_reports_no_change() -> Result<(), String> {
        // `enabled` and `changed` are separate so a script can tell "I turned it
        // off" from "it was already off" — which matters when two operators reach
        // for the same switch.
        let (pool, dir) = store().await?;
        let path = config(&dir, a_free_port().await?)?;

        stop(&pool, &path, now()).await.map_err(|e| e.to_string())?;
        let again = stop(&pool, &path, now()).await.map_err(|e| e.to_string())?;

        assert!(!again.enabled);
        assert!(!again.changed, "nothing was there to change");
        Ok(())
    }

    #[tokio::test]
    async fn the_probe_distinguishes_a_bound_port_from_a_free_one() -> Result<(), String> {
        // A stored "enabled" is intent; what is on the port is an observation.
        // Reporting the first as if it were the second is how a dead listener
        // reads as a healthy one.
        let (pool, dir) = store().await?;
        let port = a_free_port().await?;
        let path = config(&dir, port)?;

        let free = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(free.listener, Listener::NotListening);

        let held = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        let bound = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(bound.listener, Listener::InUse);
        drop(held);
        Ok(())
    }

    #[tokio::test]
    async fn a_port_of_zero_is_disabled_rather_than_free() -> Result<(), String> {
        // Binding to port 0 asks the OS for any free port and always succeeds.
        // Probing it would report "nothing is listening" about a port that does
        // not exist — and send somebody to restart a daemon that was never meant
        // to bind.
        let (pool, dir) = store().await?;
        let path = config(&dir, 0)?;

        let report = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.listener, Listener::Disabled);
        assert!(
            report
                .next_steps
                .iter()
                .any(|s| s.contains("global.webhook_port")),
            "{:?}",
            report.next_steps
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_says_when_no_repository_has_opted_in() -> Result<(), String> {
        // §7.3's second switch. Port set, tunnel up, nobody opted in is a
        // configuration where every check passes and no review ever happens —
        // which reads exactly like a quiet week.
        let (pool, dir) = store().await?;
        let path = config(&dir, a_free_port().await?)?;
        add_repo(&pool, "acme", "{}").await?;

        let report = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.repos.len(), 1);
        assert_eq!(report.opted_in(), 0);
        assert!(
            report
                .next_steps
                .iter()
                .any(|s| s.contains("no repository has webhook_enabled")),
            "{:?}",
            report.next_steps
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_opted_in_repo_with_no_secret_is_called_out() -> Result<(), String> {
        // Without a secret every delivery fails signature verification, which
        // from the outside is indistinguishable from GitHub not sending anything.
        let (pool, dir) = store().await?;
        let path = config(&dir, a_free_port().await?)?;
        add_repo(&pool, "acme", r#"{"webhook_enabled": true}"#).await?;

        let report = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.opted_in(), 1);
        assert!(!report.repos[0].secret_configured);
        assert!(
            report
                .next_steps
                .iter()
                .any(|s| s.contains("webhook_secret_ref")),
            "{:?}",
            report.next_steps
        );
        assert!(report.render_human().contains("NO SECRET"), "and visibly");
        Ok(())
    }

    #[tokio::test]
    async fn a_repo_whose_config_will_not_parse_is_treated_as_opted_out() -> Result<(), String> {
        // The one wrong direction to fail in: reporting a repository as receiving
        // webhooks because its config could not be read.
        let (pool, dir) = store().await?;
        let path = config(&dir, a_free_port().await?)?;
        add_repo(&pool, "acme", "not json at all").await?;

        let report = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.opted_in(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_config_file_is_the_spec_default_not_an_error() -> Result<(), String> {
        // A fresh install has no config file, and §13.1's document is the default.
        let (pool, dir) = store().await?;
        let path = dir.path().join("nothing-here.toml");

        let report = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.port, 0, "§13.1 defaults webhook_port to 0");
        assert_eq!(report.listener, Listener::Disabled);
        Ok(())
    }

    #[tokio::test]
    async fn status_offers_no_next_steps_it_cannot_justify() -> Result<(), String> {
        // With both switches on, a secret set and the listener bound, the only
        // thing that can still be missing is the tunnel binary — and `manual`
        // does not need one. Nothing else should be invented to fill the list.
        let (pool, dir) = store().await?;
        let port = a_free_port().await?;
        let path = config(&dir, port)?;
        add_repo(
            &pool,
            "acme",
            r#"{"webhook_enabled": true, "webhook_secret_ref": "keychain:acme"}"#,
        )
        .await?;
        start(&pool, &path, Some("manual"), now())
            .await
            .map_err(|e| e.to_string())?;

        let held = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        let report = status(&pool, &path, None, now())
            .await
            .map_err(|e| e.to_string())?;
        drop(held);

        assert_eq!(report.next_steps.len(), 1, "{:?}", report.next_steps);
        assert!(
            report.next_steps[0].contains("public URL"),
            "{:?}",
            report.next_steps
        );
        Ok(())
    }
}

// --- §14 at subcommand granularity (RL-1206) --------------------------------

mod spec_subcommand_surface {
    use super::cli_surface::{binary, spec_subcommands, SUBCOMMANDS_NOT_YET};

    /// Whether the built binary accepts `revlocal <group> <sub>`.
    ///
    /// Asked of the binary, not of a help string. A `--help` that mentions a word
    /// is not a command that runs, and this file exists because the group-level
    /// check believed a weaker thing than it reported.
    fn accepted(group: &str, sub: &str) -> bool {
        std::process::Command::new(binary())
            .args([group, sub, "--help"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn every_subcommand_in_spec_14_exists_or_is_listed() -> Result<(), String> {
        let spec = spec_subcommands()?;
        assert!(
            spec.len() > 15,
            "only {} subcommands parsed from §14; the parser has stopped matching",
            spec.len()
        );

        let missing: Vec<String> = spec
            .iter()
            .filter(|(group, sub)| !accepted(group, sub))
            .filter(|(group, sub)| {
                !SUBCOMMANDS_NOT_YET
                    .iter()
                    .any(|(g, s, _)| g == group && s == sub)
            })
            .map(|(group, sub)| format!("{group} {sub}"))
            .collect();
        assert!(
            missing.is_empty(),
            "§14 names these and the binary does not accept them: {missing:?}\n\
             Implement them, or add them to SUBCOMMANDS_NOT_YET with what they wait on."
        );
        Ok(())
    }

    #[test]
    fn nothing_waits_on_a_blocker_it_no_longer_has() -> Result<(), String> {
        // The failure mode of any "not yet" list is that it outlives the work.
        // A subcommand that now exists must come off the list, or the list stops
        // being a description of what is missing.
        let landed: Vec<String> = SUBCOMMANDS_NOT_YET
            .iter()
            .filter(|(group, sub, _)| accepted(group, sub))
            .map(|(group, sub, _)| format!("{group} {sub}"))
            .collect();
        assert!(
            landed.is_empty(),
            "these are listed as missing and the binary accepts them: {landed:?}\n\
             Take them out of SUBCOMMANDS_NOT_YET."
        );
        Ok(())
    }

    #[test]
    fn every_reason_says_what_it_waits_on() -> Result<(), String> {
        // A ticket number is not an explanation. This caught the same laziness at
        // group level, where `resume` was listed as blocked on "RL-804".
        for (group, sub, reason) in SUBCOMMANDS_NOT_YET {
            assert!(
                reason.split_whitespace().count() >= 5,
                "`{group} {sub}` waits on {reason:?}, which does not say what it waits on"
            );
        }
        Ok(())
    }
}

// --- decisions (RL-1201, §12.4, §14) ----------------------------------------

mod decisions {
    use revlocal_cli::decide::{approve, reject, reset_budget, retry_action, suppress, Scope};
    use revlocal_core::{
        Capability, PublishAction, PublishActionId, PublishActionStatus, RepoId, RiskClass, RunId,
    };
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

    /// A repository, a change and a run, so publish actions have something to hang on.
    async fn a_run(pool: &Pool, name: &str) -> Result<RunId, String> {
        a_run_with(
            pool,
            name,
            revlocal_core::RunStatus::AwaitingApproval,
            1,
            None,
            None,
        )
        .await
    }

    /// The same, with the fields a retry test needs to vary.
    ///
    /// Through the store rather than by raw SQL: a fixture that writes rows the
    /// store would refuse to write is a fixture testing a database that cannot
    /// exist.
    async fn a_run_with(
        pool: &Pool,
        name: &str,
        status: revlocal_core::RunStatus,
        attempt: u32,
        usage: Option<revlocal_core::Usage>,
        error: Option<&str>,
    ) -> Result<RunId, String> {
        build_run(pool, name, status, attempt, usage, error).await
    }

    async fn build_run(
        pool: &Pool,
        name: &str,
        status: revlocal_core::RunStatus,
        attempt: u32,
        usage: Option<revlocal_core::Usage>,
        error: Option<&str>,
    ) -> Result<RunId, String> {
        let at = now();
        let repo = revlocal_store::RepoStore::new(pool)
            .insert(&revlocal_core::Repo {
                id: RepoId::new(0),
                name: name.to_owned(),
                kind: revlocal_core::RepoKind::Git,
                local_path: Some("/nowhere".to_owned()),
                remote_url: None,
                default_branch: Some("main".to_owned()),
                engine: revlocal_core::EngineKind::Mock,
                autonomy: revlocal_core::AutonomyMode::DryRun,
                enabled: true,
                config_json: "{}".to_owned(),
                created_at: at,
                updated_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;

        let change = revlocal_store::ChangeStore::new(pool)
            .upsert(&revlocal_core::Change {
                id: revlocal_core::ChangeId::new(0),
                repo_id: repo.id,
                kind: revlocal_core::ChangeKind::Commit,
                external_id: format!("{name}-sha"),
                title: None,
                author_name: None,
                author_email: None,
                authored_at: None,
                branch: None,
                base_ref: None,
                head_ref: None,
                url: None,
                diff_stat: revlocal_core::DiffStat::default(),
                detected_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;

        let run = revlocal_store::RunStore::new(pool)
            .insert(&revlocal_core::Run {
                id: RunId::new(0),
                change_id: change.id,
                attempt,
                status,
                engine: revlocal_core::EngineKind::Mock,
                depth: revlocal_core::Depth::Standard,
                trigger: revlocal_core::TriggerSource::Manual,
                skip_reason: None,
                error: error.map(str::to_owned),
                usage: usage.unwrap_or_default(),
                started_at: None,
                finished_at: None,
                transcript_path: None,
                truncated: false,
                omitted_files: Vec::new(),
                verdict: None,
                summary: None,
                degraded: None,
                created_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(run.id)
    }

    /// One action awaiting a human, with `payload` as its body.
    async fn awaiting(
        pool: &Pool,
        run_id: RunId,
        target: &str,
        payload: &str,
    ) -> Result<PublishActionId, String> {
        let action = revlocal_store::PublishActionStore::new(pool)
            .insert(&PublishAction {
                id: PublishActionId::new(0),
                run_id,
                finding_id: None,
                target: target.to_owned(),
                capability: Capability::PostReview,
                risk: RiskClass::High,
                idempotency_key: format!("{target}-{payload}"),
                payload_json: payload.to_owned(),
                status: PublishActionStatus::AwaitingApproval,
                attempts: 0,
                response_json: None,
                external_ref: None,
                error: None,
                created_at: now(),
                sent_at: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(action.id)
    }

    #[tokio::test]
    async fn approving_records_the_digest_of_what_was_approved() -> Result<(), String> {
        // §12.4's rule is that an edit after approval is impossible, and the queue
        // enforces it by re-computing this digest at dispatch. Approving without
        // recording *what* was approved would leave that rule as an intention.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let id = awaiting(&pool, run, "github", r#"{"body":"hi"}"#).await?;

        approve(&pool, Scope::One(id.get()))
            .await
            .map_err(|e| e.to_string())?;

        let stored = revlocal_store::PublishActionStore::new(&pool)
            .approved_digest(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("no digest recorded")?;
        assert_eq!(
            stored,
            revlocal_core::payload_digest(r#"{"body":"hi"}"#),
            "the digest must be of the payload, not of something else"
        );
        Ok(())
    }

    #[tokio::test]
    async fn approving_something_already_decided_is_an_error() -> Result<(), String> {
        // A named id carries a belief about its state. Silently succeeding would
        // tell somebody they had approved a thing that was already sent.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let id = awaiting(&pool, run, "github", "{}").await?;

        approve(&pool, Scope::One(id.get()))
            .await
            .map_err(|e| e.to_string())?;
        let again = approve(&pool, Scope::One(id.get())).await;

        let text = again.err().ok_or("must refuse")?.to_string();
        assert!(text.contains("not waiting for approval"), "{text}");
        assert!(
            text.contains("revlocal approvals list"),
            "and say where to look"
        );
        Ok(())
    }

    #[tokio::test]
    async fn approve_all_on_an_empty_inbox_says_so_rather_than_failing() -> Result<(), String> {
        // "Approve everything" over nothing is a true and useful answer, not an
        // error — unlike a named id, it carries no belief that anything is there.
        let (pool, _dir) = store().await?;

        let report = approve(&pool, Scope::All)
            .await
            .map_err(|e| e.to_string())?;

        assert!(report.decided.is_empty());
        assert!(
            report.detail.contains("Nothing was waiting"),
            "{}",
            report.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn approve_by_run_leaves_other_runs_alone() -> Result<(), String> {
        // The scope is the whole point of the flag. An `--run` that approved
        // everything would be `--all` with a longer name.
        let (pool, _dir) = store().await?;
        let mine = a_run(&pool, "acme").await?;
        let theirs = a_run(&pool, "other").await?;
        awaiting(&pool, mine, "github", "{}").await?;
        let untouched = awaiting(&pool, theirs, "andare", "{}").await?;

        let report = approve(&pool, Scope::Run(mine.get()))
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.decided.len(), 1);
        let still_waiting = revlocal_store::PublishActionStore::new(&pool)
            .list_awaiting_approval()
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(still_waiting.len(), 1);
        assert_eq!(still_waiting[0].id, untouched);
        Ok(())
    }

    #[tokio::test]
    async fn rejecting_records_a_decision_not_a_timeout() -> Result<(), String> {
        // §12.4 keeps `expired` distinct from a person saying no: one is a
        // decision, the other is that nobody looked. Collapsing them loses the
        // only signal that the approval flow is being ignored.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let id = awaiting(&pool, run, "github", "{}").await?;

        reject(&pool, id.get(), false, now())
            .await
            .map_err(|e| e.to_string())?;

        let reason = revlocal_store::PublishActionStore::new(&pool)
            .decision_reason(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("no reason recorded")?;
        assert!(reason.contains("operator"), "{reason}");
        assert!(
            !reason.contains("expired"),
            "a rejection is not a timeout: {reason}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejecting_with_suppress_on_an_action_with_no_finding_says_so() -> Result<(), String> {
        // A suppression with no fingerprint and no glob can never match anything.
        // Creating one anyway would look like a suppression that stopped working.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let id = awaiting(&pool, run, "github", "{}").await?;

        let report = reject(&pool, id.get(), true, now())
            .await
            .map_err(|e| e.to_string())?;

        assert!(report.suppressed.is_empty());
        assert!(
            report.detail.contains("Nothing to suppress"),
            "{}",
            report.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_suppression_is_global_only_when_asked_for() -> Result<(), String> {
        // Global is the wider choice, not the safer one, so it must be what was
        // asked for rather than what was left out — and the report says which.
        let (pool, _dir) = store().await?;
        a_run(&pool, "acme").await?;

        let global = suppress(&pool, "abc123", None, now())
            .await
            .map_err(|e| e.to_string())?;
        assert!(global.repo.is_none());
        assert!(global.detail.contains("everywhere"), "{}", global.detail);

        let scoped = suppress(&pool, "def456", Some("acme"), now())
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(scoped.repo.as_deref(), Some("acme"));
        assert!(scoped.detail.contains("in acme"), "{}", scoped.detail);
        Ok(())
    }

    #[tokio::test]
    async fn suppressing_in_an_unknown_repository_is_refused() -> Result<(), String> {
        let (pool, _dir) = store().await?;

        let error = suppress(&pool, "abc", Some("nope"), now())
            .await
            .err()
            .ok_or("must refuse")?
            .to_string();

        assert!(error.contains("no repository named"), "{error}");
        assert!(error.contains("revlocal repo list"), "{error}");
        Ok(())
    }

    #[tokio::test]
    async fn resetting_a_budget_that_was_never_spent_says_so() -> Result<(), String> {
        // Silently succeeding leaves an operator wondering whether it worked —
        // and this command exists for the moment somebody is already unsure.
        let (pool, _dir) = store().await?;
        a_run(&pool, "acme").await?;

        let report = reset_budget(&pool, "acme", now())
            .await
            .map_err(|e| e.to_string())?;

        assert!(!report.cleared);
        assert!(
            report.detail.contains("nothing to clear"),
            "{}",
            report.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn resetting_a_budget_clears_the_day_but_not_the_runs() -> Result<(), String> {
        // The escape hatch must not make the spend unexplainable afterwards. It
        // clears the allowance accounting; the record that work happened stays.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let at = now();
        let day = at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string();
        let repo_id = revlocal_store::RepoStore::new(&pool)
            .list()
            .await
            .map_err(|e| e.to_string())?[0]
            .id;

        revlocal_store::BudgetLedgerStore::new(&pool)
            .add_run(repo_id, &day, 1, &revlocal_core::Usage::default())
            .await
            .map_err(|e| e.to_string())?;

        let report = reset_budget(&pool, "acme", at)
            .await
            .map_err(|e| e.to_string())?;
        assert!(report.cleared);

        let ledger = revlocal_store::BudgetLedgerStore::new(&pool)
            .get(repo_id, &day)
            .await
            .map_err(|e| e.to_string())?;
        assert!(ledger.is_none(), "the day's accounting is gone");

        let still_there = revlocal_store::RunStore::new(&pool)
            .get(run)
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(still_there.id, run, "the run itself must survive");
        Ok(())
    }

    #[tokio::test]
    async fn retrying_one_action_leaves_the_others_alone() -> Result<(), String> {
        // The whole difference from `publish replay --run R --target T`. When a
        // run produced several actions for a target and one failed, replaying the
        // target re-posts the ones that already landed.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let store_ref = revlocal_store::PublishActionStore::new(&pool);

        let mut ids = Vec::new();
        for n in 0..3 {
            let action = store_ref
                .insert(&PublishAction {
                    id: PublishActionId::new(0),
                    run_id: run,
                    finding_id: None,
                    target: "github".to_owned(),
                    capability: Capability::PostReview,
                    risk: RiskClass::Low,
                    idempotency_key: format!("k{n}"),
                    payload_json: "{}".to_owned(),
                    status: PublishActionStatus::Failed,
                    attempts: 3,
                    response_json: None,
                    external_ref: None,
                    error: Some("the target refused it".to_owned()),
                    created_at: now(),
                    sent_at: None,
                })
                .await
                .map_err(|e| e.to_string())?;
            ids.push(action.id);
        }

        retry_action(&pool, ids[1].get())
            .await
            .map_err(|e| e.to_string())?;

        for (n, id) in ids.iter().enumerate() {
            let action = store_ref.get(*id).await.map_err(|e| e.to_string())?;
            let expected = if n == 1 {
                PublishActionStatus::Pending
            } else {
                PublishActionStatus::Failed
            };
            assert_eq!(action.status, expected, "action {n} of 3");
        }
        Ok(())
    }

    #[tokio::test]
    async fn retrying_something_that_did_not_fail_is_refused() -> Result<(), String> {
        // Returning a count rather than `()` is what makes this checkable: the
        // alternative is a command that quietly does nothing and reports success.
        let (pool, _dir) = store().await?;
        let run = a_run(&pool, "acme").await?;
        let id = awaiting(&pool, run, "github", "{}").await?;

        let error = retry_action(&pool, id.get())
            .await
            .err()
            .ok_or("must refuse")?
            .to_string();

        assert!(error.contains("not in a failed state"), "{error}");
        Ok(())
    }

    #[tokio::test]
    async fn a_retried_run_carries_none_of_the_previous_attempts_spend() -> Result<(), String> {
        // The reason the successor is built in one shared place. A retry that
        // carried usage forward would charge the budget twice for work that was
        // thrown away — and it would do it quietly.
        let (pool, _dir) = store().await?;
        let run = a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Failed,
            1,
            Some(revlocal_core::Usage {
                tokens_in: 900,
                tokens_out: 100,
                tokens_known: true,
                cost_usd: Some(0.42),
            }),
            Some("boom"),
        )
        .await?;

        let report = revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .map_err(|e| e.to_string())?;

        let successor = revlocal_store::RunStore::new(&pool)
            .get(RunId::new(report.run_id))
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(successor.usage.tokens_in, 0);
        assert_eq!(successor.usage.tokens_out, 0);
        assert!(successor.error.is_none(), "and no inherited error");
        assert_eq!(successor.attempt, 2);
        assert_eq!(successor.status, revlocal_core::RunStatus::Queued);
        Ok(())
    }

    #[tokio::test]
    async fn a_retry_reviews_the_same_change_the_same_way() -> Result<(), String> {
        // Not a reset of everything. If a retry ran at a different depth it would
        // not be a retry, and comparing the two would compare different questions.
        let (pool, _dir) = store().await?;
        let run = a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Failed,
            1,
            None,
            Some("the engine exited 1"),
        )
        .await?;
        let before = revlocal_store::RunStore::new(&pool)
            .get(run)
            .await
            .map_err(|e| e.to_string())?;

        let report = revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .map_err(|e| e.to_string())?;
        let after = revlocal_store::RunStore::new(&pool)
            .get(RunId::new(report.run_id))
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(after.change_id, before.change_id);
        assert_eq!(after.engine, before.engine);
        assert_eq!(after.depth, before.depth);
        assert_eq!(after.trigger, before.trigger);
        Ok(())
    }

    #[tokio::test]
    async fn the_run_that_was_retried_is_left_exactly_as_it_was() -> Result<(), String> {
        // A run is the record of one attempt. Rewriting it would lose the evidence
        // of what went wrong, which is the thing somebody retrying most wants next.
        let (pool, _dir) = store().await?;
        let run = a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Failed,
            1,
            None,
            Some("the engine exited 137"),
        )
        .await?;

        revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .map_err(|e| e.to_string())?;

        let original = revlocal_store::RunStore::new(&pool)
            .get(run)
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(original.status, revlocal_core::RunStatus::Failed);
        assert_eq!(original.error.as_deref(), Some("the engine exited 137"));
        Ok(())
    }

    #[tokio::test]
    async fn a_run_still_in_flight_cannot_be_retried() -> Result<(), String> {
        // Two runs for one change, both working, both publishing.
        let (pool, _dir) = store().await?;
        let run = a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Reviewing,
            1,
            None,
            None,
        )
        .await?;

        let error = revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .err()
            .ok_or("must refuse")?
            .to_string();

        assert!(error.contains("still reviewing"), "{error}");
        assert!(
            error.contains("kill --hard"),
            "and say how to stop it: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retrying_twice_reports_the_successor_rather_than_a_constraint() -> Result<(), String> {
        // `(change_id, attempt)` is unique, so a second retry hits the database's
        // constraint. Somebody who retried twice wants to be told where the first
        // one went, not shown a UNIQUE violation.
        let (pool, _dir) = store().await?;
        let run = a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Failed,
            1,
            None,
            Some("the engine exited 1"),
        )
        .await?;

        revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .map_err(|e| e.to_string())?;
        let error = revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .err()
            .ok_or("must refuse")?
            .to_string();

        assert!(
            error.contains("already been retried as attempt 2"),
            "{error}"
        );
        assert!(
            !error.to_lowercase().contains("unique"),
            "not the raw constraint: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_human_retry_is_not_bounded_by_the_recovery_ceiling() -> Result<(), String> {
        // §13.1 defines `max_attempts` as where *recovery* gives up, and recovery
        // is the automatic pass — its job is to stop a change that crashes the
        // daemon from retrying forever with nobody watching. Somebody typing the
        // command is that condition not applying.
        let (pool, _dir) = store().await?;
        let run = a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Failed,
            9,
            None,
            Some("the engine exited 1"),
        )
        .await?;

        let report = revlocal_cli::decide::retry_run(&pool, run.get(), now())
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.attempt, 10, "well past the default ceiling of 3");
        // Visible rather than silent: the attempt number is in the report.
        assert!(report.detail.contains("attempt 10"), "{}", report.detail);
        Ok(())
    }

    #[tokio::test]
    async fn retrying_a_run_that_does_not_exist_does_not_blame_the_database() -> Result<(), String>
    {
        let (pool, _dir) = store().await?;

        let error = revlocal_cli::decide::retry_run(&pool, 999, now())
            .await
            .err()
            .ok_or("must refuse")?
            .to_string();

        assert!(error.contains("no run with id 999"), "{error}");
        assert!(
            !error.contains("db migrate"),
            "a typo is not a broken database: {error}"
        );
        Ok(())
    }
    #[tokio::test]
    async fn a_vacuum_takes_the_transcript_file_with_the_row() -> Result<(), String> {
        // The row is the only thing that knows where the file is. Deleting one
        // without the other leaks disk space permanently and silently — the exact
        // opposite of what somebody reclaiming space asked for.
        let (pool, dir) = store().await?;
        let transcript = dir.path().join("old.log");
        std::fs::write(&transcript, "engine output").map_err(|e| e.to_string())?;

        a_finished_run(
            &pool,
            "acme",
            "2020-01-01T00:00:00Z",
            Some(&transcript.display().to_string()),
        )
        .await?;

        let report = revlocal_cli::decide::vacuum(&pool, "2021-01-01")
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.runs_deleted, 1);
        assert_eq!(report.transcripts_removed, 1);
        assert!(!transcript.exists(), "the file must go with the row");
        assert!(report.transcripts_left.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_vacuum_leaves_a_run_that_has_not_finished() -> Result<(), String> {
        // A run with no `finished_at` is in flight or was interrupted. Deleting it
        // mid-flight would leave the daemon writing to a row that is gone.
        let (pool, _dir) = store().await?;
        a_run_with(
            &pool,
            "acme",
            revlocal_core::RunStatus::Reviewing,
            1,
            None,
            None,
        )
        .await?;

        let report = revlocal_cli::decide::vacuum(&pool, "2099-01-01")
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(report.runs_deleted, 0, "an unfinished run is not old");
        assert!(
            report.detail.contains("nothing to remove"),
            "{}",
            report.detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_vacuum_cutoff_that_is_not_a_date_is_refused() -> Result<(), String> {
        // Silently parsing "yesterday" as something is how a vacuum deletes the
        // wrong decade.
        let (pool, _dir) = store().await?;

        let error = revlocal_cli::decide::vacuum(&pool, "yesterday")
            .await
            .err()
            .ok_or("must refuse")?
            .to_string();

        assert!(error.contains("is not a date"), "{error}");
        assert!(error.contains("YYYY-MM-DD"), "and show the shape: {error}");
        Ok(())
    }

    /// A run that finished at `finished_at`, so a vacuum can see it.
    ///
    /// `finished_at` is set at insert rather than patched afterwards, because the
    /// store validates the whole row and a fixture that dodges that validation
    /// tests a database state that cannot occur.
    async fn a_finished_run(
        pool: &Pool,
        name: &str,
        finished_at: &str,
        transcript: Option<&str>,
    ) -> Result<RunId, String> {
        let at = chrono::DateTime::parse_from_rfc3339(finished_at)
            .map_err(|e| e.to_string())?
            .with_timezone(&chrono::Utc);
        let run = a_run_with(pool, name, revlocal_core::RunStatus::Done, 1, None, None).await?;
        let mut row = revlocal_store::RunStore::new(pool)
            .get(run)
            .await
            .map_err(|e| e.to_string())?;
        row.finished_at = Some(at);
        row.transcript_path = transcript.map(str::to_owned);
        row.attempt = 2;
        row.id = RunId::new(0);
        let inserted = revlocal_store::RunStore::new(pool)
            .insert(&row)
            .await
            .map_err(|e| e.to_string())?;
        Ok(inserted.id)
    }
}
