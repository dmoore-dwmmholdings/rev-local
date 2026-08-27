//! Depth selection (SPEC §9.3).
//!
//! Three tiers, each with a wall-clock budget: `summary` (3 min), `standard`
//! (10 min), `deep` (25 min, plus a self-refutation instruction).
//!
//! # A depth is never chosen without a reason
//!
//! [`DepthDecision`] carries the reasons that produced it. §18 forbids silent caps,
//! and "we only summarised your 200-file security change" is exactly the cap a user
//! must be able to see. The reasons are also what makes the summary-vs-deep conflict
//! below explicable rather than mysterious.
//!
//! # When size says summary and risk says deep, risk wins
//!
//! §9.3's table does not say which trigger wins when both fire — a 200-file commit
//! that touches `**/auth/**` matches the `summary` row and the `deep` row at once.
//!
//! **Deep wins.** `summary` is a *cost* degradation: the diff is too large to read
//! carefully, so we stop pretending. `deep` is a *risk* escalation. Letting cost
//! silently downgrade a security-relevant review is the precise failure §18 exists to
//! prevent, and it fails quietly — a summary review of an auth change reports nothing
//! and looks exactly like a clean one.
//!
//! The size problem does not go away; it is handled independently by §9.4's
//! truncation, which reduces the diff and says that it did. A deep review of a
//! truncated diff is worth more than a summary of a whole one.
//!
//! Both reasons are recorded, so the UI can say "deep, despite 200 files".

use globset::{Glob, GlobSetBuilder};
use revlocal_core::{Depth, DiffStat, Finding, RepoConfig, Severity};

/// Changed lines above which a change is summarised (SPEC §9.3).
pub const MAX_DEEP_LINES: u64 = 20_000;

/// Extensions that make a file documentation for §9.3's doc-only rule.
const DOC_EXTENSIONS: &[&str] = &["md", "rst", "txt", "adoc", "markdown"];

/// Exact filenames that are lockfiles, beyond the `*.lock` glob.
///
/// Named individually because a lockfile is defined by its ecosystem's convention,
/// not by a suffix — `go.sum` and `package-lock.json` share nothing but their role.
const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "npm-shrinkwrap.json",
    "composer.lock",
    "gemfile.lock",
    "poetry.lock",
    "pipfile.lock",
    "go.sum",
    "cargo.lock",
];

/// Path substrings §9.3 calls security-relevant.
///
/// Substrings, not globs, because §9.3 wrote them as patterns and because the
/// interesting paths are named inconsistently — `auth/`, `oauth2.rs`,
/// `src/authn/mod.rs`. This over-matches (`authors.rs` reads as security-relevant),
/// and that is the correct direction to be wrong in: a wasted deep review costs 25
/// minutes of local compute, a missed one costs a vulnerability.
const SECURITY_SUBSTRINGS: &[&str] = &["auth", "crypto", "payment", "passwd", "password", "secret"];

/// CI configuration paths §9.3 calls security-relevant.
///
/// A change here can exfiltrate anything the CI system holds, which is usually
/// everything.
const CI_PREFIXES: &[&str] = &[
    ".github/workflows/",
    ".github/actions/",
    ".circleci/",
    ".gitlab-ci.yml",
    "azure-pipelines.yml",
    "jenkinsfile",
    ".buildkite/",
    ".drone.yml",
];

/// Why a depth was chosen (SPEC §9.3, §18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthReason {
    /// More files than `deep_file_limit`.
    TooManyFiles {
        /// How many the change touches.
        files: u32,
        /// The configured ceiling.
        limit: u32,
    },
    /// More changed lines than [`MAX_DEEP_LINES`].
    TooManyLines {
        /// Insertions plus deletions.
        lines: u64,
        /// The ceiling.
        limit: u64,
    },
    /// Every path is documentation or a lockfile.
    DocsOrLockfilesOnly,
    /// A path matched `repo.config.sensitive_globs`.
    SensitivePath {
        /// The first matching path.
        path: String,
    },
    /// A path matched one of §9.3's built-in security patterns.
    SecurityPattern {
        /// The first matching path.
        path: String,
        /// Which pattern matched.
        pattern: String,
    },
    /// The pull request carries a label from `deep_labels`.
    DeepLabel {
        /// The label.
        label: String,
    },
    /// A `standard` run reported a finding at `critical` or `high`.
    SeverityEscalation {
        /// The finding's fingerprint.
        fingerprint: String,
        /// How bad it was.
        severity: Severity,
    },
    /// `sensitive_globs` did not compile, so every path is treated as sensitive.
    ///
    /// The opposite direction from `ignore_globs`, deliberately. A broken ignore list
    /// must review *more*; a broken sensitive list must also review more. Both err
    /// toward looking harder, because a config typo must never quietly reduce
    /// scrutiny.
    SensitiveGlobsInvalid,
    /// Nothing special: §9.3's default row.
    Default,
}

