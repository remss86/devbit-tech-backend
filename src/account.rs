//!登录，注册，头像，获取登录凭证模块
use crate::{auth, ws};
use axum::{
    Extension, Json, Router,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use lettre::{
    Message, SmtpTransport, Transport,
    message::{Mailbox, header::ContentType},
    transport::smtp::authentication::Credentials,
};
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row};
use std::env;
use tokio::{fs, io::AsyncWriteExt, sync::Semaphore, task};
use uuid::Uuid;

const VERIFICATION_CODE_EXPIRES_SECONDS: u32 = 600;
const VERIFICATION_RESEND_SECONDS: i64 = 60;
const MAX_VERIFICATION_ATTEMPTS: i32 = 5;
const VERIFICATION_CODE_LENGTH: usize = 6;
const MAX_NAME_CHARS: usize = 50;
const MAX_EMAIL_CHARS: usize = 254;
const MIN_PASSWORD_CHARS: usize = 8;
/// Password hashing is intentionally CPU-expensive. Bound concurrent
/// derivations so a burst of registration/login attempts cannot exhaust all
/// blocking threads.
static PASSWORD_HASH_LIMIT: Semaphore = Semaphore::const_new(4);

const AVATAR_DIR: &str = "uploads/avatars";
const MAX_AVATAR_SIZE: usize = 2 * 1024 * 1024;
const ALLOWED_AVATAR_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Accept a bare address or `Name <address>`, but persist only the canonical
/// lowercase address. This also prevents cooldown bypass by varying the display
/// name for the same mailbox.
fn normalize_email(value: &str) -> Result<String, StatusCode> {
    let mailbox = value
        .trim()
        .parse::<Mailbox>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let email = mailbox.email.to_string().to_lowercase();
    if email.is_empty() || email.chars().count() > MAX_EMAIL_CHARS {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(email)
}

fn validate_name(value: &str) -> Result<String, StatusCode> {
    let name = value.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(name.to_string())
}

fn validate_new_password(value: &str) -> Result<(), StatusCode> {
    let has_letter = value.chars().any(char::is_alphabetic);
    let has_digit = value.chars().any(|character| character.is_ascii_digit());
    if value.chars().count() < MIN_PASSWORD_CHARS
        || value.len() > auth::MAX_PASSWORD_BYTES
        || !has_letter
        || !has_digit
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn validate_verification_code(value: &str) -> Result<String, StatusCode> {
    let code = value.trim();
    if code.len() == VERIFICATION_CODE_LENGTH && code.chars().all(|c| c.is_ascii_digit()) {
        Ok(code.to_string())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

fn rate_limited_verification_response(retry_after: i64) -> Response {
    let retry_after = retry_after.max(1);
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.to_string())],
        Json(serde_json::json!({
            "error": {
                "code": "RATE_LIMITED",
                "message": "操作过于频繁，请稍后再试。",
                "retryAfterSeconds": retry_after,
            }
        })),
    )
        .into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct User {
    id: i32,
    name: String,
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
    is_admin: bool,
}

#[derive(Deserialize)]
struct CreateUserRequest {
    name: String,
    email: String,
    code: String,
    password: String,
}

#[derive(Serialize)]
struct CreateUserResponse {
    name: String,
    email: String,
    id: i32,
}

#[derive(Deserialize)]
struct SendCodeRequest {
    email: String,
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    user: User,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendCodeResponse {
    message: String,
    expires_in_seconds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    development_code: Option<String>,
}

#[derive(Serialize)]
struct LogoutResponse {
    success: bool,
}

pub fn auth_routes() -> Router<Pool<Postgres>> {
    Router::new()
        .route("/register/send_code", post(send_verification_code))
        .route("/register", post(create_user))
        .route("/login", post(login_check))
        .route("/api/register/send_code", post(send_verification_code))
        .route("/api/register", post(create_user))
        .route("/api/login", post(login_check))
}

pub fn account_routes() -> Router<Pool<Postgres>> {
    Router::new()
        .route("/me", get(current_user))
        .route("/me/avatar", post(upload_avatar))
        .route("/logout", post(logout))
        .route("/api/me", get(current_user))
        .route("/api/me/avatar", post(upload_avatar))
        .route("/api/logout", post(logout))
}

/// Public static assets. These are mounted outside the rate limiter: a page
/// with many avatars must not consume the API request budget.
pub fn asset_routes() -> Router<Pool<Postgres>> {
    Router::new()
        .route("/avatars/{filename}", get(serve_avatar))
        .route("/api/avatars/{filename}", get(serve_avatar))
}

async fn create_user(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<CreateUserResponse>, StatusCode> {
    let email = normalize_email(&payload.email)?;
    let name = validate_name(&payload.name)?;
    validate_new_password(&payload.password)?;
    let code = validate_verification_code(&payload.code)?;

    let password = payload.password.clone();
    let password_hash = {
        let _permit = PASSWORD_HASH_LIMIT
            .acquire()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        task::spawn_blocking(move || auth::hash_password(&password))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let row = sqlx::query(
        "SELECT code, attempts FROM verify_code
         WHERE LOWER(email) = LOWER($1) AND expires_at > NOW()
         ORDER BY created_at DESC
         LIMIT 1
         FOR UPDATE",
    )
    .bind(&email)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(row) = row else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let stored_code: String = row.get("code");
    let attempts: i32 = row.get("attempts");

    if attempts >= MAX_VERIFICATION_ATTEMPTS {
        sqlx::query("DELETE FROM verify_code WHERE LOWER(email) = LOWER($1)")
            .bind(&email)
            .execute(&mut *transaction)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        transaction
            .commit()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Err(StatusCode::UNAUTHORIZED);
    }

    if stored_code != code {
        sqlx::query(
            "UPDATE verify_code
             SET attempts = attempts + 1
             WHERE LOWER(email) = LOWER($1)",
        )
        .bind(&email)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        transaction
            .commit()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Err(StatusCode::UNAUTHORIZED);
    }

    let inserted = sqlx::query(
        "INSERT INTO users (name, email, password, is_admin)
         VALUES ($1, $2, $3, FALSE)
         RETURNING id",
    )
    .bind(&name)
    .bind(&email)
    .bind(&password_hash)
    .fetch_one(&mut *transaction)
    .await;

    let row = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            transaction.rollback().await.ok();
            return Err(StatusCode::CONFLICT);
        }
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    sqlx::query("DELETE FROM verify_code WHERE LOWER(email) = LOWER($1)")
        .bind(&email)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    transaction
        .commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(CreateUserResponse {
        name,
        email,
        id: row.get(0),
    }))
}

async fn login_check(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, StatusCode> {
    if payload.password.is_empty() || payload.password.len() > auth::MAX_PASSWORD_BYTES {
        return Err(StatusCode::BAD_REQUEST);
    }

    let email = normalize_email(&payload.email)?;
    let row = sqlx::query(
        "SELECT password, id, name, email, avatar_url FROM users WHERE LOWER(email) = LOWER($1)",
    )
    .bind(&email)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let stored_password: String = row.get(0);
    let password = payload.password.clone();
    let password_hash = stored_password.clone();
    let password_is_valid = {
        let _permit = PASSWORD_HASH_LIMIT
            .acquire()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        task::spawn_blocking(move || auth::verify_password(&password, &password_hash))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };
    if !password_is_valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id: i32 = row.get(1);
    let user_name: String = row.get(2);
    let user_email: String = row.get(3);
    let user_avatar_url: Option<String> = row.get(4);

    if auth::password_needs_upgrade(&stored_password) {
        let password = payload.password.clone();
        let upgraded_hash = {
            let _permit = PASSWORD_HASH_LIMIT
                .acquire()
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            task::spawn_blocking(move || auth::hash_password(&password))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        };
        sqlx::query("UPDATE users SET password = $1 WHERE id = $2")
            .bind(upgraded_hash)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    //登陆的时候把token给用户，往后需要用户信息的请求用户便可携带此信息访问。
    let token = auth::generate_token(user_id, &user_email)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        [(header::SET_COOKIE, auth::auth_cookie(&token))],
        Json(LoginResponse {
            token,
            user: User {
                id: user_id,
                name: user_name,
                email: user_email,
                avatar_url: user_avatar_url,
                is_admin: auth::is_admin_user(user_id),
            },
        }),
    )
        .into_response())
}

async fn send_verification_code(
    State(pool): State<Pool<Postgres>>,
    Json(request): Json<SendCodeRequest>,
) -> Result<Response, StatusCode> {
    let email = normalize_email(&request.email)?;

    let development_code_is_allowed = auth::should_expose_development_code();
    let smtp_username = env::var("SMTP_USERNAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if smtp_username.is_none() && !development_code_is_allowed {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Serialise sends for one mailbox so concurrent requests cannot both pass
    // the cooldown check and trigger duplicate emails.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), 0)")
        .bind(&email)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let last_sent: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT created_at FROM verify_code WHERE LOWER(email) = LOWER($1)")
            .bind(&email)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(last_sent) = last_sent {
        let elapsed = Utc::now().signed_duration_since(last_sent).num_seconds();
        if elapsed < VERIFICATION_RESEND_SECONDS {
            transaction
                .rollback()
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            return Ok(rate_limited_verification_response(
                VERIFICATION_RESEND_SECONDS - elapsed,
            ));
        }
    }

    let code = rand::random_range(100000..=999999).to_string();
    sqlx::query(
        "INSERT INTO verify_code (email, code, expires_at, created_at, attempts)
         VALUES ($1, $2, NOW() + INTERVAL '10 minutes', NOW(), 0)
         ON CONFLICT (LOWER(email)) DO UPDATE
         SET code = EXCLUDED.code,
             expires_at = EXCLUDED.expires_at,
             created_at = NOW(),
             attempts = 0",
    )
    .bind(&email)
    .bind(&code)
    .execute(&mut *transaction)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    transaction
        .commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let development_code = if development_code_is_allowed {
        Some(code.clone())
    } else {
        None
    };

    let smtp_username = match smtp_username {
        Some(value) => value,
        None => {
            return Ok(Json(SendCodeResponse {
                message: "Verification code generated for development.".to_string(),
                expires_in_seconds: VERIFICATION_CODE_EXPIRES_SECONDS,
                development_code,
            })
            .into_response());
        }
    };
    let smtp_password = env::var("SMTP_PASSWORD").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let smtp_server = env::var("SMTP_SERVER").unwrap_or_else(|_| "smtp.qq.com".to_string());
    let smtp_port = env::var("SMTP_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(465);

    let email_message = Message::builder()
        .from(Mailbox::new(
            Some("devbit".to_owned()),
            smtp_username
                .parse()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        ))
        .to(Mailbox::new(
            Some("client".to_owned()),
            email.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
        ))
        .subject("devbit verification code")
        .header(ContentType::TEXT_PLAIN)
        .body(format!(
            "[devbit] Verification code: {code}. It expires in 10 minutes."
        ))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let credentials = Credentials::new(smtp_username, smtp_password);
    let mailer = SmtpTransport::relay(&smtp_server)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .port(smtp_port)
        .credentials(credentials)
        .build();

    task::spawn_blocking(move || mailer.send(&email_message))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok(Json(SendCodeResponse {
        message: "Verification code sent. Please check your email.".to_string(),
        expires_in_seconds: VERIFICATION_CODE_EXPIRES_SECONDS,
        development_code,
    })
    .into_response())
}

async fn current_user(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<User>, StatusCode> {
    let user_id = auth::user_id_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let row = sqlx::query("SELECT id, name, email, avatar_url FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: i32 = row.get("id");
    Ok(Json(User {
        id: user_id,
        name: row.get("name"),
        email: row.get("email"),
        avatar_url: row.get("avatar_url"),
        is_admin: auth::is_admin_user(user_id),
    }))
}

async fn upload_avatar(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<User>, StatusCode> {
    let user_id = auth::user_id_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;

    fs::create_dir_all(AVATAR_DIR)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut field_found = false;
    //while let 是“只要模式匹配成功就继续循环”的语法糖
    while let Ok(Some(mut field)) = multipart.next_field().await {
        if field.name().unwrap_or("") != "avatar" {
            continue;
        }

        let file_name = field.file_name().unwrap_or("").to_string();
        //很难想象一个人怎么传递空文件过来的
        if file_name.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        //WINDOWS允许大写后缀
        let extension = file_name.rsplit('.').next().unwrap_or("").to_lowercase();
        if !ALLOWED_AVATAR_EXTS.contains(&extension.as_str()) {
            return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        let filename = format!("{}.{}", Uuid::new_v4(), extension);
        let filepath = format!("{AVATAR_DIR}/{filename}");

        let mut data = Vec::new();
        while let Ok(Some(chunk)) = field.chunk().await {
            data.extend_from_slice(&chunk);
            if data.len() > MAX_AVATAR_SIZE {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
        }

        if data.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }

        let old_avatar: Option<String> =
            sqlx::query_scalar("SELECT avatar_url FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&pool)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .flatten();

        if let Some(old_url) = old_avatar
            && let Some(old_filename) = old_url.strip_prefix("/api/avatars/")
        {
            let _ = fs::remove_file(format!("{AVATAR_DIR}/{old_filename}")).await;
        }

        let mut file = fs::File::create(&filepath)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        file.write_all(&data)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let avatar_url = format!("/api/avatars/{filename}");
        sqlx::query("UPDATE users SET avatar_url = $1 WHERE id = $2")
            .bind(&avatar_url)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        field_found = true;
        break;
    }

    if !field_found {
        return Err(StatusCode::BAD_REQUEST);
    }

    let row = sqlx::query("SELECT id, name, email, avatar_url FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(User {
        id: row.get("id"),
        name: row.get("name"),
        email: row.get("email"),
        avatar_url: row.get("avatar_url"),
        is_admin: auth::is_admin_user(user_id),
    }))
}

///暂时不知道前端什么时候请求这个函数
async fn serve_avatar(Path(filename): Path<String>) -> Result<Response, StatusCode> {
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err(StatusCode::NOT_FOUND);
    }

    let data = fs::read(format!("{AVATAR_DIR}/{filename}"))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mime = mime_guess::from_path(&filename).first_or_octet_stream();

    // Avatar names are content-addressed UUIDs and never reused, so browsers
    // can cache them for a year without revalidation.
    Ok((
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        data,
    )
        .into_response())
}

async fn logout(headers: HeaderMap, Extension(ws_state): Extension<ws::WsState>) -> Response {
    if let Some(user_id) = auth::user_id_from_headers(&headers) {
        ws_state.disconnect_user(user_id);
    }
    //通过响应头通知浏览器删除cookie
    (
        [(header::SET_COOKIE, auth::expired_auth_cookie())],
        Json(LogoutResponse { success: true }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TEST_LOCK, setup};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use tower::ServiceExt;

    fn app(pool: PgPool) -> Router {
        auth_routes().with_state(pool)
    }

    async fn post(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
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

    async fn issue_code(app: &Router, email: &str) -> String {
        let (status, body) = post(app, "/api/register/send_code", json!({ "email": email })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["developmentCode"].as_str().unwrap().to_string()
    }

    async fn register(
        app: &Router,
        name: &str,
        email: &str,
        code: &str,
        password: &str,
    ) -> (StatusCode, Value) {
        post(
            app,
            "/api/register",
            json!({
                "name": name,
                "email": email,
                "code": code,
                "password": password,
            }),
        )
        .await
    }

    #[tokio::test]
    async fn registration_validates_inputs_and_never_grants_admin() {
        let _guard = TEST_LOCK.lock().await;
        let pool = setup().await;
        let app = app(pool.clone());

        let code = issue_code(&app, "new@example.com").await;

        let (status, _) = register(&app, "New User", "new@example.com", &code, "short").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = register(&app, "New User", "new@example.com", &code, "onlyletters").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = register(&app, "New User", "new@example.com", &code, "12345678").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let long_name = "A".repeat(MAX_NAME_CHARS + 1);
        let (status, _) = register(
            &app,
            &long_name,
            "new@example.com",
            &code,
            "long-enough-password1",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, created) = register(
            &app,
            "New User",
            "new@example.com",
            &code,
            "long-enough-password1",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["id"], 1);

        let user_id = i32::try_from(created["id"].as_i64().unwrap()).unwrap();
        let is_admin: bool = sqlx::query_scalar("SELECT is_admin FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!is_admin);
    }

    #[tokio::test]
    async fn verification_code_locks_after_five_failures() {
        let _guard = TEST_LOCK.lock().await;
        let pool = setup().await;
        let app = app(pool.clone());

        let code = issue_code(&app, "attempt@example.com").await;
        for _ in 0..MAX_VERIFICATION_ATTEMPTS {
            let (status, _) = register(
                &app,
                "Attempt",
                "attempt@example.com",
                "000000",
                "long-enough-password1",
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        let (status, _) = register(
            &app,
            "Attempt",
            "attempt@example.com",
            &code,
            "long-enough-password1",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM verify_code WHERE LOWER(email) = LOWER($1)")
                .bind("attempt@example.com")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);

        // Simulate the cooldown elapsing, then prove a fresh code still works.
        sqlx::query(
            "UPDATE verify_code SET created_at = NOW() - INTERVAL '61 seconds'
             WHERE LOWER(email) = LOWER($1)",
        )
        .bind("attempt@example.com")
        .execute(&pool)
        .await
        .unwrap();
        let fresh_code = issue_code(&app, "attempt@example.com").await;
        let (status, _) = register(
            &app,
            "Attempt",
            "attempt@example.com",
            &fresh_code,
            "long-enough-password1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn verification_email_has_a_per_address_cooldown() {
        let _guard = TEST_LOCK.lock().await;
        let pool = setup().await;
        let app = app(pool);

        let (status, _) = post(
            &app,
            "/api/register/send_code",
            json!({ "email": "Display Name <cooldown@example.com>" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = post(
            &app,
            "/api/register/send_code",
            json!({ "email": "Another Name <cooldown@example.com>" }),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"]["code"], "RATE_LIMITED");
    }

    #[tokio::test]
    async fn malformed_verification_codes_are_rejected() {
        let _guard = TEST_LOCK.lock().await;
        let pool = setup().await;
        let app = app(pool);

        for code in ["12345", "1234567", "abcdef", ""] {
            let (status, _) = register(
                &app,
                "User",
                "malformed@example.com",
                code,
                "long-enough-password1",
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "code={code:?}");
        }
    }
}
