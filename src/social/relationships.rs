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
    CreateFriendRequestPayload, FriendGroup, FriendMetadata, FriendMetadataPatch, FriendRequest,
    FriendRequestPolicy, Friendship, GroupPayload, MessagePolicy, Page, PreferencesPatch,
    PresenceEntry, PresenceSnapshot, RecentPost, Relationship, SocialPreferences, SocialProfile,
    SocialSummary, SocialUser,
};

const MAX_FRIEND_REQUEST_MESSAGE_CHARS: usize = 200;

#[derive(Debug, Deserialize)]
pub(crate) struct SearchUsersQuery {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FriendsQuery {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    status: Option<String>,
    starred: Option<bool>,
    group_id: Option<String>,
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

pub(crate) async fn get_profile(
    State(pool): State<Pool<Postgres>>,
    Path(user_id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<SocialProfile>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let target_id = positive_id(user_id, "userId")?;
    let target = load_forum_user(&pool, target_id)
        .await
        .map_err(SocialError::internal)?
        .ok_or_else(SocialError::user_not_found)?;

    let context = RelationshipContext::load(&pool, viewer.id, &[target_id])
        .await
        .map_err(SocialError::internal)?;
    let (relationship, request_id) = if target_id == viewer.id {
        (Relationship::Current, None)
    } else {
        context.relationship(target_id)
    };
    let can_message = target_id != viewer.id && context.can_message(viewer.id, target_id);

    let rows = sqlx::query(
        "SELECT id, title FROM forum_posts
         WHERE author_id = $1
         ORDER BY created_at DESC, id DESC
         LIMIT 5",
    )
    .bind(target_id)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let recent_posts = rows
        .iter()
        .map(|row| RecentPost {
            id: row.get("id"),
            title: row.get("title"),
        })
        .collect();

    Ok(Json(SocialProfile {
        user: SocialUser::new(target, relationship, request_id, can_message),
        recent_posts,
    }))
}

/// Posts written by confirmed friends, newest first.
pub(crate) async fn friend_feed(
    State(pool): State<Pool<Postgres>>,
    Query(query): Query<super::LimitQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<forum::ForumPost>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let cursor = decode_time_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    let mut rows = sqlx::query(
        "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                p.created_at, p.updated_at, p.view_count::BIGINT AS view_count,
                p.is_pinned, p.is_locked,
                u.name AS author_name, u.avatar_url AS author_avatar_url,
                (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT AS comment_count,
                COUNT(l.user_id)::BIGINT AS like_count,
                COALESCE(BOOL_OR(l.user_id = $1), false) AS liked_by_me,
                (SELECT COUNT(*) FROM forum_reposts r WHERE r.post_id = p.id)::BIGINT AS repost_count,
                EXISTS(SELECT 1 FROM forum_reposts r WHERE r.post_id = p.id AND r.user_id = $1) AS reposted_by_me
         FROM forum_posts p
         JOIN users u ON u.id = p.author_id
         LEFT JOIN forum_post_likes l ON l.post_id = p.id
         WHERE p.author_id IN (
             SELECT CASE WHEN f.user_low = $1 THEN f.user_high ELSE f.user_low END
             FROM social_friendships f
             WHERE f.user_low = $1 OR f.user_high = $1
         )
           AND p.author_id NOT IN (
             SELECT CASE WHEN b.user_id = $1 THEN b.blocked_id ELSE b.user_id END
             FROM social_blocks b
             WHERE b.user_id = $1 OR b.blocked_id = $1
           )
           AND ($2::timestamptz IS NULL OR (p.created_at, p.id) < ($2::timestamptz, $3::int))
         GROUP BY p.id, u.name, u.avatar_url
         ORDER BY p.created_at DESC, p.id DESC
         LIMIT $4",
    )
    .bind(viewer.id)
    .bind(cursor.map(|value| value.at))
    .bind(i32::try_from(cursor.map(|value| value.id).unwrap_or(0)).unwrap_or(0))
    .bind(limit + 1)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let items = rows.iter().map(forum::row_to_post).collect();

    let next_cursor = has_more.then(|| {
        let last = rows
            .last()
            .expect("rows is non-empty when has_more is true");
        encode_time_cursor(last.get("created_at"), i64::from(last.get::<i32, _>("id")))
    });
    Ok(Json(Page::new(items, next_cursor)))
}

pub(crate) async fn list_friends(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Query(query): Query<FriendsQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<Friendship>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let search_term = validate_query(query.q.as_deref())?;
    let cursor = decode_offset_cursor(query.cursor.as_deref())?;
    let limit = page_limit(query.limit);

    if let Some(status) = query.status.as_deref()
        && status != "online"
    {
        return Err(SocialError::validation("status", "status 只支持 online"));
    }
    let online_only = query.status.as_deref() == Some("online");
    let online_ids: Option<Vec<i32>> = online_only.then(|| ws_state.online_user_ids());

    // `groupId` is deliberately a string: the frontend encodes "all groups" by
    // omitting it and "ungrouped" as `groupId=null`.
    let group_filter = match query.group_id.as_deref() {
        None => None,
        Some("null") | Some("") => Some(None),
        Some(value) => Some(Some(value.parse::<i64>().map_err(|_| {
            SocialError::validation("groupId", "groupId 必须是正整数或 null")
        })?)),
    };
    if let Some(Some(group_id)) = group_filter
        && group_id <= 0
    {
        return Err(SocialError::validation("groupId", "groupId 必须是正整数"));
    }

    let name_pattern = if search_term.is_empty() {
        "%".to_string()
    } else {
        format!(
            "%{}%",
            forum::escape_like_pattern(&search_term.to_lowercase())
        )
    };

    let rows = sqlx::query(
        "WITH friend_rows AS (
             SELECT CASE WHEN f.user_low = $1 THEN f.user_high ELSE f.user_low END AS friend_id,
                    f.created_at
             FROM social_friendships f
             WHERE f.user_low = $1 OR f.user_high = $1
         )
         SELECT fr.friend_id, fr.created_at, u.name, u.avatar_url,
                m.user_id IS NOT NULL AS has_metadata,
                COALESCE(m.note, '') AS note,
                COALESCE(m.starred, false) AS starred,
                m.group_id,
                EXISTS (
                    SELECT 1 FROM social_blocks b
                    WHERE b.user_id = $1 AND b.blocked_id = fr.friend_id
                ) AS blocked_by_me,
                (NOT EXISTS (
                    SELECT 1 FROM social_blocks b
                    WHERE (b.user_id = $1 AND b.blocked_id = fr.friend_id)
                       OR (b.user_id = fr.friend_id AND b.blocked_id = $1)
                 )
                 AND COALESCE(p.message_policy, 'everyone') <> 'nobody') AS can_message,
                COUNT(*) OVER() AS total
         FROM friend_rows fr
         JOIN users u ON u.id = fr.friend_id
         LEFT JOIN social_friend_metadata m
                ON m.user_id = $1 AND m.friend_id = fr.friend_id
         LEFT JOIN social_preferences p ON p.user_id = fr.friend_id
         WHERE ($2 = '%'
                OR LOWER(u.name) LIKE $2 ESCAPE '\\'
                OR LOWER(COALESCE(m.note, '')) LIKE $2 ESCAPE '\\')
           AND ($3::bool IS NULL OR COALESCE(m.starred, false) = $3)
           AND (NOT $4
                OR CASE WHEN $5::bigint IS NULL THEN m.group_id IS NULL ELSE m.group_id = $5 END)
           AND ($6::int[] IS NULL OR fr.friend_id = ANY($6))
         ORDER BY fr.created_at DESC, fr.friend_id DESC
         LIMIT $7 OFFSET $8",
    )
    .bind(viewer.id)
    .bind(&name_pattern)
    .bind(query.starred)
    .bind(group_filter.is_some())
    .bind(group_filter.flatten())
    .bind(online_ids)
    .bind(limit + 1)
    .bind(cursor)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;

    let has_more = rows.len() as i64 > limit;
    let total: Option<i64> = rows.first().map(|row| row.get("total"));
    let mut rows = rows;
    rows.truncate(limit as usize);

    let items: Vec<Friendship> = rows
        .iter()
        .map(|row| {
            let id: i32 = row.get("friend_id");
            let created_at: DateTime<Utc> = row.get("created_at");
            let relationship = if row.get::<bool, _>("blocked_by_me") {
                Relationship::Blocked
            } else {
                Relationship::Friends
            };
            let metadata = if row.get::<bool, _>("has_metadata") {
                Some(FriendMetadata {
                    note: row.get("note"),
                    starred: row.get("starred"),
                    group_id: row.get("group_id"),
                })
            } else {
                Some(FriendMetadata::default())
            };
            Friendship {
                user: SocialUser::new(
                    forum::forum_user(id, row.get("name"), row.get("avatar_url")),
                    relationship,
                    None,
                    row.get("can_message"),
                ),
                created_at: created_at.to_rfc3339(),
                metadata,
            }
        })
        .collect();

    let mut page = Page::new(
        items,
        has_more.then(|| encode_offset_cursor(cursor + limit)),
    );
    page.total = total;
    Ok(Json(page))
}

/// Aggregate counts for the friends sidebar. Online is a live snapshot, not a
/// stored flag; the frontend polls it only while a WebSocket is connected.
pub(crate) async fn summary(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    headers: HeaderMap,
) -> Result<Json<SocialSummary>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_friendships WHERE user_low = $1 OR user_high = $1",
    )
    .bind(viewer.id)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;

