//! Poll scheduling: clamping, jitter, backoff and health (RL-1002, SPEC §7.1).
//!
//! # A polling interval is a promise to somebody else's server
//!
//! §7.1 sets a floor of 30 seconds and it is not a performance tuning knob. A repo
//! configured to poll every second is a repo that will be rate-limited by GitHub,
//! and the account it is rate-limiting belongs to the user. So a value below the
//! floor is **clamped and said out loud** rather than honoured or rejected: honour
//! it and rev-local misbehaves on the user's behalf; reject it and a typo in a
//! config file stops the daemon from starting.
//!
//! §18's rule, in the ordinary case where it is easiest to skip — the clamp is
//! obviously right, which is exactly why it would be tempting to apply silently.
//!
//! # Jitter is derived, not random
//!
//! ADR 0029 settled this for publish retries and the same reasoning applies here:
//! the failure jitter exists to prevent is a thundering herd — twenty repositories
//! configured with the same interval, all polling on the same second forever. A
//! random source would fix that and make the schedule untestable and
//! irreproducible.
//!
//! So the offset is derived from `(repo, poll number)` through splitmix64's
//! finaliser: the same fixed four-line mixer, chosen because `DefaultHasher` is
//! explicitly not stable across Rust releases and a poll schedule that shifts under
//! a compiler upgrade is a bug nobody would ever find.
//!
//! # Backing off is not the same as giving up
//!
//! A repository whose remote is unreachable backs off to 30 minutes and reports
//! `degraded`. It does not stop polling, and it does not stop being a repository
//! the user asked to watch. The distinction matters because a laptop that closed
//! its lid on a train comes back, and the first success must return it to its
//! normal interval immediately rather than working the backoff ladder back down.

use std::time::Duration;

use revlocal_core::RepoId;

/// SPEC §7.1's enforced floor.
pub const MIN_POLL_INTERVAL_SECS: u64 = 30;

/// SPEC §7.1's default per-repo interval.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;

/// SPEC §7.1's ceiling for backoff.
pub const MAX_BACKOFF_SECS: u64 = 30 * 60;

/// SPEC §7.1's jitter, as a fraction of the interval.
pub const JITTER_FRACTION: f64 = 0.10;

/// How a repository's polling is going (SPEC §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoHealth {
    /// Polling normally.
    Healthy,
    /// Consecutive failures; backed off and still trying.
    Degraded,
}

impl RepoHealth {
    /// How this reads in `repo show` and the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
        }
    }
}

/// A configured interval, and whether it was the one that was asked for.
///
/// The clamp is a value rather than a side effect so a caller cannot apply it and
/// forget to report it — `warning` is the whole reason this type exists instead of
/// a bare `u64`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollInterval {
    /// The interval actually used.
    pub secs: u64,
    /// What to tell the user, when the configured value was not honoured.
    pub warning: Option<String>,
}

impl PollInterval {
    /// Clamp a configured interval to §7.1's floor, reporting it if it moved.
    pub fn clamp(configured: u64) -> Self {
        if configured < MIN_POLL_INTERVAL_SECS {
            return Self {
                secs: MIN_POLL_INTERVAL_SECS,
                warning: Some(format!(
                    "poll_interval_secs = {configured} is below the {MIN_POLL_INTERVAL_SECS}s \
                     minimum (SPEC §7.1) and was raised to {MIN_POLL_INTERVAL_SECS}s; \
                     polling faster than this gets the account rate-limited"
                )),
            };
        }
        Self {
            secs: configured,
            warning: None,
        }
    }

    /// Whether the configured value was changed.
    pub const fn was_clamped(&self) -> bool {
        self.warning.is_some()
    }
}

impl Default for PollInterval {
    fn default() -> Self {
        Self::clamp(DEFAULT_POLL_INTERVAL_SECS)
    }
}

/// One repository's polling state (SPEC §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollSchedule {
    /// Which repository.
    pub repo_id: RepoId,
    /// The clamped interval.
    pub interval: PollInterval,
    /// How many polls have been attempted, used to vary the jitter.
    pub polls: u64,
    /// Consecutive failures. Zero when healthy.
    pub consecutive_failures: u32,
    /// The last failure's message, for the UI.
    pub last_error: Option<String>,
}

