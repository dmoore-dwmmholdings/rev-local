//! `revlocal webhook start | stop | status [--tunnel ...]` (RL-1201, SPEC §7.3, §14).
//!
//! The last of §14's sixteen command groups. Everything underneath it was built by
//! RL-1005 and RL-1006 — the signature-checking listener, the delivery log, the
//! tunnel providers — and none of it was reachable from a command line.
//!
//! # Two switches, and `status` reports both
//!
//! §7.3 turns webhooks on in two places: `global.webhook_port` (`0` disables the
//! listener for everything) and each repository's `webhook_enabled` (off by
//! default). Both must be on for a delivery to become a review.
//!
//! A status that showed only the global one would be worse than showing nothing.
//! Port set, tunnel up, no repository opted in is a configuration where every
//! check passes and no review ever happens — which reads exactly like a quiet
//! week. So `status` names the repositories that are opted in, and says plainly
//! when none are.
//!
//! # `start` records intent; it does not hold the socket
//!
//! Same shape as `pause` and `resume`: the command writes state that the daemon
//! reads, because the daemon is the process with the run loop. What `start` will
//! not do is *report* a listener it has not observed. It probes the port instead —
//! a bind that fails with "address in use" is evidence something is listening; a
//! bind that succeeds is evidence nothing is. That is a weaker claim than "rev-local
//! is receiving webhooks" and it is the one that is actually true.
//!
//! # It refuses to start when the port is zero, rather than picking one
//!
//! `webhook_port` lives in the config file (§13.1), and a CLI flag that silently
//! overrode a config file would make the file stop explaining the behaviour. So
//! `start` fails with the line to add and a port that was free when it checked.

use std::collections::BTreeMap;
use std::path::Path;

use revlocal_core::{GlobalConfig, RepoConfig, Timestamp};
use revlocal_daemon::tunnel::{locate, TunnelProvider};
use revlocal_store::{Pool, RepoStore, SettingStore};
use serde::{Deserialize, Serialize};

/// Setting key: whether the operator has asked for the listener to run.
pub const SETTING_WEBHOOK_ENABLED: &str = "webhook.enabled";

/// Setting key: which tunnel provider was chosen.
pub const SETTING_WEBHOOK_TUNNEL: &str = "webhook.tunnel";

/// Why a webhook command could not complete.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The database could not be read or written.
    #[error("could not reach the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// `--tunnel` named a provider that does not exist.
    ///
    /// The known names are listed rather than described: a typo is the common
    /// case, and the fix is visible the moment the right spelling is on screen.
    #[error("unknown tunnel provider {given:?}\n  try: --tunnel {known}")]
    UnknownProvider {
        /// What was asked for.
        given: String,
        /// What exists, joined for the message.
        known: String,
    },

    /// The listener is disabled in config, so there is nothing to start.
    #[error(
        "webhooks are disabled: global.webhook_port is 0 in {path}\n  \
         try: set global.webhook_port = {suggestion} in that file, then run this again"
    )]
    Disabled {
        /// Which config file was read.
        path: String,
        /// A port that was free when this error was built.
        suggestion: u16,
    },

    /// The config file could not be read.
    #[error("could not read {path}: {source}\n  try: revlocal doctor")]
    ReadConfig {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },

    /// The config file is not valid TOML, or not a config.
    #[error("{path} is not a valid config: {source}")]
    Malformed {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> WebhookError {
    WebhookError::Store {
        source: Box::new(source),
    }
}

/// What is actually listening on the webhook port, as far as can be observed.
///
/// Deliberately three states rather than a `bool`. "Nothing is listening" and "the
/// port is 0 so nothing ever will be" have different remedies, and collapsing them
/// sends somebody to restart a daemon that was never meant to bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Listener {
    /// `global.webhook_port` is 0.
    Disabled,
    /// The port is configured and free — nothing is listening on it.
    NotListening,
    /// The port is configured and in use, so something is listening.
    ///
    /// Something, not necessarily rev-local. A bind probe cannot tell the
    /// difference and this name does not pretend it can.
    InUse,
}

impl Listener {
    /// The line the human path prints.
    pub const fn summary_line(self) -> &'static str {
        match self {
            Self::Disabled => "listener: disabled (global.webhook_port = 0)",
            Self::NotListening => "listener: configured, but nothing is bound to the port",
            Self::InUse => "listener: the port is in use — something is listening",
        }
    }
}

