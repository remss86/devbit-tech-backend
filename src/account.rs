use crate::{auth, ws};
use axum::{
    Extension, Json, Router,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use lettre::{
    Message, SmtpTransport, Transport,
    message::{Mailbox, header::ContentType},
    transport::smtp::authentication::Credentials,
};
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row};
use std::env;
use tokio::{fs, io::AsyncWriteExt, task};
use uuid::Uuid;

const VERIFICATION_CODE_EXPIRES_SECONDS: u32 = 600;
const AVATAR_DIR: &str = "uploads/avatars";
const MAX_AVATAR_SIZE: usize = 2 * 1024 * 1024;
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
        .route("/avatars/{filename}", get(serve_avatar))
        .route("/api/me", get(current_user))
        .route("/api/me/avatar", post(upload_avatar))
        .route("/api/avatars/{filename}", get(serve_avatar))
        .route("/api/logout", post(logout))
}

async fn create_user(
    State(pool): State<Pool<Postgres>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<CreateUserResponse>, StatusCode> {
    let email = payload.email.trim().to_lowercase();
    if payload.name.trim().is_empty()
        || email.is_empty()
        || payload.password.is_empty()
        || payload.password.len() > auth::MAX_PASSWORD_BYTES
    {
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

    let password = payload.password.clone();
    let password_hash = task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    if payload.password.is_empty() || payload.password.len() > auth::MAX_PASSWORD_BYTES {
        return Err(StatusCode::BAD_REQUEST);
    }

    let email = payload.email.trim().to_lowercase();
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
    let password_is_valid =
        task::spawn_blocking(move || auth::verify_password(&password, &password_hash))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !password_is_valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id: i32 = row.get(1);
    let user_name: String = row.get(2);
    let user_email: String = row.get(3);
    let user_avatar_url: Option<String> = row.get(4);

    if auth::password_needs_upgrade(&stored_password) {
        let password = payload.password.clone();
        let upgraded_hash = task::spawn_blocking(move || auth::hash_password(&password))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        sqlx::query("UPDATE users SET password = $1 WHERE id = $2")
            .bind(upgraded_hash)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

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
) -> Result<Json<SendCodeResponse>, StatusCode> {
    let email = request.email.trim().to_lowercase();
    if email.parse::<Mailbox>().is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let development_code_is_allowed = auth::should_expose_development_code();
    let smtp_username = env::var("SMTP_USERNAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if smtp_username.is_none() && !development_code_is_allowed {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
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
    }))
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
    while let Ok(Some(mut field)) = multipart.next_field().await {
        if field.name().unwrap_or("") != "avatar" {
            continue;
        }

        let file_name = field.file_name().unwrap_or("").to_string();
        if file_name.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }

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

async fn serve_avatar(Path(filename): Path<String>) -> Result<Response, StatusCode> {
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err(StatusCode::NOT_FOUND);
    }

    let data = fs::read(format!("{AVATAR_DIR}/{filename}"))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mime = mime_guess::from_path(&filename).first_or_octet_stream();

    Ok(([(header::CONTENT_TYPE, mime.as_ref())], data).into_response())
}

async fn logout(headers: HeaderMap, Extension(ws_state): Extension<ws::WsState>) -> Response {
    if let Some(user_id) = auth::user_id_from_headers(&headers) {
        ws_state.disconnect_user(user_id);
    }

    (
        [(header::SET_COOKIE, auth::expired_auth_cookie())],
        Json(LogoutResponse { success: true }),
    )
        .into_response()
}
