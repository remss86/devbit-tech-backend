use devbit::{config::AppConfig, database, server, ws};
use dotenv::dotenv;
use std::{error::Error, sync::Arc, time::Instant};
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv().ok();
    let config = AppConfig::from_env()?;
    let _log_guard = init_tracing(config.is_production);

    info!("Starting DevBit backend");
    let pool = database::db_init().await?;
    let websocket_state = ws::WsState::new();
    let app = server::build_router(pool, websocket_state, Arc::new(Instant::now()));

    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    info!(bind_address = %config.bind_address, "DevBit backend listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn init_tracing(production: bool) -> Option<WorkerGuard> {
    let environment_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if production {
        let file_appender = tracing_appender::rolling::daily("logs", "devbit.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt()
            .with_env_filter(environment_filter)
            .json()
            .with_writer(non_blocking)
            .with_target(false)
            .init();
        Some(guard)
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(environment_filter)
            .with_target(false)
            .init();
        None
    }
}
