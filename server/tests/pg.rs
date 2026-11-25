//! PgStore against a real Postgres. Skipped unless `TEST_DATABASE_URL` points
//! at a THROWAWAY database (the test truncates every table), e.g.:
//!
//!   docker run --rm -d --name segnale-pg-test -e POSTGRES_PASSWORD=test -p 55432:5432 postgres:17-alpine
//!   TEST_DATABASE_URL=postgres://postgres:test@127.0.0.1:55432/postgres cargo test --test pg

use chrono::{Duration, Utc};
use serde_json::json;

use segnale_server::{
    pg::PgStore,
    store::{DailyRun, Store, Submitter, TrialRun},
};

fn guest(c: &str) -> Submitter {
    Submitter { player_key: format!("guest:{c}"), sub: None, callsign: c.into(), verified: false }
}

fn run(victory: bool, floor: i32, t: f64) -> DailyRun {
    DailyRun { victory, floor, time_secs: t, peak_speed: 20.0, max_bhop_chain: 3, score: 100 }
}

fn trial(t: f64) -> TrialRun {
    TrialRun { time_secs: t, splits: vec![t / 4.0, t / 2.0, t * 0.75, t], peak_speed: 10.0, max_bhop_chain: 1 }
}

#[tokio::test]
async fn pg_store_roundtrip() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set; skipping");
        return;
    };
    let s = PgStore::connect(&url).await.unwrap();
    s.migrate().await.unwrap(); // idempotent
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("TRUNCATE sessions, daily_scores, trial_scores, players CASCADE").execute(&pool).await.unwrap();

    s.health_check().await.unwrap();

    // Players + sessions.
    let p = s.upsert_player("sub1", "ann", "ANN").await.unwrap();
    assert_eq!(p.callsign, "ANN");
    assert_eq!(p.profile, json!({}));
    let p = s.upsert_player("sub1", "ann2", "OTHER").await.unwrap();
    assert_eq!((p.username.as_str(), p.callsign.as_str()), ("ann2", "ANN"));
    let h = [7u8; 32];
    s.create_session(&h, "sub1", Utc::now() + Duration::days(30)).await.unwrap();
    assert_eq!(s.session_player(&h, Utc::now()).await.unwrap().unwrap().sub, "sub1");
    assert!(s.session_player(&h, Utc::now() + Duration::days(31)).await.unwrap().is_none());
    let old = [8u8; 32];
    s.create_session(&old, "sub1", Utc::now() - Duration::seconds(5)).await.unwrap();
    assert_eq!(s.purge_expired_sessions(Utc::now()).await.unwrap(), 1);
    s.delete_session(&h).await.unwrap();
    assert!(s.session_player(&h, Utc::now()).await.unwrap().is_none());

    // Profile merge.
    let p = s.update_profile("sub1", None, Some(&json!({"stats": {"runs_played": 5}}))).await.unwrap().unwrap();
    assert_eq!(p.profile, json!({"stats": {"runs_played": 5}, "callsign": "ANN"}));
    let p =
        s.update_profile("sub1", Some("NEWCALL"), Some(&json!({"stats": {"runs_played": 2}}))).await.unwrap().unwrap();
    assert_eq!(p.callsign, "NEWCALL");
    assert_eq!(p.profile["stats"]["runs_played"], 5);
    assert_eq!(p.profile["callsign"], "NEWCALL");
    assert!(s.update_profile("missing", None, None).await.unwrap().is_none());

    // Daily best-only + ranks.
    let d = "2026-11-25";
    let o = s.submit_daily(d, &guest("A"), &run(false, 4, 300.0)).await.unwrap();
    assert!(o.improved);
    assert_eq!(o.entry.rank, 1);
    let o = s.submit_daily(d, &guest("A"), &run(false, 3, 100.0)).await.unwrap();
    assert!(!o.improved);
    assert_eq!(o.entry.run.floor, 4);
    assert!(s.submit_daily(d, &guest("A"), &run(false, 4, 250.0)).await.unwrap().improved);
    let o = s.submit_daily(d, &guest("B"), &run(true, 3, 900.0)).await.unwrap();
    assert_eq!(o.entry.rank, 1);
    let o = s.submit_daily(d, &guest("C"), &run(false, 4, 250.0)).await.unwrap();
    assert_eq!(o.entry.rank, 3, "tie: later submission ranks after");
    let auth = Submitter {
        player_key: "auth:sub1".into(),
        sub: Some("sub1".into()),
        callsign: "NEWCALL".into(),
        verified: true,
    };
    s.submit_daily(d, &auth, &run(false, 1, 50.0)).await.unwrap();
    let top = s.daily_top(d, 25).await.unwrap();
    let names: Vec<_> = top.iter().map(|e| (e.rank, e.callsign.as_str(), e.verified)).collect();
    assert_eq!(names, [(1, "B", false), (2, "A", false), (3, "C", false), (4, "NEWCALL", true)]);
    assert_eq!(s.daily_top("2000-01-01", 25).await.unwrap().len(), 0);
    assert_eq!(s.daily_top(d, 2).await.unwrap().len(), 2);

    // Trial.
    assert!(s.submit_trial(&guest("A"), &trial(120.0)).await.unwrap().improved);
    assert!(!s.submit_trial(&guest("A"), &trial(130.0)).await.unwrap().improved);
    let o = s.submit_trial(&guest("B"), &trial(60.0)).await.unwrap();
    assert_eq!(o.entry.rank, 1);
    assert_eq!(o.entry.run.splits, vec![15.0, 30.0, 45.0, 60.0]);
    let o = s.submit_trial(&guest("A"), &trial(100.0)).await.unwrap();
    assert!(o.improved);
    assert_eq!(o.entry.rank, 2);
    let top = s.trial_top(25).await.unwrap();
    assert_eq!(top.iter().map(|e| e.callsign.as_str()).collect::<Vec<_>>(), ["B", "A"]);
}
