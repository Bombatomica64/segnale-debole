//! Postgres-backed [`Store`]. Runtime `sqlx::query` only (no compile-time
//! macros), so building never needs a database.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgPoolOptions, postgres::PgRow};

use crate::store::{
    DailyEntry, DailyRun, Player, Store, StoreResult, SubmitOutcome, Submitter, TokenHash, TrialEntry, TrialRun,
    next_profile,
};

/// Idempotent schema; run on every startup.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS players (
        sub TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        callsign TEXT NOT NULL,
        profile_json JSONB NOT NULL DEFAULT '{}'::jsonb,
        profile_updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
    )",
    "CREATE TABLE IF NOT EXISTS sessions (
        token_hash BYTEA PRIMARY KEY,
        sub TEXT NOT NULL REFERENCES players(sub) ON DELETE CASCADE,
        expires_at TIMESTAMPTZ NOT NULL,
        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
    )",
    "CREATE INDEX IF NOT EXISTS sessions_expires_at_idx ON sessions (expires_at)",
    "CREATE TABLE IF NOT EXISTS daily_scores (
        date TEXT NOT NULL,
        player_key TEXT NOT NULL,
        sub TEXT REFERENCES players(sub) ON DELETE SET NULL,
        callsign TEXT NOT NULL,
        verified BOOLEAN NOT NULL DEFAULT FALSE,
        victory BOOLEAN NOT NULL,
        floor INT NOT NULL,
        time_secs DOUBLE PRECISION NOT NULL,
        peak_speed DOUBLE PRECISION NOT NULL,
        max_bhop_chain INT NOT NULL,
        score BIGINT NOT NULL,
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        PRIMARY KEY (date, player_key)
    )",
    "CREATE INDEX IF NOT EXISTS daily_scores_rank_idx
        ON daily_scores (date, victory DESC, floor DESC, time_secs ASC, updated_at ASC)",
    "CREATE TABLE IF NOT EXISTS trial_scores (
        player_key TEXT PRIMARY KEY,
        sub TEXT REFERENCES players(sub) ON DELETE SET NULL,
        callsign TEXT NOT NULL,
        verified BOOLEAN NOT NULL DEFAULT FALSE,
        time_secs DOUBLE PRECISION NOT NULL,
        splits DOUBLE PRECISION[] NOT NULL,
        peak_speed DOUBLE PRECISION NOT NULL,
        max_bhop_chain INT NOT NULL,
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
    )",
    "CREATE INDEX IF NOT EXISTS trial_scores_rank_idx ON trial_scores (time_secs ASC, updated_at ASC)",
];

const PLAYER_COLS: &str = "sub, username, callsign, profile_json, profile_updated_at";

/// Strict "ranks ahead of d" predicate for daily rows (o = other row).
const DAILY_AHEAD: &str = "(o.victory > d.victory
    OR (o.victory = d.victory AND (o.floor > d.floor
    OR (o.floor = d.floor AND (o.time_secs < d.time_secs
    OR (o.time_secs = d.time_secs AND (o.updated_at < d.updated_at
    OR (o.updated_at = d.updated_at AND o.player_key < d.player_key))))))))";

