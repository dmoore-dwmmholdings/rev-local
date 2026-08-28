//! Review prompt assembly (SPEC §9.2).
//!
//! Seven sections, in a fixed order. The order is not decoration: an engine reads
//! top to bottom, and the output contract has to arrive before the thing it governs.
//! `prompt_sections_are_in_spec_order` asserts it against §9.2 rather than against
//! whatever the template currently says.
//!
//! # The template escapes by default, and that would corrupt every diff
//!
//! Handlebars HTML-escapes `{{value}}`. A diff run through it has `<` turned into
//! `&lt;`, and the engine reviews code nobody wrote — silently, plausibly, and with
//! findings that cite lines that do not exist. Every interpolation in
//! `review.md.hbs` is a triple-stache for that reason, and
//! `prompt_a_diff_is_not_html_escaped` is the guard.
//!
//! # Conventions are capped, and the cap is stated
//!
//! §9.2 gives repo conventions a byte budget, because they are the basis of the
//! convention scope (D8) and a 400 KB `CONTRIBUTING.md` would otherwise crowd the
//! diff out of the prompt entirely. Truncation is **announced inside the prompt**:
//! an engine shown two thirds of a style guide must not treat it as the whole one.

use std::path::Path;

use handlebars::Handlebars;
use revlocal_core::{Category, Change, Finding, RepoConfig};
use revlocal_engine::REVIEW_TEMPLATE;

/// The file the rendered prompt is written to, beside the transcript.
///
/// §9.2: "The rendered prompt is stored alongside the transcript for
/// reproducibility." A finding nobody can explain is usually a prompt nobody kept.
pub const PROMPT_FILE: &str = "prompt.md";

/// The seven sections of §9.2, in order.
///
/// Named here so the order is asserted against the spec rather than against the
/// template — a template edit that reordered them would otherwise pass.
pub const SECTIONS: [&str; 7] = [
    "Role and output contract",
    "Change metadata",
    "The diff",
    "Repository conventions",
    "Review scope",
    "Prior context",
    "Rules of engagement",
];

/// Files read as repository conventions, beyond `repo.config.convention_files`.
///
/// §9.2 names these explicitly. `.editorconfig` is included because it is the one
/// convention file that is machine-readable and therefore never argued about.
pub const DEFAULT_CONVENTION_FILES: [&str; 4] =
    ["CLAUDE.md", "AGENTS.md", "CONTRIBUTING.md", ".editorconfig"];

/// What can go wrong assembling a prompt.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// The template did not compile or render.
    ///
    /// A rev-local bug rather than a user one, and said so, because the message
    /// otherwise reads as something the user could fix.
    #[error("rev-local's review template failed to render: {0}\n  this is a bug in rev-local, not in your configuration")]
    Template(String),

    /// The rendered prompt could not be written beside the transcript.
    #[error("could not write the rendered prompt to {path}: {source}")]
    Write {
        /// Where it was being written.
        path: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// One convention file, as it will appear in the prompt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConventionFile {
    /// Repository-relative path.
    pub path: String,
    /// The content, possibly truncated.
    pub content: String,
    /// Whether it was cut short.
    pub truncated: bool,
    /// How much is shown.
    pub shown_bytes: usize,
    /// How much there was.
    pub total_bytes: usize,
}

/// Everything the template needs.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PromptContext {
    /// The §8.3 schema, inlined so the engine never has to be told where to find it.
    pub output_contract: String,
    /// Repository name.
    pub repo_name: String,
    /// `git` | `github` | `svn`.
    pub repo_kind: String,
    /// `commit` | `pr` | `svn_rev` | `svn_pseudo_pr`.
    pub change_kind: String,
    /// The change's identity in its own system.
    pub external_id: String,
    /// Branch, where the concept applies.
    pub branch: Option<String>,
    /// Author display name.
    pub author: Option<String>,
    /// Web URL.
    pub url: Option<String>,
    /// Commit message or PR body.
    pub message: Option<String>,
    /// Files touched.
    pub files_changed: u32,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
    /// The unified diff.
    pub diff: String,
    /// Whether the diff was reduced (§9.4).
    pub truncated: bool,
    /// What was left out, in full.
    pub omitted_files: Vec<String>,
    /// Repository conventions.
    pub conventions: Vec<ConventionFile>,
    /// The enabled review categories, with guidance.
    pub scope: Vec<ScopeEntry>,
    /// Findings from earlier runs on this change.
    pub prior_findings: Vec<PriorFinding>,
    /// Fingerprints a human asked never to see again.
    pub suppressed: Vec<String>,
    /// Whether section 6 has anything to say.
    pub has_prior_context: bool,
}

