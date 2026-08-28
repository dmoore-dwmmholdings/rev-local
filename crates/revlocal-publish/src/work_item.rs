//! Reporting a review outcome onto the work item a change names (RL-706, §11.4).
//!
//! §11.4: if the commit message, PR title or SVN log contains a key matching
//! `repo.config.andare_key_regex`, the run reports its outcome onto that ticket —
//! a comment always, and a status transition when `andare_transition_on` maps the
//! verdict to a state name.
//!
//! # The comment goes first, and it goes even when the transition will not
//!
//! A transition can fail for reasons that have nothing to do with rev-local: the
//! project's workflow may not allow that move from the ticket's current state, the
//! state may have been renamed, the ticket may be closed. None of that makes the
//! review outcome less worth recording. So the comment is posted first and the
//! transition attempted second, which means a rejected transition leaves a ticket
//! that still says what the review found.
//!
//! Doing it the other way round — transition, then comment — loses the comment
//! whenever the transition is refused, which is precisely when somebody most needs
//! to read why.
//!
//! # A transition is high risk, always
//!
//! §12.3 already says so via `ActionIntent::SetStatus`, and this module does not
//! restate the classification — it uses it. Moving somebody's ticket is a visible
//! change to shared state that other people's work queues are built on.

use std::collections::BTreeMap;

use regex::Regex;
use revlocal_core::{ActionIntent, RiskClass, Verdict};
use serde::{Deserialize, Serialize};

/// A compiled `andare_key_regex`.
#[derive(Debug, Clone)]
pub struct KeyPattern {
    regex: Regex,
}

/// Why a key pattern could not be used.
#[derive(Debug, thiserror::Error)]
pub enum KeyPatternError {
    /// The configured pattern is not a valid regular expression.
    #[error("`andare_key_regex` is not a valid regular expression: {source}\n  try: the default is [A-Z][A-Z0-9]+-\\d+")]
    Invalid {
        /// Why the engine refused it.
        #[source]
        source: Box<regex::Error>,
    },
}

impl KeyPattern {
    /// Compile a pattern.
    pub fn new(pattern: &str) -> Result<Self, KeyPatternError> {
        Regex::new(pattern)
            .map(|regex| Self { regex })
            .map_err(|source| KeyPatternError::Invalid {
                source: Box::new(source),
            })
    }

    /// The default from SPEC §13.1.
    pub fn default_pattern() -> Result<Self, KeyPatternError> {
        Self::new(r"[A-Z][A-Z0-9]+-\d+")
    }

    /// Every distinct key in `text`, in the order they appear.
    ///
    /// Deduplicated, because a commit that says "REVL-42" in its subject and again
    /// in its body means one ticket, and commenting twice on it is noise a person
    /// has to read.
    ///
    /// Keys inside URLs count. Pasting the ticket's URL is one of the two common
    /// ways people reference a ticket, and refusing it would skip the case most
    /// worth handling. The cost is that a URL which merely *looks* like a key can
    /// produce a comment attempt on a ticket that does not exist — which fails,
    /// visibly, against that one key, and leaves the rest of the report intact.
    pub fn keys(&self, text: &str) -> Vec<String> {
        let mut seen = Vec::new();
        for found in self.regex.find_iter(text) {
            let key = found.as_str().to_owned();
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        seen
    }
}

/// Where a review outcome should be reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeReport {
    /// The work item.
    pub key: String,
    /// What to say. Always present (§11.4: a comment always).
    pub comment: String,
    /// The state to move it to, when configured.
    pub transition: Option<String>,
}

impl OutcomeReport {
    /// The risk class of this report's transition, per §12.3.
    ///
    /// `None` when there is no transition — a comment is a comment.
    pub fn transition_risk(&self) -> Option<RiskClass> {
        self.transition
            .as_ref()
            .map(|_| ActionIntent::SetStatus.baseline_risk())
    }
}

/// The state a verdict maps to, per `andare_transition_on`.
///
/// Absent from the map means "do not transition", which is the default: §11.4
/// makes the comment unconditional and the transition opt-in, and a tool that
/// moved tickets without being asked would be rearranging somebody's board.
pub fn transition_for(verdict: Verdict, transition_on: &BTreeMap<String, String>) -> Option<&str> {
    transition_on.get(verdict.as_str()).map(String::as_str)
}

/// The comment left on a work item.
pub fn outcome_comment(
    verdict: Verdict,
    findings: usize,
    change_ref: Option<&str>,
    review_url: Option<&str>,
) -> String {
    let headline = match (verdict, findings) {
        (Verdict::Approve, 0) => "rev-local reviewed this change and found nothing.".to_owned(),
        (Verdict::Approve | Verdict::Comment, n) => {
            format!("rev-local reviewed this change and found {n} finding(s), none blocking.")
        }
        (Verdict::RequestChanges, n) => {
            format!(
                "rev-local reviewed this change and found {n} finding(s), including blocking ones."
            )
        }
    };

    let mut body = format!("{headline}\n\n");
    if let Some(reference) = change_ref {
        body.push_str(&format!("- Change: {reference}\n"));
    }
    if let Some(url) = review_url {
        body.push_str(&format!("- Review: {url}\n"));
    }
    body
}

/// Everything a run wants to report onto the work items its change names.
///
/// One report per distinct key. Each becomes its own publish action so each has
/// its own idempotency key and its own retry budget — a ticket that has been
/// deleted must not stop the other tickets in the same commit from being updated.
pub fn plan_outcomes(
    text: &str,
    pattern: &KeyPattern,
    verdict: Verdict,
    findings: usize,
    transition_on: &BTreeMap<String, String>,
    change_ref: Option<&str>,
    review_url: Option<&str>,
) -> Vec<OutcomeReport> {
    let comment = outcome_comment(verdict, findings, change_ref, review_url);
    let transition = transition_for(verdict, transition_on).map(str::to_owned);

    pattern
        .keys(text)
        .into_iter()
        .map(|key| OutcomeReport {
            key,
            comment: comment.clone(),
            transition: transition.clone(),
        })
        .collect()
}
