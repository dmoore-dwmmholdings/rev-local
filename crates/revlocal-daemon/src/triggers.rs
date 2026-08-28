//! The trigger bus and its coalescing window (RL-1001, SPEC §7).
//!
//! # One-way flow, and why it is the whole point
//!
//! §7: *a trigger never reviews directly — it schedules discovery, discovery
//! creates Changes, Changes enqueue Runs*. That is not layering for its own sake.
//! A developer commits, which fires a post-commit hook; the poll interval elapses
//! a moment later; a webhook arrives for the same push; and somebody clicks
//! "review now" because they are watching. Four sources, one commit. If any of
//! them could start a review, that commit gets reviewed four times, costs four
//! times as much, and files each finding four times.
//!
//! Coalescing is what makes the one-way flow sufficient rather than merely tidy.
//!
//! # Coalescing is per repo, and that is a correctness property
//!
//! Two repositories that happen to be triggered in the same millisecond are two
//! independent pieces of work. Collapsing them would drop one entirely — not delay
//! it, *drop* it, because the survivor's discovery pass looks only at its own
//! repository. So the window is keyed by `RepoId` and the test for it asserts two
//! passes, not one.
//!
//! # An event during a pass is not an event that can be discarded
//!
//! Discovery reads the repository at a moment in time. A commit landing *during*
//! that read may or may not be seen, and the bus cannot tell which. Assuming it
//! was seen loses the change until something else happens to trigger the repo —
//! which, on a quiet afternoon, can be hours.
//!
//! So an event arriving mid-pass schedules exactly one follow-up: one, because
//! ten commits during a long pass are still one repository to re-read; at least
//! one, because zero risks losing them all.
//!
//! # A window that closes, not a pass that starts
//!
//! §7 says events within the window *collapse into one discovery pass*, and that
//! forces the shape: the first event **opens a window**, it does not start a pass.
//! Starting immediately and treating everything after it as a follow-up would turn
//! four simultaneous triggers into two passes — the first, and one more for the
//! three that arrived while it ran. Two is better than four and is still not one.
//!
//! So `admit` schedules and `due_passes` collects. The caller drives the clock,
//! which is also what makes the window testable without waiting for it.
//!
//! # Time is injected
//!
//! The window is compared against a clock the caller supplies, so the tests assert
//! coalescing behaviour rather than assert that sleeping for 1.5 seconds works.
//! ADR 0024: a test that sleeps is a test that is flaky on a loaded CI runner.

use std::collections::BTreeMap;

use revlocal_core::{RepoId, Timestamp, TriggerSource};

/// SPEC §7's default coalescing window.
pub const DEFAULT_COALESCE_WINDOW_MS: u64 = 1500;

/// What every trigger source produces (SPEC §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerEvent {
    /// Which repository was triggered.
    pub repo_id: RepoId,
    /// Where it came from.
    pub source: TriggerSource,
    /// A sha, PR number or revision, when the source knew one.
    ///
    /// Advisory only. Discovery does not trust it — a hook can be fired by hand
    /// with any string in it, and a hint that turned into a lookup key would let a
    /// local script make the daemon fetch an arbitrary ref.
    pub hint: Option<String>,
    /// When the bus received it.
    pub received_at: Timestamp,
}

impl TriggerEvent {
    /// An event with no hint.
    pub fn new(repo_id: RepoId, source: TriggerSource, received_at: Timestamp) -> Self {
        Self {
            repo_id,
            source,
            hint: None,
            received_at,
        }
    }

    /// An event carrying a hint.
    #[must_use]
    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_owned());
        self
    }
}

