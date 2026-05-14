use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, put},
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row, postgres::PgRow};
use std::env;

const AUTH_COOKIE_NAME: &str = "auth_token";

#[derive(Debug, Clone, Deserialize)]
struct Claims {
    sub: i32,
    #[serde(rename = "exp")]
    _exp: usize,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForumUser {
    pub id: i32,
    pub name: String,
    pub avatar: String,
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
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForumComment {
    pub id: i32,
    pub post_id: i32,
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
#[serde(rename_all = "camelCase")]
pub struct CreateCommentRequest {
    pub content: String,
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
    } else if initials.len() == 1 {
        if let Some(next) = name.chars().find(|ch| ch.is_ascii_lowercase()) {
            initials.push(next.to_ascii_uppercase());
        }
    }

    initials
}

fn forum_user(id: i32, name: String) -> ForumUser {
    ForumUser {
        id,
        avatar: avatar_for_user(id, &name),
        name,
        is_admin: id == 1 || id == 2,
    }
}

fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(token) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(token.to_string());
    }

    headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix(&format!("{AUTH_COOKIE_NAME}="))
                    .map(str::to_string)
            })
        })
}

fn user_id_from_token(token: &str) -> Option<i32> {
    let secret = env::var("JWT_SECRET").ok()?;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .ok()?;
    Some(data.claims.sub)
}

fn optional_user_id(headers: &HeaderMap) -> Option<i32> {
    token_from_headers(headers).and_then(|token| user_id_from_token(&token))
}

fn require_user_id(headers: &HeaderMap) -> Result<i32, StatusCode> {
    optional_user_id(headers).ok_or(StatusCode::UNAUTHORIZED)
}

