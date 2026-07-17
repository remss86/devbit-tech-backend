use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
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

#[derive(Clone)]
pub struct RateLimiter {
    requests: Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
    last_sweep: Arc<Mutex<Instant>>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            last_sweep: Arc::new(Mutex::new(Instant::now())),
            max_requests,
            window,
        }
    }

    async fn check_at(&self, address: IpAddr, now: Instant) -> Result<(), Duration> {
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

        let entries = requests.entry(address).or_default();
        while entries
            .front()
            .is_some_and(|timestamp| now.saturating_duration_since(*timestamp) >= self.window)
        {
            entries.pop_front();
        }

        if entries.len() >= self.max_requests {
            return Err(self
                .window
                .saturating_sub(now.saturating_duration_since(entries[0])));
        }

        entries.push_back(now);
        Ok(())
    }
}

#[derive(Serialize)]
struct RateLimitError {
    error: String,
}

fn retry_after_seconds(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
        .max(1)
}

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

pub async fn middleware(
    State(limiter): State<RateLimiter>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let client_ip = client_ip(address.ip(), request.headers());
    if let Err(retry_after) = limiter.check_at(client_ip, Instant::now()).await {
        let retry_after = retry_after_seconds(retry_after);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after.to_string())],
            Json(RateLimitError {
                error: "Too many requests. Try again later.".into(),
            }),
        )
            .into_response();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP_ONE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
    const IP_TWO: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2));

    #[tokio::test]
    async fn strict_and_general_thresholds_are_independent() {
        let strict = RateLimiter::new(5, Duration::from_secs(60));
        let general = RateLimiter::new(10, Duration::from_secs(60));
        let now = Instant::now();

        for _ in 0..5 {
            assert!(strict.check_at(IP_ONE, now).await.is_ok());
        }
        assert!(strict.check_at(IP_ONE, now).await.is_err());

        for _ in 0..10 {
            assert!(general.check_at(IP_ONE, now).await.is_ok());
        }
        assert!(general.check_at(IP_ONE, now).await.is_err());
    }

    #[tokio::test]
    async fn requests_are_counted_separately_per_ip() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();

        assert!(limiter.check_at(IP_ONE, now).await.is_ok());
        assert!(limiter.check_at(IP_ONE, now).await.is_err());
        assert!(limiter.check_at(IP_TWO, now).await.is_ok());
    }

    #[tokio::test]
    async fn expired_requests_leave_the_sliding_window() {
        let window = Duration::from_secs(60);
        let limiter = RateLimiter::new(1, window);
        let now = Instant::now();

        assert!(limiter.check_at(IP_ONE, now).await.is_ok());
        assert!(limiter.check_at(IP_ONE, now + window).await.is_ok());
    }

    #[tokio::test]
    async fn global_sweep_removes_stale_ip_entries() {
        let window = Duration::from_secs(60);
        let limiter = RateLimiter::new(1, window);
        let now = Instant::now();
        limiter.check_at(IP_ONE, now).await.unwrap();

        limiter.check_at(IP_TWO, now + window).await.unwrap();

        let requests = limiter.requests.lock().await;
        assert!(!requests.contains_key(&IP_ONE));
        assert!(requests.contains_key(&IP_TWO));
    }

    #[tokio::test]
    async fn retry_after_tracks_oldest_request_and_rounds_up() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        limiter.check_at(IP_ONE, now).await.unwrap();

        let remaining = limiter
            .check_at(IP_ONE, now + Duration::from_millis(10_500))
            .await
            .unwrap_err();
        assert_eq!(remaining, Duration::from_millis(49_500));
        assert_eq!(retry_after_seconds(remaining), 50);
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
