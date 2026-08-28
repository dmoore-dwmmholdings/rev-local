//! Choosing how to reach GitHub (SPEC §6.3).
//!
//! Three ways in, in priority order: the configured GitHub **MCP server**, the
//! **`gh` CLI** if it is authenticated, and **unauthenticated REST** for public
//! repositories. The last is read-only.
//!
//! # Why the ladder is a pure function
//!
//! [`select`] takes the answers and returns a decision. It does not probe anything
//! itself, which means the whole ladder — including every rung failing — can be
//! tested without a network, a token, or a GitHub account. [`probe`] does the
//! asking and is the only part that needs any of those.
//!
//! # Why every rung reports, not just the winner
//!
//! A user whose repository silently fell back to unauthenticated mode sees reviews
//! that read PRs and never post. The interesting question is not "which transport
//! am I on" but "why not the one I configured", and that can only be answered if
//! the rungs that were skipped say why they were skipped. `revlocal doctor` prints
//! the whole ladder for that reason.

use crate::adapter::ProbeProblem;

/// How rev-local reaches GitHub for one repository.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum GitHubTransport {
    /// The configured GitHub MCP server.
    Mcp {
        /// Which `mcpServers` entry (SPEC §13.1).
        server: String,
    },
    /// The `gh` CLI, authenticated.
    GhCli {
        /// The account `gh` is logged in as, for the doctor report.
        account: Option<String>,
    },
    /// Unauthenticated REST. **Read-only**, public repositories only.
    Unauthenticated,
}

impl GitHubTransport {
    /// A short name for the UI and `repo.github_transport`.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Mcp { .. } => "mcp",
            Self::GhCli { .. } => "gh_cli",
            Self::Unauthenticated => "unauthenticated",
        }
    }

    /// Whether this transport may perform write operations.
    ///
    /// SPEC §6.3: unauthenticated access is read-only and discovers PRs only.
    pub const fn can_write(&self) -> bool {
        match self {
            Self::Mcp { .. } | Self::GhCli { .. } => true,
            Self::Unauthenticated => false,
        }
    }
}

/// A write rev-local might attempt against GitHub (SPEC §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubWrite {
    /// A threaded PR review with inline comments.
    PostReview,
    /// A single comment on a PR or commit.
    Comment,
    /// A `rev-local/review` check run.
    SetCheck,
}

impl GitHubWrite {
    /// How to describe the operation in an error.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::PostReview => "post a pull-request review",
            Self::Comment => "post a comment",
            Self::SetCheck => "set a check run",
        }
    }
}

/// Why a write was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "cannot {} on {repo}: rev-local is reaching GitHub without authentication, \
     which is read-only\n  try: {remediation}",
    operation.describe()
)]
pub struct WriteRefused {
    /// What was attempted.
    pub operation: GitHubWrite,
    /// The repository it was attempted against.
    pub repo: String,
    /// What the user should do.
    pub remediation: String,
}

/// What happened at one rung of the ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungReport {
    /// The transport this rung would have selected.
    pub rung: &'static str,
    /// Whether it was usable.
    pub available: bool,
    /// Why not, when it was not. Always carries remediation.
    pub problem: Option<ProbeProblem>,
}

/// The outcome of walking the ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportSelection {
    /// The transport that was chosen, if any rung was usable.
    ///
    /// `None` means even unauthenticated access is unusable — a private repository
    /// with no credentials. That is a real state and it is not "unauthenticated":
    /// pretending otherwise would produce a repo that discovers nothing and reports
    /// itself healthy.
    pub transport: Option<GitHubTransport>,
    /// Every rung, in order, whether it was taken or not.
    pub ladder: Vec<RungReport>,
}

impl TransportSelection {
    /// Whether GitHub is reachable at all.
    pub const fn is_usable(&self) -> bool {
        self.transport.is_some()
    }

    /// A `revlocal doctor` line for each rung, in ladder order.
    ///
    /// The chosen rung is marked, and every skipped rung says why — the question a
    /// user actually has is "why not the one I configured".
    pub fn doctor_lines(&self) -> Vec<String> {
        let chosen = self.transport.as_ref().map(GitHubTransport::name);

        self.ladder
            .iter()
            .map(|rung| {
                let marker = if chosen == Some(rung.rung) {
                    "USING"
                } else if rung.available {
                    "ok"
                } else {
                    "no"
                };
                match &rung.problem {
                    Some(problem) => format!(
                        "  [{marker}] {}: {}\n        try: {}",
                        rung.rung, problem.problem, problem.remediation
                    ),
                    None => format!("  [{marker}] {}", rung.rung),
                }
            })
            .collect()
    }
}

