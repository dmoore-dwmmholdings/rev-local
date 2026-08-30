//! The standing no-silent-caps audit (RL-1302, SPEC §18).
//!
//! §18: every place the system truncates, samples, caps or drops must record it
//! and surface it. The rule is easy to hold while writing the code that caps and
//! easy to lose in the next one, so this is a *standing* check rather than a
//! one-off review: it finds cap sites in the source and fails on any that is not
//! accounted for.
//!
//! # Why a registry rather than a cleverer detector
//!
//! No pattern can tell "this drops user data" from "this bounds a retry delay".
//! A detector that tried would either miss real caps or cry wolf until somebody
//! deleted it. So the detector is deliberately broad and the *judgement* is
//! written down: each file that caps carries an entry saying where the cap is
//! recorded, or why it is not a cap at all.
//!
//! The failure mode this is built for is the new one. Add a file with a `MAX_`, a
//! `.take(n)` or a `.truncate()`, and this test fails until somebody says what
//! happens to what was cut. That is the whole mechanism — it does not check that
//! existing entries are still true, it checks that nobody added a cap quietly.

use std::path::{Path, PathBuf};

/// Every file that caps, samples, truncates or drops — and where that is recorded.
///
/// An entry is a claim somebody made and can be checked against. "Not a cap" is a
/// legitimate entry; a *blank* one is not, which is why the test rejects those.
const ACCOUNTED_FOR: &[(&str, &str)] = &[
    (
        "crates/revlocal-cli/src/backfill.rs",
        "ENUMERATION_CAP bounds how much history `backfill` reads before --limit \
         is applied. This one genuinely can hide changes, so the report carries \
         `truncated_enumeration`: when the cap is hit, the excluded count is \
         printed as a lower bound (\"at least — enumeration stopped at 10000\") \
         rather than as a total. §18 one level up, since the field being hedged is \
         itself the one that exists to report a cap.",
    ),
    (
        "crates/revlocal-tauri/src/bin/revlocal-desktop.rs",
        "MAX_BYTES bounds how much of a transcript the run detail screen reads. \
         An engine writes whatever it likes, and a gigabyte of progress bars \
         should not be able to exhaust this process through a UI control. When \
         the bound is hit the returned text *begins* with a line saying the real \
         size and how much is shown — in the text itself, because the screen \
         renders it verbatim and a silently clipped log looks like a short one. \
         The tail is kept rather than the head: the end of a log is where the \
         failure is.",
    ),
    (
        "crates/revlocal-cli/src/export.rs",
        "EXPORT_RUN_CAP bounds how many runs `db export` reads. It genuinely can \
         hide history, so the document carries `truncated` and the summary line \
         says \"stopped at N runs, so this is not the whole history\" — an export \
         that quietly ended early would present a partial record as a complete \
         one, which is the same failure as a partial review looking whole.",
    ),
    (
        "crates/revlocal-core/src/finding.rs",
        "TITLE_MAX_CHARS is enforced at the schema boundary (result.v1.json, \
         maxLength 80), not by truncating here — an over-long title is a rejected \
         finding recorded as a DroppedFinding, never a quietly shortened one.",
    ),
    (
        "crates/revlocal-core/src/fingerprint.rs",
        "Truncates the digest to FINGERPRINT_HEX_LEN. Not user data: the \
         fingerprint is defined as that many hex characters, so there is nothing \
         cut that anybody could have wanted.",
    ),
    (
        "crates/revlocal-daemon/src/budgets.rs",
        "DEFAULT_MAX_CONCURRENT_RUNS bounds how many runs execute at once, not how \
         many happen. Excess runs wait on the semaphore; none is dropped.",
    ),
    (
        "crates/revlocal-daemon/src/depth.rs",
        "MAX_DEEP_LINES downgrades a review's depth rather than skipping it, and \
         records DepthReason::TooManyLines { lines, limit } — the number that \
         triggered it and the limit it exceeded.",
    ),
    (
        "crates/revlocal-daemon/src/poll.rs",
        "MAX_BACKOFF_SECS is a ceiling on a delay, not on data. Nothing is \
         dropped; the repository reports health `degraded` with its failure count \
         and last error while it backs off.",
    ),
    (
        "crates/revlocal-daemon/src/state_machine.rs",
        "DEFAULT_MAX_ATTEMPTS stops recovery re-enqueueing forever. Giving up \
         emits RunEvent::GivenUp { reason } — §18's point exactly, since a change \
         that stops being reviewed with no record is indistinguishable from one \
         reviewed and found clean.",
    ),
    (
        "crates/revlocal-daemon/src/truncation.rs",
        "The primary cap site (§9.4). Records run.truncated and run.omitted_files, \
         and the prompt names every omitted file rather than reporting a count.",
    ),
    (
        "crates/revlocal-daemon/src/webhook.rs",
        "DELIVERY_MEMORY evicts old delivery ids. Eviction causes a redelivery to \
         be reviewed a second time, not to be dropped — and that second review is \
         itself a recorded run. The cache is bounded because it is fed by a network \
         endpoint.",
    ),
    (
        "crates/revlocal-engine/src/supervise.rs",
        "GRACE, CANCEL_GRACE and DRAIN_GRACE bound how long a killed engine's \
         output is collected, so output can genuinely be lost. Recorded as \
         run.killed with its KillReason, and the run is marked degraded so §8.2's \
         salvage ladder and §12.3's risk escalation both see it. \
         EXIT_DRAIN_GRACE bounds the same collection for an engine that was NOT \
         killed — a parent can exit cleanly while a child it spawned keeps the \
         pipe open — and `run.killed` says nothing about that case, since it is \
         None. That one is recorded as Supervised::output_truncated, read through \
         output_is_complete(), because \"it finished\" and \"we read all of it\" \
         are separate facts and only one is about the exit code.",
    ),
    (
        "crates/revlocal-mcp/src/stdio.rs",
        "SHUTDOWN_GRACE bounds how long a subprocess is given to exit. It caps a \
         teardown, not a payload; no review data passes through it.",
    ),
    (
        "crates/revlocal-vcs/src/generated.rs",
        "HEADER_LINES and HEADER_BYTES bound how much of a file is read looking for \
         a generated-file marker. Nothing reviewable is cut: a file whose marker \
         sits below the header is treated as NOT generated, so it is reviewed \
         rather than skipped. The cap can only cost tokens, never coverage — which \
         is the direction a detection heuristic should fail in.",
    ),
    (
        "crates/revlocal-publish/src/retry.rs",
        "MAX_ATTEMPTS and MAX_DELAY bound retrying a publish. Exhaustion is \
         recorded on the publish_action row with its error, and §11.6's report \
         lists it as replayable rather than dropping it.",
    ),
    (
        "crates/revlocal-publish/src/trama.rs",
        "DEFAULT_INDEX_LIMIT shows the most recent reviews. The page states \
         'Showing the N most recent reviews (X of Y recorded)' — an index silently \
         showing 50 of 700 reads as 'there have been 50 reviews'.",
    ),
];

