//! Tailscale remote control: multi-session hub + per-session stream/steer.
//!
//! One HTTP server per Grok process. Each session that runs `/remote` gets its
//! own secret URL and QR. The hub root lists all live sessions. Local TUI and
//! remote browsers share dual input per session.

mod qr;
mod server;
mod tailscale;

pub use qr::render_qr_unicode;
pub use server::{
    RemoteCommand, RemoteHubHandle, RemoteHubStart, RemotePrompt, SessionSlot, DEFAULT_PORT,
};
pub use tailscale::{TailscaleInfo, TailscaleStatus, probe as probe_tailscale};

use server::RemoteEvent;
use tailscale::probe;
use tokio::sync::mpsc;

use crate::app::agent::AgentId;
use crate::scrollback::block::RenderBlock;

/// Per-session remote bookkeeping (token, URL, stream progress).
#[derive(Debug, Clone)]
pub struct SessionRemoteMeta {
    pub token: String,
    pub url: String,
    pub connection_card: String,
    pub agent_id: AgentId,
    pub last_transcript_len: usize,
    pub last_tool_fingerprint: String,
    pub suppress_next_user_publish: bool,
    /// Last permission fingerprint we published (avoid spam).
    pub last_permission_fp: String,
}

/// Process-wide remote hub (one port; many sessions).
pub struct RemoteHub {
    pub handle: RemoteHubHandle,
    /// Connection card for the last-registered remote session.
    pub last_card: String,
    pub command_rx: mpsc::UnboundedReceiver<RemoteCommand>,
    /// session_id → meta
    pub sessions: std::collections::HashMap<String, SessionRemoteMeta>,
    /// Session whose panel is open (DocViewer context for disconnect key).
    pub panel_session_id: Option<String>,
}

impl std::fmt::Debug for RemoteHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHub")
            .field("hub_url", &self.handle.hub_url)
            .field("port", &self.handle.port)
            .field("sessions", &self.sessions.len())
            .field("panel_session_id", &self.panel_session_id)
            .finish_non_exhaustive()
    }
}

impl RemoteHub {
    pub fn is_session_remote(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    pub fn meta_for(&self, session_id: &str) -> Option<&SessionRemoteMeta> {
        self.sessions.get(session_id)
    }

    pub fn meta_for_mut(&mut self, session_id: &str) -> Option<&mut SessionRemoteMeta> {
        self.sessions.get_mut(session_id)
    }
}

/// Result of registering a session on an existing or new hub.
pub struct RegisterResult {
    pub token: String,
    pub url: String,
    pub connection_card: String,
    pub message: String,
    /// Only set when a new hub was started.
    pub new_hub: Option<RemoteHub>,
}

/// Build the human-readable connection card (URL, QR, phone instructions).
pub fn format_connection_card(
    session_url: &str,
    hub_url: &str,
    info: &TailscaleInfo,
    port: u16,
    dns_name: Option<&str>,
    label: &str,
) -> String {
    let mut out = String::new();
    out.push_str("Remote control enabled (Tailscale)\n\n");
    out.push_str(&format!("  Session  {label}\n"));
    out.push_str(&format!("  URL      {session_url}\n"));
    out.push_str(&format!("  Hub      {hub_url}  (all remote sessions)\n"));
    out.push_str(&format!("  IP       {}:{port}\n", info.ip));
    if let Some(dns) = dns_name.filter(|s| !s.is_empty()) {
        out.push_str(&format!(
            "  DNS      http://{dns}:{port}/… (use the full URL above)\n"
        ));
    }
    out.push('\n');
    out.push_str("Phone / other device:\n");
    out.push_str("  • Must be logged into the SAME Tailscale account as this machine\n");
    out.push_str("  • Open this session URL in a mobile browser, or scan the QR\n");
    out.push_str("  • Or open the Hub URL to pick among multiple remote sessions\n");
    out.push_str("  • Local TUI and remote browser share dual input for this session\n\n");

    // Keep the main panel compact — full terminal QR opens in a second window.
    out.push_str("QR code:\n");
    out.push_str("  →  Press q  ·  View QR code\n");
    out.push_str("     (opens a scannable QR window · Esc closes it)\n\n");

    out.push_str("TUI: click the \"remote\" status chip for this panel, or:\n");
    out.push_str("  /remote          re-show this card\n");
    out.push_str("  /remote stop     disconnect this session\n");
    out.push_str("\nIn this panel:  q  view QR  ·  d  disconnect  ·  Esc  close\n");
    out.push_str("Hub exits when the last remote session disconnects or Grok quits.\n");
    out
}

/// Content for the dedicated QR viewer modal (unicode QR + URL).
///
/// QR lines must stay intact (no markdown reflow) — the DocViewer special-cases
/// title `"Remote QR"` for plain no-wrap rendering.
pub fn format_qr_viewer_content(session_url: &str, label: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("Session  {label}\n"));
    out.push_str(&format!("URL      {session_url}\n"));
    out.push_str("Scan with phone (same Tailscale account)\n");
    out.push('\n');
    if let Some(qr) = render_qr_unicode(session_url) {
        // Keep QR as consecutive full lines — no blank lines inside the block.
        out.push_str(qr.trim_end());
        out.push('\n');
    } else {
        out.push_str("(QR unavailable — open the session URL in a browser instead)\n");
    }
    out.push('\n');
    out.push_str("Esc  back  ·  d  disconnect remote\n");
    out
}

