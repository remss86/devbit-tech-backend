use axum::{
    Extension,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::Pool;
use sqlx::Postgres;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ── WebSocket shared state ──────────────────────────────────────────────────

/// Per-connection sender handle.
type Tx = mpsc::Sender<Message>;

const OUTBOUND_QUEUE_CAPACITY: usize = 64;
const MAX_WEBSOCKET_MESSAGE_SIZE: usize = 64 * 1024;
const AUTH_COOKIE_NAME: &str = "auth_token";

#[derive(Clone)]
pub struct WsState {
    /// user_id → list of sender handles (supports multiple tabs/devices)
    connections: Arc<DashMap<i32, Vec<Tx>>>,
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
            let text = json.to_string();
            senders.retain(|tx| enqueue(tx, Message::Text(text.clone().into())));
        }
    }

    /// Broadcast a JSON message to all connected users.
    #[allow(dead_code)]
    pub fn broadcast(&self, json: &str) {
        let text = json.to_string();
        for mut entry in self.connections.iter_mut() {
            let senders = entry.value_mut();
            senders.retain(|tx| enqueue(tx, Message::Text(text.clone().into())));
        }
    }
}

fn enqueue(tx: &Tx, message: Message) -> bool {
    match tx.try_send(message) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("dropping WebSocket message because the outbound queue is full");
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

// ── WebSocket message protocol ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// Heartbeat ping
    Ping,
    /// Subscribe to a channel
    Subscribe { channel: String },
    /// Unsubscribe from a channel
    Unsubscribe { channel: String },
}

fn parse_application_message(text: &str) -> Result<ClientMessage, &'static str> {
    serde_json::from_str::<ClientMessage>(text).map_err(|_| "Invalid message")
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    /// Heartbeat pong
    Pong,
    /// Authentication result
    AuthOk { user_id: i32 },
    /// User came online
    UserOnline { user_id: i32 },
    /// User went offline
    UserOffline { user_id: i32 },
}

// ── JWT validation for WebSocket ────────────────────────────────────────────

fn token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix(&format!("{AUTH_COOKIE_NAME}="))
                    .filter(|token| !token.is_empty())
            })
        })
}

fn user_id_from_token_with_secret(token: &str, secret: &str) -> Option<i32> {
    #[derive(serde::Deserialize)]
    struct Claims {
        sub: i32,
    }
    let mut validation = jsonwebtoken::Validation::default();
    validation.leeway = 0;
    let data = jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()?;
    Some(data.claims.sub)
}

fn user_id_from_token(token: &str) -> Option<i32> {
    let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "devbit-local-secret".to_string());
    user_id_from_token_with_secret(token, &secret)
}

