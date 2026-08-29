//! The error taxonomy (RL-1301, SPEC §18).
//!
//! §18 asks that a user-visible error say what to do, not only what happened.
//! "could not parse config.toml" is a diagnosis; a line telling somebody where to
//! look is a remedy, and the difference is whether they have to go and search.
//!
//! # What counts as user-visible
//!
//! The surface a person actually reads: `revlocal`'s command errors, and the
//! errors the desktop UI renders. A `StoreError` wrapped and re-emitted with
//! context is judged where it surfaces, not where it originates — which is why
//! `#[error(transparent)]` variants are not required to carry their own
//! remediation. They delegate, and the thing they delegate to is checked.
//!
//! # Why some variants correctly have none
//!
//! Not every failure is actionable. `NoSuchRun { run_id }` in a UI is a stale
//! window, not something a user can fix by doing anything differently, and
//! inventing a remedy for it would be noise that teaches people to skip the line
//! where the real remedies live. So an exemption is allowed — and has to be
//! written down, because "no remedy" and "nobody thought about it" look identical
//! in the source.

mod errors {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// Files whose error enums a person reads directly.
    const USER_VISIBLE: &[&str] = &[
        "crates/revlocal-cli/src/publish.rs",
        "crates/revlocal-cli/src/repo.rs",
        "crates/revlocal-cli/src/review.rs",
        "crates/revlocal-cli/src/targets.rs",
        "crates/revlocal-tauri/src/ipc.rs",
    ];