fn card_from_handle(
    handle: &RemoteHubHandle,
    session_url: &str,
    label: &str,
) -> String {
    let info = TailscaleInfo {
        binary: Default::default(),
        ip: handle.host_ip.clone(),
        dns_name: handle.dns_name.clone(),
        backend_state: None,
    };
    format_connection_card(
        session_url,
        &handle.hub_url,
        &info,
        handle.port,
        handle.dns_name.as_deref(),
        label,
    )
}

/// Register `session_id` on an existing hub handle (cloneable).
pub async fn register_session_on_hub(
    handle: RemoteHubHandle,
    session_id: String,
    label: String,
) -> RegisterResult {
    let (token, url) = handle
        .register_session(session_id.clone(), label.clone())
        .await;
    let card = card_from_handle(&handle, &url, &label);
    RegisterResult {
        token,
        url,
        connection_card: card.clone(),
        message: card,
        new_hub: None,
    }
}

/// Start a new hub and register the first session.
pub async fn start_hub_and_register(
    session_id: String,
    label: String,
    agent_id: AgentId,
) -> Result<RegisterResult, String> {
    match probe() {
        TailscaleStatus::NotInstalled { install_hint } => Err(install_hint),
        TailscaleStatus::NotRunning { hint, .. } => Err(hint),
        TailscaleStatus::Ready(info) => {
            match server::RemoteHubStart::start(info.ip.clone(), info.dns_name.clone(), DEFAULT_PORT)
                .await
            {
                Ok(started) => {
                    let handle = started.handle;
                    let (token, url) = handle
                        .register_session(session_id.clone(), label.clone())
                        .await;
                    let card = format_connection_card(
                        &url,
                        &handle.hub_url,
                        &info,
                        handle.port,
                        handle.dns_name.as_deref(),
                        &label,
                    );
                    let mut sessions = std::collections::HashMap::new();
                    sessions.insert(
                        session_id,
                        SessionRemoteMeta {
                            token: token.clone(),
                            url: url.clone(),
                            connection_card: card.clone(),
                            agent_id,
                            last_transcript_len: 0,
                            last_tool_fingerprint: String::new(),
                            suppress_next_user_publish: false,
                            last_permission_fp: String::new(),
                        },
                    );
                    let hub = RemoteHub {
                        handle,
                        last_card: card.clone(),
                        command_rx: started.command_rx,
                        sessions,
                        panel_session_id: None,
                    };
                    Ok(RegisterResult {
                        token,
                        url,
                        connection_card: card.clone(),
                        message: card,
                        new_hub: Some(hub),
                    })
                }
                Err(e) => Err(format!(
                    "Could not start remote control hub: {e}\n\n\
                     Check that port {DEFAULT_PORT} is free, or that Tailscale is connected."
                )),
            }
        }
    }
}

/// Disconnect one session. Returns true if hub has no sessions left.
pub async fn stop_remote_session(handle: &RemoteHubHandle, session_id: &str) -> bool {
    handle.unregister_session(session_id).await;
    handle.session_count().await == 0
}

