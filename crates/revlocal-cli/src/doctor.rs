//! `revlocal doctor` (RL-1202, SPEC §8.4, §14).
//!
//! The first thing somebody runs on a fresh install, and the thing they run again
//! when reviews have quietly stopped. Both cases want the same answer — *what is
//! wrong and what do I type* — so every failing line carries a next action rather
//! than a diagnosis (§18).
//!
//! # Reporting, never fixing
//!
//! Decision D9: engines authenticate through the user's existing CLI logins and
//! this app stores no API keys. So an unauthenticated engine is something to
//! *report*, and a doctor that offered to log in for you would be asking for
//! credentials the product exists not to hold.
//!
//! # Not installed is not the same as not working
//!
//! §8.4 separates three things an engine can fail at, and collapsing them loses
//! the distinction that matters most: a CLI can be installed, logged in, and still
//! not honour §8.2's output contract — because its flags changed under it. That is
//! the failure doctor exists to catch *before* a real review spends tokens
//! discovering it, and it is invisible to a version check.
//!
//! # A missing prerequisite is only a problem for what needs it
//!
//! Subversion missing on a machine with no SVN repositories is a fact, not a
//! fault. Reporting it as a failure trains people to ignore the report, which
//! costs more than the line saves.

use serde::{Deserialize, Serialize};

/// How a checked thing is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Working.
    Ok,
    /// Working, with something worth knowing.
    Warn,
    /// Not working, and something depends on it.
    Fail,
    /// Absent, and nothing configured needs it.
    NotNeeded,
}

impl Health {
    /// The marker the human report prints.
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Ok => "ok  ",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
            Self::NotNeeded => "n/a ",
        }
    }

    /// Whether this should make `doctor` exit non-zero.
    ///
    /// Only `Fail`. A warning that fails the command is a warning nobody leaves
    /// in, and `NotNeeded` is the whole point of distinguishing it from `Fail`.
    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::Fail)
    }
}

/// One checked thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// What was checked, e.g. `engine:claude-code` or `svn`.
    pub name: String,
    /// How it is doing.
    pub health: Health,
    /// What was found.
    pub detail: String,
    /// What to type next. Present on every non-`Ok` check that a user can act on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Check {
    /// A passing check.
    pub fn ok(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_owned(),
            health: Health::Ok,
            detail: detail.to_owned(),
            remediation: None,
        }
    }

    /// A check that failed, with what to do about it.
    pub fn fail(name: &str, detail: &str, remediation: &str) -> Self {
        Self {
            name: name.to_owned(),
            health: Health::Fail,
            detail: detail.to_owned(),
            remediation: Some(remediation.to_owned()),
        }
    }

    /// Something worth knowing that is not stopping anything.
    pub fn warn(name: &str, detail: &str, remediation: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            health: Health::Warn,
            detail: detail.to_owned(),
            remediation: remediation.map(str::to_owned),
        }
    }

    /// Absent, and nothing needs it.
    pub fn not_needed(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_owned(),
            health: Health::NotNeeded,
            detail: detail.to_owned(),
            remediation: None,
        }
    }

    /// The line the human report prints.
    pub fn line(&self) -> String {
        let mut out = format!(
            "  [{}] {}: {}",
            self.health.marker(),
            self.name,
            self.detail
        );
        if let Some(remediation) = &self.remediation {
            out.push_str(&format!("\n         try: {remediation}"));
        }
        out
    }
}

/// What `doctor` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Tools rev-local shells out to.
    pub prerequisites: Vec<Check>,
    /// One per configured engine (§8.4).
    pub engines: Vec<Check>,
    /// One per configured MCP target (§11.2).
    pub targets: Vec<Check>,
    /// Anything platform-specific worth saying.
    pub platform: Vec<Check>,
}

impl DoctorReport {
    /// Every check, in report order.
    pub fn all(&self) -> impl Iterator<Item = &Check> {
        self.prerequisites
            .iter()
            .chain(self.engines.iter())
            .chain(self.targets.iter())
            .chain(self.platform.iter())
    }