impl PollSchedule {
    /// A schedule for `repo_id` at `configured` seconds, clamped.
    pub fn new(repo_id: RepoId, configured: u64) -> Self {
        Self {
            repo_id,
            interval: PollInterval::clamp(configured),
            polls: 0,
            consecutive_failures: 0,
            last_error: None,
        }
    }

    /// Repo health, as §7.1 reports it.
    pub const fn health(&self) -> RepoHealth {
        if self.consecutive_failures > 0 {
            RepoHealth::Degraded
        } else {
            RepoHealth::Healthy
        }
    }

    /// The base delay before the next poll, before jitter.
    ///
    /// Doubles per consecutive failure and stops at 30 minutes. A repository whose
    /// remote is down should not poll every two minutes forever, and should not
    /// stop either — §7.1 backs off, it does not disable.
    pub fn base_delay(&self) -> Duration {
        let doubled = self
            .interval
            .secs
            .saturating_mul(1_u64 << self.consecutive_failures.min(16));
        Duration::from_secs(doubled.min(MAX_BACKOFF_SECS))
    }

    /// The delay before the next poll, with §7.1's ±10% jitter applied.
    pub fn next_delay(&self) -> Duration {
        apply_jitter(
            self.base_delay(),
            JITTER_FRACTION,
            jitter_unit(self.repo_id, self.polls),
        )
    }

    /// Record a successful poll.
    ///
    /// Recovery is immediate and total. A laptop that closed its lid on a train
    /// comes back, and working a backoff ladder back down would leave it polling
    /// every half hour long after the network returned.
    pub fn succeeded(&mut self) {
        self.polls = self.polls.saturating_add(1);
        self.consecutive_failures = 0;
        self.last_error = None;
    }

    /// Record a failed poll.
    pub fn failed(&mut self, error: &str) {
        self.polls = self.polls.saturating_add(1);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_error = Some(error.to_owned());
    }

    /// What `revlocal repo show --json` reports for this repository.
    pub fn health_report(&self, repo_name: &str) -> HealthReport {
        HealthReport {
            repo: repo_name.to_owned(),
            health: self.health(),
            poll_interval_secs: self.interval.secs,
            configured_interval_clamped: self.interval.was_clamped(),
            next_poll_in_secs: self.next_delay().as_secs(),
            consecutive_failures: self.consecutive_failures,
            last_error: self.last_error.clone(),
            notes: self.interval.warning.clone().into_iter().collect(),
        }
    }
}

/// The machine-readable health of one repository (§7.1, `repo show --json`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HealthReport {
    /// The repository's name.
    pub repo: String,
    /// `healthy` or `degraded`.
    pub health: RepoHealth,
    /// The interval in force, after clamping.
    pub poll_interval_secs: u64,
    /// Whether the configured interval was raised to the floor.
    pub configured_interval_clamped: bool,
    /// Roughly how long until the next poll, jitter included.
    pub next_poll_in_secs: u64,
    /// How many polls have failed in a row.
    pub consecutive_failures: u32,
    /// The most recent failure, if any.
    pub last_error: Option<String>,
    /// Anything the user should know, such as a clamped interval.
    ///
    /// Always present, empty when there is nothing to say — an absent field and an
    /// empty one read the same to a human and differently to a parser.
    pub notes: Vec<String>,
}

/// Move `delay` by up to `fraction` either way, using `unit` in `[0, 1)`.
fn apply_jitter(delay: Duration, fraction: f64, unit: f64) -> Duration {
    if fraction <= 0.0 {
        return delay;
    }
    // `1 - f + 2fu` maps u in [0,1) onto [1-f, 1+f).
    let multiplier = (2.0f64.mul_add(fraction * unit, 1.0) - fraction).max(0.0);
    let millis = delay.as_millis() as f64 * multiplier;
    Duration::from_millis(millis.round().max(0.0) as u64)
}

/// A value in `[0, 1)` derived from a repository and its poll number.
///
/// splitmix64's finaliser, the same mixer ADR 0029 fixed for publish retries.
/// `DefaultHasher` is explicitly not stable across Rust releases, which would make
/// a poll schedule shift under a compiler upgrade.
fn jitter_unit(repo_id: RepoId, poll: u64) -> f64 {
    let mut z = (repo_id.get() as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(poll);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;

    // 53 bits is what an f64 can hold exactly.
    (z >> 11) as f64 / (1_u64 << 53) as f64
}
