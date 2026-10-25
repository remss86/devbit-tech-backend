//! Sliding-window rate limiting.
//!
//! Authenticated requests are bucketed per user so several people behind one
//! NAT/proxy do not consume a single shared quota. Anonymous requests fall back
//! to the trusted-proxy-aware client IP. General routes use separate read and
//! write budgets; authentication routes keep a stricter IP budget.
//!
//! Responses expose `X-RateLimit-Limit`, `X-RateLimit-Remaining` and
//! `X-RateLimit-Reset`, and a 429 body follows the V2 error envelope with
//! `retryAfterSeconds`.

use crate::auth;
use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const X_RATELIMIT_LIMIT: &str = "x-ratelimit-limit";
const X_RATELIMIT_REMAINING: &str = "x-ratelimit-remaining";
const X_RATELIMIT_RESET: &str = "x-ratelimit-reset";

/// Bucket identity: a signed-in user, or an anonymous client IP.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RateKey {
    Ip(IpAddr),
    User(i32),
}

impl RateKey {
    /// Prefer the JWT identity; fall back to the trusted-proxy-aware IP.
    ///
    /// `identity_from_headers` only verifies the signature/expiry, so this
    /// stays cheap and does not add a database query to every request.
    pub fn for_request(headers: &HeaderMap, peer_ip: IpAddr) -> Self {
        auth::identity_from_headers(headers)
            .map(|identity| Self::User(identity.user_id))
            .unwrap_or_else(|| Self::Ip(client_ip(peer_ip, headers)))
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    requests: Arc<Mutex<HashMap<RateKey, VecDeque<Instant>>>>,
    last_sweep: Arc<Mutex<Instant>>,
    max_requests: usize,
    window: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct RateLimitInfo {
    pub allowed: bool,
    pub limit: usize,
    pub remaining: usize,
    pub reset_after: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            last_sweep: Arc::new(Mutex::new(Instant::now())),
            max_requests: max_requests.max(1),
            window,
        }
    }

    async fn check_at(&self, key: RateKey, now: Instant) -> RateLimitInfo {
        let should_sweep = {
            let mut last_sweep = self.last_sweep.lock().await;
            if now.saturating_duration_since(*last_sweep) >= self.window {
                *last_sweep = now;
                true
            } else {
                false
            }
        };

        let mut requests = self.requests.lock().await;
        if should_sweep {
            requests.retain(|_, entries| {
                entries.retain(|timestamp| now.saturating_duration_since(*timestamp) < self.window);
                !entries.is_empty()
            });
        }

        let entries = requests.entry(key).or_default();
        while entries
            .front()
            .is_some_and(|timestamp| now.saturating_duration_since(*timestamp) >= self.window)
        {
            entries.pop_front();
        }

        if entries.len() >= self.max_requests {
            let reset_after = self
                .window
                .saturating_sub(now.saturating_duration_since(entries[0]));
            return RateLimitInfo {
                allowed: false,
                limit: self.max_requests,
                remaining: 0,
                reset_after,
            };
        }

        entries.push_back(now);
        let reset_after = self
            .window
            .saturating_sub(now.saturating_duration_since(entries[0]));
        RateLimitInfo {
            allowed: true,
            limit: self.max_requests,
            remaining: self.max_requests.saturating_sub(entries.len()),
            reset_after,
        }
    }
}

/// Read/write budgets for one general route group.
#[derive(Clone)]
pub struct GeneralRateLimiters {
    read: RateLimiter,
    write: RateLimiter,
}

impl GeneralRateLimiters {
    pub fn new(read_limit: usize, write_limit: usize, window: Duration) -> Self {
        Self {
            read: RateLimiter::new(read_limit, window),
            write: RateLimiter::new(write_limit, window),
        }
    }
}

