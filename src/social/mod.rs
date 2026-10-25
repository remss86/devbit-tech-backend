//! Social V2 API shared plumbing.
//!
//! The module implements the backend side of `SOCIAL-API-V2.md`:
//! symmetric friendships, friend requests, blocks, receive preferences,
//! presence snapshots and per-user conversation visibility.
//!
//! All error bodies use the contract envelope:
//! `{"error":{"code":"...","message":"...","fieldErrors":{...}}}`.

mod messages;
mod model;
mod relationships;

pub use messages::{MessagePermission, message_permission};
pub use model::MessagePolicy;

use crate::{
    auth,
    forum::{self, ForumUser},
    ws,
};
use axum::{
    Json, Router,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{Pool, Postgres, Row};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const DEFAULT_PAGE_SIZE: u32 = 20;
pub const MAX_PAGE_SIZE: u32 = 50;

pub fn social_routes() -> Router<Pool<Postgres>> {
    Router::new().merge(v2_routes()).nest("/api", v2_routes())
}

fn v2_routes() -> Router<Pool<Postgres>> {
    Router::new()
        .route("/v2/social/users", get(relationships::search_users))
        .route(
            "/v2/social/users/{user_id}/profile",
            get(relationships::get_profile),
        )
        .route("/v2/social/feed", get(relationships::friend_feed))
        .route("/v2/social/friends", get(relationships::list_friends))
        .route(
            "/v2/social/friends/{user_id}",
            delete(relationships::remove_friend),
        )
        .route(
            "/v2/social/friend-requests",
            get(relationships::list_requests).post(relationships::create_request),
        )
        .route(
            "/v2/social/friend-requests/{id}/accept",
            post(relationships::accept_request),
        )
        .route(
            "/v2/social/friend-requests/{id}/reject",
            post(relationships::reject_request),
        )
        .route(
            "/v2/social/friend-requests/{id}",
            delete(relationships::cancel_request),
        )
        .route("/v2/social/blocks", get(relationships::list_blocks))
        .route(
            "/v2/social/blocks/{user_id}",
            put(relationships::block_user).delete(relationships::unblock_user),
        )
        .route(
            "/v2/social/preferences",
            get(relationships::get_preferences).patch(relationships::update_preferences),
        )
        .route("/v2/social/presence", get(relationships::presence))
        .route(
            "/v2/social/conversations/{partner_id}/history",
            delete(messages::clear_history),
        )
}

/// Authenticated caller.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Viewer {
    pub id: i32,
}

pub(crate) async fn require_viewer(
    pool: &Pool<Postgres>,
    headers: &HeaderMap,
) -> Result<Viewer, SocialError> {
    let identity = auth::identity_from_headers(headers).ok_or_else(SocialError::unauthenticated)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(identity.user_id)
        .fetch_one(pool)
        .await
        .map_err(SocialError::internal)?;
    if !exists {
        return Err(SocialError::unauthenticated());
    }

    Ok(Viewer {
        id: identity.user_id,
    })
}

pub(crate) async fn load_forum_user(
    pool: &Pool<Postgres>,
    user_id: i32,
) -> Result<Option<ForumUser>, sqlx::Error> {
    let row = sqlx::query("SELECT id, name, avatar_url FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|row| {
        let id: i32 = row.get("id");
        forum::forum_user(id, row.get("name"), row.get("avatar_url"))
    }))
}

#[derive(Debug)]
pub struct SocialError {
    status: StatusCode,
    code: &'static str,
    message: String,
    field_errors: BTreeMap<String, String>,
    retry_after_seconds: Option<u64>,
}

impl SocialError {
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "登录状态已失效，请重新登录",
        )
    }

    pub fn validation(field: &'static str, message: impl Into<String>) -> Self {
        let mut error = Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "请求参数不合法",
        );
        error.field_errors.insert(field.to_string(), message.into());
        error
    }

    pub fn bad_cursor() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "BAD_CURSOR",
            "分页游标已失效，请重新加载",
        )
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "FORBIDDEN", message)
    }

    pub fn user_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "USER_NOT_FOUND",
            "用户不存在或已注销",
        )
    }

    pub fn request_not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "REQUEST_NOT_FOUND", "好友申请不存在")
    }

    pub fn relationship_changed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "RELATIONSHIP_CHANGED",
            "关系已发生变化，请刷新后重试",
        )
    }

    pub fn request_not_pending() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "REQUEST_NOT_PENDING",
            "该好友申请已处理",
        )
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "social request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "服务暂时不可用，请稍后重试",
        )
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            field_errors: BTreeMap::new(),
            retry_after_seconds: None,
        }
    }
}

