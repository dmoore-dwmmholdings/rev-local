//! Starting a review by hand (SPEC §15 screen 2).
//!
//! Everything here is plain Rust that never touches Tauri or a process, for the
//! reason the crate docs give: the IPC layer is a thin delegation, and the way to
//! keep it thin is for the parts with a decision in them to live where they can be
//! compiled and tested without a webview.
//!
//! That is not an abstract preference. The branch list was first written inline in
//! the window binary, asking `git for-each-ref` for a `%x1f` separator — a format
//! escape that `git log` understands and `for-each-ref` does not. Every line failed
//! to split, the branch picker was empty on every repository, and nothing could
//! have caught it: the binary needs webkit to compile, so no test in the workspace
//! ever built that line.
//!
//! # The three scopes
//!
//! A review is always "this commit, against this base", and the base is what makes
//! the three cases different:
//!
//! | scope | base | what the engine sees |
//! |---|---|---|
//! | one commit | none | that commit's own diff |
//! | a branch | the branch it forked from | every commit on the branch |
//! | the whole repository | git's empty tree | every tracked file |
//!
//! Expressing "the whole repository" as a base rather than as a mode is what keeps
//! depth selection, truncation and the omitted-file report applying to it: a
//! whole-repository review that only saw part of the tree still says so (§18).

use revlocal_core::{Change, ChangeId, ChangeKind, DiffStat, RepoId, Timestamp};

/// Git's empty tree, re-exported so callers need not know the constant.
pub use revlocal_vcs::EMPTY_TREE;

/// Names a branch review offers as a base, in the order they are preferred.
const BASE_CANDIDATES: [&str; 4] = ["main", "master", "develop", "trunk"];

/// The `for-each-ref` format the branch list is parsed from.
///
/// Object name first, then a space, then the ref: git refuses a ref name
/// containing a space, and the object name in front of it is fixed width, so the
/// first space is unambiguously the separator. `%x1f` is **not** an option —
/// `for-each-ref` emits it literally.
pub const BRANCH_FORMAT: &str = "%(objectname) %(refname:short)";

/// The `git log` format the commit list is parsed from.
///
/// `%xNN` *is* expanded by `git log`'s pretty formats, and a unit separator is
/// safe here because a commit subject can contain almost anything else.
pub const COMMIT_FORMAT: &str = "%H%x1f%s%x1f%an%x1f%aI";

/// A branch which can be browsed and reviewed from the repository screen.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewBranch {
    /// The short ref name.
    pub name: String,
    /// The commit the branch points at, captured when the screen loaded.
    pub head: String,
    /// Whether this is the branch the working copy is on.
    pub current: bool,
}

/// The branches, plus what a branch review should default to comparing against.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BranchList {
    /// Every local branch, most recently committed to first.
    pub branches: Vec<ReviewBranch>,
    /// The base a branch review defaults to, when one is obvious.
    pub suggested_base: Option<String>,
    /// The checked-out branch, so the screen opens on the one being worked on.
    pub current: Option<String>,
}

/// One selectable commit.
///
/// The SHA rather than a display label is the review target, so a branch advancing
/// after the screen loaded cannot change what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewCommit {
    /// The full commit id.
    pub sha: String,
    /// Its subject line, which may be empty.
    pub subject: String,
    /// The author's name as git recorded it.
    pub author: String,
    /// When it was authored, RFC 3339.
    pub authored_at: String,
}

/// What a queued review is, as the screen that asked for it needs to say.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StartedReview {
    /// The run that now exists.
    pub run_id: i64,
    /// Its status — `queued`, because the engine has not started yet.
    pub status: String,
    /// What was queued, in words: "branch feature/x", say.
    pub scope: String,
}

/// Parse `git for-each-ref --format=BRANCH_FORMAT` output.
///
/// A line that does not split is skipped rather than guessed at: half a branch
/// name pointing at half a sha is worse than one fewer branch in the list.
pub fn parse_branches(text: &str, current: Option<&str>) -> Vec<ReviewBranch> {
    text.lines()
        .filter_map(|line| {
            let (head, name) = line.trim_end().split_once(' ')?;
            (!head.is_empty() && !name.is_empty()).then(|| ReviewBranch {
                name: name.to_owned(),
                head: head.to_owned(),
                current: Some(name) == current,
            })
        })
        .collect()
}