#[derive(Serialize)]
struct RateLimitError {
    error: RateLimitErrorDetail,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitErrorDetail {
    code: &'static str,
    message: &'static str,
    retry_after_seconds: u64,
}

fn retry_after_seconds(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
        .max(1)
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: impl ToString) {
    if let Ok(value) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

fn add_rate_limit_headers(headers: &mut HeaderMap, info: RateLimitInfo) {
    insert_header(headers, X_RATELIMIT_LIMIT, info.limit);
    insert_header(headers, X_RATELIMIT_REMAINING, info.remaining);
    insert_header(
        headers,
        X_RATELIMIT_RESET,
        retry_after_seconds(info.reset_after),
    );
}

fn rate_limited_response(info: RateLimitInfo) -> Response {
    let retry_after = retry_after_seconds(info.reset_after);
    let body = RateLimitError {
        error: RateLimitErrorDetail {
            code: "RATE_LIMITED",
            message: "操作过于频繁，请稍后再试。",
            retry_after_seconds: retry_after,
        },
    };

    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    add_rate_limit_headers(response.headers_mut(), info);
    insert_header(response.headers_mut(), "retry-after", retry_after);
    response
}

async fn apply_limit(
    limiter: &RateLimiter,
    key: RateKey,
    request: Request,
    next: Next,
) -> Response {
    let info = limiter.check_at(key, Instant::now()).await;
    if !info.allowed {
        return rate_limited_response(info);
    }

    let mut response = next.run(request).await;
    add_rate_limit_headers(response.headers_mut(), info);
    response
}

/// Strict budget for authentication endpoints, keyed by client IP.
pub async fn auth_middleware(
    State(limiter): State<RateLimiter>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let key = RateKey::Ip(client_ip(address.ip(), request.headers()));
    apply_limit(&limiter, key, request, next).await
}

/// Per-user (or per-IP) budget for business routes, split by method class.
pub async fn general_middleware(
    State(limiters): State<GeneralRateLimiters>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let key = RateKey::for_request(request.headers(), address.ip());
    let method = request.method();
    let limiter = if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        &limiters.read
    } else {
        &limiters.write
    };
    apply_limit(limiter, key, request, next).await
}

/// Trust `X-Real-IP` and `X-Forwarded-For` only from a loopback proxy.
fn client_ip(peer_ip: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !peer_ip.is_loopback() {
        return peer_ip;
    }

    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                // The bundled Nginx config appends its observed client with
                // $proxy_add_x_forwarded_for, so the last hop is authoritative.
                .and_then(|value| value.split(',').next_back())
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(peer_ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;
    use http_body_util::BodyExt;

    const IP_ONE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
    const IP_TWO: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2));

    fn assert_allowed(info: RateLimitInfo) {
        assert!(info.allowed);
        assert!(info.remaining < info.limit);
    }

    #[tokio::test]
    async fn strict_and_general_thresholds_are_independent() {
        let strict = RateLimiter::new(5, Duration::from_secs(60));
        let general = RateLimiter::new(10, Duration::from_secs(60));
        let now = Instant::now();

        for _ in 0..5 {
            assert_allowed(strict.check_at(RateKey::Ip(IP_ONE), now).await);
        }
        assert!(!strict.check_at(RateKey::Ip(IP_ONE), now).await.allowed);

        for _ in 0..10 {
            assert_allowed(general.check_at(RateKey::Ip(IP_ONE), now).await);
        }
        assert!(!general.check_at(RateKey::Ip(IP_ONE), now).await.allowed);
    }

    #[tokio::test]
    async fn requests_are_counted_separately_per_ip() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();

