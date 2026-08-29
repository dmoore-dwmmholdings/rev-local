//! The documentation describes the binary that exists (RL-1206).
//!
//! REVL-103's third criterion is that every doc claim is verified against the
//! built binary rather than assumed. A person can check that once; only a test
//! checks it again after the next rename.
//!
//! # What this can and cannot check
//!
//! It checks the part that goes stale silently: whether a command written in the
//! documentation is one the binary accepts. It cannot check that the prose is
//! *true* — that `pause` really holds publish actions is asserted where `pause`
//! lives, not here.
//!
//! That is the right split. A doc test that tried to verify behaviour would
//! duplicate the suite and drift from it; a doc test that verifies the surface
//! catches the failure documentation actually has, which is describing a command
//! that was renamed or never shipped.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path
}

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_default();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) {
        "revlocal.exe"
    } else {
        "revlocal"
    })
}

/// The documents this checks.
const DOCS: &[&str] = &["README.md", "USAGE.md", "docs/OPERATIONS.md"];

/// Every `revlocal <group> [sub]` invocation written in `text`.
///
/// Only from places where the whole span is a command: a fenced code block, or
/// between backticks. Prose is skipped, because "`revlocal doctor` is the only
/// command that…" would otherwise read `is` as a subcommand of `doctor`.
///
/// The first version handled that with a fallback — if `group sub` was rejected
/// but `group` alone was accepted, let it pass. That silently swallowed a renamed
/// subcommand: `budget clear` was accepted because `budget` exists, which is the
/// exact failure this test is for. Reading only real command spans removes the
/// need for the fallback, and with it the hole.
fn commands_in(text: &str) -> BTreeSet<(String, Option<String>)> {
    let mut found = BTreeSet::new();
    let mut in_fence = false;

    let take = |span: &str, found: &mut BTreeSet<(String, Option<String>)>| {
        // `$ ` prefixes a transcript line.
        let span = span.trim().trim_start_matches("$ ");
        let Some(rest) = span.strip_prefix("revlocal ") else {
            return;
        };
        let mut words = rest.split_whitespace();
        let Some(group) = words.next() else { return };
        if !group.chars().all(|c| c.is_ascii_lowercase()) {
            return;
        }
        let sub = words.next().and_then(|word| {
            // A hyphen is allowed inside a subcommand and never at the start:
            // `bare-mirror` is a name, `--repo` is a flag.
            let bare =
                !word.starts_with('-') && word.chars().all(|c| c.is_ascii_lowercase() || c == '-');
            bare.then(|| word.to_owned())
        });
        found.insert((group.to_owned(), sub));
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }

        if in_fence {
            take(trimmed, &mut found);
            continue;
        }

        // Outside a fence, only the odd-numbered backtick spans are code.
        for (n, span) in trimmed.split('`').enumerate() {
            if n % 2 == 1 {
                take(span, &mut found);
            }
        }
    }
    found
}

