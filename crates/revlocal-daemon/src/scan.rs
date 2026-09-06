//! Finding the repositories that are already on the disk (REVL-193, SPEC §14).
//!
//! `repo add` registers one repository, which is the right shape for the first
//! one and the wrong shape for the goal: rev-local is meant to sit pointed at
//! every repository on the machine, and typing a command per checkout is the
//! clicking the unattended loop exists to remove.
//!
//! # Why the walk is written out rather than pulled in
//!
//! It is thirty lines and it needs three behaviours a general-purpose directory
//! walker does not give for free: stop at a repository rather than descending
//! into its history, refuse to disappear into `node_modules`, and produce the
//! same order every time so that two scans of an unchanged tree report the same
//! thing. A dependency would still need all three written on top of it.
//!
//! Nothing here touches the database or the network. The walk answers "what is
//! on this disk", `repos::scan` decides what to do about it — which is what
//! makes `--dry-run` a real preview rather than a second code path.

use std::path::{Path, PathBuf};

use revlocal_core::RepoKind;

/// How deep a scan goes below its root when nobody says.
///
/// Four levels reaches `~/code/<org>/<project>` and the odd
/// `~/code/<org>/<group>/<project>`, and stops well short of walking a home
/// directory to its leaves. Somebody with a deeper layout passes `--depth`.
pub const DEFAULT_SCAN_DEPTH: usize = 4;

/// Directory names a scan never descends into.
///
/// Every one of these is a place a checkout *is not*, and two of them
/// (`node_modules`, `target`) are where the time goes on a machine with real
/// work on it. `.git` and `.svn` are listed for the same reason even though the
/// walk already stops at their parent: a nested checkout inside a repository's
/// own metadata is not a repository somebody wants reviewed.
const NEVER_DESCEND: [&str; 8] = [
    ".git",
    ".svn",
    "node_modules",
    "target",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
];

/// One checkout the walk found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Discovered {
    /// Where it is.
    pub path: PathBuf,
    /// What kind of working copy it is.
    pub kind: RepoKind,
}

/// What a walk found, and what it did not look at.
///
/// The second half is the point (SPEC §18). A depth bound can hide a repository
/// as completely as a crash can, and a scan that reported only what it found
/// would read identically whether the tree was fully explored or cut off one
/// level above thirty checkouts.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Discovery {
    /// Every checkout, in a stable order.
    pub found: Vec<Discovered>,
    /// Directories the depth bound stopped the walk from looking inside.
    pub not_descended: usize,
    /// Directories that exist but could not be read.
    pub unreadable: usize,
}

/// Every checkout under `root`, in a stable order.
///
/// Depth is counted from `root`, which is itself examined: scanning a directory
/// that *is* a checkout finds that checkout rather than nothing.
///
/// A directory that cannot be read is skipped rather than failing the scan. On a
/// home directory there is always one — a permission-denied cache, a broken
/// symlink target — and refusing to register twenty-nine readable repositories
/// because of the thirtieth would make the command useless exactly where it is
/// most needed. It is counted, because "there was nothing there" and "I was not
/// allowed to look" are different answers.
pub fn discover(root: &Path, max_depth: usize) -> Discovery {
    let mut discovery = Discovery::default();
    walk(root, 0, max_depth, &mut discovery);
    discovery
}

fn walk(dir: &Path, depth: usize, max_depth: usize, out: &mut Discovery) {
    if let Some(kind) = kind_of(dir) {
        out.found.push(Discovered {
            path: dir.to_path_buf(),
            kind,
        });
        // A repository's subdirectories are its own contents, not more
        // repositories. Submodules are the exception and are deliberately not
        // followed: a submodule is reviewed as part of the repository that
        // pins it, and registering it separately would review every change to
        // it twice.
        return;
    }

    if depth >= max_depth {
        // Counted rather than returned quietly: this is the cap §18 is about,
        // and `--depth` is the remedy the report can then name.
        out.not_descended += 1;
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        out.unreadable += 1;
        return;
    };

    // Collected and sorted rather than walked in `read_dir` order, which is the
    // filesystem's and differs between two machines with identical trees.
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_walkable(path))
        .collect();
    children.sort();

    for child in children {
        walk(&child, depth + 1, max_depth, out);
    }
}