    let starred: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_friend_metadata m
         WHERE m.user_id = $1 AND m.starred = true
           AND EXISTS (
               SELECT 1 FROM social_friendships f
               WHERE f.user_low = LEAST($1, m.friend_id)
                 AND f.user_high = GREATEST($1, m.friend_id)
           )",
    )
    .bind(viewer.id)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;

    let incoming: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_friend_requests
         WHERE recipient_id = $1 AND status = 'pending'",
    )
    .bind(viewer.id)
    .fetch_one(&pool)
    .await
    .map_err(SocialError::internal)?;

    let friend_ids: Vec<i32> = sqlx::query_scalar(
        "SELECT CASE WHEN user_low = $1 THEN user_high ELSE user_low END
         FROM social_friendships WHERE user_low = $1 OR user_high = $1",
    )
    .bind(viewer.id)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;
    let online = friend_ids
        .into_iter()
        .filter(|id| ws_state.is_online(*id))
        .count() as i64;

    Ok(Json(SocialSummary {
        total,
        online,
        starred,
        incoming,
    }))
}

pub(crate) async fn get_friend_metadata(
    State(pool): State<Pool<Postgres>>,
    Path(friend_id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<FriendMetadata>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let friend_id = positive_id(friend_id, "userId")?;
    require_friendship(&pool, viewer.id, friend_id).await?;
    Ok(Json(
        load_friend_metadata(&pool, viewer.id, friend_id).await?,
    ))
}

pub(crate) async fn update_friend_metadata(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(friend_id): Path<i64>,
    headers: HeaderMap,
    Json(payload): Json<FriendMetadataPatch>,
) -> Result<Json<FriendMetadata>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let friend_id = positive_id(friend_id, "userId")?;
    require_friendship(&pool, viewer.id, friend_id).await?;

    let note = match payload.note.as_deref() {
        Some(value) => {
            let note = value.trim().to_string();
            if note.chars().count() > 60 {
                return Err(SocialError::validation("note", "备注不能超过 60 个字符"));
            }
            if note.chars().any(char::is_control) {
                return Err(SocialError::validation("note", "备注不能包含控制字符"));
            }
            Some(note)
        }
        None => None,
    };

    let current = load_friend_metadata(&pool, viewer.id, friend_id).await?;
    let next_note = note.unwrap_or(current.note);
    let next_starred = payload.starred.unwrap_or(current.starred);
    let next_group = match payload.group_id {
        None => current.group_id,
        Some(value) => value,
    };

    if let Some(group_id) = next_group {
        if group_id <= 0 {
            return Err(SocialError::validation("groupId", "分组标识不合法"));
        }
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM social_friend_groups WHERE id = $1 AND user_id = $2)",
        )
        .bind(group_id)
        .bind(viewer.id)
        .fetch_one(&pool)
        .await
        .map_err(SocialError::internal)?;
        if !owned {
            return Err(SocialError::validation("groupId", "分组不存在"));
        }
    }

    sqlx::query(
        "INSERT INTO social_friend_metadata (user_id, friend_id, note, starred, group_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (user_id, friend_id) DO UPDATE
         SET note = EXCLUDED.note,
             starred = EXCLUDED.starred,
             group_id = EXCLUDED.group_id,
             updated_at = NOW()",
    )
    .bind(viewer.id)
    .bind(friend_id)
    .bind(&next_note)
    .bind(next_starred)
    .bind(next_group)
    .execute(&pool)
    .await
    .map_err(SocialError::internal)?;

    notify(
        &ws_state,
        viewer.id,
        "friend_metadata_changed",
        None,
        Some(friend_id),
    );

    Ok(Json(FriendMetadata {
        note: next_note,
        starred: next_starred,
        group_id: next_group,
    }))
}