impl DepthReason {
    /// The depth this reason on its own implies.
    const fn implies(&self) -> Depth {
        match self {
            Self::TooManyFiles { .. } | Self::TooManyLines { .. } | Self::DocsOrLockfilesOnly => {
                Depth::Summary
            }
            Self::SensitivePath { .. }
            | Self::SecurityPattern { .. }
            | Self::DeepLabel { .. }
            | Self::SeverityEscalation { .. }
            | Self::SensitiveGlobsInvalid => Depth::Deep,
            Self::Default => Depth::Standard,
        }
    }

    /// A one-line explanation for the UI and the run record.
    pub fn describe(&self) -> String {
        match self {
            Self::TooManyFiles { files, limit } => {
                format!("{files} files changed, over the limit of {limit}")
            }
            Self::TooManyLines { lines, limit } => {
                format!("{lines} lines changed, over the limit of {limit}")
            }
            Self::DocsOrLockfilesOnly => "only documentation and lockfiles changed".to_owned(),
            Self::SensitivePath { path } => format!("`{path}` matches this repo's sensitive_globs"),
            Self::SecurityPattern { path, pattern } => {
                format!("`{path}` looks security-relevant (matched `{pattern}`)")
            }
            Self::DeepLabel { label } => format!("labelled `{label}`"),
            Self::SeverityEscalation {
                fingerprint,
                severity,
            } => format!("the standard review found a {severity} issue ({fingerprint})"),
            Self::SensitiveGlobsInvalid => {
                "sensitive_globs did not compile, so every path is treated as sensitive".to_owned()
            }
            Self::Default => "no rule applied".to_owned(),
        }
    }
}

/// A depth and everything that argued for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthDecision {
    /// The depth to run at.
    pub depth: Depth,
    /// Every reason that fired, deep ones first. Never empty.
    pub reasons: Vec<DepthReason>,
}

impl DepthDecision {
    /// Resolve a set of reasons into a decision.
    ///
    /// Deep beats summary beats standard — see the module docs for why risk outranks
    /// cost. `Depth`'s ordering is declaration order (`Summary < Standard < Deep`),
    /// so "the deepest reason wins" is `max`.
    fn resolve(mut reasons: Vec<DepthReason>) -> Self {
        if reasons.is_empty() {
            reasons.push(DepthReason::Default);
        }

        let depth = reasons
            .iter()
            .map(DepthReason::implies)
            .max()
            .unwrap_or(Depth::Standard);

        reasons.sort_by_key(|r| std::cmp::Reverse(r.implies()));

        Self { depth, reasons }
    }

    /// Whether any reason forced this depth *against* another that wanted less.
    ///
    /// True exactly when the decision is worth explaining unprompted: the change was
    /// large enough to summarise and got a deep review anyway.
    pub fn is_contested(&self) -> bool {
        self.reasons.iter().any(|r| r.implies() != self.depth)
    }

    /// The reasons rendered for a human, deepest first.
    pub fn explain(&self) -> Vec<String> {
        self.reasons.iter().map(DepthReason::describe).collect()
    }
}

/// Whether a path is documentation or a lockfile (SPEC §9.3).
fn is_doc_or_lockfile(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);

    if lower.ends_with(".lock") || LOCKFILE_NAMES.contains(&name) {
        return true;
    }

    // A file with no extension is not documentation. LICENSE and CHANGELOG are
    // borderline, but treating an extensionless file as docs would summarise a
    // change to a shell script or a Dockerfile.
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| DOC_EXTENSIONS.contains(&ext))
}

