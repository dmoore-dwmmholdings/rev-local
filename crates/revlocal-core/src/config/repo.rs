//! The per-repo config document — `repo.config_json` (SPEC §13.2).

use super::{collect_unknown_keys, ConfigWarning, Extra};
use crate::{AutonomyMode, Category, EngineKind, Severity};
use serde::{Deserialize, Serialize};

/// Per-repository configuration (SPEC §13.2).
///
/// Every field has the default from §13.2, so a repo added with no configuration
/// behaves exactly as the spec's example document describes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoConfig {
    /// Branch globs to watch.
    pub branches: Vec<String>,
    /// Review pull requests.
    pub review_prs: bool,
    /// Review individual commits.
    pub review_commits: bool,
    /// Review draft pull requests.
    pub review_draft_prs: bool,
    /// Review merge commits.
    pub review_merge_commits: bool,
    /// For SVN, watch `branches/*` as well as trunk (decision D6).
    pub watch_branches: bool,
    /// Poll interval in seconds (SPEC §7.1).
    pub poll_interval_secs: u64,
    /// What the review covers (decision D8).
    pub scope: Vec<Category>,
    /// Which engine reviews this repo (decision D3).
    pub engine: EngineKind,
    /// This repo's requested autonomy, capped by the global ceiling (SPEC §12.2).
    pub autonomy: AutonomyMode,
    /// Paths never reviewed.
    pub ignore_globs: Vec<String>,
    /// Authors whose changes are never reviewed.
    pub ignore_authors: Vec<String>,
    /// Paths that force `deep` review (SPEC §9.3).
    pub sensitive_globs: Vec<String>,
    /// File count above which a change is reviewed as `summary` (SPEC §9.3).
    ///
    /// Defaults to 150. A diff this size cannot be read carefully in any budget, so
    /// the honest move is a cheap summary rather than a deep review that pretends
    /// otherwise.
    pub deep_file_limit: u32,
    /// PR labels that force `deep` review (SPEC §9.3).
    ///
    /// Empty by default. rev-local does not know what labels a repository uses, and
    /// guessing at `security` would be a rule that silently never fires.
    pub deep_labels: Vec<String>,
    /// Files read as the repo's own conventions (decision D8).
    pub convention_files: Vec<String>,
    /// Publish targets enabled for this repo.
    pub targets: Vec<String>,
    /// Andare project key for filed issues (SPEC §11.4).
    pub andare_project: Option<String>,
    /// Minimum severity that becomes an Andare issue (SPEC §11.4).
    pub andare_min_severity: Severity,
    /// Pattern for finding a work-item key in a commit message (SPEC §11.4).
    pub andare_key_regex: String,
    /// Trama space for review pages (SPEC §11.5).
    pub trama_space: Option<String>,
    /// Whether Trama pages are published rather than left as drafts.
    ///
    /// Load-bearing for risk: publishing is high risk, a draft is low (SPEC §12.3).
    pub trama_publish: bool,
    /// Total budget for repo-convention files in the prompt (SPEC §9.2).
    ///
    /// 24 KB. Conventions are the whole basis of the convention/architecture-drift
    /// scope (D8), and a repository with a 400 KB CONTRIBUTING.md would otherwise
    /// crowd the diff out of the prompt entirely.
    pub max_convention_bytes: usize,
    /// Per-file diff budget before hunks become a stat line (SPEC §9.4).
    pub max_file_diff_bytes: usize,
    /// Total diff budget before whole files are dropped (SPEC §9.4).
    pub max_total_diff_bytes: usize,
    /// Whether the webhook listener is enabled for this repo (SPEC §7.3).
    ///
    /// Off by default and opt-in per repo, as §7.3 requires: a listener bound
    /// without the user asking is a port they did not open.
    pub webhook_enabled: bool,
    /// Keychain entry holding this repo's webhook secret (SPEC §7.3, §13.1).
    ///
    /// A **reference**, never the secret. §13.1: secrets are never in this file.
    pub webhook_secret_ref: Option<String>,
    /// Whether `request_changes` produces a failing check (SPEC §11.3).
    pub block_on_findings: bool,
    /// Whether the app may submit a GitHub `APPROVE` review.
    ///
    /// Default `false`. SPEC §10.2: an AI approving code is a stronger claim than
    /// the product makes unattended.
    pub allow_approve: bool,
    /// Pattern for detecting an SVN branch reintegration (decision D6).
    pub merge_detect_regex: String,
    /// Keys present in the document that this version does not know.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for RepoConfig {
    /// Exactly the document in SPEC §13.2.
    fn default() -> Self {
        Self {
            branches: vec!["main".to_owned(), "release/*".to_owned()],
            review_prs: true,
            review_commits: false,
            review_draft_prs: false,
            review_merge_commits: false,
            watch_branches: true,
            poll_interval_secs: 120,
            scope: vec![
                Category::Correctness,
                Category::Security,
                Category::Convention,
                Category::Tests,
            ],
            engine: EngineKind::Claude,
            autonomy: AutonomyMode::AutoLowAskHigh,
            // SPEC §9.4's list, which is the fuller of the two the spec gave;
            // §13.2's example document listed only the first three. See ADR 0014.
            // "generated-file markers" from §9.4 are deliberately absent: that is a
            // content check, not a glob, and it is tracked separately.
            ignore_globs: vec![
                "**/node_modules/**".to_owned(),
                "**/vendor/**".to_owned(),
                "**/*.lock".to_owned(),
                "**/dist/**".to_owned(),
                "**/*.min.*".to_owned(),
                "**/target/**".to_owned(),
            ],
            ignore_authors: vec!["dependabot[bot]".to_owned(), "renovate[bot]".to_owned()],
            sensitive_globs: vec![
                "**/auth/**".to_owned(),
                "**/crypto/**".to_owned(),
                "**/*.sql".to_owned(),
                ".github/workflows/**".to_owned(),
            ],
            deep_file_limit: 150,
            deep_labels: Vec::new(),
            convention_files: vec![
                "CLAUDE.md".to_owned(),
                "AGENTS.md".to_owned(),
                "CONTRIBUTING.md".to_owned(),
            ],
            targets: vec!["github".to_owned(), "andare".to_owned(), "trama".to_owned()],
            andare_project: None,
            andare_min_severity: Severity::High,
            andare_key_regex: r"[A-Z][A-Z0-9]+-\d+".to_owned(),
            trama_space: None,
            trama_publish: false,
            max_convention_bytes: 24 * 1024,
            max_file_diff_bytes: 64 * 1024,
            max_total_diff_bytes: 512 * 1024,
            webhook_enabled: false,
            webhook_secret_ref: None,
            block_on_findings: false,
            allow_approve: false,
            merge_detect_regex: r"(?i)\b(merge|reintegrat\w+)\b.*\b(branches?/[\w./-]+)".to_owned(),
            extra: Extra::default(),
        }
    }
}

impl RepoConfig {
    /// Parse a per-repo config document from `repo.config_json`.
    ///
    /// Unknown keys become warnings, not errors, for the same reason as the global
    /// document: an older rev-local must still run a repo configured by a newer one.
    pub fn parse_json(json: &str) -> Result<(Self, Vec<ConfigWarning>), serde_json::Error> {
        let config: Self = serde_json::from_str(json)?;
        let warnings = config.warnings();
        Ok((config, warnings))
    }

    /// Every unknown key in this document, as warnings.
    pub fn warnings(&self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        collect_unknown_keys(&self.extra, "", &mut warnings);
        warnings
    }

    /// Whether `category` is in this repo's review scope (decision D8).
    pub fn covers(&self, category: Category) -> bool {
        self.scope.contains(&category)
    }

    /// Whether `target` is enabled for this repo.
    pub fn targets_include(&self, target: &str) -> bool {
        self.targets.iter().any(|t| t == target)
    }
}
