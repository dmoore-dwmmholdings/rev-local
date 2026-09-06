//! The standing no-unreachable-API audit (REVL-216, REVL-219).
//!
//! A `pub fn` that nothing calls is not dead weight — it is a claim. It has a doc
//! comment describing behaviour, it appears in the module next to functions that
//! do run, and to anybody scanning the file it reads as a working feature. That
//! is how the following each stayed hidden, some of them for milestones:
//!
//! - `revlocal_mcp::builtin_target` — §11.2's "built-in profile per known target",
//!   reachable only from the desktop settings screen, so a headless install
//!   mapped no capabilities at all and could file nothing to Andare (REVL-219);
//! - `PollSchedule::succeeded`/`failed` — every repository reported `healthy`
//!   forever, and a repository failing every poll was retried at full rate
//!   because the backoff reads a failure count nothing increments (REVL-215);
//! - `AuditStore::list_for_run` — everything outbound is audited and nothing can
//!   read the audit log (REVL-217);
//! - `svn::pseudo_pr` and the SVN/GitHub adapters (REVL-184, REVL-189);
//! - `TickReport::is_newsworthy`, whose own doc says "a quiet tick every minute
//!   would drown the one that matters", consulted by nothing (REVL-216).
//!
//! # Why this is a test rather than a lint
//!
//! `dead_code` does not fire on `pub` items in a library: they are part of the
//! crate's API by definition, and the compiler cannot know that this workspace is
//! the only consumer. It is, and that makes "nothing in the workspace names this"
//! a fact worth asserting.
//!
//! # What it checks, and what it deliberately does not
//!
//! Only the unambiguous case: a `pub fn` whose name appears **exactly once** in
//! the whole workspace — its own definition — so nothing calls it, and no test
//! covers it either. A function used only inside its own module is a different
//! and much weaker signal (it may simply want to be private), and flagging those
//! would produce a list nobody reads, which is the failure mode this file exists
//! to prevent rather than commit.
//!
//! Like `no_silent_caps`, this does not verify that existing entries are still
//! true. It checks that nobody adds a new unreachable public function quietly.
//!
//! # What it cannot see
//!
//! Counting occurrences of a name means a **common name hides**. `logging::init`
//! is the whole of SPEC's file-logging requirement — a rolling JSON log under
//! `{data_dir}/logs/` with the redaction layer — and nothing calls it, so neither
//! the daemon nor the app writes a log file at all (REVL-220). This check is
//! silent about it, because "init" appears everywhere.
//!
//! That was found by hand, hours after this file was written, which is the
//! honest measure of the gap. Anything named `init`, `new`, `run`, `get` or
//! similar is outside what this can assert, and a reviewer should not read a
//! passing run as "every public function has a caller".
//!
//! The second blind spot is **transitive**: this asks whether a function has a
//! caller, not whether that caller has one. `EngineRunner::probe` calls
//! `withheld_credentials`, so both look reachable — and nothing calls `probe`
//! outside its own crate's tests, so the §8.5 diagnostic that explains a withheld
//! `ANTHROPIC_API_KEY` reaches no screen at all (REVL-224). A chain of live-looking
//! functions hanging off a dead root is invisible here by construction.
//!
//! Resolving either properly needs call-graph information rather than text —
//! `cargo +nightly rustc -Zunpretty=expanded` or a `syn`-based pass, reachability
//! computed from the binaries' entry points rather than from name counts — which
//! is a bigger tool than this file is trying to be. The narrow check earns its
//! keep by catching the distinctive names, which is where nine of the eleven
//! instances were.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Public functions that nothing references, and why that is allowed for now.
///
/// An entry is a claim somebody made and can be checked. "Tracked in REVL-216"
/// is a legitimate entry while that issue is open; a *blank* one is not, which is
/// why the test below rejects those.
const ACCOUNTED_FOR: &[(&str, &str)] = &[
    (
        "engine_out_dir",
        "REVL-216. §6.1's per-run scratch layout, exposed for a caller that was \
         never written — the executor builds its own paths. Wire it up or delete \
         it; the decision is which of the two is the real layout.",
    ),
    (
        "fully_reported",
        "REVL-216. Whether a run's output is the whole of what the engine said, \
         written 'for a screen deciding whether to show a banner at all'. No \
         screen asks. The three underlying flags are reported separately and are \
         what the screens actually read.",
    ),
    (
        "help_epilogue",
        "REVL-216. The exit-code epilogue is inlined in `main.rs`'s `after_help` \
         as a literal instead of being read from here, so the two can disagree \
         about what exit code 3 means.",
    ),
    (
        "is_newsworthy",
        "REVL-216, and the one worth attention: 'a quiet tick every minute would \
         drown the one that matters', and nothing consults it. The desktop \
         notifier has its own `is_worth_showing` and rate limiter, so \
         notifications are filtered — by a second opinion, which is how the two \
         come to disagree.",
    ),
    (
        "render_unchecked",
        "REVL-216. Renders a capability's args without validating them against the \
         tool's schema. Nothing calls it, and §11.2 requires the checked path — \
         this one existing at all is a foot-gun worth deleting rather than wiring.",
    ),
    (
        "set_probe",
        "REVL-216. A hook for steering the mock engine's probe result, which no \
         test uses; the mock's behaviour is driven by its fixture profiles instead.",
    ),
    (
        "stale_before",
        "REVL-216. Its doc says 'exposed because the daemon logs it at startup: \
         \"recovering runs untouched since X\"'. The daemon logs neither line. The \
         comment describes behaviour the product does not have, which is worse \
         than the unused function.",
    ),
    (
        "is_exhausted",
        "REVL-216. `RetryPolicy`'s own answer to \"has this run out of attempts\". \
         The queue decides by comparing `attempts` to `max_attempts` itself, so \
         the rule is written twice and only one copy is the policy's.",
    ),
    (
        "repo_of",
        "REVL-216. `pub const fn repo_of(id: RepoId) -> RepoId { id }` — an \
         identity function with a doc comment, 'for callers that only need the \
         id'. There are none. This one is deletion rather than wiring.",
    ),
    (
        "retry_policy",
        "REVL-216. Hands out the queue's `RetryPolicy` for inspection. Nothing \
         inspects it; `dispatch_pending` applies it internally.",
    ),
    (
        "with_limit",
        "REVL-216, and the one to look at before GitHub PR review lands: it caps \
         how many pull requests one pass fetches, and the builder that offers it \
         is never called with it, so the hardcoded 100 is what every pass uses. \
         §18 territory once PR discovery has an adapter behind it (README calls \
         that out as not yet supported).",
    ),
    (
        "target_ids",
        "REVL-216. Enumerates the queue's registered targets. `dispatch_pending` \
         routes by looking each action's target up directly, so nothing needs the \
         list.",
    ),
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Every `.rs` file under `crates/`, source and tests both.
///
/// Tests count as references on purpose. A function only a test calls is covered
/// by something, which is a different situation from one nothing mentions at all
/// — and the ones that hurt were in the second group.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `target/` is build output; scanning it would take minutes and
                // find generated copies of the same names.
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The `pub fn` names defined in `text`, ignoring anything under `#[cfg(test)]`.
///
/// A test module's own helpers are not API and are not this file's business.
fn public_fns(text: &str) -> Vec<String> {
    let head = text.split("#[cfg(test)]").next().unwrap_or(text);
    let mut names = Vec::new();
    for line in head.lines() {
        let line = line.trim_start();
        let rest = line
            .strip_prefix("pub async fn ")
            .or_else(|| line.strip_prefix("pub fn "))
            .or_else(|| line.strip_prefix("pub const fn "));
        if let Some(rest) = rest {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

/// How many times `name` appears as a whole word across every file.
fn occurrences(name: &str, texts: &[String]) -> usize {
    let mut total = 0;
    for text in texts {
        let bytes = text.as_bytes();
        let mut from = 0;
        while let Some(at) = text[from..].find(name) {
            let start = from + at;
            let end = start + name.len();
            let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
            let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
            if before_ok && after_ok {
                total += 1;
            }
            from = end;
        }
    }
    total
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Public functions whose name appears exactly once in the workspace.
fn unreachable_fns(root: &Path) -> Vec<String> {
    let files = rust_files(root);
    let texts: Vec<String> = files
        .iter()
        // This file is excluded from its own scan. Every name in `ACCOUNTED_FOR`
        // appears here, so counting this file would make each accounted-for
        // function occur twice, drop out of the detector, and be reported as a
        // stale entry — the registry hiding the very things it records.
        //
        // Found by running it: the first execution reported all eight entries as
        // no longer unreachable, seconds after they were.
        .filter(|f| {
            f.file_name()
                .is_some_and(|name| name != "no_unreachable_api.rs")
        })
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .collect();

    // Defined in exactly one place: a name defined twice is an inherent method on
    // two types, and "which one is unused" is not a question this can answer.
    let mut defined: BTreeMap<String, usize> = BTreeMap::new();
    for text in &texts {
        for name in public_fns(text) {
            *defined.entry(name).or_default() += 1;
        }
    }

    let mut found: Vec<String> = defined
        .into_iter()
        .filter(|(_, times)| *times == 1)
        .map(|(name, _)| name)
        .filter(|name| occurrences(name, &texts) == 1)
        .collect();
    found.sort();
    found
}

#[test]
fn no_unreachable_api_every_public_fn_is_called_or_accounted_for() {
    // The check that earns this file's keep. A new `pub fn` that nothing calls
    // fails the suite until somebody says why it is there.
    let unaccounted: Vec<String> = unreachable_fns(&workspace_root())
        .into_iter()
        .filter(|name| ACCOUNTED_FOR.iter().all(|(known, _)| known != name))
        .collect();

    assert!(
        unaccounted.is_empty(),
        "these public functions are defined and never named again — nothing calls \
         them and no test covers them. A `pub fn` with a doc comment and no caller \
         reads as a working feature to anybody scanning the module, which is how \
         REVL-184, 189, 215, 217 and 219 each stayed hidden. Wire it up, delete \
         it, or add it to ACCOUNTED_FOR with the reason:\n  {}",
        unaccounted.join("\n  ")
    );
}

#[test]
fn no_unreachable_api_the_registry_has_no_stale_entries() {
    // A registry nobody prunes is a registry nobody trusts. An entry for a
    // function that is now called — or now deleted — is a claim that has quietly
    // stopped being checked.
    let live = unreachable_fns(&workspace_root());
    let stale: Vec<&str> = ACCOUNTED_FOR
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !live.iter().any(|found| found == name))
        .collect();

    assert!(
        stale.is_empty(),
        "these are no longer unreachable — they have a caller now, or they are \
         gone. Remove their entries so the registry keeps meaning something:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn no_unreachable_api_every_entry_says_something() {
    // "Tracked in REVL-216" is a legitimate entry. A blank one is a box ticked,
    // and a box ticked is what this file is written against.
    for (name, reason) in ACCOUNTED_FOR {
        assert!(
            reason.len() > 40,
            "{name}'s entry does not say why it has no caller: {reason:?}"
        );
    }
}

#[test]
fn no_unreachable_api_the_detector_still_detects() {
    // The guard's own guard. If `public_fns` stops matching — a rustfmt change, a
    // new `pub(crate)` spelling — every one of the checks above passes while
    // reading nothing, which is the failure this whole file is about.
    let sample = r"
        /// Doc.
        pub fn plain_one(a: u32) -> u32 { a }
        pub async fn async_one() {}
        pub const fn const_one() -> u32 { 1 }
        fn private_one() {}
        pub struct NotAFn;
    ";
    let found = public_fns(sample);

    assert!(found.contains(&"plain_one".to_owned()), "{found:?}");
    assert!(found.contains(&"async_one".to_owned()), "{found:?}");
    assert!(found.contains(&"const_one".to_owned()), "{found:?}");
    assert!(!found.contains(&"private_one".to_owned()), "{found:?}");
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn no_unreachable_api_a_test_only_caller_counts_as_a_caller() {
    // Deliberate: a function only a test calls is covered by something, which is
    // a different situation from one nothing names at all. Asserted because the
    // opposite choice is tempting and would bury the real signal under helpers.
    let root = workspace_root();
    let live = unreachable_fns(&root);

    // `group_held_for_test` exists solely so a test can reach a private helper.
    assert!(
        !live.iter().any(|name| name == "group_held_for_test"),
        "a function a test calls must not be reported as unreachable"
    );
}
