use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum::extract::{ConnectInfo, Multipart, Path as AxumPath};
use axum::middleware;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use dotenv::dotenv;
use hmac::{Hmac, Mac};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use rand;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{Pool, Postgres, Row};
use std::collections::HashMap;
use std::env;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

mod database;
use devbit::forum;

type HmacSha256 = Hmac<Sha256>;
const AUTH_COOKIE_NAME: &str = "auth_token";
const PASSWORD_HASH_PREFIX: &str = "pbkdf2-sha256";
const PASSWORD_HASH_ITERATIONS: u32 = 100_000;
const PASSWORD_HASH_BYTES: usize = 32;
const VERIFICATION_CODE_EXPIRES_SECONDS: u32 = 600;
const AVATAR_DIR: &str = "uploads/avatars";
const MAX_AVATAR_SIZE: usize = 2 * 1024 * 1024; // 2 MB
const ALLOWED_AVATAR_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

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

async fn create_user(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<CreateUserResponse>, StatusCode> {
    let email = payload.email.trim().to_lowercase();
    if payload.name.trim().is_empty() || email.is_empty() || payload.password.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let code_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM verify_code
            WHERE LOWER(email) = LOWER($1)
              AND code = $2
              AND expires_at > NOW()
        )",
    )
    .bind(&email)
    .bind(payload.code.trim())
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !code_exists {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let password_hash = hash_password(&payload.password);
    let row = sqlx::query(
        "INSERT INTO users (name, email, password)
         VALUES ($1, $2, $3)
         RETURNING id",
    )
    .bind(payload.name.trim())
    .bind(&email)
    .bind(&password_hash)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::CONFLICT)?;

    sqlx::query("DELETE FROM verify_code WHERE LOWER(email) = LOWER($1)")
        .bind(&email)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(CreateUserResponse {
        name: payload.name.trim().to_string(),
        email,
        id: row.get(0),
    }))
}

async fn login_check(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, StatusCode> {
    let email = payload.email.trim().to_lowercase();
    let row =
        sqlx::query("SELECT password, id, name, email, avatar_url FROM users WHERE LOWER(email) = LOWER($1)")
            .bind(&email)
            .fetch_optional(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::UNAUTHORIZED)?;

    let stored_password: String = row.get(0);
    if !verify_password(&payload.password, &stored_password) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id: i32 = row.get(1);
    let user_name: String = row.get(2);
    let user_email: String = row.get(3);
    let user_avatar_url: Option<String> = row.get(4);

    if !stored_password.starts_with(PASSWORD_HASH_PREFIX) {
        let upgraded_hash = hash_password(&payload.password);
        sqlx::query("UPDATE users SET password = $1 WHERE id = $2")
            .bind(upgraded_hash)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    let token =
        generate_token(user_id, &user_email).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let cookie =
        format!("{AUTH_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400");

    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(LoginResponse {
            token,
            user: User {
                id: user_id,
                name: user_name,
                email: user_email,
                avatar_url: user_avatar_url,
                is_admin: is_admin_user(user_id),
            },
        }),
    )
        .into_response())
}

async fn send_verification_code(
    State(pool): State<Pool<Postgres>>,
    Json(req): Json<SendCodeRequest>,
) -> Result<Json<SendCodeResponse>, StatusCode> {
    let email = req.email.trim().to_lowercase();
    if email.parse::<Mailbox>().is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let code = rand::random_range(100000..=999999).to_string();
    sqlx::query("DELETE FROM verify_code WHERE LOWER(email) = LOWER($1)")
        .bind(&email)
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    sqlx::query(
        "INSERT INTO verify_code (email, code, expires_at)
         VALUES ($1, $2, NOW() + INTERVAL '10 minutes')",
    )
    .bind(&email)
    .bind(&code)
    .execute(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let development_code = if should_expose_development_code() {
        Some(code.clone())
    } else {
        None
    };

    let smtp_username = match env::var("SMTP_USERNAME") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return Ok(Json(SendCodeResponse {
                message: "Verification code generated for development.".to_string(),
                expires_in_seconds: VERIFICATION_CODE_EXPIRES_SECONDS,
                development_code,
            }));
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

    let creds = Credentials::new(smtp_username, smtp_password);
    let mailer = SmtpTransport::relay(&smtp_server)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .port(smtp_port)
        .credentials(creds)
        .build();

    mailer
        .send(&email_message)
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok(Json(SendCodeResponse {
        message: "Verification code sent. Please check your email.".to_string(),
        expires_in_seconds: VERIFICATION_CODE_EXPIRES_SECONDS,
        development_code,
    }))
}

async fn current_user(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
) -> Result<Json<User>, StatusCode> {
    let user_id = user_id_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
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
        is_admin: is_admin_user(user_id),
    }))
}