// ── WebSocket handler ───────────────────────────────────────────────────────

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Extension(ws_state): Extension<WsState>,
) -> Result<Response, StatusCode> {
    let token = token_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user_id = user_id_from_token(token).ok_or(StatusCode::UNAUTHORIZED)?;
    let user_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(user_id)
            .fetch_one(&ws_state.pool)
            .await
            .map_err(|error| {
                warn!(%error, "failed to validate WebSocket user");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    if !user_exists {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(ws
        .max_frame_size(MAX_WEBSOCKET_MESSAGE_SIZE)
        .max_message_size(MAX_WEBSOCKET_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_socket(socket, ws_state, user_id))
        .into_response())
}

async fn handle_socket(socket: WebSocket, ws_state: WsState, user_id: i32) {
    let (mut sender_tx, mut receiver_rx) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);

    // Forward messages from the channel to the WebSocket sender
    let mut forward_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut last_heartbeat = Instant::now();

    ws_state
        .connections
        .entry(user_id)
        .or_default()
        .push(tx.clone());
    debug!(user_id, "WebSocket authenticated");

    let online_msg =
        serde_json::to_string(&ServerMessage::UserOnline { user_id }).unwrap_or_default();
    ws_state.broadcast(&online_msg);
    enqueue(
        &tx,
        Message::Text(
            serde_json::to_string(&ServerMessage::AuthOk { user_id })
                .unwrap_or_default()
                .into(),
        ),
    );

    // Heartbeat ticker
    let mut heartbeat_timer = tokio::time::interval(HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            // Incoming messages from client
            msg = receiver_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match parse_application_message(&text) {
                            Ok(client_msg) => match client_msg {
                                ClientMessage::Ping => {
                                    last_heartbeat = Instant::now();
                                    enqueue(&tx, Message::Text(
                                        serde_json::to_string(&ServerMessage::Pong)
                                            .unwrap_or_default()
                                            .into()
                                    ));
                                }
                                ClientMessage::Subscribe { channel } => {
                                    debug!(?channel, user_id, "WebSocket subscribe");
                                    // Channel subscriptions can be extended later
                                    enqueue(&tx, Message::Text(
                                        format!(r#"{{"type":"subscribed","channel":"{}"}}"#, channel).into()
                                    ));
                                }
                                ClientMessage::Unsubscribe { channel } => {
                                    debug!(?channel, user_id, "WebSocket unsubscribe");
                                }
                            },
                            Err(reason) => {
                                let error = format!(r#"{{"type":"error","reason":"{reason}"}}"#);
                                enqueue(&tx, Message::Text(error.into()));
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        enqueue(&tx, Message::Pong(data));
                        last_heartbeat = Instant::now();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_heartbeat = Instant::now();
                    }
                    Some(Ok(Message::Binary(_))) => {
                        let error = r#"{"type":"error","reason":"Invalid message"}"#;
                        enqueue(&tx, Message::Text(error.into()));
                        break;
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
                    warn!(user_id, "WebSocket heartbeat timeout");
                    break;
                }
                // Send server ping
                enqueue(&tx, Message::Ping(Vec::new().into()));
            }
        }
    }

    // ── Cleanup on disconnect ────────────────────────────────────────────
    {
        // Remove this connection
        if let Some(mut senders) = ws_state.connections.get_mut(&user_id) {
            senders.retain(|sender| !sender.same_channel(&tx));
        }
        let removed = ws_state
            .connections
            .remove_if(&user_id, |_, senders| senders.is_empty())
            .is_some();

        // Broadcast offline if no other connections remain
        if removed {
            let offline_msg =
                serde_json::to_string(&ServerMessage::UserOffline { user_id }).unwrap_or_default();
            ws_state.broadcast(&offline_msg);
            info!(user_id, "User offline (all connections closed)");
        }
    }

    // Let queued protocol responses flush, but never let a stalled peer retain the task.
    drop(tx);
    if tokio::time::timeout(Duration::from_secs(1), &mut forward_task)
        .await
        .is_err()
    {
        warn!("timed out flushing the WebSocket outbound queue");
        forward_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde::Serialize;

    #[test]
    fn client_auth_messages_are_not_part_of_the_protocol() {
        assert_eq!(
            parse_application_message(r#"{"type":"auth","token":"token"}"#).unwrap_err(),
            "Invalid message"
        );
        assert!(matches!(
            parse_application_message(r#"{"type":"ping"}"#),
            Ok(ClientMessage::Ping)
        ));
    }

    #[test]
    fn auth_token_is_read_from_cookie_headers() {
        let headers = HeaderMap::from_iter([(
            axum::http::header::COOKIE,
            "theme=dark; auth_token=jwt-value; locale=en"
                .parse()
                .unwrap(),
        )]);
        assert_eq!(token_from_headers(&headers), Some("jwt-value"));
        assert_eq!(token_from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn token_validation_rejects_invalid_and_expired_tokens() {
        #[derive(Serialize)]
        struct Claims {
            sub: i32,
            exp: usize,
        }

        let secret = "test-secret";
        let valid = encode(
            &Header::default(),
            &Claims {
                sub: 42,
                exp: (chrono::Utc::now().timestamp() + 60) as usize,
            },
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        let expired = encode(
            &Header::default(),
            &Claims {
                sub: 42,
                exp: (chrono::Utc::now().timestamp() - 1) as usize,
            },
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        assert_eq!(user_id_from_token_with_secret(&valid, secret), Some(42));
        assert_eq!(user_id_from_token_with_secret(&expired, secret), None);
        assert_eq!(user_id_from_token_with_secret("not-a-token", secret), None);
    }

    #[tokio::test]
    async fn full_queue_drops_message_without_discarding_sender() {
        let (tx, mut rx) = mpsc::channel(1);
        assert!(enqueue(&tx, Message::Text("first".into())));
        assert!(enqueue(&tx, Message::Text("dropped".into())));
        assert!(matches!(rx.recv().await, Some(Message::Text(text)) if text == "first"));
        assert!(enqueue(&tx, Message::Text("next".into())));
    }

    #[test]
    fn closed_queue_discards_sender() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert!(!enqueue(&tx, Message::Text("message".into())));
    }
}