        assert_allowed(limiter.check_at(RateKey::Ip(IP_ONE), now).await);
        assert!(!limiter.check_at(RateKey::Ip(IP_ONE), now).await.allowed);
        assert_allowed(limiter.check_at(RateKey::Ip(IP_TWO), now).await);
    }

    #[tokio::test]
    async fn authenticated_users_are_bucketed_per_user_not_ip() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();

        assert_allowed(limiter.check_at(RateKey::User(1), now).await);
        assert!(!limiter.check_at(RateKey::User(1), now).await.allowed);
        // A second signed-in user behind the same NAT still has a full budget.
        assert_allowed(limiter.check_at(RateKey::User(2), now).await);
    }

    #[tokio::test]
    async fn read_and_write_budgets_are_independent() {
        let limiters = GeneralRateLimiters::new(1, 1, Duration::from_secs(60));
        let now = Instant::now();

        assert_allowed(limiters.read.check_at(RateKey::User(1), now).await);
        assert!(!limiters.read.check_at(RateKey::User(1), now).await.allowed);
        assert_allowed(limiters.write.check_at(RateKey::User(1), now).await);
        assert!(!limiters.write.check_at(RateKey::User(1), now).await.allowed);
    }

    #[tokio::test]
    async fn expired_requests_leave_the_sliding_window() {
        let window = Duration::from_secs(60);
        let limiter = RateLimiter::new(1, window);
        let now = Instant::now();

        assert_allowed(limiter.check_at(RateKey::Ip(IP_ONE), now).await);
        assert_allowed(limiter.check_at(RateKey::Ip(IP_ONE), now + window).await);
    }

    #[tokio::test]
    async fn global_sweep_removes_stale_keys() {
        let window = Duration::from_secs(60);
        let limiter = RateLimiter::new(1, window);
        let now = Instant::now();
        limiter.check_at(RateKey::Ip(IP_ONE), now).await;
        limiter.check_at(RateKey::Ip(IP_TWO), now + window).await;

        let requests = limiter.requests.lock().await;
        assert!(!requests.contains_key(&RateKey::Ip(IP_ONE)));
        assert!(requests.contains_key(&RateKey::Ip(IP_TWO)));
    }

    #[tokio::test]
    async fn retry_after_tracks_oldest_request_and_rounds_up() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        limiter.check_at(RateKey::Ip(IP_ONE), now).await;

        let info = limiter
            .check_at(RateKey::Ip(IP_ONE), now + Duration::from_millis(10_500))
            .await;
        assert!(!info.allowed);
        assert_eq!(info.reset_after, Duration::from_millis(49_500));
        assert_eq!(retry_after_seconds(info.reset_after), 50);
    }

    #[tokio::test]
    async fn rate_limited_response_uses_the_contract_envelope() {
        let info = RateLimitInfo {
            allowed: false,
            limit: 600,
            remaining: 0,
            reset_after: Duration::from_secs(42),
        };
        let response = rate_limited_response(info);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("42")
        );
        assert_eq!(
            response
                .headers()
                .get(X_RATELIMIT_LIMIT)
                .and_then(|value| value.to_str().ok()),
            Some("600")
        );

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "RATE_LIMITED");
        assert_eq!(body["error"]["retryAfterSeconds"], 42);
    }

    #[test]
    fn bearer_tokens_bucket_requests_by_user() {
        let token = auth::generate_token(42, "rate-limit@example.com").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        assert_eq!(RateKey::for_request(&headers, IP_ONE), RateKey::User(42));
        assert_eq!(
            RateKey::for_request(&HeaderMap::new(), IP_ONE),
            RateKey::Ip(IP_ONE)
        );
    }

    #[test]
    fn loopback_proxy_headers_supply_the_client_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.8".parse().unwrap());
        headers.insert(
            "x-forwarded-for",
            "198.51.100.9, 127.0.0.1".parse().unwrap(),
        );

        assert_eq!(
            client_ip(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), &headers),
            "198.51.100.8".parse::<IpAddr>().unwrap()
        );

        headers.remove("x-real-ip");
        assert_eq!(
            client_ip(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), &headers),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn forwarded_for_uses_the_proxy_appended_last_hop() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.9, 203.0.113.12".parse().unwrap(),
        );

        assert_eq!(
            client_ip(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), &headers),
            "203.0.113.12".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn non_loopback_peers_cannot_spoof_forwarded_headers() {
        let peer = "203.0.113.25".parse::<IpAddr>().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.8".parse().unwrap());
        headers.insert("x-forwarded-for", "198.51.100.9".parse().unwrap());

        assert_eq!(client_ip(peer, &headers), peer);
    }

    #[test]
    fn malformed_proxy_headers_fall_back_to_the_peer() {
        let peer = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "not-an-ip".parse().unwrap());
        headers.insert("x-forwarded-for", "also-not-an-ip".parse().unwrap());

        assert_eq!(client_ip(peer, &headers), peer);
    }
}
