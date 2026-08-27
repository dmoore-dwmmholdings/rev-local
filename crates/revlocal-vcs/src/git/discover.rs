//! Git commit discovery (SPEC §6.2).
//!
//! Discovery is a **pure read**. It reports what it found and what cursor value
//! would correspond to each change; it never advances a cursor itself. That is what
//! makes cursor advancement crash-safe: the caller records the change durably and
//! only then stores the cursor, so a crash between the two re-discovers the same
//! change rather than skipping it. Re-discovery is harmless because the store
//! upserts by `(repo_id, kind, external_id)` — the same commit lands on the same
//! row (`RL-109b`).
//!
//! Losing a change is unrecoverable; seeing one twice costs nothing. The design
//! picks the second failure deliberately.

use std::collections::BTreeSet;
use std::path::Path;

use revlocal_core::{ChangeKind, DiffStat};

use super::cmd::{GitError, GitRunner};
use crate::adapter::DetectedChange;

/// Record separator between commits in the `git log` stream.
///
/// ASCII 0x1e, and fields within a record are separated by NUL. Neither can occur
/// in a commit subject or an author name, which `\n` and `|` both can — a subject
/// containing either would silently split one commit into two records.
const RECORD_SEPARATOR: char = '\u{1e}';
const FIELD_SEPARATOR: char = '\0';

/// The `--format` string matching [`parse_log`].
///
/// Uses git's own `%xNN` escapes rather than literal control bytes: an argv string
/// cannot contain a NUL on Unix, so writing the separator directly fails at spawn
/// time with "nul byte found in provided data". Git expands these in its *output*,
/// which is where they are needed.
const LOG_FORMAT: &str = "%x1e%H%x00%an%x00%ae%x00%aI%x00%P%x00%s";

/// One commit as `git log` reported it, before it becomes a [`DetectedChange`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawCommit {
    sha: String,
    author_name: String,
    author_email: String,
    authored_at: String,
    parents: Vec<String>,
    subject: String,
    stat: DiffStat,
    paths: Vec<String>,
}

/// Resolve a repository's watched-branch patterns against its actual refs.
///
/// Patterns may be exact names or globs (`release/*`, SPEC §13.2's default). A
/// pattern matching nothing is **not** an error: a repo that has not created its
/// `release/*` branches yet is normal, and failing discovery over it would stop
/// reviewing `main` too.
pub async fn resolve_branches(
    runner: &GitRunner,
    dir: &Path,
    patterns: &[String],
) -> Result<Vec<String>, GitError> {
    let output = runner
        .run(
            dir,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        )
        .await?;

    let existing: Vec<&str> = output.lines();

    // BTreeSet so overlapping patterns cannot list a branch twice, and the order is
    // stable regardless of how the patterns were written.
    let mut matched: BTreeSet<String> = BTreeSet::new();
    for pattern in patterns {
        for branch in &existing {
            if glob_matches(pattern, branch) {
                matched.insert((*branch).to_owned());
            }
        }
    }

    Ok(matched.into_iter().collect())
}

/// Match a branch name against one pattern.
///
/// Deliberately small: git's own refspec globbing allows one `*`, and that is what
/// `release/*` needs. A full glob engine here would accept patterns that git itself
/// would not, and the difference would only show up as a branch someone expected to
/// be watched and was not.
fn glob_matches(pattern: &str, branch: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == branch,
        Some((prefix, suffix)) => {
            branch.len() >= prefix.len() + suffix.len()
                && branch.starts_with(prefix)
                && branch.ends_with(suffix)
        }
    }
}

/// Commits on `branch` newer than `cursor`, oldest first.
///
/// `--first-parent` because a merge is reviewed as one change against its first
/// parent (SPEC §6.2); without it, every commit merged in would be re-reported the
/// moment the merge landed on a watched branch.
///
/// `--reverse` because reviews publish in the order changes happened. Newest-first
/// would put a fix's review above the bug's.
///
/// # Why this is two git calls
///
/// `--max-count` is applied **during traversal, before `--reverse`**. So
/// `--reverse --max-count=N` yields the *newest* N commits, not the oldest — and a
/// discovery that returned the newest N would review those, advance the cursor past
/// them, and never come back for the older ones. Changes would be silently lost,
/// which is the one failure this layer must not have.
///
/// So the shas are listed first (cheap: `rev-list` emits ~41 bytes per commit), the
/// oldest `limit` are selected, and metadata is fetched for exactly that range. The
/// first-parent path is linear, so the selected commits are always contiguous and
/// `cursor..last` names precisely them.
pub async fn discover_branch(
    runner: &GitRunner,
    dir: &Path,
    branch: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Vec<DetectedChange>, GitError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let range = match cursor {
        Some(sha) => format!("{sha}..{branch}"),
        // No cursor means the branch has never been looked at, so everything on it
        // is new. `RL-303b` bounds a first pass so a decade-old repository does not
        // enqueue its whole history at once.
        None => branch.to_owned(),
    };

    let listed = runner
        .run(dir, &["rev-list", "--reverse", "--first-parent", &range])
        .await?;
    let shas: Vec<&str> = listed.lines();

    let selected = &shas[..shas.len().min(limit)];
    let Some(last) = selected.last() else {
        return Ok(Vec::new());
    };

    // Exactly the selected commits: linear on the first-parent path, so the range
    // from the cursor (or from the root) up to the last selected commit is them.
    let metadata_range = match cursor {
        Some(sha) => format!("{sha}..{last}"),
        None => (*last).to_owned(),
    };

    let format_arg = format!("--format={LOG_FORMAT}");
    let output = runner
        .run(
            dir,
            &[
                "log",
                "--reverse",
                "--first-parent",
                "--numstat",
                &format_arg,
                &metadata_range,
            ],
        )
        .await?;

    Ok(parse_log(&output.stdout)
        .into_iter()
        .map(|commit| to_detected(commit, branch))
        .collect())
}

