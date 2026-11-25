//! Environment configuration.

pub const DEFAULT_BIND: &str = "0.0.0.0:8081";
pub const DEFAULT_PUBLIC_BASE_URL: &str = "https://signal.bombatomica64.dev";
pub const DEFAULT_AUTHORIZE_URL: &str = "https://auth.bombatomica64.dev/application/o/authorize/";
pub const DEFAULT_TOKEN_URL: &str = "https://auth.bombatomica64.dev/application/o/token/";
pub const DEFAULT_USERINFO_URL: &str = "https://auth.bombatomica64.dev/application/o/userinfo/";

#[derive(Clone, Debug)]
pub struct OidcSettings {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: Option<String>,
    /// Must be explicitly enabled (`ALLOW_MEMORY_STORE=1`) for local dev without
    /// Postgres so a missing `DATABASE_URL` in production fails fast.
    pub allow_memory_store: bool,
    pub bind: String,
    /// No trailing slash.
    pub public_base_url: String,
    pub cookie_secure: bool,
    /// `None` when `OIDC_CLIENT_ID` is unset: login disabled, leaderboards still work.
    pub oidc: Option<OidcSettings>,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let get = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let flag = |k: &str, default: bool| {
            get(k)
                .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
                .unwrap_or(default)
        };
        let oidc = get("OIDC_CLIENT_ID").map(|client_id| OidcSettings {
            client_id,
            client_secret: get("OIDC_CLIENT_SECRET"),
            authorize_url: get("OIDC_AUTHORIZE_URL").unwrap_or_else(|| DEFAULT_AUTHORIZE_URL.into()),
            token_url: get("OIDC_TOKEN_URL").unwrap_or_else(|| DEFAULT_TOKEN_URL.into()),
            userinfo_url: get("OIDC_USERINFO_URL").unwrap_or_else(|| DEFAULT_USERINFO_URL.into()),
        });
        Config {
            database_url: get("DATABASE_URL"),
            allow_memory_store: flag("ALLOW_MEMORY_STORE", false),
            bind: get("SERVER_BIND").unwrap_or_else(|| DEFAULT_BIND.into()),
            public_base_url: get("PUBLIC_BASE_URL")
                .unwrap_or_else(|| DEFAULT_PUBLIC_BASE_URL.into())
                .trim_end_matches('/')
                .to_string(),
            cookie_secure: flag("COOKIE_SECURE", true),
            oidc,
        }
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/api/v1/auth/callback", self.public_base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = Config::from_lookup(|_| None);
        assert_eq!(c.bind, DEFAULT_BIND);
        assert!(c.cookie_secure);
        assert!(c.oidc.is_none());
        assert!(c.database_url.is_none());
        assert_eq!(c.redirect_uri(), "https://signal.bombatomica64.dev/api/v1/auth/callback");

        let c = Config::from_lookup(|k| match k {
            "OIDC_CLIENT_ID" => Some("segnale-debole".into()),
            "COOKIE_SECURE" => Some("false".into()),
            "PUBLIC_BASE_URL" => Some("http://localhost:8080/".into()),
            _ => None,
        });
        assert!(!c.cookie_secure);
        let o = c.oidc.as_ref().unwrap();
        assert_eq!(o.token_url, DEFAULT_TOKEN_URL);
        assert!(o.client_secret.is_none());
        assert_eq!(c.redirect_uri(), "http://localhost:8080/api/v1/auth/callback");
    }
}
