//! Materialising one Subversion revision (RL-903, SPEC §6.4).
//!
//! # `export`, never a working copy
//!
//! §6.1's absolute constraint is that reviewing never mutates the repository under
//! review, and Subversion makes that easier to get wrong than git does: `svn
//! update` and `svn switch` operate on a *user's* working copy, in place. So this
//! only ever uses `svn export`, which writes a fresh unversioned tree into scratch
//! and touches nothing the user has.
//!
//! There is no SVN equivalent of `git worktree add` to get wrong, and no lock to
//! leave behind, because nothing here opens the user's `.svn` at all.
//!
//! # A change that produced no diff text still changed something
//!
//! Two cases produce an empty unified diff and mean entirely different things:
//!
//! - a **property-only** change — `svn:executable`, `svn:mergeinfo`, a custom
//!   property — which svn reports in `--summarize` as `item="none"
//!   props="modified"` and shows nothing for in the patch;
//! - a **binary** file, which svn refuses to render.
//!
//! Passing either through as an empty diff tells the engine "nothing happened
//! here", which is false, and §18 is explicit that a review which saw less must
//! not look like one that saw everything. Both are summarised instead: what
//! changed, and for a binary, its size and type.

use std::path::{Path, PathBuf};

use revlocal_core::{DiffStat, FileDiff, FileStatus};
use serde::Deserialize;

use super::cmd::{SvnError, SvnRunner};
use crate::adapter::ChangeContext;

/// Where the exported tree goes inside a scratch directory.
pub const EXPORT_SUBDIR: &str = "export";

/// The marker svn prints instead of a binary file's contents.
const BINARY_MARKER: &str = "Cannot display: file marked as a binary type";

// --- `svn diff --summarize --xml` -----------------------------------------

#[derive(Debug, Deserialize)]
struct DiffXml {
    paths: Option<SummaryPathsXml>,
}

#[derive(Debug, Deserialize)]
struct SummaryPathsXml {
    #[serde(default, rename = "path")]
    paths: Vec<SummaryPathXml>,
}

#[derive(Debug, Deserialize)]
struct SummaryPathXml {
    #[serde(rename = "@item")]
    item: String,
    #[serde(rename = "@props")]
    props: Option<String>,
    #[serde(rename = "@kind")]
    kind: Option<String>,
    #[serde(rename = "$text")]
    path: Option<String>,
}

/// One path as `svn diff --summarize` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPath {
    /// Repository-absolute path, with the repository URL stripped.
    pub path: String,
    /// `added` | `modified` | `deleted` | `none`.
    pub item: String,
    /// Whether properties changed on it.
    pub props_changed: bool,
    /// `file` or `dir`.
    pub kind: Option<String>,
}

impl ChangedPath {
    /// Whether only properties changed — no text at all.
    ///
    /// This is the case that produces an empty patch and means something.
    pub fn is_property_only(&self) -> bool {
        self.item == "none" && self.props_changed
    }

    /// The `FileStatus` this maps to.
    pub fn status(&self) -> FileStatus {
        match self.item.as_str() {
            "added" => FileStatus::Added,
            "deleted" => FileStatus::Deleted,
            // A property-only change is a modification of the file, even though
            // none of its bytes moved.
            _ => FileStatus::Modified,
        }
    }
}

