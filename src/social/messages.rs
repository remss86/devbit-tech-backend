//! Message permissions and per-user conversation visibility for Social V2.

use super::{SocialError, notify, positive_id, require_viewer};
use crate::ws;
use axum::{
    Extension,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use sqlx::{Pool, Postgres, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePermission {
    Allowed,
    Blocked,
    Restricted,
}

/// Server-side authority for "can sender send a direct message to recipient?".
///
/// The UI uses `canMessage` as a hint only; the send endpoint must always call
/// this function so blocks and receive preferences are enforced.
pub async fn message_permission(
    pool: &Pool<Postgres>,
    sender_id: i32,
    recipient_id: i32,
) -> Result<MessagePermission, sqlx::Error> {
    if sender_id == recipient_id {
        return Ok(MessagePermission::Restricted);
    }

    let row = sqlx::query(
        "SELECT
            EXISTS(
                SELECT 1 FROM social_blocks
                WHERE (user_id = $1 AND blocked_id = $2)
                   OR (user_id = $2 AND blocked_id = $1)
            ) AS blocked,
            COALESCE(
                (SELECT message_policy FROM social_preferences WHERE user_id = $2),
                'everyone'
            ) AS policy,
            EXISTS(
                SELECT 1 FROM social_friendships
                WHERE user_low = LEAST($1, $2) AND user_high = GREATEST($1, $2)
            ) AS friends",
    )
    .bind(sender_id)
    .bind(recipient_id)
    .fetch_one(pool)
    .await?;

    if row.get::<bool, _>("blocked") {
        return Ok(MessagePermission::Blocked);
    }

    let policy: String = row.get("policy");
    let friends: bool = row.get("friends");
    match policy.as_str() {
        "nobody" => Ok(MessagePermission::Restricted),
        "friends" if !friends => Ok(MessagePermission::Restricted),
        _ => Ok(MessagePermission::Allowed),
    }
}

pub(crate) async fn clear_history(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(partner_id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let partner_id = positive_id(partner_id, "partnerId")?;
    if partner_id == viewer.id {
        return Err(SocialError::validation("partnerId", "不能清除与自己的会话"));
    }

    let cleared_through_id: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(id), 0)::BIGINT FROM forum_messages
         WHERE (sender_id = $1 AND recipient_id = $2)
            OR (sender_id = $2 AND recipient_id = $1)",
    )
    .bind(viewer.id)
    .bind(partner_id)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;

    sqlx::query(
        "INSERT INTO social_message_clears (user_id, partner_id, cleared_through_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id, partner_id) DO UPDATE
         SET cleared_through_id = GREATEST(
                 social_message_clears.cleared_through_id,
                 EXCLUDED.cleared_through_id
             ),
             updated_at = NOW()",
    )
    .bind(viewer.id)
    .bind(partner_id)
    .bind(cleared_through_id)
    .execute(&pool)
    .await
    .map_err(SocialError::internal)?;

    notify(
        &ws_state,
        viewer.id,
        "conversation_changed",
        None,
        Some(partner_id),
    );
    Ok(StatusCode::NO_CONTENT)
}
