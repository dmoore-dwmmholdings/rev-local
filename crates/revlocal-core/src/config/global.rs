//! The global config document — `{config_dir}/rev-local/config.toml` (SPEC §13.1).

use super::{collect_unknown_keys, ConfigWarning, Extra, SecretRef};
use crate::AutonomyMode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

string_enum! {
    /// What to do when a repo exhausts its daily budget (SPEC §13.1, decision D10).
    ///
    /// There is deliberately no "drop" variant: D10 says exhaustion never silently
    /// drops a change, so a config cannot ask for one.
    pub enum OnExhausted {
        /// Stop reviewing this repo until the budget rolls over.
        Pause => "pause",
        /// Hold changes and review them when budget returns.
        Queue => "queue",
        /// Record the change as skipped, with a reason.
        Skip => "skip",
    }
}

/// `[global]` (SPEC §13.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalSettings {
    /// The ceiling on every repo's autonomy (SPEC §12.2).
    pub mode: AutonomyMode,
    /// Semaphore size for the run queue (SPEC §4.3).
    pub max_concurrent_runs: u32,
    /// How long to wait for further changes before starting a run.
    pub coalesce_window_ms: u64,
    /// Loopback port the git-hook receiver listens on.
    pub trigger_port: u16,
    /// Webhook port; `0` disables it.
    pub webhook_port: u16,
    /// Transcripts older than this are pruned on startup (SPEC §5.1).
    pub transcript_retention_days: u32,
    /// Keep the scratch worktree when a run fails, for debugging (SPEC §6.1).
    pub keep_scratch_on_failure: bool,
    /// A run stuck this long is considered stale.
    pub stale_run_minutes: u32,
    /// How many attempts a change gets before recovery gives up (§9.1, ADR 0012).
    ///
    /// Without a ceiling, a change that crashes the daemon is recovered on every
    /// startup, crashes again, and rev-local spends its life re-reviewing one
    /// commit while never reaching the rest. RL-501 recorded that gap and put the
    /// value in the daemon as a *parameter* rather than a call-site constant, so
    /// it could become config without a rewrite. This is that.
    ///
    /// Giving up is announced with a reason (§18): a change that stops being
    /// reviewed with no record is indistinguishable from one that was reviewed and
    /// found clean.
    pub max_attempts: u32,
    /// How long a queued approval waits before expiring (SPEC §12.4).
    pub approval_ttl_hours: u32,
    /// Actions per hour above which a repo's actions escalate (SPEC §12.3).
    pub burst_threshold: u32,
    /// Keys present in the file that this version does not know.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for GlobalSettings {
    /// Exactly the document in SPEC §13.1.
    fn default() -> Self {
        Self {
            mode: AutonomyMode::AutoLowAskHigh,
            max_concurrent_runs: 2,
            coalesce_window_ms: 1500,
            trigger_port: 41791,
            webhook_port: 0,
            transcript_retention_days: 30,
            keep_scratch_on_failure: true,
            stale_run_minutes: 10,
            max_attempts: 3,
            approval_ttl_hours: 72,
            burst_threshold: crate::DEFAULT_BURST_THRESHOLD,
            extra: Extra::default(),
        }
    }
}

/// `[budgets]` (SPEC §13.1, decision D10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetSettings {
    /// Daily token allowance per repo.
    pub daily_tokens_per_repo: u64,
    /// Daily run allowance per repo.
    pub daily_runs_per_repo: u32,
    /// Daily USD allowance per repo; `0` means unlimited.
    pub daily_cost_usd_per_repo: f64,
    /// What happens when an allowance runs out. Never a silent drop.
    pub on_exhausted: OnExhausted,
    /// Keys present in the file that this version does not know.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for BudgetSettings {
    /// Exactly the document in SPEC §13.1.
    fn default() -> Self {
        Self {
            daily_tokens_per_repo: 2_000_000,
            daily_runs_per_repo: 200,
            daily_cost_usd_per_repo: 0.0,
            on_exhausted: OnExhausted::Pause,
            extra: Extra::default(),
        }
    }
}

impl BudgetSettings {
    /// Whether a limit of `0` means "unlimited" for cost (SPEC §13.1).
    pub fn cost_is_unlimited(&self) -> bool {
        self.daily_cost_usd_per_repo <= 0.0
    }
}

/// How to reach one MCP server (SPEC §13.1, §11.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerSettings {
    /// `stdio` or `http`.
    #[serde(rename = "type")]
    pub transport: String,
    /// Executable, for `stdio`.
    pub command: Option<String>,
    /// Arguments, for `stdio`.
    pub args: Vec<String>,
    /// Endpoint, for `http`.
    pub url: Option<String>,
    /// Headers, which may carry `{{keychain:name}}` placeholders.
    pub headers: BTreeMap<String, SecretRef>,
    /// Keys present in the file that this version does not know.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            transport: "stdio".to_owned(),
            command: None,
            args: Vec::new(),
            url: None,
            headers: BTreeMap::new(),
            extra: Extra::default(),
        }
    }
}

/// The global configuration document.
///
/// Engine and target sections are held as opaque tables for now: SPEC §13.1 defers
/// their shapes to §8.4 and §11.3–11.5, and inventing a schema here would let this
/// file and those sections drift. They are preserved verbatim across a load/save
/// round-trip so nothing a user wrote is lost.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalConfig {
    /// `[global]`.
    pub global: GlobalSettings,
    /// `[budgets]`.
    pub budgets: BudgetSettings,
    /// `[engines.*]` — shape defined by SPEC §8.4.
    pub engines: BTreeMap<String, toml::Value>,
    /// `[mcpServers.*]`. Note the camelCase key, which matches the MCP convention
    /// rather than the rest of this document.
    #[serde(rename = "mcpServers")]
    pub mcp_servers: BTreeMap<String, McpServerSettings>,
    /// `[targets.*]` — shapes defined by SPEC §11.3–11.5.
    pub targets: BTreeMap<String, toml::Value>,
    /// Top-level keys this version does not know.
    #[serde(flatten)]
    pub extra: Extra,
}

impl GlobalConfig {
    /// Parse a global config document.
    ///
    /// Unknown keys are collected as warnings rather than rejected: a config
    /// written for a newer rev-local must still start an older one, and a typo
    /// should tell the user rather than refuse to boot (SPEC §18's "no silent
    /// caps" cuts the other way here — the warning is what stops it being silent).
    pub fn parse(toml_text: &str) -> Result<(Self, Vec<ConfigWarning>), toml::de::Error> {
        let config: Self = toml::from_str(toml_text)?;
        let warnings = config.warnings();
        Ok((config, warnings))
    }

    /// Every unknown key found in the document, as warnings.
    pub fn warnings(&self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        collect_unknown_keys(&self.extra, "", &mut warnings);
        collect_unknown_keys(&self.global.extra, "global", &mut warnings);
        collect_unknown_keys(&self.budgets.extra, "budgets", &mut warnings);
        for (name, server) in &self.mcp_servers {
            collect_unknown_keys(&server.extra, &format!("mcpServers.{name}"), &mut warnings);
        }
        warnings
    }
}