/// The answers [`select`] needs. Gathered by [`probe`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportProbes {
    /// The `mcpServers` entry configured for GitHub, if any (SPEC §13.1).
    pub mcp_server: Option<String>,
    /// Whether that server answered.
    pub mcp_reachable: bool,
    /// Whether `gh` is on PATH.
    pub gh_installed: bool,
    /// Whether `gh auth status` succeeded.
    pub gh_authenticated: bool,
    /// The account `gh` reports, for the doctor line.
    pub gh_account: Option<String>,
    /// Whether the repository is public.
    ///
    /// `None` means unknown — which is treated as "not known to be public", because
    /// assuming public and falling back to unauthenticated on a private repo would
    /// produce a repo that discovers nothing and looks configured.
    pub repo_is_public: Option<bool>,
}

/// Walk the ladder (SPEC §6.3).
pub fn select(probes: &TransportProbes) -> TransportSelection {
    let mut ladder = Vec::new();
    let mut chosen: Option<GitHubTransport> = None;

    // (a) the configured GitHub MCP server.
    match (&probes.mcp_server, probes.mcp_reachable) {
        (Some(server), true) => {
            ladder.push(RungReport {
                rung: "mcp",
                available: true,
                problem: None,
            });
            chosen = Some(GitHubTransport::Mcp {
                server: server.clone(),
            });
        }
        (Some(server), false) => ladder.push(RungReport {
            rung: "mcp",
            available: false,
            problem: Some(ProbeProblem {
                problem: format!("the `{server}` MCP server did not answer"),
                remediation: format!(
                    "check the `mcpServers.{server}` entry in config.toml, and that \
                     the server starts on its own"
                ),
            }),
        }),
        (None, _) => ladder.push(RungReport {
            rung: "mcp",
            available: false,
            problem: Some(ProbeProblem {
                problem: "no GitHub MCP server is configured".to_owned(),
                remediation: "add an `mcpServers.github` entry to config.toml".to_owned(),
            }),
        }),
    }

    // (b) the gh CLI, if authenticated.
    let gh_problem = if !probes.gh_installed {
        Some(ProbeProblem {
            problem: "`gh` is not on PATH".to_owned(),
            remediation: "install the GitHub CLI, then run `gh auth login`".to_owned(),
        })
    } else if !probes.gh_authenticated {
        Some(ProbeProblem {
            problem: "`gh` is installed but not authenticated".to_owned(),
            remediation: "run `gh auth login`; rev-local uses your existing CLI login \
                          and stores no credentials of its own"
                .to_owned(),
        })
    } else {
        None
    };
    let gh_available = gh_problem.is_none();
    ladder.push(RungReport {
        rung: "gh_cli",
        available: gh_available,
        problem: gh_problem,
    });
    if chosen.is_none() && gh_available {
        chosen = Some(GitHubTransport::GhCli {
            account: probes.gh_account.clone(),
        });
    }

    // (c) unauthenticated REST, public repositories only, read-only.
    let public = probes.repo_is_public.unwrap_or(false);
    let unauth_problem = if public {
        None
    } else if probes.repo_is_public.is_none() {
        Some(ProbeProblem {
            problem: "the repository's visibility is unknown, so unauthenticated \
                      access cannot be assumed"
                .to_owned(),
            remediation: "authenticate with `gh auth login`, or configure a GitHub \
                          MCP server"
                .to_owned(),
        })
    } else {
        Some(ProbeProblem {
            problem: "the repository is private, and unauthenticated access cannot \
                      read it"
                .to_owned(),
            remediation: "authenticate with `gh auth login`, or configure a GitHub \
                          MCP server"
                .to_owned(),
        })
    };
    let unauth_available = unauth_problem.is_none();
    ladder.push(RungReport {
        rung: "unauthenticated",
        available: unauth_available,
        problem: unauth_problem,
    });
    if chosen.is_none() && unauth_available {
        chosen = Some(GitHubTransport::Unauthenticated);
    }

    TransportSelection {
        transport: chosen,
        ladder,
    }
}