/// Build remote events from scrollback for history hydrate / tools.
pub fn events_from_scrollback<'a>(
    blocks: impl Iterator<Item = &'a RenderBlock>,
    start_seq: u64,
) -> Vec<RemoteEvent> {
    let mut out = Vec::new();
    let mut seq = start_seq;
    let mut assistant_buf = String::new();

    let flush_assistant = |buf: &mut String, seq: &mut u64, out: &mut Vec<RemoteEvent>| {
        if buf.is_empty() {
            return;
        }
        *seq += 1;
        out.push(RemoteEvent {
            kind: "assistant".into(),
            text: std::mem::take(buf),
            origin: "local".into(),
            seq: *seq,
            payload: None,
        });
    };

    for block in blocks {
        match block {
            RenderBlock::UserPrompt(u) => {
                flush_assistant(&mut assistant_buf, &mut seq, &mut out);
                seq += 1;
                out.push(RemoteEvent {
                    kind: "user".into(),
                    text: u.copy_text(),
                    origin: "local".into(),
                    seq,
                    payload: None,
                });
            }
            RenderBlock::AgentMessage(a) => {
                assistant_buf.push_str(&a.copy_text(true));
            }
            RenderBlock::ToolCall(tc) => {
                flush_assistant(&mut assistant_buf, &mut seq, &mut out);
                let summary = tool_summary_line(tc);
                seq += 1;
                out.push(RemoteEvent {
                    kind: "tool".into(),
                    text: summary,
                    origin: "local".into(),
                    seq,
                    payload: None,
                });
            }
            _ => {}
        }
    }
    flush_assistant(&mut assistant_buf, &mut seq, &mut out);
    out
}

fn tool_summary_line(tc: &crate::scrollback::blocks::tool::ToolCallBlock) -> String {
    use crate::scrollback::blocks::tool::ToolCallBlock;
    match tc {
        ToolCallBlock::Read(r) => format!("read {}", r.path),
        ToolCallBlock::Edit(e) => format!("edit {}", e.path),
        ToolCallBlock::Execute(x) => {
            let cmd = x.command.chars().take(120).collect::<String>();
            format!("shell {cmd}")
        }
        ToolCallBlock::Search(s) => format!("search {}", s.pattern),
        ToolCallBlock::ListDir(l) => format!("list {}", l.path),
        ToolCallBlock::WebSearch(w) => format!("web_search {}", w.query),
        ToolCallBlock::WebFetch(w) => format!("web_fetch {}", w.url),
        ToolCallBlock::IntegrationSearch(s) => format!("search_tool {}", s.query),
        ToolCallBlock::UseTool(u) => format!("use_tool {}", u.tool_name),
        ToolCallBlock::MemorySearch(m) => format!("memory_search {}", m.query),
        ToolCallBlock::Skill(o) | ToolCallBlock::Other(o) => {
            format!("tool {} {}", o.name, o.summary)
        }
        ToolCallBlock::Lifecycle(l) => format!("lifecycle {}", l.name),
    }
}

/// Publish a pending permission prompt to remote clients.
pub fn publish_permission(
    slot: &SessionSlot,
    title: &str,
    options: &[(String, String)], // option_id, name
) {
    let payload = serde_json::json!({
        "options": options.iter().map(|(id, name)| {
            serde_json::json!({ "option_id": id, "name": name })
        }).collect::<Vec<_>>(),
    });
    slot.publish_payload("permission", title, "local", Some(payload));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_card_mentions_hub_and_session() {
        let info = TailscaleInfo {
            binary: Default::default(),
            ip: "100.64.0.1".into(),
            dns_name: Some("dev.tailnet.ts.net".into()),
            backend_state: Some("Running".into()),
        };
        let url = "http://100.64.0.1:7788/s/abc123/";
        let hub = "http://100.64.0.1:7788/";
        let card =
            format_connection_card(url, hub, &info, 7788, info.dns_name.as_deref(), "my work");
        assert!(card.contains(url));
        assert!(card.contains(hub));
        assert!(card.contains("Press q"));
        assert!(card.to_lowercase().contains("view qr"));
        assert!(card.to_lowercase().contains("same tailscale account"));
        assert!(card.contains("/remote stop"));
        // Main panel stays compact — no embedded block QR.
        assert!(!card.contains('▀') && !card.contains('█'));

        let qr_view = format_qr_viewer_content(url, "my work");
        assert!(qr_view.contains(url));
        assert!(qr_view.contains('▀') || qr_view.contains('█') || qr_view.contains('▄'));
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::user::UserPromptBlock;
    use crate::scrollback::blocks::agent::AgentMessageBlock;

    #[test]
    fn history_includes_user_and_assistant() {
        let user = RenderBlock::UserPrompt(UserPromptBlock::new("hello world"));
        let agent = RenderBlock::AgentMessage(AgentMessageBlock::new("hi there"));
        let events = events_from_scrollback([&user, &agent].into_iter(), 0);
        assert!(events.iter().any(|e| e.kind == "user" && e.text.contains("hello")));
        assert!(events.iter().any(|e| e.kind == "assistant" && e.text.contains("hi")));
    }
}
