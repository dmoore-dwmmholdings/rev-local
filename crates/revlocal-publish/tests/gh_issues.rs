//! Findings filed as GitHub issues (RL-1510, SPEC §11.3, §11.6).
//!
//! Same hermetic approach as `gh_writer.rs`: a shell script stands in for `gh`,
//! so there is no network, no token, and the awkward cases can be produced on
//! demand.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_publish::github::GitHubWriter;
use revlocal_publish::{find_own_issue, github_slug, GhWriter, GitHubIssue};
use tempfile::TempDir;

fn fake_gh(dir: &TempDir, stdout: &str, code: i32) -> Result<String, String> {
    let path = dir.path().join("gh");
    let script = format!(
        "#!/bin/sh\ncat > /dev/null\nprintf '%s' '{}'\nexit {code}\n",
        stdout.replace('\'', r"'\''")
    );
    std::fs::write(&path, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(path.display().to_string())
}

fn issue() -> GitHubIssue {
    GitHubIssue {
        repo: "acme/widgets".to_owned(),
        title: "The query is built by concatenation".to_owned(),
        body: "body\n\n---\nrev-local-fingerprint: fp1\n".to_owned(),
        fingerprint: "fp1".to_owned(),
    }
}

// --- which repository an issue is filed against -----------------------------

#[test]
fn every_shape_of_github_remote_yields_owner_and_name() {
    for url in [
        "https://github.com/acme/widgets.git",
        "https://github.com/acme/widgets",
        "git@github.com:acme/widgets.git",
        "ssh://git@github.com/acme/widgets.git",
        "https://github.company.com/acme/widgets.git",
        "  https://github.com/acme/widgets/  ",
    ] {
        assert_eq!(
            github_slug(url).as_deref(),
            Some("acme/widgets"),
            "could not read a slug from {url}"
        );
    }
}

#[test]
fn a_remote_that_is_not_github_is_refused_rather_than_guessed_at() {
    // Every forge uses these two URL shapes. A parser that only read the path
    // would hand `acme/widgets` to `gh`, which would file a private repository's
    // findings into whatever `acme/widgets` happens to exist on github.com.
    for url in [
        "https://gitlab.com/acme/widgets.git",
        "git@bitbucket.org:acme/widgets.git",
        "https://git.company.com/acme/widgets.git",
    ] {
        assert_eq!(github_slug(url), None, "{url} must not be read as GitHub");
    }
}

#[test]
fn a_url_that_is_not_a_repository_is_refused() {
    // A gist, a tree view, a bare host. Taking the first two path segments would
    // invent a repository nobody asked for.
    for url in [
        "https://github.com/acme",
        "https://github.com/acme/widgets/tree/main",
        "https://github.com/",
        "",
    ] {
        assert_eq!(github_slug(url), None, "{url:?} must not yield a slug");
    }
}

// --- filing -----------------------------------------------------------------

#[tokio::test]
async fn a_filed_issue_is_read_back_from_the_url_gh_prints(
) -> Result<(), Box<dyn std::error::Error>> {
    // `gh issue create` has no JSON mode; it prints the URL and nothing else.
    let dir = TempDir::new()?;
    let program = fake_gh(&dir, "https://github.com/acme/widgets/issues/91\n", 0)?;

    let filed = GhWriter::with_program(&program)
        .create_issue(&issue())
        .await?;

    assert_eq!(filed.number, 91);
    assert_eq!(
        filed.url.as_deref(),
        Some("https://github.com/acme/widgets/issues/91")
    );
    Ok(())
}

#[tokio::test]
async fn output_with_no_issue_number_is_refused_rather_than_retried(
) -> Result<(), Box<dyn std::error::Error>> {
    // The issue was filed. Retrying would file a second one, so this must never
    // be classified as retryable however odd the output looks.
    let dir = TempDir::new()?;
    let program = fake_gh(&dir, "something else entirely\n", 0)?;

    let error = GhWriter::with_program(&program)
        .create_issue(&issue())
        .await
        .err()
        .ok_or("output with no number must fail")?;

    assert!(!error.is_retryable(), "{error}");
    Ok(())
}

// --- not filing twice -------------------------------------------------------

#[test]
fn the_search_result_is_confirmed_against_the_body_not_trusted() {
    // GitHub's search tokenises on punctuation, so a query for one fingerprint
    // can return an issue carrying another. Commenting on the wrong issue is
    // worse than filing a second one.
    let listing = r#"[
        {"number": 5, "url": "u5", "body": "unrelated\n\n---\nrev-local-fingerprint: fp2\n"},
        {"number": 9, "url": "u9", "body": "ours\n\n---\nrev-local-fingerprint: fp1\n"}
    ]"#;

    assert_eq!(find_own_issue(listing, "fp1").map(|i| i.number), Some(9));
    assert_eq!(find_own_issue(listing, "fp3"), None);
}

#[test]
fn a_listing_with_no_match_files_a_new_issue() {
    assert_eq!(find_own_issue("[]", "fp1"), None);
}

#[tokio::test]
async fn a_closed_issue_still_counts_as_already_filed() -> Result<(), Box<dyn std::error::Error>> {
    // `--state all`. A finding that recurs after somebody closed the issue should
    // reopen that conversation rather than start a second one.
    let dir = TempDir::new()?;
    let listing =
        r#"[{"number": 12, "url": "u12", "body": "x\n\n---\nrev-local-fingerprint: fp1\n"}]"#;
    let program = fake_gh(&dir, listing, 0)?;

    let found = GhWriter::with_program(&program)
        .find_issue("acme/widgets", "fp1")
        .await?;

    assert_eq!(found.map(|i| i.number), Some(12));
    Ok(())
}

#[tokio::test]
async fn a_comment_keeps_the_issue_number_it_was_given() -> Result<(), Box<dyn std::error::Error>> {
    // The comment's permalink is not the issue's number, and reading one back off
    // the other would be a second source of truth for something already known.
    let dir = TempDir::new()?;
    let program = fake_gh(
        &dir,
        "https://github.com/acme/widgets/issues/12#issuecomment-77\n",
        0,
    )?;

    let commented = GhWriter::with_program(&program)
        .comment_issue("acme/widgets", 12, "rev-local saw this again.")
        .await?;

    assert_eq!(commented.number, 12);
    Ok(())
}