/// Whether this entry is a directory a scan should look inside.
///
/// Symlinks are not followed. A link into a parent directory turns the walk into
/// a loop, and a link to a checkout elsewhere would register that checkout under
/// a path that stops resolving the moment somebody tidies the link away.
fn is_walkable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }

    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    if NEVER_DESCEND.contains(&name) {
        return false;
    }
    // Hidden directories hold caches and application state, and a checkout
    // somebody wants reviewed is not usually in one. The root is exempt because
    // it is named explicitly: `repo scan ~/.local/src` scans what it was asked to.
    !name.starts_with('.')
}

/// What kind of working copy this directory is, if it is one.
///
/// `.git` is matched as either a directory or a file: a linked worktree and a
/// submodule both have a `.git` *file* pointing elsewhere, and both are working
/// copies rev-local can review.
fn kind_of(dir: &Path) -> Option<RepoKind> {
    if dir.join(".git").exists() {
        return Some(RepoKind::Git);
    }
    if dir.join(".svn").is_dir() {
        return Some(RepoKind::Svn);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tree of directories, marking any path ending in `/.git` etc.
    fn tree(root: &Path, dirs: &[&str]) {
        for dir in dirs {
            std::fs::create_dir_all(root.join(dir)).expect("create dir");
        }
    }

    fn paths(discovery: &Discovery, root: &Path) -> Vec<String> {
        discovery
            .found
            .iter()
            .map(|d| {
                d.path
                    .strip_prefix(root)
                    .unwrap_or(&d.path)
                    .display()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_scan_finds_every_checkout_under_the_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &["one/.git", "two/.git", "notes"]);

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        assert_eq!(paths(&found, root), vec!["one", "two"]);
    }

    #[test]
    fn a_scan_stops_at_a_repository_rather_than_walking_its_contents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        // A vendored checkout inside a checkout: one repository, not two.
        tree(root, &["outer/.git", "outer/deps/inner/.git"]);

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        assert_eq!(paths(&found, root), vec!["outer"]);
    }

    #[test]
    fn a_root_that_is_itself_a_checkout_is_the_result() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &[".git"]);

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        assert_eq!(found.found.len(), 1, "the root itself: {found:?}");
        assert_eq!(found.found[0].path, root);
    }

    #[test]
    fn subversion_working_copies_are_found_too() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &["gitrepo/.git", "svnrepo/.svn"]);

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        assert_eq!(
            found
                .found
                .iter()
                .map(|d| d.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["git", "svn"]
        );
    }

    #[test]
    fn a_worktree_whose_dot_git_is_a_file_still_counts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &["linked"]);
        std::fs::write(root.join("linked/.git"), "gitdir: /elsewhere\n").expect("write");

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        assert_eq!(paths(&found, root), vec!["linked"]);
    }

    #[test]
    fn the_places_repositories_are_not_are_never_walked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(
            root,
            &[
                "app/node_modules/pkg/.git",
                "app/target/debug/thing/.git",
                ".cache/something/.git",
            ],
        );

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        assert!(found.found.is_empty(), "walked into noise: {found:?}");
    }

    #[test]
    fn depth_bounds_the_walk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &["a/b/c/deep/.git"]);

        let shallow = discover(root, 2);
        assert!(shallow.found.is_empty(), "depth 2 reached a/b/c/deep");
        assert!(
            shallow.not_descended > 0,
            "a walk cut off by its own depth bound must say so, or a scan that \
             found nothing looks like a tree with nothing in it: {shallow:?}"
        );
        assert_eq!(paths(&discover(root, 4), root), vec!["a/b/c/deep"]);
    }

    #[test]
    fn a_directory_that_cannot_be_read_does_not_stop_the_scan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &["readable/.git", "locked"]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
        }

        let found = discover(root, DEFAULT_SCAN_DEPTH);

        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;
        #[cfg(unix)]
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");

        assert_eq!(paths(&found, root), vec!["readable"]);
        #[cfg(unix)]
        assert_eq!(
            found.unreadable, 1,
            "\"nothing there\" and \"not allowed to look\" are different answers: {found:?}"
        );
    }

    #[test]
    fn the_order_is_the_same_on_every_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        tree(root, &["zeta/.git", "alpha/.git", "mid/.git"]);

        assert_eq!(
            paths(&discover(root, DEFAULT_SCAN_DEPTH), root),
            vec!["alpha", "mid", "zeta"]
        );
    }
}