/// One repository's side of the second switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoWebhook {
    /// The repository's name.
    pub repo: String,
    /// §13.2's `webhook_enabled`.
    pub enabled: bool,
    /// Whether a secret reference is configured.
    ///
    /// Without one, every delivery fails signature verification — which looks
    /// from the outside like GitHub not sending anything.
    pub secret_configured: bool,
}

/// What a webhook command did or found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookReport {
    /// `start`, `stop` or `status`.
    pub action: String,
    /// Whether the operator has asked for the listener to run.
    pub enabled: bool,
    /// Whether this command changed that, as opposed to it already being so.
    pub changed: bool,
    /// The configured port. `0` means disabled.
    pub port: u16,
    /// The chosen tunnel provider, if one has been chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<String>,
    /// Where the provider's binary was found, if it needs one and it is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_binary: Option<String>,
    /// What is on the port.
    pub listener: Listener,
    /// Every repository and its opt-in state, in name order.
    pub repos: Vec<RepoWebhook>,
    /// A sentence for a person.
    pub detail: String,
    /// What is still needed before a delivery would become a review.
    ///
    /// Empty means nothing is; §18 forbids leaving that to be inferred.
    pub next_steps: Vec<String>,
}

impl WebhookReport {
    /// How many repositories have opted in.
    pub fn opted_in(&self) -> usize {
        self.repos.iter().filter(|r| r.enabled).count()
    }

    /// The block the human path prints.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.detail);
        out.push('\n');
        out.push_str("  ");
        out.push_str(self.listener.summary_line());
        out.push('\n');

        out.push_str("  tunnel: ");
        match (&self.tunnel, &self.tunnel_binary) {
            (None, _) => out.push_str("not chosen"),
            (Some(name), Some(path)) => out.push_str(&format!("{name} ({path})")),
            (Some(name), None) if name == TunnelProvider::Manual.as_str() => {
                out.push_str("manual — you supply the public URL");
            }
            (Some(name), None) => out.push_str(&format!("{name} — NOT INSTALLED")),
        }
        out.push('\n');

        let opted_in = self.opted_in();
        out.push_str(&format!(
            "  repositories opted in: {opted_in} of {}\n",
            self.repos.len()
        ));
        for repo in self.repos.iter().filter(|r| r.enabled) {
            let secret = if repo.secret_configured {
                "secret configured"
            } else {
                "NO SECRET — every delivery will be rejected"
            };
            out.push_str(&format!("    {} — {secret}\n", repo.repo));
        }

        for step in &self.next_steps {
            out.push_str(&format!("  next: {step}\n"));
        }
        out
    }
}

/// Render a report for a person or a script.
pub fn render(report: &WebhookReport, json: bool) -> Result<String, WebhookError> {
    if json {
        serde_json::to_string_pretty(report).map_err(|source| WebhookError::Unrenderable { source })
    } else {
        Ok(report.render_human())
    }
}

/// Read the global config, or use the defaults when the file is absent.
///
/// Absent is not an error: a fresh install has no config file and §13.1's document
/// is the default. Unreadable *is* an error — a file that exists and cannot be
/// read is a different situation from one that was never written.
fn read_config(path: &Path) -> Result<GlobalConfig, WebhookError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GlobalConfig::default())
        }
        Err(source) => {
            return Err(WebhookError::ReadConfig {
                path: path.display().to_string(),
                source,
            })
        }
    };
    GlobalConfig::parse(&text)
        .map(|(config, _warnings)| config)
        .map_err(|source| WebhookError::Malformed {
            path: path.display().to_string(),
            source: Box::new(source),
        })
}

/// Ask the OS whether anything holds `port` on loopback.
///
/// A successful bind is released immediately; it is the answer that is wanted, not
/// the socket. `port == 0` is not probed at all — binding to 0 asks for any free
/// port and would always succeed, which would report "nothing is listening" about
/// a port that does not exist.
async fn probe(port: u16) -> Listener {
    if port == 0 {
        return Listener::Disabled;
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            drop(listener);
            Listener::NotListening
        }
        Err(_) => Listener::InUse,
    }
}