    /// Whether anything is actually broken.
    pub fn has_failures(&self) -> bool {
        self.all().any(|check| check.health.is_blocking())
    }

    /// How many checks are in each state.
    pub fn tally(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for check in self.all() {
            match check.health {
                Health::Ok => counts.0 += 1,
                Health::Warn => counts.1 += 1,
                Health::Fail => counts.2 += 1,
                Health::NotNeeded => counts.3 += 1,
            }
        }
        counts
    }

    /// The human report.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        for (title, checks) in [
            ("Prerequisites", &self.prerequisites),
            ("Engines", &self.engines),
            ("Publish targets", &self.targets),
            ("Platform", &self.platform),
        ] {
            if checks.is_empty() {
                continue;
            }
            out.push_str(&format!("{title}\n"));
            for check in checks {
                out.push_str(&check.line());
                out.push('\n');
            }
            out.push('\n');
        }

        let (ok, warn, fail, na) = self.tally();
        out.push_str(&format!(
            "{ok} ok, {warn} warning(s), {fail} failure(s), {na} not needed\n"
        ));
        if !self.has_failures() {
            // Saying so explicitly matters: a report that ends in silence looks
            // like a report that stopped early.
            out.push_str("Nothing is blocking a review.\n");
        }
        out
    }
}

/// Turn an engine probe into a check (§8.4).
///
/// The three failure modes are kept apart because they need different actions:
/// install it, log in to it, or find out why its output stopped parsing.
pub fn engine_check(
    id: &str,
    installed: bool,
    version: Option<&str>,
    authenticated: bool,
    honours_contract: Option<bool>,
    problems: &[(String, String)],
) -> Check {
    let name = format!("engine:{id}");

    if !installed {
        let remediation = problems
            .first()
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| format!("install {id}, or set `engines.{id}.bin` to its full path"));
        return Check::fail(&name, "not installed", &remediation);
    }

    let version = version.unwrap_or("version unknown");

    if !authenticated {
        // D9: report, never fix. Offering to log in would mean asking for
        // credentials this product exists not to hold.
        return Check::fail(
            &name,
            &format!("installed ({version}) but not logged in"),
            &format!("log in with `{id}` itself; rev-local stores no API keys (decision D9)"),
        );
    }

    match honours_contract {
        Some(false) => Check::fail(
            &name,
            &format!("installed ({version}) and logged in, but its smoke task produced no usable result.json"),
            &format!(
                "run `{id}` by hand and compare its output to §8.2's contract; a CLI \
                 whose flags changed passes a version check and fails every review"
            ),
        ),
        // §8.4's smoke task costs tokens, so it is opt-in. Not having run it is a
        // warning rather than a pass — the distinction doctor exists to draw.
        None => Check::warn(
            &name,
            &format!("installed ({version}) and logged in; output contract not verified"),
            Some("run `revlocal doctor --smoke` to spend a few tokens checking it"),
        ),
        Some(true) => Check::ok(&name, &format!("{version}, logged in, output contract verified")),
    }
}

/// Turn a target's capability mapping into a check (§11.2).
pub fn target_check(
    name: &str,
    reachable: bool,
    tools: usize,
    mapped: usize,
    unmapped: usize,
) -> Check {
    let check_name = format!("target:{name}");

    if !reachable {
        return Check::fail(
            &check_name,
            "could not connect",
            &format!("check `targets.{name}` in config.toml, then `revlocal targets list`"),
        );
    }
    if unmapped > 0 {
        // §11.2: unmapped is only useful if somebody can see it. A target that
        // binds four of five capabilities publishes fine until a run needs the
        // fifth.
        return Check::warn(
            &check_name,
            &format!("{tools} tool(s), {mapped} capability/ies mapped, {unmapped} unmapped"),
            Some(&format!(
                "run `revlocal targets list --json` to see which, then \
                 `revlocal targets map {name} <capability> --tool T`"
            )),
        );
    }
    Check::ok(
        &check_name,
        &format!("{tools} tool(s), all {mapped} capability/ies mapped"),
    )
}