async fn upload_avatar(
    State(pool): State<Pool<Postgres>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<User>, StatusCode> {
    let user_id = user_id_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;

    // Ensure avatar directory exists
    fs::create_dir_all(AVATAR_DIR)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut field_found = false;
    while let Ok(Some(mut field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name != "avatar" {
            continue;
        }

        let file_name = field.file_name().unwrap_or("").to_string();
        if file_name.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }

        // Validate file extension
        let ext = file_name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        if !ALLOWED_AVATAR_EXTS.contains(&ext.as_str()) {
            return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        // Generate unique filename
        let filename = format!("{}.{}", Uuid::new_v4(), ext);
        let filepath = format!("{}/{}", AVATAR_DIR, filename);

        // Read file bytes
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

        // Delete old avatar file if exists
        let old_avatar: Option<String> = sqlx::query_scalar(
            "SELECT avatar_url FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .flatten();

        if let Some(old_url) = old_avatar {
            if let Some(old_filename) = old_url.strip_prefix("/api/avatars/") {
                let old_path = format!("{}/{}", AVATAR_DIR, old_filename);
                let _ = fs::remove_file(&old_path).await;
            }
        }

        // Write new file
        let mut file = fs::File::create(&filepath)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        file.write_all(&data)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        // Update database
        let avatar_url = format!("/api/avatars/{}", filename);
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

    // Return updated user
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
        is_admin: is_admin_user(user_id),
    }))
}

async fn serve_avatar(
    AxumPath(filename): AxumPath<String>,
) -> Result<Response, StatusCode> {
    // Prevent directory traversal
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err(StatusCode::NOT_FOUND);
    }

    let filepath = format!("{}/{}", AVATAR_DIR, filename);

    let data = fs::read(&filepath)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let mime = mime_guess::from_path(&filename)
        .first_or_octet_stream();

    Ok((
        [(header::CONTENT_TYPE, mime.as_ref())],
        data,
    )
        .into_response())
}

async fn logout() -> Response {
    let cookie = format!(
        "{AUTH_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
    );

    (
        [(header::SET_COOKIE, cookie)],
        Json(LogoutResponse { success: true }),
    )
        .into_response()
}

// ── Rate Limiter ────────────────────────────────────────────────────────────

type RateLimiter = Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>;

#[derive(Serialize)]
struct RateLimitError {
    error: String,
}

/// Simple sliding-window rate limiting middleware (10 req / 60s per IP).
async fn rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    ConnectInfo(addr): ConnectInfo<IpAddr>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    const MAX_REQUESTS: usize = 10;
    const WINDOW_SECS: u64 = 60;

    let now = Instant::now();
    let window = std::time::Duration::from_secs(WINDOW_SECS);

    {
        let mut map = limiter.lock().unwrap();
        let entries = map.entry(addr).or_default();
        entries.retain(|t| now.duration_since(*t) < window);

        if entries.len() >= MAX_REQUESTS {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "60")],
                Json(RateLimitError {
                    error: "Too many requests. Try again later.".into(),
                }),
            )
                .into_response();
        }

        entries.push(now);
    }

    next.run(request).await
}

