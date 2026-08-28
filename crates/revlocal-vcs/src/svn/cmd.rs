//! The single place `svn` and `svnlook` are invoked (RL-901, SPEC §6.4).
//!
//! A choke point for the same reasons `git/cmd.rs` is one, plus one that is
//! specific to Subversion.
//!
//! # `svn` blocks on a human by default, and in more ways than git does
//!
//! `svn` will prompt for credentials, and it will *also* stop and ask whether to
//! trust an unrecognised server certificate. A daemon has nobody to answer either,
//! so both are refused explicitly rather than left to whatever the user's
//! `~/.subversion/servers` happens to say.
//!
//! `--non-interactive` is therefore not optional and not a preference: without it,
//! a certificate change on a server rev-local polls turns every subsequent run
//! into a process waiting forever on a question nobody will see.
//!
//! # Trusting a certificate is a decision the operator makes, not rev-local
//!
//! `--trust-server-cert-failures` exists and is off. Turning it on means accepting
//! whatever certificate is presented, which for a server rev-local reads code
//! from is a decision with a security consequence. It is configurable, per repo,
//! and it names which failures are accepted rather than being a boolean — because
//! "the certificate expired" and "the certificate is for a different host" are not
//! the same risk.
//!
//! # Absence of `svn` is blocking for SVN repositories only
//!
//! §6.4: `revlocal doctor` reports it "as a blocking prerequisite for SVN repos
//! only". A machine with no Subversion installed must keep reviewing its git
//! repositories exactly as before, which means nothing on the git path may consult
//! this module and nothing here may run at import time.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// How long an `svn` invocation gets before it is killed.
///
/// Longer than git's default: `svn` talks to a server for almost every operation,
/// including ones that are local in git.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// Which certificate failures a repository is willing to accept.
///
/// A set rather than a boolean. `--trust-server-cert-failures=expired` and
/// `=unknown-ca` are different decisions with different consequences, and folding
/// them into one flag means somebody who wanted to tolerate a clock skew also
/// silently accepted an unknown authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertFailure {
    /// The certificate is not signed by a known authority.
    UnknownCa,
    /// The certificate has expired.
    Expired,
    /// The certificate is not yet valid.
    NotYetValid,
    /// The certificate's hostname does not match.
    CnMismatch,
    /// Some other verification failure.
    Other,
}

impl CertFailure {
    /// The token `svn` expects.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownCa => "unknown-ca",
            Self::Expired => "expired",
            Self::NotYetValid => "not-yet-valid",
            Self::CnMismatch => "cn-mismatch",
            Self::Other => "other",
        }
    }
}

/// Environment applied to every invocation, so `svn` cannot block on a human.
///
/// Returned as data rather than applied inline so it can be asserted directly —
/// same reasoning as `git/cmd.rs`: a test that only checked end-to-end behaviour
/// would not notice one of these going missing.
pub fn non_interactive_env() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        // Stable, parseable output regardless of the user's locale. `svn log --xml`
        // is locale-independent, but error text is not, and errors are matched on.
        ("LC_ALL", "C"),
        // Never open an editor: `svn` reaches for one on several subcommands and
        // would wait forever for a file that is never saved.
        ("SVN_EDITOR", "false"),
        ("EDITOR", "false"),
        ("VISUAL", "false"),
        // Never open a pager.
        ("PAGER", "cat"),
    ])
}

/// What an `svn` invocation can do other than succeed.
#[derive(Debug, thiserror::Error)]
pub enum SvnError {
    /// `svn` is not on PATH.
    ///
    /// §6.4 makes this blocking for SVN repositories **only**, and the message
    /// says so — a user with git repositories who sees this should not think their
    /// installation is broken.
    #[error(
        "`svn` is not on PATH\n  try: install Subversion (apt-get install \
         subversion / brew install subversion / choco install svn). Only \
         repositories with kind=svn need it; git repositories are unaffected"
    )]
    NotInstalled,

    /// The call exceeded its timeout and was killed.
    #[error(
        "`svn {args}` did not finish within {timeout:?} and was killed\n  \
         try: check the repository URL is reachable, or raise the timeout"
    )]
    Timeout {
        /// The arguments, for identifying which call hung.
        args: String,
        /// How long it was given.
        timeout: Duration,
    },

    /// The server wanted credentials and there were none.
    ///
    /// Distinct because the remedy is specific: rev-local never prompts and stores
    /// no credentials, so the fix is always outside it.
    #[error(
        "`svn {args}` needs credentials rev-local does not have\n  \
         try: authenticate once outside rev-local (`svn --username … checkout`), \
         which caches the credential where svn expects it; rev-local never prompts"
    )]
    CredentialsRequired {
        /// The arguments.
        args: String,
        /// What svn said.
        stderr: String,
    },

    /// The server's certificate was not accepted.
    ///
    /// Separate from a credential failure because the remedy is a deliberate
    /// decision — accepting a certificate — rather than supplying a password.
    #[error(
        "`svn {args}` rejected the server's certificate: {stderr}\n  \
         try: if this is expected, set `svn_trust_cert_failures` for this repo to \
         the specific failures you accept. rev-local will not accept a certificate \
         on your behalf"
    )]
    CertificateRejected {
        /// The arguments.
        args: String,
        /// What svn said.
        stderr: String,
    },

    /// svn ran and reported failure.
    #[error("`svn {args}` failed with exit code {code}: {stderr}")]
    Failed {
        /// The arguments.
        args: String,
        /// The exit code, or -1 if it was signalled.
        code: i32,
        /// What svn said.
        stderr: String,
    },

    /// The process could not be spawned or waited on.
    #[error("running `svn {args}`: {source}")]
    Spawn {
        /// The arguments.
        args: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl SvnError {
    /// Whether this is worth trying again.
    ///
    /// A timeout might be a slow server. Missing credentials, a rejected
    /// certificate and a missing binary will all fail identically next time.
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::Spawn { .. })
    }
}