/// Parse `git log --format=COMMIT_FORMAT` output.
pub fn parse_commits(text: &str) -> Vec<ReviewCommit> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.trim_end().split('\u{1f}');
            let sha = fields.next().filter(|sha| !sha.is_empty())?;
            Some(ReviewCommit {
                sha: sha.to_owned(),
                subject: fields.next().unwrap_or_default().to_owned(),
                author: fields.next().unwrap_or_default().to_owned(),
                authored_at: fields.next().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// The base a branch review should default to, when the repository has one.
pub fn suggested_base(branches: &[ReviewBranch]) -> Option<String> {
    BASE_CANDIDATES
        .iter()
        .find(|candidate| branches.iter().any(|branch| branch.name == **candidate))
        .map(|candidate| (*candidate).to_owned())
}

/// Assemble the branch list a repository screen opens with.
pub fn branch_list(text: &str, current: Option<&str>) -> BranchList {
    let branches = parse_branches(text, current);
    BranchList {
        suggested_base: suggested_base(&branches),
        current: current.map(str::to_owned),
        branches,
    }
}

/// A short, stable prefix of a commit id, for a sentence rather than a table.
fn short(sha: &str) -> &str {
    let end = sha
        .char_indices()
        .nth(12)
        .map_or(sha.len(), |(index, _)| index);
    &sha[..end]
}

/// What was queued, in the words the person who asked would use.
///
/// Said rather than inferred from a run id: "queued run #14" alone does not tell
/// somebody which of the three buttons they pressed took effect.
pub fn scope_of(base: Option<&str>, branch: Option<&str>, sha: &str) -> String {
    match (base, branch) {
        (Some(EMPTY_TREE), Some(branch)) => format!("the whole repository at {branch}"),
        (Some(EMPTY_TREE), None) => format!("the whole repository at {}", short(sha)),
        (Some(_), Some(branch)) => format!("branch {branch}"),
        (Some(base), None) => format!("{}, compared against {}", short(sha), short(base)),
        (None, _) => format!("commit {}", short(sha)),
    }
}

/// The change's title, which is what every list of runs shows for it.
pub fn title_of(
    base: Option<&str>,
    branch: Option<&str>,
    sha: &str,
    subject: &str,
) -> Option<String> {
    match base {
        Some(EMPTY_TREE) => Some(format!("Whole repository at {}", short(sha))),
        Some(_) => Some(format!(
            "Branch review: {}",
            branch.unwrap_or_else(|| short(sha))
        )),
        None => (!subject.trim().is_empty()).then(|| subject.trim().to_owned()),
    }
}

/// Everything a caller must resolve before a review can be queued.
///
/// The refs are already resolved to object names: a branch that moves between the
/// click and the run must not change what was asked for.
pub struct ResolvedRequest<'a> {
    /// The repository the review belongs to.
    pub repo_id: RepoId,
    /// The commit to review.
    pub sha: &'a str,
    /// Its subject, for the change's title when there is no better one.
    pub subject: &'a str,
    /// The branch the commit was chosen from, when it was chosen from one.
    pub branch: Option<&'a str>,
    /// The resolved base, turning the review into a range.
    pub base: Option<&'a str>,
    /// The author, as `(name, email, authored_at)` — any part may be absent.
    pub author: (Option<String>, Option<String>, Option<Timestamp>),
    /// When the request was made.
    pub at: Timestamp,
}

/// Build the change a manual review is queued against.
pub fn change_for(request: &ResolvedRequest<'_>) -> Change {
    Change {
        id: ChangeId::new(0),
        repo_id: request.repo_id,
        kind: ChangeKind::Commit,
        external_id: request.sha.to_owned(),
        title: title_of(request.base, request.branch, request.sha, request.subject),
        author_name: request.author.0.clone(),
        author_email: request.author.1.clone(),
        authored_at: request.author.2,
        branch: request.branch.map(str::to_owned),
        base_ref: request.base.map(str::to_owned),
        head_ref: Some(request.sha.to_owned()),
        url: None,
        // Filled in by materialization, which is the only thing that can measure
        // it — a range's size is not known until the range has been diffed.
        diff_stat: DiffStat::default(),
        detected_at: request.at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this module exists for. `for-each-ref` does not expand `%xNN`, so a
    /// unit-separated format yields the literal text and every line fails to
    /// split — an empty branch picker on every repository, with no error anywhere.
    #[test]
    fn a_branch_line_is_split_on_the_first_space_not_a_unit_separator() {
        let branches = parse_branches("abc123 main\ndef456 feature/widgets\n", Some("main"));

        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0].name, "main");
        assert_eq!(branches[0].head, "abc123");
        assert!(branches[0].current);
        assert!(!branches[1].current);
    }

    /// A ref name cannot contain a space, but it can contain slashes and dots.
    #[test]
    fn a_branch_name_may_contain_anything_git_allows_in_one() {
        let branches = parse_branches("abc123 release/2.1.x\n", None);

        assert_eq!(branches[0].name, "release/2.1.x");
    }

    /// Skipped, not guessed at: half a name pointing at half a sha is worse than
    /// one fewer branch.
    #[test]
    fn an_unsplittable_branch_line_is_dropped_rather_than_half_read() {
        let branches = parse_branches("main\n\nabc123 real\n", None);

        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, "real");
    }

    #[test]
    fn a_commit_line_keeps_an_empty_subject_as_an_empty_field() {
        // `%s` is empty for a commit with no subject, and the fields after it still
        // have to line up — reading the author into the subject would be worse than
        // showing nothing.
        let commits = parse_commits("abc\u{1f}\u{1f}Sam\u{1f}2026-01-01T00:00:00Z\n");

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "");
        assert_eq!(commits[0].author, "Sam");
    }

    #[test]
    fn a_commit_subject_containing_a_space_is_not_split_on_it() {
        let commits = parse_commits("abc\u{1f}widen the widget\u{1f}Sam\u{1f}2026-01-01T00:00:00Z");

        assert_eq!(commits[0].subject, "widen the widget");
    }

    #[test]
    fn the_suggested_base_prefers_main_then_master() {
        let with_both = parse_branches("a master\nb main\n", None);
        assert_eq!(suggested_base(&with_both).as_deref(), Some("main"));

        let with_master = parse_branches("a master\nb feature\n", None);
        assert_eq!(suggested_base(&with_master).as_deref(), Some("master"));

        let with_neither = parse_branches("a feature\n", None);
        assert_eq!(suggested_base(&with_neither), None);
    }

    /// The three scopes read as three different things, because they are.
    #[test]
    fn each_scope_says_which_of_the_three_reviews_was_queued() {
        let sha = "0123456789abcdef";

        assert_eq!(scope_of(None, Some("feature"), sha), "commit 0123456789ab");
        assert_eq!(
            scope_of(Some("main"), Some("feature"), sha),
            "branch feature"
        );
        assert_eq!(
            scope_of(Some(EMPTY_TREE), Some("feature"), sha),
            "the whole repository at feature"
        );
    }

    #[test]
    fn a_title_names_the_scope_and_falls_back_to_the_commit_subject() {
        let sha = "0123456789abcdef";

        assert_eq!(
            title_of(None, Some("feature"), sha, "widen the widget").as_deref(),
            Some("widen the widget")
        );
        assert_eq!(
            title_of(Some("main"), Some("feature"), sha, "ignored").as_deref(),
            Some("Branch review: feature")
        );
        assert_eq!(
            title_of(Some(EMPTY_TREE), Some("feature"), sha, "ignored").as_deref(),
            Some("Whole repository at 0123456789ab")
        );
        // A commit with no subject gets no invented one.
        assert_eq!(title_of(None, None, sha, "   "), None);
    }

    /// The base is what the executor and the prompt both read to decide the scope.
    #[test]
    fn the_change_carries_the_base_that_makes_it_a_range() {
        let change = change_for(&ResolvedRequest {
            repo_id: RepoId::new(1),
            sha: "0123456789abcdef",
            subject: "widen the widget",
            branch: Some("feature"),
            base: Some(EMPTY_TREE),
            author: (Some("Sam".to_owned()), None, None),
            at: Timestamp::default(),
        });

        assert_eq!(change.base_ref.as_deref(), Some(EMPTY_TREE));
        assert_eq!(change.head_ref.as_deref(), Some("0123456789abcdef"));
        assert_eq!(change.branch.as_deref(), Some("feature"));
        // Not written into `branch`, which means the branch: a marker there would
        // be read as a branch name by everything that shows one.
        assert_eq!(change.external_id, "0123456789abcdef");
    }

    #[test]
    fn a_commit_review_has_no_base_at_all() {
        let change = change_for(&ResolvedRequest {
            repo_id: RepoId::new(1),
            sha: "0123456789abcdef",
            subject: "widen the widget",
            branch: Some("feature"),
            base: None,
            author: (None, None, None),
            at: Timestamp::default(),
        });

        assert_eq!(change.base_ref, None);
        assert_eq!(change.title.as_deref(), Some("widen the widget"));
    }
}
