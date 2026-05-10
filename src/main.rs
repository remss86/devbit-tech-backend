use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use dotenv::dotenv;
use jsonwebtoken::{DecodingKey, Validation, decode};
use jsonwebtoken::{EncodingKey, Header, encode};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use rand;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row};
use std::env;
mod database;
#[derive(Serialize)]
struct User {
    id: i32,
    name: String,
    email: String,
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
#[derive(Debug, sqlx::FromRow)]
struct PostRow {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub author_id: i32,
    pub category: String,
    pub tags: Vec<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub view_count: i32,
    pub comment_count: i32,
    pub like_count: i32,
    pub is_pinned: bool,
    pub is_locked: bool,
}
#[derive(Debug, Deserialize)]
struct CreatePostPayload {
    title: String,
    content: String,
    category: ForumCategory,
    tags: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT")]
pub enum ForumCategory {
    General,
    Tech,
    Devbit,
    Help,
    Showcase,
    Announcement,
}
use std::fmt;

impl fmt::Display for ForumCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForumCategory::General => write!(f, "general"),
            ForumCategory::Tech => write!(f, "tech"),
            ForumCategory::Devbit => write!(f, "devbit"),
            ForumCategory::Help => write!(f, "help"),
            ForumCategory::Showcase => write!(f, "showcase"),
            ForumCategory::Announcement => write!(f, "announcement"),
        }
    }
}
impl std::str::FromStr for ForumCategory {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "general" => Ok(ForumCategory::General),
            "tech" => Ok(ForumCategory::Tech),
            "devbit" => Ok(ForumCategory::Devbit),
            "help" => Ok(ForumCategory::Help),
            "showcase" => Ok(ForumCategory::Showcase),
            "announcement" => Ok(ForumCategory::Announcement),
            _ => Err(format!("Invalid category: {}", s)),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForumPost {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub author: ForumUser,
    pub category: ForumCategory,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub view_count: i32,
    pub comment_count: i32,
    pub like_count: i32,
    pub liked_by_me: bool,
    pub is_pinned: bool,
    pub is_locked: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ForumUser {
    pub id: i32,
    pub name: String,
    pub avatar: String,
    pub is_admin: bool,
}
#[derive(Serialize)]
struct BootstrapResponse {
    users: Vec<ForumUser>,
    posts: Vec<ForumPost>,
    messages: Vec<()>, // 暂时返回空数组
}
async fn bootstrap(pool: State<Pool<Postgres>>) -> Result<Json<BootstrapResponse>, StatusCode> {
    // 查询所有用户
    let users: Vec<ForumUser> =
        sqlx::query_as::<_, ForumUser>("SELECT id, name, avatar, is_admin FROM users")
            .fetch_all(&*pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 查询所有帖子
    let post_rows: Vec<PostRow> = sqlx::query_as::<_, PostRow>(
        "SELECT id, title, content, author_id, category, tags,
                created_at, updated_at, view_count, comment_count,
                like_count, is_pinned, is_locked
         FROM posts
         ORDER BY created_at DESC",
    )
    .fetch_all(&*pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut posts = Vec::new();
    for row in post_rows {
        let author: ForumUser = sqlx::query_as::<_, ForumUser>(
            "SELECT id, name, avatar, is_admin FROM users WHERE id = $1",
        )
        .bind(row.author_id)
        .fetch_one(&*pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        posts.push(ForumPost {
            id: row.id,
            title: row.title,
            content: row.content,
            author,
            category: row.category.parse().unwrap_or(ForumCategory::General),
            tags: row.tags,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            view_count: row.view_count,
            comment_count: row.comment_count,
            like_count: row.like_count,
            liked_by_me: false,
            is_pinned: row.is_pinned,
            is_locked: row.is_locked,
        });
    }

    Ok(Json(BootstrapResponse {
        users,
        posts,
        messages: vec![],
    }))
}
async fn create_user(
    pool: State<Pool<Postgres>>,
    payload: Json<CreateUserRequest>,
) -> Json<CreateUserResponse> {
    let temp: String = sqlx::query("SELECT code FROM verify_code WHERE email = $1")
        .bind(&payload.email)
        .fetch_one(&*pool)
        .await
        .unwrap()
        .get(0);
    if temp != payload.code {
        return Json(CreateUserResponse {
            name: payload.name.clone(),
            email: payload.email.clone(),
            id: 0,
        });
    }
    println!("接收到前端json，开始将用户数据插入数据库");
    let row = sqlx::query("INSERT INTO users (name, email,password,avatar,is_admin) VALUES ($1, $2, $3,$4,$5) RETURNING id")
        .bind(&payload.name)
        .bind(&payload.email)
        .bind(&payload.password)
        .fetch_one(&*pool)
        .await;
    println!("插入成功!");
    Json(CreateUserResponse {
        name: payload.name.clone(),
        email: payload.email.clone(),
        id: row.unwrap().get(0),
    })
}
async fn login_check(
    pool: State<Pool<Postgres>>,
    payload: Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let row = sqlx::query("SELECT password, id, name FROM users WHERE email = $1")
        .bind(&payload.email)
        .fetch_one(&*pool)
        .await
        .unwrap();
    let db_password: String = row.get(0);
    let user_id: i32 = row.get(1);
    let user_name: String = row.get(2);
    if db_password == payload.password {
        let token = generate_token(user_id, &user_name);
        Ok(Json(LoginResponse {
            token,
            user: User {
                id: user_id,
                name: user_name,
                email: payload.email.clone(),
            },
        }))
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
async fn send_verification_code(pool: State<Pool<Postgres>>, req: Json<SendCodeRequest>) {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email = $1)")
        .bind(&req.email)
        .fetch_one(&*pool)
        .await
        .unwrap();
    if exists {
    } else {
        println!("接收到前端json，开始发送验证码");
        let code = rand::random_range(100000..=999999);
        sqlx::query("INSERT INTO verify_code (email, code) VALUES ($1, $2)")
            .bind(&req.email)
            .bind(&code)
            .execute(&*pool)
            .await
            .unwrap();
        let email = Message::builder()
            .from(Mailbox::new(
                Some("devbit".to_owned()),
                "2043399410@qq.com".parse().unwrap(),
            ))
            .to(Mailbox::new(
                Some("client".to_owned()),
                req.email.parse().unwrap(),
            ))
            .subject("devbit")
            .header(ContentType::TEXT_PLAIN)
            .body(format!(
                "[devbit]验证码:{},有效期5分钟,如非本人操作，请忽略.",
                code
            ))
            .unwrap();

        let creds = Credentials::new(
            "2043399410@qq.com".to_owned(),
            "raaukatcqjxydiaa".to_owned(),
        );

        let mailer = SmtpTransport::relay("smtp.qq.com")
            .unwrap()
            .port(465)
            .credentials(creds)
            .build();

        match mailer.send(&email) {
            Ok(_) => println!("Email sent successfully!"),
            Err(e) => panic!("Could not send email: {e:?}"),
        }
    }
}
async fn post_post(
    pool: State<Pool<Postgres>>,
    headers: HeaderMap,
    payload: Json<CreatePostPayload>,
) -> Result<Json<ForumPost>, StatusCode> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id = decode_token(token)?;

    // 插入并返回记录（不用 !）
    let row: PostRow = sqlx::query_as::<_, PostRow>(
        "INSERT INTO posts (title, content, author_id, category, tags)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, title, content, author_id, category, tags,
                   created_at, updated_at, view_count, comment_count,
                   like_count, is_pinned, is_locked",
    )
    .bind(&payload.title)
    .bind(&payload.content)
    .bind(user_id)
    .bind(&payload.category.to_string())
    .bind(&payload.tags)
    .fetch_one(&*pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 查作者（不用 !）
    let author: ForumUser = sqlx::query_as::<_, ForumUser>(
        "SELECT id, name, avatar, is_admin FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&*pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ForumPost {
        id: row.id,
        title: row.title,
        content: row.content,
        author,
        category: payload.category.clone(), // 直接用请求体里的枚举
        tags: row.tags,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        view_count: row.view_count,
        comment_count: row.comment_count,
        like_count: row.like_count,
        liked_by_me: false,
        is_pinned: row.is_pinned,
        is_locked: row.is_locked,
    }))
}

fn decode_token(token: &str) -> Result<i32, StatusCode> {
    let secret = env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?;

    Ok(data.claims.sub) // user_id
}
async fn get_post(
    pool: State<Pool<Postgres>>,
    id: Path<i32>,
) -> Result<Json<ForumPost>, StatusCode> {
    let row: PostRow = sqlx::query_as::<_, PostRow>(
        "SELECT id, title, content, author_id, category, tags,
                created_at, updated_at, view_count, comment_count,
                like_count, is_pinned, is_locked
         FROM posts WHERE id = $1",
    )
    .bind(*id)
    .fetch_one(&*pool)
    .await
    .map_err(|_| StatusCode::NOT_FOUND)?;

    // 浏览量 +1
    let _ = sqlx::query("UPDATE posts SET view_count = view_count + 1 WHERE id = $1")
        .bind(*id)
        .execute(&*pool)
        .await;

    let author: ForumUser = sqlx::query_as::<_, ForumUser>(
        "SELECT id, name, avatar, is_admin FROM users WHERE id = $1",
    )
    .bind(row.author_id)
    .fetch_one(&*pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ForumPost {
        id: row.id,
        title: row.title,
        content: row.content,
        author,
        category: row.category.parse().unwrap_or(ForumCategory::General),
        tags: row.tags,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        view_count: row.view_count + 1,
        comment_count: row.comment_count,
        like_count: row.like_count,
        liked_by_me: false,
        is_pinned: row.is_pinned,
        is_locked: row.is_locked,
    }))
}
async fn delete_post(
    pool: State<Pool<Postgres>>,
    headers: HeaderMap,
    id: Path<i32>,
) -> Result<StatusCode, StatusCode> {
    // 1. 从请求头获取并解码token
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id = decode_token(token)?;

    // 2. 查询帖子信息（获取作者ID）
    let post = sqlx::query("SELECT author_id FROM posts WHERE id = $1")
        .bind(*id)
        .fetch_optional(&*pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let author_id: i32 = post.get(0);

    // 3. 查询当前用户是否是管理员
    let is_admin: bool = sqlx::query_scalar("SELECT is_admin FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&*pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 4. 权限检查：是管理员或者是帖子作者
    if !is_admin && user_id != author_id {
        return Err(StatusCode::FORBIDDEN);
    }

    // 5. 执行删除
    sqlx::query("DELETE FROM posts WHERE id = $1")
        .bind(*id)
        .execute(&*pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT) // 204 删除成功
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let pool = database::db_init().await?;
    let app = Router::new()
        .route("/register", post(create_user))
        .route("/register/send_code", post(send_verification_code))
        .route("/login", post(login_check))
        .route("/forum/bootstrap", get(bootstrap))
        .route("/forum/posts", post(post_post))
        .route("/forum/posts/{id}", get(get_post).get(delete_post))
        .with_state(pool);
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

fn generate_token(user_id: i32, email: &str) -> String {
    let expiration = Utc::now()
        .checked_add_signed(Duration::hours(24))
        .expect("valid timestamp")
        .timestamp() as usize;
    let claims = Claims {
        sub: user_id,
        email: email.to_string(),
        exp: expiration,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(
            env::var("JWT_SECRET")
                .expect("JWT_SECRET must be set")
                .as_bytes(),
        ),
    )
    .unwrap()
}
