//! In-process HTTP tests: router + MemoryStore + fake OIDC provider.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use segnale_server::{
    api::{AppState, router},
    config::Config,
    oidc::{OidcClient, OidcIdentity},
    store::{MemoryStore, Store},
};

/// Fake provider: accepts `good-code` and records the verifier it received.
struct FakeOidc {
    sub: String,
    username: String,
    seen: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl OidcClient for FakeOidc {
    async fn exchange_code(&self, code: &str, verifier: &str, redirect_uri: &str) -> Result<OidcIdentity, String> {
        self.seen.lock().unwrap().push((code.into(), verifier.into(), redirect_uri.into()));
        if code == "good-code" {
            Ok(OidcIdentity { sub: self.sub.clone(), username: self.username.clone() })
        } else {
            Err("bad code".into())
        }
    }
}

struct TestApp {
    app: Router,
    store: Arc<MemoryStore>,
    oidc: Arc<FakeOidc>,
}

fn config(auth: bool, cookie_secure: bool) -> Config {
    Config::from_lookup(|k| match k {
        "OIDC_CLIENT_ID" if auth => Some("segnale-debole".into()),
        "OIDC_CLIENT_SECRET" if auth => Some("s3cret".into()),
        "COOKIE_SECURE" => Some(cookie_secure.to_string()),
        _ => None,
    })
}

fn test_app_with(auth: bool, cookie_secure: bool, username: &str) -> TestApp {
    let store = Arc::new(MemoryStore::new());
    let oidc = Arc::new(FakeOidc { sub: "sub-123".into(), username: username.into(), seen: Mutex::new(vec![]) });
    let oidc_dyn: Option<Arc<dyn OidcClient>> = if auth { Some(oidc.clone()) } else { None };
    let state = AppState::new(config(auth, cookie_secure), store.clone(), oidc_dyn);
    TestApp { app: router(state), store, oidc }
}

fn test_app() -> TestApp {
    test_app_with(true, true, "lorenzo.b")
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    json: Value,
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
    Resp { status, headers, json }
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn json_req(method: Method, uri: &str, body: &Value, extra: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri).header(header::CONTENT_TYPE, "application/json");
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn today() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

fn daily(callsign: &str, victory: bool, floor: i64, time: f64) -> Value {
    json!({"callsign": callsign, "date": today(), "victory": victory, "floor": floor, "time_secs": time,
           "peak_speed": 22.5, "max_bhop_chain": 7, "score": 12345})
}

fn trial(callsign: &str, time: f64) -> Value {
    json!({"callsign": callsign, "time_secs": time, "splits": [time * 0.25, time * 0.5, time * 0.75, time],
           "peak_speed": 30.0, "max_bhop_chain": 12})
}

async fn post_daily(app: &Router, body: &Value, ip: &str, extra: &[(&str, &str)]) -> Resp {
    let mut h = vec![("x-client-ip", ip)];
    h.extend_from_slice(extra);
    send(app, json_req(Method::POST, "/api/v1/leaderboard/daily", body, &h)).await
}

async fn post_trial(app: &Router, body: &Value, ip: &str, extra: &[(&str, &str)]) -> Resp {
    let mut h = vec![("x-client-ip", ip)];
    h.extend_from_slice(extra);
    send(app, json_req(Method::POST, "/api/v1/leaderboard/trial", body, &h)).await
}

fn extract_cookie(headers: &HeaderMap, name: &str) -> String {
    let prefix = format!("{name}=");
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.split(';').next()?.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing Set-Cookie {name}"))
        .to_string()
}

/// Runs the full login → callback flow (including browser binding cookie); returns the raw session token.
async fn login(t: &TestApp) -> String {
    let r = send(&t.app, get("/api/v1/auth/login")).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let nonce = extract_cookie(&r.headers, "segnale_oidc_nonce");
    let loc = r.headers[header::LOCATION].to_str().unwrap().to_string();
    let url = reqwest::Url::parse(&loc).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    let req = Request::get(format!("/api/v1/auth/callback?code=good-code&state={state}"))
        .header(header::COOKIE, format!("segnale_oidc_nonce={nonce}"))
        .body(Body::empty())
        .unwrap();
    let r = send(&t.app, req).await;
    assert_eq!(r.status, StatusCode::FOUND);
    extract_cookie(&r.headers, "segnale_session")
}

fn cookie(token: &str) -> String {
    format!("segnale_session={token}")
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn healthz_and_livez_ok() {
    let t = test_app();
    let r = send(&t.app, get("/api/v1/livez")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json, json!({"status": "ok"}));
    let r = send(&t.app, get("/api/v1/healthz")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json, json!({"status": "ok"}));
}

#[tokio::test]
async fn unknown_route_is_json_404() {
    let t = test_app();
    let r = send(&t.app, get("/api/v1/nope")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.json["error"].is_string());
}

#[tokio::test]
async fn guest_daily_submit_and_get() {
    let t = test_app();
    let r = post_daily(&t.app, &daily("GUEST1", true, 5, 420.5), "1.1.1.1", &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["accepted"], true);
    assert_eq!(r.json["improved"], true);
    assert_eq!(r.json["rank"], 1);
    assert_eq!(r.json["verified"], false);
    assert_eq!(
        r.json["entry"],
        json!({"rank": 1, "callsign": "GUEST1", "verified": false, "victory": true, "floor": 5, "time_secs": 420.5,
               "peak_speed": 22.5, "max_bhop_chain": 7, "score": 12345})
    );
    let r = send(&t.app, get("/api/v1/leaderboard/daily")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["date"], today());
    assert_eq!(r.json["entries"].as_array().unwrap().len(), 1);
    let r = send(&t.app, get(&format!("/api/v1/leaderboard/daily?date={}", today()))).await;
    assert_eq!(r.json["entries"][0]["callsign"], "GUEST1");
    let r = send(&t.app, get("/api/v1/leaderboard/daily?date=2020-01-01")).await;
    assert_eq!(r.json, json!({"date": "2020-01-01", "entries": []}));
}

#[tokio::test]
async fn daily_get_rejects_bad_date() {
    let t = test_app();
    for d in ["2026-13-01", "yesterday", "2026-02-30", "2026-1-01"] {
        let r = send(&t.app, get(&format!("/api/v1/leaderboard/daily?date={d}"))).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{d}");
        assert!(r.json["error"].is_string());
    }
}

#[tokio::test]
async fn daily_validation_rejections() {
    let t = test_app();
    let far = (Utc::now().date_naive() + Duration::days(3)).format("%Y-%m-%d").to_string();
    let cases: Vec<(&str, Value)> = vec![
        ("lowercase callsign", json!({"callsign": "abc"})),
        ("short callsign", json!({"callsign": "A"})),
        ("long callsign", json!({"callsign": "ABCDEFGHIJKLM"})),
        ("bad date", json!({"date": "2026-02-30"})),
        ("date shape", json!({"date": "26-02-01"})),
        ("far date", json!({"date": far})),
        ("floor 0", json!({"floor": 0})),
        ("floor 201", json!({"floor": 201})),
        ("time low", json!({"time_secs": 4.9})),
        ("time high", json!({"time_secs": 7200.5})),
        ("quick victory", json!({"victory": true, "time_secs": 14.0, "floor": 5})),
        ("shallow victory", json!({"victory": true, "time_secs": 100.0, "floor": 2})),
        ("speed high", json!({"peak_speed": 65.1})),
        ("speed negative", json!({"peak_speed": -1.0})),
        ("bhop high", json!({"max_bhop_chain": 501})),
        ("bhop negative", json!({"max_bhop_chain": -1})),
        ("score high", json!({"score": 10_000_001})),
        ("score negative", json!({"score": -5})),
        ("float floor", json!({"floor": 2.5})),
        ("missing field", json!({"victory": null})),
    ];
    for (i, (name, patch)) in cases.into_iter().enumerate() {
        let mut body = daily("VALID", false, 3, 100.0);
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        let r = post_daily(&t.app, &body, &format!("9.9.9.{i}"), &[]).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{name}: {}", r.json);
        assert!(r.json["error"].is_string(), "{name}");
    }
    // Malformed JSON.
    let req = Request::post("/api/v1/leaderboard/daily").body(Body::from("{not json")).unwrap();
    let r = send(&t.app, req).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json["error"].is_string());
    // Boundary values accepted.
    let yesterday = (Utc::now().date_naive() - Duration::days(1)).format("%Y-%m-%d").to_string();
    let mut ok = daily("EDGE", true, 3, 15.0);
    ok["date"] = json!(yesterday);
    ok["peak_speed"] = json!(65.0);
    ok["max_bhop_chain"] = json!(500);
    ok["score"] = json!(10_000_000);
    let r = post_daily(&t.app, &ok, "8.8.8.8", &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[tokio::test]
async fn daily_best_only_upsert() {
    let t = test_app();
    let ip = "2.2.2.2";
    let r = post_daily(&t.app, &daily("ACE", false, 4, 300.0), ip, &[]).await;
    assert_eq!(r.json["improved"], true);
    // Worse: lower floor.
    let r = post_daily(&t.app, &daily("ACE", false, 3, 100.0), ip, &[]).await;
    assert_eq!(r.json["improved"], false);
    assert_eq!(r.json["entry"]["floor"], 4);
    assert_eq!(r.json["entry"]["time_secs"], 300.0);
    // Same floor but slower: worse. Equal: not strictly better.
    let r = post_daily(&t.app, &daily("ACE", false, 4, 301.0), ip, &[]).await;
    assert_eq!(r.json["improved"], false);
    let r = post_daily(&t.app, &daily("ACE", false, 4, 300.0), ip, &[]).await;
    assert_eq!(r.json["improved"], false);
    // Same floor, faster: better.
    let r = post_daily(&t.app, &daily("ACE", false, 4, 250.0), ip, &[]).await;
    assert_eq!(r.json["improved"], true);
    assert_eq!(r.json["entry"]["time_secs"], 250.0);
    // Victory beats any non-victory even if slower.
    let r = post_daily(&t.app, &daily("ACE", true, 3, 900.0), ip, &[]).await;
    assert_eq!(r.json["improved"], true);
    // Non-victory deeper floor does not beat a victory.
    let r = post_daily(&t.app, &daily("ACE", false, 50, 100.0), ip, &[]).await;
    assert_eq!(r.json["improved"], false);
    assert_eq!(r.json["entry"]["victory"], true);
    let r = send(&t.app, get("/api/v1/leaderboard/daily")).await;
    assert_eq!(r.json["entries"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["entries"][0]["time_secs"], 900.0);
}

#[tokio::test]
async fn daily_ranking_and_ties() {
    let t = test_app();
    post_daily(&t.app, &daily("SLOWWIN", true, 5, 600.0), "3.0.0.1", &[]).await;
    post_daily(&t.app, &daily("FASTWIN", true, 5, 500.0), "3.0.0.2", &[]).await;
    post_daily(&t.app, &daily("DEEPLOSS", false, 9, 100.0), "3.0.0.3", &[]).await;
    post_daily(&t.app, &daily("TIE_A", false, 4, 200.0), "3.0.0.4", &[]).await;
    post_daily(&t.app, &daily("TIE_B", false, 4, 200.0), "3.0.0.5", &[]).await;
    post_daily(&t.app, &daily("SHALLOW", false, 1, 50.0), "3.0.0.6", &[]).await;
    let r = send(&t.app, get("/api/v1/leaderboard/daily")).await;
    let names: Vec<&str> =
        r.json["entries"].as_array().unwrap().iter().map(|e| e["callsign"].as_str().unwrap()).collect();
    assert_eq!(names, ["FASTWIN", "SLOWWIN", "DEEPLOSS", "TIE_A", "TIE_B", "SHALLOW"]);
    let ranks: Vec<u64> = r.json["entries"].as_array().unwrap().iter().map(|e| e["rank"].as_u64().unwrap()).collect();
    assert_eq!(ranks, [1, 2, 3, 4, 5, 6]);
    // The POST rank matches the GET rank (TIE_B resubmits a non-improving run).
    let r = post_daily(&t.app, &daily("TIE_B", false, 4, 200.0), "3.0.0.5", &[]).await;
    assert_eq!(r.json["rank"], 5);
    // A later tie still ranks after the earlier one.
    let r = post_daily(&t.app, &daily("TIE_C", false, 4, 200.0), "3.0.0.7", &[]).await;
    assert_eq!(r.json["rank"], 6);
}

#[tokio::test]
async fn daily_top_25_limit() {
    let t = test_app();
    for i in 0..30 {
        let r = post_daily(&t.app, &daily(&format!("P{i:02}"), false, 3, 100.0 + i as f64), &format!("4.0.0.{i}"), &[])
            .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let r = send(&t.app, get("/api/v1/leaderboard/daily")).await;
    let e = r.json["entries"].as_array().unwrap();
    assert_eq!(e.len(), 25);
    assert_eq!(e[0]["callsign"], "P00");
    assert_eq!(e[24]["rank"], 25);
}

#[tokio::test]
async fn trial_guest_submit_validation_and_best_only() {
    let t = test_app();
    let r = post_trial(&t.app, &trial("RUNNER", 120.0), "5.5.5.5", &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        r.json,
        json!({"accepted": true, "improved": true, "rank": 1, "verified": false,
               "entry": {"rank": 1, "callsign": "RUNNER", "verified": false, "time_secs": 120.0,
                         "splits": [30.0, 60.0, 90.0, 120.0], "peak_speed": 30.0, "max_bhop_chain": 12}})
    );
    let r = post_trial(&t.app, &trial("RUNNER", 130.0), "5.5.5.5", &[]).await;
    assert_eq!(r.json["improved"], false);
    assert_eq!(r.json["entry"]["time_secs"], 120.0);
    let r = post_trial(&t.app, &trial("RUNNER", 120.0), "5.5.5.5", &[]).await;
    assert_eq!(r.json["improved"], false, "equal time is not strictly faster");
    let r = post_trial(&t.app, &trial("RUNNER", 110.0), "5.5.5.5", &[]).await;
    assert_eq!(r.json["improved"], true);
    post_trial(&t.app, &trial("OTHER", 110.0), "5.5.5.6", &[]).await;
    post_trial(&t.app, &trial("FAST", 60.0), "5.5.5.7", &[]).await;
    let r = send(&t.app, get("/api/v1/leaderboard/trial")).await;
    let names: Vec<&str> =
        r.json["entries"].as_array().unwrap().iter().map(|e| e["callsign"].as_str().unwrap()).collect();
    assert_eq!(names, ["FAST", "RUNNER", "OTHER"]);

    let bad: Vec<(&str, Value)> = vec![
        ("callsign", json!({"callsign": "x"})),
        ("time low", json!({"time_secs": 4.0, "splits": [1.0, 2.0, 3.0, 4.0]})),
        ("time high", json!({"time_secs": 3601.0, "splits": [1.0, 2.0, 3.0, 3601.0]})),
        ("3 splits", json!({"splits": [30.0, 60.0, 100.0]})),
        ("5 splits", json!({"splits": [10.0, 30.0, 60.0, 90.0, 100.0]})),
        ("not increasing", json!({"splits": [30.0, 30.0, 60.0, 100.0]})),
        ("negative split", json!({"splits": [-1.0, 30.0, 60.0, 100.0]})),
        ("last mismatch", json!({"splits": [30.0, 60.0, 90.0, 100.3]})),
        ("speed", json!({"peak_speed": 66.0})),
        ("bhop", json!({"max_bhop_chain": 501})),
    ];
    for (i, (name, patch)) in bad.into_iter().enumerate() {
        let mut body = trial("VALID", 100.0);
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        let r = post_trial(&t.app, &body, &format!("6.0.0.{i}"), &[]).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{name}: {}", r.json);
    }
    // Within tolerance is fine.
    let mut ok = trial("TOL", 100.0);
    ok["splits"] = json!([30.0, 60.0, 90.0, 100.2]);
    assert_eq!(post_trial(&t.app, &ok, "6.1.0.0", &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_shared_across_post_endpoints() {
    let t = test_app();
    for i in 0..10 {
        assert_eq!(
            post_daily(&t.app, &daily("RL", false, 3, 100.0), "7.7.7.7", &[]).await.status,
            StatusCode::OK,
            "{i}"
        );
        assert_eq!(post_trial(&t.app, &trial("RL", 100.0), "7.7.7.7", &[]).await.status, StatusCode::OK, "{i}");
    }
    let r = post_daily(&t.app, &daily("RL", false, 3, 100.0), "7.7.7.7", &[]).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.json["error"].is_string());
    assert_eq!(post_trial(&t.app, &trial("RL", 100.0), "7.7.7.7", &[]).await.status, StatusCode::TOO_MANY_REQUESTS);
    // Other IPs are unaffected; spoofed X-Forwarded-For / CF-Connecting-IP headers
    // are ignored when canonical X-Client-IP is absent (they all share "unknown" bucket).
    assert_eq!(post_daily(&t.app, &daily("RL", false, 3, 100.0), "7.7.7.8", &[]).await.status, StatusCode::OK);
    for i in 0..20 {
        let req = json_req(
            Method::POST,
            "/api/v1/leaderboard/trial",
            &trial("XFF", 100.0),
            &[("x-forwarded-for", &format!("8.1.1.{i}")), ("cf-connecting-ip", &format!("9.1.1.{i}"))],
        );
        assert_eq!(send(&t.app, req).await.status, StatusCode::OK);
    }
    let req = json_req(
        Method::POST,
        "/api/v1/leaderboard/trial",
        &trial("XFF", 100.0),
        &[("x-forwarded-for", "8.2.2.2"), ("cf-connecting-ip", "9.2.2.2")],
    );
    assert_eq!(send(&t.app, req).await.status, StatusCode::TOO_MANY_REQUESTS);
    // GETs are never rate limited.
    assert_eq!(send(&t.app, get("/api/v1/leaderboard/trial")).await.status, StatusCode::OK);
}

#[tokio::test]
async fn body_caps_return_413() {
    let t = test_app();
    let mut body = daily("BIG", false, 3, 100.0);
    body["padding"] = json!("x".repeat(5000));
    let r = post_daily(&t.app, &body, "9.0.0.1", &[]).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(r.json["error"].is_string());

    let token = login(&t).await;
    let big = json!({"profile": {"settings": {"blob": "y".repeat(70 * 1024)}}});
    let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &big, &[("cookie", &cookie(&token))])).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    // 60 KiB fits under the 64 KiB profile cap.
    let ok = json!({"profile": {"settings": {"blob": "y".repeat(60 * 1024)}}});
    let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &ok, &[("cookie", &cookie(&token))])).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn login_redirects_with_pkce() {
    let t = test_app();
    let r = send(&t.app, get("/api/v1/auth/login")).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let url = reqwest::Url::parse(r.headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(url.as_str().split('?').next().unwrap(), "https://auth.bombatomica64.dev/application/o/authorize/");
    let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "segnale-debole");
    assert_eq!(q["redirect_uri"], "https://signal.bombatomica64.dev/api/v1/auth/callback");
    assert_eq!(q["scope"], "openid profile email");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["code_challenge"].len(), 43);
    assert!(q["state"].len() >= 32);
    assert!(
        url.query().unwrap().contains("scope=openid+profile+email")
            || url.query().unwrap().contains("scope=openid%20profile%20email")
    );
}

#[tokio::test]
async fn full_login_flow_sets_secure_cookie_and_checks_state() {
    let t = test_app();
    // Unknown state is rejected before any provider call.
    let r = send(&t.app, get("/api/v1/auth/callback?code=good-code&state=forged")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(t.oidc.seen.lock().unwrap().is_empty());
    let r = send(&t.app, get("/api/v1/auth/callback?error=access_denied")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = send(&t.app, get("/api/v1/auth/login")).await;
    let nonce = extract_cookie(&r.headers, "segnale_oidc_nonce");
    let url = reqwest::Url::parse(r.headers[header::LOCATION].to_str().unwrap()).unwrap();
    let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    // Another browser without (or with wrong) binding cookie cannot steal or consume the state.
    let r = send(&t.app, get(&format!("/api/v1/auth/callback?code=good-code&state={}", q["state"]))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let wrong_req = Request::get(format!("/api/v1/auth/callback?code=good-code&state={}", q["state"]))
        .header(header::COOKIE, format!("segnale_oidc_nonce={}", segnale_server::oidc::random_token(32)))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&t.app, wrong_req).await.status, StatusCode::BAD_REQUEST);
    assert!(t.oidc.seen.lock().unwrap().is_empty());

    // Original browser with matching binding cookie succeeds.
    let cb_req = Request::get(format!("/api/v1/auth/callback?code=good-code&state={}", q["state"]))
        .header(header::COOKIE, format!("segnale_oidc_nonce={nonce}"))
        .body(Body::empty())
        .unwrap();
    let r = send(&t.app, cb_req).await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert_eq!(r.headers[header::LOCATION], "/");
    let set_cookie = r
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("segnale_session="))
        .unwrap();
    for attr in ["; Path=/", "; HttpOnly", "; SameSite=Lax", "; Max-Age=2592000", "; Secure"] {
        assert!(set_cookie.contains(attr), "{set_cookie} lacks {attr}");
    }
    // The provider got the PKCE verifier whose S256 matches the challenge.
    let (code, verifier, redirect_uri) = t.oidc.seen.lock().unwrap()[0].clone();
    assert_eq!(code, "good-code");
    assert_eq!(segnale_server::oidc::pkce_challenge(&verifier), q["code_challenge"]);
    assert_eq!(redirect_uri, "https://signal.bombatomica64.dev/api/v1/auth/callback");
    // State is single-use even with the original binding cookie.
    let replay_req = Request::get(format!("/api/v1/auth/callback?code=good-code&state={}", q["state"]))
        .header(header::COOKIE, format!("segnale_oidc_nonce={nonce}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&t.app, replay_req).await.status, StatusCode::BAD_REQUEST);
    // Player created with a sanitized default callsign.
    let p = t.store.get_player("sub-123").await.unwrap().unwrap();
    assert_eq!(p.callsign, "LORENZOB");
    assert_eq!(p.username, "lorenzo.b");
}

#[tokio::test]
async fn provider_failure_is_502() {
    let t = test_app();
    let r = send(&t.app, get("/api/v1/auth/login")).await;
    let nonce = extract_cookie(&r.headers, "segnale_oidc_nonce");
    let url = reqwest::Url::parse(r.headers[header::LOCATION].to_str().unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    let req = Request::get(format!("/api/v1/auth/callback?code=bad&state={state}"))
        .header(header::COOKIE, format!("segnale_oidc_nonce={nonce}"))
        .body(Body::empty())
        .unwrap();
    let r = send(&t.app, req).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn cookie_secure_toggle_off() {
    let t = test_app_with(true, false, "a.");
    let token = login(&t).await;
    assert_eq!(token.len(), 43);
    let r = send(&t.app, Request::post("/api/v1/auth/logout").body(Body::empty()).unwrap()).await;
    let set_cookie = r.headers[header::SET_COOKIE].to_str().unwrap();
    assert!(!set_cookie.contains("Secure"));
    // Short usernames fall back to MARINE.
    assert_eq!(t.store.get_player("sub-123").await.unwrap().unwrap().callsign, "MARINE");
}

#[tokio::test]
async fn existing_callsign_not_overwritten_on_relogin() {
    let t = test_app();
    let token = login(&t).await;
    let r = send(
        &t.app,
        json_req(Method::PUT, "/api/v1/profile", &json!({"callsign": "NEWNAME"}), &[("cookie", &cookie(&token))]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    login(&t).await;
    assert_eq!(t.store.get_player("sub-123").await.unwrap().unwrap().callsign, "NEWNAME");
}

#[tokio::test]
async fn me_with_cookie_and_bearer() {
    let t = test_app();
    let r = send(&t.app, get("/api/v1/auth/me")).await;
    assert_eq!(r.json, json!({"authenticated": false, "auth_enabled": true}));
    let token = login(&t).await;
    for (k, v) in [("cookie", cookie(&token)), ("authorization", bearer(&token))] {
        let req = Request::get("/api/v1/auth/me").header(k, v).body(Body::empty()).unwrap();
        let r = send(&t.app, req).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.json["authenticated"], true);
        assert_eq!(r.json["auth_enabled"], true);
        assert_eq!(r.json["sub"], "sub-123");
        assert_eq!(r.json["username"], "lorenzo.b");
        assert_eq!(r.json["callsign"], "LORENZOB");
        assert_eq!(r.json["profile"], json!({}));
        assert!(r.json["profile_updated_at"].is_string());
    }
    // Garbage token = unauthenticated, not an error.
    let req = Request::get("/api/v1/auth/me").header("cookie", "segnale_session=garbage").body(Body::empty()).unwrap();
    assert_eq!(send(&t.app, req).await.json["authenticated"], false);
}

#[tokio::test]
async fn expired_session_rejected() {
    let t = test_app();
    t.store.upsert_player("old", "old", "OLD").await.unwrap();
    let token = segnale_server::oidc::random_token(32);
    let hash = segnale_server::api::hash_token(&token).unwrap();
    t.store.create_session(&hash, "old", Utc::now() - Duration::seconds(1)).await.unwrap();
    let req = Request::get("/api/v1/auth/me").header("authorization", bearer(&token)).body(Body::empty()).unwrap();
    assert_eq!(send(&t.app, req).await.json["authenticated"], false);
    assert_eq!(t.store.purge_expired_sessions(Utc::now()).await.unwrap(), 1);
}

#[tokio::test]
async fn auth_disabled_mode() {
    let t = test_app_with(false, true, "x");
    let r = send(&t.app, get("/api/v1/auth/me")).await;
    assert_eq!(r.json, json!({"authenticated": false, "auth_enabled": false}));
    let r = send(&t.app, get("/api/v1/auth/login")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(r.json["error"].is_string());
    let r = send(&t.app, get("/api/v1/auth/callback?code=a&state=b")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(post_daily(&t.app, &daily("GUEST", false, 3, 100.0), "1.2.3.4", &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn authenticated_submissions_are_verified_with_stored_callsign() {
    let t = test_app();
    let token = login(&t).await;
    let c = cookie(&token);
    let r = post_daily(&t.app, &daily("SPOOF", true, 5, 400.0), "1.0.0.1", &[("cookie", &c)]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["verified"], true);
    assert_eq!(r.json["entry"]["callsign"], "LORENZOB");
    assert_eq!(r.json["entry"]["verified"], true);
    // Separate from a guest using the same callsign.
    let r = post_daily(&t.app, &daily("LORENZOB", true, 5, 300.0), "1.0.0.2", &[]).await;
    assert_eq!(r.json["verified"], false);
    assert_eq!(r.json["rank"], 1);
    let r = send(&t.app, get("/api/v1/leaderboard/daily")).await;
    let e = r.json["entries"].as_array().unwrap();
    assert_eq!(e.len(), 2);
    assert_eq!((e[1]["callsign"].as_str(), e[1]["verified"].as_bool()), (Some("LORENZOB"), Some(true)));

    let r = post_trial(&t.app, &trial("WHATEVER", 90.0), "1.0.0.1", &[("authorization", &bearer(&token))]).await;
    assert_eq!(r.json["verified"], true);
    assert_eq!(r.json["entry"]["callsign"], "LORENZOB");
}

#[tokio::test]
async fn cross_origin_cookie_is_guest_but_trusted_origin_is_not() {
    let t = test_app();
    let token = login(&t).await;
    let c = cookie(&token);
    let r = post_daily(
        &t.app,
        &daily("EVILGUEST", false, 3, 100.0),
        "1.0.0.3",
        &[("cookie", &c), ("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(r.json["verified"], false);
    assert_eq!(r.json["entry"]["callsign"], "EVILGUEST");
    assert_eq!(r.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(r.headers.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).is_none());
    let req = Request::get("/api/v1/auth/me")
        .header("cookie", &c)
        .header("origin", "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&t.app, req).await.json["authenticated"], false);
    let req = Request::get("/api/v1/auth/me")
        .header("cookie", &c)
        .header("origin", "http://localhost:8080")
        .body(Body::empty())
        .unwrap();
    let r = send(&t.app, req).await;
    assert_eq!(r.json["authenticated"], true);
    assert_eq!(r.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "http://localhost:8080");
    assert_eq!(r.headers[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
    assert!(r.headers.get_all(header::VARY).iter().any(|v| v == "Origin"));
}

#[tokio::test]
async fn cors_preflight() {
    let t = test_app();
    for (origin, expect_origin, creds) in [
        ("https://signal.bombatomica64.dev", "https://signal.bombatomica64.dev", true),
        ("http://127.0.0.1:5173", "http://127.0.0.1:5173", true),
        ("https://other.example", "*", false),
    ] {
        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/v1/leaderboard/daily")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .unwrap();
        let r = send(&t.app, req).await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
        assert_eq!(r.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], expect_origin);
        assert_eq!(r.headers.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).is_some(), creds);
        assert_eq!(r.headers[header::ACCESS_CONTROL_ALLOW_METHODS], "GET, POST, PUT, OPTIONS");
        assert_eq!(r.headers[header::ACCESS_CONTROL_ALLOW_HEADERS], "Content-Type, Authorization");
        assert_eq!(r.headers[header::ACCESS_CONTROL_MAX_AGE], "600");
    }
    // Preflights never count against the rate limit.
    for _ in 0..30 {
        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/v1/leaderboard/trial")
            .header("cf-connecting-ip", "1.9.9.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&t.app, req).await.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(post_trial(&t.app, &trial("PRE", 100.0), "1.9.9.9", &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn profile_requires_auth() {
    let t = test_app();
    assert_eq!(send(&t.app, get("/api/v1/profile")).await.status, StatusCode::UNAUTHORIZED);
    let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &json!({"profile": {}}), &[])).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.json["error"].is_string());
}

#[tokio::test]
async fn profile_get_put_monotonic_merge() {
    let t = test_app();
    let token = login(&t).await;
    let c = cookie(&token);
    let h = [("cookie", c.as_str())];
    let r = send(&t.app, Request::get("/api/v1/profile").header("cookie", &c).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["callsign"], "LORENZOB");
    assert_eq!(r.json["profile"], json!({}));
    assert!(chrono::DateTime::parse_from_rfc3339(r.json["updated_at"].as_str().unwrap()).is_ok());

    let device_a = json!({"profile": {
        "schema_version": 6, "callsign": "IGNORED", "unlocked_feats": ["FirstExtraction", "JammerHunter"],
        "stats": {"runs_played": 10, "victories": 2, "fastest_victory_secs": 300.0, "enemy_kills": [5, 1, 0, 0, 0, 0, 0], "best_speed": 31.0},
        "settings": {"volume": 0.8},
        "daily_records": [{"date": {"year": 2026, "month": 11, "day": 24}, "best_floor": 5, "victory": true,
                           "best_time_secs": 400.0, "peak_speed": 30.0, "attempts": 2}],
        "time_trial_best_secs": 95.0, "time_trial_best_splits": [20.0, 45.0, 70.0, 95.0]
    }});
    let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &device_a, &h)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["profile"]["callsign"], "LORENZOB", "profile callsign mirrors the account callsign");

    // Device B is behind on most things but ahead on some.
    let device_b = json!({"callsign": "NEW_CALL", "profile": {
        "schema_version": 6, "unlocked_feats": ["SpeedExtraction"],
        "stats": {"runs_played": 4, "victories": 3, "fastest_victory_secs": null, "enemy_kills": [2, 9, 1, 0, 0, 0, 0], "best_speed": 20.0},
        "settings": {"volume": 0.1},
        "daily_records": [{"date": {"year": 2026, "month": 11, "day": 24}, "best_floor": 3, "victory": false,
                           "best_time_secs": null, "peak_speed": 35.0, "attempts": 5}],
        "time_trial_best_secs": 100.0, "time_trial_best_splits": [25.0, 50.0, 75.0, 100.0]
    }});
    let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &device_b, &h)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let p = &r.json["profile"];
    assert_eq!(r.json["callsign"], "NEW_CALL");
    assert_eq!(p["callsign"], "NEW_CALL");
    assert_eq!(p["unlocked_feats"], json!(["FirstExtraction", "JammerHunter", "SpeedExtraction"]));
    assert_eq!(p["stats"]["runs_played"], 10);
    assert_eq!(p["stats"]["victories"], 3);
    assert_eq!(p["stats"]["fastest_victory_secs"], 300.0);
    assert_eq!(p["stats"]["enemy_kills"], json!([5, 9, 1, 0, 0, 0, 0]));
    assert_eq!(p["stats"]["best_speed"], 31.0);
    assert_eq!(p["settings"], json!({"volume": 0.1}), "settings: last write wins");
    assert_eq!(
        p["daily_records"],
        json!([{"date": {"year": 2026, "month": 11, "day": 24}, "best_floor": 5, "victory": true,
                "best_time_secs": 400.0, "peak_speed": 35.0, "attempts": 5}])
    );
    assert_eq!(p["time_trial_best_secs"], 95.0);
    assert_eq!(p["time_trial_best_splits"], json!([20.0, 45.0, 70.0, 95.0]));

    // GET returns the merged state; /me too.
    let r = send(&t.app, Request::get("/api/v1/profile").header("cookie", &c).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json["profile"]["stats"]["runs_played"], 10);
    let r = send(&t.app, Request::get("/api/v1/auth/me").header("cookie", &c).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json["callsign"], "NEW_CALL");

    // Callsign-only update keeps the profile.
    let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &json!({"callsign": "THIRD"}), &h)).await;
    assert_eq!(r.json["profile"]["stats"]["runs_played"], 10);
    assert_eq!(r.json["profile"]["callsign"], "THIRD");

    // Validation (invalid payloads are rejected without mutating the stored profile).
    for bad in [
        json!({"callsign": "lower"}),
        json!({"callsign": "X"}),
        json!({"profile": [1, 2]}),
        json!({"profile": "str"}),
        json!({"profile": {"stats": {"enemy_kills": [1, 2, 3]}}}),
        json!({"profile": {"unlocked_feats": ["FakeFeat"]}}),
        json!({"profile": {"best_floor_segments": [10.0, 20.0]}}),
    ] {
        let r = send(&t.app, json_req(Method::PUT, "/api/v1/profile", &bad, &h)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let r = send(&t.app, Request::get("/api/v1/profile").header("cookie", &c).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json["profile"]["stats"]["enemy_kills"], json!([5, 9, 1, 0, 0, 0, 0]));
}

#[tokio::test]
async fn logout_invalidates_session() {
    let t = test_app();
    let token = login(&t).await;
    let c = cookie(&token);
    let r = send(&t.app, Request::post("/api/v1/auth/logout").header("cookie", &c).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json, json!({"ok": true}));
    let set_cookie = r.headers[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.starts_with("segnale_session=;"));
    assert!(set_cookie.contains("Max-Age=0"));
    let r = send(&t.app, Request::get("/api/v1/auth/me").header("cookie", &c).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json["authenticated"], false);
    let r = send(
        &t.app,
        Request::get("/api/v1/auth/me").header("authorization", bearer(&token)).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.json["authenticated"], false);
}