/// A loopback port that was free when this was called.
async fn suggest_port() -> u16 {
    const FALLBACK: u16 = 41792;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener.local_addr().map_or(FALLBACK, |a| a.port()),
        Err(_) => FALLBACK,
    }
}

/// Every repository's opt-in state, in name order.
async fn repo_switches(pool: &Pool) -> Result<Vec<RepoWebhook>, WebhookError> {
    let repos = RepoStore::new(pool).list().await.map_err(boxed)?;
    let mut ordered = BTreeMap::new();
    for repo in repos {
        // A config that does not parse is treated as the defaults, which are off.
        // Reporting a repo as opted in because its config was unreadable would be
        // the one wrong direction to fail in.
        let config = serde_json::from_str::<RepoConfig>(&repo.config_json).unwrap_or_default();
        ordered.insert(
            repo.name.clone(),
            RepoWebhook {
                repo: repo.name,
                enabled: config.webhook_enabled,
                secret_configured: config.webhook_secret_ref.is_some(),
            },
        );
    }
    Ok(ordered.into_values().collect())
}

/// Parse `--tunnel`, naming what exists when it does not parse.
fn parse_provider(name: &str) -> Result<TunnelProvider, WebhookError> {
    TunnelProvider::parse(name).ok_or_else(|| WebhookError::UnknownProvider {
        given: name.to_owned(),
        known: "cloudflared | ngrok | manual".to_owned(),
    })
}

/// Assemble the parts of a report that `start`, `stop` and `status` all share.
async fn survey(
    pool: &Pool,
    config_path: &Path,
    provider: Option<TunnelProvider>,
) -> Result<(u16, Listener, Vec<RepoWebhook>, Option<String>), WebhookError> {
    let port = read_config(config_path)?.global.webhook_port;
    let listener = probe(port).await;
    let repos = repo_switches(pool).await?;
    // A provider whose binary cannot be found is not an error here — it is a
    // finding, and `next_steps` says how to fix it. Refusing to report status
    // because a tunnel is missing would hide the rest of the status.
    let binary = provider
        .and_then(|p| locate(p).ok().flatten())
        .map(|p| p.display().to_string());
    Ok((port, listener, repos, binary))
}

/// What still stands between this configuration and a delivery becoming a review.
fn next_steps(
    enabled: bool,
    listener: Listener,
    provider: Option<TunnelProvider>,
    binary: Option<&str>,
    repos: &[RepoWebhook],
) -> Vec<String> {
    let mut steps = Vec::new();

    if listener == Listener::Disabled {
        steps.push("set global.webhook_port in your config; 0 disables the listener".to_owned());
    }
    if !enabled {
        // Listed even when the config is otherwise complete. "Everything is
        // configured" and "deliveries are wanted" are different statements, and a
        // status that made only the first one is how a correct-looking setup
        // receives nothing.
        steps.push("webhooks are switched off — revlocal webhook start".to_owned());
    }
    if enabled && listener == Listener::NotListening {
        steps.push("start the daemon — revlocal watch — so something binds the port".to_owned());
    }
    match provider {
        None => {
            steps.push("choose a tunnel — revlocal webhook start --tunnel cloudflared".to_owned())
        }
        Some(TunnelProvider::Manual) => steps.push(
            "point your own public URL at the webhook port; manual starts no process".to_owned(),
        ),
        Some(p) if binary.is_none() => steps.push(p.install_hint().to_owned()),
        Some(_) => {}
    }
    if repos.iter().all(|r| !r.enabled) {
        steps.push(
            "no repository has webhook_enabled set; deliveries would arrive and be ignored"
                .to_owned(),
        );
    }
    for repo in repos.iter().filter(|r| r.enabled && !r.secret_configured) {
        steps.push(format!(
            "{}: set webhook_secret_ref, or every delivery fails signature verification",
            repo.repo
        ));
    }
    steps
}