/// Check whether a transport may perform `operation`, and refuse it if not.
///
/// Returns the refusal rather than a bool, so a caller cannot forget to construct
/// an error and quietly do nothing — a publish that silently did not happen is the
/// exact failure SPEC §18 forbids.
pub fn authorize(
    transport: &GitHubTransport,
    operation: GitHubWrite,
    repo: &str,
) -> Result<(), WriteRefused> {
    if transport.can_write() {
        return Ok(());
    }
    Err(WriteRefused {
        operation,
        repo: repo.to_owned(),
        remediation: "run `gh auth login`, or configure a GitHub MCP server; \
                      unauthenticated access can discover pull requests but cannot \
                      post anything"
            .to_owned(),
    })
}

/// Ask the machine which rungs are available.
///
/// The only part of this module that touches anything outside the process. Kept
/// small and separate so [`select`] stays testable: every ladder shape, including
/// all three rungs failing, is reachable by constructing [`TransportProbes`].
pub async fn probe(
    runner: &super::super::git::GitRunner,
    mcp_server: Option<&str>,
    mcp_reachable: bool,
    repo_is_public: Option<bool>,
) -> TransportProbes {
    // `gh auth status` exits non-zero when unauthenticated, which is the whole
    // signal. It is run through the git command wrapper for its timeout and its
    // non-interactive environment: `gh` will happily open a browser and wait
    // forever otherwise, and a daemon has nobody to click it.
    let gh = runner
        .clone()
        .with_program("gh")
        .run(std::path::Path::new("."), &["auth", "status"])
        .await;

    let (gh_installed, gh_authenticated, gh_account) = match gh {
        Ok(output) => (true, true, parse_gh_account(&output.stdout, &output.stderr)),
        Err(crate::git::GitError::NotInstalled) => (false, false, None),
        // Any other failure means gh is there and unhappy — almost always "not
        // logged in". Reported as installed-but-unauthenticated rather than
        // missing, because the remediation differs.
        Err(_) => (true, false, None),
    };

    TransportProbes {
        mcp_server: mcp_server.map(str::to_owned),
        mcp_reachable,
        gh_installed,
        gh_authenticated,
        gh_account,
        repo_is_public,
    }
}

