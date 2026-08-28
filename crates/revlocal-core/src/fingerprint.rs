//! Stable, line-number-independent finding fingerprints (SPEC §10.3).
//!
//! The fingerprint is what makes dedupe work across runs. The same defect found
//! again after a rebase — same file, same claim, different line — must produce the
//! same value, or every rebase re-files every finding.
//!
//! ```text
//! fingerprint = sha256(
//!     repo.name || '\x00' ||
//!     normalize_path(file) || '\x00' ||
//!     category || '\x00' ||
//!     normalized_title
//! )[0..16]
//! ```
//!
//! Line numbers are deliberately absent from the input. So is severity: an engine
//! that rates the same defect `high` on one run and `medium` on the next has not
//! found a different defect.

use crate::Category;
use sha2::{Digest, Sha256};

/// Number of hex characters kept from the digest (SPEC §10.3, `[0..16]`).
///
/// 16 hex characters is 64 bits. Collisions are a dedupe nuisance, not a security
/// boundary — a collision merges two findings, it does not authorise anything —
/// so a truncated digest is the right trade for something that appears in issue
/// bodies and log lines.
pub const FINGERPRINT_HEX_LEN: usize = 16;

/// Shortest token treated as an identifier rather than digit-masked.
///
/// SPEC §10.3: "identifiers longer than 3 chars kept verbatim".
const MIN_IDENTIFIER_LEN: usize = 4;

/// The separator between fingerprint fields.
///
/// A NUL, so no field's content can spoof a boundary: without it, repo `"a"` with
/// file `"b/c"` and repo `"a/b"` with file `"c"` would hash identically.
const FIELD_SEPARATOR: u8 = 0;