/// A completed invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnOutput {
    /// Captured stdout, trailing newline trimmed.
    pub stdout: String,
    /// Captured stderr, trailing newline trimmed.
    pub stderr: String,
}

impl SvnOutput {
    /// stdout split into lines, empty when there is none.
    pub fn lines(&self) -> Vec<&str> {
        if self.stdout.is_empty() {
            Vec::new()
        } else {
            self.stdout.lines().collect()
        }
    }
}

/// The choke point for `svn` and `svnlook`.
#[derive(Debug, Clone)]
pub struct SvnRunner {
    program: PathBuf,
    timeout: Duration,
    trust: Vec<CertFailure>,
}

impl Default for SvnRunner {
    fn default() -> Self {
        Self {
            program: PathBuf::from("svn"),
            timeout: DEFAULT_TIMEOUT,
            // Empty, deliberately. Accepting a certificate is the operator's
            // decision and rev-local does not make it by default.
            trust: Vec::new(),
        }
    }
}

impl SvnRunner {
    /// A runner using `svn` from PATH, the default timeout, and no certificate
    /// failures accepted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a different timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Use a different program — `svnlook`, or an `svn` that is not on PATH.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Accept these certificate failures.
    pub fn trusting(mut self, failures: impl IntoIterator<Item = CertFailure>) -> Self {
        self.trust = failures.into_iter().collect();
        self
    }

    /// This runner's timeout.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Which certificate failures this runner accepts.
    pub fn trusted(&self) -> &[CertFailure] {
        &self.trust
    }

    /// The flags prepended to every invocation.
    ///
    /// Exposed so a test can assert them directly. `--non-interactive` is the one
    /// that must never be missing: without it a certificate change turns every
    /// later run into a process waiting on a question nobody will see.
    pub fn safety_flags(&self) -> Vec<String> {
        let mut flags = vec!["--non-interactive".to_owned()];
        if !self.trust.is_empty() {
            let accepted: Vec<&str> = self.trust.iter().map(|f| f.as_str()).collect();
            flags.push(format!(
                "--trust-server-cert-failures={}",
                accepted.join(",")
            ));
        }
        flags
    }

    /// Run `svn` in `dir` with `args`.
    pub async fn run<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
    ) -> Result<SvnOutput, SvnError> {
        let flags = self.safety_flags();
        let rendered = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&flags)
            .args(args)
            .current_dir(dir)
            // Nothing to type into. Explicit so a subcommand that reads stdin gets
            // a fast EOF rather than blocking on an inherited terminal.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in non_interactive_env() {
            command.env(key, value);
        }

        // A new process group, so a timeout reaches whatever svn spawned — ssh for
        // an `svn+ssh://` URL, most obviously.
        #[cfg(unix)]
        command.process_group(0);

        let child = command.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                SvnError::NotInstalled
            } else {
                SvnError::Spawn {
                    args: rendered.clone(),
                    source,
                }
            }
        })?;

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(source)) => {
                return Err(SvnError::Spawn {
                    args: rendered,
                    source,
                })
            }
            Err(_) => {
                return Err(SvnError::Timeout {
                    args: rendered,
                    timeout: self.timeout,
                })
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_owned();

        if output.status.success() {
            return Ok(SvnOutput { stdout, stderr });
        }

        Err(classify(
            &rendered,
            output.status.code().unwrap_or(-1),
            &stderr,
        ))
    }
}