impl IntoResponse for SocialError {
    fn into_response(self) -> Response {
        let mut body = Map::new();
        body.insert("code".into(), Value::String(self.code.to_string()));
        body.insert("message".into(), Value::String(self.message));
        if !self.field_errors.is_empty() {
            body.insert(
                "fieldErrors".into(),
                serde_json::to_value(&self.field_errors).unwrap_or(Value::Null),
            );
        }
        if let Some(seconds) = self.retry_after_seconds {
            body.insert("retryAfterSeconds".into(), json!(seconds));
        }

        let mut response = (self.status, Json(json!({ "error": body }))).into_response();
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimeCursor {
    pub at: DateTime<Utc>,
    pub id: i64,
}

#[derive(Serialize, Deserialize)]
struct TimeCursorPayload {
    at: DateTime<Utc>,
    id: i64,
}

#[derive(Serialize, Deserialize)]
struct OffsetCursorPayload {
    offset: i64,
}

pub(crate) fn encode_time_cursor(at: DateTime<Utc>, id: i64) -> String {
    let payload = TimeCursorPayload { at, id };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap_or_default())
}

pub(crate) fn decode_time_cursor(raw: Option<&str>) -> Result<Option<TimeCursor>, SocialError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| SocialError::bad_cursor())?;
    let payload: TimeCursorPayload =
        serde_json::from_slice(&bytes).map_err(|_| SocialError::bad_cursor())?;
    Ok(Some(TimeCursor {
        at: payload.at,
        id: payload.id,
    }))
}

pub(crate) fn encode_offset_cursor(offset: i64) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&OffsetCursorPayload { offset }).unwrap_or_default())
}

pub(crate) fn decode_offset_cursor(raw: Option<&str>) -> Result<i64, SocialError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(0);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| SocialError::bad_cursor())?;
    let payload: OffsetCursorPayload =
        serde_json::from_slice(&bytes).map_err(|_| SocialError::bad_cursor())?;
    if !(0..=100_000).contains(&payload.offset) {
        return Err(SocialError::bad_cursor());
    }
    Ok(payload.offset)
}

pub(crate) fn page_limit(limit: Option<u32>) -> i64 {
    i64::from(limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE))
}