async fn require_friendship(
    pool: &Pool<Postgres>,
    viewer_id: i32,
    friend_id: i32,
) -> Result<(), SocialError> {
    if friend_id == viewer_id {
        return Err(SocialError::validation("userId", "不能对自己设置好友信息"));
    }
    let are_friends: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM social_friendships
             WHERE user_low = LEAST($1, $2) AND user_high = GREATEST($1, $2)
         )",
    )
    .bind(viewer_id)
    .bind(friend_id)
    .fetch_one(pool)
    .await
    .map_err(SocialError::internal)?;
    if !are_friends {
        return Err(SocialError::forbidden("只能为好友设置备注与分组"));
    }
    Ok(())
}

async fn load_friend_metadata(
    pool: &Pool<Postgres>,
    viewer_id: i32,
    friend_id: i32,
) -> Result<FriendMetadata, SocialError> {
    let row = sqlx::query(
        "SELECT note, starred, group_id FROM social_friend_metadata
         WHERE user_id = $1 AND friend_id = $2",
    )
    .bind(viewer_id)
    .bind(friend_id)
    .fetch_optional(pool)
    .await
    .map_err(SocialError::internal)?;
    Ok(row
        .map(|row| FriendMetadata {
            note: row.get("note"),
            starred: row.get("starred"),
            group_id: row.get("group_id"),
        })
        .unwrap_or_default())
}

