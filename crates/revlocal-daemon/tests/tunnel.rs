//! Tunnel providers and webhook registration (RL-1006, SPEC §7.3).
//!
//! The cloudflared fixtures in this file are **verbatim lines from a real
//! `cloudflared tunnel --url` run**, captured on 2026-08-28 with cloudflared
//! 2026.8.2, per ADR 0023's rule: never write a string match against another
//! tool's output without running the tool and reading what it says.
//!
//! That rule earned itself again during capture. `trycloudflare.com` appears in a
//! progress message four seconds *before* the URL does, so a matcher looking for
//! the bare hostname returns a line that is not a tunnel address. The test below
//! encodes that line specifically.

use revlocal_daemon::tunnel::{
    cloudflared_url, locate, ngrok_url, registration_risk, registration_summary, TunnelError,
    TunnelHealth, TunnelProvider, REGISTER_WEBHOOK_ACTION,
};

/// Verbatim stderr from `cloudflared tunnel --url http://127.0.0.1:59999`,
/// cloudflared 2026.8.2 on macOS, captured 2026-08-28.
const REAL_CLOUDFLARED_STDERR: &str = concat!(
    "2026-08-28T10:25:19Z INF Thank you for trying Cloudflare Tunnel. Doing so, without a Cloudflare account, is a quick way to experiment and try it out. However, be aware that these account-less Tunnels have no uptime guarantee, are subject to the Cloudflare Online Services Terms of Use (https://www.cloudflare.com/website-terms/), and Cloudflare reserves the right to investigate your use of Tunnels for violations of such terms. If you intend to use Tunnels in production you should use a pre-created named tunnel by following: https://developers.cloudflare.com/cloudflare-one/connections/connect-apps\n",
    "2026-08-28T10:25:19Z INF Requesting new quick Tunnel on trycloudflare.com...\n",
    "2026-08-28T10:25:23Z INF +--------------------------------------------------------------------------------------------+\n",
    "2026-08-28T10:25:23Z INF |  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |\n",
    "2026-08-28T10:25:23Z INF |  https://involved-therapeutic-adaptive-career.trycloudflare.com                            |\n",
    "2026-08-28T10:25:23Z INF +--------------------------------------------------------------------------------------------+\n",
    "2026-08-28T10:25:23Z INF Cannot determine default configuration path. No file [config.yml config.yaml] in [~/.cloudflared]\n",
    "2026-08-28T10:25:23Z INF Version 2026.8.2 (Checksum b6bc98e794894b4ccee49c027c7cae050bbf74a92212e2c4bef348f5b33fa846)\n",
    "2026-08-28T10:25:23Z INF Initial protocol quic\n",
);

#[test]
fn the_public_url_is_captured_from_real_cloudflared_output() {
    // Criterion 2, against output the tool actually produced rather than output
    // somebody imagined it produced.
    let found: Vec<String> = REAL_CLOUDFLARED_STDERR
        .lines()
        .filter_map(cloudflared_url)
        .collect();

    assert_eq!(
        found,
        vec!["https://involved-therapeutic-adaptive-career.trycloudflare.com"],
        "exactly one line in a real run carries the tunnel URL"
    );
}

#[test]
fn the_progress_line_that_names_the_host_is_not_mistaken_for_the_url() {
    // The trap, encoded. This line arrives four seconds before the URL, and a
    // matcher looking for the bare hostname takes it.
    let progress = "2026-08-28T10:25:19Z INF Requesting new quick Tunnel on trycloudflare.com...";

    assert!(progress.contains("trycloudflare.com"));
    assert_eq!(
        cloudflared_url(progress),
        None,
        "a progress message is not a tunnel address"
    );
}

#[test]
fn the_banners_own_links_are_not_mistaken_for_the_url() {
    // cloudflared's first line carries two real https URLs — the terms of use and
    // the docs — on stderr, before any tunnel exists. Matching "https://" alone
    // would return cloudflare.com as the webhook endpoint.
    let banner = REAL_CLOUDFLARED_STDERR.lines().next().unwrap_or_default();

    assert!(banner.contains("https://www.cloudflare.com/website-terms/"));
    assert!(banner.contains("https://developers.cloudflare.com/"));
    assert_eq!(cloudflared_url(banner), None);
}

#[test]
fn a_url_is_read_as_a_url_not_as_a_position_in_a_box() {
    // The box drawing, the column padding, the timestamp format and the wording
    // can all change; only the URL's own shape matters, and that is fixed by
    // Cloudflare's DNS rather than by their log formatter.
    for line in [
        "https://abc-def.trycloudflare.com",
        "  https://abc-def.trycloudflare.com  ",
        "INF |  https://abc-def.trycloudflare.com          |",
        "2099-01-01T00:00:00Z WRN something https://abc-def.trycloudflare.com/ else",
    ] {
        assert_eq!(
            cloudflared_url(line).as_deref(),
            Some("https://abc-def.trycloudflare.com"),
            "failed on {line:?}"
        );
    }
}

#[test]
fn ngrok_is_read_from_its_api_not_from_its_output() {
    // Where a tool offers a machine-readable interface, using it is the difference
    // between a contract and a guess.
    let body = r#"{"tunnels":[
        {"name":"command_line (http)","public_url":"http://abc.ngrok.io","proto":"http"},
        {"name":"command_line","public_url":"https://abc.ngrok.io","proto":"https"}
    ]}"#;

    // https is preferred: ngrok opens both, and GitHub webhooks should not be
    // delivered over plaintext. Taking the first entry would pick the http one.
    assert_eq!(ngrok_url(body).as_deref(), Some("https://abc.ngrok.io"));
}