/// One review category and what it means (§9.2 section 5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScopeEntry {
    /// The category name.
    pub name: String,
    /// What to look for.
    pub guidance: String,
}

/// A finding from an earlier run on the same change (§9.2 section 6).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PriorFinding {
    /// Its fingerprint (§10.3).
    pub fingerprint: String,
    /// How bad it was.
    pub severity: String,
    /// Where.
    pub file: String,
    /// The claim.
    pub title: String,
}

/// Per-category guidance for §9.2's scope section (decision D8).
fn guidance_for(category: Category) -> &'static str {
    match category {
        Category::Correctness => "logic that produces a wrong result or a crash. Name the inputs.",
        Category::Security => {
            "untrusted input reaching somewhere it is trusted: injection, auth \
             bypass, unsafe deserialization, secrets in code."
        }
        Category::Convention => {
            "drift from the conventions stated above. Not from your own preferences \
             — if this repository does not state it, it is not a finding."
        }
        Category::Tests => {
            "behaviour this change introduces or alters that no test would catch \
             regressing. Name the behaviour, not the coverage percentage."
        }
        Category::Perf => {
            "work that is superlinear where it need not be, on a path that runs often."
        }
        Category::Other => "anything defensible that the categories above do not cover.",
    }
}

/// Read the repository's convention files from a materialized worktree (§9.2).
///
/// Absence is **not** an error: most repositories have some of these and no
/// repository has all of them. Failing here would stop a review over a missing
/// `CONTRIBUTING.md`, which is not a problem with the change.
///
/// The budget is shared across all files and spent in order, so a repository that
/// puts its real conventions in `CLAUDE.md` is not crowded out by a long
/// `CONTRIBUTING.md` read first — §9.2 lists `CLAUDE.md` first for that reason and
/// this preserves it.
pub fn read_conventions(worktree: &Path, config: &RepoConfig) -> Vec<ConventionFile> {
    let mut budget = config.max_convention_bytes;
    let mut files = Vec::new();

    let paths: Vec<String> = DEFAULT_CONVENTION_FILES
        .iter()
        .map(|p| (*p).to_owned())
        .chain(config.convention_files.iter().cloned())
        .collect();

    let mut seen = std::collections::BTreeSet::new();

    for path in paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        if budget == 0 {
            break;
        }

        let Ok(content) = std::fs::read_to_string(worktree.join(&path)) else {
            continue;
        };

        let total_bytes = content.len();
        let (shown, truncated) = if total_bytes <= budget {
            (content, false)
        } else {
            // Cut on a character boundary: slicing a UTF-8 string by bytes panics
            // mid-character, and a convention file with an em dash in it is ordinary.
            let mut end = budget;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            (content[..end].to_owned(), true)
        };

        budget = budget.saturating_sub(shown.len());
        files.push(ConventionFile {
            path,
            shown_bytes: shown.len(),
            content: shown,
            truncated,
            total_bytes,
        });
    }

    files
}

/// Build the context for one review.
#[allow(clippy::too_many_arguments)]
pub fn build_context(
    repo_name: &str,
    repo_kind: &str,
    change: &Change,
    config: &RepoConfig,
    diff: &str,
    truncated: bool,
    omitted_files: &[String],
    conventions: Vec<ConventionFile>,
    prior_findings: &[Finding],
    suppressed: &[String],
) -> PromptContext {
    let prior: Vec<PriorFinding> = prior_findings
        .iter()
        .map(|finding| PriorFinding {
            fingerprint: finding.fingerprint.clone(),
            severity: finding.severity.to_string(),
            file: finding
                .file
                .clone()
                .unwrap_or_else(|| "(no file)".to_owned()),
            title: finding.title.clone(),
        })
        .collect();

    PromptContext {
        output_contract: revlocal_engine::RESULT_SCHEMA_V1.to_owned(),
        repo_name: repo_name.to_owned(),
        repo_kind: repo_kind.to_owned(),
        change_kind: change.kind.to_string(),
        external_id: change.external_id.clone(),
        branch: change.branch.clone(),
        author: change.author_name.clone(),
        url: change.url.clone(),
        message: change.title.clone(),
        files_changed: change.diff_stat.files,
        insertions: change.diff_stat.insertions,
        deletions: change.diff_stat.deletions,
        diff: diff.to_owned(),
        truncated,
        omitted_files: omitted_files.to_vec(),
        conventions,
        scope: config
            .scope
            .iter()
            .map(|category| ScopeEntry {
                name: category.to_string(),
                guidance: guidance_for(*category).to_owned(),
            })
            .collect(),
        has_prior_context: !prior.is_empty() || !suppressed.is_empty(),
        prior_findings: prior,
        suppressed: suppressed.to_vec(),
    }
}

