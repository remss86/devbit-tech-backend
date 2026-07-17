use sqlx::{Pool, Postgres, migrate::Migrator, postgres::PgPoolOptions};
use std::{env, time::Duration};
use tokio::time::timeout;
use tracing::info;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[tracing::instrument(skip_all)]
pub async fn db_init() -> Result<Pool<Postgres>, sqlx::Error> {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:@localhost:5432/users".to_string());
    let pool = timeout(
        Duration::from_secs(5),
        PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout = '10s'")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url),
    )
    .await
    .map_err(|_| sqlx::Error::PoolTimedOut)??;

    info!("Database connected, pool ready");
    MIGRATOR.run(&pool).await?;
    info!("Database migrations applied");

    Ok(pool)
}