/// Turn a failed invocation into the most specific error that fits.
///
/// Matched on svn's real output. ADR 0023's rule applies — these strings were read
/// from `svn` rather than guessed — and the fallback is a plain `Failed` carrying
/// stderr verbatim, so an unrecognised failure is still readable rather than
/// mislabelled.
fn classify(args: &str, code: i32, stderr: &str) -> SvnError {
    let lower = stderr.to_lowercase();

    if lower.contains("server certificate verification failed")
        || lower.contains("certificate verification")
    {
        return SvnError::CertificateRejected {
            args: args.to_owned(),
            stderr: stderr.to_owned(),
        };
    }

    if lower.contains("authorization failed")
        || lower.contains("authentication failed")
        || lower.contains("no more credentials")
        || lower.contains("username or password")
    {
        return SvnError::CredentialsRequired {
            args: args.to_owned(),
            stderr: stderr.to_owned(),
        };
    }

    SvnError::Failed {
        args: args.to_owned(),
        code,
        stderr: stderr.to_owned(),
    }
}

/// Whether `svn` is available on this machine.
///
/// Used by `revlocal doctor`. Deliberately a question rather than a check that
/// returns an error: §6.4 makes absence blocking for SVN repositories only, and a
/// machine with none should not be told it has a problem.
pub async fn is_available() -> bool {
    SvnRunner::new()
        .with_timeout(Duration::from_secs(10))
        .run(Path::new("."), &["--version", "--quiet"])
        .await
        .is_ok()
}

