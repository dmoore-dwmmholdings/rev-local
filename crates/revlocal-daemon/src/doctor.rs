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
///
/// `Default` is an empty report — no checks run — rather than a passing one. A
/// report that defaulted to healthy would let a caller that forgot to run
/// anything look like a caller that ran everything and found it fine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Tools rev-local shells out to.
    pub prerequisites: Vec<Check>,
    /// One per configured engine (§8.4).
    pub engines: Vec<Check>,
    /// One per configured MCP target (§11.2).
    pub targets: Vec<Check>,
    /// Anything platform-specific worth saying.
    pub platform: Vec<Check>,
    /// What the install itself is doing, read from the database (RL-1551).
    ///
    /// Doctor's own help calls it "the thing to run again when reviews have
    /// quietly stopped", and until this existed it checked binaries on `PATH`
    /// and nothing about the install — so on a machine where autopilot had
    /// never been switched on, 51 runs sat queued and two checkouts were gone,
    /// it reported that nothing was blocking a review.
    #[serde(default)]
    pub install: Vec<Check>,
}

impl DoctorReport {
    /// Every check, in report order.
    pub fn all(&self) -> impl Iterator<Item = &Check> {
        self.prerequisites
            .iter()
            .chain(self.engines.iter())
            .chain(self.targets.iter())
            .chain(self.platform.iter())
            .chain(self.install.iter())
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
            ("Install", &self.install),
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
            //
            // But "nothing is blocking a review" is a claim about the install and
            // not only about the tooling, and it was being made on a machine
            // where nothing had run for days (RL-1551). A prerequisite can be
            // perfect while the work sits still.
            if self
                .install
                .iter()
                .any(|check| check.health == Health::Warn)
            {
                out.push_str(
                    "Nothing is blocking a review, but work is not moving — see Install above.\n",
                );
            } else {
                out.push_str("Nothing is blocking a review.\n");
            }
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
            // Not `doctor --smoke`, which this used to name and which does not
            // exist. A remediation that points at a flag somebody cannot type is
            // worse than none: they conclude their install is broken.
            Some("run `revlocal review --repo <path> --rev HEAD --engine <id>` to check it end to end"),
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
        // Filled in by `install_checks` when a database is available; `gather`
        // itself is sync and knows nothing about one.
        install: Vec::new(),
        prerequisites,
        // Engines and targets need config, which the caller supplies. Empty here
        // rather than absent, so the JSON shape does not change once they arrive.
        //
        // Usage reporting is the exception: it is a property of the engine
        // *implementation*, not of anybody's configuration, so it is known
        // without one and belongs in the report a fresh install prints.
        //
        // So is presence. `engines` used to hold only the usage warnings, which
        // meant a machine with both engines installed and measured got an EMPTY
        // engine list from the command §8.4 says "tells the user exactly which
        // engine is usable and why not" — and onboarding's first screen, which
        // shows this report, listed no engines at all.
        engines: {
            let mut checks: Vec<Check> = [
                revlocal_core::EngineKind::Claude,
                revlocal_core::EngineKind::Codex,
            ]
            .into_iter()
            .map(engine_presence)
            .collect();
            checks.extend(usage_checks());
            checks
        },
        targets: Vec::new(),
        platform,
    }
}

