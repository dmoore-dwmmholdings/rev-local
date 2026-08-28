//! Per-revision discovery over watched paths (RL-902, SPEC §6.4).
//!
//! # `--xml`, not the human log
//!
//! `svn log` has a human format and a machine one, and the human one is a
//! newline-delimited layout with `----` separators that changes with locale and
//! with svn version. ADR 0023's rule says parse the machine format — and the
//! attribute order inside `--xml` is *itself* not stable between revisions, which
//! is why this uses a real parser rather than a regex over angle brackets.
//!
//! # A revision that touches nothing watched is still a revision
//!
//! Path filtering decides what gets **reviewed**, not what gets *seen*. The cursor
//! advances past a filtered revision, because the alternative is re-reading it on
//! every poll forever — but the filtering happens after discovery, so a revision
//! is skipped by decision rather than by never having been looked at.
//!
//! # An empty revision is ordinary
//!
//! A commit can change no paths at all: a property-only change to the repository
//! root, or a commit made with `--keep-changelists` and nothing staged. `<paths>`
//! is then absent from the XML entirely. That has to parse, because a discovery
//! pass that fails on one is a poller that stops at the first unusual commit and
//! never advances again.

use serde::Deserialize;

use super::cmd::{SvnError, SvnRunner};

/// One revision, as `svn log --xml -v` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnRevision {
    /// The revision number. `external_id` is `r{revision}` (§6.4).
    pub revision: u64,
    /// Who committed it, where svn recorded one.
    pub author: Option<String>,
    /// When, as svn's ISO-8601 string. Parsed by the caller.
    pub date: Option<String>,
    /// The log message, empty when there is none.
    pub message: String,
    /// The paths it touched, empty for a revision that touched none.
    pub paths: Vec<SvnPath>,
}

impl SvnRevision {
    /// §6.4's identifier for this change.
    pub fn external_id(&self) -> String {
        format!("r{}", self.revision)
    }

    /// Whether this revision touched anything under `watched`.
    pub fn touches(&self, watched: &WatchedPaths) -> bool {
        self.paths.iter().any(|path| watched.matches(&path.path))
    }
}

/// One path inside a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnPath {
    /// `A`, `M`, `D`, `R`.
    pub action: String,
    /// `file` or `dir`, where svn said.
    pub kind: Option<String>,
    /// Repository-absolute, e.g. `/trunk/src/pager.rs`.
    pub path: String,
    /// Where this path was copied from, for a branch creation.
    pub copyfrom_path: Option<String>,
    /// The revision it was copied from.
    pub copyfrom_rev: Option<u64>,
}

/// Which paths a repository watches (§6.4: trunk, plus `branches/*` optionally).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedPaths {
    /// The trunk path, repository-absolute — `/trunk` in a standard layout.
    pub trunk: String,
    /// Whether `branches/*` is watched too.
    pub watch_branches: bool,
    /// The branches root, repository-absolute.
    pub branches_root: String,
}

impl Default for WatchedPaths {
    fn default() -> Self {
        Self {
            trunk: "/trunk".to_owned(),
            watch_branches: false,
            branches_root: "/branches".to_owned(),
        }
    }
}

impl WatchedPaths {
    /// Watch trunk only.
    pub fn trunk_only(trunk: &str) -> Self {
        Self {
            trunk: normalize(trunk),
            ..Self::default()
        }
    }

    /// Watch trunk and every branch.
    pub fn with_branches(trunk: &str, branches_root: &str) -> Self {
        Self {
            trunk: normalize(trunk),
            watch_branches: true,
            branches_root: normalize(branches_root),
        }
    }

    /// Whether a repository-absolute path is watched.
    ///
    /// Matched on a path *boundary*, not a prefix: `/trunk-old/x` is not under
    /// `/trunk`, and a plain `starts_with` would say it was — quietly reviewing a
    /// second repository layout nobody asked about.
    pub fn matches(&self, path: &str) -> bool {
        let path = normalize(path);
        under(&path, &self.trunk) || (self.watch_branches && under(&path, &self.branches_root))
    }
}

