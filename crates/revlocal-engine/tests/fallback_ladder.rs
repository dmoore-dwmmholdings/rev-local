//! Acceptance tests for `RL-404` — SPEC §8.2's output contract.
//!
//! These drive the **real subprocess fixture**, not the in-process mock. §8.2's
//! ladder is about what an engine actually leaves on disk and on stdout, and an
//! in-process mock returning a struct would skip exactly the part being tested.
//! `fixtures/mock-engine` has one mode per rung for this reason (`RL-203`).

mod fallback_ladder {
    use revlocal_core::{EngineKind, Usage, Verdict};
    use revlocal_engine::ladder::{resolve, RepairPass, RepairResult, Rung};
    use revlocal_engine::EngineError;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// Run the fixture engine in `mode`, returning its out dir and stdout.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    fn run_fixture(mode: &str) -> Result<(TempDir, String), String> {
        let out = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
        let output = std::process::Command::new(revlocal_engine::mock_engine_program())
            .env("MOCK_ENGINE_MODE", mode)
            .env("REVLOCAL_OUT", out.path())
            .output()
            .map_err(|e| format!("spawning the fixture: {e}"))?;

        Ok((out, String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    async fn climb(
        out: &Path,
        stdout: &str,
        repair: Option<&dyn RepairPass>,
    ) -> Result<revlocal_engine::ladder::LadderOutcome, EngineError> {
        resolve(EngineKind::Mock, out, stdout, repair).await
    }

    /// A repair pass that counts calls and returns whatever it was configured with.
    struct CountingRepair {
        calls: AtomicUsize,
        seen: std::sync::Mutex<Vec<String>>,
        response: Result<RepairResult, EngineError>,
    }

    impl CountingRepair {
        fn returning(json: &str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                seen: std::sync::Mutex::new(Vec::new()),
                response: Ok(RepairResult {
                    json: json.to_owned(),
                    usage: Usage {
                        tokens_in: 400,
                        tokens_out: 120,
                        cost_usd: None,
                    },
                }),
            }
        }

        fn failing() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                seen: std::sync::Mutex::new(Vec::new()),
                response: Err(EngineError::Failed {
                    id: EngineKind::Mock,
                    detail: "the repair invocation itself failed".to_owned(),
                }),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// The last text it was asked to repair, for asserting what gets sent.
        fn last_input(&self) -> Option<String> {
            self.seen.lock().ok().and_then(|s| s.last().cloned())
        }
    }

    #[async_trait::async_trait]
    impl RepairPass for CountingRepair {
        async fn repair(&self, malformed: &str) -> Result<RepairResult, EngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(malformed.to_owned());
            }
            self.response.clone()
        }
    }

    /// A valid document, for the repair pass to return.
    fn valid_json() -> String {
        serde_json::json!({
            "schema_version": 1,
            "verdict": "comment",
            "summary": "Repaired.",
            "findings": []
        })
        .to_string()
    }

    // --- one test per rung ----------------------------------------------------

    #[tokio::test]
    async fn ladder_rung_0_reads_result_json_and_is_not_degraded() {
        let (out, stdout) = run_fixture("valid").unwrap_or_else(|e| panic!("{e}"));
        let result = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(result.rung, Rung::ResultFile);
        assert_eq!(result.rung.letter(), "0");
        assert!(!result.rung.is_degraded());
        assert_eq!(
            result.outcome.degraded, None,
            "the contract being honoured is not a degradation"
        );
        assert_eq!(result.outcome.findings.len(), 2);
        assert_eq!(result.outcome.verdict, Verdict::RequestChanges);
    }

    #[tokio::test]
    async fn ladder_rung_a_recovers_from_the_fenced_block_when_the_file_is_malformed() {
        // The fixture's `malformed_json` mode writes an unparseable result.json AND
        // a good fence, which is exactly the shape rung (a) exists for.
        let (out, stdout) = run_fixture("malformed_json").unwrap_or_else(|e| panic!("{e}"));
        assert!(
            out.path().join("result.json").is_file(),
            "the file must exist but be bad, or this is rung (b)"
        );

        let result = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(result.rung, Rung::FencedBlock);
        assert_eq!(result.rung.letter(), "a");
        assert_eq!(
            result.outcome.findings.len(),
            2,
            "the fence carried the real review"
        );
    }

    #[tokio::test]
    async fn ladder_rung_a_takes_the_last_fence_not_the_first() {
        // §8.2 says the LAST block is authoritative, and the fixture emits two on
        // purpose: an engine that answers, reconsiders and answers again puts its
        // draft first. Taking the first would review the draft and report it.
        let (out, stdout) = run_fixture("fenced_only").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            stdout.matches("```json").count(),
            2,
            "the fixture must emit two"
        );
        assert!(!out.path().join("result.json").exists());

        let result = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(result.rung, Rung::FencedBlock);
        assert_eq!(
            result.outcome.verdict,
            Verdict::RequestChanges,
            "the FIRST fence is a draft that approves; taking it would report a clean review"
        );
        assert_eq!(result.outcome.findings.len(), 2);
    }

    #[tokio::test]
    async fn ladder_rung_b_parses_the_whole_of_stdout() {
        let (out, stdout) = run_fixture("no_file").unwrap_or_else(|e| panic!("{e}"));
        assert!(!out.path().join("result.json").exists());
        assert!(
            !stdout.contains("```"),
            "no fence, or this would be rung (a)"
        );

        let result = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(result.rung, Rung::WholeStdout);
        assert_eq!(result.rung.letter(), "b");
        assert_eq!(result.outcome.findings.len(), 2);
    }

    #[tokio::test]
    async fn ladder_rung_c_repairs_once_and_charges_its_tokens() {
        // Acceptance criterion 2. The repair costs tokens, and a salvage that spent
        // them invisibly would let a repo exceed a limit its operator set.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        std::fs::write(out.path().join("result.json"), "{\"schema_version\": 1,")
            .unwrap_or_else(|e| panic!("write: {e}"));

        let repair = CountingRepair::returning(&valid_json());
        let result = climb(out.path(), "no json here at all", Some(&repair))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(result.rung, Rung::Repair);
        assert_eq!(result.rung.letter(), "c");
        assert_eq!(repair.calls(), 1, "at most once");
        assert_eq!(
            result.outcome.usage.tokens_in, 400,
            "the repair's tokens must reach the outcome, or the budget under-counts"
        );
        assert_eq!(result.outcome.usage.tokens_out, 120);
    }

    #[tokio::test]
    async fn ladder_the_repair_is_shown_the_engines_own_json_not_the_whole_transcript() {
        // A repair prompt containing a megabyte of progress logs costs more and
        // succeeds less than one containing the JSON that nearly worked.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let nearly = "{\"schema_version\": 1, \"verdict\": \"comment\",";
        std::fs::write(out.path().join("result.json"), nearly)
            .unwrap_or_else(|e| panic!("write: {e}"));

        let repair = CountingRepair::returning(&valid_json());
        let noisy_stdout = "progress: 1%\nprogress: 2%\n".repeat(500);
        climb(out.path(), &noisy_stdout, Some(&repair))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let sent = repair.last_input().unwrap_or_default();
        assert_eq!(
            sent, nearly,
            "the engine's own attempt is what gets sent back"
        );
        assert!(!sent.contains("progress:"), "not the transcript");
    }

    #[tokio::test]
    async fn ladder_rung_d_fails_without_fabricating_findings() {
        // Acceptance criterion 3, and §8.2's "Never guess findings." A salvaged
        // half-document is worse than no review, because a review reporting nothing
        // looks exactly like a clean one.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        std::fs::write(out.path().join("result.json"), "not json")
            .unwrap_or_else(|e| panic!("write: {e}"));

        let error = climb(out.path(), "also not json", None)
            .await
            .expect_err("nothing was recoverable");

        assert_eq!(error.code(), "engine_output_unparseable");
        assert!(!error.is_cancellation(), "this is a failure, not a stop");
    }

    #[tokio::test]
    async fn ladder_a_failed_repair_still_ends_in_unparseable_not_in_a_guess() {
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        std::fs::write(out.path().join("result.json"), "{")
            .unwrap_or_else(|e| panic!("write: {e}"));

        let repair = CountingRepair::returning("still not json");
        let error = climb(out.path(), "nothing", Some(&repair))
            .await
            .expect_err("a repair that produced nonsense must not be trusted");

        assert_eq!(error.code(), "engine_output_unparseable");
        assert_eq!(repair.calls(), 1, "and it must not be retried");
    }

    #[tokio::test]
    async fn ladder_a_repair_that_errors_propagates_rather_than_being_swallowed() {
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        std::fs::write(out.path().join("result.json"), "{")
            .unwrap_or_else(|e| panic!("write: {e}"));

        let repair = CountingRepair::failing();
        let error = climb(out.path(), "nothing", Some(&repair))
            .await
            .expect_err("the repair failed");
        assert_eq!(error.code(), "engine_failed");
    }

    #[tokio::test]
    async fn ladder_without_a_repair_pass_the_ladder_stops_at_rung_b() {
        // Whether to spend tokens on a repair is the budget guard's decision, not
        // this function's. A caller with nothing left passes None.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        std::fs::write(out.path().join("result.json"), "{")
            .unwrap_or_else(|e| panic!("write: {e}"));

        let error = climb(out.path(), "nothing", None)
            .await
            .expect_err("with no repair available there is nowhere left to go");
        assert_eq!(error.code(), "engine_output_unparseable");
    }

    // --- degraded -------------------------------------------------------------

    #[tokio::test]
    async fn ladder_degraded_is_set_for_a_b_and_c_and_unset_for_0() {
        // Acceptance criterion 4, and the reason it matters: §12.3 escalates every
        // publish action on a degraded run to high risk. This is what puts a salvaged
        // review in front of a human.
        assert!(!Rung::ResultFile.is_degraded());
        assert_eq!(Rung::ResultFile.degraded_reason(), None);

        for rung in [Rung::FencedBlock, Rung::WholeStdout, Rung::Repair] {
            assert!(rung.is_degraded(), "{rung:?}");
            let reason = rung
                .degraded_reason()
                .unwrap_or_else(|| panic!("{rung:?} is degraded but gives no reason"));
            assert!(
                !reason.is_empty(),
                "an escalation nobody can explain is worse than none"
            );
        }

        // ...and observed end to end, not only on the enum.
        let (out, stdout) = run_fixture("malformed_json").unwrap_or_else(|e| panic!("{e}"));
        let salvaged = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(salvaged.outcome.is_degraded());
        assert!(
            salvaged
                .outcome
                .degraded
                .as_deref()
                .unwrap_or_default()
                .contains("fenced"),
            "the reason should say which rung salvaged it: {:?}",
            salvaged.outcome.degraded
        );

        let (clean_out, clean_stdout) = run_fixture("valid").unwrap_or_else(|e| panic!("{e}"));
        let clean = climb(clean_out.path(), &clean_stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!clean.outcome.is_degraded());
    }

    #[tokio::test]
    async fn ladder_every_degraded_reason_is_distinct() {
        // They are stored on the run and shown in the approvals inbox. Two rungs
        // sharing a reason would make the inbox unable to say what actually happened.
        let reasons: std::collections::BTreeSet<&str> =
            [Rung::FencedBlock, Rung::WholeStdout, Rung::Repair]
                .iter()
                .filter_map(|r| r.degraded_reason())
                .collect();
        assert_eq!(reasons.len(), 3);
    }

    // --- interaction with §8.3 -------------------------------------------------

    #[tokio::test]
    async fn ladder_findings_are_still_validated_on_a_salvaged_rung() {
        // Salvaging the document does not lower the bar for what is in it. A rung
        // that skipped validation would let a malformed finding through precisely
        // when the engine was already misbehaving.
        let (out, stdout) = run_fixture("partial_findings").unwrap_or_else(|e| panic!("{e}"));
        let result = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            result.rung,
            Rung::ResultFile,
            "this mode writes a valid envelope"
        );
        assert!(
            !result.dropped.is_empty(),
            "the invalid findings must be dropped"
        );
        assert!(
            !result.outcome.findings.is_empty(),
            "and the valid ones kept"
        );
    }

    #[tokio::test]
    async fn ladder_a_nonzero_exit_does_not_stop_the_output_being_read() {
        // The fixture's `nonzero_exit` mode writes a valid document and then fails.
        // A runner keying only off the exit code would throw away a review the
        // engine actually produced.
        let (out, stdout) = run_fixture("nonzero_exit").unwrap_or_else(|e| panic!("{e}"));
        let result = climb(out.path(), &stdout, None)
            .await
            .unwrap_or_else(|e| panic!("a usable document must still be read: {e}"));
        assert_eq!(result.rung, Rung::ResultFile);
        assert_eq!(result.outcome.findings.len(), 2);
    }

    // --- fence scanning --------------------------------------------------------

    #[test]
    fn ladder_a_fence_inside_the_payload_does_not_end_the_block() {
        // A finding quoting a markdown code block is ordinary — `suggested_fix` is
        // markdown. Index arithmetic over the raw text would close the block early
        // and produce truncated JSON.
        use revlocal_engine::ladder::last_fenced_json_block;

        let stdout = "```json\n{\n  \"suggested_fix\": \"use ``` for code\"\n}\n```\n";
        let block = last_fenced_json_block(stdout)
            .unwrap_or_else(|| panic!("the block should have been found"));
        assert!(block.contains("suggested_fix"));
        assert!(
            block.trim().ends_with('}'),
            "the block must be complete: {block}"
        );
    }

    #[test]
    fn ladder_no_fence_yields_nothing_rather_than_the_whole_transcript() {
        use revlocal_engine::ladder::last_fenced_json_block;
        assert_eq!(last_fenced_json_block("just some prose"), None);
    }
}