/// Parse `svn diff --summarize --xml`.
///
/// `repo_url` is stripped from each path so the result is repository-relative,
/// which is what every skip rule and every glob in the config is written against.
pub fn parse_summary(xml: &str, repo_url: &str) -> Result<Vec<ChangedPath>, SvnError> {
    let parsed: DiffXml = quick_xml::de::from_str(xml).map_err(|source| SvnError::Failed {
        args: "diff --summarize --xml".to_owned(),
        code: 0,
        stderr: format!("could not read svn's XML diff summary: {source}"),
    })?;

    Ok(parsed
        .paths
        .map(|paths| {
            paths
                .paths
                .into_iter()
                .map(|path| {
                    let raw = path.path.unwrap_or_default();
                    ChangedPath {
                        path: strip_repo_url(&raw, repo_url),
                        item: path.item,
                        props_changed: path.props.as_deref().is_some_and(|p| p != "none"),
                        kind: path.kind,
                    }
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Make a path repository-relative.
fn strip_repo_url(path: &str, repo_url: &str) -> String {
    let trimmed = repo_url.trim_end_matches('/');
    path.strip_prefix(trimmed)
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_owned()
}

/// What is known about a binary file that could not be diffed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySummary {
    /// Repository-relative path.
    pub path: String,
    /// Size in bytes at this revision, where svn reported one.
    pub size_bytes: Option<u64>,
    /// The `svn:mime-type` property, where one is set.
    pub mime_type: Option<String>,
}

impl BinarySummary {
    /// The line that stands in for the diff svn would not render.
    ///
    /// §18: a reviewer reading this must be able to tell that a file changed and
    /// was not shown, rather than inferring it from an absence.
    pub fn render(&self) -> String {
        let kind = self.mime_type.as_deref().unwrap_or("unknown type");
        match self.size_bytes {
            Some(bytes) => format!(
                "Binary file {} changed ({kind}, {bytes} bytes). Contents not shown.",
                self.path
            ),
            None => format!(
                "Binary file {} changed ({kind}, size unknown). Contents not shown.",
                self.path
            ),
        }
    }
}

/// The line that stands in for a property-only change.
pub fn render_property_only(path: &str) -> String {
    format!(
        "Properties on {path} changed; the file's contents did not. \
         An empty patch here would read as `nothing happened`."
    )
}

/// Whether svn declined to render this file because it is binary.
fn diff_marks_binary(diff: &str, path: &str) -> bool {
    // svn prints `Index: <path>` then the marker within that file's section.
    let Some(start) = diff.find(&format!("Index: {path}")) else {
        return false;
    };
    let section = &diff[start..];
    let end = section[1..]
        .find("\nIndex: ")
        .map_or(section.len(), |offset| offset + 1);
    section[..end].contains(BINARY_MARKER)
}

#[derive(Debug, Deserialize)]
struct ListXml {
    list: Option<ListEntriesXml>,
}

#[derive(Debug, Deserialize)]
struct ListEntriesXml {
    #[serde(default, rename = "entry")]
    entries: Vec<ListEntryXml>,
}

#[derive(Debug, Deserialize)]
struct ListEntryXml {
    size: Option<u64>,
}

/// The size svn reports for a file at a revision.
fn parse_list_size(xml: &str) -> Option<u64> {
    let parsed: ListXml = quick_xml::de::from_str(xml).ok()?;
    parsed
        .list?
        .entries
        .into_iter()
        .find_map(|entry| entry.size)
}

/// Look up what is knowable about a binary file.
async fn summarise_binary(
    runner: &SvnRunner,
    repo_url: &str,
    revision: u64,
    path: &str,
) -> BinarySummary {
    let target = format!("{}/{path}", repo_url.trim_end_matches('/'));
    let rev = revision.to_string();

    // Both lookups are best-effort. A binary file whose size could not be read is
    // still worth reporting as a binary file that changed — failing the whole
    // materialisation because a metadata call did not answer would lose the
    // review over a detail.
    let size_bytes = runner
        .run(
            Path::new("."),
            &["list", "--xml", "-r", &rev, &format!("{target}@{rev}")],
        )
        .await
        .ok()
        .and_then(|output| parse_list_size(&output.stdout));

    let mime_type = runner
        .run(
            Path::new("."),
            &[
                "propget",
                "svn:mime-type",
                "-r",
                &rev,
                &format!("{target}@{rev}"),
            ],
        )
        .await
        .ok()
        .map(|output| output.stdout.trim().to_owned())
        .filter(|value| !value.is_empty());

    BinarySummary {
        path: path.to_owned(),
        size_bytes,
        mime_type,
    }
}

/// Export one revision and describe what it changed (§6.4).
///
/// `into` is a scratch directory the caller owns; the tree lands in its
/// [`EXPORT_SUBDIR`]. Nothing here reads or writes a user's working copy.
pub async fn materialize(
    runner: &SvnRunner,
    repo_url: &str,
    revision: u64,
    into: &Path,
) -> Result<ChangeContext, SvnError> {
    let rev = revision.to_string();
    let export = into.join(EXPORT_SUBDIR);

    std::fs::create_dir_all(into).map_err(|source| SvnError::Spawn {
        args: format!("creating {}", into.display()),
        source,
    })?;

    // `export`, not `checkout`: an export has no `.svn`, so nothing downstream can
    // accidentally commit from it, and there is no working copy to leave locked.
    runner
        .run(
            Path::new("."),
            &[
                "export",
                "--quiet",
                "--force",
                "-r",
                &rev,
                &format!("{}@{rev}", repo_url.trim_end_matches('/')),
                &export.display().to_string(),
            ],
        )
        .await?;

    let mut diff_unified = runner
        .run(Path::new("."), &["diff", "-c", &rev, repo_url])
        .await?
        .stdout;

    let summary = runner
        .run(
            Path::new("."),
            &["diff", "-c", &rev, "--summarize", "--xml", repo_url],
        )
        .await?
        .stdout;
    let changed = parse_summary(&summary, repo_url)?;

    let message = super::discover::parse_log_xml(
        &runner
            .run(Path::new("."), &["log", "--xml", "-r", &rev, repo_url])
            .await?
            .stdout,
    )?
    .first()
    .map(|entry| entry.message.clone())
    .unwrap_or_default();

    let mut notes = Vec::new();
    let mut diff_files = Vec::new();

    for path in &changed {
        let is_binary = diff_marks_binary(&diff_unified, &path.path);

        if path.is_property_only() {
            notes.push(render_property_only(&path.path));
        } else if is_binary {
            notes.push(
                summarise_binary(runner, repo_url, revision, &path.path)
                    .await
                    .render(),
            );
        }

        let (insertions, deletions) = if is_binary || path.is_property_only() {
            (0, 0)
        } else {
            count_lines(&diff_unified, &path.path)
        };

        diff_files.push(FileDiff {
            path: path.path.clone(),
            previous_path: None,
            status: path.status(),
            insertions,
            deletions,
            binary: is_binary,
        });
    }

    // Appended rather than prepended: the patch is what a reviewer reads first,
    // and the notes are about what the patch could not show.
    if !notes.is_empty() {
        if !diff_unified.is_empty() {
            diff_unified.push_str("\n\n");
        }
        diff_unified.push_str("--- changes svn could not render as a patch ---\n");
        diff_unified.push_str(&notes.join("\n"));
        diff_unified.push('\n');
    }

    let stat = diff_files
        .iter()
        .fold(DiffStat::default(), |mut acc, file| {
            acc.files += 1;
            acc.insertions += file.insertions;
            acc.deletions += file.deletions;
            acc
        });

    Ok(ChangeContext {
        worktree: export,
        diff_unified,
        diff_files,
        message,
        parents: revision
            .checked_sub(1)
            .map(|previous| vec![format!("r{previous}")])
            .unwrap_or_default(),
        stat,
        // Truncation is §9.4's decision, applied later. A freshly materialized
        // context has everything svn gave, and claiming otherwise would be a
        // silent cap in reverse.
        truncated: false,
        omitted_files: Vec::new(),
    })
}

/// Count added and removed lines for one path inside a unified diff.
fn count_lines(diff: &str, path: &str) -> (u64, u64) {
    let Some(start) = diff.find(&format!("Index: {path}")) else {
        return (0, 0);
    };
    let section = &diff[start..];
    let end = section[1..]
        .find("\nIndex: ")
        .map_or(section.len(), |offset| offset + 1);

    section[..end]
        .lines()
        .skip_while(|line| !line.starts_with("@@"))
        .fold((0, 0), |(added, removed), line| {
            if line.starts_with('+') {
                (added + 1, removed)
            } else if line.starts_with('-') {
                (added, removed + 1)
            } else {
                (added, removed)
            }
        })
}

/// Where the exported tree lands for a scratch directory.
pub fn export_path(scratch: &Path) -> PathBuf {
    scratch.join(EXPORT_SUBDIR)
}