/// Whether a missing `svn` is a problem here (§6.4).
///
/// Only for the repositories that need it. Reporting an absent tool nobody uses as
/// a failure trains people to ignore the report, which costs more than the line
/// saves.
pub fn svn_check(available: bool, svn_repos: usize) -> Check {
    match (available, svn_repos) {
        (true, 0) => Check::ok("svn", "available (no SVN repositories configured)"),
        (true, n) => Check::ok("svn", &format!("available ({n} SVN repository/ies configured)")),
        (false, 0) => Check::not_needed(
            "svn",
            "not installed, and no SVN repositories are configured; git is unaffected",
        ),
        (false, n) => Check::fail(
            "svn",
            &format!("NOT INSTALLED, and {n} SVN repository/ies are configured — those cannot be reviewed"),
            "install Subversion (brew install subversion / apt-get install subversion / \
             winget install --id CollabNet.Subversion); git repositories are unaffected",
        ),
    }
}

/// Whether a required tool is present.
pub fn required_tool_check(tool: &str, available: bool, needed_for: &str, install: &str) -> Check {
    if available {
        return Check::ok(tool, "available");
    }
    Check::fail(
        tool,
        &format!("not installed, and it is required for {needed_for}"),
        install,
    )
}

// --- gathering the report from the real machine ---------------------------

/// Whether a program can be run at all.
///
/// `--version` rather than a PATH lookup: a binary that is present but cannot
/// execute — the wrong architecture, a broken symlink, a WSL shim with no
/// distribution — is not available, and only running it says so.
fn can_run(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Build the report for this machine.
///
/// `svn_repos` is how many configured repositories have `kind = 'svn'`; it is the
/// caller's because doctor should not open a database to answer a question about
/// prerequisites.
pub fn gather(svn_repos: usize) -> DoctorReport {
    let prerequisites = vec![
        required_tool_check(
            "git",
            can_run("git", &["--version"]),
            "every git and GitHub repository",
            "install git (brew install git / apt-get install git / winget install --id Git.Git)",
        ),
        svn_check(can_run("svn", &["--version", "--quiet"]), svn_repos),
        // node is the fixture engine's runtime, not the product's. Absent, real
        // reviews still work and the mock does not — worth saying, not worth
        // failing.
        if can_run("node", &["--version"]) {
            Check::ok("node", "available (used by the fixture engine)")
        } else {
            Check::warn(
                "node",
                "not installed; the fixture engine cannot run, real engines are unaffected",
                Some("install Node.js only if you want to run rev-local's own test fixtures"),
            )
        },
    ];

    let platform = vec![platform_check()];

    DoctorReport {
        prerequisites,
        // Engines and targets need config, which the caller supplies. Empty here
        // rather than absent, so the JSON shape does not change once they arrive.
        engines: Vec::new(),
        targets: Vec::new(),
        platform,
    }
}

/// Anything worth saying about this platform specifically.
fn platform_check() -> Check {
    #[cfg(windows)]
    {
        // §8.5's Job Object is unimplemented (REVL-106), so a timed-out or
        // cancelled engine can leave a grandchild running. A user deserves to know
        // that before they hit the kill switch and find something still going.
        Check::warn(
            "platform:windows",
            "process-group termination is not implemented, so a cancelled engine \
             may leave a child process running",
            Some(
                "check Task Manager for stray `node` or CLI processes after a \
                 cancellation; tracked as REVL-106",
            ),
        )
    }
    #[cfg(not(windows))]
    {
        Check::ok(
            &format!("platform:{}", std::env::consts::OS),
            "process-group termination available",
        )
    }
}

/// Render for whichever output the caller asked for.
pub fn render(report: &DoctorReport, json: bool) -> Result<String, serde_json::Error> {
    if json {
        // §14: exactly one JSON document reaches stdout.
        return serde_json::to_string_pretty(report);
    }
    Ok(report.render_human())
}
