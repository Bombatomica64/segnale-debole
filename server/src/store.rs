//! Persistence: the [`Store`] trait plus the in-process [`MemoryStore`].
//! The Postgres implementation lives in [`crate::pg`].

use std::{
    cmp::Ordering,
    collections::HashMap,
    fmt,
    sync::{Mutex, MutexGuard},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::merge::merge_profile;

#[derive(Debug)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store error: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError(e.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// SHA-256 of a raw session token.
pub type TokenHash = [u8; 32];

#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    pub sub: String,
    pub username: String,
    pub callsign: String,
    pub profile: Value,
    pub profile_updated_at: DateTime<Utc>,
}

/// Who a leaderboard submission is stored under.
#[derive(Clone, Debug)]
pub struct Submitter {
    /// `auth:{sub}` or `guest:{callsign}`.
    pub player_key: String,
    pub sub: Option<String>,
    pub callsign: String,
    pub verified: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DailyRun {
    pub victory: bool,
    pub floor: i32,
    pub time_secs: f64,
    pub peak_speed: f64,
    pub max_bhop_chain: i32,
    pub score: i64,
}

impl DailyRun {
    /// `self` strictly better than `other`: victory, then deeper floor, then faster.
    pub fn better_than(&self, other: &DailyRun) -> bool {
        self.rank_cmp(other) == Ordering::Less
    }

