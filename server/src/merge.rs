//! Monotonic merge of the game's `PlayerProfile` JSON.
//!
//! The server stores the profile as opaque JSON, but a `PUT /api/v1/profile`
//! must never lose progress made on another device. The client merges before
//! uploading too; this is the server-side safety net.
//!
//! Field rules (top level of `PlayerProfile`):
//! - `schema_version`: max.
//! - `unlocked_feats`: set union, sorted.
//! - `stats`: numeric counters/records → max; `enemy_kills` → element-wise
//!   max; any `*_secs` key naming a best/fastest time (e.g.
//!   `fastest_victory_secs`) → min of the non-null values.
//! - `daily_records`: merged by `date` (`best_floor` max, `victory` OR,
//!   `best_time_secs` min of non-null, `peak_speed` max, `attempts` max, other
//!   fields incoming), sorted by date ascending, last [`DAILY_RECORDS_KEPT`].
//! - `best_floor_segments`: element-wise min of non-null.
//! - `standard_pb_splits`: the array whose last element is lower (non-empty
//!   beats empty).
//! - `time_trial_best_secs` + `time_trial_best_splits`: kept as a pair from
//!   the side with the lower non-null `time_trial_best_secs`.
//! - Any other best/fastest `*_secs` key: min of non-null.
//! - Everything else (settings, history, equipped items, loadout, unknown
//!   keys): incoming wins; keys missing from the incoming side are kept.
//!
//! Fixed-length arrays (`enemy_kills`, `best_floor_segments`) keep the
//! incoming length so the uploading client can always deserialize the reply.

use serde_json::{Map, Value};
use std::cmp::Ordering;

/// Same cap the game applies (`trials::DAILY_RECORDS_KEPT`).
pub const DAILY_RECORDS_KEPT: usize = 30;
/// Exact `PlayerProfile.stats.enemy_kills` array length (`enemies::KIND_COUNT`).
pub const ENEMY_KIND_COUNT: usize = 7;
/// Exact `PlayerProfile.best_floor_segments` array length (5 floors).
pub const FLOOR_SEGMENT_COUNT: usize = 5;

const KNOWN_FEATS: [&str; 10] = [
    "FirstExtraction",
    "SignalScout",
    "JammerHunter",
    "GhostInTheStatic",
    "FiendBuster",
    "DeadZoneTank",
    "BunkerCleared",
    "ContractCleared",
    "AllContractsCleared",
    "SpeedExtraction",
];

const TT_SECS: &str = "time_trial_best_secs";
const TT_SPLITS: &str = "time_trial_best_splits";

