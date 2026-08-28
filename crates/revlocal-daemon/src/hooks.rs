//! The git hook installer (RL-1004, SPEC §7.2).
//!
//! # The rule that shapes everything here
//!
//! §7.2: **a developer's commit must never fail because rev-local is down.** That
//! is not a quality goal, it is the condition for this feature being installable
//! at all. A code-review tool that can block `git commit` is a tool people
//! uninstall after the first time it happens, and they are right to.
//!
//! So the generated script has exactly one guarantee it makes unconditionally: it
//! ends with `exit 0`. Not "exits 0 on success" — `exit 0` is the last line, and
//! every path reaches it. `set -e` is deliberately absent. `curl`'s status is
//! deliberately discarded. The receiver being down, the port being wrong, the
//! secret being stale, `curl` not existing at all: every one of those is a commit
//! that succeeds and a review that does not happen, which is the correct trade in
//! both directions.
//!
//! The 2-second timeout is the other half. A receiver that accepts the connection
//! and then stalls would hang the commit without ever failing it, which is worse
//! than a refused connection — so the timeout is on the *whole* request, not just
//! the connect.
//!
//! # Never clobber a hook somebody else wrote
//!
//! A repository may already have hooks, from Husky, from pre-commit, from a script
//! a colleague wrote in 2019 that nobody understands and everybody needs. Writing
//! over one loses work that was not ours to lose.
//!
//! So the block is delimited, appended, and removed by exactly its markers.
//! Install is idempotent because it removes any existing block before appending —
//! running it twice is the same as running it once, which matters because the
//! natural response to "did that work?" is to run it again.
//!
//! # Line endings are a correctness issue, not cosmetics
//!
//! Git for Windows runs hooks through its own bash, which will not execute a
//! script whose shebang line ends `\r\n` — it reports `/bin/sh^M: bad
//! interpreter`. A hook written with native line endings on Windows is a hook that
//! never runs, silently, which reads exactly like a hook that ran and found
//! nothing. Every file this module writes uses `\n`, on every platform.

use std::path::{Path, PathBuf};

/// Where a block starts. Matched exactly; never generated with anything appended.
pub const BEGIN_MARKER: &str = "# >>> rev-local begin (managed block; do not edit) >>>";

/// Where a block ends.
pub const END_MARKER: &str = "# <<< rev-local end <<<";

/// How long a hook waits for the receiver before giving up (§7.2).
pub const HOOK_TIMEOUT_SECS: u32 = 2;

/// Which hooks to install (SPEC §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookMode {
    /// The developer's own clone: `post-commit`, `post-merge`, `post-checkout`.
    Reference,
    /// A bare mirror developers push to: `post-receive`.
    ///
    /// §7.2: the only way to see every pushed ref, including deletions.
    BareMirror,
}

impl HookMode {
    /// The hook file names this mode installs.
    pub const fn hook_names(self) -> &'static [&'static str] {
        match self {
            Self::Reference => &["post-commit", "post-merge", "post-checkout"],
            Self::BareMirror => &["post-receive"],
        }
    }

    /// How this reads on the command line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::BareMirror => "bare-mirror",
        }
    }
}

/// What an install or uninstall did to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// The file did not exist; a whole hook was written.
    Created(PathBuf),
    /// A block was appended to a hook somebody else owns.
    Appended(PathBuf),
    /// A block was already there and was replaced, leaving one block.
    Replaced(PathBuf),
    /// A block was removed and the rest of the file kept.
    Removed(PathBuf),
    /// A file rev-local wrote entirely was deleted.
    Deleted(PathBuf),
    /// Nothing to do.
    Untouched(PathBuf),
}

impl HookOutcome {
    /// The file this concerns.
    pub fn path(&self) -> &Path {
        match self {
            Self::Created(path)
            | Self::Appended(path)
            | Self::Replaced(path)
            | Self::Removed(path)
            | Self::Deleted(path)
            | Self::Untouched(path) => path,
        }
    }
}

