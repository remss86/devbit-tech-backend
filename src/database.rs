use sqlx::{Pool, Postgres, postgres::PgPoolOptions};
use std::env;
use std::time::Duration;
use tokio::time::timeout;

pub async fn db_init() -> Result<Pool<Postgres>, sqlx::Error> {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:@localhost:5432/users".to_string());
    let pool = timeout(
        Duration::from_secs(5),
        PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&database_url),
    )
    .await
    .map_err(|_| sqlx::Error::PoolTimedOut)??;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            password VARCHAR(255) NOT NULL DEFAULT '',
            avatar_url TEXT
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "ALTER TABLE users
         ADD COLUMN IF NOT EXISTS avatar_url TEXT",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS verify_code (
            email TEXT NOT NULL,
            code VARCHAR(6) NOT NULL,
            expires_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '10 minutes'
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "ALTER TABLE verify_code
         ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '10 minutes'",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "DELETE FROM verify_code a
         USING verify_code b
         WHERE LOWER(a.email) = LOWER(b.email)
           AND a.ctid < b.ctid",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS verify_code_email_lower_idx
         ON verify_code (LOWER(email))",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_posts (
            id SERIAL PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            author_id INT NOT NULL REFERENCES users(id),
            category VARCHAR(32) NOT NULL DEFAULT 'general',
            tags TEXT[] NOT NULL DEFAULT '{}',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            view_count INT NOT NULL DEFAULT 0,
            is_pinned BOOLEAN NOT NULL DEFAULT false,
            is_locked BOOLEAN NOT NULL DEFAULT false
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_comments (
            id SERIAL PRIMARY KEY,
            post_id INT NOT NULL REFERENCES forum_posts(id) ON DELETE CASCADE,
            author_id INT NOT NULL REFERENCES users(id),
            content TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_messages (
            id SERIAL PRIMARY KEY,
            sender_id INT NOT NULL REFERENCES users(id),
            recipient_id INT NOT NULL REFERENCES users(id),
            content TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            is_read BOOLEAN NOT NULL DEFAULT false
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_post_likes (
            post_id INT NOT NULL REFERENCES forum_posts(id) ON DELETE CASCADE,
            user_id INT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            PRIMARY KEY (post_id, user_id)
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS friends (
            user_id INT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            friend_id INT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            PRIMARY KEY (user_id, friend_id)
        )",
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}
