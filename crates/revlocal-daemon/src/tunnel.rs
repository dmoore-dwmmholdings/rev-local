//! Tunnel providers and webhook registration (RL-1006, SPEC §7.3).
//!
//! # The URL is read as a URL, not as a sentence
//!
//! ADR 0023's standing rule is: never write a string match against another tool's
//! output without running the tool and reading what it says. So cloudflared was
//! run, and this is what it prints — on **stderr**, inside an ASCII box, behind a
//! timestamp and a log level:
//!
//! ```text
//! 2026-08-28T10:25:19Z INF Requesting new quick Tunnel on trycloudflare.com...
//! 2026-08-28T10:25:23Z INF +------------------------------------------------------+
//! 2026-08-28T10:25:23Z INF |  Your quick Tunnel has been created! Visit it at ...  |
//! 2026-08-28T10:25:23Z INF |  https://involved-therapeutic-adaptive-career.trycloudflare.com  |
//! 2026-08-28T10:25:23Z INF +------------------------------------------------------+
//! ```
//!
//! The trap is in line one. **`trycloudflare.com` appears four seconds before the
//! URL does**, in a progress message, with no scheme. A matcher looking for the
//! bare hostname finds that line first and returns a tunnel address that is not
//! one. I made exactly that mistake while capturing this output, which is the
//! second-best argument for the rule after the rule itself.
//!
//! So extraction matches a **URL shape** — `https://<label>.trycloudflare.com` —
//! rather than any part of cloudflared's prose. The box drawing, the column
//! padding, the timestamp format and the wording of the sentence can all change
//! without breaking it; only the URL's own form matters, and that one is fixed by
//! Cloudflare's DNS rather than by their log formatter.
//!
//! # ngrok is not parsed at all
//!
//! ngrok runs a local HTTP API on `127.0.0.1:4040` that returns its tunnels as
//! JSON. Where a tool offers a machine-readable interface, using it is not an
//! optimisation — it is the difference between a contract and a guess.
//!
//! # A dead tunnel is worse than no tunnel
//!
//! A tunnel that dies leaves the listener bound, healthy, and receiving nothing.
//! Every check passes and no reviews happen — which reads exactly like a quiet
//! week. §7.3 shows tunnel state in the UI for this reason, and [`TunnelHealth`]
//! makes "we had a public URL and no longer do" a state rather than an absence.

use std::path::PathBuf;

use revlocal_core::RiskClass;

/// Where ngrok's local API lives.
pub const NGROK_API: &str = "http://127.0.0.1:4040/api/tunnels";

/// Which tunnel provider (SPEC §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelProvider {
    /// `cloudflared tunnel --url`.
    Cloudflared,
    /// `ngrok http`, read through its local API.
    Ngrok,
    /// The user supplies their own public URL. No binary, no process.
    Manual,
}

impl TunnelProvider {
    /// The binary this provider needs, if any.
    pub const fn binary(self) -> Option<&'static str> {
        match self {
            Self::Cloudflared => Some("cloudflared"),
            Self::Ngrok => Some("ngrok"),
            Self::Manual => None,
        }
    }

    /// How this reads in config and on the command line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cloudflared => "cloudflared",
            Self::Ngrok => "ngrok",
            Self::Manual => "manual",
        }
    }

    /// Parse a provider name from config.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "cloudflared" => Some(Self::Cloudflared),
            "ngrok" => Some(Self::Ngrok),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// How to install it, per platform.
    ///
    /// §18 requires a user-visible error to say what to do. "cloudflared not
    /// found" is a diagnosis; a command to run is a remedy, and the difference is
    /// whether somebody has to go and search for it.
    pub const fn install_hint(self) -> &'static str {
        match self {
            Self::Cloudflared => {
                "install cloudflared — brew install cloudflared (macOS), \
                 winget install --id Cloudflare.cloudflared (Windows), or see \
                 https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"
            }
            Self::Ngrok => {
                "install ngrok — brew install ngrok (macOS), \
                 winget install --id ngrok.ngrok (Windows), or see https://ngrok.com/download"
            }
            Self::Manual => "set tunnel.public_url to a URL that reaches this machine",
        }
    }

    /// The arguments that start a tunnel to `port`.
    pub fn args(self, port: u16) -> Vec<String> {
        let local = format!("http://127.0.0.1:{port}");
        match self {
            Self::Cloudflared => vec![
                "tunnel".to_owned(),
                "--url".to_owned(),
                local,
                // Without this cloudflared may replace its own binary mid-run,
                // which is a surprising thing for a review tool to do to a
                // developer's machine.
                "--no-autoupdate".to_owned(),
            ],
            Self::Ngrok => vec!["http".to_owned(), port.to_string()],
            Self::Manual => Vec::new(),
        }
    }
}

/// Why a tunnel could not start or could not be read.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// The provider's binary is not on PATH.
    ///
    /// Not a crash: §7.3's providers are optional, and a machine without
    /// cloudflared is a machine that should be told how to get it.
    #[error("{provider} is not installed, so the tunnel cannot start\n  try: {hint}")]
    BinaryMissing {
        /// Which provider.
        provider: &'static str,
        /// How to install it.
        hint: &'static str,
    },

    /// The provider started but never announced a URL.
    #[error(
        "{provider} started but did not report a public URL within {seconds}s\n  \
         try: run `{provider} {args}` by hand to see what it says"
    )]
    NoUrl {
        /// Which provider.
        provider: &'static str,
        /// How long was waited.
        seconds: u64,
        /// The arguments used, so the suggestion is copy-pasteable.
        args: String,
    },

    /// `manual` was selected without a URL.
    #[error(
        "the manual tunnel provider needs a public URL\n  \
         try: set tunnel.public_url, or choose the cloudflared or ngrok provider"
    )]
    ManualUrlMissing,
}

