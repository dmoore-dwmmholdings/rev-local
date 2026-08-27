//! Acceptance tests for `RL-103` — serde round-trips for every domain enum.
//!
//! Each enum is checked three ways, because they can fail independently:
//!
//! 1. `value -> JSON -> value` is the identity, for **every** variant;
//! 2. the JSON is exactly the wire literal from SPEC §5's `CHECK` constraint,
//!    which is what stops a variant rename from changing the stored string;
//! 3. `Display` and `FromStr` agree with serde, so the SQLite text path and the
//!    JSON path cannot drift apart.

use revlocal_core::{
    AutonomyMode, Capability, Category, ChangeKind, Depth, EngineKind, FindingState,
    PublishActionStatus, RepoKind, RiskClass, RunStatus, Severity, TriggerSource, Verdict,
};
use std::fmt::Debug;
use std::str::FromStr;

/// Round-trip every variant of one enum through JSON, `Display` and `FromStr`.
///
/// Returns `Err` rather than unwrapping: this is a helper, not a `#[test]` fn, so
/// clippy's unwrap/expect ban still applies to it (ADR 0003). The caller does the
/// unwrapping inside the test.
fn round_trip<T>(all: &[T], wire_names: &[&str]) -> Result<(), String>
where
    T: serde::Serialize + serde::de::DeserializeOwned + FromStr + Debug + Copy + PartialEq,
    <T as FromStr>::Err: Debug,
{
    assert_eq!(
        all.len(),
        wire_names.len(),
        "ALL and WIRE_NAMES must stay the same length"
    );

    for (value, wire) in all.iter().zip(wire_names) {
        let json = serde_json::to_string(value).map_err(|e| format!("{value:?}: {e}"))?;
        assert_eq!(
            json,
            format!("\"{wire}\""),
            "{value:?} must serialize as the SPEC §5 wire literal"
        );

        let back: T = serde_json::from_str(&json).map_err(|e| format!("{json}: {e}"))?;
        assert_eq!(back, *value, "{value:?} did not survive a JSON round-trip");

        let parsed = T::from_str(wire).map_err(|e| format!("{wire}: {e:?}"))?;
        assert_eq!(parsed, *value, "FromStr disagrees with serde for {wire:?}");
    }
    Ok(())
}

/// Round-trip an enum and confirm it rejects a value outside its `CHECK` list.
macro_rules! check_enum {
    ($name:ident) => {{
        round_trip($name::ALL, $name::WIRE_NAMES)
            .unwrap_or_else(|e| panic!("{} round-trip failed: {e}", stringify!($name)));
        assert_eq!(
            $name::ALL.len(),
            $name::WIRE_NAMES
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "{} has duplicate wire spellings",
            stringify!($name)
        );
        assert!(
            $name::from_str("definitely-not-a-variant").is_err(),
            "{} must reject a value its CHECK constraint would reject",
            stringify!($name)
        );
        assert!(
            serde_json::from_str::<$name>("\"definitely-not-a-variant\"").is_err(),
            "{} must reject an unknown value from JSON too",
            stringify!($name)
        );
    }};
}

#[test]
fn every_domain_enum_round_trips_through_json_display_and_fromstr() {
    check_enum!(RepoKind);
    check_enum!(EngineKind);
    check_enum!(AutonomyMode);
    check_enum!(ChangeKind);
    check_enum!(RunStatus);
    check_enum!(Depth);
    check_enum!(TriggerSource);
    check_enum!(Severity);
    check_enum!(Category);
    check_enum!(FindingState);
    check_enum!(Capability);
    check_enum!(PublishActionStatus);
    check_enum!(RiskClass);
    check_enum!(Verdict);
}

#[test]
fn display_matches_the_wire_spelling() {
    for (value, wire) in Severity::ALL.iter().zip(Severity::WIRE_NAMES) {
        assert_eq!(&value.to_string(), wire);
    }
}

#[test]
fn a_rejected_value_names_the_alternatives() {
    let err = Severity::from_str("catastrophic").expect_err("not a severity");
    let message = err.to_string();
    assert!(
        message.contains("catastrophic"),
        "error must quote the bad value: {message}"
    );
    assert!(
        message.contains("Severity"),
        "error must name the type: {message}"
    );
    assert!(
        message.contains("critical"),
        "error must list the alternatives: {message}"
    );
}

#[test]
fn autonomy_mode_orders_least_to_most_permissive() {
    // SPEC §12.2: off < dry_run < auto_low_ask_high < auto.
    assert!(AutonomyMode::Off < AutonomyMode::DryRun);
    assert!(AutonomyMode::DryRun < AutonomyMode::AutoLowAskHigh);
    assert!(AutonomyMode::AutoLowAskHigh < AutonomyMode::Auto);
}

#[test]
fn the_global_mode_is_a_ceiling_on_the_repo_mode() {
    // A repo cannot be more autonomous than the app allows.
    assert_eq!(
        AutonomyMode::effective(AutonomyMode::DryRun, AutonomyMode::Auto),
        AutonomyMode::DryRun,
        "global dry_run must cap a repo asking for auto"
    );
    // ...and the ceiling does not promote a cautious repo.
    assert_eq!(
        AutonomyMode::effective(AutonomyMode::Auto, AutonomyMode::Off),
        AutonomyMode::Off
    );
    assert!(!AutonomyMode::Off.runs_reviews());
    assert!(AutonomyMode::DryRun.runs_reviews());
}

#[test]
fn severity_orders_least_to_most_severe() {
    assert!(Severity::Info < Severity::Low);
    assert!(Severity::Medium < Severity::High);
    assert!(Severity::High < Severity::Critical);
    assert_eq!(
        Severity::ALL.iter().copied().max(),
        Some(Severity::Critical),
        "max() over severities must be the worst one"
    );
    assert!(Severity::Critical.is_blocking() && Severity::High.is_blocking());
    assert!(!Severity::Medium.is_blocking());
}

#[test]
fn verdict_follows_the_spec_mapping() {
    // SPEC §10.2: request_changes if any critical/high; comment if any medium/low;
    // approve otherwise.
    assert_eq!(Verdict::from_severities([]), Verdict::Approve);
    assert_eq!(Verdict::from_severities([Severity::Info]), Verdict::Approve);
    assert_eq!(
        Verdict::from_severities([Severity::Info, Severity::Low]),
        Verdict::Comment
    );
    assert_eq!(
        Verdict::from_severities([Severity::Low, Severity::Critical]),
        Verdict::RequestChanges
    );
    assert_eq!(
        Verdict::from_severities([Severity::High, Severity::Medium]),
        Verdict::RequestChanges,
        "a high finding wins regardless of what follows it"
    );
}

#[test]
fn depth_orders_shallowest_first() {
    assert!(Depth::Summary < Depth::Standard);
    assert!(Depth::Standard < Depth::Deep);
}

#[test]
fn terminal_run_statuses_are_exactly_the_four_that_stop() {
    let terminal: Vec<_> = RunStatus::ALL
        .iter()
        .copied()
        .filter(|s| s.is_terminal())
        .collect();
    assert_eq!(
        terminal,
        vec![
            RunStatus::Done,
            RunStatus::Failed,
            RunStatus::Skipped,
            RunStatus::Cancelled
        ]
    );
}