/// Walk a crate's `src`, returning files containing a cap-shaped line.
fn cap_sites(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    let Ok(crates) = std::fs::read_dir(root.join("crates")) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = crates
        .flatten()
        .map(|entry| entry.path().join("src"))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        walk(&dir, &mut found);
    }
    found.sort();
    found
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, found);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.lines().any(is_cap_shaped) {
            found.push(path);
        }
    }
}

/// Whether a line looks like a cap.
///
/// Deliberately broad — see the module docs. Two deliberate narrowings, both
/// because the false positives were noise rather than judgement calls:
/// `.take()` with no argument is `Option::take`, and prose about a cap is not one.
fn is_cap_shaped(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or(line);

    if code.contains(".truncate(") {
        return true;
    }
    // `.take(` followed by anything but `)` — so `Option::take()` does not match.
    if let Some(rest) = code.split(".take(").nth(1) {
        if !rest.trim_start().starts_with(')') {
            return true;
        }
    }
    code.split_whitespace()
        .skip_while(|word| *word != "const")
        .nth(1)
        .is_some_and(|name| {
            let name = name.trim_end_matches(':');
            ["MAX", "LIMIT", "CAP", "MEMORY", "GRACE"]
                .iter()
                .any(|marker| name.contains(marker))
        })
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

#[test]
fn no_silent_caps_every_cap_site_is_accounted_for() {
    // The check that earns this file's keep: a new cap fails the suite until
    // somebody writes down what happens to what was cut.
    let root = workspace_root();
    let sites = cap_sites(&root);

    let unaccounted: Vec<String> = sites
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            ACCOUNTED_FOR
                .iter()
                .all(|(known, _)| *known != relative)
                .then_some(relative)
        })
        .collect();

    assert!(
        unaccounted.is_empty(),
        "SPEC §18: these files cap, sample, truncate or drop and are not accounted \
         for. Add an entry to ACCOUNTED_FOR in this file saying where the cap is \
         recorded — or why it is not a cap:\n  {}",
        unaccounted.join("\n  ")
    );
}