/// Whether a provider's binary is available, and where.
///
/// Separate from starting one so the UI can report "cloudflared: not installed"
/// without launching anything.
pub fn locate(provider: TunnelProvider) -> Result<Option<PathBuf>, TunnelError> {
    let Some(binary) = provider.binary() else {
        return Ok(None);
    };

    which(binary).map(Some).ok_or(TunnelError::BinaryMissing {
        provider: provider.as_str(),
        hint: provider.install_hint(),
    })
}

/// Find an executable on PATH.
///
/// Hand-rolled rather than a dependency: this is the only place that needs it, and
/// `PATHEXT` on Windows is the one wrinkle.
fn which(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let extensions: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_owned())
        .split(';')
        .map(|extension| extension.to_lowercase())
        .collect();
    #[cfg(not(windows))]
    let extensions: Vec<String> = vec![String::new()];

    for directory in std::env::split_paths(&path) {
        for extension in &extensions {
            let candidate = directory.join(format!("{binary}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Extract a quick-tunnel URL from a line of cloudflared's output.
///
/// Matches a URL shape, never cloudflared's prose. See the module docs: the bare
/// hostname appears in a progress message four seconds *before* the URL does, so a
/// matcher looking for `trycloudflare.com` returns something that is not a tunnel
/// address.
pub fn cloudflared_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = line.get(start..)?;

    let end = rest.find(|c: char| !is_url_char(c)).unwrap_or(rest.len());
    let url = rest.get(..end)?;

    // A scheme and a host is not enough — cloudflared's banner also contains
    // https://www.cloudflare.com/website-terms/ and a developers.cloudflare.com
    // link, both on stderr, both before the tunnel exists.
    url.strip_suffix('/')
        .unwrap_or(url)
        .ends_with(".trycloudflare.com")
        .then(|| url.trim_end_matches('/').to_owned())
}

/// Characters that may appear in the URLs this reads.
fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '/' | ':' | '_' | '~' | '%')
}

/// Extract the public URL from ngrok's local API response.
///
/// ngrok's `/api/tunnels` returns `{"tunnels":[{"public_url":…,"proto":…}]}`. The
/// https tunnel is preferred: ngrok opens both, GitHub webhooks should not be
/// delivered over plaintext, and taking the first entry would pick whichever ngrok
/// happened to list first.
pub fn ngrok_url(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let tunnels = parsed.get("tunnels")?.as_array()?;

    let url_with_proto = |wanted: &str| {
        tunnels.iter().find_map(|tunnel| {
            (tunnel.get("proto")?.as_str()? == wanted)
                .then(|| tunnel.get("public_url")?.as_str().map(str::to_owned))
                .flatten()
        })
    };

    url_with_proto("https").or_else(|| url_with_proto("http"))
}

/// What a tunnel is doing (SPEC §7.3 — shown in the UI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelHealth {
    /// Not configured.
    Disabled,
    /// Started, no URL yet.
    Starting,
    /// Up, with a public URL.
    Running {
        /// The public URL webhooks should be delivered to.
        url: String,
    },
    /// It had a URL and no longer does.
    ///
    /// Deliberately distinct from `Disabled`. A tunnel that dies leaves the
    /// listener bound, healthy and receiving nothing — every check passes and no
    /// reviews happen, which reads exactly like a quiet week.
    Dead {
        /// The URL it used to have, so the UI can say which one stopped.
        was: Option<String>,
        /// Why, as far as it can be told.
        reason: String,
    },
}

impl TunnelHealth {
    /// The URL webhooks are reaching, if any.
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Running { url } => Some(url),
            _ => None,
        }
    }

    /// Whether deliveries can arrive right now.
    pub const fn is_receiving(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Whether this is worth telling somebody about.
    ///
    /// Only `Dead`. Disabled is a choice and Starting is a moment.
    pub const fn needs_attention(&self) -> bool {
        matches!(self, Self::Dead { .. })
    }

    /// A line for the UI and `revlocal repo show`.
    pub fn summary_line(&self) -> String {
        match self {
            Self::Disabled => "tunnel: not configured".to_owned(),
            Self::Starting => "tunnel: starting".to_owned(),
            Self::Running { url } => format!("tunnel: up at {url}"),
            Self::Dead {
                was: Some(url),
                reason,
            } => {
                format!("tunnel: DOWN (was {url}) — {reason}; webhooks are not arriving")
            }
            Self::Dead { was: None, reason } => {
                format!("tunnel: DOWN — {reason}; webhooks are not arriving")
            }
        }
    }
}

/// Registering a webhook mutates a GitHub repository's settings.
///
/// §12.3 classifies actions by blast radius, and this one writes configuration on
/// somebody else's server that outlives the run, is visible to every admin of the
/// repository, and keeps delivering to a URL after rev-local has forgotten about
/// it. That is `High` whatever the repo's autonomy mode says about reviewing.
pub const fn registration_risk() -> RiskClass {
    RiskClass::High
}

/// The action name recorded for a webhook registration.
pub const REGISTER_WEBHOOK_ACTION: &str = "register_webhook";

/// What registering would do, for the approval prompt.
///
/// A high-risk action is only meaningfully gated if the person approving it can
/// see what they are approving.
pub fn registration_summary(repo_full_name: &str, url: &str, events: &[&str]) -> String {
    format!(
        "Create a webhook on {repo_full_name} delivering {} to {url}/webhook.\n\
         This changes settings on GitHub, is visible to every admin of that \
         repository, and keeps delivering until it is removed there.",
        events.join(", ")
    )
}
