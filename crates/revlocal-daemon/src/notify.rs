//! Native notifications, and the limit that keeps them worth reading
//! (RL-1111, SPEC §15).
//!
//! # A rate limit is a cap, so it reports what it dropped
//!
//! §15 wants a notification when a high-severity finding lands and when an action
//! starts waiting for approval. A backfill over a year of history produces
//! hundreds of both, and a person who is shown hundreds of notifications learns to
//! dismiss them without reading — which costs more than never having sent any.
//!
//! So there is a limit. And because §18 applies to notifications exactly as it
//! applies to anything else, the limit **counts what it suppressed and says so**.
//! A rate limiter that quietly drops the fourth notification of the minute is a
//! silent cap, and the one it drops is as likely to be the important one as any
//! other. [`Decision::Summarise`] is what makes it not silent: the next
//! notification that gets through carries "and 47 more", which is a fact somebody
//! can act on.
//!
//! # Deduplicating by fingerprint, not by message
//!
//! §10.3's fingerprint is what makes "the same finding" mean something across
//! runs. Two runs over the same unfixed bug produce two findings and one problem,
//! and notifying twice is telling somebody about a thing they already know.

use std::collections::VecDeque;

use revlocal_core::{Severity, Timestamp};
use serde::{Deserialize, Serialize};

/// How many notifications may be shown per window.
///
/// Four an hour. Low enough that a backfill cannot fill the screen, high enough
/// that a normal day's genuinely urgent findings all arrive. Not a silent cap:
/// see [`Decision::Summarise`].
pub const PER_WINDOW: usize = 4;

/// The window the limit applies over, in seconds.
pub const WINDOW_SECS: i64 = 3_600;

/// The severity at or above which a finding is worth interrupting somebody for.
///
/// `High`, not `Medium`. §10.1 puts four levels below critical, and a
/// notification for every medium-severity style note is a notification people turn
/// off — which loses the critical ones too.
pub const NOTIFY_AT: Severity = Severity::High;

/// Why rev-local wants to interrupt somebody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    /// A finding at or above [`NOTIFY_AT`] (§10.1).
    Finding {
        /// How bad.
        severity: Severity,
        /// §10.3's fingerprint, which is what "the same finding" means.
        fingerprint: String,
        /// One line.
        title: String,
        /// Which repository.
        repo: String,
    },
    /// An action is waiting for a person (§12.4).
    Approval {
        /// Which action.
        action_id: i64,
        /// Where it would go — §15: an outbound action names its target.
        target: String,
        /// What it would do.
        capability: String,
    },
}

impl Reason {
    /// The key two notifications must share to be the same notification.
    ///
    /// A finding is identified by its fingerprint rather than its title, because
    /// §10.3's fingerprint is the thing that survives a reword. An approval is
    /// identified by its action id, because two actions that happen to target the
    /// same place are two decisions.
    pub fn key(&self) -> String {
        match self {
            Self::Finding { fingerprint, .. } => format!("finding:{fingerprint}"),
            Self::Approval { action_id, .. } => format!("approval:{action_id}"),
        }
    }

    /// Whether this is worth interrupting somebody for at all.
    pub fn is_worth_showing(&self) -> bool {
        match self {
            Self::Finding { severity, .. } => *severity >= NOTIFY_AT,
            // Every approval is, by construction: §12.4 queued it *because* it
            // needs a person, and one nobody is told about waits until it expires.
            Self::Approval { .. } => true,
        }
    }

    /// The title and body a notification carries.
    pub fn render(&self) -> (String, String) {
        match self {
            Self::Finding {
                severity,
                title,
                repo,
                ..
            } => (
                format!("{} finding in {repo}", severity.as_str()),
                title.clone(),
            ),
            Self::Approval {
                target, capability, ..
            } => (
                "Waiting for your approval".to_owned(),
                // Named explicitly, per §15: "are you sure?" tells nobody
                // anything, and the target is the fact that decides the answer.
                format!("{capability} → {target}"),
            ),
        }
    }
}

/// What to do about one reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Show it.
    Show {
        /// The notification's title.
        title: String,
        /// Its body.
        body: String,
    },
    /// Show it, and say how many were held back while the limit was in force.
    ///
    /// §18. A limiter that dropped the rest without saying so would leave
    /// somebody believing they had seen everything.
    Summarise {
        /// The notification's title.
        title: String,
        /// Its body, including the count that was suppressed.
        body: String,
        /// How many were suppressed since the last thing shown.
        suppressed: usize,
    },
    /// Not shown, and counted.
    Suppressed {
        /// Why — the limit, a duplicate, or not severe enough.
        reason: String,
    },
}

/// Decides what to show, and remembers enough to be honest about the rest.
#[derive(Debug, Default)]
pub struct Notifier {
    /// When each notification in the current window was shown.
    shown: VecDeque<Timestamp>,
    /// Keys already notified about, newest last.
    seen: VecDeque<String>,
    /// How many have been held back since the last one that got through.
    suppressed: usize,
}