/// `revlocal webhook start [--tunnel P]` (SPEC §7.3).
pub async fn start(
    pool: &Pool,
    config_path: &Path,
    tunnel: Option<&str>,
    at: Timestamp,
) -> Result<WebhookReport, WebhookError> {
    let settings = SettingStore::new(pool);

    // A provider named on the command line is remembered; one omitted keeps
    // whatever was chosen last, so `start` after a `stop` does not silently lose
    // the choice.
    let stored = settings
        .get(SETTING_WEBHOOK_TUNNEL)
        .await
        .map_err(boxed)?
        .and_then(|name| TunnelProvider::parse(&name));
    let provider = match tunnel {
        Some(name) => Some(parse_provider(name)?),
        None => stored,
    };

    let (port, listener, repos, binary) = survey(pool, config_path, provider).await?;
    if listener == Listener::Disabled {
        return Err(WebhookError::Disabled {
            path: config_path.display().to_string(),
            suggestion: suggest_port().await,
        });
    }

    let already =
        settings.get(SETTING_WEBHOOK_ENABLED).await.map_err(boxed)? == Some("true".into());
    settings
        .set(SETTING_WEBHOOK_ENABLED, "true", at)
        .await
        .map_err(boxed)?;
    if let Some(p) = provider {
        settings
            .set(SETTING_WEBHOOK_TUNNEL, p.as_str(), at)
            .await
            .map_err(boxed)?;
    }

    let detail = if already {
        format!("webhooks were already enabled on port {port}")
    } else {
        format!("webhooks enabled on port {port}")
    };
    Ok(WebhookReport {
        action: "start".to_owned(),
        enabled: true,
        changed: !already,
        port,
        tunnel: provider.map(|p| p.as_str().to_owned()),
        tunnel_binary: binary.clone(),
        listener,
        next_steps: next_steps(true, listener, provider, binary.as_deref(), &repos),
        repos,
        detail,
    })
}

/// `revlocal webhook stop` (SPEC §7.3).
///
/// The provider choice is deliberately kept. Stopping is not unconfiguring, and
/// making somebody re-pick their tunnel every time they pause deliveries would
/// teach them to leave it running instead.
pub async fn stop(
    pool: &Pool,
    config_path: &Path,
    at: Timestamp,
) -> Result<WebhookReport, WebhookError> {
    let settings = SettingStore::new(pool);
    let was = settings.get(SETTING_WEBHOOK_ENABLED).await.map_err(boxed)? == Some("true".into());
    settings
        .set(SETTING_WEBHOOK_ENABLED, "false", at)
        .await
        .map_err(boxed)?;

    let provider = settings
        .get(SETTING_WEBHOOK_TUNNEL)
        .await
        .map_err(boxed)?
        .and_then(|name| TunnelProvider::parse(&name));
    let (port, listener, repos, binary) = survey(pool, config_path, provider).await?;

    let detail = if was {
        "webhooks disabled; the tunnel choice is remembered".to_owned()
    } else {
        "webhooks were already disabled".to_owned()
    };
    Ok(WebhookReport {
        action: "stop".to_owned(),
        enabled: false,
        changed: was,
        port,
        tunnel: provider.map(|p| p.as_str().to_owned()),
        tunnel_binary: binary,
        listener,
        repos,
        detail,
        // Nothing is pending when the answer is "it is off on purpose".
        next_steps: Vec::new(),
    })
}

/// `revlocal webhook status [--tunnel P]` (SPEC §7.3).
///
/// `--tunnel` here asks "what would this provider look like?" without choosing it —
/// which is how somebody checks whether `cloudflared` is installed before
/// committing to it.
pub async fn status(
    pool: &Pool,
    config_path: &Path,
    tunnel: Option<&str>,
    _at: Timestamp,
) -> Result<WebhookReport, WebhookError> {
    let settings = SettingStore::new(pool);
    let enabled =
        settings.get(SETTING_WEBHOOK_ENABLED).await.map_err(boxed)? == Some("true".into());
    let stored = settings
        .get(SETTING_WEBHOOK_TUNNEL)
        .await
        .map_err(boxed)?
        .and_then(|name| TunnelProvider::parse(&name));
    let provider = match tunnel {
        Some(name) => Some(parse_provider(name)?),
        None => stored,
    };

    let (port, listener, repos, binary) = survey(pool, config_path, provider).await?;
    let detail = if enabled {
        format!("webhooks are enabled on port {port}")
    } else {
        "webhooks are disabled".to_owned()
    };
    Ok(WebhookReport {
        action: "status".to_owned(),
        enabled,
        changed: false,
        port,
        tunnel: provider.map(|p| p.as_str().to_owned()),
        tunnel_binary: binary.clone(),
        listener,
        next_steps: next_steps(enabled, listener, provider, binary.as_deref(), &repos),
        repos,
        detail,
    })
}
