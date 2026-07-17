use crate::auth;
use axum::{
    Extension,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::HeaderMap,
    response::IntoResponse,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

type Tx = mpsc::Sender<Message>;
type DisconnectTx = watch::Sender<bool>;

#[derive(Clone)]
struct WsConnection {
    sender: Tx,
    disconnect: DisconnectTx,
}

const OUTBOUND_QUEUE_CAPACITY: usize = 64;
const MAX_WEBSOCKET_MESSAGE_SIZE: usize = 64 * 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone)]
pub struct WsState {
    connections: Arc<DashMap<i32, Vec<WsConnection>>>,
    presence_transitions: Arc<Mutex<()>>,
}

impl WsState {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            presence_transitions: Arc::new(Mutex::new(())),
        }
    }

    fn register(&self, user_id: i32, sender: Tx, disconnect: DisconnectTx) -> bool {
        let mut senders = self.connections.entry(user_id).or_default();
        let was_offline = senders.is_empty();
        senders.push(WsConnection { sender, disconnect });
        was_offline
    }

    fn unregister(&self, user_id: i32, sender: &Tx) -> bool {
        if let Some(mut senders) = self.connections.get_mut(&user_id) {
            senders.retain(|registered| !registered.sender.same_channel(sender));
        }

        self.connections
            .remove_if(&user_id, |_, senders| senders.is_empty())
            .is_some()
    }

    pub fn send_to_user(&self, user_id: i32, json: &str) {
        if let Some(ref mut senders) = self.connections.get_mut(&user_id) {
            let text = json.to_string();
            senders.retain(|connection| {
                enqueue(&connection.sender, Message::Text(text.clone().into()))
            });
        }
    }

    pub fn broadcast(&self, json: &str) {
        let text = json.to_string();
        for mut entry in self.connections.iter_mut() {
            entry.value_mut().retain(|connection| {
                enqueue(&connection.sender, Message::Text(text.clone().into()))
            });
        }
    }

    pub fn disconnect_user(&self, user_id: i32) {
        let _transition = self
            .presence_transitions
            .lock()
            .expect("presence transition lock must not be poisoned");
        let Some((_, senders)) = self.connections.remove(&user_id) else {
            return;
        };

        for connection in senders {
            connection.disconnect.send_replace(true);
        }
        let offline_message =
            serde_json::to_string(&ServerMessage::UserOffline { user_id }).unwrap_or_default();
        self.broadcast(&offline_message);
    }
}

impl Default for WsState {
    fn default() -> Self {
        Self::new()
    }
}

fn enqueue(sender: &Tx, message: Message) -> bool {
    match sender.try_send(message) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("dropping WebSocket message because the outbound queue is full");
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Auth { token: String },
    Ping,
    Subscribe { channel: String },
    Unsubscribe { channel: String },
}

fn parse_application_message(
    text: &str,
    authenticated: bool,
) -> Result<ClientMessage, &'static str> {
    let message = serde_json::from_str::<ClientMessage>(text).map_err(|_| {
        if authenticated {
            "Invalid message"
        } else {
            "Authentication must be the first message"
        }
    })?;

    match (&message, authenticated) {
        (ClientMessage::Auth { .. }, true) => Err("Already authenticated"),
        (ClientMessage::Auth { .. }, false) => Ok(message),
        (_, false) => Err("Authentication must be the first message"),
        (_, true) => Ok(message),
    }
}

fn compatible_redundant_identity(text: &str, user_id: Option<i32>) -> Option<auth::AuthIdentity> {
    let Ok(ClientMessage::Auth { token }) = serde_json::from_str::<ClientMessage>(text) else {
        return None;
    };
    auth::identity_from_token(&token).filter(|identity| Some(identity.user_id) == user_id)
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Pong,
    AuthOk { user_id: i32 },
    AuthError { reason: String },
    UserOnline { user_id: i32 },
    UserOffline { user_id: i32 },
    Subscribed { channel: String },
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(ws_state): Extension<WsState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let initial_identity = auth::identity_from_headers(&headers);
    ws.max_frame_size(MAX_WEBSOCKET_MESSAGE_SIZE)
        .max_message_size(MAX_WEBSOCKET_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_socket(socket, ws_state, initial_identity))
}