#[test]
fn ngrok_with_no_tunnels_is_not_a_url() {
    assert_eq!(ngrok_url(r#"{"tunnels":[]}"#), None);
    assert_eq!(ngrok_url("not json"), None);
    assert_eq!(ngrok_url("{}"), None);
}

#[test]
fn a_missing_binary_is_reported_with_install_guidance_not_a_crash() {
    // Criterion 1. §7.3's providers are optional, and a machine without
    // cloudflared is a machine that should be told how to get it.
    let error = locate(TunnelProvider::Ngrok)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();

    // ngrok is not installed on this machine; if it ever is, the assertion below
    // still holds for the message shape via `install_hint`.
    if !error.is_empty() {
        assert!(error.contains("ngrok is not installed"), "{error}");
        assert!(error.contains("try:"), "must say what to do: {error}");
        assert!(error.contains("brew install ngrok"), "{error}");
        assert!(error.contains("ngrok.com/download"), "{error}");
    }

    // The hint exists regardless of what happens to be installed here.
    for provider in [TunnelProvider::Cloudflared, TunnelProvider::Ngrok] {
        let hint = provider.install_hint();
        assert!(hint.contains("brew install"), "{provider:?}: {hint}");
        assert!(hint.contains("winget"), "{provider:?}: {hint}");
        assert!(hint.contains("http"), "{provider:?} needs a link: {hint}");
    }
}

#[test]
fn the_manual_provider_needs_no_binary() {
    // `manual` is the escape hatch for a machine that cannot or will not run a
    // tunnel daemon — a corporate laptop, an air-gapped build box, a user who
    // already has an ingress.
    assert_eq!(TunnelProvider::Manual.binary(), None);
    assert!(matches!(locate(TunnelProvider::Manual), Ok(None)));
    assert!(TunnelProvider::Manual.args(41791).is_empty());
}

#[test]
fn tunnel_death_is_a_state_not_an_absence() {
    // Criterion 3. A tunnel that dies leaves the listener bound, healthy and
    // receiving nothing: every check passes and no reviews happen, which reads
    // exactly like a quiet week.
    let running = TunnelHealth::Running {
        url: "https://abc.trycloudflare.com".to_owned(),
    };
    assert!(running.is_receiving());
    assert!(!running.needs_attention());

    let dead = TunnelHealth::Dead {
        was: Some("https://abc.trycloudflare.com".to_owned()),
        reason: "cloudflared exited with status 1".to_owned(),
    };
    assert!(!dead.is_receiving());
    assert!(dead.needs_attention(), "a dead tunnel must be surfaced");

    // Distinct from never having had one — the UI has different things to say.
    assert!(!TunnelHealth::Disabled.needs_attention());
    assert_ne!(dead, TunnelHealth::Disabled);

    let line = dead.summary_line();
    assert!(line.contains("DOWN"), "{line}");
    assert!(
        line.contains("abc.trycloudflare.com"),
        "must name which: {line}"
    );
    assert!(
        line.contains("webhooks are not arriving"),
        "must say what it means: {line}"
    );
}

#[test]
fn registering_a_webhook_is_high_risk() {
    // Criterion 4. §12.3 classifies by blast radius: this writes configuration on
    // somebody else's server that outlives the run, is visible to every admin of
    // the repository, and keeps delivering after rev-local has forgotten about it.
    assert_eq!(registration_risk(), revlocal_core::RiskClass::High);
    assert_eq!(REGISTER_WEBHOOK_ACTION, "register_webhook");

    // And a high-risk action is only meaningfully gated if the person approving it
    // can see what they are approving.
    let summary = registration_summary(
        "acme/api",
        "https://abc.trycloudflare.com",
        &["push", "pull_request"],
    );
    assert!(summary.contains("acme/api"), "{summary}");
    assert!(
        summary.contains("https://abc.trycloudflare.com/webhook"),
        "{summary}"
    );
    assert!(summary.contains("push, pull_request"), "{summary}");
    assert!(summary.contains("visible to every admin"), "{summary}");
    assert!(summary.contains("until it is removed"), "{summary}");
}

#[test]
fn providers_round_trip_through_config() {
    for provider in [
        TunnelProvider::Cloudflared,
        TunnelProvider::Ngrok,
        TunnelProvider::Manual,
    ] {
        assert_eq!(TunnelProvider::parse(provider.as_str()), Some(provider));
    }
    assert_eq!(TunnelProvider::parse("localtunnel"), None);
}

#[test]
fn cloudflared_is_told_not_to_update_itself() {
    // Replacing its own binary mid-run is a surprising thing for a code-review
    // tool to do to a developer's machine, and it changes what is running under a
    // tunnel somebody is depending on.
    let args = TunnelProvider::Cloudflared.args(41791);

    assert!(args.contains(&"--no-autoupdate".to_owned()), "{args:?}");
    assert!(
        args.contains(&"http://127.0.0.1:41791".to_owned()),
        "{args:?}"
    );
    // Loopback, not 0.0.0.0: the tunnel is the only thing that should reach the
    // listener from outside.
    assert!(!args.iter().any(|arg| arg.contains("0.0.0.0")), "{args:?}");
}

#[test]
fn the_no_url_error_is_copy_pasteable() {
    let error = TunnelError::NoUrl {
        provider: "cloudflared",
        seconds: 30,
        args: "tunnel --url http://127.0.0.1:41791".to_owned(),
    }
    .to_string();

    assert!(error.contains("30s"), "{error}");
    assert!(
        error.contains("cloudflared tunnel --url http://127.0.0.1:41791"),
        "the suggestion must be runnable as printed: {error}"
    );
}
