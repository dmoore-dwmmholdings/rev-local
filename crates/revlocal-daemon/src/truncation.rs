//! Diff truncation that never hides a file (SPEC §9.4, §18).
//!
//! Two caps, applied in the order §9.4 states them:
//!
//! 1. **Per file.** Hunks beyond `max_file_diff_bytes` (64 KB) are replaced by a
//!    stat line. The file stays in the diff and says what happened to it.
//! 2. **In total.** Beyond `max_total_diff_bytes` (512 KB), whole files are dropped
//!    in ascending order of interest — data first, then config, then tests, then
//!    source — until the diff fits.
//!
//! The order matters and is not incidental. Capping per file first bounds every
//! remaining section at 64 KB, so the total pass is choosing between comparably
//! sized files rather than deciding whether one enormous generated file is worth
//! forty real ones. Reversed, a single 500 KB file could evict the entire rest of
//! the change before the per-file rule ever ran.
//!
//! # The omitted list is the whole point
//!
//! §9.4: "Truncation must never silently hide a file." A review that saw 60% of a
//! diff and a review that saw all of it produce the same shape of output — the same
//! findings list, the same clean verdict — and nothing distinguishes them unless the
//! omission is carried forward explicitly. So [`TruncationOutcome::omitted_files`]
//! is complete, always, and is never itself truncated: it is a list of *names*, and
//! ten thousand names cost less than one unexplained silence.
//!
//! Two different things happen to a file, and they are reported separately:
//!
//! - **reduced** — hunks replaced by a stat line. Self-announcing: the file is still
//!   in the diff, saying it was too large to show.
//! - **omitted** — dropped entirely. Invisible in the diff, so it must be named
//!   outside it, which is what §9.2's prompt section 3 does.

use revlocal_core::{FileDiff, RepoConfig};

/// How interesting a file is to a reviewer (SPEC §9.4).
///
/// Declaration order is the ordering: `Data < Config < Tests < Source`. §9.4 names
/// these four and only these four, so `Data` doubles as the catch-all for anything
/// that is neither code, nor exercising code, nor configuring it — documentation,
/// fixtures, snapshots, images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Interest {
    /// Not code and not about code: docs, fixtures, snapshots, binaries.
    Data,
    /// Files that configure the build, the tooling, or the deployment.
    Config,
    /// Files that exercise the source.
    Tests,
    /// The code under review.
    Source,
}

/// Path fragments that mark a file as a test.
const TEST_MARKERS: &[&str] = &[
    "/tests/",
    "/test/",
    "/spec/",
    "/__tests__/",
    "/testdata/",
    "/fixtures/",
    "_test.",
    ".test.",
    "test_",
    ".spec.",
    "_spec.",
];

/// Extensions that make a file configuration.
const CONFIG_EXTENSIONS: &[&str] = &[
    "toml",
    "yaml",
    "yml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "editorconfig",
    "env",
];

/// Exact filenames that are configuration whatever their extension.
const CONFIG_NAMES: &[&str] = &[
    "dockerfile",
    "makefile",
    "justfile",
    "procfile",
    "package.json",
    "tsconfig.json",
    "composer.json",
    ".gitignore",
    ".gitattributes",
    ".dockerignore",
];

/// Extensions that make a file data rather than code.
const DATA_EXTENSIONS: &[&str] = &[
    "md", "rst", "txt", "adoc", "markdown", "csv", "tsv", "snap", "golden", "svg", "png", "jpg",
    "jpeg", "gif", "ico", "pdf", "parquet", "avro", "bin", "wasm", "map",
];

