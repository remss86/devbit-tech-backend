use crate::{account, forum, rate_limit, ws};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::Response,
    routing::get,
};
use serde::Serialize;
use sqlx::{Pool, Postgres};
use std::{sync::Arc, time::Duration, time::Instant};
use tracing::{Instrument, error, info, info_span, warn};
use uuid::Uuid;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    db: &'static str,
    uptime_secs: u64,
}

pub fn build_router(
    pool: Pool<Postgres>,
    ws_state: ws::WsState,
    start_time: Arc<Instant>,
) -> Router {
    let strict_rate_limiter = rate_limit::RateLimiter::new(5, Duration::from_secs(60));
    let general_rate_limiter = rate_limit::RateLimiter::new(10, Duration::from_secs(60));

    let auth_routes = account::auth_routes().route_layer(middleware::from_fn_with_state(
        strict_rate_limiter,
        rate_limit::middleware,
    ));

    let general_routes = account::account_routes()
        .merge(forum::forum_routes())
        .route_layer(middleware::from_fn_with_state(
            general_rate_limiter,
            rate_limit::middleware,
        ));

    let infrastructure_routes = Router::new()
        .route("/health", get(health_check))
        .route("/api/health", get(health_check))
        .route("/ws", get(ws::ws_handler))
        .route("/api/ws", get(ws::ws_handler));

    auth_routes
        .merge(general_routes)
        .merge(infrastructure_routes)
        .layer(middleware::from_fn(logging_middleware))
        .layer(middleware::from_fn(security_headers_middleware))
        .layer(Extension(start_time))
        .layer(Extension(ws_state))
        .with_state(pool)
}

async fn health_check(
    State(pool): State<Pool<Postgres>>,
    Extension(start_time): Extension<Arc<Instant>>,
) -> Json<HealthResponse> {
    let database_is_available = sqlx::query("SELECT 1").fetch_one(&pool).await.is_ok();

    Json(HealthResponse {
        status: if database_is_available {
            "ok"
        } else {
            "degraded"
        },
        db: if database_is_available {
            "ok"
        } else {
            "unreachable"
        },
        uptime_secs: start_time.elapsed().as_secs(),
    })
}

async fn logging_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let start = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().clone();
    let request_id = Uuid::new_v4().to_string();
    let span = info_span!("request", %request_id, method = %method, uri = %uri);

    async move {
        let response = next.run(request).await;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = response.status().as_u16();

        if status >= StatusCode::INTERNAL_SERVER_ERROR.as_u16() {
            error!(
                %request_id,
                method = %method,
                uri = %uri,
                status,
                latency_ms,
                "Request failed with server error"
            );
        } else if status >= StatusCode::BAD_REQUEST.as_u16() {
            warn!(
                %request_id,
                method = %method,
                uri = %uri,
                status,
                latency_ms,
                "Request failed with client error"
            );
        } else {
            info!(
                %request_id,
                method = %method,
                uri = %uri,
                status,
                latency_ms,
                "Request completed"
            );
        }

        response
    }
    .instrument(span)
    .await
}

async fn security_headers_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains; preload"),
    );
    response
}
