use crate::{auth, social};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, put},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row, postgres::PgRow};

const MAX_POST_TITLE_CHARS: usize = 200;
const MAX_POST_CONTENT_CHARS: usize = 50_000;
const MAX_COMMENT_CHARS: usize = 10_000;
const MAX_MESSAGE_CHARS: usize = 10_000;
const MAX_CATEGORY_CHARS: usize = 32;
const MAX_TAGS: usize = 10;
const MAX_TAG_CHARS: usize = 24;

/// Trim user text and enforce a hard character budget (not byte length, so
/// UTF-8 content is counted the same way the UI counts it).
fn validate_content(value: &str, max_chars: usize) -> Result<String, StatusCode> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max_chars {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForumUser {
    pub id: i32,
    pub name: String,
    pub avatar: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub is_admin: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForumPost {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub author: ForumUser,
    pub category: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub view_count: i64,
    pub comment_count: i64,
    pub like_count: i64,
    pub liked_by_me: bool,
    pub is_pinned: bool,
    pub is_locked: bool,
    /// Number of accounts that shared this post. A repost never duplicates the
    /// original row, so comments and likes stay attached to one post.
    pub repost_count: i64,
    /// Whether the current viewer has already reposted this post.
    pub reposted_by_me: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForumComment {
    pub id: i32,
    pub post_id: i32,
    /// Parent comment for a nested reply, or `null` for a top-level comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<i32>,
    /// Author of the parent comment, so the UI can render "回复 @name" even
    /// when the parent is outside the currently loaded page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ForumUser>,
    pub author: ForumUser,
    pub content: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForumMessage {
    pub id: i32,
    pub sender: ForumUser,
    pub recipient: ForumUser,
    pub content: String,
    pub created_at: String,
    pub is_read: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForumBootstrap {
    pub users: Vec<ForumUser>,
    pub posts: Vec<ForumPost>,
    pub comments: Vec<ForumComment>,
    pub messages: Vec<ForumMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePostRequest {
    pub title: String,
    pub content: String,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct UpdatePostRequest {
    pub content: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCommentRequest {
    pub content: String,
    /// When present, create the comment as a reply under this comment.
    pub parent_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub recipient_id: i32,
    pub content: String,
}

#[derive(Deserialize)]
pub struct PostsQuery {
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

struct LikeSearch {
    query: String,
    contains_pattern: String,
    prefix_pattern: String,
}

impl LikeSearch {
    fn new(query: String) -> Self {
        let escaped_query = escape_like_pattern(&query);
        Self {
            query,
            contains_pattern: format!("%{}%", escaped_query),
            prefix_pattern: format!("{}%", escaped_query),
        }
    }
}

pub(crate) fn escape_like_pattern(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for ch in query.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

///初始化未定义头像
fn avatar_for_user(id: i32, name: &str) -> String {
    match id {
        1 => return "CD".to_string(),
        2 => return "EH".to_string(),
        _ => {}
    }

    let mut initials: String = name
        .chars()
        .filter(|ch| ch.is_ascii_uppercase())
        .take(2)
        .collect();

    if initials.is_empty() {
        initials = name.chars().take(2).collect::<String>().to_uppercase();
    } else if initials.len() == 1
        && let Some(next) = name.chars().find(|ch| ch.is_ascii_lowercase())
    {
        initials.push(next.to_ascii_uppercase());
    }

    initials
}

pub(crate) fn forum_user(id: i32, name: String, avatar_url: Option<String>) -> ForumUser {
    ForumUser {
        id,
        avatar: avatar_for_user(id, &name),
        name,
        avatar_url,
        is_admin: auth::is_admin_user(id),
    }
}

fn optional_user_id(headers: &HeaderMap) -> Option<i32> {
    auth::user_id_from_headers(headers)
}

fn require_user_id(headers: &HeaderMap) -> Result<i32, StatusCode> {
    optional_user_id(headers).ok_or(StatusCode::UNAUTHORIZED)
}

async fn get_current_user(pool: &Pool<Postgres>, user_id: i32) -> Result<ForumUser, StatusCode> {
    let row = sqlx::query("SELECT id, name, avatar_url FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    row.map(|r| forum_user(r.get("id"), r.get("name"), r.get("avatar_url")))
        .ok_or(StatusCode::UNAUTHORIZED)
}

async fn require_current_user(
    pool: &Pool<Postgres>,
    headers: &HeaderMap,
) -> Result<ForumUser, StatusCode> {
    let user_id = require_user_id(headers)?;
    get_current_user(pool, user_id).await
}

fn require_admin(user: &ForumUser) -> Result<(), StatusCode> {
    if user.is_admin {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn require_post_moderator(
    pool: &Pool<Postgres>,
    post_id: i32,
    user: &ForumUser,
) -> Result<(), StatusCode> {
    if user.is_admin {
        return Ok(());
    }

    let author_id = sqlx::query_scalar::<_, i32>("SELECT author_id FROM forum_posts WHERE id = $1")
        .bind(post_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if author_id == user.id {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn require_comment_moderator(
    pool: &Pool<Postgres>,
    comment_id: i32,
    user: &ForumUser,
) -> Result<(), StatusCode> {
    if user.is_admin {
        return Ok(());
    }

    let author_id =
        sqlx::query_scalar::<_, i32>("SELECT author_id FROM forum_comments WHERE id = $1")
            .bind(comment_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;

    if author_id == user.id {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub(crate) fn row_to_post(row: &PgRow) -> ForumPost {
    let author_id: i32 = row.get("author_id");
    let author_name: String = row.get("author_name");
    let author_avatar_url: Option<String> = row.get("author_avatar_url");

    ForumPost {
        id: row.get("id"),
        title: row.get("title"),
        content: row.get("content"),
        author: forum_user(author_id, author_name, author_avatar_url),
        category: row.get("category"),
        tags: row.get::<Vec<String>, _>("tags"),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
        updated_at: row.get::<DateTime<Utc>, _>("updated_at").to_rfc3339(),
        view_count: row.get("view_count"),
        comment_count: row.get("comment_count"),
        like_count: row.get("like_count"),
        liked_by_me: row.get("liked_by_me"),
        is_pinned: row.get("is_pinned"),
        is_locked: row.get("is_locked"),
        // Older queries (for example the bootstrap query) predate reposts.
        // Missing columns intentionally fall back to zero rather than failing
        // the whole feed.
        repost_count: row.try_get::<i64, _>("repost_count").unwrap_or(0),
        reposted_by_me: row.try_get::<bool, _>("reposted_by_me").unwrap_or(false),
    }
}

fn row_to_message(row: &PgRow) -> ForumMessage {
    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");

    ForumMessage {
        id: row.get("id"),
        sender: forum_user(
            sender_id,
            row.get("sender_name"),
            row.get("sender_avatar_url"),
        ),
        recipient: forum_user(
            recipient_id,
            row.get("recipient_name"),
            row.get("recipient_avatar_url"),
        ),
        content: row.get("content"),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
        is_read: row.get("is_read"),
    }
}

fn row_to_comment(row: &PgRow) -> ForumComment {
    let author_id: i32 = row.get("author_id");
    let reply_to = row
        .try_get::<Option<i32>, _>("reply_author_id")
        .ok()
        .flatten()
        .map(|id| {
            forum_user(
                id,
                row.try_get::<Option<String>, _>("reply_author_name")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                row.try_get::<Option<String>, _>("reply_author_avatar_url")
                    .ok()
                    .flatten(),
            )
        });

    ForumComment {
        id: row.get("id"),
        post_id: row.get("post_id"),
        parent_id: row.try_get("parent_id").ok().flatten(),
        reply_to,
        author: forum_user(
            author_id,
            row.get("author_name"),
            row.get("author_avatar_url"),
        ),
        content: row.get("content"),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
    }
}

async fn fetch_users(pool: &Pool<Postgres>) -> Result<Vec<ForumUser>, StatusCode> {
    let rows = sqlx::query("SELECT id, name, avatar_url FROM users ORDER BY id ASC")
        .fetch_all(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows
        .iter()
        .map(|row| forum_user(row.get("id"), row.get("name"), row.get("avatar_url")))
        .collect())
}

///TODO:LEFT JOIN
async fn fetch_comments(pool: &Pool<Postgres>) -> Result<Vec<ForumComment>, StatusCode> {
    let rows = sqlx::query(
        "SELECT c.*, u.name as author_name, u.avatar_url as author_avatar_url,
                pc.author_id as reply_author_id,
                ru.name as reply_author_name,
                ru.avatar_url as reply_author_avatar_url
         FROM forum_comments c
         JOIN users u ON u.id = c.author_id
         LEFT JOIN forum_comments pc ON pc.id = c.parent_id
         LEFT JOIN users ru ON ru.id = pc.author_id
         ORDER BY c.created_at ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows.iter().map(row_to_comment).collect())
}

///SQL学习语句
async fn fetch_post_by_id(
    pool: &Pool<Postgres>,
    id: i32,
    viewer_user_id: Option<i32>,
) -> Result<ForumPost, StatusCode> {
    let row = sqlx::query(
        "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                p.is_pinned, p.is_locked, u.name as author_name, u.avatar_url as author_avatar_url,
                (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                COUNT(l.user_id)::BIGINT as like_count,
                COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me,
                (SELECT COUNT(*) FROM forum_reposts r WHERE r.post_id = p.id)::BIGINT as repost_count,
                EXISTS(SELECT 1 FROM forum_reposts r WHERE r.post_id = p.id AND r.user_id = $1) as reposted_by_me
         FROM forum_posts p
         JOIN users u ON u.id = p.author_id
         LEFT JOIN forum_post_likes l ON l.post_id = p.id
         WHERE p.id = $2
         GROUP BY p.id, u.name, u.avatar_url",
    )
    .bind(viewer_user_id)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    row.map(|row| row_to_post(&row))
        .ok_or(StatusCode::NOT_FOUND)
}

///TODO:分页
async fn fetch_messages_for_user(
    pool: &Pool<Postgres>,
    user_id: i32,
) -> Result<Vec<ForumMessage>, StatusCode> {
    let rows = sqlx::query(
        "SELECT m.id, m.sender_id, m.recipient_id, m.content, m.created_at, m.is_read,
                s.name as sender_name, s.avatar_url as sender_avatar_url,
                r.name as recipient_name, r.avatar_url as recipient_avatar_url
         FROM forum_messages m
         JOIN users s ON s.id = m.sender_id
         JOIN users r ON r.id = m.recipient_id
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
         ORDER BY m.created_at ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows.iter().map(row_to_message).collect())
}

///TODO:性能隐患,分页问题
async fn bootstrap(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<ForumBootstrap>, StatusCode> {
    let viewer_user_id = optional_user_id(&headers);
    let users = fetch_users(&pool).await?;
    let posts = fetch_posts(&pool, None, viewer_user_id).await?;
    let comments = fetch_comments(&pool).await?;
    let messages = match viewer_user_id {
        Some(user_id) => fetch_messages_for_user(&pool, user_id).await?,
        None => Vec::new(),
    };

    Ok(Json(ForumBootstrap {
        users,
        posts,
        comments,
        messages,
    }))
}

///为了用Json包装，又不想改原函数
async fn list_users(
    State(pool): State<Pool<Postgres>>,
) -> Result<Json<Vec<ForumUser>>, StatusCode> {
    Ok(Json(fetch_users(&pool).await?))
}

async fn fetch_posts(
    pool: &Pool<Postgres>,
    category: Option<&str>,
    viewer_user_id: Option<i32>,
) -> Result<Vec<ForumPost>, StatusCode> {
    let rows = if let Some(category) = category.filter(|value| !value.is_empty() && *value != "all") {
        sqlx::query(
            "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                    p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                    p.is_pinned, p.is_locked, u.name as author_name, u.avatar_url as author_avatar_url,
                    (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                    COUNT(l.user_id)::BIGINT as like_count,
                    COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me,
                    (SELECT COUNT(*) FROM forum_reposts r WHERE r.post_id = p.id)::BIGINT as repost_count,
                    EXISTS(SELECT 1 FROM forum_reposts r WHERE r.post_id = p.id AND r.user_id = $1) as reposted_by_me
             FROM forum_posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN forum_post_likes l ON l.post_id = p.id
             WHERE p.category = $2
             GROUP BY p.id, u.name, u.avatar_url
             ORDER BY p.is_pinned DESC, p.created_at DESC",
        )
        .bind(viewer_user_id)
        .bind(category)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query(
            "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                    p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                    p.is_pinned, p.is_locked, u.name as author_name, u.avatar_url as author_avatar_url,
                    (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                    COUNT(l.user_id)::BIGINT as like_count,
                    COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me,
                    (SELECT COUNT(*) FROM forum_reposts r WHERE r.post_id = p.id)::BIGINT as repost_count,
                    EXISTS(SELECT 1 FROM forum_reposts r WHERE r.post_id = p.id AND r.user_id = $1) as reposted_by_me
             FROM forum_posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN forum_post_likes l ON l.post_id = p.id
             GROUP BY p.id, u.name, u.avatar_url
             ORDER BY p.is_pinned DESC, p.created_at DESC",
        )
        .bind(viewer_user_id)
        .fetch_all(pool)
        .await
    }
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows.iter().map(row_to_post).collect())
}

///不知道何时访问
async fn list_posts(
    State(pool): State<Pool<Postgres>>,
    Query(q): Query<PostsQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<ForumPost>>, StatusCode> {
    Ok(Json(
        fetch_posts(&pool, q.category.as_deref(), optional_user_id(&headers)).await?,
    ))
}

async fn get_post(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<Json<ForumPost>, StatusCode> {
    sqlx::query("UPDATE forum_posts SET view_count = view_count + 1 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(
        fetch_post_by_id(&pool, id, optional_user_id(&headers)).await?,
    ))
}

async fn create_post(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
    Json(payload): Json<CreatePostRequest>,
) -> Result<Json<ForumPost>, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    let title = validate_content(&payload.title, MAX_POST_TITLE_CHARS)?;
    let content = validate_content(&payload.content, MAX_POST_CONTENT_CHARS)?;
    let category = payload
        .category
        .as_deref()
        .unwrap_or("general")
        .trim()
        .to_string();
    if category.is_empty() || category.chars().count() > MAX_CATEGORY_CHARS {
        return Err(StatusCode::BAD_REQUEST);
    }
    let tags: Vec<String> = payload
        .tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    if tags.len() > MAX_TAGS || tags.iter().any(|tag| tag.chars().count() > MAX_TAG_CHARS) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let row = sqlx::query(
        "INSERT INTO forum_posts (title, content, author_id, category, tags)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, created_at, updated_at",
    )
    .bind(&title)
    .bind(&content)
    .bind(user.id)
    .bind(&category)
    .bind(&tags)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let created_at: DateTime<Utc> = row.get("created_at");
    let updated_at: DateTime<Utc> = row.get("updated_at");

    Ok(Json(ForumPost {
        id: row.get("id"),
        title,
        content,
        author: user,
        category,
        tags,
        created_at: created_at.to_rfc3339(),
        updated_at: updated_at.to_rfc3339(),
        view_count: 0,
        comment_count: 0,
        like_count: 0,
        liked_by_me: false,
        is_pinned: false,
        is_locked: false,
        repost_count: 0,
        reposted_by_me: false,
    }))
}

async fn my_posts(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ForumPost>>, StatusCode> {
    let user_id = require_user_id(&headers)?;
    let rows = sqlx::query(
        "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                p.is_pinned, p.is_locked, u.name as author_name,
                u.avatar_url as author_avatar_url,
                (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                COUNT(l.user_id)::BIGINT as like_count,
                COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me,
                (SELECT COUNT(*) FROM forum_reposts r WHERE r.post_id = p.id)::BIGINT as repost_count,
                EXISTS(SELECT 1 FROM forum_reposts r WHERE r.post_id = p.id AND r.user_id = $1) as reposted_by_me
         FROM forum_posts p
         JOIN users u ON u.id = p.author_id
         LEFT JOIN forum_post_likes l ON l.post_id = p.id
         WHERE u.id = $1
         GROUP BY p.id, u.name, u.avatar_url
         ORDER BY p.is_pinned DESC, p.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows.iter().map(row_to_post).collect()))
}

async fn modify_post(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
    Json(payload): Json<UpdatePostRequest>,
) -> Result<StatusCode, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    require_post_moderator(&pool, id, &user).await?;
    let content = validate_content(&payload.content, MAX_POST_CONTENT_CHARS)?;

    let result = sqlx::query(
        "UPDATE forum_posts
SET content = $1,updated_at = NOW() 
WHERE id = $2;",
    )
    .bind(content)
    .bind(id)
    .execute(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::OK)
    }
}

async fn delete_post(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    require_post_moderator(&pool, id, &user).await?;

    let result = sqlx::query("DELETE FROM forum_posts WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

async fn toggle_pin(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<Json<ForumPost>, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    require_admin(&user)?;

    let result = sqlx::query("UPDATE forum_posts SET is_pinned = NOT is_pinned WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(fetch_post_by_id(&pool, id, None).await?))
}

async fn toggle_lock(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<Json<ForumPost>, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    require_admin(&user)?;

    let result = sqlx::query("UPDATE forum_posts SET is_locked = NOT is_locked WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    //Q:到底线程的访问情况是如何的
    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(fetch_post_by_id(&pool, id, None).await?))
}

///TODO:给帖子加点赞数字段
async fn toggle_like(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<Json<ForumPost>, StatusCode> {
    let user_id = require_user_id(&headers)?;
    //Q:处理拉黑？
    let _ = get_current_user(&pool, user_id).await?;

    let post_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM forum_posts WHERE id = $1)")
            .bind(id)
            .fetch_one(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !post_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    let liked: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM forum_post_likes WHERE post_id = $1 AND user_id = $2
        )",
    )
    .bind(id)
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if liked {
        sqlx::query("DELETE FROM forum_post_likes WHERE post_id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    } else {
        sqlx::query(
            "INSERT INTO forum_post_likes (post_id, user_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(user_id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    Ok(Json(fetch_post_by_id(&pool, id, Some(user_id)).await?))
}

/// Toggle the current account's repost of a post.
///
/// Reposts are intentionally idempotent: pressing again removes the share.
/// The original post row is never copied, so comments and likes stay in one
/// place and the count can never drift from the join table.
async fn toggle_repost(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<Json<ForumPost>, StatusCode> {
    let user_id = require_user_id(&headers)?;
    let post_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM forum_posts WHERE id = $1)")
            .bind(id)
            .fetch_one(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !post_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    let reposted: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM forum_reposts WHERE post_id = $1 AND user_id = $2)",
    )
    .bind(id)
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if reposted {
        sqlx::query("DELETE FROM forum_reposts WHERE post_id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    } else {
        sqlx::query(
            "INSERT INTO forum_reposts (post_id, user_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(user_id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    Ok(Json(fetch_post_by_id(&pool, id, Some(user_id)).await?))
}

/// Comments are returned flat, oldest first, with `parentId`/`replyTo` so the
/// client can render an arbitrarily nested tree without re-fetching parents.
async fn list_comments(
    State(pool): State<Pool<Postgres>>,
    Path(post_id): Path<i32>,
) -> Result<Json<Vec<ForumComment>>, StatusCode> {
    let rows = sqlx::query(
        "SELECT c.*, u.name as author_name, u.avatar_url as author_avatar_url,
                pc.author_id as reply_author_id,
                ru.name as reply_author_name,
                ru.avatar_url as reply_author_avatar_url
         FROM forum_comments c
         JOIN users u ON u.id = c.author_id
         LEFT JOIN forum_comments pc ON pc.id = c.parent_id
         LEFT JOIN users ru ON ru.id = pc.author_id
         WHERE c.post_id = $1
         ORDER BY c.created_at ASC, c.id ASC",
    )
    .bind(post_id)
    .fetch_all(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(rows.iter().map(row_to_comment).collect()))
}

/// Maximum nesting depth below a top-level comment. Six levels is enough for
/// a forum thread while keeping the rendered tree and ancestor checks cheap.
const MAX_COMMENT_DEPTH: i64 = 5;

async fn create_comment(
    State(pool): State<Pool<Postgres>>,
    Path(post_id): Path<i32>,
    headers: HeaderMap,
    Json(payload): Json<CreateCommentRequest>,
) -> Result<Json<ForumComment>, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    let content = validate_content(&payload.content, MAX_COMMENT_CHARS)?;

    let post = sqlx::query("SELECT is_locked FROM forum_posts WHERE id = $1")
        .bind(post_id)
        .fetch_optional(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match post {
        Some(r) => {
            let locked: bool = r.get("is_locked");
            if locked {
                return Err(StatusCode::FORBIDDEN);
            }
        }
        None => return Err(StatusCode::NOT_FOUND),
    }

    let parent_id = match payload.parent_id {
        None => None,
        Some(raw) => {
            let parent_id = i32::try_from(raw)
                .ok()
                .filter(|id| *id > 0)
                .ok_or(StatusCode::BAD_REQUEST)?;
            let parent =
                sqlx::query("SELECT id, post_id, author_id FROM forum_comments WHERE id = $1")
                    .bind(parent_id)
                    .fetch_optional(&pool)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                    .ok_or(StatusCode::NOT_FOUND)?;
            let parent_post_id: i32 = parent.get("post_id");
            if parent_post_id != post_id {
                return Err(StatusCode::BAD_REQUEST);
            }

            let depth: Option<i64> = sqlx::query_scalar(
                "WITH RECURSIVE ancestors AS (
                     SELECT id, parent_id, 1::BIGINT AS depth
                     FROM forum_comments WHERE id = $1
                     UNION ALL
                     SELECT c.id, c.parent_id, a.depth + 1
                     FROM forum_comments c
                     JOIN ancestors a ON c.id = a.parent_id
                 )
                 SELECT MAX(depth) FROM ancestors",
            )
            .bind(parent_id)
            .fetch_one(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if depth.unwrap_or(0) > MAX_COMMENT_DEPTH {
                return Err(StatusCode::BAD_REQUEST);
            }
            Some(parent_id)
        }
    };

    let row = sqlx::query(
        "INSERT INTO forum_comments (post_id, author_id, content, parent_id)
         VALUES ($1, $2, $3, $4)
         RETURNING id, created_at",
    )
    .bind(post_id)
    .bind(user.id)
    .bind(&content)
    .bind(parent_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let created_at: DateTime<Utc> = row.get("created_at");
    //创建帖子也要返回，因为创建完要立刻显示
    Ok(Json(ForumComment {
        id: row.get("id"),
        post_id,
        parent_id,
        reply_to: None,
        author: user,
        content,
        created_at: created_at.to_rfc3339(),
    }))
}

async fn delete_comment(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    require_comment_moderator(&pool, id, &user).await?;

    let result = sqlx::query("DELETE FROM forum_comments WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

async fn list_messages(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ForumMessage>>, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    Ok(Json(fetch_messages_for_user(&pool, user.id).await?))
}

// Axum responses are inherently larger than the success payload; boxing every
// error in this handler would add noise without a measurable benefit.
#[allow(clippy::result_large_err)]
async fn send_message(
    State(pool): State<Pool<Postgres>>,
    //不用Extension包装可能axum识别不出来，无法嵌入
    Extension(ws_state): Extension<crate::ws::WsState>,
    headers: HeaderMap,
    Json(payload): Json<SendMessageRequest>,
) -> Result<Json<ForumMessage>, Response> {
    let sender = require_current_user(&pool, &headers)
        .await
        .map_err(IntoResponse::into_response)?;
    if sender.id == payload.recipient_id {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let content = validate_content(&payload.content, MAX_MESSAGE_CHARS)
        .map_err(IntoResponse::into_response)?;
    let recipient = get_current_user(&pool, payload.recipient_id)
        .await
        .map_err(IntoResponse::into_response)?;

    match social::message_permission(&pool, sender.id, recipient.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
    {
        social::MessagePermission::Allowed => {}
        permission => return Err(social::permission_error(permission).into_response()),
    }

    let row = sqlx::query(
        "INSERT INTO forum_messages (sender_id, recipient_id, content)
         VALUES ($1, $2, $3)
         RETURNING id, created_at",
    )
    .bind(sender.id)
    .bind(payload.recipient_id)
    .bind(&content)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;

    let created_at: DateTime<Utc> = row.get("created_at");

    let message = ForumMessage {
        id: row.get("id"),
        sender: sender.clone(),
        recipient: recipient.clone(),
        content: content.clone(),
        created_at: created_at.to_rfc3339(),
        is_read: false,
    };

    // Push WebSocket notification to recipient
    let content_preview = message_preview(&content);
    let ws_msg = serde_json::json!({
        "type": "new_message",
        "message_id": message.id,
        "sender_id": sender.id,
        "sender_name": sender.name,
        "content_preview": content_preview,
    });
    ws_state.send_to_user(payload.recipient_id, &ws_msg.to_string());

    for (user_id, actor_id) in [(recipient.id, sender.id), (sender.id, recipient.id)] {
        let event = serde_json::json!({
            "type": "conversation_changed",
            "userId": actor_id,
        });
        ws_state.send_to_user(user_id, &event.to_string());
    }

    Ok(Json(message))
}

///Q:实际上显示的没有那么多
fn message_preview(content: &str) -> String {
    let mut chars = content.chars();
    let first_sixty: String = chars.by_ref().take(60).collect();

    if chars.next().is_none() {
        first_sixty
    } else {
        format!("{}...", first_sixty.chars().take(57).collect::<String>())
    }
}

async fn mark_message_read(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    let result = sqlx::query(
        "UPDATE forum_messages SET is_read = true
         WHERE id = $1 AND recipient_id = $2",
    )
    .bind(id)
    .bind(user.id)
    .execute(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

async fn mark_conversation_read(
    State(pool): State<Pool<Postgres>>,
    Path(partner_id): Path<i32>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let user = require_current_user(&pool, &headers).await?;
    let result = sqlx::query(
        "UPDATE forum_messages SET is_read = true
         WHERE sender_id = $1 AND recipient_id = $2 AND is_read = false",
    )
    .bind(partner_id)
    .bind(user.id)
    .execute(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

/// Q:搜索好像不是后端实现的，因为刚开始就一次性读取了全部帖子返回了
/// 感觉返回信息冗余了
async fn search_posts(
    State(pool): State<Pool<Postgres>>,
    Query(q): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<ForumPost>>, StatusCode> {
    let query = match q.q {
        Some(ref s) if !s.trim().is_empty() => s.trim().to_lowercase(),
        _ => return Ok(Json(vec![])),
    };

    let search = LikeSearch::new(query);
    let rows = sqlx::query(
        "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                p.is_pinned, p.is_locked, u.name as author_name, u.avatar_url as author_avatar_url,
                (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                COUNT(l.user_id)::BIGINT as like_count,
                COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me,
                (SELECT COUNT(*) FROM forum_reposts r WHERE r.post_id = p.id)::BIGINT as repost_count,
                EXISTS(SELECT 1 FROM forum_reposts r WHERE r.post_id = p.id AND r.user_id = $1) as reposted_by_me
         FROM forum_posts p
         JOIN users u ON u.id = p.author_id
         LEFT JOIN forum_post_likes l ON l.post_id = p.id
         WHERE LOWER(p.title) LIKE $2 ESCAPE '\\'
            OR LOWER(p.content) LIKE $2 ESCAPE '\\'
         GROUP BY p.id, u.name, u.avatar_url
         ORDER BY p.is_pinned DESC, p.created_at DESC",
    )
    .bind(optional_user_id(&headers))
    .bind(&search.contains_pattern)
    .fetch_all(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(rows.iter().map(row_to_post).collect()))
}

///TODO:UID唯一搜索
async fn search_users(
    State(pool): State<Pool<Postgres>>,
    //提取URL的?内容，类似PATH
    Query(q): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<ForumUser>>, StatusCode> {
    let _user = require_current_user(&pool, &headers).await?;
    let query = match q.q {
        Some(ref s) if !s.trim().is_empty() => s.trim().to_lowercase(),
        _ => return Ok(Json(vec![])),
    };

    let search = LikeSearch::new(query);
    let rows = sqlx::query(
        "SELECT id, name, avatar_url FROM users
         WHERE LOWER(name) LIKE $1 ESCAPE '\\'
         ORDER BY
            CASE WHEN LOWER(name) LIKE $2 ESCAPE '\\' THEN 0 ELSE 1 END,
            POSITION($3 IN LOWER(name)),
            LOWER(name) ASC,
            id ASC
         LIMIT 20",
    )
    .bind(&search.contains_pattern)
    .bind(&search.prefix_pattern)
    .bind(&search.query)
    .fetch_all(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(
        rows.iter()
            .map(|row| forum_user(row.get("id"), row.get("name"), row.get("avatar_url")))
            .collect(),
    ))
}

pub fn forum_routes() -> Router<Pool<Postgres>> {
    Router::new()
        .route("/api/forum/bootstrap", get(bootstrap))
        .route("/api/forum/users", get(list_users))
        .route("/api/forum/posts", get(list_posts).post(create_post))
        .route("/api/forum/posts/search", get(search_posts))
        .route("/api/forum/posts/{id}", get(get_post).delete(delete_post))
        .route("/api/forum/posts/{id}/pin", put(toggle_pin))
        .route("/api/forum/posts/{id}/lock", put(toggle_lock))
        .route("/api/forum/posts/{id}/like", put(toggle_like))
        .route("/api/forum/posts/{id}/repost", put(toggle_repost))
        .route(
            "/api/forum/posts/{id}/comments",
            get(list_comments).post(create_comment),
        )
        .route("/api/forum/comments/{id}", delete(delete_comment))
        //rest风格
        .route("/api/forum/messages", get(list_messages).post(send_message))
        .route("/api/forum/messages/{id}/read", put(mark_message_read))
        .route(
            "/api/forum/messages/conversation/{partner_id}/read",
            put(mark_conversation_read),
        )
        .route("/api/forum/users/search", get(search_users))
        .route("/api/forum/posts/myposts", get(my_posts))
        .route(
            "/api/forum/posts/myposts/modify_post/{id}",
            put(modify_post),
        )
}

#[cfg(test)]
mod tests {
    use super::{UpdatePostRequest, message_preview, validate_content};

    #[test]
    fn content_validation_trims_and_counts_characters() {
        assert_eq!(validate_content("  hello  ", 10).unwrap(), "hello");
        assert!(validate_content("   ", 10).is_err());
        assert!(validate_content(&"a".repeat(11), 10).is_err());
        assert!(validate_content(&"界".repeat(10), 10).is_ok());
    }

    #[test]
    fn message_preview_preserves_up_to_sixty_characters() {
        for value in [
            "a".repeat(60),
            '\u{754c}'.to_string().repeat(60),
            '\u{1f600}'.to_string().repeat(60),
        ] {
            assert_eq!(message_preview(&value), value);
        }
    }

    #[test]
    fn message_preview_truncates_by_unicode_characters() {
        for character in ['a', '\u{754c}', '\u{1f600}'] {
            let value = character.to_string().repeat(61);
            assert_eq!(
                message_preview(&value),
                format!("{}...", character.to_string().repeat(57))
            );
        }
    }

    #[test]
    fn update_post_request_only_requires_content() {
        let payload: UpdatePostRequest = serde_json::from_str(
            r#"{"content":"updated","id":7,"title":"ignored for compatibility"}"#,
        )
        .unwrap();

        assert_eq!(payload.content, "updated");
    }
}