/// Parse the `git log --numstat` stream into commits.
fn parse_log(stdout: &str) -> Vec<RawCommit> {
    let mut commits = Vec::new();

    for record in stdout.split(RECORD_SEPARATOR) {
        if record.trim().is_empty() {
            continue;
        }

        // The header line is the formatted fields; everything after is numstat.
        let (header, numstat) = record.split_once('\n').unwrap_or((record, ""));
        let fields: Vec<&str> = header.split(FIELD_SEPARATOR).collect();
        if fields.len() < 6 {
            continue;
        }

        commits.push(RawCommit {
            sha: fields[0].trim().to_owned(),
            author_name: fields[1].to_owned(),
            author_email: fields[2].to_owned(),
            authored_at: fields[3].to_owned(),
            parents: fields[4].split_whitespace().map(str::to_owned).collect(),
            subject: fields[5].trim_end().to_owned(),
            stat: parse_numstat(numstat),
            paths: parse_numstat_paths(numstat),
        });
    }

    commits
}

/// Sum a `--numstat` block into a [`DiffStat`].
///
/// A binary file's counts are reported as `-`, which is not zero: it means "not
/// countable". Counting it as zero would understate a change that is entirely
/// binary as an empty one, and depth selection (§9.3) would call it trivial. The
/// file still counts toward `files`.
fn parse_numstat(block: &str) -> DiffStat {
    let mut stat = DiffStat::default();

    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(added), Some(removed)) = (parts.next(), parts.next()) else {
            continue;
        };
        stat.files += 1;
        stat.insertions += added.parse::<u64>().unwrap_or(0);
        stat.deletions += removed.parse::<u64>().unwrap_or(0);
    }

    stat
}

/// The repository-relative paths a `--numstat` block names.
///
/// A rename is reported as `old => new` (or with a brace form); the **new** path is
/// what a glob should be matched against, since that is where the file is now.
fn parse_numstat_paths(block: &str) -> Vec<String> {
    block
        .lines()
        .filter_map(|line| line.trim().split('\t').nth(2))
        .map(|path| match path.rsplit_once(" => ") {
            Some((_, new)) => new.trim_end_matches('}').to_owned(),
            None => path.to_owned(),
        })
        .collect()
}

/// Turn a parsed commit into a [`DetectedChange`].
///
/// `skip_reason` is left unset here. Deciding what not to review is `RL-305`'s
/// job, and discovery reporting a commit it will not review is exactly right —
/// SPEC §18 wants the skip recorded with its reason, not the change omitted.
fn to_detected(commit: RawCommit, branch: &str) -> DetectedChange {
    let authored_at = chrono::DateTime::parse_from_rfc3339(&commit.authored_at)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc));

    DetectedChange {
        kind: ChangeKind::Commit,
        external_id: commit.sha.clone(),
        title: Some(commit.subject),
        author_name: Some(commit.author_name),
        author_email: Some(commit.author_email),
        authored_at,
        branch: Some(branch.to_owned()),
        base_ref: commit.parents.first().cloned(),
        parents: commit.parents,
        paths: commit.paths,
        head_ref: Some(commit.sha.clone()),
        url: None,
        diff_stat: commit.stat,
        skip_reason: None,
        // The cursor for a git branch is the last reviewed SHA (SPEC §6.2).
        cursor_value: commit.sha,
    }
}

/// Merge per-branch discoveries into one oldest-first list, without duplicates.
///
/// A commit reachable from two watched branches is **one change**, not two. The
/// store's `UNIQUE (repo_id, kind, external_id)` would catch a duplicate, but only
/// after a second review had been queued and paid for.
///
/// Input order is preserved within a branch and branches are consumed in order, so
/// the result stays oldest-first for each branch's own history.
pub fn merge_discoveries(per_branch: Vec<Vec<DetectedChange>>) -> Vec<DetectedChange> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut merged = Vec::new();

    for branch in per_branch {
        for change in branch {
            if seen.insert(change.external_id.clone()) {
                merged.push(change);
            }
        }
    }

    merged
}