/// What the bus decided to do with an event.
///
/// Every event produces one of these — none is silently dropped, and the variants
/// exist so "nothing happened" is distinguishable from "it was folded into work
/// already scheduled" (§18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Opened a coalescing window. A pass becomes due when it closes.
    Scheduled {
        /// The repository.
        repo_id: RepoId,
        /// When the pass becomes due.
        due_at: Timestamp,
    },
    /// Folded into a window already open.
    Coalesced {
        /// The repository.
        repo_id: RepoId,
        /// How many events have now folded in, including this one.
        folded: usize,
    },
    /// A pass is running; this joined the single follow-up.
    Queued {
        /// The repository.
        repo_id: RepoId,
        /// Whether this event created the follow-up, as opposed to joining one
        /// already scheduled.
        first: bool,
    },
}

impl Admission {
    /// Whether this event opened a new window.
    pub const fn is_scheduled(&self) -> bool {
        matches!(self, Self::Scheduled { .. })
    }

    /// The repository this concerns.
    pub const fn repo_id(&self) -> RepoId {
        match self {
            Self::Scheduled { repo_id, .. }
            | Self::Coalesced { repo_id, .. }
            | Self::Queued { repo_id, .. } => *repo_id,
        }
    }
}

/// One discovery pass the caller should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPass {
    /// The repository to read.
    pub repo_id: RepoId,
    /// Every source that contributed, in arrival order.
    ///
    /// Kept so the run record can say "poll + hook + webhook" rather than
    /// crediting whichever happened to arrive first.
    pub sources: Vec<TriggerSource>,
}

/// One repository's coalescing state.
#[derive(Debug, Clone)]
struct RepoState {
    /// When the window that is currently open began.
    window_opened_at: Option<Timestamp>,
    /// Sources that have arrived inside the open window.
    pending_sources: Vec<TriggerSource>,
    /// Whether a discovery pass is running right now.
    pass_running: bool,
    /// Whether an event arrived during the running pass.
    follow_up_scheduled: bool,
    /// Sources that arrived during the running pass.
    follow_up_sources: Vec<TriggerSource>,
}

impl RepoState {
    const fn new() -> Self {
        Self {
            window_opened_at: None,
            pending_sources: Vec::new(),
            pass_running: false,
            follow_up_scheduled: false,
            follow_up_sources: Vec::new(),
        }
    }
}

/// Coalesces trigger events into discovery passes (SPEC §7).
///
/// Deliberately synchronous and clock-injected. The bus makes a decision about an
/// event; running the pass is the caller's job. That split is what lets the
/// coalescing rules be tested without a runtime, a sleep, or a real repository.
#[derive(Debug)]
pub struct TriggerBus {
    window_ms: u64,
    repos: BTreeMap<RepoId, RepoState>,
}