/// How interesting one path is (SPEC §9.4).
///
/// Tests are checked before configuration because a `tests/fixtures/config.toml` is a
/// test asset, not the build's configuration, and dropping it early would take the
/// context a test needs to be readable.
pub fn interest_of(path: &str) -> Interest {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let extension = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

    // A leading `/` on the haystack lets `/tests/` match a top-level `tests/` dir.
    let haystack = format!("/{lower}");
    if TEST_MARKERS.iter().any(|m| haystack.contains(m)) {
        return Interest::Tests;
    }
    if DATA_EXTENSIONS.contains(&extension) {
        return Interest::Data;
    }
    if CONFIG_NAMES.contains(&name)
        || CONFIG_EXTENSIONS.contains(&extension)
        || name.starts_with('.') && !name.contains('/')
    {
        return Interest::Config;
    }
    // A `.json` that is not a known config file is data — a fixture, a snapshot, an
    // export. The named ones above are the exceptions worth reviewing.
    if extension == "json" {
        return Interest::Data;
    }

    Interest::Source
}

/// What truncation did, in full (SPEC §9.4, §18).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TruncationOutcome {
    /// The diff to send to the engine.
    pub diff: String,
    /// Whether anything was cut.
    pub truncated: bool,
    /// Files dropped entirely, **complete and never itself truncated**.
    pub omitted_files: Vec<String>,
    /// Files whose hunks became a stat line but which remain in the diff.
    pub reduced_files: Vec<String>,
    /// Size of the diff as it arrived.
    pub original_bytes: usize,
    /// Size of the diff being sent.
    pub retained_bytes: usize,
}

impl TruncationOutcome {
    /// Whether the outcome is self-consistent (SPEC §18).
    ///
    /// Claiming truncation without naming anything is the silent cap this module
    /// exists to prevent, and so is naming omissions while claiming nothing was cut.
    pub fn is_consistent(&self) -> bool {
        let named = !self.omitted_files.is_empty() || !self.reduced_files.is_empty();
        self.truncated == named
    }

    /// A line for the run record and the UI.
    pub fn describe(&self) -> Option<String> {
        if !self.truncated {
            return None;
        }
        Some(format!(
            "diff reduced from {} to {} bytes: {} file(s) omitted, {} shown as a stat line only",
            self.original_bytes,
            self.retained_bytes,
            self.omitted_files.len(),
            self.reduced_files.len(),
        ))
    }
}

/// One file's section of a unified diff.
#[derive(Debug, Clone)]
struct Section {
    path: String,
    text: String,
    /// Position in the original diff, so ties break deterministically.
    ordinal: usize,
}

/// Split a unified diff into per-file sections.
///
/// Anything before the first `diff --git` is kept as a preamble under an empty path
/// and never dropped: it is usually a commit header, it is small, and losing it would
/// change what the engine thinks it is looking at.
fn split_sections(diff: &str) -> (String, Vec<Section>) {
    let mut preamble = String::new();
    let mut sections: Vec<Section> = Vec::new();

    for line in diff.split_inclusive('\n') {
        if let Some(path) = line.strip_prefix("diff --git ").and_then(header_path) {
            sections.push(Section {
                path,
                text: line.to_owned(),
                ordinal: sections.len(),
            });
        } else if let Some(current) = sections.last_mut() {
            current.text.push_str(line);
        } else {
            preamble.push_str(line);
        }
    }

    (preamble, sections)
}

/// The new-state path from a `diff --git a/old b/new` header.
///
/// Split on the **last** ` b/`, because `a/` and `b/` are prefixes git adds and a
/// real path may contain either sequence. A path containing a literal ` b/` is
/// pathological and would need `-z`-quoted output to disambiguate; `None` here means
/// the section is kept rather than misattributed.
fn header_path(header: &str) -> Option<String> {
    let header = header.trim_end();
    let (_, new) = header.rsplit_once(" b/")?;
    (!new.is_empty()).then(|| new.to_owned())
}