/// How many keys to remember for deduplication.
///
/// Bounded so a long-running app does not grow a set forever. The oldest key
/// falling out means a finding nobody has seen in a very long time can notify
/// again, which is the right way round: the alternative is a memory leak that
/// silently stops notifying.
const KEY_MEMORY: usize = 512;

impl Notifier {
    /// A notifier that has shown nothing.
    ///
    /// `const` so it can live in a `static`: the limit is about one person's
    /// attention, and a per-caller notifier would show as many notifications as
    /// there are callers.
    pub const fn new() -> Self {
        Self {
            shown: VecDeque::new(),
            seen: VecDeque::new(),
            suppressed: 0,
        }
    }

    /// How many have been held back since the last notification got through.
    pub const fn pending_suppressed(&self) -> usize {
        self.suppressed
    }

    /// Decide what to do with one reason, at `now`.
    pub fn consider(&mut self, reason: &Reason, now: Timestamp) -> Decision {
        if !reason.is_worth_showing() {
            // Not counted as suppressed. Nothing was held back — it was never
            // going to be shown, and folding it into "and 47 more" would inflate
            // a number somebody is meant to trust.
            return Decision::Suppressed {
                reason: format!("below {}", NOTIFY_AT.as_str()),
            };
        }

        let key = reason.key();
        if self.seen.contains(&key) {
            return Decision::Suppressed {
                reason: "already notified about this one".to_owned(),
            };
        }

        self.expire(now);

        if self.shown.len() >= PER_WINDOW {
            self.suppressed += 1;
            // Remembered even though it was not shown, so a backfill that raises
            // the same finding twice does not consume the limit twice.
            self.remember(key);
            return Decision::Suppressed {
                reason: format!(
                    "the last {PER_WINDOW} notifications were within the hour; \
                     {} held back so far",
                    self.suppressed
                ),
            };
        }

        let (title, body) = reason.render();
        self.shown.push_back(now);
        self.remember(key);

        if self.suppressed > 0 {
            let held = std::mem::take(&mut self.suppressed);
            return Decision::Summarise {
                title,
                body: format!("{body}\n\nand {held} more while notifications were limited"),
                suppressed: held,
            };
        }

        Decision::Show { title, body }
    }

    /// Drop the record of notifications older than the window.
    fn expire(&mut self, now: Timestamp) {
        while let Some(oldest) = self.shown.front() {
            if (now - *oldest).num_seconds() >= WINDOW_SECS {
                self.shown.pop_front();
            } else {
                break;
            }
        }
    }

    fn remember(&mut self, key: String) {
        if self.seen.len() >= KEY_MEMORY {
            self.seen.pop_front();
        }
        self.seen.push_back(key);
    }
}

/// What the tray says about the daemon right now (§12.1, §15).
///
/// The paused state has to be *visible* without opening the window. A kill switch
/// somebody pressed, that left no trace in the only part of the app still on
/// screen, is one they press again to be sure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayStatus {
    /// Reviewing normally.
    Running,
    /// Paused: nothing is being reviewed and publish actions are held.
    Paused,
}

impl TrayStatus {
    /// Which state a paused flag means.
    pub const fn of(paused: bool) -> Self {
        if paused {
            Self::Paused
        } else {
            Self::Running
        }
    }

    /// The tooltip the tray icon carries.
    ///
    /// The state is in the text rather than only in an icon, because a tray icon
    /// that changes shade is exactly the kind of signal people stop noticing —
    /// and this one means "nothing is being reviewed".
    pub const fn tooltip(self) -> &'static str {
        match self {
            Self::Running => "rev-local — reviewing",
            Self::Paused => "rev-local — PAUSED, nothing is being reviewed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(minute: u32) -> Timestamp {
        chrono::Utc
            .with_ymd_and_hms(2026, 8, 30, 12, minute, 0)
            .single()
            .unwrap_or_default()
    }

    fn finding(fingerprint: &str, severity: Severity) -> Reason {
        Reason::Finding {
            severity,
            fingerprint: fingerprint.to_owned(),
            title: "SQL injection in find_user".to_owned(),
            repo: "acme".to_owned(),
        }
    }

    fn approval(id: i64) -> Reason {
        Reason::Approval {
            action_id: id,
            target: "github".to_owned(),
            capability: "post_review".to_owned(),
        }
    }

    #[test]
    fn notify_a_backfill_cannot_spam_the_user() {
        // The acceptance criterion. Twenty findings in a minute is exactly what a
        // backfill over a year of history produces.
        let mut notifier = Notifier::new();

        let shown = (0..20)
            .filter(|i| {
                matches!(
                    notifier.consider(&finding(&format!("fp-{i}"), Severity::Critical), at(1)),
                    Decision::Show { .. } | Decision::Summarise { .. }
                )
            })
            .count();

        assert_eq!(shown, PER_WINDOW);
    }