    /// Variants that carry no `try:` line, and why that is right.
    ///
    /// Keyed by **(file, variant)**, not by variant name. Keying on the name alone
    /// was the first shape and it was a loophole: `NoSuchRepo` exists on both
    /// `IpcError` and `RepoCommandError`, so exempting the UI's stale-window case
    /// silently exempted the CLI's — which *does* have a remedy and would have gone
    /// unchecked if it ever lost one. A mutation test caught it, which is the only
    /// reason it is not still there.
    ///
    /// An entry is a claim somebody made. A blank one is not allowed, for the same
    /// reason the no-silent-caps registry rejects blanks.
    const NO_REMEDY_IS_CORRECT: &[(&str, &str, &str)] = &[
        (
            "crates/revlocal-cli/src/repo.rs",
            "Unrenderable",
            "The report could not be serialised. That is a bug in rev-local, not \
             something the user configured — telling them to try something would be \
             blaming them for it.",
        ),
        (
            "crates/revlocal-cli/src/review.rs",
            "Json",
            "Same: a serialisation failure on our own output. Nothing the user did \
             causes it and nothing they can do fixes it.",
        ),
        (
            "crates/revlocal-cli/src/targets.rs",
            "Discovery",
            "Wraps a DiscoveryError, which carries its own remediation naming the \
             server that failed. A second, vaguer suggestion here would bury it.",
        ),
        (
            "crates/revlocal-tauri/src/ipc.rs",
            "DaemonUnavailable",
            "Carries remediation in a field rather than the message, because the UI \
             renders it as a button rather than a sentence. IpcError::remediation() \
             returns it.",
        ),
        (
            "crates/revlocal-tauri/src/ipc.rs",
            "Store",
            "Carries remediation in a field, for the same reason as \
             DaemonUnavailable.",
        ),
        (
            "crates/revlocal-tauri/src/ipc.rs",
            "NoSuchRepo",
            "A stale window naming a repository that has since been removed. There \
             is no action the user can take, and IpcError::is_retryable says so.",
        ),
        (
            "crates/revlocal-tauri/src/ipc.rs",
            "NoSuchRun",
            "Same as NoSuchRepo: a stale window, not a misconfiguration.",
        ),
    ];

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Every `#[error(...)]` attribute in a file, with the variant it precedes.
    fn variants(path: &Path) -> Vec<(String, String)> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };

        let mut found = Vec::new();
        let mut attribute: Option<String> = None;
        let mut depth = 0_i32;

        for line in text.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("#[error(") || attribute.is_some() && depth > 0 {
                let held = attribute.get_or_insert_with(String::new);
                held.push_str(trimmed);
                depth += trimmed.matches('(').count() as i32;
                depth -= trimmed.matches(')').count() as i32;
                if depth <= 0 {
                    depth = 0;
                }
                continue;
            }

            if let Some(message) = attribute.take() {
                // The identifier on the line after the attribute is the variant.
                let name: String = trimmed
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    found.push((name, message));
                }
            }
        }
        found
    }

    #[test]
    fn every_user_visible_error_says_what_to_do_or_says_why_not() {
        // The criterion, and the reason it is a test rather than a review: a new
        // variant added without a remedy is invisible in a diff and obvious here.
        let root = workspace_root();
        let exempt: BTreeSet<(&str, &str)> = NO_REMEDY_IS_CORRECT
            .iter()
            .map(|(file, variant, _)| (*file, *variant))
            .collect();

        let mut offenders = Vec::new();
        let mut checked = 0;

        for relative in USER_VISIBLE {
            for (variant, message) in variants(&root.join(relative)) {
                // A transparent variant delegates; the thing it delegates to is
                // checked where it surfaces.
                if message.contains("transparent") {
                    continue;
                }
                checked += 1;
                if message.contains("try:") || exempt.contains(&(*relative, variant.as_str())) {
                    continue;
                }
                offenders.push(format!("{relative}: {variant}"));
            }
        }

        assert!(
            checked > 10,
            "the scanner found only {checked} variants; it has stopped matching"
        );
        assert!(
            offenders.is_empty(),
            "SPEC §18: a user-visible error should say what to do. Add a `try:` line, \
             or add the variant to NO_REMEDY_IS_CORRECT saying why none is right:\n  {}",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn every_exemption_gives_a_reason() {
        // "No remedy" and "nobody thought about it" look identical in the source.
        // This is what makes them different.
        for (file, variant, reason) in NO_REMEDY_IS_CORRECT {
            assert!(
                reason.len() > 40,
                "{file}'s {variant} exemption does not explain itself: {reason:?}"
            );
        }
    }

    #[test]
    fn no_library_crate_exposes_anyhow() {
        // §18's other half, and the reason this project has one thiserror enum per
        // crate: `anyhow` in a public API erases the variant a caller would branch
        // on, so a UI cannot tell "the daemon is down" from "no such repository"
        // and cannot offer a retry button for one and not the other.
        let root = workspace_root();
        let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
            panic!("the crates directory must be readable");
        };

        let mut offenders = Vec::new();
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            // Only the dependency section matters; a comment mentioning anyhow is
            // not a dependency on it.
            for line in text.lines() {
                let code = line.split('#').next().unwrap_or(line);
                if code.trim_start().starts_with("anyhow") {
                    offenders.push(manifest.display().to_string());
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "anyhow is not a library dependency in this workspace — errors cross \
             crate boundaries as named variants so callers can branch on them:\n  {}",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn the_remediation_that_lives_in_a_field_is_actually_reachable() {
        // Two IpcError variants carry remediation in a field rather than the
        // message, and are exempt above on that basis. If the accessor stopped
        // returning it, the exemption would be a loophole rather than a design.
        use revlocal_tauri::IpcError;

        let unavailable = IpcError::DaemonUnavailable {
            remediation: "start rev-local".to_owned(),
        };
        assert_eq!(unavailable.remediation(), Some("start rev-local"));
        assert!(unavailable.is_retryable());

        // And the ones exempt as "not actionable" really offer nothing, rather
        // than offering an empty string that reads as a remedy in a UI.
        let stale = IpcError::NoSuchRun { run_id: 7 };
        assert_eq!(stale.remediation(), None);
        assert!(!stale.is_retryable());
    }
}