/// Why hooks could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// The path given is not a git repository.
    #[error(
        "{path} does not look like a git repository (no hooks directory)\n  \
         try: point --repo at a working copy, or at the bare mirror itself for \
         --mode bare-mirror"
    )]
    NotARepo {
        /// What was given.
        path: String,
    },

    /// A hook file could not be read or written.
    #[error("could not write {path}: {source}")]
    Io {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// Where the hooks directory is for a repository path.
///
/// Accepts a working copy (`.git/hooks`) or a bare repository (`hooks`), because
/// §7.2's two modes point at exactly those two shapes.
pub fn hooks_dir(repo_path: &Path) -> Result<PathBuf, HookError> {
    let candidates = [
        repo_path.join(".git").join("hooks"),
        repo_path.join("hooks"),
    ];

    for candidate in candidates {
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }

    // A `.git` file rather than a directory is a worktree or submodule. Creating
    // the directory would put hooks somewhere git will not read them.
    Err(HookError::NotARepo {
        path: repo_path.display().to_string(),
    })
}

/// The managed block for one hook.
///
/// Every line matters, so each is commented in the generated script itself. A
/// developer who opens their own hook file and finds an unexplained curl in it is
/// entitled to be alarmed.
pub fn managed_block(repo_name: &str, port: u16, secret_env: &str) -> String {
    format!(
        "{BEGIN_MARKER}\n\
         # Notifies rev-local that {repo_name} changed. Installed by:\n\
         #   revlocal hooks install --repo {repo_name}\n\
         # Remove with `revlocal hooks uninstall`, or delete these lines.\n\
         #\n\
         # This block must never fail your commit. It has no `set -e`, it ignores\n\
         # curl's exit status, and the hook ends in `exit 0`. If rev-local is not\n\
         # running, your commit succeeds and no review happens.\n\
         if command -v curl >/dev/null 2>&1; then\n\
         \x20 curl --silent --show-error --output /dev/null \\\n\
         \x20      --max-time {HOOK_TIMEOUT_SECS} \\\n\
         \x20      --header 'content-type: application/json' \\\n\
         \x20      --header \"x-revlocal-secret: ${{{secret_env}:-}}\" \\\n\
         \x20      --data '{{\"repo\":\"{repo_name}\"}}' \\\n\
         \x20      http://127.0.0.1:{port}/trigger >/dev/null 2>&1 || true\n\
         fi\n\
         {END_MARKER}\n"
    )
}

/// A complete hook file, for the case where none existed.
fn whole_hook(block: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Written by rev-local. Safe to edit outside the managed block below.\n\
         \n\
         {block}\n\
         # rev-local's block must never be the reason this hook fails.\n\
         exit 0\n"
    )
}

/// Insert a block into a hook's text, in a position where it will actually run.
///
/// Appending is the obvious thing and it is wrong. Hook scripts conventionally end
/// with `exit 0`, and a block appended after an unconditional exit is a block that
/// never executes — the install reports success, the file visibly contains the
/// trigger, and no trigger ever fires. That is the worst shape a bug can take: it
/// looks correct in every place somebody would check.
///
/// So a trailing `exit` is found and the block goes before it. §7.2 says the block
/// is appended, which this still is in the ordinary sense of "added at the end of
/// the script's work" rather than "after the line that ends the script".
fn insert_block(base: &str, block: &str) -> String {
    let mut lines: Vec<&str> = base.lines().collect();

    // The last line that actually does something. Comments and blanks after an
    // `exit` are still after it.
    let trailing_exit = lines.iter().rposition(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with('#')
    });

    let insert_at = match trailing_exit {
        Some(index) if lines[index].trim_start().starts_with("exit") => index,
        _ => lines.len(),
    };

    let block_lines: Vec<&str> = block.lines().collect();
    for (offset, line) in block_lines.iter().enumerate() {
        lines.insert(insert_at + offset, line);
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Remove the managed block from a hook's text, if it has one.
///
/// Returns `None` when there was no block, so a caller can tell "nothing to do"
/// from "removed something".
pub fn strip_block(text: &str) -> Option<String> {
    let begin = text.find(BEGIN_MARKER)?;
    let end = text.find(END_MARKER)?;
    if end < begin {
        return None;
    }

    let after = end + END_MARKER.len();
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..begin]);
    // The newline that followed the end marker belongs to the block, not to
    // whatever came after it — otherwise every install/uninstall cycle leaves the
    // file one blank line longer than it started.
    let tail = text[after..].strip_prefix('\n').unwrap_or(&text[after..]);
    out.push_str(tail);
    Some(out)
}

/// Swap a new block in where the old one was.
fn replace_block_in_place(text: &str, block: &str) -> String {
    let (Some(begin), Some(end)) = (text.find(BEGIN_MARKER), text.find(END_MARKER)) else {
        return insert_block(text, block);
    };
    if end < begin {
        return insert_block(text, block);
    }

    let after = end + END_MARKER.len();
    let tail = text[after..].strip_prefix('\n').unwrap_or(&text[after..]);

    let mut out = String::with_capacity(text.len() + block.len());
    out.push_str(&text[..begin]);
    out.push_str(block);
    out.push_str(tail);
    out
}