impl TriggerBus {
    /// A bus with the given window. Zero means no coalescing at all.
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms,
            repos: BTreeMap::new(),
        }
    }

    /// The configured window.
    pub const fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Offer an event to the bus.
    ///
    /// The returned [`Admission`] says whether to start a pass. It never says
    /// "ignore this" — see the type's docs.
    pub fn admit(&mut self, event: &TriggerEvent) -> Admission {
        let window_ms = self.window_ms;
        let state = self
            .repos
            .entry(event.repo_id)
            .or_insert_with(RepoState::new);

        // A pass is running. Everything that arrives now becomes part of exactly
        // one follow-up, because discovery may or may not have seen the change
        // that caused this event and the bus cannot tell which.
        if state.pass_running {
            let first = !state.follow_up_scheduled;
            state.follow_up_scheduled = true;
            state.follow_up_sources.push(event.source);
            return Admission::Queued {
                repo_id: event.repo_id,
                first,
            };
        }

        match state.window_opened_at {
            // Inside an open window: fold in.
            Some(opened) if within(opened, event.received_at, window_ms) => {
                state.pending_sources.push(event.source);
                Admission::Coalesced {
                    repo_id: event.repo_id,
                    folded: state.pending_sources.len(),
                }
            }
            // No window, or the previous one has expired. This event opens one;
            // anything arriving inside it folds in, and the pass becomes due when
            // it closes.
            _ => {
                state.window_opened_at = Some(event.received_at);
                state.pending_sources = vec![event.source];
                Admission::Scheduled {
                    repo_id: event.repo_id,
                    due_at: event.received_at + chrono::Duration::milliseconds(window_ms as i64),
                }
            }
        }
    }

    /// Every pass whose coalescing window has closed by `now`.
    ///
    /// Marks each as running, so a second call before [`pass_finished`] returns
    /// nothing for that repository. Ordered by `RepoId`, which comes from the
    /// `BTreeMap` rather than from a sort at the end — ADR 0024's point is that
    /// the ordering should be a property of the storage, not something restored
    /// afterwards by whoever remembers to.
    ///
    /// [`pass_finished`]: Self::pass_finished
    pub fn due_passes(&mut self, now: Timestamp) -> Vec<DiscoveryPass> {
        let window_ms = self.window_ms;
        self.repos
            .iter_mut()
            .filter_map(|(repo_id, state)| {
                if state.pass_running || state.pending_sources.is_empty() {
                    return None;
                }
                let opened = state.window_opened_at?;
                if within(opened, now, window_ms) {
                    return None;
                }
                state.pass_running = true;
                Some(DiscoveryPass {
                    repo_id: *repo_id,
                    sources: state.pending_sources.clone(),
                })
            })
            .collect()
    }

    /// Tell the bus a discovery pass finished.
    ///
    /// Returns the follow-up pass to run, if events arrived while it was running.
    /// Exactly one, however many arrived — ten commits during a long pass are still
    /// one repository to re-read.
    ///
    /// The follow-up is returned directly rather than opening another window: the
    /// events are already older than a window by the time the pass they arrived
    /// during has finished, and making them wait again would delay a change behind
    /// its own trigger twice.
    pub fn pass_finished(&mut self, repo_id: RepoId, at: Timestamp) -> Option<DiscoveryPass> {
        let state = self.repos.get_mut(&repo_id)?;

        state.pass_running = false;
        state.pending_sources.clear();

        if !state.follow_up_scheduled {
            state.window_opened_at = None;
            return None;
        }

        let sources = std::mem::take(&mut state.follow_up_sources);
        state.follow_up_scheduled = false;
        state.window_opened_at = Some(at);
        state.pending_sources = sources.clone();
        state.pass_running = true;

        Some(DiscoveryPass { repo_id, sources })
    }

    /// The sources folded into the pass currently open for a repository.
    ///
    /// For the run record: a pass triggered by a poll, a hook and a webhook should
    /// say so rather than crediting whichever arrived first.
    pub fn pending_sources(&self, repo_id: RepoId) -> Vec<TriggerSource> {
        self.repos
            .get(&repo_id)
            .map(|state| state.pending_sources.clone())
            .unwrap_or_default()
    }

    /// Whether a pass is running for a repository.
    pub fn is_pass_running(&self, repo_id: RepoId) -> bool {
        self.repos
            .get(&repo_id)
            .is_some_and(|state| state.pass_running)
    }

    /// Whether a follow-up pass is waiting for the running one to finish.
    pub fn has_follow_up(&self, repo_id: RepoId) -> bool {
        self.repos
            .get(&repo_id)
            .is_some_and(|state| state.follow_up_scheduled)
    }
}

impl Default for TriggerBus {
    fn default() -> Self {
        Self::new(DEFAULT_COALESCE_WINDOW_MS)
    }
}

/// Whether `later` is within `window_ms` of `opened`.
///
/// A negative difference — an event stamped before the window opened, which a
/// clock adjustment can produce — counts as inside it. The alternative is opening
/// a second window for an event that arrived first, which is the one outcome
/// coalescing exists to prevent.
fn within(opened: Timestamp, later: Timestamp, window_ms: u64) -> bool {
    let elapsed = later.signed_duration_since(opened).num_milliseconds();
    elapsed < 0 || (elapsed as u64) < window_ms
}
