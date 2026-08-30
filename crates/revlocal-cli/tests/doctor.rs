//! `revlocal doctor` (RL-1202, SPEC §8.4, §14).
//!
//! Most of what doctor exists to say cannot be produced on a healthy machine, so
//! the report is built from a pure function over probe results and the failure
//! paths are tested directly. A suite that only ran `doctor` here would assert
//! that everything is fine, which is the one case nobody needs help with.

mod doctor {
    use std::process::Command;

    use revlocal_cli::doctor::{
        engine_check, gather, render, required_tool_check, svn_check, target_check, Check, Health,
    };

    fn binary() -> std::path::PathBuf {
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
    fn an_engine_is_usable_or_unusable_with_a_reason() {
        // Criterion 1. §8.4 separates three ways an engine can fail because they
        // need three different actions, and collapsing them loses the one that
        // matters most.
        let missing = engine_check("claude-code", false, None, false, None, &[]);
        assert_eq!(missing.health, Health::Fail);
        assert!(missing.remediation.is_some());

        let logged_out = engine_check("claude-code", true, Some("1.2.3"), false, None, &[]);
        assert_eq!(logged_out.health, Health::Fail);
        let remedy = logged_out.remediation.unwrap_or_default();
        // Decision D9: report, never fix. A doctor offering to log in would be
        // asking for credentials this product exists not to hold.
        assert!(remedy.contains("stores no API keys"), "{remedy}");

        let healthy = engine_check("claude-code", true, Some("1.2.3"), true, Some(true), &[]);
        assert_eq!(healthy.health, Health::Ok);
        assert!(healthy.detail.contains("1.2.3"));
    }

    #[test]
    fn an_engine_that_runs_but_breaks_the_contract_is_the_case_doctor_is_for() {
        // Installed, logged in, and its output no longer parses — because its flags
        // changed under it. Invisible to a version check, and the failure doctor
        // exists to catch before a real review spends tokens discovering it.
        let broken = engine_check("codex", true, Some("2.0.0"), true, Some(false), &[]);

        assert_eq!(broken.health, Health::Fail);
        assert!(
            broken.detail.contains("no usable result.json"),
            "{}",
            broken.detail
        );
        let remedy = broken.remediation.unwrap_or_default();
        assert!(
            remedy.contains("§8.2"),
            "must point at the contract: {remedy}"
        );
    }

    #[test]
    fn an_unverified_contract_is_a_warning_not_a_pass() {
        // §8.4's smoke task spends tokens, so it is opt-in — which means "not
        // checked" is a state, and reporting it as `ok` would be the report
        // claiming something nobody verified.
        let unverified = engine_check("claude-code", true, Some("1.2.3"), true, None, &[]);

        assert_eq!(unverified.health, Health::Warn);
        assert!(
            !unverified.health.is_blocking(),
            "a warning must not fail doctor"
        );
        assert!(unverified
            .remediation
            .unwrap_or_default()
            .contains("--smoke"));
    }

    #[test]
    fn a_target_reports_tools_and_both_capability_counts() {
        // Criterion 2. §11.2: unmapped is only useful if somebody can see it. A
        // target that binds four of five publishes fine until a run needs the
        // fifth.
        let partial = target_check("github", true, 12, 4, 1);
        assert_eq!(partial.health, Health::Warn);
        assert!(partial.detail.contains("12 tool"), "{}", partial.detail);
        assert!(
            partial.detail.contains("4 capability"),
            "{}",
            partial.detail
        );
        assert!(partial.detail.contains("1 unmapped"), "{}", partial.detail);
        assert!(partial
            .remediation
            .unwrap_or_default()
            .contains("targets map"));

        let complete = target_check("andare", true, 9, 5, 0);
        assert_eq!(complete.health, Health::Ok);

        let unreachable = target_check("trama", false, 0, 0, 0);
        assert_eq!(unreachable.health, Health::Fail);
    }

    #[test]
    fn missing_svn_blocks_only_when_svn_repositories_are_configured() {
        // Criterion 3. An absent tool nobody uses reported as a failure trains
        // people to ignore the report, which costs more than the line saves.
        let irrelevant = svn_check(false, 0);
        assert_eq!(irrelevant.health, Health::NotNeeded);
        assert!(!irrelevant.health.is_blocking());
        assert!(irrelevant.detail.contains("git is unaffected"));

        let blocking = svn_check(false, 3);
        assert_eq!(blocking.health, Health::Fail);
        assert!(
            blocking.detail.contains("3 SVN repository"),
            "{}",
            blocking.detail
        );
        let remedy = blocking.remediation.unwrap_or_default();
        // All three platforms, because the person reading this is on one of them
        // and does not know which the author had in mind.
        assert!(remedy.contains("brew"), "{remedy}");
        assert!(remedy.contains("apt-get"), "{remedy}");
        assert!(remedy.contains("winget"), "{remedy}");

        assert_eq!(svn_check(true, 0).health, Health::Ok);
        assert_eq!(svn_check(true, 2).health, Health::Ok);
    }

    #[test]
    fn every_failure_line_includes_a_concrete_next_action() {
        // Criterion 4, checked over every way a check can fail rather than a
        // sample. A failing line without a next action is a diagnosis.
        let failures = [
            engine_check("e", false, None, false, None, &[]),
            engine_check("e", true, Some("1"), false, None, &[]),
            engine_check("e", true, Some("1"), true, Some(false), &[]),
            target_check("t", false, 0, 0, 0),
            svn_check(false, 1),
            // The real remediation, not a placeholder — a fixture shorter than
            // the rule under test proves nothing about the rule.
            required_tool_check(
                "git",
                false,
                "every git and GitHub repository",
                "install git (brew install git / apt-get install git / winget install --id Git.Git)",
            ),
        ];

        for check in failures {
            assert_eq!(check.health, Health::Fail, "{}", check.name);
            let remedy = check.remediation.unwrap_or_default();
            assert!(
                remedy.len() > 15,
                "{} fails with no usable next action: {remedy:?}",
                check.name
            );
        }
    }

    #[test]
    fn the_human_line_puts_the_action_where_it_can_be_read() {
        let check = svn_check(false, 2);
        let line = check.line();

        assert!(line.contains("[FAIL]"), "{line}");
        assert!(line.contains("try:"), "{line}");
        // On its own line, so a long remediation does not run off the right edge
        // behind the detail.
        assert!(line.contains('\n'), "{line}");
    }

    #[test]
    fn a_report_with_no_failures_says_so_rather_than_ending_in_silence() {
        // A report that just stops looks like a report that stopped early.
        let report = gather(0);
        let human = render(&report, false).unwrap_or_default();

        assert!(human.contains("ok,"), "{human}");
        if !report.has_failures() {
            assert!(human.contains("Nothing is blocking a review"), "{human}");
        }
    }

    #[test]
    fn the_json_keeps_its_shape_when_a_section_is_empty() {
        // Criterion 5. Engines and targets need config the CLI does not have yet;
        // they are empty arrays rather than absent, so a consumer written today
        // still parses tomorrow's output.
        let report = gather(0);
        let json = render(&report, true).unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();

        for section in ["prerequisites", "engines", "targets", "platform"] {
            assert!(
                parsed[section].is_array(),
                "`{section}` must be an array even when empty: {json}"
            );
        }
        // `remediation` is omitted when there is none rather than being null, so a
        // consumer can treat its presence as "there is something to do".
        let ok_check = Check::ok("x", "fine");
        let value = serde_json::to_value(&ok_check).unwrap_or_default();
        assert!(value.get("remediation").is_none(), "{value}");
    }

    #[test]
    fn the_gate_command_runs_and_produces_exactly_one_json_document() -> Result<(), String> {
        // The item's gate, run as written: `revlocal doctor --json`.
        let output = Command::new(binary())
            .args(["doctor", "--json"])
            .output()
            .map_err(|e| format!("running revlocal doctor: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("stdout was not one JSON document: {e}\n{stdout}"))?;

        assert!(parsed["prerequisites"].is_array());
        // A healthy machine exits 0; an unhealthy one exits 1. Either is correct
        // here — what must not happen is exiting 0 while reporting a failure.
        let failed = parsed["prerequisites"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|check| check["health"] == "fail");
        if failed {
            assert_ne!(
                output.status.code(),
                Some(0),
                "a failing doctor must not exit 0"
            );
        }
        Ok(())
    }

    #[test]
    fn only_a_failure_blocks() {
        // `NotNeeded` exists precisely so an absent-but-unused tool does not fail
        // the command, and a warning that failed it is a warning nobody leaves in.
        assert!(Health::Fail.is_blocking());
        assert!(!Health::Ok.is_blocking());
        assert!(!Health::Warn.is_blocking());
        assert!(!Health::NotNeeded.is_blocking());
    }
}

/// Doctor says which engines cannot report token usage (RL-409, §8.1, §18).
///
/// A budget against an engine whose usage nobody can read is advisory. `budget
/// show` already reports the ledger honestly — it hedges an unmeasured day rather
/// than presenting it as a total — but by then the operator has already trusted a
/// ceiling that was not holding anything. Doctor is where they find out first.
#[test]
fn doctor_names_the_engines_whose_token_budgets_are_advisory() -> Result<(), String> {
    let report = revlocal_cli::doctor::gather(0);

    let usage: Vec<&revlocal_cli::doctor::Check> = report
        .engines
        .iter()
        .filter(|check| check.name.ends_with(":usage"))
        .collect();

    // One, not two. Claude became measured when RL-409's extractor landed —
    // `from_claude_json`, tested against a captured payload — so warning about it
    // would now be false. Codex has no extractor until RL-408 establishes what
    // `codex exec --json` emits.
    assert_eq!(
        usage.len(),
        1,
        "only unmeasured engines should be named: {:?}",
        report.engines.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        usage[0].name.contains("codex"),
        "and it is codex that is unmeasured: {}",
        usage[0].name
    );

    for check in &usage {
        // A warning, not a failure: reviews work, the budget is what does not.
        assert_eq!(
            check.health,
            revlocal_cli::doctor::Health::Warn,
            "{}: an engine that reviews fine must not fail doctor",
            check.name
        );
        assert!(
            check.detail.contains("advisory"),
            "{}: must say what it means for a budget, not just that a feature is \
             missing: {}",
            check.name,
            check.detail
        );
        // §8.4's rule: every non-ok check carries something to type next.
        let remediation = check
            .remediation
            .as_deref()
            .ok_or_else(|| format!("{} has no remediation", check.name))?;
        assert!(
            remediation.contains("daily_runs_per_repo"),
            "the remedy is the ceiling that *is* enforceable: {remediation}"
        );
    }

    // Neither the mock nor claude is listed: both are measured, and a line per
    // measured engine would bury the one that is not.
    for measured in ["mock", "claude"] {
        assert!(
            !report.engines.iter().any(|c| c.name.contains(measured)),
            "{measured} reports usage; warning about it would be false"
        );
    }
    Ok(())
}

/// The warning does not block a review.
#[test]
fn doctor_still_exits_zero_when_only_usage_is_unmeasured() {
    let report = revlocal_cli::doctor::gather(0);

    // Whatever else is true of this machine, an unmeasured engine must not be
    // the thing that stops somebody reviewing code.
    let blocking: Vec<&str> = report
        .all()
        .filter(|check| check.health == revlocal_cli::doctor::Health::Fail)
        .map(|check| check.name.as_str())
        .filter(|name| name.ends_with(":usage"))
        .collect();

    assert!(
        blocking.is_empty(),
        "usage checks must never block: {blocking:?}"
    );
}
