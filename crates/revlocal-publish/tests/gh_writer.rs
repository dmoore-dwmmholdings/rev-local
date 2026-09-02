//! The `gh`-backed GitHub writer (RL-1511, SPEC §11.3, §11.6).
//!
//! No network and no token: every test points the writer at a shell script that
//! prints what a real `gh` would print and exits with the code a real `gh` would
//! exit with. That is the ground rule these tests exist to keep — and it is also
//! the only way to exercise a 502, which is the case that matters most and the
//! one a live GitHub will not produce on request.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_publish::github::{GitHubWriter, ReviewPayload};
use revlocal_publish::{GhWriter, PublishError};
use tempfile::TempDir;

/// A fake `gh` that prints `stdout`, prints `stderr`, and exits with `code`.
fn fake_gh(dir: &TempDir, stdout: &str, stderr: &str, code: i32) -> Result<String, String> {
    let path = dir.path().join("gh");
    let script = format!(
        "#!/bin/sh\n\
         cat > /dev/null\n\
         printf '%s' {}\n\
         printf '%s' {} >&2\n\
         exit {code}\n",
        shell_quote(stdout),
        shell_quote(stderr),
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

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn payload() -> ReviewPayload {
    ReviewPayload {
        repo: "acme/widgets".to_owned(),
        pr: 7,
        head_sha: "a1b2c3d".to_owned(),
        body: "no blocking findings".to_owned(),
        comments: Vec::new(),
        event: "COMMENT".to_owned(),
    }
}

#[tokio::test]
async fn a_posted_review_is_read_back_from_what_github_returned(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let program = fake_gh(
        &dir,
        r#"{"id": 4211, "html_url": "https://github.com/acme/widgets/pull/7#pullrequestreview-4211"}"#,
        "",
        0,
    )?;

    let review = GhWriter::with_program(&program)
        .create_review(&payload())
        .await?;

    assert_eq!(review.id, 4211);
    // The URL is what the receipt shows, so a review that posted and reported no
    // link would be a delivery nobody can go and look at.
    assert!(review
        .url
        .is_some_and(|url| url.contains("pullrequestreview")));
    Ok(())
}

#[tokio::test]
async fn a_missing_gh_says_how_to_install_it_rather_than_blaming_the_network(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;

    let error = GhWriter::with_program(dir.path().join("definitely-not-here"))
        .create_review(&payload())
        .await
        .err()
        .ok_or("a missing program must fail")?;

    let message = error.to_string();
    assert!(message.contains("not installed"), "{message}");
    assert!(message.contains("gh auth login"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_404_is_terminal_because_retrying_it_cannot_help(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let program = fake_gh(&dir, "", "gh: Not Found (HTTP 404)\n", 1)?;

    let error = GhWriter::with_program(&program)
        .create_review(&payload())
        .await
        .err()
        .ok_or("a 404 must fail")?;

    assert!(
        matches!(
            error,
            PublishError::Rejected {
                status: Some(404),
                ..
            }
        ),
        "{error}"
    );
    assert!(!error.is_retryable());
    Ok(())
}

#[tokio::test]
async fn a_502_is_retried_because_github_hiccuped() -> Result<(), Box<dyn std::error::Error>> {
    // The case that decides whether a review is dropped or delivered, and the one
    // a live GitHub will not produce on request.
    let dir = TempDir::new()?;
    let program = fake_gh(&dir, "", "gh: Bad Gateway (HTTP 502)\n", 1)?;

    let error = GhWriter::with_program(&program)
        .create_review(&payload())
        .await
        .err()
        .ok_or("a 502 must fail")?;

    assert!(
        matches!(
            error,
            PublishError::Server {
                status: Some(502),
                ..
            }
        ),
        "{error}"
    );
    assert!(error.is_retryable());
    Ok(())
}

#[tokio::test]
async fn a_rate_limited_403_is_deferred_rather_than_dropped(
) -> Result<(), Box<dyn std::error::Error>> {
    // A secondary rate limit *is* a 403, and GitHub only distinguishes it in the
    // message. Reading it as a permissions refusal would throw away work GitHub
    // asked us to postpone.
    let dir = TempDir::new()?;
    let program = fake_gh(
        &dir,
        "",
        "gh: You have exceeded a secondary rate limit (HTTP 403)\n",
        1,
    )?;

    let error = GhWriter::with_program(&program)
        .create_review(&payload())
        .await
        .err()
        .ok_or("a rate limit must fail")?;

    assert!(matches!(error, PublishError::RateLimited { .. }), "{error}");
    assert!(error.is_retryable());
    Ok(())
}

#[tokio::test]
async fn a_plain_403_is_still_terminal() -> Result<(), Box<dyn std::error::Error>> {
    // The other half of the rule above: without the rate-limit wording, a 403 is
    // a token that cannot do this, and retrying will never fix it.
    let dir = TempDir::new()?;
    let program = fake_gh(
        &dir,
        "",
        "gh: Resource not accessible by integration (HTTP 403)\n",
        1,
    )?;

    let error = GhWriter::with_program(&program)
        .create_review(&payload())
        .await
        .err()
        .ok_or("a 403 must fail")?;

    assert!(
        matches!(
            error,
            PublishError::Rejected {
                status: Some(403),
                ..
            }
        ),
        "{error}"
    );
    assert!(!error.is_retryable());
    Ok(())
}

#[tokio::test]
async fn a_failure_before_github_is_retryable() -> Result<(), Box<dyn std::error::Error>> {
    // Not logged in, no network, a bad flag — `gh` never reached GitHub and there
    // is no status to read. Retryable, because two of those three are transient.
    let dir = TempDir::new()?;
    let program = fake_gh(
        &dir,
        "",
        "gh: To get started with GitHub CLI, please run: gh auth login\n",
        4,
    )?;

    let error = GhWriter::with_program(&program)
        .create_review(&payload())
        .await
        .err()
        .ok_or("an unauthenticated gh must fail")?;

    assert!(matches!(error, PublishError::Transport { .. }), "{error}");
    assert!(error.is_retryable());
    Ok(())
}

#[tokio::test]
async fn output_that_is_not_a_review_is_refused_rather_than_retried(
) -> Result<(), Box<dyn std::error::Error>> {
    // The call succeeded. Repeating it would post a second review, so this must
    // never be classified as retryable however odd the response looks.
    let dir = TempDir::new()?;
    let program = fake_gh(&dir, r#"{"message": "ok"}"#, "", 0)?;

    let error = GhWriter::with_program(&program)
        .create_review(&payload())
        .await
        .err()
        .ok_or("a response with no id must fail")?;

    assert!(!error.is_retryable(), "{error}");
    assert!(error.to_string().contains("no id"), "{error}");
    Ok(())
}

#[tokio::test]
async fn our_own_review_is_found_and_somebody_elses_is_not(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let listing = r#"[
        {"id": 1, "body": "looks fine to me", "commit_id": "a1b2c3d"},
        {"id": 2, "body": "<!-- rev-local:review -->\nno blocking findings", "commit_id": "a1b2c3d"}
    ]"#;
    let program = fake_gh(&dir, listing, "", 0)?;

    let found = GhWriter::with_program(&program)
        .find_review("acme/widgets", 7, "a1b2c3d")
        .await?;

    assert_eq!(found.map(|review| review.id), Some(2));
    Ok(())
}

#[tokio::test]
async fn a_review_of_an_earlier_push_is_not_mistaken_for_this_one(
) -> Result<(), Box<dyn std::error::Error>> {
    // The marker says it is ours; the SHA says which commit it is about. Matching
    // on the marker alone would edit the review of a previous push.
    let dir = TempDir::new()?;
    let listing =
        r#"[{"id": 2, "body": "<!-- rev-local:review -->\nold", "commit_id": "0000000"}]"#;
    let program = fake_gh(&dir, listing, "", 0)?;

    let found = GhWriter::with_program(&program)
        .find_review("acme/widgets", 7, "a1b2c3d")
        .await?;

    assert!(found.is_none());
    Ok(())
}