/// Validate recognized `PlayerProfile` fields before persisting/merging so a
/// malformed upload can never corrupt a player's cloud save or break client
/// deserialization (e.g., short `enemy_kills` array or unknown `FeatId` enum).
pub fn validate_profile_json(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or_else(|| "profile must be a JSON object".to_string())?;
    if let Some(sv) = obj.get("schema_version")
        && sv.as_u64().is_none()
    {
        return Err("schema_version must be a non-negative integer".into());
    }
    if let Some(feats) = obj.get("unlocked_feats") {
        let arr = feats.as_array().ok_or_else(|| "unlocked_feats must be an array".to_string())?;
        for f in arr {
            let s = f.as_str().ok_or_else(|| "unlocked_feats items must be strings".to_string())?;
            if !KNOWN_FEATS.contains(&s) {
                return Err(format!("unknown feat id: {s}"));
            }
        }
    }
    if let Some(stats) = obj.get("stats") {
        let sm = stats.as_object().ok_or_else(|| "stats must be an object".to_string())?;
        if let Some(ek) = sm.get("enemy_kills") {
            let arr = ek.as_array().ok_or_else(|| "stats.enemy_kills must be an array".to_string())?;
            if arr.len() != ENEMY_KIND_COUNT {
                return Err(format!("stats.enemy_kills must have length {ENEMY_KIND_COUNT}"));
            }
            if !arr.iter().all(|x| x.as_u64().is_some()) {
                return Err("stats.enemy_kills items must be non-negative integers".into());
            }
        }
        if let Some(fv) = sm.get("fastest_victory_secs")
            && !fv.is_null()
            && !fv.as_f64().is_some_and(|x| x.is_finite() && x > 0.0)
        {
            return Err("stats.fastest_victory_secs must be null or a positive number".into());
        }
        for (k, val) in sm {
            if k == "enemy_kills" || k == "fastest_victory_secs" {
                continue;
            }
            if let Some(n) = val.as_f64()
                && (!n.is_finite() || n < 0.0)
            {
                return Err(format!("stats.{k} must be a finite non-negative number"));
            }
        }
    }
    if let Some(segs) = obj.get("best_floor_segments") {
        let arr = segs.as_array().ok_or_else(|| "best_floor_segments must be an array".to_string())?;
        if arr.len() != FLOOR_SEGMENT_COUNT {
            return Err(format!("best_floor_segments must have length {FLOOR_SEGMENT_COUNT}"));
        }
        for item in arr {
            if !item.is_null() && !item.as_f64().is_some_and(|x| x.is_finite() && x > 0.0) {
                return Err("best_floor_segments items must be null or positive numbers".into());
            }
        }
    }
    if let Some(tt) = obj.get(TT_SECS)
        && !tt.is_null()
        && !tt.as_f64().is_some_and(|x| x.is_finite() && x > 0.0)
    {
        return Err("time_trial_best_secs must be null or a positive number".into());
    }
    for key in ["standard_pb_splits", TT_SPLITS] {
        if let Some(sp) = obj.get(key) {
            let arr = sp.as_array().ok_or_else(|| format!("{key} must be an array"))?;
            if arr.len() > 16 || !arr.iter().all(|x| x.as_f64().is_some_and(|n| n.is_finite() && n > 0.0)) {
                return Err(format!("{key} must be an array of positive numbers"));
            }
        }
    }
    if let Some(dr) = obj.get("daily_records") {
        let arr = dr.as_array().ok_or_else(|| "daily_records must be an array".to_string())?;
        if arr.len() > 64 {
            return Err("daily_records exceeds maximum length".into());
        }
        for rec in arr {
            if !rec.is_object() || date_key(rec).is_none() {
                return Err("daily_records items must be objects with a valid date".into());
            }
        }
    }
    Ok(())
}

/// Merge `incoming` into `existing`, returning the new stored profile.
pub fn merge_profile(existing: &Value, incoming: &Value) -> Value {
    match (existing, incoming) {
        (Value::Object(e), Value::Object(i)) => Value::Object(merge_top(e, i)),
        (Value::Object(_), _) => existing.clone(),
        _ => incoming.clone(),
    }
}

/// `true` for keys like `fastest_victory_secs`, `best_time_secs`.
pub fn is_best_time_key(key: &str) -> bool {
    key.ends_with("_secs") && (key.contains("best") || key.contains("fastest"))
}

fn merge_top(e: &Map<String, Value>, i: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in union_keys(e, i) {
        if key == TT_SECS || key == TT_SPLITS {
            continue;
        }
        let merged = match (e.get(&key), i.get(&key)) {
            (Some(ev), Some(iv)) => merge_top_field(&key, ev, iv),
            (Some(v), None) | (None, Some(v)) => v.clone(),
            (None, None) => continue,
        };
        out.insert(key, merged);
    }
    merge_trial_pair(e, i, &mut out);
    out
}

fn merge_top_field(key: &str, e: &Value, i: &Value) -> Value {
    match key {
        "schema_version" => max_num(e, i),
        "unlocked_feats" => union_sorted(e, i),
        "stats" => merge_stats(e, i),
        "daily_records" => merge_daily_records(e, i),
        "best_floor_segments" => elementwise(e, i, min_opt),
        "standard_pb_splits" => lower_last(e, i),
        k if is_best_time_key(k) => min_opt(e, i),
        _ => i.clone(),
    }
}