#[test]
fn no_silent_caps_the_registry_has_no_stale_entries() {
    // A registry nobody prunes is a registry nobody trusts. An entry for a file
    // that no longer caps is a claim that has quietly stopped being checked.
    let root = workspace_root();
    let sites: Vec<String> = cap_sites(&root)
        .iter()
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    let stale: Vec<&str> = ACCOUNTED_FOR
        .iter()
        .map(|(path, _)| *path)
        .filter(|path| !sites.iter().any(|site| site == path))
        .collect();

    assert!(
        stale.is_empty(),
        "these files no longer contain a cap; remove their ACCOUNTED_FOR entries \
         so the registry keeps meaning something:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn no_silent_caps_every_entry_says_something() {
    // "Not a cap" is a legitimate entry. A blank one is a box ticked, and a box
    // ticked is exactly what §18 is written against.
    for (path, justification) in ACCOUNTED_FOR {
        assert!(
            justification.len() > 40,
            "{path}'s entry does not say where the cap is recorded: {justification:?}"
        );
        assert!(
            !justification.trim().is_empty(),
            "{path} has an empty justification"
        );
    }
}

#[test]
fn no_silent_caps_the_detector_still_detects() {
    // A guard whose detector has quietly stopped matching passes forever while
    // checking nothing — the failure mode this whole file exists to prevent,
    // reappearing one level up.
    assert!(is_cap_shaped("pub const MAX_DEEP_LINES: u64 = 20_000;"));
    assert!(is_cap_shaped("const DELIVERY_MEMORY: usize = 4096;"));
    assert!(is_cap_shaped("    hex.truncate(FINGERPRINT_HEX_LEN);"));
    assert!(is_cap_shaped("    entries.iter().take(limit)"));

    // And still ignores what it was narrowed against.
    assert!(
        !is_cap_shaped("    let Some(stdin) = child.stdin.take() else {"),
        "Option::take is not a cap"
    );
    assert!(
        !is_cap_shaped("// MAX_DEEP_LINES caps the deep review"),
        "prose about a cap is not one"
    );
    assert!(!is_cap_shaped("    let total = entries.len();"));
}

#[test]
fn no_silent_caps_finds_the_site_it_was_written_for() {
    // §9.4's diff truncation is the cap this rule was written for. If the detector
    // ever stops seeing it, the registry above is decoration.
    let sites: Vec<String> = cap_sites(&workspace_root())
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();

    assert!(
        sites
            .iter()
            .any(|s| s.ends_with("daemon/src/truncation.rs")),
        "the diff truncation site was not detected; sites were:\n  {}",
        sites.join("\n  ")
    );
}

/// A registered file's entry must name every cap constant that file now has.
///
/// The audit's doc comment says plainly that it "does not check that existing
/// entries are still true, it checks that nobody added a cap quietly". That was a
/// deliberate limit and it has now cost something: `EXIT_DRAIN_GRACE` was added to
/// `supervise.rs`, a file already in the registry, so nothing fired — and the
/// entry went on describing three constants and a recording mechanism
/// (`run.killed`) that does not apply to the fourth. A run that was never killed
/// has `killed = None`.
///
/// This closes the cheap half of that gap. It cannot tell whether an entry's
/// *reasoning* is still sound, but it can tell when a constant appeared that
/// nobody wrote down — and "a new cap in an old file" is the case that slips
/// through, because adding to a file that already has an entry feels like nothing
/// new has happened.
#[test]
fn no_silent_caps_every_named_constant_appears_in_its_entry() {
    let root = workspace_root();
    let mut missing: Vec<String> = Vec::new();

    for (file, entry) in ACCOUNTED_FOR {
        let path = root.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            // A stale path is `no_silent_caps_the_registry_has_no_stale_entries`'
            // job, not this one.
            continue;
        };

        for line in text.lines() {
            if !is_cap_shaped(line) {
                continue;
            }
            // Only named constants; `.take(` and `.truncate(` have no name to
            // look for and are covered by the entry as a whole.
            let code = line.split("//").next().unwrap_or(line);
            let Some(name) = code
                .split_whitespace()
                .skip_while(|word| *word != "const")
                .nth(1)
                .map(|name| name.trim_end_matches(':'))
            else {
                continue;
            };

            if !entry.contains(name) {
                missing.push(format!("{file}: {name}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "these cap constants exist in files the registry already covers, and the \
         entry does not name them:\n  {}\n\
         Adding a cap to a file that already has an entry is the case that slips \
         through: nothing about it looks new.",
        missing.join("\n  ")
    );
}