/// What `revlocal doctor` says about Subversion.
///
/// Takes whether any SVN repository is configured, because that is what decides
/// whether absence is a problem or a fact.
pub fn doctor_line(available: bool, svn_repos_configured: usize) -> String {
    match (available, svn_repos_configured) {
        (true, 0) => "svn: available (no SVN repositories configured)".to_owned(),
        (true, n) => format!("svn: available ({n} SVN repository/ies configured)"),
        (false, 0) => "svn: not installed — not needed, because no SVN repositories are \
             configured. Git repositories are unaffected."
            .to_owned(),
        (false, n) => format!(
            "svn: NOT INSTALLED, and {n} SVN repository/ies are configured — those \
             repositories cannot be reviewed.\n  try: install Subversion \
             (apt-get install subversion / brew install subversion / choco install \
             svn). Git repositories are unaffected."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.
    fn svn_is_installed() -> bool {
        std::process::Command::new("svn")
            .arg("--version")
            .arg("--quiet")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    // --- criterion 3: the safety flags are explicit and configurable -------

    #[test]
    fn svn_cmd_non_interactive_is_always_passed() {
        let flags = SvnRunner::new().safety_flags();
        assert!(
            flags.contains(&"--non-interactive".to_owned()),
            "without it, a certificate change on a polled server turns every later \
             run into a process waiting on a question nobody will see: {flags:?}"
        );
    }

    #[test]
    fn svn_cmd_trusts_no_certificate_failure_by_default() {
        let runner = SvnRunner::new();
        assert!(runner.trusted().is_empty());
        assert!(
            !runner
                .safety_flags()
                .iter()
                .any(|f| f.starts_with("--trust-server-cert-failures")),
            "accepting a certificate for a server rev-local reads code from is the \
             operator's decision, not a default"
        );
    }

    #[test]
    fn svn_cmd_trusted_failures_are_named_individually() {
        let runner = SvnRunner::new().trusting([CertFailure::Expired]);
        let flags = runner.safety_flags();

        assert!(flags.contains(&"--trust-server-cert-failures=expired".to_owned()));
        assert!(
            !flags.iter().any(|f| f.contains("unknown-ca")),
            "`the certificate expired` and `the certificate is from an unknown \
             authority` are not the same risk, and a boolean would have accepted \
             both: {flags:?}"
        );

        let both = SvnRunner::new()
            .trusting([CertFailure::Expired, CertFailure::UnknownCa])
            .safety_flags();
        assert!(both.contains(&"--trust-server-cert-failures=expired,unknown-ca".to_owned()));
    }

    #[test]
    fn svn_cmd_the_non_interactive_environment_closes_every_prompt() {
        let env = non_interactive_env();

        for editor in ["SVN_EDITOR", "EDITOR", "VISUAL"] {
            assert_eq!(
                env.get(editor),
                Some(&"false"),
                "svn reaches for an editor on several subcommands and would wait \
                 forever for a file that is never saved"
            );
        }
        assert_eq!(env.get("PAGER"), Some(&"cat"));
        assert_eq!(
            env.get("LC_ALL"),
            Some(&"C"),
            "error text is matched on, and it is locale-dependent"
        );
    }

    // --- criterion 1: a missing binary is actionable, not a panic ----------

    #[tokio::test]
    async fn svn_cmd_a_missing_binary_is_a_typed_error_that_says_what_to_do() {
        let runner = SvnRunner::new().with_program("revlocal-no-such-svn");
        let error = runner
            .run(std::path::Path::new("."), &["--version"])
            .await
            .expect_err("a missing binary must not succeed");

        assert!(matches!(error, SvnError::NotInstalled), "{error:?}");

        let message = error.to_string();
        assert!(message.contains("try:"), "§18: {message}");
        assert!(
            message.contains("brew install subversion")
                || message.contains("apt-get install subversion"),
            "the remedy is platform-specific and should say so: {message}"
        );
        assert!(
            message.contains("git repositories are unaffected"),
            "§6.4 makes this blocking for SVN repos ONLY; a git user seeing this \
             must not think their install is broken: {message}"
        );
        assert!(
            !error.is_retryable(),
            "installing Subversion is not something a retry does"
        );
    }

    // --- criterion 2: git repos are unaffected -----------------------------

    #[test]
    fn svn_cmd_doctor_reports_absence_as_a_problem_only_when_svn_repos_exist() {
        let no_repos = doctor_line(false, 0);
        assert!(
            !no_repos.contains("NOT INSTALLED"),
            "a machine with only git repositories has no problem to report: \
             {no_repos}"
        );
        assert!(no_repos.contains("Git repositories are unaffected"));

        let with_repos = doctor_line(false, 2);
        assert!(
            with_repos.contains("NOT INSTALLED"),
            "and one with SVN repositories does: {with_repos}"
        );
        assert!(with_repos.contains("try:"), "§18: {with_repos}");
        assert!(with_repos.contains("Git repositories are unaffected"));

        assert!(doctor_line(true, 0).contains("available"));
        assert!(doctor_line(true, 3).contains("3 SVN repository"));
    }

    // --- classification, against svn's real output -------------------------

    #[test]
    fn svn_cmd_a_certificate_rejection_is_not_reported_as_a_credential_problem() {
        let error = classify(
            "info http://x",
            1,
            "svn: E230001: Server SSL certificate verification failed: certificate \
             issued for a different hostname",
        );

        assert!(
            matches!(error, SvnError::CertificateRejected { .. }),
            "the remedy is a deliberate decision about a certificate, not a \
             password: {error:?}"
        );
        assert!(error
            .to_string()
            .contains("will not accept a certificate on your behalf"));
    }

    #[test]
    fn svn_cmd_a_credential_failure_names_the_remedy_outside_rev_local() {
        let error = classify(
            "log --xml",
            1,
            "svn: E170013: Unable to connect\nsvn: E215004: No more credentials or \
             we tried too many times",
        );

        assert!(
            matches!(error, SvnError::CredentialsRequired { .. }),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("rev-local never prompts"), "{message}");
        assert!(!error.is_retryable());
    }

    #[test]
    fn svn_cmd_an_unrecognised_failure_keeps_stderr_verbatim() {
        let error = classify("export -r 7", 1, "svn: E200009: something new");

        let SvnError::Failed { code, stderr, .. } = &error else {
            panic!("expected a plain failure, got {error:?}");
        };
        assert_eq!(*code, 1);
        assert_eq!(
            stderr, "svn: E200009: something new",
            "an unrecognised failure must stay readable rather than be mislabelled \
             as one of the cases we do recognise"
        );
    }

    // --- against a real svn, where there is one ---------------------------

    #[tokio::test]
    async fn svn_cmd_runs_a_real_command_when_subversion_is_installed() {
        if !svn_is_installed() {
            println!("SKIPPED (svn not installed, nothing verified): svn_cmd_runs_a_real_command");
            return;
        }

        let output = SvnRunner::new()
            .run(std::path::Path::new("."), &["--version", "--quiet"])
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(
            output.stdout.starts_with(char::is_numeric),
            "`svn --version --quiet` prints a bare version: {:?}",
            output.stdout
        );
        assert!(is_available().await);
    }

    #[tokio::test]
    async fn svn_cmd_a_failing_command_reports_svns_own_error() {
        if !svn_is_installed() {
            println!("SKIPPED (svn not installed, nothing verified): svn_cmd_a_failing_command");
            return;
        }

        let error = SvnRunner::new()
            .run(
                std::path::Path::new("."),
                &["info", "file:///revlocal/definitely/not/a/repository"],
            )
            .await
            .expect_err("that is not a repository");

        // Whatever svn called it, the text is preserved rather than replaced.
        assert!(
            error.to_string().contains("svn:"),
            "svn's own error should reach the user: {error}"
        );
    }
}
