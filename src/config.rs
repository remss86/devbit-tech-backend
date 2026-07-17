use crate::auth;
use std::{env, error::Error, fmt, net::SocketAddr};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:7878";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub is_production: bool,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let environment = env::var("NODE_ENV").ok();
        let jwt_secret = env::var("JWT_SECRET").ok();
        let bind_address = env::var("BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.into());

        Self::from_values(
            environment.as_deref(),
            jwt_secret.as_deref(),
            &bind_address,
            cfg!(not(debug_assertions)),
        )
    }

    fn from_values(
        environment: Option<&str>,
        jwt_secret: Option<&str>,
        bind_address: &str,
        release_build: bool,
    ) -> Result<Self, ConfigError> {
        auth::validate_production_secret(environment, jwt_secret, release_build)
            .map_err(ConfigError::new)?;
        let bind_address = bind_address.trim().parse::<SocketAddr>().map_err(|error| {
            ConfigError::new(format!("invalid BIND_ADDR {bind_address:?}: {error}"))
        })?;

        Ok(Self {
            bind_address,
            is_production: auth::is_production_environment(environment) || release_build,
        })
    }
}

#[derive(Debug)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_uses_the_default_bind_address_without_a_secret() {
        let config = AppConfig::from_values(None, None, DEFAULT_BIND_ADDRESS, false).unwrap();

        assert_eq!(config.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert!(!config.is_production);
    }

    #[test]
    fn production_requires_a_non_default_secret() {
        assert!(
            AppConfig::from_values(Some("production"), None, DEFAULT_BIND_ADDRESS, false).is_err()
        );
        assert!(
            AppConfig::from_values(
                Some("production"),
                Some(auth::DEFAULT_JWT_SECRET),
                DEFAULT_BIND_ADDRESS,
                false,
            )
            .is_err()
        );
        assert!(
            AppConfig::from_values(
                Some("production"),
                Some("deployment-specific-secret"),
                DEFAULT_BIND_ADDRESS,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn invalid_bind_address_is_rejected_before_startup() {
        assert!(AppConfig::from_values(None, None, "localhost", false).is_err());
    }

    #[test]
    fn release_build_requires_a_non_default_secret_without_node_env() {
        assert!(AppConfig::from_values(None, None, DEFAULT_BIND_ADDRESS, true).is_err());
        let config = AppConfig::from_values(
            None,
            Some("deployment-specific-secret"),
            DEFAULT_BIND_ADDRESS,
            true,
        )
        .unwrap();

        assert!(config.is_production);
    }
}
