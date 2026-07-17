//! Multi-session Tailscale remote-control HTTP hub.
//!
//! One process binds one port. Each Grok session that runs `/remote` gets its
//! own secret token path (`/s/<token>/`) and QR code. The hub root `/` lists
//! all live remote sessions.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, extract::Request, middleware::Next};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// Default bind port; falls back to ephemeral if busy.
pub const DEFAULT_PORT: u16 = 7788;

/// Max retained transcript lines per session for late-joining browsers.
const TRANSCRIPT_CAP: usize = 500;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoteEvent {
    pub kind: String,
    pub text: String,
    pub origin: String,
    pub seq: u64,
}

/// Steer prompt from a remote browser, tagged with the Grok session it targets.
#[derive(Debug, Clone)]
pub struct RemotePrompt {
    pub session_id: String,
    pub text: String,
}

/// Per-session remote channel (one token URL / QR per Grok session).
pub struct SessionSlot {
    pub token: String,
    pub session_id: String,
    pub label: String,
    events: broadcast::Sender<RemoteEvent>,
    transcript: RwLock<Vec<RemoteEvent>>,
    seq: AtomicU64,
}

impl SessionSlot {
    pub fn publish(&self, kind: &str, text: &str, origin: &str) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let ev = RemoteEvent {
            kind: kind.to_string(),
            text: text.to_string(),
            origin: origin.to_string(),
            seq,
        };
        if let Ok(mut guard) = self.transcript.try_write() {
            guard.push(ev.clone());
            if guard.len() > TRANSCRIPT_CAP {
                let drain = guard.len() - TRANSCRIPT_CAP;
                guard.drain(0..drain);
            }
        }
        let _ = self.events.send(ev);
        seq
    }

    fn authorized(token: &str, expected: &str) -> bool {
        token.len() == expected.len()
            && token
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

/// Shared hub state for the HTTP server.
pub struct HubState {
    pub host_ip: String,
    pub port: u16,
    /// token → session slot
    pub sessions: RwLock<HashMap<String, Arc<SessionSlot>>>,
    prompt_tx: mpsc::UnboundedSender<RemotePrompt>,
    #[allow(dead_code)] // held so drop order is explicit with handle cancel
    cancel: CancellationToken,
}

/// Cloneable handle for the process-wide remote hub.
#[derive(Clone)]
pub struct RemoteHubHandle {
    pub host_ip: String,
    pub dns_name: Option<String>,
    pub port: u16,
    /// Hub root URL (session list).
    pub hub_url: String,
    pub state: Arc<HubState>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for RemoteHubHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHubHandle")
            .field("hub_url", &self.hub_url)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl RemoteHubHandle {
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn session_url(&self, token: &str) -> String {
        format!("http://{}:{}/s/{token}/", self.host_ip, self.port)
    }

    /// Register a Grok session and return its token + public URL.
    pub async fn register_session(&self, session_id: String, label: String) -> (String, String) {
        // Reuse existing token if this session is already remote-enabled.
        {
            let map = self.state.sessions.read().await;
            for (tok, slot) in map.iter() {
                if slot.session_id == session_id {
                    return (tok.clone(), self.session_url(tok));
                }
            }
        }

        let token = uuid::Uuid::new_v4().to_string().replace('-', "");
        let token_short = token[..12.min(token.len())].to_string();
        let (events, _) = broadcast::channel(256);
        let slot = Arc::new(SessionSlot {
            token: token_short.clone(),
            session_id: session_id.clone(),
            label: label.clone(),
            events,
            transcript: RwLock::new(Vec::new()),
            seq: AtomicU64::new(0),
        });
        slot.publish(
            "system",
            "Remote control connected. Type a message below to steer this Grok session.",
            "system",
        );
        self.state
            .sessions
            .write()
            .await
            .insert(token_short.clone(), slot);
        let url = self.session_url(&token_short);
        (token_short, url)
    }

    /// Unregister a session by Grok session id. Returns true if removed.
    pub async fn unregister_session(&self, session_id: &str) -> bool {
        let mut map = self.state.sessions.write().await;
        let key = map
            .iter()
            .find(|(_, s)| s.session_id == session_id)
            .map(|(k, _)| k.clone());
        if let Some(k) = key {
            map.remove(&k);
            true
        } else {
            false
        }
    }

    pub async fn session_count(&self) -> usize {
        self.state.sessions.read().await.len()
    }

    pub async fn get_by_session_id(&self, session_id: &str) -> Option<Arc<SessionSlot>> {
        let map = self.state.sessions.read().await;
        map.values()
            .find(|s| s.session_id == session_id)
            .cloned()
    }

    pub async fn get_by_token(&self, token: &str) -> Option<Arc<SessionSlot>> {
        self.state.sessions.read().await.get(token).cloned()
    }
}

/// Result of starting the hub.
pub struct RemoteHubStart {
    pub handle: RemoteHubHandle,
    pub prompt_rx: mpsc::UnboundedReceiver<RemotePrompt>,
}

impl RemoteHubStart {
    pub async fn start(
        host_ip: String,
        dns_name: Option<String>,
        preferred_port: u16,
    ) -> anyhow::Result<Self> {
        let listener = bind_preferred(preferred_port).await?;
        let bind_addr = listener.local_addr()?;
        let port = bind_addr.port();
        let hub_url = format!("http://{host_ip}:{port}/");
        let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let state = Arc::new(HubState {
            host_ip: host_ip.clone(),
            port,
            sessions: RwLock::new(HashMap::new()),
            prompt_tx,
            cancel: cancel.clone(),
        });

        let app = Router::new()
            .route("/", get(hub_dashboard))
            .route("/s/{token}/", get(ui_page))
            .route("/s/{token}", get(ui_page))
            .route("/s/{token}/qr", get(qr_page))
            .route("/s/{token}/qr/", get(qr_page))
            .route("/s/{token}/api/events", get(sse_events))
            .route("/s/{token}/api/transcript", get(get_transcript))
            .route("/s/{token}/api/message", post(post_message))
            .route("/s/{token}/api/status", get(get_status))
            .route("/api/sessions", get(list_sessions))
            .layer(axum::middleware::from_fn(security_headers))
            .with_state(state.clone());

        let cancel_server = cancel.clone();
        tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            );
            tokio::select! {
                _ = cancel_server.cancelled() => {}
                res = serve => {
                    if let Err(e) = res {
                        tracing::warn!("remote control hub exited: {e}");
                    }
                }
            }
        });

        Ok(Self {
            handle: RemoteHubHandle {
                host_ip,
                dns_name,
                port,
                hub_url,
                state,
                cancel,
            },
            prompt_rx,
        })
    }
}

