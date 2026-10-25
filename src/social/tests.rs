//! Social V2 integration tests.
//!
//! The suite uses a single disposable PostgreSQL database from `DATABASE_URL`.
//! Tests are serialised with an async mutex and truncate the schema between
//! cases, which keeps local Windows runs and CI behaviour identical without
//! creating/dropping databases at runtime.

use super::*;
use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn app(pool: PgPool) -> Router {
    crate::forum::forum_routes()
        .merge(social_routes())
        .layer(Extension(ws::WsState::new()))
        .with_state(pool)
}

fn token(user_id: i32) -> String {
    auth::generate_token(user_id, "test@example.com").unwrap()
}

async fn setup() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect(
        "DATABASE_URL must point to a disposable PostgreSQL database for integration tests",
    );
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("apply migrations");
    sqlx::query("TRUNCATE TABLE users, verify_code RESTART IDENTITY CASCADE")
        .execute(&pool)
        .await
        .expect("reset test data");
    pool
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    user_id: i32,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {}", token(user_id)));
    let request_body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(request_body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn create_user(pool: &PgPool, id: i32, name: &str) {
    sqlx::query(
        "INSERT INTO users (id, name, email, password)
         VALUES ($1, $2, $3, 'pbkdf2-sha256$100000$c2FsdHNhbHRzYWx0c2FsdA$ZHVtbXk')",
    )
    .bind(id)
    .bind(name)
    .bind(format!("user{id}@example.com"))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn friend_request_flow_creates_a_symmetric_friendship() {
    let _guard = TEST_LOCK.lock().await;
    let pool = setup().await;
    create_user(&pool, 1, "Alice").await;
    create_user(&pool, 2, "Bob").await;
    let app = app(pool);

    let (status, created) = request(
        &app,
        "POST",
        "/api/v2/social/friend-requests",
        1,
        Some(json!({"recipientId": 2, "message": "let's connect"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let request_id = created["id"].as_i64().unwrap();
    assert_eq!(created["status"], "pending");

    let (status, duplicate) = request(
        &app,
        "POST",
        "/api/v2/social/friend-requests",
        1,
        Some(json!({"recipientId": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(duplicate["id"], request_id);

    let (status, incoming) = request(
        &app,
        "GET",
        "/api/v2/social/friend-requests?direction=incoming",
        2,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(incoming["items"][0]["id"], request_id);

    let (status, accepted) = request(
        &app,
        "POST",
        &format!("/api/v2/social/friend-requests/{request_id}/accept"),
        2,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["status"], "accepted");

    let (status, friends) = request(&app, "GET", "/api/v2/social/friends", 1, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(friends["items"].as_array().unwrap().len(), 1);
    assert_eq!(friends["items"][0]["user"]["name"], "Bob");

    let (status, _) = request(&app, "DELETE", "/api/v2/social/friends/2", 1, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, friends) = request(&app, "GET", "/api/v2/social/friends", 2, None).await;
    assert_eq!(friends["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn block_prevents_messages_and_new_requests() {
    let _guard = TEST_LOCK.lock().await;
    let pool = setup().await;
    create_user(&pool, 1, "Alice").await;
    create_user(&pool, 2, "Bob").await;
    let app = app(pool);

    let (status, _) = request(&app, "PUT", "/api/v2/social/blocks/2", 1, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, blocked) = request(
        &app,
        "POST",
        "/api/v2/social/friend-requests",
        2,
        Some(json!({"recipientId": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{blocked}");

    let (status, _) = request(
        &app,
        "POST",
        "/api/forum/messages",
        2,
        Some(json!({"recipientId": 1, "content": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = request(&app, "DELETE", "/api/v2/social/blocks/2", 1, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = request(
        &app,
        "POST",
        "/api/forum/messages",
        2,
        Some(json!({"recipientId": 1, "content": "hello again"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn blocking_keeps_the_friendship_but_disables_messaging() {
    let _guard = TEST_LOCK.lock().await;
    let pool = setup().await;
    create_user(&pool, 1, "Alice").await;
    create_user(&pool, 2, "Bob").await;
    let app = app(pool);

    let (_, created) = request(
        &app,
        "POST",
        "/api/v2/social/friend-requests",
        1,
        Some(json!({"recipientId": 2})),
    )
    .await;
    let request_id = created["id"].as_i64().unwrap();
    let (status, _) = request(
        &app,
        "POST",
        &format!("/api/v2/social/friend-requests/{request_id}/accept"),
        2,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(&app, "PUT", "/api/v2/social/blocks/2", 1, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, friends) = request(&app, "GET", "/api/v2/social/friends", 1, None).await;
    assert_eq!(friends["items"].as_array().unwrap().len(), 1);
    assert_eq!(friends["items"][0]["user"]["canMessage"], false);

    let (status, _) = request(
        &app,
        "POST",
        "/api/forum/messages",
        2,
        Some(json!({"recipientId": 1, "content": "blocked"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = request(&app, "DELETE", "/api/v2/social/blocks/2", 1, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, friends) = request(&app, "GET", "/api/v2/social/friends", 1, None).await;
    assert_eq!(friends["items"][0]["user"]["canMessage"], true);
}

#[tokio::test]
async fn receive_policy_friends_requires_a_friendship() {
    let _guard = TEST_LOCK.lock().await;
    let pool = setup().await;
    create_user(&pool, 1, "Alice").await;
    create_user(&pool, 2, "Bob").await;
    let app = app(pool);

    let (status, preferences) = request(
        &app,
        "PATCH",
        "/api/v2/social/preferences",
        2,
        Some(json!({"messagePolicy": "friends"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preferences["messagePolicy"], "friends");

    let (status, _) = request(
        &app,
        "POST",
        "/api/forum/messages",
        1,
        Some(json!({"recipientId": 2, "content": "not friends yet"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn search_and_presence_report_relationship_and_online_state() {
    let _guard = TEST_LOCK.lock().await;
    let pool = setup().await;
    create_user(&pool, 1, "Alice").await;
    create_user(&pool, 2, "Bob").await;
    let app = app(pool);

    let (status, users) = request(&app, "GET", "/api/v2/social/users?q=bob", 1, None).await;
    assert_eq!(status, StatusCode::OK, "{users}");
    assert_eq!(users["items"][0]["relationship"], "none");
    assert_eq!(users["items"][0]["canMessage"], true);

    let (status, presence) =
        request(&app, "GET", "/api/v2/social/presence?userIds=1,2", 1, None).await;
    assert_eq!(status, StatusCode::OK, "{presence}");
    // Only the viewer is visible until a V2 friendship exists.
    assert_eq!(presence["users"].as_array().unwrap().len(), 1);
    assert_eq!(presence["users"][0]["userId"], 1);
    assert_eq!(presence["users"][0]["status"], "offline");
}

#[tokio::test]
async fn clear_history_hides_only_the_viewer_side() {
    let _guard = TEST_LOCK.lock().await;
    let pool = setup().await;
    create_user(&pool, 1, "Alice").await;
    create_user(&pool, 2, "Bob").await;
    let app = app(pool);

    let (status, _) = request(
        &app,
        "POST",
        "/api/forum/messages",
        1,
        Some(json!({"recipientId": 2, "content": "secret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "DELETE",
        "/api/v2/social/conversations/2/history",
        1,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, alice_messages) = request(&app, "GET", "/api/forum/messages", 1, None).await;
    assert_eq!(alice_messages.as_array().unwrap().len(), 0);

    let (_, bob_messages) = request(&app, "GET", "/api/forum/messages", 2, None).await;
    assert_eq!(bob_messages.as_array().unwrap().len(), 1);
}
