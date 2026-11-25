//! OIDC (Authentik) authorization-code + PKCE login.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::OidcSettings;

pub const PENDING_TTL: Duration = Duration::from_secs(600);
pub const PENDING_MAX: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OidcIdentity {
    pub sub: String,
    pub username: String,
}

/// Token + userinfo exchange; a trait so tests can inject a fake provider.
#[async_trait]
pub trait OidcClient: Send + Sync {
    async fn exchange_code(&self, code: &str, code_verifier: &str, redirect_uri: &str) -> Result<OidcIdentity, String>;
}

/// `count` random bytes, base64url without padding.
pub fn random_token(count: usize) -> String {
    let mut buf = vec![0u8; count];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// PKCE S256 challenge of a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

struct PendingEntry {
    verifier: String,
    binding_hash: [u8; 32],
    created: Instant,
}

/// Bounded map of in-flight logins: `state` -> [`PendingEntry`].
pub struct PendingLogins {
    ttl: Duration,
    cap: usize,
    inner: Mutex<HashMap<String, PendingEntry>>,
}

impl Default for PendingLogins {
    fn default() -> Self {
        Self::new(PENDING_TTL, PENDING_MAX)
    }
}

impl PendingLogins {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        PendingLogins { ttl, cap: cap.max(1), inner: Mutex::new(HashMap::new()) }
    }

    pub fn insert(&self, state: String, verifier: String, binding_hash: [u8; 32], now: Instant) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let ttl = self.ttl;
        g.retain(|_, e| now.saturating_duration_since(e.created) < ttl);
        while g.len() >= self.cap {
            let oldest = g.iter().min_by_key(|(_, e)| e.created).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => g.remove(&k),
                None => break,
            };
        }
        g.insert(state, PendingEntry { verifier, binding_hash, created: now });
    }

    /// Consume a state only when it exists, has not expired, and its browser
    /// binding cookie hash matches `binding_hash` (a mismatch leaves the real
    /// browser's pending login intact).
    pub fn take(&self, state: &str, binding_hash: &[u8; 32], now: Instant) -> Option<String> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let entry = g.get(state)?;
        if now.saturating_duration_since(entry.created) >= self.ttl {
            g.remove(state);
            return None;
        }
        // Constant-time-style full-slice fold over 32-byte SHA-256 digest.
        let mut diff = 0u8;
        for (a, b) in entry.binding_hash.iter().zip(binding_hash.iter()) {
            diff |= a ^ b;
        }
        if diff != 0 {
            return None;
        }
        g.remove(state).map(|e| e.verifier)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Real provider over HTTP (in-cluster token/userinfo URLs).
pub struct HttpOidcClient {
    http: reqwest::Client,
    settings: OidcSettings,
}

impl HttpOidcClient {
    pub fn new(settings: OidcSettings) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("reqwest client");
        HttpOidcClient { http, settings }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    preferred_username: Option<String>,
    name: Option<String>,
}

/// `preferred_username`, then `name`, then `sub`.
pub fn pick_username(sub: &str, preferred: Option<&str>, name: Option<&str>) -> String {
    [preferred, name].into_iter().flatten().map(str::trim).find(|s| !s.is_empty()).unwrap_or(sub).to_string()
}

#[async_trait]
impl OidcClient for HttpOidcClient {
    async fn exchange_code(&self, code: &str, code_verifier: &str, redirect_uri: &str) -> Result<OidcIdentity, String> {
        let s = &self.settings;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", s.client_id.as_str()),
            ("code_verifier", code_verifier),
        ];
        if let Some(secret) = &s.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        let resp =
            self.http.post(&s.token_url).form(&form).send().await.map_err(|e| format!("token request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("token endpoint returned {status}: {}", body.chars().take(300).collect::<String>()));
        }
        let token: TokenResponse = resp.json().await.map_err(|e| format!("bad token response: {e}"))?;
        let resp = self
            .http
            .get(&s.userinfo_url)
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|e| format!("userinfo request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("userinfo endpoint returned {}", resp.status()));
        }
        let info: UserInfo = resp.json().await.map_err(|e| format!("bad userinfo response: {e}"))?;
        if info.sub.trim().is_empty() {
            return Err("userinfo without sub".into());
        }
        let username = pick_username(&info.sub, info.preferred_username.as_deref(), info.name.as_deref());
        Ok(OidcIdentity { sub: info.sub, username })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_token(32).len(), 43);
    }

    #[test]
    fn pending_ttl_single_use_binding_and_cap() {
        let p = PendingLogins::new(Duration::from_secs(600), 3);
        let t0 = Instant::now();
        let h1 = [1u8; 32];
        let h2 = [2u8; 32];
        p.insert("a".into(), "va".into(), h1, t0);
        assert_eq!(p.take("a", &h2, t0), None, "wrong browser binding must not consume");
        assert_eq!(p.take("a", &h1, t0), Some("va".into()));
        assert_eq!(p.take("a", &h1, t0), None, "single use");
        p.insert("b".into(), "vb".into(), h1, t0);
        assert_eq!(p.take("b", &h1, t0 + Duration::from_secs(601)), None, "expired");
        p.insert("c1".into(), "1".into(), h1, t0);
        p.insert("c2".into(), "2".into(), h1, t0 + Duration::from_secs(1));
        p.insert("c3".into(), "3".into(), h1, t0 + Duration::from_secs(2));
        p.insert("c4".into(), "4".into(), h1, t0 + Duration::from_secs(3));
        assert_eq!(p.len(), 3);
        assert_eq!(p.take("c1", &h1, t0 + Duration::from_secs(3)), None, "oldest evicted");
        assert_eq!(p.take("c4", &h1, t0 + Duration::from_secs(3)), Some("4".into()));
    }

    #[test]
    fn username_fallbacks() {
        assert_eq!(pick_username("s", Some("ana"), Some("Ana B")), "ana");
        assert_eq!(pick_username("s", Some(" "), Some("Ana B")), "Ana B");
        assert_eq!(pick_username("s", None, None), "s");
    }
}
