//! Message permissions, conversations and per-user conversation visibility.
//!
//! Sending is governed by three independent rules:
//! 1. either side may block the other (hard block);
//! 2. the recipient's `message_policy` decides who may start a conversation;
//! 3. a non-friend may send only one message until the recipient replies.
//!
//! Rule 3 is deliberately persisted in PostgreSQL. Older clients implemented a
//! one-message limit in memory, which reset on refresh; this endpoint is the
//! authority for every send path.

use super::model::{
    ConversationPreferences, ConversationPreferencesPatch, ConversationSummary, Page,
    ReadConversationPayload, SendWorkspaceMessagePayload, WorkspaceMessage,
};
use super::{
    SocialError, decode_offset_cursor, decode_time_cursor, encode_offset_cursor,
    encode_time_cursor, load_forum_user, notify, page_limit, positive_id, require_viewer,
    validate_positive_i64,
};
use crate::{forum, ws};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use sqlx::{Pool, Postgres, Row};
use uuid::Uuid;

const MAX_MESSAGE_CHARS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePermission {
    Allowed,
    Blocked,
    Restricted,
    /// Non-friend already sent one message and the recipient has not replied.
    OneMessageLimit,
}

/// Server-side authority for "can sender send a direct message to recipient?".
///
/// The UI uses `canMessage` as a hint only; every send endpoint must call this
/// function so blocks, receive preferences and the stranger one-message limit
/// are enforced even after the sender refreshes the page.
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
            ) AS friends,
            EXISTS(
                SELECT 1 FROM forum_messages
                WHERE sender_id = $1 AND recipient_id = $2
            ) AS sender_sent,
            EXISTS(
                SELECT 1 FROM forum_messages
                WHERE sender_id = $2 AND recipient_id = $1
            ) AS recipient_replied",
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
        "nobody" => return Ok(MessagePermission::Restricted),
        "friends" if !friends => return Ok(MessagePermission::Restricted),
        _ => {}
    }

    let sender_sent: bool = row.get("sender_sent");
    let recipient_replied: bool = row.get("recipient_replied");
    if !friends && sender_sent && !recipient_replied {
        return Ok(MessagePermission::OneMessageLimit);
    }

    Ok(MessagePermission::Allowed)
}

pub fn permission_error(permission: MessagePermission) -> SocialError {
    match permission {
        MessagePermission::Allowed => SocialError::internal("permission unexpectedly allowed"),
        MessagePermission::Blocked => SocialError::forbidden("当前关系不允许发送私信"),
        MessagePermission::Restricted => SocialError::forbidden("对方当前不接收你的私信"),
        MessagePermission::OneMessageLimit => {
            SocialError::message_limit("对方回复前，你只能发送一条私信")
        }
    }
}