async fn get_current_user(pool: &Pool<Postgres>, user_id: i32) -> Result<ForumUser, StatusCode> {
    let row = sqlx::query("SELECT id, name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    row.map(|r| forum_user(r.get("id"), r.get("name")))
        .ok_or(StatusCode::UNAUTHORIZED)
}

fn row_to_post(row: &PgRow) -> ForumPost {
    let author_id: i32 = row.get("author_id");
    let author_name: String = row.get("author_name");

    ForumPost {
        id: row.get("id"),
        title: row.get("title"),
        content: row.get("content"),
        author: forum_user(author_id, author_name),
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
    }
}

fn row_to_message(row: &PgRow) -> ForumMessage {
    let sender_id: i32 = row.get("sender_id");
    let recipient_id: i32 = row.get("recipient_id");

    ForumMessage {
        id: row.get("id"),
        sender: forum_user(sender_id, row.get("sender_name")),
        recipient: forum_user(recipient_id, row.get("recipient_name")),
        content: row.get("content"),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
        is_read: row.get("is_read"),
    }
}

async fn fetch_users(pool: &Pool<Postgres>) -> Result<Vec<ForumUser>, StatusCode> {
    let rows = sqlx::query("SELECT id, name FROM users ORDER BY id ASC")
        .fetch_all(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows
        .iter()
        .map(|row| forum_user(row.get("id"), row.get("name")))
        .collect())
}

async fn fetch_post_by_id(
    pool: &Pool<Postgres>,
    id: i32,
    viewer_user_id: Option<i32>,
) -> Result<ForumPost, StatusCode> {
    let row = sqlx::query(
        "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                p.is_pinned, p.is_locked, u.name as author_name,
                (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                COUNT(l.user_id)::BIGINT as like_count,
                COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me
         FROM forum_posts p
         JOIN users u ON u.id = p.author_id
         LEFT JOIN forum_post_likes l ON l.post_id = p.id
         WHERE p.id = $2
         GROUP BY p.id, u.name",
    )
    .bind(viewer_user_id)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    row.map(|row| row_to_post(&row))
        .ok_or(StatusCode::NOT_FOUND)
}

async fn fetch_messages_for_user(
    pool: &Pool<Postgres>,
    user_id: i32,
) -> Result<Vec<ForumMessage>, StatusCode> {
    let rows = sqlx::query(
        "SELECT m.id, m.sender_id, m.recipient_id, m.content, m.created_at, m.is_read,
                s.name as sender_name,
                r.name as recipient_name
         FROM forum_messages m
         JOIN users s ON s.id = m.sender_id
         JOIN users r ON r.id = m.recipient_id
         WHERE m.sender_id = $1 OR m.recipient_id = $1
         ORDER BY m.created_at ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows.iter().map(row_to_message).collect())
}

async fn bootstrap(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<ForumBootstrap>, StatusCode> {
    let viewer_user_id = optional_user_id(&headers);
    let users = fetch_users(&pool).await?;
    let posts = fetch_posts(&pool, None, viewer_user_id).await?;
    let messages = match viewer_user_id {
        Some(user_id) => fetch_messages_for_user(&pool, user_id).await?,
        None => Vec::new(),
    };

    Ok(Json(ForumBootstrap {
        users,
        posts,
        messages,
    }))
}

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
                    p.is_pinned, p.is_locked, u.name as author_name,
                    (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                    COUNT(l.user_id)::BIGINT as like_count,
                    COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me
             FROM forum_posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN forum_post_likes l ON l.post_id = p.id
             WHERE p.category = $2
             GROUP BY p.id, u.name
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
                    p.is_pinned, p.is_locked, u.name as author_name,
                    (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                    COUNT(l.user_id)::BIGINT as like_count,
                    COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me
             FROM forum_posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN forum_post_likes l ON l.post_id = p.id
             GROUP BY p.id, u.name
             ORDER BY p.is_pinned DESC, p.created_at DESC",
        )
        .bind(viewer_user_id)
        .fetch_all(pool)
        .await
    }
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows.iter().map(row_to_post).collect())
}

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
    let _ = sqlx::query("UPDATE forum_posts SET view_count = view_count + 1 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await;

    Ok(Json(
        fetch_post_by_id(&pool, id, optional_user_id(&headers)).await?,
    ))
}

async fn create_post(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<CreatePostRequest>,
) -> Result<Json<ForumPost>, StatusCode> {
    let user_id: i32 = 1;
    let user = get_current_user(&pool, user_id).await?;
    let category = payload.category.unwrap_or_else(|| "general".to_string());
    let tags = payload.tags.unwrap_or_default();

    let row = sqlx::query(
        "INSERT INTO forum_posts (title, content, author_id, category, tags)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, created_at, updated_at",
    )
    .bind(&payload.title)
    .bind(&payload.content)
    .bind(user_id)
    .bind(&category)
    .bind(&tags)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let created_at: DateTime<Utc> = row.get("created_at");
    let updated_at: DateTime<Utc> = row.get("updated_at");

    Ok(Json(ForumPost {
        id: row.get("id"),
        title: payload.title,
        content: payload.content,
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
    }))
}

async fn delete_post(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
) -> Result<StatusCode, StatusCode> {
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
) -> Result<Json<ForumPost>, StatusCode> {
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
) -> Result<Json<ForumPost>, StatusCode> {
    let result = sqlx::query("UPDATE forum_posts SET is_locked = NOT is_locked WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(fetch_post_by_id(&pool, id, None).await?))
}

async fn toggle_like(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> Result<Json<ForumPost>, StatusCode> {
    let user_id = require_user_id(&headers)?;
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

async fn list_comments(
    State(pool): State<Pool<Postgres>>,
    Path(post_id): Path<i32>,
) -> Json<Vec<ForumComment>> {
    let rows = sqlx::query(
        "SELECT c.*, u.name as author_name
         FROM forum_comments c
         JOIN users u ON u.id = c.author_id
         WHERE c.post_id = $1
         ORDER BY c.created_at ASC",
    )
    .bind(post_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let comments: Vec<ForumComment> = rows
        .iter()
        .map(|r| {
            let author_id: i32 = r.get("author_id");
            ForumComment {
                id: r.get("id"),
                post_id: r.get("post_id"),
                author: forum_user(author_id, r.get("author_name")),
                content: r.get("content"),
                created_at: r.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
            }
        })
        .collect();

    Json(comments)
}

async fn create_comment(
    State(pool): State<Pool<Postgres>>,
    Path(post_id): Path<i32>,
    Json(payload): Json<CreateCommentRequest>,
) -> Result<Json<ForumComment>, StatusCode> {
    let user_id: i32 = 1;
    let user = get_current_user(&pool, user_id).await?;

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

    let row = sqlx::query(
        "INSERT INTO forum_comments (post_id, author_id, content) VALUES ($1, $2, $3)
         RETURNING id, created_at",
    )
    .bind(post_id)
    .bind(user_id)
    .bind(&payload.content)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let created_at: DateTime<Utc> = row.get("created_at");

    Ok(Json(ForumComment {
        id: row.get("id"),
        post_id,
        author: user,
        content: payload.content,
        created_at: created_at.to_rfc3339(),
    }))
}

async fn delete_comment(
    State(pool): State<Pool<Postgres>>,
    Path(id): Path<i32>,
) -> Result<StatusCode, StatusCode> {
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

async fn list_messages(State(pool): State<Pool<Postgres>>) -> Json<Vec<ForumMessage>> {
    let user_id: i32 = 1;
    Json(
        fetch_messages_for_user(&pool, user_id)
            .await
            .unwrap_or_default(),
    )
}

async fn send_message(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<SendMessageRequest>,
) -> Result<Json<ForumMessage>, StatusCode> {
    let user_id: i32 = 1;
    let sender = get_current_user(&pool, user_id).await?;
    let recipient = get_current_user(&pool, payload.recipient_id).await?;

    let row = sqlx::query(
        "INSERT INTO forum_messages (sender_id, recipient_id, content)
         VALUES ($1, $2, $3)
         RETURNING id, created_at",
    )
    .bind(user_id)
    .bind(payload.recipient_id)
    .bind(&payload.content)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let created_at: DateTime<Utc> = row.get("created_at");

    Ok(Json(ForumMessage {
        id: row.get("id"),
        sender,
        recipient,
        content: payload.content,
        created_at: created_at.to_rfc3339(),
        is_read: false,
    }))
}

async fn mark_message_read(State(pool): State<Pool<Postgres>>, Path(id): Path<i32>) -> StatusCode {
    let _ = sqlx::query("UPDATE forum_messages SET is_read = true WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await;

    StatusCode::NO_CONTENT
}

async fn mark_conversation_read(
    State(pool): State<Pool<Postgres>>,
    Path(partner_id): Path<i32>,
) -> StatusCode {
    let user_id: i32 = 1;
    let _ = sqlx::query(
        "UPDATE forum_messages SET is_read = true
         WHERE sender_id = $1 AND recipient_id = $2 AND is_read = false",
    )
    .bind(partner_id)
    .bind(user_id)
    .execute(&pool)
    .await;

    StatusCode::NO_CONTENT
}

async fn search_posts(
    State(pool): State<Pool<Postgres>>,
    Query(q): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<ForumPost>>, StatusCode> {
    let query = match q.q {
        Some(ref s) if !s.trim().is_empty() => s.trim().to_lowercase(),
        _ => return Ok(Json(vec![])),
    };

    let pattern = format!("%{}%", query);
    let rows = sqlx::query(
        "SELECT p.id, p.title, p.content, p.author_id, p.category, p.tags,
                p.created_at, p.updated_at, p.view_count::BIGINT as view_count,
                p.is_pinned, p.is_locked, u.name as author_name,
                (SELECT COUNT(*) FROM forum_comments WHERE post_id = p.id)::BIGINT as comment_count,
                COUNT(l.user_id)::BIGINT as like_count,
                COALESCE(BOOL_OR(l.user_id = $1), false) as liked_by_me
         FROM forum_posts p
         JOIN users u ON u.id = p.author_id
         LEFT JOIN forum_post_likes l ON l.post_id = p.id
         WHERE LOWER(p.title) LIKE $2 OR LOWER(p.content) LIKE $2
         GROUP BY p.id, u.name
         ORDER BY p.is_pinned DESC, p.created_at DESC",
    )
    .bind(optional_user_id(&headers))
    .bind(&pattern)
    .fetch_all(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(rows.iter().map(row_to_post).collect()))
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
        .route(
            "/api/forum/posts/{id}/comments",
            get(list_comments).post(create_comment),
        )
        .route("/api/forum/comments/{id}", delete(delete_comment))
        .route("/api/forum/messages", get(list_messages).post(send_message))
        .route("/api/forum/messages/{id}/read", put(mark_message_read))
        .route(
            "/api/forum/messages/conversation/{partner_id}/read",
            put(mark_conversation_read),
        )
}
