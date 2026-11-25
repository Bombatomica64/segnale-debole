//! Input validation for leaderboard submissions and callsigns.

use chrono::{NaiveDate, Utc};
use serde::Deserialize;

use crate::store::{DailyRun, TrialRun};

pub const FLOOR_MAX: i64 = 200;
pub const SPEED_MAX: f64 = 65.0;
pub const BHOP_MAX: i64 = 500;
pub const SCORE_MAX: i64 = 10_000_000;
pub const SPLIT_TOLERANCE: f64 = 0.2;

/// `^[A-Z0-9_-]{2,12}$`
pub fn valid_callsign(s: &str) -> bool {
    (2..=12).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Default callsign from an OIDC username: uppercase, keep `[A-Z0-9_-]`,
/// truncate to 12, `MARINE` if fewer than 2 characters remain.
pub fn sanitize_callsign(username: &str) -> String {
    let s: String = username
        .chars()
        .flat_map(char::to_uppercase)
        .filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .take(12)
        .collect();
    if s.len() < 2 { "MARINE".into() } else { s }
}

/// Strict `YYYY-MM-DD` real calendar date.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let b = s.as_bytes();
    let shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shape {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub fn today_utc() -> NaiveDate {
    Utc::now().date_naive()
}

#[derive(Debug, Deserialize)]
pub struct DailySubmission {
    pub callsign: String,
    pub date: String,
    pub victory: bool,
    pub floor: i64,
    pub time_secs: f64,
    pub peak_speed: f64,
    pub max_bhop_chain: i64,
    pub score: i64,
}

#[derive(Debug, Deserialize)]
pub struct TrialSubmission {
    pub callsign: String,
    pub time_secs: f64,
    pub splits: Vec<f64>,
    pub peak_speed: f64,
    pub max_bhop_chain: i64,
}

fn check(ok: bool, msg: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(msg.to_string()) }
}

fn check_common(callsign: &str, peak_speed: f64, max_bhop_chain: i64) -> Result<(), String> {
    check(valid_callsign(callsign), "callsign must match ^[A-Z0-9_-]{2,12}$")?;
    check(peak_speed.is_finite() && (0.0..=SPEED_MAX).contains(&peak_speed), "peak_speed must be within 0..=65")?;
    check((0..=BHOP_MAX).contains(&max_bhop_chain), "max_bhop_chain must be within 0..=500")
}

/// Validate a daily submission against `today` (UTC); returns the normalized date + run.
pub fn validate_daily(s: &DailySubmission, today: NaiveDate) -> Result<(String, DailyRun), String> {
    check_common(&s.callsign, s.peak_speed, s.max_bhop_chain)?;
    let date = parse_date(&s.date).ok_or("date must be a real YYYY-MM-DD date")?;
    check((date - today).num_days().abs() <= 1, "date must be within one day of the server's UTC date")?;
    check(s.time_secs.is_finite() && (5.0..=7200.0).contains(&s.time_secs), "time_secs must be within 5..=7200")?;
    check((1..=FLOOR_MAX).contains(&s.floor), "floor must be within 1..=200")?;
    check(!s.victory || (s.time_secs >= 15.0 && s.floor >= 3), "a victory needs time_secs >= 15 and floor >= 3")?;
    check((0..=SCORE_MAX).contains(&s.score), "score must be within 0..=10000000")?;
    Ok((
        s.date.clone(),
        DailyRun {
            victory: s.victory,
            floor: s.floor as i32,
            time_secs: s.time_secs,
            peak_speed: s.peak_speed,
            max_bhop_chain: s.max_bhop_chain as i32,
            score: s.score,
        },
    ))
}

pub fn validate_trial(s: &TrialSubmission) -> Result<TrialRun, String> {
    check_common(&s.callsign, s.peak_speed, s.max_bhop_chain)?;
    check(s.time_secs.is_finite() && (5.0..=3600.0).contains(&s.time_secs), "time_secs must be within 5..=3600")?;
    check(s.splits.len() == 4, "splits must have exactly 4 cumulative entries")?;
    check(s.splits.iter().all(|x| x.is_finite() && *x > 0.0), "splits must be finite and positive")?;
    check(s.splits.windows(2).all(|w| w[1] > w[0]), "splits must be strictly increasing")?;
    check((s.splits[3] - s.time_secs).abs() <= SPLIT_TOLERANCE + 1e-9, "last split must match time_secs within 0.2 s")?;
    Ok(TrialRun {
        time_secs: s.time_secs,
        splits: s.splits.clone(),
        peak_speed: s.peak_speed,
        max_bhop_chain: s.max_bhop_chain as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callsigns() {
        assert!(valid_callsign("AB"));
        assert!(valid_callsign("MARINE_01-X"));
        assert!(!valid_callsign("A"));
        assert!(!valid_callsign("abc"));
        assert!(!valid_callsign("THIRTEENCHARS"));
        assert!(!valid_callsign("SP ACE"));
        assert_eq!(sanitize_callsign("lorenzo.b"), "LORENZOB");
        assert_eq!(sanitize_callsign("a"), "MARINE");
        assert_eq!(sanitize_callsign("über_long_name_here"), "BER_LONG_NAM");
    }

    #[test]
    fn dates() {
        assert!(parse_date("2026-02-28").is_some());
        assert!(parse_date("2026-02-29").is_none());
        assert!(parse_date("2028-02-29").is_some());
        assert!(parse_date("2026-2-28").is_none());
        assert!(parse_date("2026-02-28 ").is_none());
        assert!(parse_date("+026-02-28").is_none());
    }
}
