//! Friends, requests, blocks, preferences and presence handlers.

use super::{
    RelationshipContext, SocialError, decode_offset_cursor, decode_time_cursor,
    encode_offset_cursor, encode_time_cursor, friend_request_from_row, load_forum_user, lock_pair,
    notify, now_iso, page_limit, positive_id, require_viewer, validate_optional_message,
    validate_positive_i64, validate_query,
};
use crate::{forum, ws};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{Pool, Postgres, Row};
use std::collections::HashSet;

use super::model::{
    CreateFriendRequestPayload, FriendRequest, FriendRequestPolicy, Friendship, MessagePolicy,
    Page, PreferencesPatch, PresenceEntry, PresenceSnapshot, Relationship, SocialPreferences,
    SocialUser,
};

const MAX_FRIEND_REQUEST_MESSAGE_CHARS: usize = 200;

#[derive(Debug, Deserialize)]
pub(crate) struct SearchUsersQuery {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FriendsQuery {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RequestsQuery {
    direction: Option<String>,
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresenceQuery {
    user_ids: Option<String>,
}

pub(crate) async fn search_users(
    State(pool): State<Pool<Postgres>>,
    Query(query): Query<SearchUsersQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<SocialUser>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let search_term = validate_query(query.q.as_deref())?;
    let limit = page_limit(query.limit);
    let offset = decode_offset_cursor(query.cursor.as_deref())?;

    if search_term.is_empty() {
        return Ok(Json(Page::new(Vec::new(), None)));
    }

    let normalized = search_term.to_lowercase();
    let escaped = forum::escape_like_pattern(&normalized);
    let contains = format!("%{escaped}%");
    let prefix = format!("{escaped}%");

    let mut rows = sqlx::query(
        "SELECT id, name, avatar_url FROM users
         WHERE id <> $1 AND LOWER(name) LIKE $2 ESCAPE '\\'
         ORDER BY
             CASE WHEN LOWER(name) LIKE $3 ESCAPE '\\' THEN 0 ELSE 1 END,
             POSITION($4 IN LOWER(name)),
             LOWER(name),
             id
         LIMIT $5 OFFSET $6",
    )
    .bind(viewer.id)
    .bind(&contains)
    .bind(&prefix)
    .bind(&normalized)
    .bind(limit + 1)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    let targets: Vec<i32> = rows.iter().map(|row| row.get("id")).collect();
    let context = RelationshipContext::load(&pool, viewer.id, &targets)
        .await
        .map_err(SocialError::internal)?;

    let items = rows
        .iter()
        .map(|row| {
            let id: i32 = row.get("id");
            let (relationship, request_id) = context.relationship(id);
            SocialUser::new(
                forum::forum_user(id, row.get("name"), row.get("avatar_url")),
                relationship,
                request_id,
                context.can_message(viewer.id, id),
            )
        })
        .collect();

    let next_cursor = has_more.then(|| encode_offset_cursor(offset + limit));
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn list_friends(
    State(pool): State<Pool<Postgres>>,
    Query(query): Query<FriendsQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<Friendship>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let search_term = validate_query(query.q.as_deref())?;
    let cursor = decode_time_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    let name_pattern = if search_term.is_empty() {
        "%".to_string()
    } else {
        format!(
            "%{}%",
            forum::escape_like_pattern(&search_term.to_lowercase())
        )
    };

    let mut rows = sqlx::query(
        "SELECT f.created_at, u.id, u.name, u.avatar_url,
                (NOT EXISTS (
                    SELECT 1 FROM social_blocks b
                    WHERE (b.user_id = $1 AND b.blocked_id = u.id)
                       OR (b.user_id = u.id AND b.blocked_id = $1)
                 )
                 AND COALESCE(p.message_policy, 'everyone') <> 'nobody') AS can_message
         FROM social_friendships f
         JOIN users u ON u.id = CASE WHEN f.user_low = $1 THEN f.user_high ELSE f.user_low END
         LEFT JOIN social_preferences p ON p.user_id = u.id
         WHERE (f.user_low = $1 OR f.user_high = $1)
           AND ($2 = '%' OR LOWER(u.name) LIKE $2 ESCAPE '\\')
           AND ($3::timestamptz IS NULL OR (f.created_at, u.id) < ($3::timestamptz, $4::int))
         ORDER BY f.created_at DESC, u.id DESC
         LIMIT $5",
    )
    .bind(viewer.id)
    .bind(&name_pattern)
    .bind(cursor.map(|value| value.at))
    .bind(cursor.map(|value| value.id).unwrap_or(0))
    .bind(limit + 1)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    let items: Vec<Friendship> = rows
        .iter()
        .map(|row| {
            let id: i32 = row.get("id");
            let created_at: DateTime<Utc> = row.get("created_at");
            Friendship {
                user: SocialUser::new(
                    forum::forum_user(id, row.get("name"), row.get("avatar_url")),
                    Relationship::Friends,
                    None,
                    row.get("can_message"),
                ),
                created_at: created_at.to_rfc3339(),
            }
        })
        .collect();

    let next_cursor = has_more.then(|| {
        let last = rows
            .last()
            .expect("rows is non-empty when has_more is true");
        encode_time_cursor(last.get("created_at"), last.get("id"))
    });
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn list_requests(
    State(pool): State<Pool<Postgres>>,
    Query(query): Query<RequestsQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<FriendRequest>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let direction = query.direction.as_deref().unwrap_or("incoming");
    if !matches!(direction, "incoming" | "outgoing") {
        return Err(SocialError::validation(
            "direction",
            "direction 必须是 incoming 或 outgoing",
        ));
    }
    let status = query.status.as_deref().unwrap_or("pending");
    if !matches!(status, "pending" | "accepted" | "rejected" | "cancelled") {
        return Err(SocialError::validation("status", "status 取值不合法"));
    }
    let cursor = decode_time_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    let mut rows = sqlx::query(
        "SELECT r.id, r.message, r.status, r.created_at, r.updated_at,
                s.id AS sender_id, s.name AS sender_name, s.avatar_url AS sender_avatar_url,
                t.id AS recipient_id, t.name AS recipient_name, t.avatar_url AS recipient_avatar_url
         FROM social_friend_requests r
         JOIN users s ON s.id = r.sender_id
         JOIN users t ON t.id = r.recipient_id
         WHERE r.status = $1
           AND (($2 = 'incoming' AND r.recipient_id = $3)
             OR ($2 = 'outgoing' AND r.sender_id = $3))
           AND ($4::timestamptz IS NULL OR (r.created_at, r.id) < ($4::timestamptz, $5::bigint))
         ORDER BY r.created_at DESC, r.id DESC
         LIMIT $6",
    )
    .bind(status)
    .bind(direction)
    .bind(viewer.id)
    .bind(cursor.map(|value| value.at))
    .bind(cursor.map(|value| value.id).unwrap_or(0))
    .bind(limit + 1)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    let items: Vec<FriendRequest> = rows.iter().map(friend_request_from_row).collect();
    let next_cursor = has_more.then(|| {
        let last = rows
            .last()
            .expect("rows is non-empty when has_more is true");
        encode_time_cursor(last.get("created_at"), last.get("id"))
    });
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn create_request(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    headers: HeaderMap,
    Json(payload): Json<CreateFriendRequestPayload>,
) -> Result<(StatusCode, Json<FriendRequest>), SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let recipient_id = positive_id(payload.recipient_id.unwrap_or(0), "recipientId")?;
    if recipient_id == viewer.id {
        return Err(SocialError::validation(
            "recipientId",
            "不能向自己发送好友申请",
        ));
    }
    let message =
        validate_optional_message(payload.message.as_deref(), MAX_FRIEND_REQUEST_MESSAGE_CHARS)?;

    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    lock_pair(&mut transaction, viewer.id, recipient_id)
        .await
        .map_err(SocialError::internal)?;

    let recipient_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(recipient_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(SocialError::internal)?;
    if !recipient_exists {
        return Err(SocialError::user_not_found());
    }

    let blocked: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM social_blocks
            WHERE (user_id = $1 AND blocked_id = $2)
               OR (user_id = $2 AND blocked_id = $1)
        )",
    )
    .bind(viewer.id)
    .bind(recipient_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    if blocked {
        return Err(SocialError::forbidden("当前关系不允许发送好友申请"));
    }

    let request_policy: Option<String> = sqlx::query_scalar(
        "SELECT friend_request_policy FROM social_preferences WHERE user_id = $1",
    )
    .bind(recipient_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    if request_policy
        .as_deref()
        .is_some_and(|policy| policy == "nobody")
    {
        return Err(SocialError::forbidden("对方当前不接收好友申请"));
    }

    let are_friends: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM social_friendships
            WHERE user_low = LEAST($1, $2) AND user_high = GREATEST($1, $2)
        )",
    )
    .bind(viewer.id)
    .bind(recipient_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    if are_friends {
        return Err(SocialError::relationship_changed());
    }

    let existing_pending: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM social_friend_requests
         WHERE sender_id = $1 AND recipient_id = $2 AND status = 'pending'",
    )
    .bind(viewer.id)
    .bind(recipient_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    if let Some(request_id) = existing_pending {
        transaction.commit().await.map_err(SocialError::internal)?;
        return Ok((
            StatusCode::OK,
            Json(super::current_friend_request_view(&pool, request_id).await?),
        ));
    }

    let reverse_pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM social_friend_requests
            WHERE sender_id = $1 AND recipient_id = $2 AND status = 'pending'
        )",
    )
    .bind(recipient_id)
    .bind(viewer.id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    if reverse_pending {
        return Err(SocialError::relationship_changed());
    }

    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO social_friend_requests (sender_id, recipient_id, message)
         VALUES ($1, $2, $3)
         ON CONFLICT (sender_id, recipient_id) WHERE status = 'pending' DO NOTHING
         RETURNING id",
    )
    .bind(viewer.id)
    .bind(recipient_id)
    .bind(&message)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    let request_id = match inserted {
        Some(id) => id,
        None => {
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM social_friend_requests
                 WHERE sender_id = $1 AND recipient_id = $2 AND status = 'pending'",
            )
            .bind(viewer.id)
            .bind(recipient_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(SocialError::internal)?;
            existing.ok_or_else(SocialError::relationship_changed)?
        }
    };

    transaction.commit().await.map_err(SocialError::internal)?;

    notify(
        &ws_state,
        recipient_id,
        "friend_request_received",
        Some(request_id),
        Some(viewer.id),
    );

    Ok((
        StatusCode::CREATED,
        Json(super::current_friend_request_view(&pool, request_id).await?),
    ))
}

/// Read the pair for a request and lock it before taking a row lock.
///
/// The order matters: blocking locks the pair first and then updates pending
/// requests, so accepting must never hold the request row while waiting for
/// the pair lock.
async fn lock_request_pair(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    request_id: i64,
) -> Result<(), SocialError> {
    let row =
        sqlx::query("SELECT sender_id, recipient_id FROM social_friend_requests WHERE id = $1")
            .bind(request_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(SocialError::internal)?
            .ok_or_else(SocialError::request_not_found)?;
    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");
    lock_pair(transaction, sender_id, recipient_id)
        .await
        .map_err(SocialError::internal)
}

pub(crate) async fn accept_request(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(request_id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<FriendRequest>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let request_id = validate_positive_i64(request_id, "id")?;
    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    lock_request_pair(&mut transaction, request_id).await?;

    let row = sqlx::query(
        "SELECT sender_id, recipient_id, status FROM social_friend_requests
         WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SocialError::internal)?
    .ok_or_else(SocialError::request_not_found)?;

    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");
    let status: String = row.get("status");
    if recipient_id != viewer.id {
        return Err(SocialError::forbidden("只有接收方可以接受好友申请"));
    }
    if status != "pending" {
        return Err(SocialError::request_not_pending());
    }

    sqlx::query(
        "INSERT INTO social_friendships (user_low, user_high)
         VALUES (LEAST($1, $2), GREATEST($1, $2))
         ON CONFLICT DO NOTHING",
    )
    .bind(viewer.id)
    .bind(sender_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    sqlx::query(
        "UPDATE social_friend_requests
         SET status = 'accepted', updated_at = NOW()
         WHERE id = $1",
    )
    .bind(request_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    sqlx::query(
        "UPDATE social_friend_requests
         SET status = 'cancelled', updated_at = NOW()
         WHERE status = 'pending' AND id <> $1
           AND ((sender_id = $2 AND recipient_id = $3)
             OR (sender_id = $3 AND recipient_id = $2))",
    )
    .bind(request_id)
    .bind(viewer.id)
    .bind(sender_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    transaction.commit().await.map_err(SocialError::internal)?;

    let view = super::current_friend_request_view(&pool, request_id).await?;
    for user_id in [sender_id, recipient_id] {
        notify(
            &ws_state,
            user_id,
            "friend_request_updated",
            Some(request_id),
            Some(viewer.id),
        );
        notify(
            &ws_state,
            user_id,
            "friendship_changed",
            None,
            Some(if user_id == sender_id {
                recipient_id
            } else {
                sender_id
            }),
        );
    }
    Ok(Json(view))
}

pub(crate) async fn reject_request(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(request_id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<FriendRequest>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let request_id = validate_positive_i64(request_id, "id")?;
    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    lock_request_pair(&mut transaction, request_id).await?;

    let row = sqlx::query(
        "SELECT sender_id, recipient_id, status FROM social_friend_requests
         WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SocialError::internal)?
    .ok_or_else(SocialError::request_not_found)?;

    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");
    let status: String = row.get("status");
    if recipient_id != viewer.id {
        return Err(SocialError::forbidden("只有接收方可以拒绝好友申请"));
    }
    if status != "pending" {
        return Err(SocialError::request_not_pending());
    }

    sqlx::query(
        "UPDATE social_friend_requests
         SET status = 'rejected', updated_at = NOW()
         WHERE id = $1",
    )
    .bind(request_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    transaction.commit().await.map_err(SocialError::internal)?;

    let view = super::current_friend_request_view(&pool, request_id).await?;
    for user_id in [sender_id, recipient_id] {
        notify(
            &ws_state,
            user_id,
            "friend_request_updated",
            Some(request_id),
            Some(viewer.id),
        );
    }
    Ok(Json(view))
}

pub(crate) async fn cancel_request(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(request_id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let request_id = validate_positive_i64(request_id, "id")?;
    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    lock_request_pair(&mut transaction, request_id).await?;

    let row = sqlx::query(
        "SELECT sender_id, recipient_id, status FROM social_friend_requests
         WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SocialError::internal)?
    .ok_or_else(SocialError::request_not_found)?;

    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");
    let status: String = row.get("status");
    if sender_id != viewer.id {
        return Err(SocialError::forbidden("只有发送方可以撤回好友申请"));
    }
    if status != "pending" {
        return Err(SocialError::request_not_pending());
    }

    sqlx::query(
        "UPDATE social_friend_requests
         SET status = 'cancelled', updated_at = NOW()
         WHERE id = $1",
    )
    .bind(request_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    transaction.commit().await.map_err(SocialError::internal)?;

    for user_id in [sender_id, recipient_id] {
        notify(
            &ws_state,
            user_id,
            "friend_request_updated",
            Some(request_id),
            Some(viewer.id),
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn remove_friend(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(friend_id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let friend_id = positive_id(friend_id, "userId")?;
    if friend_id == viewer.id {
        return Err(SocialError::validation("userId", "不能删除自己"));
    }

    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    lock_pair(&mut transaction, viewer.id, friend_id)
        .await
        .map_err(SocialError::internal)?;
    sqlx::query(
        "DELETE FROM social_friendships
         WHERE user_low = LEAST($1, $2) AND user_high = GREATEST($1, $2)",
    )
    .bind(viewer.id)
    .bind(friend_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    transaction.commit().await.map_err(SocialError::internal)?;

    notify(
        &ws_state,
        viewer.id,
        "friendship_changed",
        None,
        Some(friend_id),
    );
    notify(
        &ws_state,
        friend_id,
        "friendship_changed",
        None,
        Some(viewer.id),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn list_blocks(
    State(pool): State<Pool<Postgres>>,
    Query(query): Query<super::LimitQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<SocialUser>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let cursor = decode_time_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    let mut rows = sqlx::query(
        "SELECT b.created_at, u.id, u.name, u.avatar_url
         FROM social_blocks b
         JOIN users u ON u.id = b.blocked_id
         WHERE b.user_id = $1
           AND ($2::timestamptz IS NULL OR (b.created_at, u.id) < ($2::timestamptz, $3::int))
         ORDER BY b.created_at DESC, u.id DESC
         LIMIT $4",
    )
    .bind(viewer.id)
    .bind(cursor.map(|value| value.at))
    .bind(cursor.map(|value| value.id).unwrap_or(0))
    .bind(limit + 1)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let items = rows
        .iter()
        .map(|row| {
            let id: i32 = row.get("id");
            SocialUser::new(
                forum::forum_user(id, row.get("name"), row.get("avatar_url")),
                Relationship::Blocked,
                None,
                false,
            )
        })
        .collect();

    let next_cursor = has_more.then(|| {
        let last = rows
            .last()
            .expect("rows is non-empty when has_more is true");
        encode_time_cursor(last.get("created_at"), last.get("id"))
    });
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn block_user(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(user_id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let target_id = positive_id(user_id, "userId")?;
    if target_id == viewer.id {
        return Err(SocialError::validation("userId", "不能屏蔽自己"));
    }
    if load_forum_user(&pool, target_id)
        .await
        .map_err(SocialError::internal)?
        .is_none()
    {
        return Err(SocialError::user_not_found());
    }

    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    lock_pair(&mut transaction, viewer.id, target_id)
        .await
        .map_err(SocialError::internal)?;
    sqlx::query(
        "INSERT INTO social_blocks (user_id, blocked_id)
         VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(viewer.id)
    .bind(target_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    // Pending requests can never survive a block. Existing friendships are
    // retained so unblocking does not invent a new relationship.
    sqlx::query(
        "UPDATE social_friend_requests
         SET status = 'cancelled', updated_at = NOW()
         WHERE status = 'pending'
           AND ((sender_id = $1 AND recipient_id = $2)
             OR (sender_id = $2 AND recipient_id = $1))",
    )
    .bind(viewer.id)
    .bind(target_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;
    transaction.commit().await.map_err(SocialError::internal)?;

    notify(
        &ws_state,
        viewer.id,
        "friendship_changed",
        None,
        Some(target_id),
    );
    notify(
        &ws_state,
        target_id,
        "friendship_changed",
        None,
        Some(viewer.id),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn unblock_user(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(user_id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let target_id = positive_id(user_id, "userId")?;
    if target_id == viewer.id {
        return Err(SocialError::validation("userId", "不能解除对自己的屏蔽"));
    }

    sqlx::query("DELETE FROM social_blocks WHERE user_id = $1 AND blocked_id = $2")
        .bind(viewer.id)
        .bind(target_id)
        .execute(&pool)
        .await
        .map_err(SocialError::internal)?;

    notify(
        &ws_state,
        viewer.id,
        "friendship_changed",
        None,
        Some(target_id),
    );
    notify(
        &ws_state,
        target_id,
        "friendship_changed",
        None,
        Some(viewer.id),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_preferences(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<SocialPreferences>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let row = sqlx::query(
        "SELECT message_policy, friend_request_policy
         FROM social_preferences WHERE user_id = $1",
    )
    .bind(viewer.id)
    .fetch_optional(&pool)
    .await
    .map_err(SocialError::internal)?;

    let preferences = match row {
        Some(row) => {
            let message_policy: String = row.get("message_policy");
            let friend_request_policy: String = row.get("friend_request_policy");
            SocialPreferences {
                message_policy: MessagePolicy::from_db(&message_policy),
                friend_request_policy: FriendRequestPolicy::from_db(&friend_request_policy),
            }
        }
        None => SocialPreferences::default(),
    };
    Ok(Json(preferences))
}

pub(crate) async fn update_preferences(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
    Json(payload): Json<PreferencesPatch>,
) -> Result<Json<SocialPreferences>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;

    let message_policy = match payload.message_policy.as_deref() {
        None => None,
        Some(value @ ("everyone" | "friends" | "nobody")) => Some(value),
        Some(_) => {
            return Err(SocialError::validation(
                "messagePolicy",
                "messagePolicy 必须是 everyone、friends 或 nobody",
            ));
        }
    };
    let friend_request_policy = match payload.friend_request_policy.as_deref() {
        None => None,
        Some(value @ ("everyone" | "nobody")) => Some(value),
        Some(_) => {
            return Err(SocialError::validation(
                "friendRequestPolicy",
                "friendRequestPolicy 必须是 everyone 或 nobody",
            ));
        }
    };

    let row = sqlx::query(
        "INSERT INTO social_preferences (user_id, message_policy, friend_request_policy)
         VALUES ($1, COALESCE($2, 'everyone'), COALESCE($3, 'everyone'))
         ON CONFLICT (user_id) DO UPDATE
         SET message_policy = COALESCE($2, social_preferences.message_policy),
             friend_request_policy = COALESCE($3, social_preferences.friend_request_policy),
             updated_at = NOW()
         RETURNING message_policy, friend_request_policy",
    )
    .bind(viewer.id)
    .bind(message_policy)
    .bind(friend_request_policy)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;

    let message_policy: String = row.get("message_policy");
    let friend_request_policy: String = row.get("friend_request_policy");
    Ok(Json(SocialPreferences {
        message_policy: MessagePolicy::from_db(&message_policy),
        friend_request_policy: FriendRequestPolicy::from_db(&friend_request_policy),
    }))
}

pub(crate) async fn presence(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Query(query): Query<PresenceQuery>,
    headers: HeaderMap,
) -> Result<Json<PresenceSnapshot>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let requested: HashSet<i32> = query
        .user_ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .trim()
                .parse::<i64>()
                .ok()
                .and_then(|id| positive_id(id, "userIds").ok())
                .ok_or_else(|| SocialError::validation("userIds", "userIds 必须是逗号分隔的正整数"))
        })
        .collect::<Result<HashSet<_>, _>>()?;

    if requested.len() > 100 {
        return Err(SocialError::validation(
            "userIds",
            "一次最多查询 100 位用户",
        ));
    }

    let targets: Vec<i32> = requested.iter().copied().collect();
    let context = RelationshipContext::load(&pool, viewer.id, &targets)
        .await
        .map_err(SocialError::internal)?;

    let mut users = Vec::new();
    for user_id in targets {
        if user_id != viewer.id && !context.is_friend(user_id) {
            continue;
        }
        users.push(PresenceEntry {
            user_id,
            status: if ws_state.is_online(user_id) {
                "online"
            } else {
                "offline"
            },
        });
    }
    users.sort_by_key(|entry| entry.user_id);

    Ok(Json(PresenceSnapshot {
        users,
        generated_at: now_iso(),
    }))
}