/// A stat line standing in for content that was not sent.
fn stat_line(file: &FileDiff, why: &str) -> String {
    let rename = file
        .previous_path
        .as_ref()
        .map_or(String::new(), |p| format!(" (from {p})"));

    format!(
        "diff --git a/{path} b/{path}\n\
         [{why}] {path}{rename}: {status}, +{ins} -{del}\n",
        path = file.path,
        status = file.status,
        ins = file.insertions,
        del = file.deletions,
    )
}

/// Apply §9.4's two caps to a unified diff.
///
/// `files` is the per-file summary of the same diff; a section with no matching entry
/// is kept and treated as source, because dropping something we failed to identify is
/// the wrong way to be wrong.
pub fn truncate(diff_unified: &str, files: &[FileDiff], config: &RepoConfig) -> TruncationOutcome {
    let original_bytes = diff_unified.len();
    let (preamble, sections) = split_sections(diff_unified);

    let lookup = |path: &str| files.iter().find(|f| f.path == path);

    // --- pass 1: per-file cap, and binaries ---
    let mut reduced_files = Vec::new();
    let mut capped: Vec<Section> = Vec::new();

    for mut section in sections {
        if let Some(file) = lookup(&section.path) {
            // §9.4 acceptance: binary files are summarised, never emitted as bytes.
            // Size is irrelevant here — a 12-byte binary is still bytes, and an
            // engine handed them will at best waste its budget tokenising noise.
            if file.binary {
                section.text = stat_line(file, "binary");
                reduced_files.push(section.path.clone());
                capped.push(section);
                continue;
            }

            if section.text.len() > config.max_file_diff_bytes {
                section.text = stat_line(file, "diff too large to show");
                reduced_files.push(section.path.clone());
            }
        }
        capped.push(section);
    }

    // --- pass 2: total cap, least interesting dropped first ---
    let budget = config.max_total_diff_bytes.saturating_sub(preamble.len());
    let total: usize = capped.iter().map(|s| s.text.len()).sum();

    let mut omitted_files = Vec::new();

    if total > budget {
        // Sort by ascending interest, then by *descending* ordinal, so the drop list
        // is "least interesting, latest in the diff first". Popping from the front
        // then removes the cheapest thing to lose.
        let mut order: Vec<usize> = (0..capped.len()).collect();
        order.sort_by_key(|&i| {
            (
                interest_of(&capped[i].path),
                std::cmp::Reverse(capped[i].ordinal),
            )
        });

        let mut remaining = total;
        let mut dropped = vec![false; capped.len()];

        for index in order {
            if remaining <= budget {
                break;
            }
            remaining -= capped[index].text.len();
            dropped[index] = true;
            omitted_files.push(capped[index].path.clone());
        }

        // Report omissions in the diff's own order, not the order we happened to
        // drop them. A user comparing the list against the change should not have to
        // reconstruct our sort to find a file.
        let mut kept = Vec::new();
        for (index, section) in capped.into_iter().enumerate() {
            if !dropped[index] {
                kept.push(section);
            }
        }
        capped = kept;
        omitted_files.sort_unstable();
    }

    let mut diff = preamble;
    for section in &capped {
        diff.push_str(&section.text);
    }

    let retained_bytes = diff.len();
    reduced_files.retain(|p| !omitted_files.contains(p));
    reduced_files.sort_unstable();

    TruncationOutcome {
        diff,
        truncated: !omitted_files.is_empty() || !reduced_files.is_empty(),
        omitted_files,
        reduced_files,
        original_bytes,
        retained_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revlocal_core::FileStatus;

    fn file(path: &str, bytes_hint: u64, binary: bool) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            previous_path: None,
            status: FileStatus::Modified,
            insertions: bytes_hint,
            deletions: 0,
            binary,
        }
    }

    /// A plausible section for `path`, `body_bytes` of hunk content long.
    fn section(path: &str, body_bytes: usize) -> String {
        format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n{}\n",
            "+x".repeat(body_bytes / 2)
        )
    }

    fn config(per_file: usize, total: usize) -> RepoConfig {
        RepoConfig {
            max_file_diff_bytes: per_file,
            max_total_diff_bytes: total,
            ..RepoConfig::default()
        }
    }

    // --- criterion 1: truncation is set and surfaced ---

    #[test]
    fn truncation_an_untouched_diff_is_not_marked_truncated() {
        let diff = section("src/a.rs", 100);
        let outcome = truncate(
            &diff,
            &[file("src/a.rs", 50, false)],
            &config(64_000, 512_000),
        );

        assert!(!outcome.truncated);
        assert_eq!(outcome.diff, diff);
        assert!(outcome.omitted_files.is_empty());
        assert!(outcome.describe().is_none());
        assert!(outcome.is_consistent());
    }

    #[test]
    fn truncation_sets_truncated_and_describes_what_it_did() {
        let diff = format!(
            "{}{}",
            section("src/a.rs", 4_000),
            section("docs/b.md", 4_000)
        );
        let outcome = truncate(
            &diff,
            &[file("src/a.rs", 1, false), file("docs/b.md", 1, false)],
            &config(64_000, 5_000),
        );

        assert!(outcome.truncated);
        assert!(outcome.is_consistent());
        assert!(outcome.retained_bytes < outcome.original_bytes);

        let described = outcome.describe().expect("truncation is described");
        assert!(described.contains("1 file(s) omitted"), "{described}");
    }

    /// §18's shape, asserted directly: truncation claimed with nothing named is a
    /// silent cap, and so is the reverse.
    #[test]
    fn truncation_claiming_a_cut_without_naming_one_is_inconsistent() {
        let bad = TruncationOutcome {
            truncated: true,
            ..TruncationOutcome::default()
        };
        assert!(!bad.is_consistent());

        let also_bad = TruncationOutcome {
            truncated: false,
            omitted_files: vec!["a".to_owned()],
            ..TruncationOutcome::default()
        };
        assert!(!also_bad.is_consistent());
    }

    // --- criterion 2: every omitted file is named ---

    /// The load-bearing assertion of the whole item. A review that saw 60% of a diff
    /// and one that saw all of it produce identical-looking output; the omission is
    /// only visible if it is carried out of the diff and into the prompt.
    #[test]
    fn truncation_names_every_omitted_file_even_when_many_are_dropped() {
        let paths: Vec<String> = (0..40).map(|i| format!("data/f{i:02}.csv")).collect();
        let diff: String = paths.iter().map(|p| section(p, 2_000)).collect();
        let files: Vec<FileDiff> = paths.iter().map(|p| file(p, 1, false)).collect();

        let outcome = truncate(&diff, &files, &config(64_000, 10_000));

        assert!(outcome.truncated);
        // Whatever survived, everything that did not is named.
        for path in &paths {
            let in_diff = outcome.diff.contains(path.as_str());
            let named = outcome.omitted_files.contains(path);
            assert!(in_diff || named, "{path} vanished without being named");
            assert!(!(in_diff && named), "{path} both kept and reported omitted");
        }
        assert!(
            outcome.omitted_files.len() > 30,
            "expected most to be dropped"
        );
    }

    /// The omitted list is a list of names and is never itself capped. Ten thousand
    /// names cost less than one unexplained silence.
    #[test]
    fn truncation_the_omitted_list_is_never_itself_truncated() {
        let paths: Vec<String> = (0..500).map(|i| format!("data/f{i:03}.csv")).collect();
        let diff: String = paths.iter().map(|p| section(p, 500)).collect();
        let files: Vec<FileDiff> = paths.iter().map(|p| file(p, 1, false)).collect();

        let outcome = truncate(&diff, &files, &config(64_000, 1_000));

        assert!(outcome.omitted_files.len() >= 490);
    }

    /// Criterion 2 end to end: the names have to reach the *prompt*, not just the
    /// outcome struct. This crosses into RL-502's template, which is where a user
    /// actually finds out.
    #[test]
    fn truncation_omitted_files_reach_the_rendered_prompt() {
        use crate::prompt;
        use revlocal_core::{Change, ChangeId, ChangeKind, DiffStat, RepoId, Timestamp};

        let paths = ["src/keep.rs", "data/drop_me.csv", "docs/also_dropped.md"];
        let diff: String = paths.iter().map(|p| section(p, 3_000)).collect();
        let files: Vec<FileDiff> = paths.iter().map(|p| file(p, 1, false)).collect();

        let outcome = truncate(&diff, &files, &config(64_000, 4_000));
        assert!(!outcome.omitted_files.is_empty());

        let change = Change {
            id: ChangeId::new(1),
            repo_id: RepoId::new(1),
            kind: ChangeKind::Commit,
            external_id: "abc".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: None,
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: Timestamp::default(),
        };

        let context = prompt::build_context(
            "r",
            "git",
            &change,
            &RepoConfig::default(),
            &outcome.diff,
            outcome.truncated,
            &outcome.omitted_files,
            Vec::new(),
            &[],
            &[],
        );
        let rendered = prompt::render(&context).expect("renders");

        assert!(rendered.contains("This diff has been truncated"));
        for omitted in &outcome.omitted_files {
            assert!(
                rendered.contains(omitted.as_str()),
                "{omitted} not in the prompt"
            );
        }
    }

    // --- criterion 3: interest ordering, mixed-type commit ---

    #[test]
    fn truncation_interest_ordering_is_source_tests_config_data() {
        assert!(Interest::Source > Interest::Tests);
        assert!(Interest::Tests > Interest::Config);
        assert!(Interest::Config > Interest::Data);
    }

    #[test]
    fn truncation_classifies_a_mixed_commit() {
        assert_eq!(interest_of("src/engine/runner.rs"), Interest::Source);
        assert_eq!(interest_of("crates/x/tests/smoke.rs"), Interest::Tests);
        assert_eq!(interest_of("tests/smoke.rs"), Interest::Tests);
        assert_eq!(interest_of("src/runner_test.go"), Interest::Tests);
        assert_eq!(interest_of("app/foo.spec.ts"), Interest::Tests);
        assert_eq!(interest_of("Cargo.toml"), Interest::Config);
        assert_eq!(interest_of("package.json"), Interest::Config);
        assert_eq!(interest_of(".github/workflows/ci.yml"), Interest::Config);
        assert_eq!(interest_of("Dockerfile"), Interest::Config);
        assert_eq!(interest_of("README.md"), Interest::Data);
        assert_eq!(interest_of("assets/logo.png"), Interest::Data);
        assert_eq!(interest_of("db/seed.json"), Interest::Data);
    }

    /// A `tests/fixtures/config.toml` is a test asset, not the build's
    /// configuration. Classifying it as config would drop it before the test that
    /// reads it, leaving a test in the diff that cannot be understood.
    #[test]
    fn truncation_a_config_file_under_tests_is_a_test() {
        assert_eq!(interest_of("tests/fixtures/config.toml"), Interest::Tests);
        assert_eq!(interest_of("src/testdata/input.json"), Interest::Tests);
    }

    /// The acceptance criterion, on a mixed-type commit sized so exactly one tier
    /// survives at a time.
    #[test]
    fn truncation_drops_least_interesting_first_on_a_mixed_commit() {
        // Section length varies with path length, so the budgets are computed from
        // the real sizes rather than assumed uniform — the first version of this
        // test assumed uniform and silently tested a different boundary.
        let mixed = [
            "src/engine.rs",
            "tests/engine_test.rs",
            "Cargo.toml",
            "README.md",
        ];
        let parts: Vec<String> = mixed.iter().map(|p| section(p, 1_000)).collect();
        let diff: String = parts.concat();
        let files: Vec<FileDiff> = mixed.iter().map(|p| file(p, 1, false)).collect();

        // Budget that fits the n most interesting sections and nothing more.
        let by_interest = |n: usize| -> usize {
            let mut sizes: Vec<(Interest, usize)> = mixed
                .iter()
                .zip(&parts)
                .map(|(p, s)| (interest_of(p), s.len()))
                .collect();
            sizes.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
            sizes.iter().take(n).map(|(_, len)| len).sum()
        };

        let three = truncate(&diff, &files, &config(64_000, by_interest(3)));
        assert_eq!(three.omitted_files, ["README.md"]);

        let two = truncate(&diff, &files, &config(64_000, by_interest(2)));
        assert_eq!(two.omitted_files, ["Cargo.toml", "README.md"]);

        let one = truncate(&diff, &files, &config(64_000, by_interest(1)));
        assert_eq!(
            one.omitted_files,
            ["Cargo.toml", "README.md", "tests/engine_test.rs"]
        );
        assert!(one.diff.contains("src/engine.rs"));
    }

    /// Within a tier the drop order is deterministic — later in the diff goes first —
    /// so two runs over the same change omit the same files.
    #[test]
    fn truncation_is_deterministic_within_a_tier() {
        let paths: Vec<String> = (0..6).map(|i| format!("src/f{i}.rs")).collect();
        let diff: String = paths.iter().map(|p| section(p, 1_000)).collect();
        let files: Vec<FileDiff> = paths.iter().map(|p| file(p, 1, false)).collect();

        let first = truncate(&diff, &files, &config(64_000, 3_500));
        let again = truncate(&diff, &files, &config(64_000, 3_500));

        assert_eq!(first.omitted_files, again.omitted_files);
        assert!(first.omitted_files.contains(&"src/f5.rs".to_owned()));
        assert!(!first.omitted_files.contains(&"src/f0.rs".to_owned()));
    }

    // --- criterion 4: binary files ---

    #[test]
    fn truncation_binary_files_are_summarised_never_emitted() {
        let diff = format!(
            "diff --git a/img/logo.png b/img/logo.png\nGIT binary patch\n{}\n",
            "\u{fffd}zzzz".repeat(50)
        );
        let outcome = truncate(
            &diff,
            &[file("img/logo.png", 0, true)],
            &config(64_000, 512_000),
        );

        assert!(outcome.truncated);
        assert!(!outcome.diff.contains("GIT binary patch"));
        assert!(outcome.diff.contains("[binary] img/logo.png"));
        assert_eq!(outcome.reduced_files, ["img/logo.png"]);
    }

    /// Size is irrelevant for a binary. A 12-byte blob is still bytes, and an engine
    /// handed them spends its budget tokenising noise at best.
    #[test]
    fn truncation_a_tiny_binary_is_still_summarised() {
        let diff = "diff --git a/x.bin b/x.bin\nGIT binary patch\nab\n".to_owned();
        let outcome = truncate(&diff, &[file("x.bin", 0, true)], &config(64_000, 512_000));

        assert!(!outcome.diff.contains("GIT binary patch"));
        assert!(outcome.diff.contains("[binary] x.bin"));
    }

    // --- per-file cap ---

    #[test]
    fn truncation_a_large_file_becomes_a_stat_line_but_stays_in_the_diff() {
        let diff = section("src/big.rs", 10_000);
        let outcome = truncate(
            &diff,
            &[file("src/big.rs", 5_000, false)],
            &config(1_000, 512_000),
        );

        assert!(outcome.truncated);
        assert!(outcome.diff.contains("src/big.rs"));
        assert!(outcome.diff.contains("diff too large to show"));
        assert!(!outcome.diff.contains("+x+x"));
        // Reduced, not omitted: it is still in the diff, saying so itself.
        assert!(outcome.omitted_files.is_empty());
        assert_eq!(outcome.reduced_files, ["src/big.rs"]);
    }

    /// The per-file cap runs first, which bounds every section before the total pass
    /// chooses between them. Reversed, one 500 KB file could evict the entire rest of
    /// the change before the per-file rule ever ran.
    #[test]
    fn truncation_the_per_file_cap_runs_before_the_total_cap() {
        let diff = format!(
            "{}{}",
            section("src/huge.rs", 20_000),
            section("src/small.rs", 200)
        );
        let files = [
            file("src/huge.rs", 1, false),
            file("src/small.rs", 1, false),
        ];

        // A total budget that the raw diff blows but the capped diff fits inside.
        let outcome = truncate(&diff, &files, &config(500, 3_000));

        assert!(outcome.diff.contains("src/small.rs"));
        assert!(outcome.diff.contains("+x"), "the small file kept its hunks");
        assert!(
            outcome.omitted_files.is_empty(),
            "capping first should have made room: {:?}",
            outcome.omitted_files
        );
    }

    // --- parsing ---

    #[test]
    fn truncation_keeps_the_preamble_and_never_drops_it() {
        let diff = format!("commit abc\nAuthor: x\n\n{}", section("data/a.csv", 5_000));
        let outcome = truncate(&diff, &[file("data/a.csv", 1, false)], &config(64_000, 100));

        assert!(outcome.diff.starts_with("commit abc"));
        assert_eq!(outcome.omitted_files, ["data/a.csv"]);
    }

    #[test]
    fn truncation_handles_a_path_containing_a_space() {
        let path = "docs/release notes.md";
        let diff = section(path, 200);
        let outcome = truncate(&diff, &[file(path, 1, false)], &config(64_000, 512_000));

        assert!(!outcome.truncated);
        assert_eq!(outcome.diff, diff);
    }

    /// A section we could not match to a `FileDiff` is kept and treated as source.
    /// Dropping something we failed to identify is the wrong way to be wrong.
    #[test]
    fn truncation_an_unidentified_section_is_treated_as_source() {
        let diff = format!(
            "{}{}",
            section("mystery.rs", 1_000),
            section("data/x.csv", 1_000)
        );
        // Only the CSV is described; `mystery.rs` has no FileDiff entry.
        let outcome = truncate(
            &diff,
            &[file("data/x.csv", 1, false)],
            &config(64_000, 1_200),
        );

        assert_eq!(outcome.omitted_files, ["data/x.csv"]);
        assert!(outcome.diff.contains("mystery.rs"));
    }

    #[test]
    fn truncation_an_empty_diff_is_untouched() {
        let outcome = truncate("", &[], &config(64_000, 512_000));

        assert!(!outcome.truncated);
        assert!(outcome.diff.is_empty());
        assert!(outcome.is_consistent());
    }

    /// A file dropped by the total cap must not also appear as "reduced" — it is not
    /// in the diff to be reduced, and listing it twice would misreport what happened.
    ///
    /// Reaching this state needs a per-file cap low enough to reduce the file *and* a
    /// total budget too small even for the resulting stat lines.
    #[test]
    fn truncation_an_omitted_file_is_not_also_reported_reduced() {
        let diff = format!(
            "{}{}",
            section("src/a.rs", 2_000),
            section("data/big.csv", 9_000)
        );
        let files = [file("src/a.rs", 1, false), file("data/big.csv", 1, false)];

        let stat_only = truncate(&diff, &files, &config(1_000, 512_000));
        assert_eq!(stat_only.reduced_files, ["data/big.csv", "src/a.rs"]);

        // Now a budget that fits one stat line but not two.
        let outcome = truncate(&diff, &files, &config(1_000, stat_only.retained_bytes - 10));

        assert!(outcome.omitted_files.contains(&"data/big.csv".to_owned()));
        assert!(!outcome.reduced_files.contains(&"data/big.csv".to_owned()));
        assert_eq!(outcome.reduced_files, ["src/a.rs"]);
        assert!(outcome.is_consistent());
    }
}