/// Which §9.3 security pattern a path matches, if any.
fn security_pattern(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();

    if lower.ends_with(".sql") {
        return Some("*.sql");
    }
    if let Some(prefix) = CI_PREFIXES.iter().find(|p| lower.starts_with(*p)) {
        return Some(prefix);
    }
    // Jenkinsfile can live anywhere in the tree.
    if lower.rsplit('/').next().is_some_and(|n| n == "jenkinsfile") {
        return Some("jenkinsfile");
    }

    SECURITY_SUBSTRINGS
        .iter()
        .find(|s| lower.contains(*s))
        .copied()
}

/// Choose the depth for a change before it is reviewed (SPEC §9.3).
///
/// `paths` must already have had `ignore_globs` applied — depth is decided on what
/// will actually be reviewed, not on what arrived. A change that is 200 vendored
/// files and one line of auth code is a one-file auth change.
pub fn select(
    paths: &[String],
    diff_stat: &DiffStat,
    labels: &[String],
    config: &RepoConfig,
) -> DepthDecision {
    let mut reasons = Vec::new();

    // --- summary triggers ---
    if diff_stat.files > config.deep_file_limit {
        reasons.push(DepthReason::TooManyFiles {
            files: diff_stat.files,
            limit: config.deep_file_limit,
        });
    }

    let lines = diff_stat.insertions.saturating_add(diff_stat.deletions);
    if lines > MAX_DEEP_LINES {
        reasons.push(DepthReason::TooManyLines {
            lines,
            limit: MAX_DEEP_LINES,
        });
    }

    // An empty path list is not "docs only" — it is a change we know nothing about,
    // and summarising it on the strength of `all()` over an empty slice would be an
    // accident of vacuous truth rather than a decision.
    if !paths.is_empty() && paths.iter().all(|p| is_doc_or_lockfile(p)) {
        reasons.push(DepthReason::DocsOrLockfilesOnly);
    }

    // --- deep triggers ---
    match build_globset(&config.sensitive_globs) {
        Some(sensitive) => {
            if let Some(path) = paths.iter().find(|p| sensitive.is_match(p.as_str())) {
                reasons.push(DepthReason::SensitivePath { path: path.clone() });
            }
        }
        None => {
            tracing::warn!(
                globs = ?config.sensitive_globs,
                "sensitive_globs did not compile; treating the change as sensitive"
            );
            reasons.push(DepthReason::SensitiveGlobsInvalid);
        }
    }

    if let Some((path, pattern)) = paths
        .iter()
        .find_map(|p| security_pattern(p).map(|pat| (p, pat)))
    {
        reasons.push(DepthReason::SecurityPattern {
            path: path.clone(),
            pattern: pattern.to_owned(),
        });
    }

    if let Some(label) = labels
        .iter()
        .find(|l| config.deep_labels.iter().any(|d| d.eq_ignore_ascii_case(l)))
    {
        reasons.push(DepthReason::DeepLabel {
            label: label.clone(),
        });
    }

    DepthDecision::resolve(reasons)
}

/// Whether a completed run's findings require a deeper re-run (SPEC §9.3).
///
/// Returns `None` when no escalation is due, so the caller cannot accidentally
/// re-run at the same depth.
///
/// # Exactly once
///
/// Only a `standard` run escalates. §9.3 says "≥1 `critical`/`high` finding in
/// `standard`", and confining the rule to that tier is what makes "exactly once"
/// structural rather than a counter someone has to remember to increment: a `deep`
/// run has nowhere deeper to go, and a `deep` re-run that found another high finding
/// would otherwise escalate forever.
///
/// A `summary` run does not escalate either. It ran because the diff was too large or
/// too dull to read properly, and none of that changed because it noticed something —
/// escalating there would hand a 25-minute budget to a 200-file diff and produce a
/// worse review, slowly.
pub fn escalate(current: Depth, findings: &[Finding]) -> Option<DepthDecision> {
    if current != Depth::Standard {
        return None;
    }

    let trigger = findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        // The worst finding, and the earliest among equals, so the reason is stable
        // across re-runs rather than depending on the order the engine emitted them.
        .max_by_key(|f| f.severity)?;

    Some(DepthDecision::resolve(vec![
        DepthReason::SeverityEscalation {
            fingerprint: trigger.fingerprint.clone(),
            severity: trigger.severity,
        },
    ]))
}

