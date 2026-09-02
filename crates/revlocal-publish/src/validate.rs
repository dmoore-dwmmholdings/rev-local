//! Checking a payload before somebody approves it (RL-1516, SPEC §12.4).
//!
//! # The inbox could offer an Approve button that could not work
//!
//! `publish_action.payload_json` is a string. Dispatch parses it into whatever
//! shape the target expects, and until this existed that was the *first* time
//! anybody looked. Three actions on a real install carried a payload written by
//! an older version — `{title, body, fingerprint}` where `AndareTarget` wanted
//! `{draft, context, recurrence_body}` — and sat in the inbox looking approvable.
//! Approving one would have marked it failed and nothing else.
//!
//! It is not only stale data. §12.4 lets a person edit the payload before
//! approving it, so a typo in a JSON body is a live way to produce an action that
//! cannot be sent, discovered only after the approval that was supposed to be the
//! deliberate moment.
//!
//! # This parses, it does not re-render
//!
//! §12.4's rule is that the inbox shows exactly what would be sent. Nothing here
//! rewrites a payload or fills anything in — it answers one question, "will the
//! target be able to read this", and says why not.

use revlocal_core::Capability;

use crate::andare::{AndarePayload, OutcomePayload};
use crate::github::{GitHubIssue, ReviewPayload};
use crate::local::ReportPayload;
use crate::trama::PagePayload;

/// Whether `payload_json` is something `target` could actually send.
///
/// `Ok(())` for a target this code does not know: a payload it cannot check is
/// not a payload it should condemn, and refusing to show an action because its
/// target is unfamiliar would make adding a target a breaking change to the
/// inbox.
pub fn validate_payload(
    target: &str,
    capability: Capability,
    payload_json: &str,
) -> Result<(), String> {
    // The shape is chosen the same way the target chooses it at dispatch, so the
    // two cannot disagree about what a given action means.
    let parsed = match (target, capability) {
        ("andare", Capability::SetStatus | Capability::Comment) => {
            shape::<OutcomePayload>(payload_json, "an outcome report")
        }
        ("andare", _) => shape::<AndarePayload>(payload_json, "an Andare issue"),
        ("github", Capability::CreateIssue) => shape::<GitHubIssue>(payload_json, "a GitHub issue"),
        ("github", _) => shape::<ReviewPayload>(payload_json, "a GitHub review"),
        ("report", _) => shape::<ReportPayload>(payload_json, "a local report"),
        ("trama", _) => shape::<PagePayload>(payload_json, "a Trama page"),
        _ => Ok(()),
    };

    parsed.map_err(|detail| {
        format!(
            "this cannot be sent as it stands — {detail}\n  try: edit the payload, or reject it"
        )
    })
}

/// Parse into `T`, describing what was expected rather than what serde called it.
fn shape<T: serde::de::DeserializeOwned>(payload_json: &str, what: &str) -> Result<(), String> {
    serde_json::from_str::<T>(payload_json)
        .map(|_| ())
        // serde's message names the missing field, which is the useful half; the
        // prefix says what the payload was supposed to be, which serde cannot know.
        .map_err(|error| format!("it is not {what}: {error}"))
}
