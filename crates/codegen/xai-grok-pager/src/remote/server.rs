//! Multi-session Tailscale remote-control HTTP hub + mobile SPA console.

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

pub const DEFAULT_PORT: u16 = 7788;
const TRANSCRIPT_CAP: usize = 800;

const SPA_HTML: &str = include_str!("spa.html");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoteEvent {
    pub kind: String,
    pub text: String,
    pub origin: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// Commands from the mobile/web UI into the Grok process.
#[derive(Debug, Clone)]
pub enum RemoteCommand {
    Message {
        session_id: String,
        text: String,
    },
    Disconnect {
        session_id: String,
    },
    Permission {
        session_id: String,
        option_id: String,
    },
    /// Ask the pager to push a full history snapshot for this session.
    RefreshHistory {
        session_id: String,
    },
}

/// Back-compat alias used by older call sites.
pub type RemotePrompt = RemoteCommand;

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
        self.publish_payload(kind, text, origin, None)
    }

    pub fn publish_payload(
        &self,
        kind: &str,
        text: &str,
        origin: &str,
        payload: Option<serde_json::Value>,
    ) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let ev = RemoteEvent {
            kind: kind.to_string(),
            text: text.to_string(),
            origin: origin.to_string(),
            seq,
            payload,
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

    /// Replace the in-memory transcript (used when pushing scrollback history).
    pub fn replace_transcript(&self, events: Vec<RemoteEvent>) {
        if let Ok(mut guard) = self.transcript.try_write() {
            *guard = events;
            if guard.len() > TRANSCRIPT_CAP {
                let drain = guard.len() - TRANSCRIPT_CAP;
                guard.drain(0..drain);
            }
            if let Some(last) = guard.last() {
                self.seq.store(last.seq, Ordering::Relaxed);
            }
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn history_snapshot(&self) -> Vec<RemoteEvent> {
        self.transcript
            .try_read()
            .map(|g| g.clone())
            .unwrap_or_default()
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

pub struct HubState {
    pub host_ip: String,
    pub port: u16,
    pub sessions: RwLock<HashMap<String, Arc<SessionSlot>>>,
    command_tx: mpsc::UnboundedSender<RemoteCommand>,
    #[allow(dead_code)]
    cancel: CancellationToken,
}

#[derive(Clone)]
pub struct RemoteHubHandle {
    pub host_ip: String,
    pub dns_name: Option<String>,
    pub port: u16,
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

    pub async fn register_session(&self, session_id: String, label: String) -> (String, String) {
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
        let (events, _) = broadcast::channel(512);
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
            "Remote control connected. Use the menu to switch sessions.",
            "system",
        );
        self.state
            .sessions
            .write()
            .await
            .insert(token_short.clone(), slot);
        (token_short.clone(), self.session_url(&token_short))
    }

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
}

pub struct RemoteHubStart {
    pub handle: RemoteHubHandle,
    pub command_rx: mpsc::UnboundedReceiver<RemoteCommand>,
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
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let state = Arc::new(HubState {
            host_ip: host_ip.clone(),
            port,
            sessions: RwLock::new(HashMap::new()),
            command_tx,
            cancel: cancel.clone(),
        });

        let app = Router::new()
            .route("/", get(spa))
            .route("/s/{token}/", get(spa))
            .route("/s/{token}", get(spa))
            .route("/manifest.webmanifest", get(manifest))
            .route("/sw.js", get(service_worker))
            .route("/api/sessions", get(list_sessions))
            .route("/s/{token}/api/events", get(sse_events))
            .route("/s/{token}/api/transcript", get(get_transcript))
            .route("/s/{token}/api/history", get(get_transcript))
            .route("/s/{token}/api/history/refresh", post(refresh_history))
            .route("/s/{token}/api/message", post(post_message))
            .route("/s/{token}/api/permission", post(post_permission))
            .route("/s/{token}/api/disconnect", post(post_disconnect))
            .route("/s/{token}/api/status", get(get_status))
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
            command_rx,
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

async fn spa() -> Response {
    Html(SPA_HTML).into_response()
}

async fn manifest() -> Response {
    let body = r##"{
  "name": "Grok Remote",
  "short_name": "Grok",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#0a0c0f",
  "theme_color": "#0b0d10",
  "description": "Steer Grok Build sessions over Tailscale"
}"##;
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        body,
    )
        .into_response()
}

async fn service_worker() -> Response {
    let body = r#"
self.addEventListener('install', (e) => { self.skipWaiting(); });
self.addEventListener('activate', (e) => { e.waitUntil(clients.claim()); });
self.addEventListener('fetch', (e) => {
  // Network-first; offline fallback not required for tailnet control plane.
});
"#;
    ([(header::CONTENT_TYPE, "application/javascript")], body).into_response()
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

async fn refresh_history(
    State(state): State<Arc<HubState>>,
    Path(token): Path<String>,
) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let _ = state.command_tx.send(RemoteCommand::RefreshHistory {
        session_id: slot.session_id.clone(),
    });
    Json(serde_json::json!({ "ok": true })).into_response()
}

async fn sse_events(State(state): State<Arc<HubState>>, Path(token): Path<String>) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut rx = slot.events.subscribe();
    // Live-only stream: history is loaded via /api/history to avoid dupes.
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Ok(data) = serde_json::to_string(&ev) {
                        yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
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
        .command_tx
        .send(RemoteCommand::Message {
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

#[derive(Debug, Deserialize)]
struct PermissionBody {
    option_id: String,
}

async fn post_permission(
    State(state): State<Arc<HubState>>,
    Path(token): Path<String>,
    Json(body): Json<PermissionBody>,
) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let option_id = body.option_id.trim().to_string();
    if option_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "option_id required").into_response();
    }
    slot.publish(
        "system",
        &format!("Permission response sent: {option_id}"),
        "remote",
    );
    if state
        .command_tx
        .send(RemoteCommand::Permission {
            session_id: slot.session_id.clone(),
            option_id,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

async fn post_disconnect(
    State(state): State<Arc<HubState>>,
    Path(token): Path<String>,
) -> Response {
    let Some(slot) = resolve_slot(&state, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let session_id = slot.session_id.clone();
    if state
        .command_tx
        .send(RemoteCommand::Disconnect { session_id })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}