fn enqueue_server_message(sender: &Tx, message: &ServerMessage) {
    let json = serde_json::to_string(message).unwrap_or_default();
    enqueue(sender, Message::Text(json.into()));
}

fn register_authenticated_connection(
    ws_state: &WsState,
    sender: &Tx,
    disconnect: &DisconnectTx,
    user_id: i32,
) {
    let _transition = ws_state
        .presence_transitions
        .lock()
        .expect("presence transition lock must not be poisoned");
    let came_online = ws_state.register(user_id, sender.clone(), disconnect.clone());

    // Keep auth_ok first so cookie-authenticated clients can use the connection immediately.
    enqueue_server_message(sender, &ServerMessage::AuthOk { user_id });

    if came_online {
        let online_message =
            serde_json::to_string(&ServerMessage::UserOnline { user_id }).unwrap_or_default();
        ws_state.broadcast(&online_message);
    }
}

fn unregister_authenticated_connection(ws_state: &WsState, sender: &Tx, user_id: i32) -> bool {
    let _transition = ws_state
        .presence_transitions
        .lock()
        .expect("presence transition lock must not be poisoned");

    if !ws_state.unregister(user_id, sender) {
        return false;
    }

    let offline_message =
        serde_json::to_string(&ServerMessage::UserOffline { user_id }).unwrap_or_default();
    ws_state.broadcast(&offline_message);
    true
}