async fn bind_preferred(preferred: u16) -> anyhow::Result<tokio::net::TcpListener> {
    let addr = SocketAddr::from(([0, 0, 0, 0], preferred));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => Ok(l),
        Err(_) if preferred != 0 => {
            let fallback = SocketAddr::from(([0, 0, 0, 0], 0));
            Ok(tokio::net::TcpListener::bind(fallback).await?)
        }
        Err(e) => Err(e.into()),
    }
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow"),
    );
    res
}

async fn hub_dashboard(State(state): State<Arc<HubState>>) -> Response {
    let sessions: Vec<_> = {
        let map = state.sessions.read().await;
        map.values()
            .map(|s| {
                (
                    s.token.clone(),
                    s.label.clone(),
                    s.session_id.clone(),
                )
            })
            .collect()
    };
    Html(hub_html(&state.host_ip, state.port, &sessions)).into_response()
}

async fn list_sessions(State(state): State<Arc<HubState>>) -> Response {
    let map = state.sessions.read().await;
    let list: Vec<_> = map
        .values()
        .map(|s| {
            serde_json::json!({
                "token": s.token,
                "label": s.label,
                "session_id": s.session_id,
                "url": format!("/s/{}/", s.token),
            })
        })
        .collect();
    Json(list).into_response()
}

async fn resolve_slot(state: &HubState, token: &str) -> Option<Arc<SessionSlot>> {
    state.sessions.read().await.get(token).cloned()
}