fn union_keys(e: &Map<String, Value>, i: &Map<String, Value>) -> Vec<String> {
    let mut keys: Vec<String> = i.keys().cloned().collect();
    keys.extend(e.keys().filter(|k| !i.contains_key(*k)).cloned());
    keys
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// Larger of two numbers; incoming if either is not a number.
fn max_num(e: &Value, i: &Value) -> Value {
    match (e.as_f64(), i.as_f64()) {
        (Some(a), Some(b)) if a > b => e.clone(),
        _ => i.clone(),
    }
}

/// Smaller of the non-null numbers (null means "no record yet").
fn min_opt(e: &Value, i: &Value) -> Value {
    match (num(Some(e)), num(Some(i))) {
        (Some(a), Some(b)) if a < b => e.clone(),
        (Some(_), Some(_)) => i.clone(),
        (Some(_), None) => e.clone(),
        (None, Some(_)) => i.clone(),
        (None, None) => i.clone(),
    }
}

fn or_bool(e: &Value, i: &Value) -> Value {
    match (e.as_bool(), i.as_bool()) {
        (Some(a), Some(b)) => Value::Bool(a || b),
        _ => i.clone(),
    }
}

/// Element-wise merge preserving the maximum length of both arrays so existing
/// indices are never dropped.
fn elementwise(e: &Value, i: &Value, f: fn(&Value, &Value) -> Value) -> Value {
    match (e.as_array(), i.as_array()) {
        (Some(ea), Some(ia)) => {
            let len = ea.len().max(ia.len());
            Value::Array(
                (0..len)
                    .map(|idx| match (ea.get(idx), ia.get(idx)) {
                        (Some(ev), Some(iv)) => f(ev, iv),
                        (Some(ev), None) => ev.clone(),
                        (None, Some(iv)) => iv.clone(),
                        (None, None) => unreachable!(),
                    })
                    .collect(),
            )
        }
        _ => i.clone(),
    }
}

fn union_sorted(e: &Value, i: &Value) -> Value {
    match (e.as_array(), i.as_array()) {
        (Some(ea), Some(ia)) => {
            let mut all: Vec<Value> = Vec::with_capacity(ea.len() + ia.len());
            for v in ia.iter().chain(ea.iter()) {
                if !all.contains(v) {
                    all.push(v.clone());
                }
            }
            if all.iter().all(Value::is_string) {
                all.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
            Value::Array(all)
        }
        _ => i.clone(),
    }
}

/// The splits array whose last (cumulative) split is lower; non-empty wins.
fn lower_last(e: &Value, i: &Value) -> Value {
    let last = |v: &Value| v.as_array().and_then(|a| num(a.last()));
    match (last(e), last(i)) {
        (Some(a), Some(b)) if a < b => e.clone(),
        (Some(_), None) => e.clone(),
        _ => i.clone(),
    }
}

fn merge_stats(e: &Value, i: &Value) -> Value {
    let (Some(em), Some(im)) = (e.as_object(), i.as_object()) else {
        return if i.is_null() { e.clone() } else { i.clone() };
    };
    let mut out = Map::new();
    for key in union_keys(em, im) {
        let merged = match (em.get(&key), im.get(&key)) {
            (Some(ev), Some(iv)) => {
                if key == "enemy_kills" {
                    elementwise(ev, iv, max_num)
                } else if is_best_time_key(&key) {
                    min_opt(ev, iv)
                } else if ev.is_number() && iv.is_number() {
                    max_num(ev, iv)
                } else {
                    iv.clone()
                }
            }
            (Some(v), None) | (None, Some(v)) => v.clone(),
            (None, None) => continue,
        };
        out.insert(key, merged);
    }
    Value::Object(out)
}

/// `(year, month, day)` of a daily record's `date` (`{year,month,day}` object
/// or a `"YYYY-MM-DD"` string).
fn date_key(record: &Value) -> Option<(i64, i64, i64)> {
    let d = record.get("date")?;
    if let Some(o) = d.as_object() {
        let g = |k: &str| o.get(k).and_then(Value::as_i64);
        return Some((g("year")?, g("month")?, g("day")?));
    }
    let s = d.as_str()?;
    let mut parts = s.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

fn merge_daily_record(e: &Value, i: &Value) -> Value {
    let (Some(em), Some(im)) = (e.as_object(), i.as_object()) else {
        return i.clone();
    };
    let mut out = im.clone();
    for (k, ev) in em {
        let merged = match (k.as_str(), im.get(k)) {
            (_, None) => ev.clone(),
            ("best_floor" | "peak_speed" | "attempts", Some(iv)) => max_num(ev, iv),
            ("victory", Some(iv)) => or_bool(ev, iv),
            (key, Some(iv)) if is_best_time_key(key) => min_opt(ev, iv),
            (_, Some(iv)) => iv.clone(),
        };
        out.insert(k.clone(), merged);
    }
    Value::Object(out)
}

fn merge_daily_records(e: &Value, i: &Value) -> Value {
    let (Some(ea), Some(ia)) = (e.as_array(), i.as_array()) else {
        return if i.is_null() { e.clone() } else { i.clone() };
    };
    let mut by_date: Vec<((i64, i64, i64), Value)> = Vec::new();
    for rec in ea {
        if let Some(k) = date_key(rec)
            && !by_date.iter().any(|(d, _)| *d == k)
        {
            by_date.push((k, rec.clone()));
        }
    }
    for rec in ia {
        let Some(k) = date_key(rec) else { continue };
        match by_date.iter_mut().find(|(d, _)| *d == k) {
            Some((_, slot)) => *slot = merge_daily_record(slot, rec),
            None => by_date.push((k, rec.clone())),
        }
    }
    by_date.sort_by_key(|a| a.0);
    let skip = by_date.len().saturating_sub(DAILY_RECORDS_KEPT);
    Value::Array(by_date.into_iter().skip(skip).map(|(_, v)| v).collect())
}

/// Keep `time_trial_best_secs` and `time_trial_best_splits` from the same side.
fn merge_trial_pair(e: &Map<String, Value>, i: &Map<String, Value>, out: &mut Map<String, Value>) {
    if !e.contains_key(TT_SECS) && !e.contains_key(TT_SPLITS) && !i.contains_key(TT_SECS) && !i.contains_key(TT_SPLITS)
    {
        return;
    }
    let es = num(e.get(TT_SECS));
    let is = num(i.get(TT_SECS));
    let take_existing = match (es, is) {
        (Some(a), Some(b)) => a.partial_cmp(&b) == Some(Ordering::Less),
        (Some(_), None) => true,
        (None, Some(_)) => false,
        // Neither has a time: incoming wins, falling back to whatever exists.
        (None, None) => !i.contains_key(TT_SECS) && !i.contains_key(TT_SPLITS),
    };
    let (winner, loser) = if take_existing { (e, i) } else { (i, e) };
    for key in [TT_SECS, TT_SPLITS] {
        if let Some(v) = winner.get(key).or_else(|| loser.get(key)) {
            out.insert(key.to_string(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stats_counters_take_max_and_fastest_takes_min() {
        let e = json!({"stats": {"runs_played": 10, "victories": 2, "best_speed": 30.5, "fastest_victory_secs": 300.0, "best_endless_floor": 7}});
        let i = json!({"stats": {"runs_played": 4, "victories": 3, "best_speed": 20.0, "fastest_victory_secs": 400.0, "best_endless_floor": 2}});
        let m = merge_profile(&e, &i);
        assert_eq!(m["stats"]["runs_played"], 10);
        assert_eq!(m["stats"]["victories"], 3);
        assert_eq!(m["stats"]["best_speed"], 30.5);
        assert_eq!(m["stats"]["fastest_victory_secs"], 300.0);
        assert_eq!(m["stats"]["best_endless_floor"], 7);
    }

    #[test]
    fn fastest_victory_null_never_erases_a_time() {
        let e = json!({"stats": {"fastest_victory_secs": 250.0}});
        let i = json!({"stats": {"fastest_victory_secs": null}});
        assert_eq!(merge_profile(&e, &i)["stats"]["fastest_victory_secs"], 250.0);
        assert_eq!(merge_profile(&i, &e)["stats"]["fastest_victory_secs"], 250.0);
    }

    #[test]
    fn enemy_kills_elementwise_max_preserves_existing_indices() {
        let e = json!({"stats": {"enemy_kills": [5, 0, 9, 1, 7]}});
        let i = json!({"stats": {"enemy_kills": [3, 4, 9, 2]}});
        assert_eq!(merge_profile(&e, &i)["stats"]["enemy_kills"], json!([5, 4, 9, 2, 7]));
    }

    #[test]
    fn validate_profile_rejects_bad_arrays_and_unknown_feats() {
        assert!(validate_profile_json(&json!({
            "schema_version": 6,
            "unlocked_feats": ["FirstExtraction", "SpeedExtraction"],
            "stats": {"runs_played": 3, "enemy_kills": [1, 2, 3, 4, 5, 6, 7]},
            "best_floor_segments": [10.0, null, 20.0, null, null]
        }))
        .is_ok());
        assert!(validate_profile_json(&json!({"stats": {"enemy_kills": [1, 2, 3]}})).is_err());
        assert!(validate_profile_json(&json!({"unlocked_feats": ["NotARealFeat"]})).is_err());
        assert!(validate_profile_json(&json!({"best_floor_segments": [10.0, 20.0]})).is_err());
        assert!(validate_profile_json(&json!({"stats": {"runs_played": -1}})).is_err());
    }

    #[test]
    fn feats_union_sorted_and_version_max() {
        let e = json!({"schema_version": 5, "unlocked_feats": ["Zeta", "Alpha"]});
        let i = json!({"schema_version": 4, "unlocked_feats": ["Beta", "Alpha"]});
        let m = merge_profile(&e, &i);
        assert_eq!(m["unlocked_feats"], json!(["Alpha", "Beta", "Zeta"]));
        assert_eq!(m["schema_version"], 5);
    }

    #[test]
    fn settings_history_and_unknown_keys_incoming_wins_missing_kept() {
        let e = json!({"settings": {"volume": 0.9, "fov": 100}, "history": [1, 2], "equipped_chip": "StockUac", "jam_demo_unlock_all": true, "only_old": 1});
        let i = json!({"settings": {"volume": 0.2}, "history": [3], "equipped_chip": "Overclock", "jam_demo_unlock_all": false, "only_new": "x"});
        let m = merge_profile(&e, &i);
        assert_eq!(m["settings"], json!({"volume": 0.2}));
        assert_eq!(m["history"], json!([3]));
        assert_eq!(m["equipped_chip"], "Overclock");
        assert_eq!(m["jam_demo_unlock_all"], false);
        assert_eq!(m["only_old"], 1);
        assert_eq!(m["only_new"], "x");
    }

    #[allow(clippy::too_many_arguments)]
    fn rec(y: i64, mo: i64, d: i64, floor: u32, victory: bool, t: Option<f64>, speed: f64, attempts: u32) -> Value {
        json!({"date": {"year": y, "month": mo, "day": d}, "best_floor": floor, "victory": victory,
               "best_time_secs": t, "peak_speed": speed, "attempts": attempts})
    }

    #[test]
    fn daily_records_merge_per_date_sorted() {
        let e = json!({"daily_records": [rec(2026, 11, 20, 5, true, Some(400.0), 30.0, 3), rec(2026, 11, 22, 2, false, None, 10.0, 1)]});
        let i = json!({"daily_records": [rec(2026, 11, 21, 3, false, None, 12.0, 1), rec(2026, 11, 20, 4, false, Some(350.0), 25.0, 5)]});
        let m = merge_profile(&e, &i);
        let recs = m["daily_records"].as_array().unwrap();
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0], rec(2026, 11, 20, 5, true, Some(350.0), 30.0, 5));
        assert_eq!(recs[1]["date"]["day"], 21);
        assert_eq!(recs[2]["date"]["day"], 22);
    }

    #[test]
    fn daily_records_capped_to_last_30() {
        let e: Vec<Value> = (1..=20).map(|d| rec(2026, 10, d, 1, false, None, 1.0, 1)).collect();
        let i: Vec<Value> = (1..=20).map(|d| rec(2026, 11, d, 1, false, None, 1.0, 1)).collect();
        let m = merge_profile(&json!({"daily_records": e}), &json!({"daily_records": i}));
        let recs = m["daily_records"].as_array().unwrap();
        assert_eq!(recs.len(), DAILY_RECORDS_KEPT);
        assert_eq!(recs[0]["date"], json!({"year": 2026, "month": 10, "day": 11}));
        assert_eq!(recs[29]["date"], json!({"year": 2026, "month": 11, "day": 20}));
    }

    #[test]
    fn segments_min_of_non_null() {
        let e = json!({"best_floor_segments": [50.0, null, 70.0, 80.0, null]});
        let i = json!({"best_floor_segments": [60.0, 40.0, null, 75.0, null]});
        assert_eq!(merge_profile(&e, &i)["best_floor_segments"], json!([50.0, 40.0, 70.0, 75.0, null]));
    }

    #[test]
    fn standard_pb_splits_lower_last_non_empty_wins() {
        let e = json!({"standard_pb_splits": [60.0, 120.0, 300.0]});
        let i = json!({"standard_pb_splits": [50.0, 130.0, 310.0]});
        assert_eq!(merge_profile(&e, &i)["standard_pb_splits"], json!([60.0, 120.0, 300.0]));
        let empty = json!({"standard_pb_splits": []});
        assert_eq!(merge_profile(&e, &empty)["standard_pb_splits"], json!([60.0, 120.0, 300.0]));
        assert_eq!(merge_profile(&empty, &i)["standard_pb_splits"], json!([50.0, 130.0, 310.0]));
    }

    #[test]
    fn time_trial_pair_kept_together() {
        let e = json!({"time_trial_best_secs": 90.0, "time_trial_best_splits": [20.0, 45.0, 70.0, 90.0]});
        let i = json!({"time_trial_best_secs": 95.0, "time_trial_best_splits": [10.0, 30.0, 60.0, 95.0]});
        let m = merge_profile(&e, &i);
        assert_eq!(m["time_trial_best_secs"], 90.0);
        assert_eq!(m["time_trial_best_splits"], json!([20.0, 45.0, 70.0, 90.0]));
        let none = json!({"time_trial_best_secs": null, "time_trial_best_splits": []});
        let m = merge_profile(&e, &none);
        assert_eq!(m["time_trial_best_secs"], 90.0);
        assert_eq!(m["time_trial_best_splits"], json!([20.0, 45.0, 70.0, 90.0]));
        let m = merge_profile(&none, &i);
        assert_eq!(m["time_trial_best_secs"], 95.0);
        assert_eq!(m["time_trial_best_splits"], json!([10.0, 30.0, 60.0, 95.0]));
    }

    #[test]
    fn empty_existing_takes_incoming_verbatim() {
        let i = json!({"schema_version": 3, "stats": {"runs_played": 1}, "settings": {"a": 1}});
        assert_eq!(merge_profile(&json!({}), &i), i);
    }

    #[test]
    fn merge_is_idempotent_and_monotonic() {
        let e = json!({"stats": {"runs_played": 10, "enemy_kills": [1, 2]}, "unlocked_feats": ["A"]});
        let i = json!({"stats": {"runs_played": 3, "enemy_kills": [0, 5]}, "unlocked_feats": ["B"]});
        let m1 = merge_profile(&e, &i);
        let m2 = merge_profile(&m1, &i);
        assert_eq!(m1, m2);
        assert_eq!(m1["stats"]["enemy_kills"], json!([1, 5]));
    }
}