const TRIAL_AHEAD: &str = "(o.time_secs < d.time_secs
    OR (o.time_secs = d.time_secs AND (o.updated_at < d.updated_at
    OR (o.updated_at = d.updated_at AND o.player_key < d.player_key))))";

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(database_url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await?;
        let store = PgStore { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn migrate(&self) -> StoreResult<()> {
        for stmt in MIGRATIONS {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        Ok(())
    }
}

fn player_from_row(r: &PgRow) -> Result<Player, sqlx::Error> {
    Ok(Player {
        sub: r.try_get("sub")?,
        username: r.try_get("username")?,
        callsign: r.try_get("callsign")?,
        profile: r.try_get::<Value, _>("profile_json")?,
        profile_updated_at: r.try_get("profile_updated_at")?,
    })
}

fn rank_of(r: &PgRow) -> Result<u32, sqlx::Error> {
    Ok(r.try_get::<i64, _>("rank")?.clamp(1, u32::MAX as i64) as u32)
}

fn daily_from_row(r: &PgRow) -> Result<DailyEntry, sqlx::Error> {
    Ok(DailyEntry {
        rank: rank_of(r)?,
        callsign: r.try_get("callsign")?,
        verified: r.try_get("verified")?,
        run: DailyRun {
            victory: r.try_get("victory")?,
            floor: r.try_get("floor")?,
            time_secs: r.try_get("time_secs")?,
            peak_speed: r.try_get("peak_speed")?,
            max_bhop_chain: r.try_get("max_bhop_chain")?,
            score: r.try_get("score")?,
        },
    })
}

fn trial_from_row(r: &PgRow) -> Result<TrialEntry, sqlx::Error> {
    Ok(TrialEntry {
        rank: rank_of(r)?,
        callsign: r.try_get("callsign")?,
        verified: r.try_get("verified")?,
        run: TrialRun {
            time_secs: r.try_get("time_secs")?,
            splits: r.try_get("splits")?,
            peak_speed: r.try_get("peak_speed")?,
            max_bhop_chain: r.try_get("max_bhop_chain")?,
        },
    })
}

#[async_trait]
impl Store for PgStore {
    async fn health_check(&self) -> StoreResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    async fn upsert_player(&self, sub: &str, username: &str, default_callsign: &str) -> StoreResult<Player> {
        let row = sqlx::query(&format!(
            "INSERT INTO players (sub, username, callsign) VALUES ($1, $2, $3)
             ON CONFLICT (sub) DO UPDATE SET username = EXCLUDED.username, updated_at = NOW()
             RETURNING {PLAYER_COLS}"
        ))
        .bind(sub)
        .bind(username)
        .bind(default_callsign)
        .fetch_one(&self.pool)
        .await?;
        Ok(player_from_row(&row)?)
    }

    async fn get_player(&self, sub: &str) -> StoreResult<Option<Player>> {
        let row = sqlx::query(&format!("SELECT {PLAYER_COLS} FROM players WHERE sub = $1"))
            .bind(sub)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(player_from_row).transpose()?)
    }

    async fn create_session(&self, token_hash: &TokenHash, sub: &str, expires_at: DateTime<Utc>) -> StoreResult<()> {
        sqlx::query("INSERT INTO sessions (token_hash, sub, expires_at) VALUES ($1, $2, $3)")
            .bind(token_hash.to_vec())
            .bind(sub)
            .bind(expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn session_player(&self, token_hash: &TokenHash, now: DateTime<Utc>) -> StoreResult<Option<Player>> {
        let row = sqlx::query(
            "SELECT p.sub, p.username, p.callsign, p.profile_json, p.profile_updated_at
             FROM sessions s JOIN players p ON p.sub = s.sub
             WHERE s.token_hash = $1 AND s.expires_at > $2",
        )
        .bind(token_hash.to_vec())
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(player_from_row).transpose()?)
    }

    async fn delete_session(&self, token_hash: &TokenHash) -> StoreResult<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(token_hash.to_vec()).execute(&self.pool).await?;
        Ok(())
    }

    async fn purge_expired_sessions(&self, now: DateTime<Utc>) -> StoreResult<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE expires_at <= $1").bind(now).execute(&self.pool).await?;
        Ok(r.rows_affected())
    }

    async fn update_profile(
        &self,
        sub: &str,
        callsign: Option<&str>,
        incoming: Option<&Value>,
    ) -> StoreResult<Option<Player>> {
        // Row lock so concurrent PUTs from two devices merge serially.
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT callsign, profile_json FROM players WHERE sub = $1 FOR UPDATE")
            .bind(sub)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let stored_callsign: String = row.try_get("callsign")?;
        let stored: Value = row.try_get("profile_json")?;
        let callsign = callsign.map(str::to_string).unwrap_or(stored_callsign);
        let profile = next_profile(&stored, incoming, &callsign);
        let row = sqlx::query(&format!(
            "UPDATE players SET callsign = $2, profile_json = $3, profile_updated_at = NOW(), updated_at = NOW()
             WHERE sub = $1 RETURNING {PLAYER_COLS}"
        ))
        .bind(sub)
        .bind(&callsign)
        .bind(&profile)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(player_from_row(&row)?))
    }

    async fn submit_daily(
        &self,
        date: &str,
        who: &Submitter,
        run: &DailyRun,
    ) -> StoreResult<SubmitOutcome<DailyEntry>> {
        // Atomic best-only upsert: the conflict update only fires when the new
        // run is strictly better (victory, then floor, then lower time).
        let improved = sqlx::query(
            "INSERT INTO daily_scores
                (date, player_key, sub, callsign, verified, victory, floor, time_secs, peak_speed, max_bhop_chain, score, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, clock_timestamp())
             ON CONFLICT (date, player_key) DO UPDATE SET
                sub = EXCLUDED.sub, callsign = EXCLUDED.callsign, verified = EXCLUDED.verified,
                victory = EXCLUDED.victory, floor = EXCLUDED.floor, time_secs = EXCLUDED.time_secs,
                peak_speed = EXCLUDED.peak_speed, max_bhop_chain = EXCLUDED.max_bhop_chain,
                score = EXCLUDED.score, updated_at = clock_timestamp()
             WHERE (EXCLUDED.victory AND NOT daily_scores.victory)
                OR (EXCLUDED.victory = daily_scores.victory AND EXCLUDED.floor > daily_scores.floor)
                OR (EXCLUDED.victory = daily_scores.victory AND EXCLUDED.floor = daily_scores.floor
                    AND EXCLUDED.time_secs < daily_scores.time_secs)
             RETURNING 1",
        )
        .bind(date)
        .bind(&who.player_key)
        .bind(&who.sub)
        .bind(&who.callsign)
        .bind(who.verified)
        .bind(run.victory)
        .bind(run.floor)
        .bind(run.time_secs)
        .bind(run.peak_speed)
        .bind(run.max_bhop_chain)
        .bind(run.score)
        .fetch_optional(&self.pool)
        .await?
        .is_some();

        let row = sqlx::query(&format!(
            "SELECT d.callsign, d.verified, d.victory, d.floor, d.time_secs, d.peak_speed, d.max_bhop_chain, d.score,
                    (SELECT COUNT(*) FROM daily_scores o WHERE o.date = d.date AND {DAILY_AHEAD}) + 1 AS rank
             FROM daily_scores d WHERE d.date = $1 AND d.player_key = $2"
        ))
        .bind(date)
        .bind(&who.player_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(SubmitOutcome { improved, entry: daily_from_row(&row)? })
    }

    async fn daily_top(&self, date: &str, limit: u32) -> StoreResult<Vec<DailyEntry>> {
        let rows = sqlx::query(
            "SELECT callsign, verified, victory, floor, time_secs, peak_speed, max_bhop_chain, score,
                    ROW_NUMBER() OVER (ORDER BY victory DESC, floor DESC, time_secs ASC, updated_at ASC, player_key ASC) AS rank
             FROM daily_scores WHERE date = $1
             ORDER BY rank LIMIT $2",
        )
        .bind(date)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(daily_from_row).collect::<Result<_, _>>()?)
    }

    async fn submit_trial(&self, who: &Submitter, run: &TrialRun) -> StoreResult<SubmitOutcome<TrialEntry>> {
        let improved = sqlx::query(
            "INSERT INTO trial_scores
                (player_key, sub, callsign, verified, time_secs, splits, peak_speed, max_bhop_chain, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, clock_timestamp())
             ON CONFLICT (player_key) DO UPDATE SET
                sub = EXCLUDED.sub, callsign = EXCLUDED.callsign, verified = EXCLUDED.verified,
                time_secs = EXCLUDED.time_secs, splits = EXCLUDED.splits, peak_speed = EXCLUDED.peak_speed,
                max_bhop_chain = EXCLUDED.max_bhop_chain, updated_at = clock_timestamp()
             WHERE EXCLUDED.time_secs < trial_scores.time_secs
             RETURNING 1",
        )
        .bind(&who.player_key)
        .bind(&who.sub)
        .bind(&who.callsign)
        .bind(who.verified)
        .bind(run.time_secs)
        .bind(&run.splits)
        .bind(run.peak_speed)
        .bind(run.max_bhop_chain)
        .fetch_optional(&self.pool)
        .await?
        .is_some();

        let row = sqlx::query(&format!(
            "SELECT d.callsign, d.verified, d.time_secs, d.splits, d.peak_speed, d.max_bhop_chain,
                    (SELECT COUNT(*) FROM trial_scores o WHERE {TRIAL_AHEAD}) + 1 AS rank
             FROM trial_scores d WHERE d.player_key = $1"
        ))
        .bind(&who.player_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(SubmitOutcome { improved, entry: trial_from_row(&row)? })
    }

    async fn trial_top(&self, limit: u32) -> StoreResult<Vec<TrialEntry>> {
        let rows = sqlx::query(
            "SELECT callsign, verified, time_secs, splits, peak_speed, max_bhop_chain,
                    ROW_NUMBER() OVER (ORDER BY time_secs ASC, updated_at ASC, player_key ASC) AS rank
             FROM trial_scores ORDER BY rank LIMIT $1",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(trial_from_row).collect::<Result<_, _>>()?)
    }
}