    /// Leaderboard order (Less = ranks higher), without the time tie-break.
    pub fn rank_cmp(&self, other: &DailyRun) -> Ordering {
        other
            .victory
            .cmp(&self.victory)
            .then(other.floor.cmp(&self.floor))
            .then(self.time_secs.total_cmp(&other.time_secs))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DailyEntry {
    pub rank: u32,
    pub callsign: String,
    pub verified: bool,
    #[serde(flatten)]
    pub run: DailyRun,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TrialRun {
    pub time_secs: f64,
    pub splits: Vec<f64>,
    pub peak_speed: f64,
    pub max_bhop_chain: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TrialEntry {
    pub rank: u32,
    pub callsign: String,
    pub verified: bool,
    #[serde(flatten)]
    pub run: TrialRun,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubmitOutcome<E> {
    /// The submission replaced (or created) the stored best.
    pub improved: bool,
    /// The stored best entry after the submission, with its current rank.
    pub entry: E,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn health_check(&self) -> StoreResult<()>;
    /// Insert the player or refresh its username; an existing callsign is kept.
    async fn upsert_player(&self, sub: &str, username: &str, default_callsign: &str) -> StoreResult<Player>;
    async fn get_player(&self, sub: &str) -> StoreResult<Option<Player>>;
    async fn create_session(&self, token_hash: &TokenHash, sub: &str, expires_at: DateTime<Utc>) -> StoreResult<()>;
    /// The player owning a non-expired session.
    async fn session_player(&self, token_hash: &TokenHash, now: DateTime<Utc>) -> StoreResult<Option<Player>>;
    async fn delete_session(&self, token_hash: &TokenHash) -> StoreResult<()>;
    async fn purge_expired_sessions(&self, now: DateTime<Utc>) -> StoreResult<u64>;
    /// Monotonic-merge `incoming` into the stored profile and/or set the callsign.
    /// The stored profile's `callsign` field always mirrors the player's callsign.
    async fn update_profile(
        &self,
        sub: &str,
        callsign: Option<&str>,
        incoming: Option<&Value>,
    ) -> StoreResult<Option<Player>>;
    async fn submit_daily(&self, date: &str, who: &Submitter, run: &DailyRun)
    -> StoreResult<SubmitOutcome<DailyEntry>>;
    async fn daily_top(&self, date: &str, limit: u32) -> StoreResult<Vec<DailyEntry>>;
    async fn submit_trial(&self, who: &Submitter, run: &TrialRun) -> StoreResult<SubmitOutcome<TrialEntry>>;
    async fn trial_top(&self, limit: u32) -> StoreResult<Vec<TrialEntry>>;
}

/// Apply the profile merge + callsign rules shared by every store.
pub fn next_profile(stored: &Value, incoming: Option<&Value>, callsign: &str) -> Value {
    let mut merged = match incoming {
        Some(i) => merge_profile(stored, i),
        None => stored.clone(),
    };
    if let Value::Object(m) = &mut merged {
        m.insert("callsign".into(), Value::String(callsign.to_string()));
    } else {
        merged = serde_json::json!({ "callsign": callsign });
    }
    merged
}

// ---------------------------------------------------------------------------
// MemoryStore
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Row<R> {
    who: Submitter,
    run: R,
    /// Monotonic stand-in for `updated_at` (deterministic tie-breaks).
    seq: u64,
}

#[derive(Default)]
struct Inner {
    seq: u64,
    players: HashMap<String, Player>,
    sessions: HashMap<TokenHash, (String, DateTime<Utc>)>,
    daily: HashMap<(String, String), Row<DailyRun>>,
    trial: HashMap<String, Row<TrialRun>>,
}

impl Inner {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

/// In-process store for tests and local development (data is lost on exit).
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn daily_cmp(a: &Row<DailyRun>, b: &Row<DailyRun>) -> Ordering {
    a.run.rank_cmp(&b.run).then(a.seq.cmp(&b.seq))
}

fn trial_cmp(a: &Row<TrialRun>, b: &Row<TrialRun>) -> Ordering {
    a.run.time_secs.total_cmp(&b.run.time_secs).then(a.seq.cmp(&b.seq))
}

fn daily_entry(row: &Row<DailyRun>, rank: usize) -> DailyEntry {
    DailyEntry {
        rank: rank as u32,
        callsign: row.who.callsign.clone(),
        verified: row.who.verified,
        run: row.run.clone(),
    }
}

fn trial_entry(row: &Row<TrialRun>, rank: usize) -> TrialEntry {
    TrialEntry {
        rank: rank as u32,
        callsign: row.who.callsign.clone(),
        verified: row.who.verified,
        run: row.run.clone(),
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn health_check(&self) -> StoreResult<()> {
        Ok(())
    }

    async fn upsert_player(&self, sub: &str, username: &str, default_callsign: &str) -> StoreResult<Player> {
        let mut g = self.lock();
        let now = Utc::now();
        let p = g.players.entry(sub.to_string()).or_insert_with(|| Player {
            sub: sub.to_string(),
            username: username.to_string(),
            callsign: default_callsign.to_string(),
            profile: Value::Object(Default::default()),
            profile_updated_at: now,
        });
        p.username = username.to_string();
        Ok(p.clone())
    }

    async fn get_player(&self, sub: &str) -> StoreResult<Option<Player>> {
        Ok(self.lock().players.get(sub).cloned())
    }

    async fn create_session(&self, token_hash: &TokenHash, sub: &str, expires_at: DateTime<Utc>) -> StoreResult<()> {
        let mut g = self.lock();
        if !g.players.contains_key(sub) {
            return Err(StoreError(format!("unknown player {sub}")));
        }
        g.sessions.insert(*token_hash, (sub.to_string(), expires_at));
        Ok(())
    }

    async fn session_player(&self, token_hash: &TokenHash, now: DateTime<Utc>) -> StoreResult<Option<Player>> {
        let g = self.lock();
        Ok(match g.sessions.get(token_hash) {
            Some((sub, exp)) if *exp > now => g.players.get(sub).cloned(),
            _ => None,
        })
    }

    async fn delete_session(&self, token_hash: &TokenHash) -> StoreResult<()> {
        self.lock().sessions.remove(token_hash);
        Ok(())
    }

    async fn purge_expired_sessions(&self, now: DateTime<Utc>) -> StoreResult<u64> {
        let mut g = self.lock();
        let before = g.sessions.len();
        g.sessions.retain(|_, (_, exp)| *exp > now);
        Ok((before - g.sessions.len()) as u64)
    }

    async fn update_profile(
        &self,
        sub: &str,
        callsign: Option<&str>,
        incoming: Option<&Value>,
    ) -> StoreResult<Option<Player>> {
        let mut g = self.lock();
        let Some(p) = g.players.get_mut(sub) else { return Ok(None) };
        if let Some(c) = callsign {
            p.callsign = c.to_string();
        }
        p.profile = next_profile(&p.profile, incoming, &p.callsign);
        p.profile_updated_at = Utc::now();
        Ok(Some(p.clone()))
    }

    async fn submit_daily(
        &self,
        date: &str,
        who: &Submitter,
        run: &DailyRun,
    ) -> StoreResult<SubmitOutcome<DailyEntry>> {
        let mut g = self.lock();
        let key = (date.to_string(), who.player_key.clone());
        let improved = g.daily.get(&key).is_none_or(|old| run.better_than(&old.run));
        if improved {
            let seq = g.next_seq();
            g.daily.insert(key.clone(), Row { who: who.clone(), run: run.clone(), seq });
        }
        let row = &g.daily[&key];
        let ahead = g.daily.iter().filter(|((d, _), r)| d == date && daily_cmp(r, row) == Ordering::Less).count();
        Ok(SubmitOutcome { improved, entry: daily_entry(row, ahead + 1) })
    }

    async fn daily_top(&self, date: &str, limit: u32) -> StoreResult<Vec<DailyEntry>> {
        let g = self.lock();
        let mut rows: Vec<&Row<DailyRun>> = g.daily.iter().filter(|((d, _), _)| d == date).map(|(_, r)| r).collect();
        rows.sort_by(|a, b| daily_cmp(a, b));
        Ok(rows.into_iter().take(limit as usize).enumerate().map(|(i, r)| daily_entry(r, i + 1)).collect())
    }

    async fn submit_trial(&self, who: &Submitter, run: &TrialRun) -> StoreResult<SubmitOutcome<TrialEntry>> {
        let mut g = self.lock();
        let key = who.player_key.clone();
        let improved = g.trial.get(&key).is_none_or(|old| run.time_secs < old.run.time_secs);
        if improved {
            let seq = g.next_seq();
            g.trial.insert(key.clone(), Row { who: who.clone(), run: run.clone(), seq });
        }
        let row = &g.trial[&key];
        let ahead = g.trial.values().filter(|r| trial_cmp(r, row) == Ordering::Less).count();
        Ok(SubmitOutcome { improved, entry: trial_entry(row, ahead + 1) })
    }

    async fn trial_top(&self, limit: u32) -> StoreResult<Vec<TrialEntry>> {
        let g = self.lock();
        let mut rows: Vec<&Row<TrialRun>> = g.trial.values().collect();
        rows.sort_by(|a, b| trial_cmp(a, b));
        Ok(rows.into_iter().take(limit as usize).enumerate().map(|(i, r)| trial_entry(r, i + 1)).collect())
    }
}