/// Normalize a repository-relative path so platform spelling cannot split a
/// fingerprint.
///
/// The same file is reported as `src\engine\mod.rs` by a Windows-side engine and
/// `src/engine/mod.rs` elsewhere, and both must dedupe together. Also strips `./`
/// prefixes, collapses repeated separators, and drops a leading `/` — a
/// repo-relative path with one is the same file as one without.
///
/// Case is **not** folded: POSIX paths are case-sensitive, and `Readme.md` and
/// `README.md` can be two files.
pub fn normalize_path(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let mut out = String::with_capacity(unified.len());

    for segment in unified.split('/') {
        // Skips empty segments (repeated separators, and a leading `/`) and `.`.
        if segment.is_empty() || segment == "." {
            continue;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(segment);
    }
    out
}

/// Whether `token` is an identifier that should survive digit masking.
///
/// The carve-out exists so names like `sha256` and `utf8` are not mangled into
/// `sha###` and `utf#`, which would make two reports of the same defect hash
/// differently depending on how the engine phrased it.
///
/// Leading and trailing punctuation is ignored when deciding, so `parse_header,`
/// is judged on `parse_header`.
fn is_identifier(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');

    core.chars().count() >= MIN_IDENTIFIER_LEN
        && core
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && core.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Normalize a finding title (SPEC §10.3).
///
/// Lowercased, whitespace-collapsed, and digits replaced with `#` — except inside
/// identifiers longer than three characters, which are kept verbatim.
///
/// Masking bare numbers is what makes the fingerprint survive the incidental
/// detail an engine puts in a title: "buffer overflow at offset 4128" and the same
/// claim at offset 8320 are one defect, not two.
///
/// # Examples
///
/// ```
/// use revlocal_core::normalize_title;
///
/// // A bare number varies between reports of the same defect, so it is masked.
/// assert_eq!(
///     normalize_title("Buffer  overflow at offset 4128"),
///     "buffer overflow at offset ####"
/// );
///
/// // An identifier is not, even though it contains digits.
/// assert_eq!(normalize_title("sha256 digest truncated"), "sha256 digest truncated");
/// ```
pub fn normalize_title(title: &str) -> String {
    let lowered = title.to_lowercase();

    lowered
        .split_whitespace()
        .map(|token| {
            if is_identifier(token) {
                token.to_owned()
            } else {
                token
                    .chars()
                    .map(|c| if c.is_ascii_digit() { '#' } else { c })
                    .collect()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compute a finding's fingerprint (SPEC §10.3).
///
/// `file` is optional because a finding need not be file-scoped; a repo-wide
/// observation hashes with an empty path, which keeps all such findings in one
/// dedupe space rather than giving each a unique one.
pub fn fingerprint(repo_name: &str, file: Option<&str>, category: Category, title: &str) -> String {
    let mut hasher = Sha256::new();

    hasher.update(repo_name.as_bytes());
    hasher.update([FIELD_SEPARATOR]);
    hasher.update(normalize_path(file.unwrap_or_default()).as_bytes());
    hasher.update([FIELD_SEPARATOR]);
    hasher.update(category.as_str().as_bytes());
    hasher.update([FIELD_SEPARATOR]);
    hasher.update(normalize_title(title).as_bytes());

    let digest = hasher.finalize();

    // Two hex chars per byte, so only the first half of the needed bytes are
    // formatted rather than the whole digest.
    let mut hex = String::with_capacity(FINGERPRINT_HEX_LEN);
    for byte in digest.iter().take(FINGERPRINT_HEX_LEN.div_ceil(2)) {
        use std::fmt::Write as _;
        // Writing to a String is infallible; the result is discarded deliberately.
        let _ = write!(hex, "{byte:02x}");
    }
    hex.truncate(FINGERPRINT_HEX_LEN);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "rev-local";

    #[test]
    fn the_same_defect_at_a_different_line_has_the_same_fingerprint() {
        // The whole point of §10.3. Line numbers are not an input, so a rebase that
        // moves a defect cannot re-file it.
        let before = fingerprint(
            REPO,
            Some("crates/revlocal-core/src/run.rs"),
            Category::Correctness,
            "Budget total treats an unknown cost as zero",
        );
        let after = fingerprint(
            REPO,
            Some("crates/revlocal-core/src/run.rs"),
            Category::Correctness,
            "Budget total treats an unknown cost as zero",
        );
        assert_eq!(before, after);
    }

    #[test]
    fn the_same_title_in_a_different_file_has_a_different_fingerprint() {
        let here = fingerprint(
            REPO,
            Some("src/a.rs"),
            Category::Security,
            "Unvalidated input",
        );
        let there = fingerprint(
            REPO,
            Some("src/b.rs"),
            Category::Security,
            "Unvalidated input",
        );
        assert_ne!(here, there);
    }

    #[test]
    fn windows_and_posix_spellings_of_a_path_agree() {
        // A Windows-side engine reports backslashes. The same file must dedupe with
        // what a Linux-side engine reported.
        let windows = fingerprint(
            REPO,
            Some(r"crates\revlocal-core\src\risk.rs"),
            Category::Correctness,
            "Escalation not recorded",
        );
        let posix = fingerprint(
            REPO,
            Some("crates/revlocal-core/src/risk.rs"),
            Category::Correctness,
            "Escalation not recorded",
        );
        assert_eq!(windows, posix);
    }

    #[test]
    fn incidental_path_spelling_does_not_split_a_fingerprint() {
        let canonical = normalize_path("src/engine/mod.rs");
        for spelling in [
            r"src\engine\mod.rs",
            "./src/engine/mod.rs",
            "src//engine///mod.rs",
            "/src/engine/mod.rs",
            r".\src\engine\mod.rs",
        ] {
            assert_eq!(
                normalize_path(spelling),
                canonical,
                "{spelling:?} must normalize to {canonical:?}"
            );
        }
    }

    #[test]
    fn path_normalization_does_not_fold_case() {
        // POSIX filesystems are case-sensitive; Readme.md and README.md can be two
        // different files, and merging them would hide a finding.
        assert_ne!(normalize_path("Readme.md"), normalize_path("README.md"));
    }

    #[test]
    fn bare_numbers_are_masked_so_incidental_offsets_dedupe() {
        let low = fingerprint(
            REPO,
            Some("src/buf.rs"),
            Category::Security,
            "Overflow at offset 4128",
        );
        let high = fingerprint(
            REPO,
            Some("src/buf.rs"),
            Category::Security,
            "Overflow at offset 8320",
        );
        assert_eq!(
            low, high,
            "the same defect reported with a different offset is one defect"
        );
    }

    #[test]
    fn identifiers_longer_than_three_chars_survive_digit_masking() {
        // Otherwise `sha256` becomes `sha###` and two phrasings of one defect split.
        assert_eq!(normalize_title("SHA256 digest"), "sha256 digest");
        assert_eq!(normalize_title("utf8 decode fails"), "utf8 decode fails");
        assert_eq!(
            normalize_title("parse_header, then fail"),
            "parse_header, then fail"
        );

        // ...but a short token is not an identifier, and a token that does not start
        // with a letter is a number however long it is.
        assert_eq!(normalize_title("v1 shipped"), "v# shipped");
        assert_eq!(normalize_title("0x1f is wrong"), "#x#f is wrong");
    }

    #[test]
    fn titles_are_lowercased_and_whitespace_is_collapsed() {
        assert_eq!(
            normalize_title("  Buffer   overflow\tin\nparse  "),
            "buffer overflow in parse"
        );
        assert_eq!(
            fingerprint(REPO, Some("a.rs"), Category::Perf, "Slow  Loop"),
            fingerprint(REPO, Some("a.rs"), Category::Perf, "slow loop"),
        );
    }

    #[test]
    fn category_is_part_of_the_identity() {
        // The same sentence about the same file can be a correctness claim or a
        // convention claim; they are different findings.
        let correctness = fingerprint(REPO, Some("a.rs"), Category::Correctness, "Unchecked cast");
        let convention = fingerprint(REPO, Some("a.rs"), Category::Convention, "Unchecked cast");
        assert_ne!(correctness, convention);
    }

    #[test]
    fn repo_name_is_part_of_the_identity() {
        let here = fingerprint("alpha", Some("a.rs"), Category::Tests, "No coverage");
        let there = fingerprint("beta", Some("a.rs"), Category::Tests, "No coverage");
        assert_ne!(here, there);
    }

    #[test]
    fn field_boundaries_cannot_be_spoofed_by_field_content() {
        // Without a separator, ("a", "b/c") and ("a/b", "c") would hash the same
        // bytes and silently dedupe two unrelated findings together.
        let one = fingerprint("a", Some("b/c.rs"), Category::Other, "t");
        let two = fingerprint("a/b", Some("c.rs"), Category::Other, "t");
        assert_ne!(one, two);
    }

    #[test]
    fn a_finding_with_no_file_still_fingerprints() {
        let repo_wide = fingerprint(REPO, None, Category::Convention, "No CI for this repo");
        assert_eq!(repo_wide.len(), FINGERPRINT_HEX_LEN);
        assert_eq!(
            repo_wide,
            fingerprint(REPO, Some(""), Category::Convention, "No CI for this repo"),
            "an absent file and an empty file must not be two dedupe spaces"
        );
    }

    #[test]
    fn a_fingerprint_is_sixteen_lowercase_hex_characters() {
        let fp = fingerprint(REPO, Some("src/a.rs"), Category::Correctness, "Something");
        assert_eq!(fp.len(), FINGERPRINT_HEX_LEN);
        assert!(
            fp.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "fingerprints appear in issue trailers and log lines: {fp}"
        );
    }

    #[test]
    fn golden_vectors_pin_the_algorithm() {
        // Cross-checked against an independent implementation of the §10.3 formula
        // written from the spec text, not read off this code — so these pin the
        // algorithm to the spec rather than to whatever this function happens to do.
        //
        // If a change to normalization alters any of these, dedupe silently breaks
        // for every finding already stored. Regenerating them is a data migration:
        // say so in an ADR and plan the re-fingerprinting, do not just re-run this.
        let vectors: [(&str, Option<&str>, Category, &str, &str); 6] = [
            (
                "rev-local",
                Some("src/main.rs"),
                Category::Correctness,
                "Off by one",
                "3861781a40781c64",
            ),
            (
                "rev-local",
                Some(r"src\main.rs"),
                Category::Correctness,
                "Off by one",
                "3861781a40781c64",
            ),
            (
                "rev-local",
                Some("src/main.rs"),
                Category::Security,
                "Off by one",
                "934afe66dccef641",
            ),
            (
                "rev-local",
                None,
                Category::Convention,
                "No CI",
                "34a9c5e4f54ff2c8",
            ),
            (
                "rev-local",
                Some("a.rs"),
                Category::Perf,
                "Slow at line 42",
                "515252fc0b6d8db5",
            ),
            (
                "rev-local",
                Some("a.rs"),
                Category::Perf,
                "SHA256 rehashed",
                "86dbbcdbe3e07c3f",
            ),
        ];

        for (repo, file, category, title, expected) in vectors {
            assert_eq!(
                fingerprint(repo, file, category, title),
                expected,
                "golden vector drift for ({repo:?}, {file:?}, {category}, {title:?})"
            );
        }
    }
}
