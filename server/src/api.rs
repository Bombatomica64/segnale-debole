//! HTTP API (`/api/v1/*`).

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Query, Request, State, rejection::BytesRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    config::Config,
    merge::validate_profile_json,
    oidc::{OidcClient, PendingLogins, pkce_challenge, random_token},
    ratelimit::RateLimiter,
    store::{Player, Store, Submitter, TokenHash},
    validate::{
        DailySubmission, TrialSubmission, parse_date, sanitize_callsign, today_utc, valid_callsign, validate_daily,
        validate_trial,
    },
};

pub const SESSION_COOKIE: &str = "segnale_session";
pub const OIDC_NONCE_COOKIE: &str = "segnale_oidc_nonce";
pub const OIDC_NONCE_TTL_SECS: i64 = 600;
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;
pub const LEADERBOARD_LIMIT: u32 = 25;
pub const SCORE_BODY_LIMIT: usize = 4 * 1024;
pub const PROFILE_BODY_LIMIT: usize = 64 * 1024;
pub const RATE_LIMIT: usize = 20;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<dyn Store>,
    /// `None` = auth disabled.
    pub oidc: Option<Arc<dyn OidcClient>>,
    pub pending: Arc<PendingLogins>,
    pub limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn new(config: Config, store: Arc<dyn Store>, oidc: Option<Arc<dyn OidcClient>>) -> Self {
        AppState {
            config: Arc::new(config),
            store,
            oidc,
            pending: Arc::new(PendingLogins::default()),
            limiter: Arc::new(RateLimiter::new(RATE_LIMIT, RATE_WINDOW)),
        }
    }

    fn auth_enabled(&self) -> bool {
        self.oidc.is_some() && self.config.oidc.is_some()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/livez", get(livez))
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/auth/login", get(login))
        .route("/api/v1/auth/callback", get(callback))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/profile", get(get_profile).put(put_profile).layer(DefaultBodyLimit::max(PROFILE_BODY_LIMIT)))
        .route(
            "/api/v1/leaderboard/daily",
            get(get_daily).post(post_daily).layer(DefaultBodyLimit::max(SCORE_BODY_LIMIT)),
        )
        .route(
            "/api/v1/leaderboard/trial",
            get(get_trial).post(post_trial).layer(DefaultBodyLimit::max(SCORE_BODY_LIMIT)),
        )
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not found") })
        .layer(middleware::from_fn_with_state(state.clone(), cors))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError { status, message: message.into() }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "not authenticated")
    }
    fn internal(e: impl std::fmt::Display) -> Self {
        eprintln!("[segnale_server] internal error: {e}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<crate::store::StoreError> for ApiError {
    fn from(e: crate::store::StoreError) -> Self {
        ApiError::internal(e)
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Parse a JSON body, mapping body/size/parse failures to JSON errors
/// (413 for an over-limit body, 400 for malformed JSON).
fn json_body<T: DeserializeOwned>(body: Result<Bytes, BytesRejection>) -> ApiResult<T> {
    let bytes = body.map_err(|r| {
        let status = r.status();
        let msg =
            if status == StatusCode::PAYLOAD_TOO_LARGE { "request body too large".to_string() } else { r.body_text() };
        ApiError::new(status, msg)
    })?;
    serde_json::from_slice(&bytes).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

// ---------------------------------------------------------------------------
// CORS + origin trust
// ---------------------------------------------------------------------------

/// First-party origins: echoed with credentials; cookies count as auth.
pub fn is_trusted_origin(origin: &str, public_base_url: &str) -> bool {
    if origin == "https://signal.bombatomica64.dev" || origin == public_base_url {
        return true;
    }
    ["http://localhost", "http://127.0.0.1"].iter().any(|base| {
        origin.strip_prefix(base).is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix(':')
                    .is_some_and(|port| !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}

/// Cookies are only honored without an `Origin` (same-origin GET / native
/// clients) or from a trusted origin, so third-party sites act as guests.
fn cookies_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).map(HeaderValue::to_str) {
        None => true,
        Some(Ok(o)) => is_trusted_origin(o, &state.config.public_base_url),
        Some(Err(_)) => false,
    }
}

async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let origin = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()).map(str::to_string);
    let trusted = origin.as_deref().is_some_and(|o| is_trusted_origin(o, &state.config.public_base_url));
    let preflight = req.method() == Method::OPTIONS;
    let mut resp = if preflight { StatusCode::NO_CONTENT.into_response() } else { next.run(req).await };
    let h = resp.headers_mut();
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    if let Some(origin) = &origin {
        if trusted {
            if let Ok(v) = HeaderValue::from_str(origin) {
                h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
                h.insert(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
            }
        } else {
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        }
    }
    if preflight {
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, PUT, OPTIONS"));
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Content-Type, Authorization"));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    }
    resp
}

// ---------------------------------------------------------------------------
// Client IP + sessions
// ---------------------------------------------------------------------------

/// Canonical client IP overwritten by nginx (`X-Client-IP`), else socket peer
/// address (when served with `ConnectInfo`), else `"unknown"`. Never trusts
/// caller-supplied `X-Forwarded-For` lists directly.
pub struct ClientIp(pub String);

pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    headers
        .get("x-client-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| peer.map(|p| p.ip().to_string()))
        .unwrap_or_else(|| "unknown".into())
}

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        Ok(ClientIp(client_ip(&parts.headers, peer)))
    }
}

pub fn hash_token(token: &str) -> Option<TokenHash> {
    let raw = URL_SAFE_NO_PAD.decode(token.trim()).ok()?;
    (raw.len() == 32).then(|| Sha256::digest(&raw).into())
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// Session token hash from `Authorization: Bearer` or (trusted origins only)
/// the session cookie.
fn session_hash(state: &AppState, headers: &HeaderMap) -> Option<TokenHash> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")));
    if let Some(t) = bearer {
        return hash_token(t);
    }
    if cookies_allowed(state, headers) {
        return cookie_value(headers, SESSION_COOKIE).and_then(hash_token);
    }
    None
}

async fn current_player(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<Player>> {
    match session_hash(state, headers) {
        Some(h) => Ok(state.store.session_player(&h, Utc::now()).await?),
        None => Ok(None),
    }
}

fn session_cookie(state: &AppState, value: &str, max_age: i64) -> HeaderValue {
    let secure = if state.config.cookie_secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie header is ASCII")
}

fn oidc_nonce_cookie(state: &AppState, value: &str, max_age: i64) -> HeaderValue {
    let secure = if state.config.cookie_secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{OIDC_NONCE_COOKIE}={value}; Path=/api/v1/auth/callback; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie header is ASCII")
}

fn redirect(location: &str) -> Response {
    let mut resp = StatusCode::FOUND.into_response();
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn rfc3339(t: chrono::DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Process-only liveness probe (does not query Postgres so DB outages do not
/// restart the API pod and discard pending logins).
async fn livez() -> Response {
    Json(json!({ "status": "ok" })).into_response()
}

async fn healthz(State(state): State<AppState>) -> Response {
    match state.store.health_check().await {
        Ok(()) => Json(json!({ "status": "ok" })).into_response(),
        Err(e) => {
            eprintln!("[segnale_server] health check failed: {e}");
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "unavailable" }))).into_response()
        }
    }
}

fn auth_disabled() -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "authentication is not configured")
}

async fn login(State(state): State<AppState>) -> ApiResult<Response> {
    let (Some(_), Some(settings)) = (&state.oidc, &state.config.oidc) else {
        return Err(auth_disabled());
    };
    let verifier = random_token(32);
    let csrf = random_token(24);
    let nonce = random_token(32);
    let nonce_hash = hash_token(&nonce).expect("fresh nonce decodes");
    let challenge = pkce_challenge(&verifier);
    let redirect_uri = state.config.redirect_uri();
    let url = reqwest::Url::parse_with_params(
        &settings.authorize_url,
        &[
            ("response_type", "code"),
            ("client_id", settings.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", "openid profile email"),
            ("state", csrf.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ],
    )
    .map_err(ApiError::internal)?;
    state.pending.insert(csrf, verifier, nonce_hash, Instant::now());
    let mut resp = redirect(url.as_str());
    resp.headers_mut().append(header::SET_COOKIE, oidc_nonce_cookie(&state, &nonce, OIDC_NONCE_TTL_SECS));
    Ok(resp)
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> ApiResult<Response> {
    let Some(oidc) = state.oidc.clone() else {
        return Err(auth_disabled());
    };
    if let Some(err) = q.error {
        return Err(ApiError::bad_request(format!("login failed: {err}")));
    }
    let (Some(code), Some(csrf)) = (q.code, q.state) else {
        return Err(ApiError::bad_request("missing code or state"));
    };
    let binding_hash = cookie_value(&headers, OIDC_NONCE_COOKIE)
        .and_then(hash_token)
        .ok_or_else(|| ApiError::bad_request("missing or invalid login binding cookie"))?;
    let verifier = state
        .pending
        .take(&csrf, &binding_hash, Instant::now())
        .ok_or_else(|| ApiError::bad_request("unknown, expired, or mismatched login state"))?;
    let identity = oidc.exchange_code(&code, &verifier, &state.config.redirect_uri()).await.map_err(|e| {
        eprintln!("[segnale_server] OIDC exchange failed: {e}");
        ApiError::new(StatusCode::BAD_GATEWAY, "identity provider exchange failed")
    })?;
    let player =
        state.store.upsert_player(&identity.sub, &identity.username, &sanitize_callsign(&identity.username)).await?;
    let token = random_token(32);
    let hash = hash_token(&token).expect("fresh token decodes");
    let expires = Utc::now() + chrono::Duration::seconds(SESSION_TTL_SECS);
    state.store.create_session(&hash, &player.sub, expires).await?;
    let mut resp = redirect("/");
    resp.headers_mut().append(header::SET_COOKIE, session_cookie(&state, &token, SESSION_TTL_SECS));
    resp.headers_mut().append(header::SET_COOKIE, oidc_nonce_cookie(&state, "", 0));
    Ok(resp)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(h) = session_hash(&state, &headers) {
        state.store.delete_session(&h).await?;
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, session_cookie(&state, "", 0));
    Ok(resp)
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let auth_enabled = state.auth_enabled();
    Ok(Json(match current_player(&state, &headers).await? {
        Some(p) => json!({
            "authenticated": true,
            "auth_enabled": auth_enabled,
            "sub": p.sub,
            "username": p.username,
            "callsign": p.callsign,
            "profile": p.profile,
            "profile_updated_at": rfc3339(p.profile_updated_at),
        }),
        None => json!({ "authenticated": false, "auth_enabled": auth_enabled }),
    }))
}

fn profile_json(p: &Player) -> Json<Value> {
    Json(json!({ "callsign": p.callsign, "profile": p.profile, "updated_at": rfc3339(p.profile_updated_at) }))
}

async fn get_profile(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let p = current_player(&state, &headers).await?.ok_or_else(ApiError::unauthorized)?;
    Ok(profile_json(&p))
}

#[derive(Deserialize)]
struct ProfilePut {
    callsign: Option<String>,
    profile: Option<Value>,
}

async fn put_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<Json<Value>> {
    let p = current_player(&state, &headers).await?.ok_or_else(ApiError::unauthorized)?;
    let req: ProfilePut = json_body(body)?;
    if let Some(c) = &req.callsign
        && !valid_callsign(c)
    {
        return Err(ApiError::bad_request("callsign must match ^[A-Z0-9_-]{2,12}$"));
    }
    let incoming = match req.profile {
        None | Some(Value::Null) => None,
        Some(v) => {
            validate_profile_json(&v).map_err(ApiError::bad_request)?;
            Some(v)
        }
    };
    let updated = state
        .store
        .update_profile(&p.sub, req.callsign.as_deref(), incoming.as_ref())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(profile_json(&updated))
}

#[derive(Deserialize)]
struct DailyQuery {
    date: Option<String>,
}

async fn get_daily(State(state): State<AppState>, Query(q): Query<DailyQuery>) -> ApiResult<Json<Value>> {
    let date = match q.date {
        Some(d) => {
            parse_date(&d).ok_or_else(|| ApiError::bad_request("date must be a real YYYY-MM-DD date"))?;
            d
        }
        None => today_utc().format("%Y-%m-%d").to_string(),
    };
    let entries = state.store.daily_top(&date, LEADERBOARD_LIMIT).await?;
    Ok(Json(json!({ "date": date, "entries": entries })))
}

async fn get_trial(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let entries = state.store.trial_top(LEADERBOARD_LIMIT).await?;
    Ok(Json(json!({ "entries": entries })))
}

fn rate_limit(state: &AppState, ip: &ClientIp) -> ApiResult<()> {
    if state.limiter.check(&ip.0, Instant::now()) {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many submissions, slow down"))
    }
}

async fn submitter(state: &AppState, headers: &HeaderMap, guest_callsign: &str) -> ApiResult<Submitter> {
    Ok(match current_player(state, headers).await? {
        Some(p) => {
            Submitter { player_key: format!("auth:{}", p.sub), sub: Some(p.sub), callsign: p.callsign, verified: true }
        }
        None => Submitter {
            player_key: format!("guest:{guest_callsign}"),
            sub: None,
            callsign: guest_callsign.to_string(),
            verified: false,
        },
    })
}

async fn post_daily(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<Json<Value>> {
    rate_limit(&state, &ip)?;
    let sub: DailySubmission = json_body(body)?;
    let (date, run) = validate_daily(&sub, today_utc()).map_err(ApiError::bad_request)?;
    let who = submitter(&state, &headers, &sub.callsign).await?;
    let out = state.store.submit_daily(&date, &who, &run).await?;
    Ok(Json(json!({
        "accepted": true,
        "improved": out.improved,
        "rank": out.entry.rank,
        "verified": who.verified,
        "entry": out.entry,
    })))
}

async fn post_trial(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<Json<Value>> {
    rate_limit(&state, &ip)?;
    let sub: TrialSubmission = json_body(body)?;
    let run = validate_trial(&sub).map_err(ApiError::bad_request)?;
    let who = submitter(&state, &headers, &sub.callsign).await?;
    let out = state.store.submit_trial(&who, &run).await?;
    Ok(Json(json!({
        "accepted": true,
        "improved": out.improved,
        "rank": out.entry.rank,
        "verified": who.verified,
        "entry": out.entry,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_origins() {
        let base = "https://signal.bombatomica64.dev";
        assert!(is_trusted_origin("https://signal.bombatomica64.dev", base));
        assert!(is_trusted_origin("http://localhost", base));
        assert!(is_trusted_origin("http://localhost:8080", base));
        assert!(is_trusted_origin("http://127.0.0.1:3000", base));
        assert!(!is_trusted_origin("http://localhost.evil.com", base));
        assert!(!is_trusted_origin("http://localhost:", base));
        assert!(!is_trusted_origin("https://localhost:8080", base));
        assert!(!is_trusted_origin("https://evil.example", base));
        assert!(!is_trusted_origin("http://signal.bombatomica64.dev", base));
    }

    #[test]
    fn client_ip_precedence() {
        let peer: SocketAddr = "10.0.0.9:5555".parse().unwrap();
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h, Some(peer)), "10.0.0.9");
        assert_eq!(client_ip(&h, None), "unknown");
        // Untrusted caller-supplied forwarding headers are ignored.
        h.insert("x-forwarded-for", HeaderValue::from_static("1.1.1.1, 10.0.0.1"));
        h.insert("cf-connecting-ip", HeaderValue::from_static("2.2.2.2"));
        assert_eq!(client_ip(&h, Some(peer)), "10.0.0.9");
        // Canonical X-Client-IP set by nginx takes precedence.
        h.insert("x-client-ip", HeaderValue::from_static("3.3.3.3"));
        assert_eq!(client_ip(&h, Some(peer)), "3.3.3.3");
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; segnale_session=tok; b=2"));
        assert_eq!(cookie_value(&h, SESSION_COOKIE), Some("tok"));
        assert_eq!(cookie_value(&h, "c"), None);
    }
}