pub(crate) async fn list_groups(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<Vec<FriendGroup>>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let rows = sqlx::query(
        "SELECT id, name FROM social_friend_groups
         WHERE user_id = $1
         ORDER BY LOWER(name), id",
    )
    .bind(viewer.id)
    .fetch_all(&pool)
    .await
    .map_err(SocialError::internal)?;
    Ok(Json(
        rows.iter()
            .map(|row| FriendGroup {
                id: row.get("id"),
                name: row.get("name"),
            })
            .collect(),
    ))
}

fn validate_group_name(value: Option<&str>) -> Result<String, SocialError> {
    let name = value.unwrap_or("").trim();
    if name.is_empty() || name.chars().count() > 30 {
        return Err(SocialError::validation("name", "分组名称需为 1–30 个字符"));
    }
    if name.chars().any(char::is_control) {
        return Err(SocialError::validation("name", "分组名称不能包含控制字符"));
    }
    Ok(name.to_string())
}

pub(crate) async fn create_group(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    headers: HeaderMap,
    Json(payload): Json<GroupPayload>,
) -> Result<(StatusCode, Json<FriendGroup>), SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let name = validate_group_name(payload.name.as_deref())?;

    let row = sqlx::query(
        "INSERT INTO social_friend_groups (user_id, name) VALUES ($1, $2)
         RETURNING id, name",
    )
    .bind(viewer.id)
    .bind(&name)
    .fetch_one(&pool)
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(database) if database.is_unique_violation() => {
            SocialError::conflict("分组名称已存在")
        }
        other => SocialError::internal(other),
    })?;

    notify(&ws_state, viewer.id, "friend_group_changed", None, None);
    Ok((
        StatusCode::CREATED,
        Json(FriendGroup {
            id: row.get("id"),
            name: row.get("name"),
        }),
    ))
}

pub(crate) async fn rename_group(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(group_id): Path<i64>,
    headers: HeaderMap,
    Json(payload): Json<GroupPayload>,
) -> Result<Json<FriendGroup>, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let group_id = validate_positive_i64(group_id, "id")?;
    let name = validate_group_name(payload.name.as_deref())?;

    let row = sqlx::query(
        "UPDATE social_friend_groups SET name = $1, updated_at = NOW()
         WHERE id = $2 AND user_id = $3
         RETURNING id, name",
    )
    .bind(&name)
    .bind(group_id)
    .bind(viewer.id)
    .fetch_optional(&pool)
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(database) if database.is_unique_violation() => {
            SocialError::conflict("分组名称已存在")
        }
        other => SocialError::internal(other),
    })?
    .ok_or_else(|| SocialError::validation("id", "分组不存在"))?;

    notify(&ws_state, viewer.id, "friend_group_changed", None, None);
    Ok(Json(FriendGroup {
        id: row.get("id"),
        name: row.get("name"),
    }))
}

pub(crate) async fn delete_group(
    State(pool): State<Pool<Postgres>>,
    Extension(ws_state): Extension<ws::WsState>,
    Path(group_id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, SocialError> {
    let viewer = require_viewer(&pool, &headers).await?;
    let group_id = validate_positive_i64(group_id, "id")?;

    let mut transaction = pool.begin().await.map_err(SocialError::internal)?;
    sqlx::query(
        "UPDATE social_friend_metadata SET group_id = NULL, updated_at = NOW()
         WHERE user_id = $1 AND group_id = $2",
    )
    .bind(viewer.id)
    .bind(group_id)
    .execute(&mut *transaction)
    .await
    .map_err(SocialError::internal)?;

    let deleted = sqlx::query("DELETE FROM social_friend_groups WHERE id = $1 AND user_id = $2")
        .bind(group_id)
        .bind(viewer.id)
        .execute(&mut *transaction)
        .await
        .map_err(SocialError::internal)?;
    transaction.commit().await.map_err(SocialError::internal)?;

    if deleted.rows_affected() == 0 {
        return Err(SocialError::validation("id", "分组不存在"));
    }

    notify(&ws_state, viewer.id, "friend_group_changed", None, None);
    Ok(StatusCode::NO_CONTENT)
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