/// Leading slash, no trailing slash.
fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// Whether `path` is `root` or sits beneath it.
fn under(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

// --- parsing `svn log --xml` ----------------------------------------------

#[derive(Debug, Deserialize)]
struct LogXml {
    #[serde(default, rename = "logentry")]
    entries: Vec<LogEntryXml>,
}

#[derive(Debug, Deserialize)]
struct LogEntryXml {
    #[serde(rename = "@revision")]
    revision: u64,
    author: Option<String>,
    date: Option<String>,
    msg: Option<String>,
    /// Absent for a revision that changed no paths.
    paths: Option<PathsXml>,
}

#[derive(Debug, Deserialize)]
struct PathsXml {
    #[serde(default, rename = "path")]
    paths: Vec<PathXml>,
}

#[derive(Debug, Deserialize)]
struct PathXml {
    #[serde(rename = "@action")]
    action: String,
    #[serde(rename = "@kind")]
    kind: Option<String>,
    #[serde(rename = "@copyfrom-path")]
    copyfrom_path: Option<String>,
    #[serde(rename = "@copyfrom-rev")]
    copyfrom_rev: Option<u64>,
    #[serde(rename = "$text")]
    path: Option<String>,
}

/// Parse `svn log --xml -v` output.
///
/// Returns entries in the order svn emitted them; `discover` asks for ascending
/// and asserts it rather than sorting here, so a change in svn's ordering is a
/// visible failure rather than a silent correction.
pub fn parse_log_xml(xml: &str) -> Result<Vec<SvnRevision>, SvnError> {
    let parsed: LogXml = quick_xml::de::from_str(xml).map_err(|source| SvnError::Failed {
        args: "log --xml".to_owned(),
        code: 0,
        stderr: format!("could not read svn's XML log: {source}"),
    })?;

    Ok(parsed
        .entries
        .into_iter()
        .map(|entry| SvnRevision {
            revision: entry.revision,
            author: entry.author,
            date: entry.date,
            message: entry.msg.unwrap_or_default(),
            paths: entry
                .paths
                .map(|paths| {
                    paths
                        .paths
                        .into_iter()
                        .map(|path| SvnPath {
                            action: path.action,
                            kind: path.kind,
                            path: path.path.unwrap_or_default(),
                            copyfrom_path: path.copyfrom_path,
                            copyfrom_rev: path.copyfrom_rev,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// What one discovery pass found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discovery {
    /// Revisions that touched a watched path, oldest first.
    pub reviewable: Vec<SvnRevision>,
    /// Revisions seen and filtered out, oldest first.
    ///
    /// Kept rather than dropped: §18's rule, and the cursor has to advance past
    /// them or every poll re-reads them forever.
    pub filtered: Vec<SvnRevision>,
}

impl Discovery {
    /// The highest revision seen, watched or not.
    ///
    /// This is what the cursor becomes — **after** the reviewable revisions have
    /// been recorded, never before. Advancing first turns a crash between the two
    /// into a permanently skipped revision.
    pub fn highest_seen(&self) -> Option<u64> {
        self.reviewable
            .iter()
            .chain(self.filtered.iter())
            .map(|revision| revision.revision)
            .max()
    }

    /// How many revisions the pass looked at.
    pub fn seen(&self) -> usize {
        self.reviewable.len() + self.filtered.len()
    }
}

/// Discover revisions after `cursor`, oldest first (§6.4).
///
/// `cursor` is the last revision already recorded; discovery starts at
/// `cursor + 1`. A repository with nothing new returns an empty `Discovery`
/// rather than an error — no new commits is the normal state of a poll.
pub async fn discover(
    runner: &SvnRunner,
    repo_url: &str,
    cursor: u64,
    limit: u32,
    watched: &WatchedPaths,
) -> Result<Discovery, SvnError> {
    let range = format!("{}:HEAD", cursor.saturating_add(1));
    let limit = limit.to_string();

    let output = match runner
        .run(
            std::path::Path::new("."),
            &[
                "log", "--xml", "-v", "-r", &range, "--limit", &limit, repo_url,
            ],
        )
        .await
    {
        Ok(output) => output,
        // Asking for a range that starts past HEAD is how "nothing new" presents,
        // and it is the common case on a quiet repository rather than a fault.
        Err(SvnError::Failed { stderr, .. }) if is_no_such_revision(&stderr) => {
            return Ok(Discovery::default())
        }
        Err(other) => return Err(other),
    };

    let revisions = parse_log_xml(&output.stdout)?;

    let mut discovery = Discovery::default();
    for revision in revisions {
        if revision.touches(watched) {
            discovery.reviewable.push(revision);
        } else {
            discovery.filtered.push(revision);
        }
    }

    Ok(discovery)
}

/// Whether svn is saying the requested range starts past HEAD.
fn is_no_such_revision(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("no such revision") || lower.contains("e160006")
}