/// One check per engine that cannot report its token usage (RL-409, §8.1).
///
/// A warning, never a failure: the engine works and reviews run. What does not
/// work is the **budget**, and somebody who set a daily token ceiling has no way
/// to discover that from anywhere else. `budget show` reports the ledger honestly
/// — it hedges an unmeasured day rather than presenting it as a total — but by the
/// time an operator reads that, they have already trusted a ceiling that was not
/// holding.
///
/// Only unmeasured engines get a line. A check per engine saying "yes, measured"
/// would bury the two that matter under noise, and §8.4's report earns attention
/// by not spending it.
/// Whether an engine's binary is on PATH (§8.4).
///
/// Presence only, and it says so. §8.4's full probe runs the engine's
/// `version_args` and then a smoke task, and a smoke task spends tokens — so
/// `doctor`, which somebody runs to find out why nothing is working, must not
/// bill them for asking. What this can answer for free is the question that is
/// wrong most often: whether the binary is there at all.
///
/// `warn`, not `ok`, when it is found. Installed is not the same as logged in,
/// and a green line against an engine that has never been authenticated is the
/// kind of reassurance that sends somebody looking somewhere else for a day.
///
/// `warn`, not `fail`, when it is **absent** — and that is the more important
/// half. `Health::Fail` is blocking, and `doctor` is the first thing somebody
/// runs on a machine where nothing is set up yet. The first version of this
/// failed on a missing engine, which meant `revlocal doctor` exited non-zero on
/// every machine without Claude Code or Codex installed: every CI runner, and
/// every new user before their first install. RL-1205's criterion is that a user
/// with no configuration reaches a review, and the mock engine gets them there —
/// so an engine nobody has configured yet is news, not a blockage.
pub fn engine_presence(engine: revlocal_core::EngineKind) -> Check {
    let name = format!("engine:{}", engine.as_str());

    match revlocal_engine::live::readiness(engine) {
        revlocal_engine::live::Readiness::Ready { binary } => Check::warn(
            &name,
            &format!("found at {}; not probed", binary.display()),
            Some(&format!(
                "check it end to end with `revlocal review --repo <path> --rev HEAD \
                 --engine {}` — being on PATH is not the same as being logged in",
                engine.as_str()
            )),
        ),
        revlocal_engine::live::Readiness::Skip { reason } => Check::warn(
            &name,
            &reason,
            Some(&format!(
                "install {0}, or point `engines.{0}.bin` at it in your config — \
                 rev-local works without it, reviewing with the mock engine, which \
                 spends nothing and invents its findings",
                engine.as_str()
            )),
        ),
    }
}