/// Stricter rate limiter for auth-sensitive endpoints (5 req / 60s per IP).
async fn strict_rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    ConnectInfo(addr): ConnectInfo<IpAddr>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    const MAX_REQUESTS: usize = 5;
    const WINDOW_SECS: u64 = 60;

    let now = Instant::now();
    let window = std::time::Duration::from_secs(WINDOW_SECS);

    {
        let mut map = limiter.lock().unwrap();
        let entries = map.entry(addr).or_default();
        entries.retain(|t| now.duration_since(*t) < window);

        if entries.len() >= MAX_REQUESTS {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "60")],
                Json(RateLimitError {
                    error: "Too many requests. Try again later.".into(),
                }),
            )
                .into_response();
        }

        entries.push(now);
    }

    next.run(request).await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let pool = database::db_init().await?;

    // ── Rate limiter state ──────────────────────────────────────────────────
    let rate_limiter: RateLimiter = Arc::new(Mutex::new(HashMap::new()));

    // Auth-sensitive routes — strict rate limit (5 req / 60s)
    let auth_routes = Router::new()
        .route("/register/send_code", post(send_verification_code))
        .route("/register", post(create_user))
        .route("/login", post(login_check))
        .route("/api/register/send_code", post(send_verification_code))
        .route("/api/register", post(create_user))
        .route("/api/login", post(login_check))
        .route_layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            strict_rate_limit_middleware,
        ))
        .with_state(pool.clone());

    // General routes — standard rate limit (10 req / 60s)
    let general_routes = Router::new()
        .route("/me", get(current_user))
        .route("/me/avatar", post(upload_avatar))
        .route("/logout", post(logout))
        .route("/avatars/{filename}", get(serve_avatar))
        .route("/api/me", get(current_user))
        .route("/api/me/avatar", post(upload_avatar))
        .route("/api/avatars/{filename}", get(serve_avatar))
        .route("/api/logout", post(logout))
        .merge(forum::forum_routes())
        .route_layer(middleware::from_fn_with_state(
            rate_limiter,
            rate_limit_middleware,
        ))
        .with_state(pool.clone());

    let app = auth_routes.merge(general_routes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:7878").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: i32,
    email: String,
    exp: usize,
}

fn generate_token(user_id: i32, email: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let expiration = Utc::now()
        .checked_add_signed(Duration::hours(24))
        .expect("valid timestamp")
        .timestamp() as usize;
    let claims = Claims {
        sub: user_id,
        email: email.to_string(),
        exp: expiration,
    };
    let secret = env::var("JWT_SECRET").unwrap_or_else(|_| "devbit-local-secret".to_string());
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

fn is_admin_user(user_id: i32) -> bool {
    user_id == 1 || user_id == 2
}

fn should_expose_development_code() -> bool {
    env::var("NODE_ENV")
        .map(|value| value.trim() != "production")
        .unwrap_or(true)
}

fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(token.to_string());
    }

    headers
        .get(header::COOKIE)
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

fn user_id_from_headers(headers: &HeaderMap) -> Option<i32> {
    let token = token_from_headers(headers)?;
    let secret = env::var("JWT_SECRET").unwrap_or_else(|_| "devbit-local-secret".to_string());
    let data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .ok()?;
    Some(data.claims.sub)
}

fn hash_password(password: &str) -> String {
    let salt: [u8; 16] = rand::random();
    let mut output = [0_u8; PASSWORD_HASH_BYTES];
    pbkdf2_hmac_sha256(
        password.as_bytes(),
        &salt,
        PASSWORD_HASH_ITERATIONS,
        &mut output,
    );

    format!(
        "{PASSWORD_HASH_PREFIX}${PASSWORD_HASH_ITERATIONS}${}${}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(output)
    )
}

fn verify_password(password: &str, stored_hash: &str) -> bool {
    let parts: Vec<&str> = stored_hash.split('$').collect();
    if parts.len() != 4 || parts[0] != PASSWORD_HASH_PREFIX {
        return password == stored_hash;
    }

    let iterations = match parts[1].parse::<u32>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let salt = match URL_SAFE_NO_PAD.decode(parts[2]) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let expected = match URL_SAFE_NO_PAD.decode(parts[3]) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let mut actual = vec![0_u8; expected.len()];
    pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations, &mut actual);
    constant_time_eq(&actual, &expected)
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, output: &mut [u8]) {
    for (block_index, chunk) in output.chunks_mut(32).enumerate() {
        let block_number = (block_index as u32 + 1).to_be_bytes();
        let mut mac = HmacSha256::new_from_slice(password).expect("HMAC accepts any key length");
        mac.update(salt);
        mac.update(&block_number);
        let mut u = mac.finalize().into_bytes().to_vec();
        let mut t = u.clone();

        for _ in 1..iterations {
            let mut mac =
                HmacSha256::new_from_slice(password).expect("HMAC accepts any key length");
            mac.update(&u);
            u = mac.finalize().into_bytes().to_vec();
            for (target, value) in t.iter_mut().zip(u.iter()) {
                *target ^= value;
            }
        }

        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