/// Render the prompt.
pub fn render(context: &PromptContext) -> Result<String, PromptError> {
    let mut handlebars = Handlebars::new();
    // Not strict: a section whose data is absent renders empty rather than failing
    // the run. A missing optional field must not cost a review.
    handlebars.set_strict_mode(false);
    handlebars
        .render_template(REVIEW_TEMPLATE, context)
        .map_err(|e| PromptError::Template(e.to_string()))
}

/// Render and write the prompt beside the transcript (§9.2).
pub fn render_to(context: &PromptContext, out_dir: &Path) -> Result<String, PromptError> {
    let rendered = render(context)?;
    let path = out_dir.join(PROMPT_FILE);

    std::fs::write(&path, &rendered).map_err(|source| PromptError::Write {
        path: path.display().to_string(),
        source,
    })?;

    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use revlocal_core::{
        ChangeId, ChangeKind, DiffStat, FindingId, FindingState, RepoId, RunId, Severity, Timestamp,
    };

    fn change() -> Change {
        Change {
            id: ChangeId::new(1),
            repo_id: RepoId::new(1),
            kind: ChangeKind::Commit,
            external_id: "abc123".to_owned(),
            title: Some("Fix the pager".to_owned()),
            author_name: Some("A Developer".to_owned()),
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat {
                files: 2,
                insertions: 10,
                deletions: 3,
            },
            detected_at: Timestamp::default(),
        }
    }

    fn context() -> PromptContext {
        build_context(
            "rev-local",
            "git",
            &change(),
            &RepoConfig::default(),
            "--- a/x\n+++ b/x\n",
            false,
            &[],
            Vec::new(),
            &[],
            &[],
        )
    }

    /// The order is asserted against SPEC §9.2's list, not against the template.
    /// Asserting the template against itself would pass for any order at all.
    #[test]
    fn prompt_sections_are_in_spec_order() {
        let rendered = render(&context()).expect("renders");

        let mut cursor = 0;
        for (index, section) in SECTIONS.iter().enumerate() {
            let heading = format!("## {}. {section}", index + 1);
            let at = rendered[cursor..]
                .find(&heading)
                .unwrap_or_else(|| panic!("section {heading:?} missing or out of order"));
            cursor += at + heading.len();
        }
    }

    /// Handlebars HTML-escapes by default. An escaped diff is a corrupted diff, and
    /// the corruption is silent: the engine reviews plausible-looking code that
    /// nobody wrote. Delete a `{` from any triple-stache in the template and this
    /// fails.
    #[test]
    fn prompt_a_diff_is_not_html_escaped() {
        let mut ctx = context();
        ctx.diff = "-if (a < b && c > d) {\n+if (a <= b || \"x\" == 'y') {\n".to_owned();

        let rendered = render(&ctx).expect("renders");

        assert!(
            rendered.contains(r#"+if (a <= b || "x" == 'y') {"#),
            "{rendered}"
        );
        assert!(!rendered.contains("&lt;"), "diff was HTML-escaped");
        assert!(!rendered.contains("&quot;"), "diff was HTML-escaped");
        assert!(!rendered.contains("&amp;"), "diff was HTML-escaped");
        assert!(!rendered.contains("&#x27;"), "diff was HTML-escaped");
    }

    /// Same hazard, different field: a commit message saying `use <T> not &T`.
    #[test]
    fn prompt_a_commit_message_is_not_html_escaped() {
        let mut ctx = context();
        ctx.message = Some("use <T> not &T".to_owned());

        assert!(render(&ctx).expect("renders").contains("use <T> not &T"));
    }

    #[test]
    fn prompt_inlines_the_output_contract() {
        let rendered = render(&context()).expect("renders");

        assert!(rendered.contains(r#""schema_version""#));
        assert!(rendered.contains("$REVLOCAL_OUT/result.json"));
    }

    #[test]
    fn prompt_reads_the_named_convention_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("CLAUDE.md"), "always use tabs").expect("write");
        std::fs::write(dir.path().join("CONTRIBUTING.md"), "sign your commits").expect("write");

        let files = read_conventions(dir.path(), &RepoConfig::default());

        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["CLAUDE.md", "CONTRIBUTING.md"]);
        assert!(files.iter().all(|f| !f.truncated));
    }

    /// §9.2: absence is not an error. Most repositories have some of these files and
    /// none has all of them; failing here would stop a review over a missing
    /// CONTRIBUTING.md, which is not a problem with the change.
    #[test]
    fn prompt_missing_convention_files_are_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(read_conventions(dir.path(), &RepoConfig::default()).is_empty());

        let mut ctx = context();
        ctx.conventions = Vec::new();
        let rendered = render(&ctx).expect("renders");

        assert!(rendered.contains("## 4. Repository conventions"));
        assert!(rendered.contains("states no conventions of its own"));
    }

    /// A repository that states nothing must not have conventions invented for it:
    /// every D8 finding would then be the engine's taste, reported as the repo's rule.
    #[test]
    fn prompt_with_no_conventions_forbids_inventing_them() {
        let mut ctx = context();
        ctx.conventions = Vec::new();

        assert!(render(&ctx).expect("renders").contains("Do not invent any"));
    }

    #[test]
    fn prompt_conventions_are_capped_at_max_convention_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("CLAUDE.md"), "x".repeat(5000)).expect("write");

        let config = RepoConfig {
            max_convention_bytes: 1000,
            ..RepoConfig::default()
        };
        let files = read_conventions(dir.path(), &config);

        assert_eq!(files.len(), 1);
        assert!(files[0].truncated);
        assert_eq!(files[0].shown_bytes, 1000);
        assert_eq!(files[0].total_bytes, 5000);
    }

    /// The budget is shared and spent in §9.2's order, so a long CONTRIBUTING.md
    /// cannot crowd out the CLAUDE.md that is listed first.
    #[test]
    fn prompt_convention_budget_is_shared_across_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("CLAUDE.md"), "y".repeat(800)).expect("write");
        std::fs::write(dir.path().join("CONTRIBUTING.md"), "z".repeat(800)).expect("write");

        let config = RepoConfig {
            max_convention_bytes: 1000,
            ..RepoConfig::default()
        };
        let files = read_conventions(dir.path(), &config);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].shown_bytes, 800);
        assert_eq!(files[1].shown_bytes, 200);
        assert!(files[1].truncated);
    }

    /// Truncating on a byte index inside a multi-byte character panics. A
    /// CONTRIBUTING.md with an em dash in it is entirely ordinary.
    #[test]
    fn prompt_convention_truncation_respects_char_boundaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("CLAUDE.md"), "—".repeat(100)).expect("write");

        let config = RepoConfig {
            max_convention_bytes: 10,
            ..RepoConfig::default()
        };
        let files = read_conventions(dir.path(), &config);

        assert_eq!(files[0].shown_bytes, 9);
        assert_eq!(files[0].content, "—".repeat(3));
    }

    /// §18, no silent caps. An engine shown two thirds of a style guide must not be
    /// left to treat it as the whole one.
    #[test]
    fn prompt_states_convention_truncation_in_the_prompt_itself() {
        let mut ctx = context();
        ctx.conventions = vec![ConventionFile {
            path: "CLAUDE.md".to_owned(),
            content: "partial".to_owned(),
            truncated: true,
            shown_bytes: 7,
            total_bytes: 900,
        }];

        let rendered = render(&ctx).expect("renders");

        assert!(rendered.contains("Truncated: showing the first 7 bytes of 900"));
    }

    /// §9.4 truncation, likewise announced — and the omitted files named, so
    /// "coverage" is a claim the engine can qualify.
    #[test]
    fn prompt_states_diff_truncation_and_names_omitted_files() {
        let mut ctx = context();
        ctx.truncated = true;
        ctx.omitted_files = vec!["vendor/huge.js".to_owned()];

        let rendered = render(&ctx).expect("renders");

        assert!(rendered.contains("This diff has been truncated"));
        assert!(rendered.contains("vendor/huge.js"));
    }

    #[test]
    fn prompt_an_untruncated_diff_says_nothing_about_truncation() {
        assert!(!render(&context())
            .expect("renders")
            .contains("has been truncated"));
    }

    #[test]
    fn prompt_lists_the_configured_scope_only() {
        let config = RepoConfig {
            scope: vec![Category::Security],
            ..RepoConfig::default()
        };
        let ctx = build_context(
            "r",
            "git",
            &change(),
            &config,
            "",
            false,
            &[],
            Vec::new(),
            &[],
            &[],
        );

        let rendered = render(&ctx).expect("renders");

        assert!(rendered.contains("**security**"));
        assert!(!rendered.contains("**perf**"));
    }

    /// The load-bearing half of §9.2 section 6: a human has already said they do not
    /// want to hear about these. Reporting one anyway spends their attention and
    /// costs the rest of the review its credibility.
    #[test]
    fn prompt_suppressed_fingerprints_are_marked_do_not_report() {
        let ctx = build_context(
            "r",
            "git",
            &change(),
            &RepoConfig::default(),
            "",
            false,
            &[],
            Vec::new(),
            &[],
            &["deadbeef".to_owned()],
        );

        let rendered = render(&ctx).expect("renders");

        let marker = rendered
            .find("Do not report these")
            .expect("marker present");
        let fingerprint = rendered.find("deadbeef").expect("fingerprint present");
        assert!(
            marker < fingerprint,
            "fingerprint listed before its warning"
        );
        assert!(marker > rendered.find("## 6. Prior context").expect("section 6"));
    }

    #[test]
    fn prompt_prior_findings_are_listed_with_their_fingerprints() {
        let finding = Finding {
            id: FindingId::new(1),
            run_id: RunId::new(1),
            fingerprint: "cafe1234".to_owned(),
            severity: Severity::High,
            category: Category::Correctness,
            confidence: 0.9,
            file: Some("src/pager.rs".to_owned()),
            line_start: Some(10),
            line_end: Some(12),
            title: "off-by-one in page bounds".to_owned(),
            body: String::new(),
            failure_scenario: None,
            suggested_fix: None,
            state: FindingState::Open,
            created_at: Timestamp::default(),
        };

        let ctx = build_context(
            "r",
            "git",
            &change(),
            &RepoConfig::default(),
            "",
            false,
            &[],
            Vec::new(),
            std::slice::from_ref(&finding),
            &[],
        );

        let rendered = render(&ctx).expect("renders");

        assert!(rendered.contains("cafe1234"));
        assert!(rendered.contains("off-by-one in page bounds"));
        assert!(rendered.contains("src/pager.rs"));
        assert!(rendered.contains("do not report it again"));
    }

    #[test]
    fn prompt_with_no_prior_context_says_so() {
        let rendered = render(&context()).expect("renders");

        assert!(rendered.contains("has not been reviewed before"));
        assert!(!rendered.contains("Do not report these"));
    }

    /// §9.2: "The rendered prompt is stored alongside the transcript for
    /// reproducibility." A finding nobody can explain is usually a prompt nobody kept.
    #[test]
    fn prompt_is_persisted_beside_the_transcript() {
        let dir = tempfile::tempdir().expect("tempdir");

        let returned = render_to(&context(), dir.path()).expect("writes");
        let on_disk = std::fs::read_to_string(dir.path().join(PROMPT_FILE)).expect("read back");

        assert_eq!(returned, on_disk);
        assert!(on_disk.contains("## 1. Role and output contract"));
    }

    #[test]
    fn prompt_write_failure_names_the_path() {
        let err = render_to(&context(), Path::new("/nonexistent/revlocal")).expect_err("fails");

        assert!(err.to_string().contains("/nonexistent/revlocal"));
    }

    #[test]
    fn prompt_metadata_carries_the_change_identity() {
        let rendered = render(&context()).expect("renders");

        assert!(rendered.contains("`abc123`"));
        assert!(rendered.contains("Fix the pager"));
        assert!(rendered.contains("A Developer"));
        assert!(rendered.contains("2 (+10 / -3)"));
    }

    /// Optional metadata is optional: a change with no branch or author must render,
    /// not emit "None".
    #[test]
    fn prompt_omits_absent_optional_metadata() {
        let mut c = change();
        c.branch = None;
        c.author_name = None;
        c.title = None;

        let ctx = build_context(
            "r",
            "git",
            &c,
            &RepoConfig::default(),
            "",
            false,
            &[],
            Vec::new(),
            &[],
            &[],
        );
        let rendered = render(&ctx).expect("renders");

        assert!(!rendered.contains("Branch:"));
        assert!(!rendered.contains("Author:"));
        assert!(!rendered.contains("None"));
    }
}
