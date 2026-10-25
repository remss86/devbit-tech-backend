//! Shared PostgreSQL fixture for unit-level integration tests.
//!
//! A single disposable database is used. The async mutex serialises tests and
//! `setup()` truncates all user data between cases, so registration and social
//! tests cannot observe each other's rows.

#![cfg(test)]

use sqlx::{PgPool, postgres::PgPoolOptions};

pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn setup() -> PgPool {
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
