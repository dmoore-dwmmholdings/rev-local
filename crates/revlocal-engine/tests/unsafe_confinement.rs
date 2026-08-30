//! `unsafe` lives in exactly one file (RL-1303, ADR 0003).
//!
//! The workspace lint was `forbid(unsafe_code)` until SPEC §8.5's Job Object,
//! which has no safe expression: Windows offers no process-group kill, so reaping
//! an engine's grandchildren means calling Win32 directly. `forbid` cannot be
//! relaxed anywhere, so the workspace moved to `deny` and one module allows it.
//!
//! That trade is only worth making if the exemption stays one. A lint set to
//! `deny` is one `#[allow]` away from being off wherever somebody wants it, and
//! the next person to want it will have a good reason too.
//!
//! So the property is asserted here instead, and it is a stronger one than the
//! lint made: it is about every file in the workspace at once, and it names the
//! file allowed to differ.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path
}

/// Every `.rs` file in the workspace's crates.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            // `target` is build output and `node_modules` is not ours.
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if name == "target" || name == "node_modules" {
                continue;
            }
            sources(&path, found)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    Ok(())
}

#[test]
fn unsafe_is_confined_to_the_job_object() -> Result<(), String> {
    let root = workspace_root();
    let mut files = Vec::new();
    sources(&root.join("crates"), &mut files)?;

    assert!(
        files.len() > 50,
        "only {} source files found; the walk has stopped working, and a walk \
         that finds nothing passes this test for the wrong reason",
        files.len()
    );

    let mut allowing = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("reading {}: {e}", file.display()))?;
        // The attribute, not the words. Matching the bare string would flag this
        // very file, which discusses the attribute without carrying it — and
        // excluding this file by name would leave somewhere to hide one.
        //
        // Any ordering inside the attribute counts: `#[allow(dead_code,
        // unsafe_code)]` turns the lint off just as thoroughly.
        let carries = text.lines().any(|line| {
            let line = line.trim_start();
            (line.starts_with("#[allow(") || line.starts_with("#![allow("))
                && line.contains("unsafe_code")
        });
        if carries {
            // Normalised to `/` before comparing. `Path::display` uses the
            // platform separator, so the expected value below matched on Unix
            // and failed on Windows with `crates\revlocal-engine\src\job.rs` —
            // a path-separator bug in the test guarding the Windows-only code,
            // and exactly the class REVL-106 exists to catch.
            allowing.push(
                file.strip_prefix(&root)
                    .unwrap_or(file)
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }

    assert_eq!(
        allowing,
        vec!["crates/revlocal-engine/src/job.rs".to_owned()],
        "`unsafe` is allowed somewhere new. SPEC §8.5's Job Object is the only \
         exemption the workspace has agreed to; anything else needs its own \
         argument, not an attribute"
    );
    Ok(())
}

#[test]
fn every_unsafe_block_in_it_says_why_it_is_sound() -> Result<(), String> {
    // An `unsafe` block with no stated invariant cannot be reviewed — the reader
    // has to reconstruct the argument the author did not write down. This is the
    // one file where that matters most, since it is the only one that can have
    // them at all.
    let path = workspace_root().join("crates/revlocal-engine/src/job.rs");
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;

    let lines: Vec<&str> = text.lines().collect();
    let mut unexplained = Vec::new();

    for (n, line) in lines.iter().enumerate() {
        if !line.contains("unsafe {") {
            continue;
        }
        // A SAFETY note in the ten lines above it, which is where the convention
        // puts it and far enough to allow for a wrapped sentence.
        let start = n.saturating_sub(10);
        let explained = lines[start..n]
            .iter()
            .any(|above| above.contains("SAFETY:"));
        if !explained {
            unexplained.push(format!("{}:{}", path.display(), n + 1));
        }
    }

    assert!(
        unexplained.is_empty(),
        "these `unsafe` blocks have no SAFETY comment: {unexplained:?}"
    );

    // And it does have some, so the test is not passing because it found nothing.
    assert!(
        text.matches("unsafe {").count() >= 4,
        "expected the Win32 calls to still be here; found {}",
        text.matches("unsafe {").count()
    );
    Ok(())
}
