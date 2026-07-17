use axum::http::{HeaderMap, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{
    DecodingKey, EncodingKey, Header, Validation, decode, encode, errors::Error as JwtError,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::env;

type HmacSha256 = Hmac<Sha256>;

pub const AUTH_COOKIE_NAME: &str = "auth_token";
pub const DEFAULT_JWT_SECRET: &str = "devbit-local-secret";
const PASSWORD_HASH_PREFIX: &str = "pbkdf2-sha256";
const PASSWORD_HASH_ITERATIONS: u32 = 100_000;
const MAX_PASSWORD_HASH_ITERATIONS: u32 = 1_000_000;
pub const MAX_PASSWORD_BYTES: usize = 1_024;
const PASSWORD_SALT_BYTES: usize = 16;
const PASSWORD_HASH_BYTES: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    exp: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthIdentity {
    pub user_id: i32,
    expires_at: usize,
}

pub fn generate_token(user_id: i32, email: &str) -> Result<String, JwtError> {
    let expiration = Utc::now()
        .checked_add_signed(Duration::hours(24))
        .expect("24 hours is a valid timestamp offset")
        .timestamp() as usize;
    let claims = Claims {
        sub: user_id,
        email: Some(email.to_string()),
        exp: expiration,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    )
}

pub fn user_id_from_token(token: &str) -> Option<i32> {
    identity_from_token(token).map(|identity| identity.user_id)
}

pub fn identity_from_token(token: &str) -> Option<AuthIdentity> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(jwt_secret().as_bytes()),
        &Validation::default(),
    )
    .ok()
    .map(|data| AuthIdentity {
        user_id: data.claims.sub,
        expires_at: data.claims.exp,
    })
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(token.to_string());
    }

    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix(&format!("{AUTH_COOKIE_NAME}="))
                    .filter(|token| !token.is_empty())
                    .map(str::to_string)
            })
        })
}

pub fn user_id_from_headers(headers: &HeaderMap) -> Option<i32> {
    identity_from_headers(headers).map(|identity| identity.user_id)
}

pub fn identity_from_headers(headers: &HeaderMap) -> Option<AuthIdentity> {
    token_from_headers(headers).and_then(|token| identity_from_token(&token))
}

pub fn remaining_token_lifetime(identity: AuthIdentity) -> std::time::Duration {
    let now = usize::try_from(Utc::now().timestamp()).unwrap_or(0);
    let seconds = u64::try_from(identity.expires_at.saturating_sub(now)).unwrap_or(u64::MAX);
    std::time::Duration::from_secs(seconds)
}

pub fn auth_cookie(token: &str) -> String {
    format!("{AUTH_COOKIE_NAME}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=86400")
}

pub fn expired_auth_cookie() -> String {
    format!(
        "{AUTH_COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
    )
}

pub fn is_admin_user(user_id: i32) -> bool {
    user_id == 1 || user_id == 2
}

pub fn is_production_environment(environment: Option<&str>) -> bool {
    environment.is_some_and(|value| value.trim().eq_ignore_ascii_case("production"))
}

pub fn validate_production_secret(
    environment: Option<&str>,
    secret: Option<&str>,
    release_build: bool,
) -> Result<(), &'static str> {
    if !is_production_environment(environment) && !release_build {
        return Ok(());
    }

    let secret = secret.map(str::trim).filter(|value| !value.is_empty());
    if secret.is_none() || secret == Some(DEFAULT_JWT_SECRET) {
        return Err(
            "JWT_SECRET must be set to a non-default value in production and release builds",
        );
    }

    Ok(())
}

pub fn should_expose_development_code() -> bool {
    development_code_is_allowed(env::var("NODE_ENV").ok().as_deref(), cfg!(debug_assertions))
}

fn development_code_is_allowed(environment: Option<&str>, debug_build: bool) -> bool {
    match environment.map(str::trim) {
        Some(value) => {
            value.eq_ignore_ascii_case("development") || value.eq_ignore_ascii_case("test")
        }
        None => debug_build,
    }
}

pub fn hash_password(password: &str) -> String {
    let salt: [u8; PASSWORD_SALT_BYTES] = rand::random();
    let mut output = [0_u8; PASSWORD_HASH_BYTES];
    pbkdf2_hmac_sha256(
        password.as_bytes(),
        &salt,
        PASSWORD_HASH_ITERATIONS,
        &mut output,
    );

    format!(
        "{PASSWORD_HASH_PREFIX}${PASSWORD_HASH_ITERATIONS}${}${}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(output)
    )
}

pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    let parts: Vec<&str> = stored_hash.split('$').collect();
    if parts.first() != Some(&PASSWORD_HASH_PREFIX) {
        return password == stored_hash;
    }
    if parts.len() != 4 {
        return false;
    }

    let iterations = match parts[1].parse::<u32>() {
        Ok(value) if value > 0 && value <= MAX_PASSWORD_HASH_ITERATIONS => value,
        Err(_) => return false,
        Ok(_) => return false,
    };
    let salt = match URL_SAFE_NO_PAD.decode(parts[2]) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let expected = match URL_SAFE_NO_PAD.decode(parts[3]) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if salt.len() != PASSWORD_SALT_BYTES || expected.len() != PASSWORD_HASH_BYTES {
        return false;
    }
    let mut actual = vec![0_u8; expected.len()];
    pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations, &mut actual);
    constant_time_eq(&actual, &expected)
}

pub fn password_needs_upgrade(stored_hash: &str) -> bool {
    !stored_hash.starts_with(PASSWORD_HASH_PREFIX)
}

fn jwt_secret() -> String {
    env::var("JWT_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_JWT_SECRET.to_string())
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, output: &mut [u8]) {
    // Initializing HMAC normalizes long keys. Reuse that keyed state so the
    // password length is not multiplied by the PBKDF2 iteration count.
    let keyed_mac = HmacSha256::new_from_slice(password).expect("HMAC accepts any key length");
    for (block_index, chunk) in output.chunks_mut(32).enumerate() {
        let block_number = (block_index as u32 + 1).to_be_bytes();
        let mut mac = keyed_mac.clone();
        mac.update(salt);
        mac.update(&block_number);
        let mut u = mac.finalize().into_bytes().to_vec();
        let mut result = u.clone();

        for _ in 1..iterations {
            let mut mac = keyed_mac.clone();
            mac.update(&u);
            u = mac.finalize().into_bytes().to_vec();
            for (target, value) in result.iter_mut().zip(u.iter()) {
                *target ^= value;
            }
        }

        chunk.copy_from_slice(&result[..chunk.len()]);
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_round_trip_and_reject_wrong_values() {
        let hash = hash_password("correct horse battery staple");

        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!password_needs_upgrade(&hash));
        assert!(password_needs_upgrade("legacy plaintext"));
    }

    #[test]
    fn bearer_header_takes_precedence_over_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer header-token".parse().unwrap(),
        );
        headers.insert(
            header::COOKIE,
            "theme=dark; auth_token=cookie-token".parse().unwrap(),
        );

        assert_eq!(
            token_from_headers(&headers).as_deref(),
            Some("header-token")
        );
    }

    #[test]
    fn token_is_read_from_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "theme=dark; auth_token=cookie-token; locale=en"
                .parse()
                .unwrap(),
        );

        assert_eq!(
            token_from_headers(&headers).as_deref(),
            Some("cookie-token")
        );
    }

    #[test]
    fn production_rejects_missing_empty_and_default_secrets() {
        for secret in [None, Some(""), Some("  "), Some(DEFAULT_JWT_SECRET)] {
            assert!(validate_production_secret(Some("production"), secret, false).is_err());
        }

        assert!(
            validate_production_secret(Some("production"), Some("unique-secret"), false).is_ok()
        );
        assert!(validate_production_secret(Some("development"), None, false).is_ok());
        assert!(validate_production_secret(None, None, true).is_err());
        assert!(validate_production_secret(None, Some("unique-secret"), true).is_ok());
    }

    #[test]
    fn development_codes_are_only_allowed_in_safe_local_modes() {
        assert!(development_code_is_allowed(Some("development"), false));
        assert!(development_code_is_allowed(Some("test"), false));
        assert!(development_code_is_allowed(None, true));
        assert!(!development_code_is_allowed(None, false));
        assert!(!development_code_is_allowed(Some("staging"), true));
        assert!(!development_code_is_allowed(Some("production"), true));
    }

    #[test]
    fn malformed_password_hashes_are_rejected_before_derivation() {
        let salt = URL_SAFE_NO_PAD.encode([0_u8; PASSWORD_SALT_BYTES]);
        let digest = URL_SAFE_NO_PAD.encode([0_u8; PASSWORD_HASH_BYTES]);

        assert!(!verify_password(
            "password",
            &format!("{PASSWORD_HASH_PREFIX}$100000${salt}$"),
        ));
        let truncated = format!("{PASSWORD_HASH_PREFIX}$100000${salt}");
        assert!(!verify_password(&truncated, &truncated));
        assert!(!verify_password(
            "password",
            &format!("{PASSWORD_HASH_PREFIX}$0${salt}${digest}"),
        ));
        assert!(!verify_password(
            "password",
            &format!(
                "{PASSWORD_HASH_PREFIX}${}${salt}${digest}",
                MAX_PASSWORD_HASH_ITERATIONS + 1
            ),
        ));
        assert!(!verify_password(
            "password",
            &format!(
                "{PASSWORD_HASH_PREFIX}$100000${}${digest}",
                URL_SAFE_NO_PAD.encode([0_u8; PASSWORD_SALT_BYTES - 1])
            ),
        ));
    }
}