/// Whether the binary accepts this invocation.
fn accepted(group: &str, sub: Option<&str>) -> bool {
    let mut args = vec![group];
    if let Some(sub) = sub {
        args.push(sub);
    }
    args.push("--help");
    std::process::Command::new(binary())
        .args(&args)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn every_command_the_docs_mention_is_one_the_binary_accepts() -> Result<(), String> {
    let root = workspace_root();
    let mut checked = 0usize;
    let mut wrong: Vec<String> = Vec::new();

    for doc in DOCS {
        let path = root.join(doc);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;

        for (group, sub) in commands_in(&text) {
            checked += 1;
            if accepted(&group, sub.as_deref()) {
                continue;
            }
            wrong.push(match &sub {
                Some(sub) => format!("{doc}: revlocal {group} {sub}"),
                None => format!("{doc}: revlocal {group}"),
            });
        }
    }

    assert!(
        checked > 20,
        "only {checked} commands found across {} documents; the extractor has \
         stopped matching",
        DOCS.len()
    );
    assert!(
        wrong.is_empty(),
        "the documentation describes commands the binary does not accept:\n  {}",
        wrong.join("\n  ")
    );
    Ok(())
}

#[test]
fn the_documents_this_checks_all_exist() -> Result<(), String> {
    // REVL-103's gate is `test -f README.md && test -f docs/OPERATIONS.md`. A doc
    // test that silently skipped a missing file would pass hardest exactly when
    // the documentation had been deleted.
    let root = workspace_root();
    for doc in DOCS {
        let path: &Path = &root.join(doc);
        assert!(path.is_file(), "{} is missing", path.display());
    }
    Ok(())
}

#[test]
fn the_exit_codes_documented_are_the_ones_the_binary_prints() -> Result<(), String> {
    // The exit-code table is the part of the documentation a script is written
    // against, and the part with no other test. `--help` is where the binary
    // states them, so the two must agree.
    let root = workspace_root();
    let ops = std::fs::read_to_string(root.join("docs/OPERATIONS.md"))
        .map_err(|e| format!("reading docs/OPERATIONS.md: {e}"))?;

    let help = std::process::Command::new(binary())
        .arg("--help")
        .output()
        .map_err(|e| format!("running --help: {e}"))?;
    let help = String::from_utf8_lossy(&help.stdout);

    for (code, phrase) in [
        ("0", "succeeded"),
        ("1", "retrying may work"),
        ("2", "fix it rather than retrying"),
        ("3", "budget"),
        ("4", "approve"),
    ] {
        assert!(
            help.contains(&format!("  {code}  ")),
            "`revlocal --help` does not document exit code {code}"
        );
        assert!(
            help.to_lowercase().contains(phrase),
            "`revlocal --help` no longer says {phrase:?} about exit code {code}"
        );
        assert!(
            ops.contains(&format!("`{code}`")),
            "docs/OPERATIONS.md does not document exit code {code}"
        );
    }
    Ok(())
}

/// The README's minimum Rust version is the one the workspace declares.
///
/// A number in prose has nothing holding it to the manifest. This one was already
/// wrong once in the other direction — removed as unverifiable when it was in fact
/// correct — which is its own kind of drift.
///
/// It checks agreement with `rust-version`, not that the code truly builds on
/// 1.82: only a 1.82 toolchain can say that, and CI uses stable. Agreement is the
/// claim this file can honestly make.
#[test]
fn the_readme_states_the_declared_minimum_rust_version() -> Result<(), String> {
    let root = workspace_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|e| format!("reading Cargo.toml: {e}"))?;
    let declared = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("rust-version = "))
        .map(|value| value.trim_matches('"').to_owned())
        .ok_or("Cargo.toml declares no rust-version")?;

    let readme = std::fs::read_to_string(root.join("README.md"))
        .map_err(|e| format!("reading README.md: {e}"))?;

    assert!(
        readme.contains(&format!("({declared}+)")),
        "Cargo.toml declares rust-version {declared:?}; the README does not say so"
    );
    Ok(())
}

/// Every leaf command accepts `--json`, as both the docs and `--help` claim.
///
/// `revlocal --help` ends with "Every command accepts --json for machine-readable
/// output", and USAGE.md repeats it. That was **false** when written: `db migrate`
/// rejected the flag. It was found by running all seventeen commands rather than
/// reading the sentence, and this keeps the next command added from breaking it
/// again.
///
/// The parser's own generated help is the source here, not another tool's output:
/// clap prints the flag because the flag is declared, so a match on it is a match
/// on the parser.
#[test]
fn every_leaf_command_accepts_json() -> Result<(), String> {
    /// Leaf commands, from `--help`'s own listing of each group.
    fn leaves(group: &str) -> Vec<Option<String>> {
        let out = std::process::Command::new(binary())
            .args([group, "--help"])
            .output();
        let Ok(out) = out else { return vec![None] };
        let text = String::from_utf8_lossy(&out.stdout);

        let Some(commands) = text.split("Commands:").nth(1) else {
            // No subcommands: the group is itself the leaf.
            return vec![None];
        };
        let subs: Vec<Option<String>> = commands
            .lines()
            .take_while(|line| !line.trim().is_empty() || line.is_empty())
            .filter_map(|line| {
                let word = line.split_whitespace().next()?;
                (word != "help" && word.chars().all(|c| c.is_ascii_lowercase()))
                    .then(|| Some(word.to_owned()))
            })
            .collect();
        if subs.is_empty() {
            vec![None]
        } else {
            subs
        }
    }

    let groups = [
        "db",
        "publish",
        "targets",
        "backfill",
        "watch",
        "runs",
        "findings",
        "approvals",
        "budget",
        "hooks",
        "webhook",
        "doctor",
        "pause",
        "resume",
        "kill",
        "repo",
        "review",
    ];

    let mut missing = Vec::new();
    let mut checked = 0usize;
    for group in groups {
        for sub in leaves(group) {
            let mut args = vec![group.to_owned()];
            if let Some(sub) = &sub {
                args.push(sub.clone());
            }
            args.push("--help".to_owned());

            let out = std::process::Command::new(binary())
                .args(&args)
                .output()
                .map_err(|e| format!("running {args:?}: {e}"))?;
            let help = String::from_utf8_lossy(&out.stdout);
            checked += 1;

            if !help.contains("--json") {
                missing.push(match sub {
                    Some(sub) => format!("revlocal {group} {sub}"),
                    None => format!("revlocal {group}"),
                });
            }
        }
    }

    assert!(
        checked >= 17,
        "only {checked} leaf commands found; the extractor has stopped matching"
    );
    assert!(
        missing.is_empty(),
        "`revlocal --help` and USAGE.md both say every command accepts --json; \
         these do not: {missing:?}"
    );
    Ok(())
}