fn usage_checks() -> Vec<Check> {
    revlocal_engine::usage::unmeasured_engines()
        .into_iter()
        .map(|(engine, support)| {
            Check::warn(
                &format!("engine:{}:usage", engine.as_str()),
                &support.summary_line(engine),
                Some(
                    "set `budgets.daily_runs_per_repo` too — a run count is measurable for every engine, and a token ceiling alone is not enforceable against this one",
                ),
            )
        })
        .collect()
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

/// What the install itself is doing (RL-1551).
///
/// # Why doctor reads the database at all
///
/// Its own help calls it "the thing to run again when reviews have quietly
/// stopped". Until this existed it checked binaries on `PATH`, engine presence
/// and the platform — all of which can be perfect while no work moves at all.
/// Run against a machine where autopilot had never been switched on, 51 runs sat
/// queued and two checkouts had been deleted, it answered "Nothing is blocking a
/// review."
///
/// These checks are warnings rather than failures. Each describes a state
/// somebody may have chosen: a queue drained by `revlocal watch` from cron needs
/// no autopilot, and a repository on an unmounted drive is not broken. What was
/// wrong was saying nothing, not saying it too gently — so the closing line
/// accounts for them instead.
pub async fn install_checks(
    pool: &revlocal_store::Pool,
    ttl_hours: i64,
    at: revlocal_core::Timestamp,
) -> Vec<Check> {
    let mut checks = Vec::new();

    // Split, not totalled: a run whose checkout is gone is held every tick
    // forever, and counting it as waiting work makes a number that never goes
    // down (RL-1554). `install:repos` below names those repositories, and the
    // two halves of one report must not disagree about the same runs.
    let (queued, blocked) = crate::repos::queued_split(pool).await.unwrap_or_default();

    let autopilot_on = revlocal_store::SettingStore::new(pool)
        .get(crate::autopilot::SETTING_AUTOPILOT)
        .await
        .ok()
        .flatten()
        .as_deref()
        == Some("on");

    checks.push(match (autopilot_on, queued) {
        (true, _) => Check::ok("install:autopilot", "on"),
        // Off with nothing waiting is a quiet install, not a stopped one.
        // Nothing *runnable* waiting. "Nothing is waiting" would still be a
        // false claim while runs sit queued behind a missing checkout, so say
        // which it is — `install:repos` below names the repositories, and the
        // two halves of one report must agree about the same runs.
        (false, 0) if blocked > 0 => Check::ok(
            "install:autopilot",
            &format!("off; the {blocked} queued run(s) are blocked on a missing checkout"),
        ),
        (false, 0) => Check::ok("install:autopilot", "off, and nothing is waiting"),
        (false, n) => Check::warn(
            "install:autopilot",
            &format!(
                "off, and {n} run(s) are queued — nothing is reviewing them{}",
                if blocked > 0 {
                    format!(" ({blocked} more are blocked on a missing checkout)")
                } else {
                    String::new()
                }
            ),
            Some(
                "switch it on from the dashboard, or run `revlocal watch` on a timer if you drive it yourself",
            ),
        ),
    });

    // A repository whose checkout has gone holds its runs forever, and the drain
    // says so every tick. Doctor is where somebody looks first.
    match revlocal_store::RepoStore::new(pool).list().await {
        Err(error) => checks.push(Check::warn(
            "install:repos",
            &format!("could not be read — {error}"),
            Some("check the database with `revlocal db migrate`"),
        )),
        Ok(repos) => {
            // Only the enabled ones: a repository somebody switched off is not a
            // problem to report, and saying so would train people to ignore this.
            let repos: Vec<&revlocal_core::Repo> =
                repos.iter().filter(|repo| repo.enabled).collect();
            let missing: Vec<&revlocal_core::Repo> = repos
                .iter()
                .copied()
                .filter(|repo| {
                    repo.local_path
                        .as_deref()
                        .is_some_and(|path| !std::path::Path::new(path).exists())
                })
                .collect();
            // A kind with no adapter is a different problem from a missing
            // checkout and needs saying separately: the path is fine, and
            // "all present" was true and useless (RL-1557).
            let unrouted: Vec<&str> = repos
                .iter()
                .copied()
                .filter(|repo| revlocal_vcs::unsupported_kind(repo).is_some())
                .map(|repo| repo.name.as_str())
                .collect();
            if !unrouted.is_empty() {
                checks.push(Check::warn(
                    "install:kinds",
                    &format!(
                        "{} rev-local cannot review yet: {}",
                        if unrouted.len() == 1 {
                            "1 repository".to_owned()
                        } else {
                            format!("{} repositories", unrouted.len())
                        },
                        unrouted.join(", ")
                    ),
                    Some("only `git` is wired; point them at a git checkout or disable them"),
                ));
            }

            if missing.is_empty() {
                checks.push(Check::ok(
                    "install:repos",
                    &format!("{} enabled, all present", repos.len()),
                ));
            } else {
                // Named, not counted. "2 repositories are missing" cannot be acted
                // on without going to look them up.
                let names: Vec<&str> = missing.iter().map(|repo| repo.name.as_str()).collect();
                checks.push(Check::warn(
                    "install:repos",
                    &format!("{} checkout(s) gone: {}", missing.len(), names.join(", ")),
                    Some("put them back, point each repository at its new location, or disable it"),
                ));
            }
        }
    }

    // An approval nobody answers is rejected when its time runs out, and the
    // finding goes with it. That is worth saying before it happens, not after.
    if let Ok(waiting) = revlocal_store::PublishActionStore::new(pool)
        .list_awaiting_approval()
        .await
    {
        if waiting.is_empty() {
            checks.push(Check::ok("install:approvals", "nothing waiting"));
        } else {
            let expiring = waiting
                .iter()
                .filter(|action| {
                    crate::approvals::time_left(action.created_at, ttl_hours, at).is_some_and(
                        |words| words.starts_with("past") || words.starts_with("under"),
                    )
                })
                .count();
            let detail = if expiring > 0 {
                format!(
                    "{} waiting, {expiring} about to be discarded",
                    waiting.len()
                )
            } else {
                format!("{} waiting", waiting.len())
            };
            checks.push(Check::warn(
                "install:approvals",
                &detail,
                Some("see them with `revlocal approvals list`, then approve or reject"),
            ));
        }
    }

    checks
}
