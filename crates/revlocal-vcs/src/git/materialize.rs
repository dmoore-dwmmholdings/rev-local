//! Materializing a change into a scratch tree (SPEC §6.1, §6.2).
//!
//! The rule this module exists to keep: **materializing must not mutate the
//! repository under review.** Not its working tree, not its index, not HEAD, not
//! the stash. M4's exit gate asserts the fixture is byte-identical afterwards, and
//! that is not a formality — rev-local watches repositories people are actively
//! working in, and a review that stashed someone's uncommitted work to check out a
//! commit would be worse than no review at all.
//!
//! Two strategies, chosen by what the repository is:
//!
//! - **A normal repository** gets `git worktree add --detach`. It creates a second
//!   checkout without touching the first one's HEAD or index. It does write
//!   metadata under `.git/worktrees/`, which is why [`release_worktree`] exists and
//!   why the acceptance criteria ask for `git worktree list` to return to its prior
//!   state.
//! - **A bare mirror** gets `git archive`, exactly as §6.1 says. `worktree add`
//!   happens to work on a bare repository, but it would write metadata into a
//!   mirror rev-local does not own. `archive` writes nothing at all, which is the
//!   stronger guarantee and the reason the spec names it.

use std::path::{Path, PathBuf};

use revlocal_core::{Change, DiffStat, FileDiff, FileStatus};

use super::cmd::{GitError, GitRunner};
use crate::adapter::ChangeContext;

/// Where a materialized tree is placed inside a scratch directory.
///
/// A subdirectory rather than the scratch root, so the run can keep other things —
/// the engine transcript, the prompt, the archive — beside the tree without them
/// appearing to the engine as repository content.
pub const WORKTREE_SUBDIR: &str = "worktree";

/// Whether `dir` is a bare repository.
pub async fn is_bare(runner: &GitRunner, dir: &Path) -> Result<bool, GitError> {
    let output = runner
        .run(dir, &["rev-parse", "--is-bare-repository"])
        .await?;
    Ok(output.stdout.trim() == "true")
}

/// Materialize `change` from `repo_dir` into `into`, read-only for the source.
///
/// `into` is expected to be a scratch directory (`RL-301`); the tree lands in
/// `into/worktree`.
pub async fn materialize(
    runner: &GitRunner,
    repo_dir: &Path,
    change: &Change,
    into: &Path,
) -> Result<ChangeContext, GitError> {
    let sha = change.external_id.clone();
    let worktree = into.join(WORKTREE_SUBDIR);

    if is_bare(runner, repo_dir).await? {
        extract_archive(runner, repo_dir, &sha, &worktree).await?;
    } else {
        std::fs::create_dir_all(into).map_err(|source| GitError::Spawn {
            args: format!("creating {}", into.display()),
            source,
        })?;
        // --detach so no branch is created or moved. --no-checkout is deliberately
        // NOT used: the engine reviews a tree, so the tree has to be there.
        runner
            .run(
                repo_dir,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "--quiet",
                    &worktree.display().to_string(),
                    &sha,
                ],
            )
            .await?;
    }

    let parents = parents_of(runner, repo_dir, &sha).await?;
    let message = runner
        .run(repo_dir, &["log", "-1", "--format=%B", &sha])
        .await?
        .stdout;

    let diff_unified = runner
        .run(
            repo_dir,
            &["show", "--format=", "--first-parent", "--patch", &sha],
        )
        .await?
        .stdout;

    let diff_files = file_diffs(runner, repo_dir, &sha).await?;
    let stat = diff_files
        .iter()
        .fold(DiffStat::default(), |mut acc, file| {
            acc.files += 1;
            acc.insertions += file.insertions;
            acc.deletions += file.deletions;
            acc
        });

    Ok(ChangeContext {
        worktree,
        diff_unified,
        diff_files,
        message: message.trim_end().to_owned(),
        parents,
        stat,
        // Truncation is RL-307's decision; a freshly materialized context has
        // everything, and claiming otherwise would be a silent cap in reverse.
        truncated: false,
        omitted_files: Vec::new(),
    })
}

/// Remove a worktree registered against `repo_dir`, restoring `git worktree list`.
///
/// Not a `Drop`: removal is an async git call through the choke point, and `Drop`
/// is neither async nor allowed to spawn git itself (`RL-302`). Callers release
/// explicitly; the scratch directory's own RAII removes the *files* either way, and
/// `prune_worktrees` mops up the metadata left by a caller that did not.
pub async fn release_worktree(
    runner: &GitRunner,
    repo_dir: &Path,
    worktree: &Path,
) -> Result<(), GitError> {
    // --force because the tree may have been written to by the engine, and a
    // worktree with modifications is refused otherwise. Nothing here is worth
    // keeping: it is a scratch copy by construction.
    runner
        .run(
            repo_dir,
            &[
                "worktree",
                "remove",
                "--force",
                &worktree.display().to_string(),
            ],
        )
        .await?;
    Ok(())
}

