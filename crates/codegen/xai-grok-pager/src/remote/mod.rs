//! Tailscale remote control: expose a live session over the user's tailnet.
//!
//! `/remote` checks Tailscale, starts a small HTTP UI on this machine, and
//! prints a URL + QR code. The phone/browser must be on the **same Tailscale
//! account** (same tailnet). Local TUI and remote browser share one session —
//! both can stream and steer.

mod qr;
mod server;
mod tailscale;

pub use server::{RemoteControlHandle, RemotePrompt, RemoteServerStart, DEFAULT_PORT};
pub use tailscale::{TailscaleInfo, TailscaleStatus, probe as probe_tailscale};

use qr::render_qr_unicode;
use tailscale::probe;

/// Active remote-control session metadata kept on [`AppView`](crate::app::app_view::AppView).
pub struct RemoteControlState {
    pub handle: RemoteControlHandle,
    /// Connection card shown when `/remote` is re-run.
    pub connection_card: String,
    /// Steer prompts from remote browsers (polled by the event loop).
    pub prompt_rx: tokio::sync::mpsc::UnboundedReceiver<RemotePrompt>,
    /// Length of the last assistant text pushed to remote clients.
    pub last_transcript_len: usize,
    /// When true, the next local `SendPrompt` should not re-publish a user
    /// line (the HTTP handler already published the remote-origin message).
    pub suppress_next_user_publish: bool,
}

impl std::fmt::Debug for RemoteControlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteControlState")
            .field("url", &self.handle.url)
            .field("port", &self.handle.port)
            .field("last_transcript_len", &self.last_transcript_len)
            .field("suppress_next_user_publish", &self.suppress_next_user_publish)
            .finish_non_exhaustive()
    }
}

/// Result of enabling remote control for the active session.
pub enum RemoteStartResult {
    /// Server is up; show this message (URL + QR + instructions) in scrollback.
    Started {
        message: String,
        handle: RemoteControlHandle,
        prompt_rx: tokio::sync::mpsc::UnboundedReceiver<RemotePrompt>,
    },
    /// Already running — re-show connection info.
    AlreadyRunning { message: String },
    /// User-facing error (missing Tailscale, etc.).
    Failed { message: String },
}

/// Build the human-readable connection card (URL, QR, phone instructions).
pub fn format_connection_card(
    url: &str,
    info: &TailscaleInfo,
    port: u16,
    dns_name: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("Remote control enabled (Tailscale)\n\n");
    out.push_str(&format!("  URL   {url}\n"));
    out.push_str(&format!("  IP    {}:{port}\n", info.ip));
    if let Some(dns) = dns_name.filter(|s| !s.is_empty()) {
        // Full URL uses the secret token path from `url`; DNS line is informational.
        out.push_str(&format!(
            "  DNS   http://{dns}:{port}/… (use the full URL above)\n"
        ));
    }
    out.push('\n');
    out.push_str("Phone / other device:\n");
    out.push_str("  • Must be logged into the SAME Tailscale account as this machine\n");
    out.push_str("  • Open the URL above in a mobile browser (or scan the QR code)\n");
    out.push_str("  • Stream the session and send messages to steer the agent\n");
    out.push_str("  • Local TUI and remote browser share one session (dual input)\n\n");

    if let Some(qr) = render_qr_unicode(url) {
        out.push_str("QR code (scan with phone camera):\n\n");
        out.push_str(&qr);
        out.push('\n');
    } else {
        out.push_str("(QR unavailable for this URL — copy the URL instead)\n");
    }

    out.push_str("\nRun /remote again to show this card. Remote stops when Grok exits.\n");
    out
}

/// Probe Tailscale and start the remote server (or re-show if already running).
pub async fn start_remote(
    existing: Option<&RemoteControlState>,
    session_label: String,
) -> RemoteStartResult {
    if let Some(state) = existing {
        return RemoteStartResult::AlreadyRunning {
            message: state.connection_card.clone(),
        };
    }

    match probe() {
        TailscaleStatus::NotInstalled { install_hint } => RemoteStartResult::Failed {
            message: install_hint,
        },
        TailscaleStatus::NotRunning { hint, .. } => RemoteStartResult::Failed { message: hint },
        TailscaleStatus::Ready(info) => {
            match RemoteServerStart::start(
                info.ip.clone(),
                info.dns_name.clone(),
                DEFAULT_PORT,
                session_label,
            )
            .await
            {
                Ok(started) => {
                    let message = format_connection_card(
                        &started.handle.url,
                        &info,
                        started.handle.port,
                        started.handle.dns_name.as_deref(),
                    );
                    RemoteStartResult::Started {
                        message,
                        handle: started.handle,
                        prompt_rx: started.prompt_rx,
                    }
                }
                Err(e) => RemoteStartResult::Failed {
                    message: format!(
                        "Could not start remote control server: {e}\n\n\
                         Check that port {DEFAULT_PORT} is free, or that Tailscale is connected."
                    ),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_card_mentions_same_account_and_url() {
        let info = TailscaleInfo {
            binary: Default::default(),
            ip: "100.64.0.1".into(),
            dns_name: Some("dev.tailnet.ts.net".into()),
            backend_state: Some("Running".into()),
        };
        let url = "http://100.64.0.1:7788/s/abc123/";
        let card = format_connection_card(url, &info, 7788, info.dns_name.as_deref());
        assert!(card.contains(url));
        assert!(card.to_lowercase().contains("same tailscale account"));
        assert!(card.contains("100.64.0.1"));
    }
}