/// Pull the account name out of `gh auth status` output.
///
/// `gh` has moved this line between stdout and stderr across versions, so both are
/// searched. A missing account is not an error — it is one line of a doctor report.
fn parse_gh_account(stdout: &str, stderr: &str) -> Option<String> {
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(rest) = line.split_once("account ") {
            return rest.1.split_whitespace().next().map(str::to_owned);
        }
        if let Some(rest) = line.split_once("Logged in to github.com as ") {
            return rest.1.split_whitespace().next().map(str::to_owned);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probes() -> TransportProbes {
        TransportProbes::default()
    }

    // --- the ladder falls through in order ------------------------------------

    #[test]
    fn github_transport_mcp_wins_when_it_is_reachable() {
        let selection = select(&TransportProbes {
            mcp_server: Some("github".to_owned()),
            mcp_reachable: true,
            gh_installed: true,
            gh_authenticated: true,
            repo_is_public: Some(true),
            ..probes()
        });

        assert_eq!(
            selection.transport,
            Some(GitHubTransport::Mcp {
                server: "github".to_owned()
            }),
            "MCP is rung (a) and outranks an authenticated gh"
        );
    }

    #[test]
    fn github_transport_falls_through_to_gh_when_mcp_is_unreachable() {
        let selection = select(&TransportProbes {
            mcp_server: Some("github".to_owned()),
            mcp_reachable: false,
            gh_installed: true,
            gh_authenticated: true,
            gh_account: Some("octocat".to_owned()),
            repo_is_public: Some(true),
        });

        assert_eq!(
            selection.transport,
            Some(GitHubTransport::GhCli {
                account: Some("octocat".to_owned())
            })
        );
    }

    #[test]
    fn github_transport_falls_through_to_unauthenticated_for_a_public_repo() {
        let selection = select(&TransportProbes {
            repo_is_public: Some(true),
            ..probes()
        });
        assert_eq!(selection.transport, Some(GitHubTransport::Unauthenticated));
    }

    #[test]
    fn github_transport_a_private_repo_with_no_credentials_is_unusable_not_unauthenticated() {
        // The state that must not be papered over. Calling this "unauthenticated"
        // would produce a repository that discovers nothing and reports itself
        // healthy — reviews would simply stop, with nothing to point at.
        let selection = select(&TransportProbes {
            repo_is_public: Some(false),
            ..probes()
        });

        assert_eq!(selection.transport, None);
        assert!(!selection.is_usable());
        assert_eq!(selection.ladder.len(), 3, "every rung is still reported");
        assert!(selection.ladder.iter().all(|r| !r.available));
    }

    #[test]
    fn github_transport_unknown_visibility_is_not_assumed_public() {
        // Optimism here costs a repository that silently reviews nothing. Unknown is
        // treated as "not known to be public", and the remediation says so.
        let selection = select(&TransportProbes {
            repo_is_public: None,
            ..probes()
        });
        assert_eq!(selection.transport, None);

        let unauth = selection
            .ladder
            .iter()
            .find(|r| r.rung == "unauthenticated")
            .unwrap_or_else(|| panic!("no unauthenticated rung"));
        let problem = unauth
            .problem
            .clone()
            .unwrap_or_else(|| panic!("no problem recorded"));
        assert!(problem.problem.contains("unknown"), "{}", problem.problem);
    }

    // --- every rung reports, not just the winner ------------------------------

    #[test]
    fn github_transport_reports_which_rung_it_landed_on() {
        let selection = select(&TransportProbes {
            gh_installed: true,
            gh_authenticated: true,
            repo_is_public: Some(true),
            ..probes()
        });

        let rungs: Vec<&str> = selection.ladder.iter().map(|r| r.rung).collect();
        assert_eq!(
            rungs,
            ["mcp", "gh_cli", "unauthenticated"],
            "in SPEC §6.3's order"
        );

        let lines = selection.doctor_lines();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[1].contains("[USING]"),
            "the chosen rung must be marked: {lines:#?}"
        );
        assert!(
            lines[0].contains("[no]"),
            "and the skipped one marked too: {lines:#?}"
        );
    }

    #[test]
    fn github_transport_every_unavailable_rung_says_why_and_how_to_fix_it() {
        // The user's real question is not "which transport am I on" but "why not the
        // one I configured". That is only answerable if the skipped rungs explain
        // themselves.
        let selection = select(&probes());

        for rung in &selection.ladder {
            let problem = rung
                .problem
                .clone()
                .unwrap_or_else(|| panic!("{} was unavailable but gave no reason", rung.rung));
            assert!(
                !problem.problem.is_empty(),
                "{} has an empty problem",
                rung.rung
            );
            assert!(
                !problem.remediation.is_empty(),
                "{} has no remediation; SPEC §18 requires one",
                rung.rung
            );
        }

        let report = selection.doctor_lines().join("\n");
        assert!(
            report.contains("try:"),
            "doctor output must carry remediation:\n{report}"
        );
    }

    #[test]
    fn github_transport_tells_gh_missing_apart_from_gh_unauthenticated() {
        // Different remedies: one is `install the GitHub CLI`, the other is
        // `gh auth login`. Collapsing them sends half the users to the wrong page.
        let missing = select(&probes());
        let missing_rung = missing
            .ladder
            .iter()
            .find(|r| r.rung == "gh_cli")
            .and_then(|r| r.problem.clone())
            .unwrap_or_else(|| panic!("no gh rung"));
        assert!(
            missing_rung.problem.contains("not on PATH"),
            "{}",
            missing_rung.problem
        );
        assert!(
            missing_rung.remediation.contains("install"),
            "{}",
            missing_rung.remediation
        );

        let unauthenticated = select(&TransportProbes {
            gh_installed: true,
            ..probes()
        });
        let unauth_rung = unauthenticated
            .ladder
            .iter()
            .find(|r| r.rung == "gh_cli")
            .and_then(|r| r.problem.clone())
            .unwrap_or_else(|| panic!("no gh rung"));
        assert!(
            unauth_rung.problem.contains("not authenticated"),
            "{}",
            unauth_rung.problem
        );
        assert!(
            unauth_rung.remediation.contains("gh auth login"),
            "{}",
            unauth_rung.remediation
        );
        assert!(
            unauth_rung.remediation.contains("stores no credentials"),
            "decision D9: rev-local uses the user's existing CLI login: {}",
            unauth_rung.remediation
        );
    }

    #[test]
    fn github_transport_an_unconfigured_mcp_server_is_told_apart_from_an_unreachable_one() {
        let unconfigured = select(&probes());
        let a = unconfigured
            .ladder
            .iter()
            .find(|r| r.rung == "mcp")
            .and_then(|r| r.problem.clone())
            .unwrap_or_else(|| panic!("no mcp rung"));
        assert!(
            a.problem.contains("no GitHub MCP server is configured"),
            "{}",
            a.problem
        );

        let unreachable = select(&TransportProbes {
            mcp_server: Some("github".to_owned()),
            mcp_reachable: false,
            ..probes()
        });
        let b = unreachable
            .ladder
            .iter()
            .find(|r| r.rung == "mcp")
            .and_then(|r| r.problem.clone())
            .unwrap_or_else(|| panic!("no mcp rung"));
        assert!(b.problem.contains("did not answer"), "{}", b.problem);
        assert!(
            b.remediation.contains("mcpServers.github"),
            "the remediation must name the config key: {}",
            b.remediation
        );
    }

    // --- unauthenticated is read-only -----------------------------------------

    #[test]
    fn github_transport_unauthenticated_refuses_every_write() {
        // Acceptance criterion 2, over the whole write surface rather than one
        // operation — a ladder that refused reviews but allowed comments would pass
        // a single-operation test.
        for operation in [
            GitHubWrite::PostReview,
            GitHubWrite::Comment,
            GitHubWrite::SetCheck,
        ] {
            let refusal = authorize(&GitHubTransport::Unauthenticated, operation, "owner/repo")
                .expect_err("unauthenticated access is read-only");

            let message = refusal.to_string();
            assert!(message.contains("owner/repo"), "{message}");
            assert!(message.contains("read-only"), "{message}");
            assert!(
                message.contains("try:"),
                "every user-visible error carries a remedy: {message}"
            );
            assert!(
                message.contains(operation.describe()),
                "the error must name what was attempted: {message}"
            );
        }
    }

    #[test]
    fn github_transport_authenticated_transports_may_write() {
        for transport in [
            GitHubTransport::Mcp {
                server: "github".to_owned(),
            },
            GitHubTransport::GhCli { account: None },
        ] {
            assert!(transport.can_write());
            assert!(authorize(&transport, GitHubWrite::PostReview, "owner/repo").is_ok());
        }
    }

    #[test]
    fn github_transport_authorize_returns_the_refusal_rather_than_a_bool() {
        // A bool invites `if can_write() { ... }` with no else, and a publish that
        // silently did not happen is the failure SPEC §18 forbids. Returning a
        // Result makes ignoring it a warning.
        let refused = authorize(&GitHubTransport::Unauthenticated, GitHubWrite::Comment, "r");
        assert!(refused.is_err());
    }

    #[test]
    fn github_transport_names_are_stable_because_they_are_stored() {
        // These land in `repo.github_transport` and in doctor output; renaming one
        // would silently change stored data and break the UI's grouping.
        assert_eq!(
            GitHubTransport::Mcp {
                server: String::new()
            }
            .name(),
            "mcp"
        );
        assert_eq!(GitHubTransport::GhCli { account: None }.name(), "gh_cli");
        assert_eq!(GitHubTransport::Unauthenticated.name(), "unauthenticated");
    }

    #[test]
    fn github_transport_round_trips_for_the_repo_row() {
        let transport = GitHubTransport::GhCli {
            account: Some("octocat".to_owned()),
        };
        let json = serde_json::to_string(&transport).unwrap_or_default();
        let back: GitHubTransport = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, transport);
    }

    #[test]
    fn github_transport_parses_an_account_from_either_stream() {
        // `gh` has moved this line between stdout and stderr across versions.
        assert_eq!(
            parse_gh_account("", "  ✓ Logged in to github.com as octocat (keyring)"),
            Some("octocat".to_owned())
        );
        assert_eq!(
            parse_gh_account("  - Active account: true\n  - account fixtures", ""),
            Some("fixtures".to_owned())
        );
        assert_eq!(parse_gh_account("nothing useful", ""), None);
    }
}