fn row_to_workspace_message(row: &sqlx::postgres::PgRow) -> WorkspaceMessage {
    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");
    WorkspaceMessage {
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
        content: row.get("content"),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
        is_read: row.get("is_read"),
        client_message_id: row
            .try_get::<Option<Uuid>, _>("client_message_id")
            .ok()
            .flatten()
            .map(|value| value.to_string()),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ConversationsQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

pub(crate) async fn list_conversations(
    State(pool): State<Pool<Postgres>>,
    Query(query): Query<ConversationsQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<ConversationSummary>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let offset = decode_offset_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    let mut rows = sqlx::query(
        "WITH visible AS (
             SELECT m.*,
                    CASE WHEN m.sender_id = $1 THEN m.recipient_id ELSE m.sender_id END AS partner_id
             FROM forum_messages m
             WHERE (m.sender_id = $1 OR m.recipient_id = $1)
               AND NOT EXISTS (
                   SELECT 1 FROM social_message_clears c
                   WHERE c.user_id = $1
                     AND c.partner_id = CASE
                         WHEN m.sender_id = $1 THEN m.recipient_id
                         ELSE m.sender_id
                     END
                     AND m.id <= c.cleared_through_id
               )
         ),
         last AS (
             SELECT DISTINCT ON (partner_id)
                    partner_id, id, sender_id, recipient_id, content, created_at, is_read
             FROM visible
             ORDER BY partner_id, created_at DESC, id DESC
         )
         SELECT u.id AS user_id, u.name, u.avatar_url,
                l.id, l.sender_id, l.recipient_id, l.content,
                l.created_at, l.is_read,
                s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                r.name AS recipient_name, r.avatar_url AS recipient_avatar_url,
                COALESCE(p.pinned, false) AS pinned,
                COALESCE(p.muted, false) AS muted,
                COALESCE(p.archived, false) AS archived,
                (SELECT COUNT(*) FROM forum_messages um
                 WHERE um.sender_id = u.id AND um.recipient_id = $1 AND um.is_read = false
                   AND NOT EXISTS (
                       SELECT 1 FROM social_message_clears c
                       WHERE c.user_id = $1 AND c.partner_id = u.id
                         AND um.id <= c.cleared_through_id
                   ))::BIGINT AS unread
         FROM last l
         JOIN users u ON u.id = l.partner_id
         JOIN users s ON s.id = l.sender_id
         JOIN users r ON r.id = l.recipient_id
         LEFT JOIN social_conversation_preferences p
                ON p.user_id = $1 AND p.partner_id = u.id
         ORDER BY COALESCE(p.pinned, false) DESC, l.created_at DESC, u.id DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(viewer.id)
    .bind(limit + 1)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let targets: Vec<i32> = rows.iter().map(|row| row.get("user_id")).collect();
    let context = super::RelationshipContext::load(&pool, viewer.id, &targets)
        .await
        .map_err(SocialError::internal)?;

    let items = rows
        .iter()
        .map(|row| {
            let partner_id: i32 = row.get("user_id");
            let (relationship, request_id) = context.relationship(partner_id);
            let user = super::model::SocialUser::new(
                forum::forum_user(partner_id, row.get("name"), row.get("avatar_url")),
                relationship,
                request_id,
                context.can_message(viewer.id, partner_id),
            );
            ConversationSummary {
                user,
                last_message: Some(row_to_workspace_message(row)),
                unread: row.get("unread"),
                preferences: ConversationPreferences {
                    pinned: row.get("pinned"),
                    muted: row.get("muted"),
                    archived: row.get("archived"),
                },
            }
        })
        .collect();

    let next_cursor = has_more.then(|| encode_offset_cursor(offset + limit));
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn list_history(
    State(pool): State<Pool<Postgres>>,
    Path(partner_id): Path<i64>,
    Query(query): Query<ConversationsQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<WorkspaceMessage>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let partner_id = positive_id(partner_id, "partnerId")?;
    if partner_id == viewer.id {
        return Err(SocialError::validation("partnerId", "不能与自己建立会话"));
    }
    let cursor = decode_time_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    let mut rows = sqlx::query(
        "SELECT m.id, m.sender_id, m.recipient_id, m.content, m.created_at, m.is_read,
                m.client_message_id,
                s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                r.name AS recipient_name, r.avatar_url AS recipient_avatar_url
         FROM forum_messages m
         JOIN users s ON s.id = m.sender_id
         JOIN users r ON r.id = m.recipient_id
         WHERE ((m.sender_id = $1 AND m.recipient_id = $2)
             OR (m.sender_id = $2 AND m.recipient_id = $1))
           AND NOT EXISTS (
               SELECT 1 FROM social_message_clears c
               WHERE c.user_id = $1 AND c.partner_id = $2
                 AND m.id <= c.cleared_through_id
           )
           AND ($3::timestamptz IS NULL OR (m.created_at, m.id) < ($3::timestamptz, $4::int))
         ORDER BY m.created_at DESC, m.id DESC
         LIMIT $5",
    )
    .bind(viewer.id)
    .bind(partner_id)
    .bind(cursor.map(|value| value.at))
    .bind(cursor.map(|value| value.id).unwrap_or(0))
    .bind(limit + 1)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let items = rows.iter().map(row_to_workspace_message).collect();
    let next_cursor = has_more.then(|| {
        let last = rows
            .last()
            .expect("rows is non-empty when has_more is true");
        encode_time_cursor(last.get("created_at"), i64::from(last.get::<i32, _>("id")))
    });
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn send_message(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(partner_id): Path<i64>,
    headers: HeaderMap,
    Json(payload): Json<SendWorkspaceMessagePayload>,
) -> Result<Json<WorkspaceMessage>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let partner_id = positive_id(partner_id, "partnerId")?;
    if partner_id == viewer.id {
        return Err(SocialError::validation("partnerId", "不能给自己发送私信"));
    }
    if let Some(recipient_id) = payload.recipient_id {
        let recipient_id = positive_id(recipient_id, "recipientId")?;
        if recipient_id != partner_id {
            return Err(SocialError::validation(
                "recipientId",
                "recipientId 与会话对象不一致",
            ));
        }
    }
    let content = payload.content.unwrap_or_default();
    let content = content.trim();
    if content.is_empty() || content.chars().count() > MAX_MESSAGE_CHARS {
        return Err(SocialError::validation(
            "content",
            format!("消息内容需为 1–{MAX_MESSAGE_CHARS} 个字符"),
        ));
    }
    let client_message_id = match payload.client_message_id.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            Some(Uuid::parse_str(value.trim()).map_err(|_| {
                SocialError::validation("clientMessageId", "clientMessageId 必须是 UUID")
            })?)
        }
        _ => None,
    };

    if load_forum_user(&pool, partner_id)
        .await
        .map_err(SocialError::internal)?
        .is_none()
    {
        return Err(SocialError::user_not_found());
    }

    // Idempotency is checked before permissions so a retry after a successful
    // send still returns the original message even if the recipient has since
    // changed their policy.
    if let Some(client_id) = client_message_id {
        let existing = sqlx::query(
            "SELECT m.id, m.sender_id, m.recipient_id, m.content, m.created_at, m.is_read,
                    m.client_message_id,
                    s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                    r.name AS recipient_name, r.avatar_url AS recipient_avatar_url
             FROM forum_messages m
             JOIN users s ON s.id = m.sender_id
             JOIN users r ON r.id = m.recipient_id
             WHERE m.sender_id = $1 AND m.client_message_id = $2 AND m.recipient_id = $3",
        )
        .bind(viewer.id)
        .bind(client_id)
        .bind(partner_id)
        .fetch_optional(&pool)
        .await
        .map_err(SocialError::internal)?;
        if let Some(row) = existing {
            if row.get::<String, _>("content") != content {
                return Err(SocialError::message_conflict(
                    "同一 clientMessageId 已用于不同内容",
                ));
            }
            return Ok(Json(row_to_workspace_message(&row)));
        }
    }

    let permission = message_permission(&pool, viewer.id, partner_id)
        .await
        .map_err(SocialError::internal)?;
    if permission != MessagePermission::Allowed {
        return Err(permission_error(permission));
    }

    let inserted = sqlx::query(
        "INSERT INTO forum_messages (sender_id, recipient_id, content, client_message_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (sender_id, client_message_id) WHERE client_message_id IS NOT NULL
         DO NOTHING
         RETURNING id",
    )
    .bind(viewer.id)
    .bind(partner_id)
    .bind(content)
    .bind(client_message_id)
    .fetch_optional(&pool)
    .await
    .map_err(SocialError::internal)?;

    let message_id = match inserted {
        Some(row) => row.get::<i32, _>("id"),
        None => {
            // A concurrent retry inserted first. Return that row after checking
            // that the content matches.
            let client_id = client_message_id
                .ok_or_else(|| SocialError::message_conflict("消息写入冲突，请重试"))?;
            let row = sqlx::query(
                "SELECT m.id, m.sender_id, m.recipient_id, m.content, m.created_at, m.is_read,
                        m.client_message_id,
                        s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                        r.name AS recipient_name, r.avatar_url AS recipient_avatar_url
                 FROM forum_messages m
                 JOIN users s ON s.id = m.sender_id
                 JOIN users r ON r.id = m.recipient_id
                 WHERE m.sender_id = $1 AND m.client_message_id = $2 AND m.recipient_id = $3",
            )
            .bind(viewer.id)
            .bind(client_id)
            .bind(partner_id)
            .fetch_optional(&pool)
            .await
            .map_err(SocialError::internal)?
            .ok_or_else(|| SocialError::message_conflict("同一 clientMessageId 已用于其他会话"))?;
            if row.get::<String, _>("content") != content {
                return Err(SocialError::message_conflict(
                    "同一 clientMessageId 已用于不同内容",
                ));
            }
            row.get("id")
        }
    };

    let row = sqlx::query(
        "SELECT m.id, m.sender_id, m.recipient_id, m.content, m.created_at, m.is_read,
                m.client_message_id,
                s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                r.name AS recipient_name, r.avatar_url AS recipient_avatar_url
         FROM forum_messages m
         JOIN users s ON s.id = m.sender_id
         JOIN users r ON r.id = m.recipient_id
         WHERE m.id = $1",
    )
    .bind(message_id)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;
    let message = row_to_workspace_message(&row);

    let sender_name: String = row.get("sender_name");
    let preview: String = content.chars().take(60).collect();
    let ws_message = serde_json::json!({
        "type": "new_message",
        "message_id": message.id,
        "sender_id": viewer.id,
        "sender_name": sender_name,
        "content_preview": preview,
    });
    ws_state.send_to_user(partner_id, &ws_message.to_string());
    for (user_id, actor_id) in [(partner_id, viewer.id), (viewer.id, partner_id)] {
        let event = serde_json::json!({
            "type": "conversation_changed",
            "userId": actor_id,
        });
        ws_state.send_to_user(user_id, &event.to_string());
    }

    Ok(Json(message))
}

pub(crate) async fn update_conversation_preferences(
    State(pool): State<Pool<Postgres>>,
    Path(partner_id): Path<i64>,
    headers: HeaderMap,
    Json(payload): Json<ConversationPreferencesPatch>,
) -> Result<Json<ConversationPreferences>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let partner_id = positive_id(partner_id, "partnerId")?;
    if partner_id == viewer.id {
        return Err(SocialError::validation("partnerId", "会话对象不能是自己"));
    }
    if load_forum_user(&pool, partner_id)
        .await
        .map_err(SocialError::internal)?
        .is_none()
    {
        return Err(SocialError::user_not_found());
    }

    let row = sqlx::query(
        "INSERT INTO social_conversation_preferences
             (user_id, partner_id, pinned, muted, archived)
         VALUES ($1, $2, COALESCE($3, false), COALESCE($4, false), COALESCE($5, false))
         ON CONFLICT (user_id, partner_id) DO UPDATE
         SET pinned = COALESCE($3, social_conversation_preferences.pinned),
             muted = COALESCE($4, social_conversation_preferences.muted),
             archived = COALESCE($5, social_conversation_preferences.archived),
             updated_at = NOW()
         RETURNING pinned, muted, archived",
    )
    .bind(viewer.id)
    .bind(partner_id)
    .bind(payload.pinned)
    .bind(payload.muted)
    .bind(payload.archived)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;

    Ok(Json(ConversationPreferences {
        pinned: row.get("pinned"),
        muted: row.get("muted"),
        archived: row.get("archived"),
    }))
}

pub(crate) async fn mark_read(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(partner_id): Path<i64>,
    headers: HeaderMap,
    Json(payload): Json<ReadConversationPayload>,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let partner_id = positive_id(partner_id, "partnerId")?;
    if partner_id == viewer.id {
        return Err(SocialError::validation("partnerId", "会话对象不能是自己"));
    }
    let last_read_message_id = validate_positive_i64(
        payload.last_read_message_id.unwrap_or(0),
        "lastReadMessageId",
    )?;

    sqlx::query(
        "UPDATE forum_messages SET is_read = true
         WHERE recipient_id = $1 AND sender_id = $2
           AND id <= $3 AND is_read = false",
    )
    .bind(viewer.id)
    .bind(partner_id)
    .bind(last_read_message_id)
    .execute(&pool)
    .await
    .map_err(SocialError::internal)?;

    let event = serde_json::json!({
        "type": "conversation_read",
        "userId": viewer.id,
        "lastReadMessageId": last_read_message_id,
    });
    ws_state.send_to_user(partner_id, &event.to_string());
    ws_state.send_to_user(viewer.id, &event.to_string());
    Ok(StatusCode::NO_CONTENT)
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