pub(crate) fn validate_optional_message(
    value: Option<&str>,
    max_chars: usize,
) -> Result<String, SocialError> {
    let trimmed = value.unwrap_or("").trim();
    if trimmed.chars().count() > max_chars {
        return Err(SocialError::validation(
            "message",
            format!("长度不能超过 {max_chars} 个字符"),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn validate_query(value: Option<&str>) -> Result<String, SocialError> {
    let trimmed = value.unwrap_or("").trim();
    if trimmed.chars().count() > 50 {
        return Err(SocialError::validation("q", "搜索词不能超过 50 个字符"));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn positive_id(id: i64, field: &'static str) -> Result<i32, SocialError> {
    if id <= 0 || id > i64::from(i32::MAX) {
        return Err(SocialError::validation(field, "标识必须是正整数"));
    }
    i32::try_from(id).map_err(|_| SocialError::validation(field, "标识超出范围"))
}

pub(crate) fn validate_positive_i64(id: i64, field: &'static str) -> Result<i64, SocialError> {
    if id <= 0 {
        return Err(SocialError::validation(field, "标识必须是正整数"));
    }
    Ok(id)
}

pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Serialise relationship mutations for one user pair.
///
/// The advisory lock always uses the same `(low, high)` key order, so two
/// opposite friend requests, an accept and a block cannot interleave into an
/// inconsistent state. It is released automatically when the transaction ends.
pub(crate) async fn lock_pair(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    first: i32,
    second: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(LEAST($1, $2), GREATEST($1, $2))")
        .bind(first)
        .bind(second)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

pub(crate) fn notify(
    ws_state: &ws::WsState,
    user_id: i32,
    event_type: &str,
    request_id: Option<i64>,
    actor_id: Option<i32>,
) {
    let mut event = Map::new();
    event.insert("type".into(), Value::String(event_type.to_string()));
    if let Some(id) = request_id {
        event.insert("requestId".into(), json!(id));
    }
    if let Some(id) = actor_id {
        event.insert("userId".into(), json!(id));
    }
    ws_state.send_to_user(user_id, &Value::Object(event).to_string());
}

#[derive(Default)]
pub(crate) struct RelationshipContext {
    friends: HashSet<i32>,
    blocked_by_me: HashSet<i32>,
    blocking_me: HashSet<i32>,
    outgoing: HashMap<i32, i64>,
    incoming: HashMap<i32, i64>,
    policies: HashMap<i32, MessagePolicy>,
}

impl RelationshipContext {
    pub async fn load(
        pool: &Pool<Postgres>,
        viewer_id: i32,
        targets: &[i32],
    ) -> Result<Self, sqlx::Error> {
        let mut context = Self::default();
        if targets.is_empty() {
            return Ok(context);
        }

        let rows = sqlx::query(
            "SELECT CASE WHEN f.user_low = $1 THEN f.user_high ELSE f.user_low END AS other
             FROM social_friendships f
             WHERE (f.user_low = $1 AND f.user_high = ANY($2))
                OR (f.user_high = $1 AND f.user_low = ANY($2))",
        )
        .bind(viewer_id)
        .bind(targets)
        .fetch_all(pool)
        .await?;
        context.friends = rows.iter().map(|row| row.get("other")).collect();

        let rows = sqlx::query(
            "SELECT user_id, blocked_id FROM social_blocks
             WHERE (user_id = $1 AND blocked_id = ANY($2))
                OR (blocked_id = $1 AND user_id = ANY($2))",
        )
        .bind(viewer_id)
        .bind(targets)
        .fetch_all(pool)
        .await?;
        for row in &rows {
            let user_id: i32 = row.get("user_id");
            let blocked_id: i32 = row.get("blocked_id");
            if user_id == viewer_id {
                context.blocked_by_me.insert(blocked_id);
            } else {
                context.blocking_me.insert(user_id);
            }
        }

        let rows = sqlx::query(
            "SELECT id, sender_id, recipient_id FROM social_friend_requests
             WHERE status = 'pending'
               AND ((sender_id = $1 AND recipient_id = ANY($2))
                 OR (recipient_id = $1 AND sender_id = ANY($2)))",
        )
        .bind(viewer_id)
        .bind(targets)
        .fetch_all(pool)
        .await?;
        for row in &rows {
            let id: i64 = row.get("id");
            let sender_id: i32 = row.get("sender_id");
            let recipient_id: i32 = row.get("recipient_id");
            if sender_id == viewer_id {
                context.outgoing.insert(recipient_id, id);
            } else {
                context.incoming.insert(sender_id, id);
            }
        }

        let rows = sqlx::query(
            "SELECT user_id, message_policy FROM social_preferences WHERE user_id = ANY($1)",
        )
        .bind(targets)
        .fetch_all(pool)
        .await?;
        for row in &rows {
            let user_id: i32 = row.get("user_id");
            let policy: String = row.get("message_policy");
            context
                .policies
                .insert(user_id, MessagePolicy::from_db(&policy));
        }

        Ok(context)
    }

    pub fn relationship(&self, target_id: i32) -> (model::Relationship, Option<i64>) {
        if self.blocked_by_me.contains(&target_id) {
            return (model::Relationship::Blocked, None);
        }
        if self.friends.contains(&target_id) {
            return (model::Relationship::Friends, None);
        }
        if let Some(request_id) = self.outgoing.get(&target_id) {
            return (model::Relationship::OutgoingPending, Some(*request_id));
        }
        if let Some(request_id) = self.incoming.get(&target_id) {
            return (model::Relationship::IncomingPending, Some(*request_id));
        }
        (model::Relationship::None, None)
    }

    pub fn can_message(&self, viewer_id: i32, target_id: i32) -> bool {
        if viewer_id == target_id
            || self.blocked_by_me.contains(&target_id)
            || self.blocking_me.contains(&target_id)
        {
            return false;
        }
        match self
            .policies
            .get(&target_id)
            .copied()
            .unwrap_or(MessagePolicy::Everyone)
        {
            MessagePolicy::Everyone => true,
            MessagePolicy::Nobody => false,
            MessagePolicy::Friends => self.friends.contains(&target_id),
        }
    }

    pub fn is_friend(&self, target_id: i32) -> bool {
        self.friends.contains(&target_id)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct LimitQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

pub(crate) async fn current_friend_request_view(
    pool: &Pool<Postgres>,
    request_id: i64,
) -> Result<model::FriendRequest, SocialError> {
    let row = sqlx::query(
        "SELECT r.id, r.message, r.status, r.created_at, r.updated_at,
                s.id AS sender_id, s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                t.id AS recipient_id, t.name AS recipient_name, t.avatar_url AS recipient_avatar_url
         FROM social_friend_requests r
         JOIN users s ON s.id = r.sender_id
         JOIN users t ON t.id = r.recipient_id
         WHERE r.id = $1",
    )
    .bind(request_id)
    .fetch_optional(pool)
    .await
    .map_err(SocialError::internal)?
    .ok_or_else(SocialError::request_not_found)?;

    Ok(friend_request_from_row(&row))
}

pub(crate) fn friend_request_from_row(row: &sqlx::postgres::PgRow) -> model::FriendRequest {
    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");
    let status: String = row.get("status");
    model::FriendRequest {
        id: row.get("id"),
        sender: forum::forum_user(
            sender_id,
            row.get("sender_name"),
            row.get("sender_avatar_url"),
        ),
        recipient: forum::forum_user(
            recipient_id,
            row.get("recipient_name"),
            row.get("recipient_avatar_url"),
        ),
        message: row.get("message"),
        status: model::RequestStatus::from_db(&status),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
        updated_at: row.get::<DateTime<Utc>, _>("updated_at").to_rfc3339(),
    }
}

#[cfg(test)]
mod tests;