/// Drop worktree metadata whose directory is already gone.
///
/// The scratch directory removes itself when a run ends, including on a panic
/// (`RL-301`), which can leave `.git/worktrees/` entries pointing at nothing. This
/// is how a crashed run stops accumulating them.
pub async fn prune_worktrees(runner: &GitRunner, repo_dir: &Path) -> Result<(), GitError> {
    runner.run(repo_dir, &["worktree", "prune"]).await?;
    Ok(())
}

/// Materialize from a bare repository with `git archive` (SPEC §6.1).
async fn extract_archive(
    runner: &GitRunner,
    repo_dir: &Path,
    sha: &str,
    into: &Path,
) -> Result<(), GitError> {
    std::fs::create_dir_all(into).map_err(|source| GitError::Spawn {
        args: format!("creating {}", into.display()),
        source,
    })?;

    // Written to a file rather than piped: the archive is binary, and this crate's
    // command wrapper captures stdout as a lossy String — a tar full of replacement
    // characters would extract into corrupted files, and the corruption would look
    // like the repository's fault.
    let archive = into.parent().unwrap_or(into).join(format!("{sha}.tar"));

    runner
        .run(
            repo_dir,
            &[
                "archive",
                "--format=tar",
                "-o",
                &archive.display().to_string(),
                sha,
            ],
        )
        .await?;

    let file = std::fs::File::open(&archive).map_err(|source| GitError::Spawn {
        args: format!("opening {}", archive.display()),
        source,
    })?;
    tar::Archive::new(file)
        .unpack(into)
        .map_err(|source| GitError::Spawn {
            args: format!("extracting {}", archive.display()),
            source,
        })?;

    // The tar is an implementation detail of getting the tree out; leaving it beside
    // the worktree would put it in front of the engine as if it were content.
    let _ = std::fs::remove_file(&archive);
    Ok(())
}

/// The parents of `sha`.
async fn parents_of(
    runner: &GitRunner,
    repo_dir: &Path,
    sha: &str,
) -> Result<Vec<String>, GitError> {
    let output = runner
        .run(repo_dir, &["log", "-1", "--format=%P", sha])
        .await?;
    Ok(output
        .stdout
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// Per-file diffs for `sha`, against its first parent.
///
/// Two calls, not one: `--numstat` and `--name-status` are the same option slot in
/// git, so passing both silently keeps only the last. Combining them produced a
/// file list with no counts, and the review would have seen every change as empty.
async fn file_diffs(
    runner: &GitRunner,
    repo_dir: &Path,
    sha: &str,
) -> Result<Vec<FileDiff>, GitError> {
    let numstat = runner
        .run(
            repo_dir,
            &[
                "show",
                "--format=",
                "--first-parent",
                "--numstat",
                "--no-renames",
                sha,
            ],
        )
        .await?;
    let name_status = runner
        .run(
            repo_dir,
            &[
                "show",
                "--format=",
                "--first-parent",
                "--name-status",
                "--no-renames",
                sha,
            ],
        )
        .await?;

    let statuses: Vec<(String, FileStatus)> = name_status
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim_end().split('\t');
            let letter = fields.next()?;
            let path = fields.next()?;
            Some((path.to_owned(), status_from_letter(letter)))
        })
        .collect();

    Ok(numstat
        .stdout
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.trim_end().split('\t').collect();
            if fields.len() < 3 {
                return None;
            }
            // `-` for a count means binary — NOT zero. Counting a binary file as an
            // empty change would let depth selection (§9.3) call an all-binary
            // commit trivial.
            let binary = fields[0] == "-" || fields[1] == "-";
            let path = fields[2].to_owned();
            let status = statuses
                .iter()
                .find(|(p, _)| *p == path)
                .map_or(FileStatus::Modified, |(_, s)| *s);

            Some(FileDiff {
                insertions: fields[0].parse().unwrap_or(0),
                deletions: fields[1].parse().unwrap_or(0),
                path,
                previous_path: None,
                status,
                binary,
            })
        })
        .collect())
}

/// Map git's status letter onto [`FileStatus`].
fn status_from_letter(letter: &str) -> FileStatus {
    match letter.chars().next() {
        Some('A') => FileStatus::Added,
        Some('D') => FileStatus::Deleted,
        Some('R') => FileStatus::Renamed,
        Some('C') => FileStatus::Copied,
        Some('T') => FileStatus::TypeChanged,
        // Unknown letters mean a git version reporting something this build does
        // not know. `Modified` is the conservative reading: it keeps the file in
        // the review rather than dropping it.
        _ => FileStatus::Modified,
    }
}

/// Where a run's worktree lives inside `scratch`.
pub fn worktree_path(scratch: &Path) -> PathBuf {
    scratch.join(WORKTREE_SUBDIR)
}