/// Whether a depth adds §9.3's self-refutation instruction to the prompt.
pub const fn requires_self_verification(depth: Depth) -> bool {
    matches!(depth, Depth::Deep)
}

/// Compile a glob set, or `None` if any pattern is invalid.
fn build_globset(patterns: &[String]) -> Option<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).ok()?);
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use revlocal_core::{Category, FindingId, FindingState, RunId, Timestamp};

    fn stat(files: u32, insertions: u64, deletions: u64) -> DiffStat {
        DiffStat {
            files,
            insertions,
            deletions,
        }
    }

    fn paths(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("src/mod{i}.rs")).collect()
    }

    fn finding(severity: Severity, fingerprint: &str) -> Finding {
        Finding {
            id: FindingId::new(1),
            run_id: RunId::new(1),
            fingerprint: fingerprint.to_owned(),
            severity,
            category: Category::Correctness,
            confidence: 0.9,
            file: Some("src/a.rs".to_owned()),
            line_start: None,
            line_end: None,
            title: "t".to_owned(),
            body: String::new(),
            failure_scenario: None,
            suggested_fix: None,
            state: FindingState::Open,
            created_at: Timestamp::default(),
        }
    }

    // --- criterion 1: a 200-file commit selects summary ---

    #[test]
    fn depth_a_200_file_commit_selects_summary() {
        let decision = select(
            &paths(200),
            &stat(200, 400, 100),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Summary);
        assert!(decision
            .explain()
            .iter()
            .any(|r| r.contains("200 files changed, over the limit of 150")));
    }

    #[test]
    fn depth_exactly_at_the_file_limit_is_not_summary() {
        let decision = select(&paths(150), &stat(150, 10, 10), &[], &RepoConfig::default());

        assert_eq!(decision.depth, Depth::Standard);
    }

    #[test]
    fn depth_over_20k_changed_lines_selects_summary() {
        let decision = select(
            &paths(3),
            &stat(3, 19_000, 1_001),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Summary);
        assert!(decision.explain().iter().any(|r| r.contains("20001 lines")));
    }

    #[test]
    fn depth_docs_and_lockfiles_only_selects_summary() {
        let decision = select(
            &[
                "README.md".to_owned(),
                "docs/guide.rst".to_owned(),
                "Cargo.lock".to_owned(),
                "go.sum".to_owned(),
            ],
            &stat(4, 20, 5),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Summary);
    }

    #[test]
    fn depth_one_code_file_among_the_docs_is_not_summary() {
        let decision = select(
            &["README.md".to_owned(), "src/lib.rs".to_owned()],
            &stat(2, 20, 5),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Standard);
    }

    /// `all()` over an empty slice is true. Without the emptiness guard, a change we
    /// know no paths for would be summarised as "docs only" — a real decision reached
    /// by vacuous truth.
    #[test]
    fn depth_an_empty_path_list_is_not_docs_only() {
        let decision = select(&[], &stat(0, 0, 0), &[], &RepoConfig::default());

        assert_eq!(decision.depth, Depth::Standard);
        assert_eq!(decision.reasons, [DepthReason::Default]);
    }

    /// An extensionless file is not documentation: `Dockerfile` and `Makefile` are
    /// code, and summarising a change to one because it has no suffix would be wrong
    /// in the dangerous direction.
    #[test]
    fn depth_extensionless_files_are_not_documentation() {
        assert!(!is_doc_or_lockfile("Dockerfile"));
        assert!(!is_doc_or_lockfile("Makefile"));
        assert!(!is_doc_or_lockfile("scripts/deploy"));
        assert!(is_doc_or_lockfile("README.md"));
        assert!(is_doc_or_lockfile("yarn.lock"));
        assert!(is_doc_or_lockfile("sub/dir/go.sum"));
    }

    // --- criterion 2: a sensitive_glob selects deep ---

    #[test]
    fn depth_a_sensitive_glob_selects_deep() {
        let decision = select(
            &["src/auth/session.rs".to_owned()],
            &stat(1, 5, 2),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Deep);
        assert!(decision
            .explain()
            .iter()
            .any(|r| r.contains("sensitive_globs")));
    }

    #[test]
    fn depth_builtin_security_patterns_select_deep() {
        for path in [
            "migrations/001_init.sql",
            ".github/workflows/release.yml",
            "billing/payment_intent.go",
            "lib/crypto_box.py",
            "Jenkinsfile",
            "src/oauth2/token.rs",
        ] {
            let decision = select(
                &[path.to_owned()],
                &stat(1, 5, 2),
                &[],
                &RepoConfig::default(),
            );
            assert_eq!(decision.depth, Depth::Deep, "{path} should select deep");
        }
    }

    #[test]
    fn depth_a_deep_label_selects_deep() {
        let config = RepoConfig {
            deep_labels: vec!["needs-security-review".to_owned()],
            ..RepoConfig::default()
        };
        let decision = select(
            &["src/lib.rs".to_owned()],
            &stat(1, 5, 2),
            &["Needs-Security-Review".to_owned()],
            &config,
        );

        assert_eq!(decision.depth, Depth::Deep);
        assert!(decision.explain().iter().any(|r| r.contains("labelled")));
    }

    #[test]
    fn depth_an_unlisted_label_does_nothing() {
        let decision = select(
            &["src/lib.rs".to_owned()],
            &stat(1, 5, 2),
            &["bug".to_owned()],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Standard);
    }

    /// The conflict §9.3's table leaves open. Risk outranks cost: a summary review of
    /// an auth change reports nothing and is indistinguishable from a clean one.
    #[test]
    fn depth_risk_beats_size_when_both_fire() {
        let mut files = paths(200);
        files.push("src/auth/session.rs".to_owned());

        let decision = select(
            &files,
            &stat(201, 9_000, 3_000),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Deep);
        assert!(decision.is_contested());
        // §18: the losing reason is still recorded, so the UI can say "deep, despite
        // 201 files" rather than leaving the size rule looking broken.
        let explained = decision.explain();
        assert!(explained.iter().any(|r| r.contains("201 files")));
        assert!(explained.iter().any(|r| r.contains("sensitive_globs")));
        assert_eq!(explained.len(), 3, "{explained:?}");
    }

    #[test]
    fn depth_the_deep_reason_is_listed_first() {
        let mut files = paths(200);
        files.push("src/auth/session.rs".to_owned());

        let decision = select(&files, &stat(201, 10, 10), &[], &RepoConfig::default());

        assert_eq!(decision.reasons[0].implies(), Depth::Deep);
    }

    #[test]
    fn depth_an_ordinary_change_is_standard_and_uncontested() {
        let decision = select(
            &["src/lib.rs".to_owned()],
            &stat(1, 30, 4),
            &[],
            &RepoConfig::default(),
        );

        assert_eq!(decision.depth, Depth::Standard);
        assert_eq!(decision.reasons, [DepthReason::Default]);
        assert!(!decision.is_contested());
    }

    /// A config typo must never quietly reduce scrutiny. `ignore_globs` failing to
    /// compile reviews everything; `sensitive_globs` failing to compile treats
    /// everything as sensitive. Both err toward looking harder.
    #[test]
    fn depth_invalid_sensitive_globs_escalate_rather_than_disappear() {
        let config = RepoConfig {
            sensitive_globs: vec!["***/[".to_owned()],
            ..RepoConfig::default()
        };
        let decision = select(&["src/lib.rs".to_owned()], &stat(1, 5, 2), &[], &config);

        assert_eq!(decision.depth, Depth::Deep);
        assert!(decision
            .reasons
            .contains(&DepthReason::SensitiveGlobsInvalid));
    }

    // --- criterion 3: a critical finding triggers a deep re-run exactly once ---

    #[test]
    fn depth_a_critical_finding_escalates_a_standard_run() {
        let escalation = escalate(Depth::Standard, &[finding(Severity::Critical, "abc")])
            .expect("critical escalates");

        assert_eq!(escalation.depth, Depth::Deep);
        assert!(escalation.explain()[0].contains("abc"));
    }

    #[test]
    fn depth_a_high_finding_escalates_a_standard_run() {
        assert_eq!(
            escalate(Depth::Standard, &[finding(Severity::High, "abc")])
                .expect("high escalates")
                .depth,
            Depth::Deep
        );
    }

    #[test]
    fn depth_medium_and_below_do_not_escalate() {
        for severity in [Severity::Medium, Severity::Low, Severity::Info] {
            assert!(
                escalate(Depth::Standard, &[finding(severity, "abc")]).is_none(),
                "{severity} should not escalate"
            );
        }
    }

    /// "Exactly once" is structural, not a counter: the deep re-run cannot escalate
    /// again, however bad its findings are. Without this, a deep run that keeps
    /// finding high-severity issues re-runs forever.
    #[test]
    fn depth_a_deep_run_never_escalates_again() {
        assert!(escalate(Depth::Deep, &[finding(Severity::Critical, "abc")]).is_none());
    }

    /// A summary run ran because the diff was too large to read properly. Noticing
    /// something does not change that — escalating would hand 25 minutes to a
    /// 200-file diff and produce a worse review, slowly.
    #[test]
    fn depth_a_summary_run_does_not_escalate() {
        assert!(escalate(Depth::Summary, &[finding(Severity::Critical, "abc")]).is_none());
    }

    #[test]
    fn depth_a_clean_standard_run_does_not_escalate() {
        assert!(escalate(Depth::Standard, &[]).is_none());
    }

    /// The reason names the *worst* finding, and is stable across re-runs rather than
    /// depending on the order the engine happened to emit them.
    #[test]
    fn depth_escalation_names_the_worst_finding() {
        let escalation = escalate(
            Depth::Standard,
            &[
                finding(Severity::High, "high-one"),
                finding(Severity::Critical, "critical-one"),
                finding(Severity::High, "high-two"),
            ],
        )
        .expect("escalates");

        assert!(escalation.explain()[0].contains("critical-one"));
    }

    // --- criterion 4: timeouts scale with depth ---

    /// Cross-check: the daemon decides the depth, the engine crate spends the budget.
    /// If they ever disagree, a "deep" review would run on a summary's three minutes
    /// and be killed for a timeout that looks like an engine fault.
    #[test]
    fn depth_timeouts_scale_with_depth() {
        use revlocal_engine::timeout_for;
        use std::time::Duration;

        assert_eq!(timeout_for(Depth::Summary), Duration::from_secs(3 * 60));
        assert_eq!(timeout_for(Depth::Standard), Duration::from_secs(10 * 60));
        assert_eq!(timeout_for(Depth::Deep), Duration::from_secs(25 * 60));

        assert!(timeout_for(Depth::Summary) < timeout_for(Depth::Standard));
        assert!(timeout_for(Depth::Standard) < timeout_for(Depth::Deep));
    }

    /// `resolve` picks the deepest reason with `max`, which is only correct while
    /// `Depth`'s declaration order is Summary < Standard < Deep. Reordering the enum
    /// would silently invert every conflict resolution above.
    #[test]
    fn depth_ordering_is_summary_standard_deep() {
        assert!(Depth::Summary < Depth::Standard);
        assert!(Depth::Standard < Depth::Deep);
    }

    #[test]
    fn depth_only_deep_adds_the_self_verification_instruction() {
        assert!(requires_self_verification(Depth::Deep));
        assert!(!requires_self_verification(Depth::Standard));
        assert!(!requires_self_verification(Depth::Summary));
    }

    #[test]
    fn depth_every_decision_carries_at_least_one_reason() {
        for decision in [
            select(&[], &stat(0, 0, 0), &[], &RepoConfig::default()),
            select(&paths(300), &stat(300, 1, 1), &[], &RepoConfig::default()),
            select(
                &["a/auth/b.rs".to_owned()],
                &stat(1, 1, 1),
                &[],
                &RepoConfig::default(),
            ),
        ] {
            assert!(!decision.reasons.is_empty());
            assert!(decision.explain().iter().all(|r| !r.is_empty()));
        }
    }
}