/// Whether a hook file is one rev-local wrote in its entirety.
///
/// Used to decide between deleting the file and stripping the block. Getting this
/// wrong in the safe direction leaves an inert hook behind; getting it wrong the
/// other way deletes somebody's script.
fn is_ours_entirely(text: &str) -> bool {
    let Some(stripped) = strip_block(text) else {
        return false;
    };
    let remainder: String = stripped
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#') && line != "exit 0"
        })
        .collect();
    remainder.is_empty()
}

/// Install hooks into a repository (§7.2).
///
/// `secret_env` names the environment variable the hook reads its shared secret
/// from. The secret is never written into the hook: hooks live inside the
/// repository's `.git`, which is not committed, but it is backed up, copied
/// between machines, and read by anything with filesystem access. An env var name
/// is not a secret; a secret in a file is.
pub fn install(
    repo_path: &Path,
    repo_name: &str,
    mode: HookMode,
    port: u16,
    secret_env: &str,
) -> Result<Vec<HookOutcome>, HookError> {
    let dir = hooks_dir(repo_path)?;
    let block = managed_block(repo_name, port, secret_env);

    let mut outcomes = Vec::new();
    for name in mode.hook_names() {
        let path = dir.join(name);
        outcomes.push(install_one(&path, &block)?);
    }
    Ok(outcomes)
}

/// Install into one hook file.
fn install_one(path: &Path, block: &str) -> Result<HookOutcome, HookError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(HookError::Io {
                path: path.display().to_string(),
                source,
            })
        }
    };

    let (contents, outcome) = match existing {
        None => (whole_hook(block), HookOutcome::Created(path.to_owned())),
        Some(text) if text.contains(BEGIN_MARKER) => {
            // Replace in place, at the markers, rather than stripping and
            // re-appending. Re-appending moved the block to the end of the file,
            // which in a hook ending `exit 0` — including the ones rev-local
            // writes itself — put it after the exit and silently disabled it.
            // Install is idempotent because the natural response to "did that
            // work?" is to run it again.
            let merged = replace_block_in_place(&text, block);
            (merged, HookOutcome::Replaced(path.to_owned()))
        }
        Some(text) => (
            insert_block(&text, block),
            HookOutcome::Appended(path.to_owned()),
        ),
    };

    write_hook(path, &contents)?;
    Ok(outcome)
}

/// Remove rev-local's hooks from a repository.
pub fn uninstall(repo_path: &Path, mode: HookMode) -> Result<Vec<HookOutcome>, HookError> {
    let dir = hooks_dir(repo_path)?;

    let mut outcomes = Vec::new();
    for name in mode.hook_names() {
        let path = dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            outcomes.push(HookOutcome::Untouched(path));
            continue;
        };

        let Some(stripped) = strip_block(&text) else {
            outcomes.push(HookOutcome::Untouched(path));
            continue;
        };

        if is_ours_entirely(&text) {
            std::fs::remove_file(&path).map_err(|source| HookError::Io {
                path: path.display().to_string(),
                source,
            })?;
            outcomes.push(HookOutcome::Deleted(path));
        } else {
            write_hook(&path, &stripped)?;
            outcomes.push(HookOutcome::Removed(path));
        }
    }
    Ok(outcomes)
}

/// Write a hook file with LF endings and, on Unix, the executable bit.
///
/// Both halves are load-bearing. Git for Windows will not run a script whose
/// shebang ends `\r\n` — it reports `bad interpreter` — and a hook without `+x`
/// on Unix is simply skipped. Either way the failure is silent and looks exactly
/// like a hook that ran and found nothing.
fn write_hook(path: &Path, contents: &str) -> Result<(), HookError> {
    // Written as bytes so no platform layer can helpfully translate them.
    let normalized = contents.replace("\r\n", "\n");
    std::fs::write(path, normalized.as_bytes()).map_err(|source| HookError::Io {
        path: path.display().to_string(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .map_err(|source| HookError::Io {
                path: path.display().to_string(),
                source,
            })?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        std::fs::set_permissions(path, permissions).map_err(|source| HookError::Io {
            path: path.display().to_string(),
            source,
        })?;
    }

    Ok(())
}
