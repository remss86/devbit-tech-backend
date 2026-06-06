use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::Pool;
use sqlx::Postgres;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ── WebSocket shared state ──────────────────────────────────────────────────

/// Per-connection sender handle.
type Tx = mpsc::UnboundedSender<Message>;

#[derive(Clone)]
pub struct WsState {
    /// user_id → list of sender handles (supports multiple tabs/devices)
    pub connections: Arc<DashMap<i32, Vec<(Tx, Instant)>>>,
    pub pool: Pool<Postgres>,
}

impl WsState {
    pub fn new(pool: Pool<Postgres>) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            pool,
        }
    }

    /// Send a JSON message to all connections of a user.
    pub fn send_to_user(&self, user_id: i32, json: &str) {
        if let Some(ref mut senders) = self.connections.get_mut(&user_id) {
            let msg = Message::Text(json.to_string().into());
            senders.retain(|(tx, _)| tx.send(msg.clone()).is_ok());
        }
    }

    /// Broadcast a JSON message to all connected users.
    #[allow(dead_code)]
    pub fn broadcast(&self, json: &str) {
        let msg = Message::Text(json.to_string().into());
        for mut entry in self.connections.iter_mut() {
            let senders = entry.value_mut();
            senders.retain(|(tx, _)| tx.send(msg.clone()).is_ok());
        }
    }
}

// ── WebSocket message protocol ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// Authentication: sent as first message after connect
    Auth { token: String },
    /// Heartbeat ping
    Ping,
    /// Subscribe to a channel
    Subscribe { channel: String },
    /// Unsubscribe from a channel
    Unsubscribe { channel: String },
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    /// Heartbeat pong
    Pong,
    /// Authentication result
    AuthOk { user_id: i32 },
    /// Authentication failed
    AuthError { reason: String },
    /// New private message notification
    NewMessage {
        message_id: i32,
        sender_id: i32,
        sender_name: String,
        content_preview: String,
    },
    /// User came online
    UserOnline { user_id: i32 },
    /// User went offline
    UserOffline { user_id: i32 },
    /// Generic notification
    Notification { title: String, body: String },
}

// ── JWT validation for WebSocket ────────────────────────────────────────────

fn user_id_from_token(token: &str) -> Option<i32> {
    let secret =
        std::env::var("JWT_SECRET").unwrap_or_else(|_| "devbit-local-secret".to_string());
    #[derive(serde::Deserialize)]
    struct Claims {
        sub: i32,
    }
    let data = jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .ok()?;
    Some(data.claims.sub)
}

fn token_from_cookie(cookie_str: &str) -> Option<String> {
    cookie_str.split(';').find_map(|cookie| {
        cookie
            .trim()
            .strip_prefix("auth_token=")
            .map(str::to_string)
    })
}

// ── WebSocket handler ───────────────────────────────────────────────────────

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(ws_state): State<WsState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, ws_state))
}

async fn handle_socket(socket: WebSocket, ws_state: WsState) {
    let (mut sender_tx, mut receiver_rx) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Forward messages from the channel to the WebSocket sender
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut user_id: Option<i32> = None;
    let mut last_heartbeat = Instant::now();
    let mut authenticated = false;

    // Heartbeat ticker
    let mut heartbeat_timer =
        tokio::time::interval(HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            // Incoming messages from client
            msg = receiver_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            match client_msg {
                                ClientMessage::Auth { token } => {
                                    match user_id_from_token(&token) {
                                        Some(uid) => {
                                            user_id = Some(uid);
                                            authenticated = true;
                                            debug!(user_id = uid, "WebSocket authenticated");

                                            // Register connection
                                            ws_state.connections
                                                .entry(uid)
                                                .or_default()
                                                .push((tx.clone(), Instant::now()));

                                            // Broadcast online status
                                            let online_msg = serde_json::to_string(
                                                &ServerMessage::UserOnline { user_id: uid }
                                            ).unwrap_or_default();
                                            ws_state.broadcast(&online_msg);

                                            // Send auth confirmation
                                            let _ = tx.send(Message::Text(
                                                serde_json::to_string(
                                                    &ServerMessage::AuthOk { user_id: uid }
                                                ).unwrap_or_default().into()
                                            ));
                                        }
                                        None => {
                                            let _ = tx.send(Message::Text(
                                                serde_json::to_string(
                                                    &ServerMessage::AuthError {
                                                        reason: "Invalid token".into()
                                                    }
                                                ).unwrap_or_default().into()
                                            ));
                                            break;
                                        }
                                    }
                                }
                                ClientMessage::Ping => {
                                    last_heartbeat = Instant::now();
                                    let _ = tx.send(Message::Text(
                                        serde_json::to_string(&ServerMessage::Pong)
                                            .unwrap_or_default()
                                            .into()
                                    ));
                                }
                                ClientMessage::Subscribe { channel } => {
                                    debug!(?channel, user_id, "WebSocket subscribe");
                                    // Channel subscriptions can be extended later
                                    let _ = tx.send(Message::Text(
                                        format!(r#"{{"type":"subscribed","channel":"{}"}}"#, channel).into()
                                    ));
                                }
                                ClientMessage::Unsubscribe { channel } => {
                                    debug!(?channel, user_id, "WebSocket unsubscribe");
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = tx.send(Message::Pong(data));
                        last_heartbeat = Instant::now();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_heartbeat = Instant::now();
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // Binary messages not used in current protocol
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        break;
                    }
                }
            }

            // Heartbeat tick
            _ = heartbeat_timer.tick() => {
                let elapsed = last_heartbeat.elapsed();
                if elapsed > HEARTBEAT_TIMEOUT {
                    warn!(?user_id, "WebSocket heartbeat timeout");
                    break;
                }
                // Send server ping
                let _ = tx.send(Message::Ping(vec![]));
            }
        }
    }

    // ── Cleanup on disconnect ────────────────────────────────────────────
    if let Some(uid) = user_id {
        // Remove this connection
        let mut removed = false;
        if let Some(mut senders) = ws_state.connections.get_mut(&uid) {
            senders.retain(|(t, _)| !t.same_channel(&tx));
            if senders.is_empty() {
                removed = true;
            }
        }
        if removed {
            ws_state.connections.remove(&uid);
        }

        // Broadcast offline if no other connections remain
        if !ws_state.connections.contains_key(&uid) {
            let offline_msg = serde_json::to_string(
                &ServerMessage::UserOffline { user_id: uid }
            ).unwrap_or_default();
            ws_state.broadcast(&offline_msg);
            info!(user_id = uid, "User offline (all connections closed)");
        }
    }

    // Clean up forward task
    forward_task.abort();
}
