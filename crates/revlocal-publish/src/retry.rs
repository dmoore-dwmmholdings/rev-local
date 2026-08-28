//! Retry policy and idempotent replay (RL-702, SPEC §11.6).
//!
//! §11.6 fixes the shape: five attempts, exponential backoff with jitter from one
//! second to a sixty-second cap, retried only on transport, 5xx and rate-limit
//! failures. 4xx is terminal.
//!
//! # Jitter comes from the action's identity, not from a random number generator
//!
//! The failure jitter exists to prevent is a thundering herd: a target goes down,
//! fifty actions fail within the same second, and every one of them retries at the
//! same instant — then again, together, at two seconds, and four. What breaks that
//! up is that two *different* actions must not choose the same delay. Nothing
//! about it requires unpredictability.
//!
//! So the jitter is derived from `(action id, attempt)` through a fixed mixer.
//! Different actions decorrelate, the same action varies across its own attempts,
//! and the result is reproducible — which matters twice over here. ADR 0024 makes
//! determinism a property of this system, and a randomised backoff would make the
//! "two concurrent retries do not align" criterion a statistical claim that a test
//! could only sample rather than assert.

use std::time::Duration;

use revlocal_core::PublishActionId;

/// SPEC §11.6: five attempts, then the action is failed.
pub const MAX_ATTEMPTS: u32 = 5;

/// The delay before the first retry.
pub const BASE_DELAY: Duration = Duration::from_secs(1);

/// The ceiling backoff will not exceed.
pub const MAX_DELAY: Duration = Duration::from_secs(60);

/// How far either side of the computed delay jitter may move it.
///
/// A quarter. Enough to separate a burst of actions that failed together, small
/// enough that the backoff curve still means what it says.
pub const JITTER_FRACTION: f64 = 0.25;

/// When to try again, and when to stop.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// How many attempts an action gets in total.
    pub max_attempts: u32,
    /// The delay before the first retry.
    pub base: Duration,
    /// The ceiling.
    pub cap: Duration,
    /// How far jitter may move a delay, as a fraction of it.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: MAX_ATTEMPTS,
            base: BASE_DELAY,
            cap: MAX_DELAY,
            jitter: JITTER_FRACTION,
        }
    }
}

impl RetryPolicy {
    /// How long to wait before attempt number `attempts + 1`, or `None` when the
    /// action has had all the attempts it gets.
    ///
    /// `attempts` is the count already made, which is what the `publish_action`
    /// row holds after `record_outcome` — so a caller passes what it just read
    /// rather than having to reason about off-by-one.
    ///
    /// A target that said how long to wait is obeyed instead: `Retry-After` is
    /// the one case where the remote end knows better than the curve, and
    /// backing off less than it asked is how a rate limit becomes a ban.
    pub fn next_delay(
        &self,
        id: PublishActionId,
        attempts: u32,
        retry_after: Option<Duration>,
    ) -> Option<Duration> {
        if attempts >= self.max_attempts {
            return None;
        }

        if let Some(asked) = retry_after {
            return Some(asked.min(self.cap));
        }

        // 1s, 2s, 4s, 8s … capped. `saturating_sub` because attempts is at least
        // 1 by the time a retry is being considered, but a caller passing 0 should
        // get the base delay rather than a panic.
        let exponent = attempts.saturating_sub(1).min(16);
        let scaled = self
            .base
            .saturating_mul(2_u32.saturating_pow(exponent))
            .min(self.cap);

        Some(apply_jitter(scaled, self.jitter, jitter_unit(id, attempts)))
    }

    /// Whether this action has run out of attempts.
    pub const fn is_exhausted(&self, attempts: u32) -> bool {
        attempts >= self.max_attempts
    }
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

/// A value in `[0, 1)` derived from an action and its attempt number.
///
/// splitmix64's finaliser, chosen because it is four lines, has no dependency,
/// and is fixed forever — `DefaultHasher` is explicitly not stable across Rust
/// releases, which would make a retry schedule change under a compiler upgrade.
fn jitter_unit(id: PublishActionId, attempt: u32) -> f64 {
    let mut z = (id.get() as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(attempt));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;

    // 53 bits is what an f64 can hold exactly.
    (z >> 11) as f64 / (1_u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jitter_unit_stays_in_range() {
        for id in 1..200_i64 {
            for attempt in 0..5_u32 {
                let unit = jitter_unit(PublishActionId::new(id), attempt);
                assert!((0.0..1.0).contains(&unit), "id={id} attempt={attempt}");
            }
        }
    }

    #[test]
    fn jitter_never_pushes_a_delay_negative_or_past_the_band() {
        for unit in [0.0, 0.5, 0.999] {
            let out = apply_jitter(Duration::from_secs(8), 0.25, unit);
            assert!(out >= Duration::from_secs(6), "{out:?}");
            assert!(out <= Duration::from_secs(10), "{out:?}");
        }
    }
}