async fn ui_page(State(state): State<Arc<HubState>>, Path(token): Path<String>) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return (StatusCode::NOT_FOUND, "Unknown or disconnected session").into_response();
    };
    if !SessionSlot::authorized(&token, &slot.token) {
        return (StatusCode::NOT_FOUND, "Unknown session").into_response();
    }
    Html(ui_html(&slot.label, &state.host_ip)).into_response()
}

/// Full-page QR for scanning — linked from the TUI modal (keeps the modal compact).
async fn qr_page(State(state): State<Arc<HubState>>, Path(token): Path<String>) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return (StatusCode::NOT_FOUND, "Unknown or disconnected session").into_response();
    };
    let session_url = format!("http://{}:{}/s/{}/", state.host_ip, state.port, slot.token);
    let Some(svg) = crate::remote::qr::render_qr_svg(&session_url) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not render QR code",
        )
            .into_response();
    };
    Html(qr_html(&slot.label, &session_url, &svg)).into_response()
}

fn qr_html(label: &str, session_url: &str, svg: &str) -> String {
    let title = html_escape(label);
    let url = html_escape(session_url);
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover"/>
<meta name="theme-color" content="#0b0d10"/>
<title>QR · {title}</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100dvh; display: flex; flex-direction: column;
    align-items: center; justify-content: center; gap: 16px;
    background: #0b0d10; color: #eef2f7;
    font: 16px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
    padding: 24px max(16px, env(safe-area-inset-right)) max(24px, env(safe-area-inset-bottom)) max(16px, env(safe-area-inset-left));
    text-align: center;
  }}
  h1 {{ margin: 0; font-size: 18px; font-weight: 600; }}
  .sub {{ color: #8b95a8; font-size: 13px; max-width: 360px; word-break: break-all; }}
  .card {{
    background: #fff; border-radius: 20px; padding: 20px;
    box-shadow: 0 12px 40px rgba(0,0,0,0.45);
    line-height: 0;
  }}
  .card svg {{ width: min(72vw, 280px); height: auto; display: block; }}
  a.open {{
    display: inline-block; margin-top: 4px; padding: 12px 20px; border-radius: 12px;
    background: #5eead4; color: #042f2e; font-weight: 700; text-decoration: none;
  }}
  a.back {{ color: #5eead4; font-size: 13px; }}
</style>
</head>
<body>
  <h1>Scan to open remote session</h1>
  <p class="sub">{title}</p>
  <div class="card">{svg}</div>
  <p class="sub">{url}</p>
  <a class="open" href="{url}">Open session</a>
  <a class="back" href="{url}">← Back to chat</a>
</body>
</html>"##,
        title = title,
        url = url,
        svg = svg,
    )
}

async fn get_status(State(state): State<Arc<HubState>>, Path(token): Path<String>) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(serde_json::json!({
        "ok": true,
        "session": slot.label,
        "session_id": slot.session_id,
        "host": state.host_ip,
        "remote_enabled": true,
    }))
    .into_response()
}

async fn get_transcript(
    State(state): State<Arc<HubState>>,
    Path(token): Path<String>,
) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let lines = slot.transcript.read().await.clone();
    Json(lines).into_response()
}

async fn sse_events(State(state): State<Arc<HubState>>, Path(token): Path<String>) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut rx = slot.events.subscribe();
    let history = slot.transcript.read().await.clone();

    let stream = async_stream::stream! {
        for ev in history {
            if let Ok(data) = serde_json::to_string(&ev) {
                yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
            }
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Ok(data) = serde_json::to_string(&ev) {
                        yield Ok(Event::default().data(data));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[derive(Debug, Deserialize)]
struct MessageBody {
    text: String,
}

async fn post_message(
    State(state): State<Arc<HubState>>,
    Path(token): Path<String>,
    Json(body): Json<MessageBody>,
) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if text.len() > 32_768 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "message too long").into_response();
    }

    slot.publish("user", &text, "remote");
    if state
        .prompt_tx
        .send(RemotePrompt {
            session_id: slot.session_id.clone(),
            text,
        })
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "session is no longer accepting remote input",
        )
            .into_response();
    }

    Json(serde_json::json!({ "ok": true })).into_response()
}