    #[test]
    fn notify_what_was_held_back_is_counted_and_said() {
        // §18: a rate limit is a cap, and a cap that drops silently leaves
        // somebody believing they have seen everything. The one it dropped is as
        // likely to be the important one as any other.
        let mut notifier = Notifier::new();

        for i in 0..10 {
            notifier.consider(&finding(&format!("fp-{i}"), Severity::Critical), at(1));
        }
        assert_eq!(notifier.pending_suppressed(), 6);

        // An hour later the window has rolled and the next one gets through —
        // carrying the count.
        match notifier.consider(
            &finding("fp-late", Severity::Critical),
            at(1) + chrono::Duration::seconds(WINDOW_SECS),
        ) {
            Decision::Summarise {
                suppressed, body, ..
            } => {
                assert_eq!(suppressed, 6);
                assert!(body.contains("6 more"), "{body}");
            }
            other => panic!("expected a summary, got {other:?}"),
        }

        // And the count resets, so the next notification does not repeat it.
        assert_eq!(notifier.pending_suppressed(), 0);
    }

    #[test]
    fn notify_the_window_rolls_rather_than_locking_out_forever() {
        // A limiter that never forgets is a mute button somebody did not press.
        let mut notifier = Notifier::new();
        for i in 0..PER_WINDOW {
            notifier.consider(&finding(&format!("fp-{i}"), Severity::Critical), at(0));
        }

        let later = at(0) + chrono::Duration::seconds(WINDOW_SECS + 1);
        assert!(matches!(
            notifier.consider(&finding("fp-new", Severity::Critical), later),
            Decision::Show { .. }
        ));
    }

    #[test]
    fn notify_the_same_finding_twice_is_told_once() {
        // §10.3: two runs over one unfixed bug produce two findings and one
        // problem. Notifying twice is telling somebody about a thing they know.
        let mut notifier = Notifier::new();

        assert!(matches!(
            notifier.consider(&finding("fp-1", Severity::Critical), at(0)),
            Decision::Show { .. }
        ));
        assert!(matches!(
            notifier.consider(&finding("fp-1", Severity::Critical), at(5)),
            Decision::Suppressed { .. }
        ));
    }

    #[test]
    fn notify_a_medium_finding_is_not_worth_interrupting_for() {
        // A notification for every style note is a notification people turn off,
        // which loses the critical ones too.
        let mut notifier = Notifier::new();

        assert!(matches!(
            notifier.consider(&finding("fp-1", Severity::Medium), at(0)),
            Decision::Suppressed { .. }
        ));
        // And it did not consume the limit, nor inflate the held-back count: it
        // was never going to be shown.
        assert_eq!(notifier.pending_suppressed(), 0);
        assert!(matches!(
            notifier.consider(&finding("fp-2", Severity::High), at(0)),
            Decision::Show { .. }
        ));
    }

    #[test]
    fn notify_every_approval_is_worth_showing() {
        // §12.4 queued it *because* it needs a person. One nobody is told about
        // waits until it expires, which is a decision made by nobody.
        let mut notifier = Notifier::new();

        match notifier.consider(&approval(7), at(0)) {
            Decision::Show { body, .. } => {
                // §15: an outbound action names its target.
                assert!(body.contains("github"), "{body}");
                assert!(body.contains("post_review"), "{body}");
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    #[test]
    fn notify_two_approvals_for_one_target_are_two_notifications() {
        // Two actions that happen to go the same place are two decisions, and
        // deduplicating them would hide one behind the other.
        let mut notifier = Notifier::new();

        assert!(matches!(
            notifier.consider(&approval(1), at(0)),
            Decision::Show { .. }
        ));
        assert!(matches!(
            notifier.consider(&approval(2), at(0)),
            Decision::Show { .. }
        ));
    }

    #[test]
    fn notify_a_suppressed_duplicate_does_not_consume_the_limit() {
        let mut notifier = Notifier::new();
        notifier.consider(&finding("fp-1", Severity::Critical), at(0));

        for _ in 0..10 {
            notifier.consider(&finding("fp-1", Severity::Critical), at(0));
        }

        // Three of the four slots are still free, because a repeat of something
        // already told is not something held back.
        assert_eq!(notifier.pending_suppressed(), 0);
        assert!(matches!(
            notifier.consider(&finding("fp-2", Severity::Critical), at(0)),
            Decision::Show { .. }
        ));
    }

    #[test]
    fn tray_says_paused_in_words() {
        // A tray icon that changes shade is the kind of signal people stop
        // noticing, and this one means "nothing is being reviewed".
        assert_eq!(TrayStatus::of(true), TrayStatus::Paused);
        assert!(TrayStatus::Paused.tooltip().contains("PAUSED"));
        assert!(TrayStatus::Running.tooltip().contains("reviewing"));
        assert_ne!(TrayStatus::Paused.tooltip(), TrayStatus::Running.tooltip());
    }
}
