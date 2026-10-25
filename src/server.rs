//! 路由装配模块。
//!
//! 负责把认证、业务、基础设施三类路由合并，并统一挂载：
//! - 限流（认证接口严格；业务接口按用户区分读/写预算；头像不计数）
//! - 访问日志（按状态码分级）
//! - 安全响应头（HSTS / X-Frame-Options / nosniff）
//!
//! # 装配顺序
//! `with_state` 必须最后调用，否则 `Extension` 注入的状态无法被 handler 取出。

use crate::{account, forum, rate_limit, social, ws};
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
use std::{env, sync::Arc, time::Duration, time::Instant};
use tracing::{Instrument, error, info, info_span, warn};
use uuid::Uuid;

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const DEFAULT_AUTH_RATE_LIMIT: usize = 20;
const DEFAULT_READ_RATE_LIMIT: usize = 600;
const DEFAULT_WRITE_RATE_LIMIT: usize = 120;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    db: &'static str,
    uptime_secs: u64,
}

fn rate_limit_from_env(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub fn build_router(
    pool: Pool<Postgres>,
    ws_state: ws::WsState,
    start_time: Arc<Instant>,
) -> Router {
    let auth_limit = rate_limit_from_env("RATE_LIMIT_AUTH_PER_MINUTE", DEFAULT_AUTH_RATE_LIMIT);
    let read_limit = rate_limit_from_env("RATE_LIMIT_READ_PER_MINUTE", DEFAULT_READ_RATE_LIMIT);
    let write_limit = rate_limit_from_env("RATE_LIMIT_WRITE_PER_MINUTE", DEFAULT_WRITE_RATE_LIMIT);
    info!(
        auth_limit,
        read_limit, write_limit, "Rate limits configured"
    );

    let auth_limiter = rate_limit::RateLimiter::new(auth_limit, RATE_LIMIT_WINDOW);
    let general_limiters =
        rate_limit::GeneralRateLimiters::new(read_limit, write_limit, RATE_LIMIT_WINDOW);

    let auth_routes = account::auth_routes().route_layer(middleware::from_fn_with_state(
        auth_limiter,
        rate_limit::auth_middleware,
    ));

    let general_routes = account::account_routes()
        .merge(forum::forum_routes())
        .merge(social::social_routes())
        .route_layer(middleware::from_fn_with_state(
            general_limiters,
            rate_limit::general_middleware,
        ));

    let infrastructure_routes = Router::new()
        .merge(account::asset_routes())
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
    //不允许内嵌网页
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains; preload"),
    );
    response
}