fn hub_html(host_ip: &str, port: u16, sessions: &[(String, String, String)]) -> String {
    let host = html_escape(host_ip);
    let mut list = String::new();
    if sessions.is_empty() {
        list.push_str(
            r#"<p class="empty">No remote sessions yet. In Grok, open a session and run <code>/remote</code>.</p>"#,
        );
    } else {
        list.push_str("<ul class=\"sessions\">");
        for (token, label, _sid) in sessions {
            let l = html_escape(label);
            let t = html_escape(token);
            list.push_str(&format!(
                r#"<li><a href="/s/{t}/"><strong>{l}</strong><span class="tok">/s/{t}/</span></a></li>"#
            ));
        }
        list.push_str("</ul>");
    }
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover"/>
<meta name="theme-color" content="#0b0d10"/>
<title>Grok Remote Hub</title>
<style>
  :root {{ color-scheme: dark; --bg:#0a0c0f; --panel:#12161e; --border:#243041; --text:#eef2f7; --muted:#8b95a8; --accent:#5eead4; }}
  * {{ box-sizing: border-box; }}
  body {{ margin:0; min-height:100dvh; background:var(--bg); color:var(--text);
    font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;
    padding: max(16px, env(safe-area-inset-top)) 16px max(16px, env(safe-area-inset-bottom));
    max-width: 520px; margin-inline: auto; }}
  h1 {{ font-size: 18px; margin: 0 0 4px; }}
  .sub {{ color: var(--muted); font-size: 13px; margin-bottom: 20px; }}
  .sessions {{ list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 10px; }}
  .sessions a {{ display: block; padding: 14px 16px; border-radius: 14px; border: 1px solid var(--border);
    background: var(--panel); color: inherit; text-decoration: none; }}
  .sessions a:active {{ transform: scale(0.99); }}
  .sessions strong {{ display: block; margin-bottom: 4px; }}
  .tok {{ font-size: 12px; color: var(--muted); font-family: ui-monospace, monospace; }}
  .empty {{ color: var(--muted); }}
  code {{ color: var(--accent); }}
</style>
</head>
<body>
  <h1>Grok Remote</h1>
  <p class="sub">{host}:{port} · Tailscale hub · same account required</p>
  {list}
</body>
</html>"##,
        host = host,
        port = port,
        list = list,
    )
}

fn ui_html(session_label: &str, host_ip: &str) -> String {
    let title = html_escape(session_label);
    let host = html_escape(host_ip);
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover"/>
<meta name="apple-mobile-web-app-capable" content="yes"/>
<meta name="theme-color" content="#0b0d10"/>
<title>Grok Remote — {title}</title>
<style>
  :root {{
    color-scheme: dark;
    --bg: #0a0c0f;
    --panel: #12161e;
    --composer: #0f131a;
    --border: #243041;
    --text: #eef2f7;
    --muted: #8b95a8;
    --accent: #5eead4;
    --accent-text: #042f2e;
    --user-bg: #1a2740;
    --user-border: #2d4a73;
    --assistant-bg: #141a24;
    --system-bg: #1a1628;
    --system-text: #c4b5fd;
    --safe-b: env(safe-area-inset-bottom, 0px);
    --safe-t: env(safe-area-inset-top, 0px);
  }}
  * {{ box-sizing: border-box; -webkit-tap-highlight-color: transparent; }}
  html, body {{
    margin: 0; height: 100%; height: 100dvh;
    background: var(--bg); color: var(--text);
    font: 16px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
    overflow: hidden;
  }}
  body {{
    display: flex; flex-direction: column; max-width: 720px; margin: 0 auto;
    padding-top: var(--safe-t);
  }}
  header {{
    flex-shrink: 0; padding: 12px 16px 10px; border-bottom: 1px solid var(--border);
    background: rgba(18, 22, 30, 0.92); backdrop-filter: blur(12px);
  }}
  header .row {{ display: flex; align-items: center; gap: 10px; }}
  header .dot {{ width: 8px; height: 8px; border-radius: 50%; background: var(--muted); flex-shrink: 0; }}
  header .dot.live {{ background: var(--accent); box-shadow: 0 0 0 3px rgba(94, 234, 212, 0.15); }}
  header h1 {{ margin: 0; font-size: 15px; font-weight: 600; flex: 1; min-width: 0;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  header p {{ margin: 4px 0 0 18px; color: var(--muted); font-size: 12px; }}
  header a.hub {{ color: var(--accent); text-decoration: none; font-size: 12px; }}
  #log {{
    flex: 1; overflow-y: auto; -webkit-overflow-scrolling: touch;
    padding: 14px 12px 8px; display: flex; flex-direction: column; gap: 10px;
    overscroll-behavior: contain;
  }}
  .msg {{
    max-width: 92%; padding: 11px 14px; border-radius: 16px;
    border: 1px solid var(--border); background: var(--assistant-bg);
    word-break: break-word; animation: fadeIn 0.15s ease-out;
  }}
  @keyframes fadeIn {{ from {{ opacity: 0; transform: translateY(4px); }} to {{ opacity: 1; transform: none; }} }}
  .msg .meta {{ font-size: 11px; font-weight: 600; color: var(--muted); margin-bottom: 5px; }}
  .msg .body {{ white-space: pre-wrap; font-size: 16px; line-height: 1.5; }}
  .msg.assistant {{ align-self: flex-start; border-bottom-left-radius: 6px; }}
  .msg.assistant .meta {{ color: var(--accent); }}
  .msg.assistant.streaming .meta::after {{
    content: ''; display: inline-block; width: 6px; height: 6px; margin-left: 6px;
    border-radius: 50%; background: var(--accent); vertical-align: middle;
    animation: pulse 1s ease-in-out infinite;
  }}
  @keyframes pulse {{ 0%, 100% {{ opacity: 0.35; }} 50% {{ opacity: 1; }} }}
  .msg.user {{ align-self: flex-end; background: var(--user-bg); border-color: var(--user-border);
    border-bottom-right-radius: 6px; }}
  .msg.user .meta {{ color: #93c5fd; }}
  .msg.system {{ align-self: center; max-width: 100%; background: var(--system-bg);
    border-color: #3b3358; border-radius: 12px; padding: 8px 12px; }}
  .msg.system .meta {{ display: none; }}
  .msg.system .body {{ color: var(--system-text); font-size: 13px; text-align: center; }}
  .composer {{
    flex-shrink: 0; border-top: 1px solid var(--border); background: var(--composer);
    padding: 10px 12px calc(10px + var(--safe-b));
  }}
  #status {{ font-size: 11px; color: var(--muted); margin: 0 2px 8px; min-height: 14px; }}
  #status.ok {{ color: var(--accent); }}
  #status.err {{ color: #f87171; }}
  form {{ display: flex; gap: 10px; align-items: flex-end; }}
  textarea {{
    flex: 1; resize: none; min-height: 48px; max-height: 140px; border-radius: 14px;
    border: 1px solid var(--border); background: var(--bg); color: var(--text);
    padding: 12px 14px; font: inherit; font-size: 16px; line-height: 1.4; outline: none;
  }}
  textarea:focus {{ border-color: #3d5a80; }}
  button {{
    flex-shrink: 0; border: 0; border-radius: 14px; min-width: 72px; min-height: 48px;
    padding: 0 18px; background: var(--accent); color: var(--accent-text);
    font-weight: 700; font-size: 15px; cursor: pointer;
  }}
  button:active {{ transform: scale(0.97); }}
  button:disabled {{ opacity: 0.45; cursor: default; transform: none; }}
</style>
</head>
<body>
  <header>
    <div class="row">
      <span class="dot" id="liveDot"></span>
      <h1>Grok Remote · {title}</h1>
      <a class="hub" href="/">Hub</a>
    </div>
    <p>{host} · Tailscale · same account on this device</p>
  </header>
  <div id="log" aria-live="polite"></div>
  <div class="composer">
    <div id="status">Connecting…</div>
    <form id="f">
      <textarea id="t" rows="1" placeholder="Message Grok…" enterkeyhint="send" autofocus></textarea>
      <button type="submit" id="send">Send</button>
    </form>
  </div>
<script>
const log = document.getElementById('log');
const statusEl = document.getElementById('status');
const liveDot = document.getElementById('liveDot');
const form = document.getElementById('f');
const ta = document.getElementById('t');
const sendBtn = document.getElementById('send');
const base = location.pathname.replace(/\/?$/, '/');
const seen = new Set();
let openAssistant = null;
let streamIdleTimer = null;

function kindOf(ev) {{
  const k = (ev.kind || 'system').toLowerCase();
  if (k === 'assistant' || k === 'assistant_delta') return 'assistant';
  if (k === 'user') return 'user';
  return 'system';
}}
function labelFor(kind, origin) {{
  if (kind === 'assistant') return 'Grok';
  if (kind === 'user') {{
    if (origin === 'remote') return 'You';
    if (origin === 'local') return 'You (desktop)';
    return 'You';
  }}
  return 'System';
}}
function nearBottom() {{
  return log.scrollHeight - log.scrollTop - log.clientHeight < 80;
}}
function scrollIfNeeded(force) {{
  if (force || nearBottom()) log.scrollTop = log.scrollHeight;
}}
function closeAssistantStream() {{
  if (openAssistant) {{ openAssistant.el.classList.remove('streaming'); openAssistant = null; }}
  if (streamIdleTimer) {{ clearTimeout(streamIdleTimer); streamIdleTimer = null; }}
}}
function markStreaming() {{
  if (!openAssistant) return;
  openAssistant.el.classList.add('streaming');
  if (streamIdleTimer) clearTimeout(streamIdleTimer);
  streamIdleTimer = setTimeout(() => {{
    if (openAssistant) openAssistant.el.classList.remove('streaming');
  }}, 900);
}}
function addMsg(ev) {{
  if (seen.has(ev.seq)) return;
  seen.add(ev.seq);
  const kind = kindOf(ev);
  const text = ev.text || '';
  const stick = nearBottom();
  if (kind === 'assistant' && openAssistant) {{
    openAssistant.body.textContent += text;
    markStreaming();
    scrollIfNeeded(stick);
    return;
  }}
  if (kind !== 'assistant') closeAssistantStream();
  const div = document.createElement('div');
  div.className = 'msg ' + kind;
  const meta = document.createElement('div');
  meta.className = 'meta';
  meta.textContent = labelFor(kind, ev.origin);
  const body = document.createElement('div');
  body.className = 'body';
  body.textContent = text;
  div.appendChild(meta);
  div.appendChild(body);
  log.appendChild(div);
  if (kind === 'assistant') {{ openAssistant = {{ el: div, body }}; markStreaming(); }}
  scrollIfNeeded(stick || true);
}}
function setStatus(text, cls) {{
  statusEl.textContent = text;
  statusEl.className = cls || '';
  liveDot.classList.toggle('live', cls === 'ok');
}}
function autoGrow() {{
  ta.style.height = 'auto';
  ta.style.height = Math.min(ta.scrollHeight, 140) + 'px';
}}
const es = new EventSource(base + 'api/events');
es.onopen = () => setStatus('Live · desktop + this phone', 'ok');
es.onerror = () => setStatus('Reconnecting…', 'err');
es.onmessage = (e) => {{ try {{ addMsg(JSON.parse(e.data)); }} catch (_) {{}} }};
form.addEventListener('submit', async (e) => {{
  e.preventDefault();
  const text = ta.value.trim();
  if (!text) return;
  sendBtn.disabled = true;
  closeAssistantStream();
  try {{
    const res = await fetch(base + 'api/message', {{
      method: 'POST',
      headers: {{ 'content-type': 'application/json' }},
      body: JSON.stringify({{ text }}),
    }});
    if (!res.ok) throw new Error(await res.text());
    ta.value = '';
    autoGrow();
  }} catch (err) {{
    setStatus('Send failed: ' + err, 'err');
  }} finally {{
    sendBtn.disabled = false;
    ta.focus();
  }}
}});
ta.addEventListener('input', autoGrow);
ta.addEventListener('keydown', (e) => {{
  if (e.key === 'Enter' && !e.shiftKey) {{ e.preventDefault(); form.requestSubmit(); }}
}});
</script>
</body>
</html>"##
    )
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            c => c.to_string(),
        })
        .collect()
}