async fn handle_socket(
    socket: WebSocket,
    ws_state: WsState,
    initial_identity: Option<auth::AuthIdentity>,
) {
    let (mut websocket_sender, mut websocket_receiver) = socket.split();
    let (sender, mut outbound_messages) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let (disconnect, mut disconnect_receiver) = watch::channel(false);

    let mut forward_task = tokio::spawn(async move {
        while let Some(message) = outbound_messages.recv().await {
            if websocket_sender.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut user_id = initial_identity.map(|identity| identity.user_id);
    let mut authenticated = user_id.is_some();
    let mut accepts_compatible_auth_message = authenticated;
    let mut last_heartbeat = Instant::now();

    if let Some(user_id) = user_id {
        debug!(user_id, "WebSocket authenticated from upgrade headers");
        register_authenticated_connection(&ws_state, &sender, &disconnect, user_id);
    }

    let auth_deadline = tokio::time::sleep(AUTH_TIMEOUT);
    tokio::pin!(auth_deadline);
    let token_expiry = tokio::time::sleep(
        initial_identity
            .map(auth::remaining_token_lifetime)
            .unwrap_or(AUTH_TIMEOUT),
    );
    tokio::pin!(token_expiry);
    let mut heartbeat_timer = tokio::time::interval(HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            _ = &mut auth_deadline, if !authenticated => {
                enqueue_server_message(
                    &sender,
                    &ServerMessage::AuthError {
                        reason: "Authentication timed out".into(),
                    },
                );
                break;
            }
            _ = &mut token_expiry, if authenticated => {
                enqueue_server_message(
                    &sender,
                    &ServerMessage::AuthError {
                        reason: "Token expired".into(),
                    },
                );
                break;
            }
            changed = disconnect_receiver.changed() => {
                if changed.is_ok() && *disconnect_receiver.borrow() {
                    debug!(?user_id, "WebSocket disconnected by server");
                }
                break;
            }
            incoming = websocket_receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if accepts_compatible_auth_message {
                            accepts_compatible_auth_message = false;
                            if let Some(identity) = compatible_redundant_identity(&text, user_id) {
                                token_expiry.as_mut().reset(
                                    tokio::time::Instant::now()
                                        + auth::remaining_token_lifetime(identity),
                                );
                                continue;
                            }
                        }

                        match parse_application_message(&text, authenticated) {
                            Ok(ClientMessage::Auth { token }) => {
                                match auth::identity_from_token(&token) {
                                    Some(identity) => {
                                        let authenticated_user_id = identity.user_id;
                                        user_id = Some(authenticated_user_id);
                                        authenticated = true;
                                        token_expiry.as_mut().reset(
                                            tokio::time::Instant::now()
                                                + auth::remaining_token_lifetime(identity),
                                        );
                                        debug!(
                                            user_id = authenticated_user_id,
                                            "WebSocket authenticated from first message"
                                        );
                                        register_authenticated_connection(
                                            &ws_state,
                                            &sender,
                                            &disconnect,
                                            authenticated_user_id,
                                        );
                                    }
                                    None => {
                                        enqueue_server_message(
                                            &sender,
                                            &ServerMessage::AuthError {
                                                reason: "Invalid token".into(),
                                            },
                                        );
                                        break;
                                    }
                                }
                            }
                            Ok(ClientMessage::Ping) => {
                                last_heartbeat = Instant::now();
                                enqueue_server_message(&sender, &ServerMessage::Pong);
                            }
                            Ok(ClientMessage::Subscribe { channel }) => {
                                debug!(?channel, user_id, "WebSocket subscribe");
                                enqueue_server_message(
                                    &sender,
                                    &ServerMessage::Subscribed { channel },
                                );
                            }
                            Ok(ClientMessage::Unsubscribe { channel }) => {
                                debug!(?channel, user_id, "WebSocket unsubscribe");
                            }
                            Err(reason) => {
                                enqueue_server_message(
                                    &sender,
                                    &ServerMessage::AuthError {
                                        reason: reason.into(),
                                    },
                                );
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        enqueue(&sender, Message::Pong(data));
                        last_heartbeat = Instant::now();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_heartbeat = Instant::now();
                    }
                    Some(Ok(Message::Binary(_))) => {
                        if !authenticated {
                            enqueue_server_message(
                                &sender,
                                &ServerMessage::AuthError {
                                    reason: "Authentication must be the first message".into(),
                                },
                            );
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        warn!(%error, "WebSocket error");
                        break;
                    }
                }
            }
            _ = heartbeat_timer.tick() => {
                if last_heartbeat.elapsed() > HEARTBEAT_TIMEOUT {
                    warn!(?user_id, "WebSocket heartbeat timeout");
                    break;
                }
                enqueue(&sender, Message::Ping(Vec::new().into()));
            }
        }
    }

    if let Some(user_id) = user_id
        && unregister_authenticated_connection(&ws_state, &sender, user_id)
    {
        info!(user_id, "User offline (all connections closed)");
    }

    drop(sender);
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
    use axum::http::header;

    #[test]
    fn authentication_must_be_first_application_message() {
        assert!(matches!(
            parse_application_message(r#"{"type":"auth","token":"token"}"#, false),
            Ok(ClientMessage::Auth { .. })
        ));
        assert_eq!(
            parse_application_message(r#"{"type":"ping"}"#, false).unwrap_err(),
            "Authentication must be the first message"
        );
        assert_eq!(
            parse_application_message(r#"{"type":"auth","token":"token"}"#, true).unwrap_err(),
            "Already authenticated"
        );
    }

    #[test]
    fn upgrade_cookie_establishes_an_identity() {
        let token = auth::generate_token(42, "websocket@example.com").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("theme=dark; {}={token}", auth::AUTH_COOKIE_NAME)
                .parse()
                .unwrap(),
        );

        assert_eq!(auth::user_id_from_headers(&headers), Some(42));
        assert_eq!(
            compatible_redundant_identity(
                &format!(r#"{{"type":"auth","token":"{token}"}}"#),
                Some(42),
            )
            .map(|identity| identity.user_id),
            Some(42)
        );
    }

    #[tokio::test]
    async fn auth_ok_precedes_the_online_broadcast() {
        let state = WsState::new();
        let (sender, mut receiver) = mpsc::channel(4);
        let (disconnect, _disconnect_receiver) = watch::channel(false);

        register_authenticated_connection(&state, &sender, &disconnect, 7);

        assert!(matches!(
            receiver.recv().await,
            Some(Message::Text(text)) if text.contains(r#""type":"auth_ok""#)
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(Message::Text(text)) if text.contains(r#""type":"user_online""#)
        ));
    }

    #[tokio::test]
    async fn second_tab_does_not_create_another_online_transition() {
        let state = WsState::new();
        let (first_sender, _first_receiver) = mpsc::channel(1);
        let (second_sender, _second_receiver) = mpsc::channel(1);
        let (first_disconnect, _first_disconnect_receiver) = watch::channel(false);
        let (second_disconnect, _second_disconnect_receiver) = watch::channel(false);

        assert!(state.register(7, first_sender.clone(), first_disconnect));
        assert!(!state.register(7, second_sender.clone(), second_disconnect));
        assert!(!state.unregister(7, &first_sender));
        assert!(state.unregister(7, &second_sender));
    }

    #[tokio::test]
    async fn reconnect_transition_follows_offline_broadcast() {
        let state = WsState::new();
        let (observer, mut observer_messages) = mpsc::channel(4);
        let (first_sender, _first_messages) = mpsc::channel(1);
        let (second_sender, _second_messages) = mpsc::channel(1);
        let (observer_disconnect, _observer_disconnect_receiver) = watch::channel(false);
        let (first_disconnect, _first_disconnect_receiver) = watch::channel(false);
        let (second_disconnect, _second_disconnect_receiver) = watch::channel(false);
        state.register(99, observer, observer_disconnect);
        state.register(7, first_sender.clone(), first_disconnect);

        assert!(unregister_authenticated_connection(
            &state,
            &first_sender,
            7
        ));
        register_authenticated_connection(&state, &second_sender, &second_disconnect, 7);

        assert!(matches!(
            observer_messages.recv().await,
            Some(Message::Text(text)) if text.contains(r#""type":"user_offline""#)
        ));
        assert!(matches!(
            observer_messages.recv().await,
            Some(Message::Text(text)) if text.contains(r#""type":"user_online""#)
        ));
    }

    #[tokio::test]
    async fn disconnect_user_closes_all_tabs_and_broadcasts_offline() {
        let state = WsState::new();
        let (observer, mut observer_messages) = mpsc::channel(2);
        let (first_sender, _first_messages) = mpsc::channel(1);
        let (second_sender, _second_messages) = mpsc::channel(1);
        let (observer_disconnect, _observer_disconnect_receiver) = watch::channel(false);
        let (first_disconnect, mut first_disconnect_receiver) = watch::channel(false);
        let (second_disconnect, mut second_disconnect_receiver) = watch::channel(false);
        state.register(99, observer, observer_disconnect);
        state.register(7, first_sender.clone(), first_disconnect);
        state.register(7, second_sender, second_disconnect);

        // A full data queue must not prevent the control-plane disconnect.
        assert!(enqueue(&first_sender, Message::Text("queued".into())));

        state.disconnect_user(7);

        first_disconnect_receiver.changed().await.unwrap();
        second_disconnect_receiver.changed().await.unwrap();
        assert!(*first_disconnect_receiver.borrow());
        assert!(*second_disconnect_receiver.borrow());
        assert!(matches!(
            observer_messages.recv().await,
            Some(Message::Text(text)) if text.contains(r#""type":"user_offline""#)
        ));
        assert!(!state.connections.contains_key(&7));
    }

    #[tokio::test]
    async fn full_queue_drops_message_without_discarding_sender() {
        let (sender, mut receiver) = mpsc::channel(1);
        assert!(enqueue(&sender, Message::Text("first".into())));
        assert!(enqueue(&sender, Message::Text("dropped".into())));
        assert!(matches!(receiver.recv().await, Some(Message::Text(text)) if text == "first"));
        assert!(enqueue(&sender, Message::Text("next".into())));
    }

    #[test]
    fn closed_queue_discards_sender() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        assert!(!enqueue(&sender, Message::Text("message".into())));
    }
}
