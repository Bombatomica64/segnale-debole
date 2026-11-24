//! Milestone 18: Daily Seeded Runs, the speedrun split timer and the movement
//! Time Trial.
//!
//! - **Daily Seeded Run**: [`DailyDate::today_utc`] (pure civil-calendar math,
//!   no external crates) picks [`daily_seed`] + [`daily_loadout`], so every
//!   player gets the same bunker and ruleset on that UTC date. Results land in
//!   [`PlayerProfile::daily_records`] (last [`DAILY_RECORDS_KEPT`] dates).
//! - **Split timer**: [`format_split_time`] / [`format_split_delta`] /
//!   [`split_tone`] feed the HUD plate ([`split_plate_view`]). The clock is
//!   `RunStats::time_elapsed` (or [`TimeTrialState::elapsed`]), which only
//!   advances in `AppState::Playing`, so Pause / Settings / UpgradeSelect
//!   freeze it.
//! - **Time Trial** ([`RunMode::TimeTrial`]): a handcrafted course
//!   ([`build_time_trial_grid`]) on the regular [`LevelGrid`], with 3
//!   checkpoint rings + the tower finish ([`time_trial_gates`]), pit rescue,
//!   instant `R` reset, rockets with no self-damage and pinned 5-bar signal.
//!   No demons, jammers or SOS uplink (see [`not_time_trial`]).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::audio::{SfxEvent, SfxQueue};
use crate::fx::{Fx, HudFeed};
use crate::level::{
    yaw_facing, Cell, JumpPadSpec, LevelEntity, LevelGrid, RampDir, Room, RoomFeature, ATRIUM_SIZE, CATWALK_HEIGHT, CELL_SIZE, JUMP_PAD_ATRIUM_UP,
    PILLAR_RADIUS, TOWER_MAST_RADIUS,
};
use crate::meta::{default_catalog, CarrierContract, HandsetSkin, PlayerProfile, ProfileStore, RunLoadout, StarterChip, UserSettings};
use crate::player::Player;
use crate::state::{start_run, AppState, GameSet, PlayerModifiers, RunMode, RunStats, UpgradeId, FINAL_FLOOR};
use crate::ui_kit::{ACID_GREEN, ARC_CYAN, ASH_DIM, BONE_TEXT, HAZARD_YELLOW, PUNK_RED};
use crate::weapons::{PlayerProjectile, Weapon, WeaponState};

// ---------------------------------------------------------------------------
// Daily date (pure UTC civil calendar)
// ---------------------------------------------------------------------------

/// A proleptic-Gregorian UTC calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DailyDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Default for DailyDate {
    fn default() -> Self {
        DailyDate::EPOCH
    }
}

const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

impl DailyDate {
    /// 1970-01-01 (Unix day 0).
    pub const EPOCH: DailyDate = DailyDate { year: 1970, month: 1, day: 1 };

    pub const fn new(year: i32, month: u8, day: u8) -> Self {
        DailyDate { year, month, day }
    }

    /// Howard Hinnant's `civil_from_days`: Unix day number -> date (clamped to
    /// safely representable years so extreme inputs never overflow).
    pub fn from_days_since_epoch(days: i64) -> Self {
        const MAX_SAFE_DAYS: i64 = 3_652_425_000;
        let clamped = days.clamp(-MAX_SAFE_DAYS, MAX_SAFE_DAYS);
        let z = clamped + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        DailyDate { year: year as i32, month: month as u8, day: day as u8 }
    }

    /// Howard Hinnant's `days_from_civil`: date -> Unix day number.
    pub fn days_since_epoch(self) -> i64 {
        let (m, d) = (i64::from(self.month), i64::from(self.day));
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// `YYYY-MM-DD`.
    pub fn format_iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `24 NOV` (menu subtitle).
    pub fn format_short(self) -> String {
        let m = MONTHS.get(usize::from(self.month.max(1)) - 1).copied().unwrap_or("???");
        format!("{:02} {m}", self.day)
    }

    /// Today's UTC date from the system clock (handles pre-epoch clocks with floor division).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn today_utc() -> Self {
        let now = std::time::SystemTime::now();
        let secs: i64 = match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs().min(i64::MAX as u64) as i64,
            Err(e) => -(e.duration().as_secs().min(i64::MAX as u64) as i64),
        };
        DailyDate::from_days_since_epoch(secs.div_euclid(86_400))
    }

    /// Today's UTC date from the browser clock (`Date.getUTC*`), so web
    /// players get the same Daily Seeded Run as native players.
    #[cfg(target_arch = "wasm32")]
    pub fn today_utc() -> Self {
        let now = js_sys::Date::new_0();
        DailyDate { year: now.get_utc_full_year() as i32, month: (now.get_utc_month() + 1) as u8, day: now.get_utc_date() as u8 }
    }
}

/// SplitMix64 finalizer.
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const DAILY_SALT: u64 = 0x5E6A_1E00_DA11_7000;

/// Deterministic run seed of a date's Daily Seeded Run.
pub fn daily_seed(date: DailyDate) -> u64 {
    let key = ((date.year as i64 as u64) << 16) | (u64::from(date.month) << 8) | u64::from(date.day);
    splitmix64(key ^ DAILY_SALT)
}

/// The date's fixed ruleset: one Starter OS chip, 1-2 Carrier Contracts and
/// the standard 12-card catalog (never a Jam Demo exhibition).
pub fn daily_loadout(date: DailyDate) -> RunLoadout {
    let s = daily_seed(date);
    let chip = StarterChip::ALL[(splitmix64(s ^ 1) % StarterChip::ALL.len() as u64) as usize];
    let count = 1 + (splitmix64(s ^ 2) % 2) as usize;
    let start = (splitmix64(s ^ 3) % CarrierContract::ALL.len() as u64) as usize;
    let mut contracts: Vec<CarrierContract> = (0..count).map(|k| CarrierContract::ALL[(start + k * 2) % CarrierContract::ALL.len()]).collect();
    contracts.sort();
    RunLoadout { chip, skin: HandsetSkin::UacOrange, contracts, catalog: default_catalog(), jam_demo: false }
}

/// Main menu "DAILY RUN": Floor 1 of the date's seed + ruleset (the handset
/// skin stays the player's cosmetic pick).
pub fn start_daily_run(
    run: &mut RunStats,
    mods: &mut PlayerModifiers,
    current: &mut RunLoadout,
    next: &mut NextState<AppState>,
    date: DailyDate,
    skin: HandsetSkin,
) {
    let loadout = RunLoadout { skin, ..daily_loadout(date) };
    crate::meta::start_run_with_loadout(run, mods, current, loadout, next, daily_seed(date));
    run.mode = RunMode::Daily(date);
}

/// Daily ruleset line: `SURVEY + ROAM/OVRT`.
pub fn daily_rules_line(l: &RunLoadout) -> String {
    let tags: Vec<&str> = l.contracts.iter().map(|c| c.short_tag()).collect();
    format!("{} + {}", l.chip.short_name(), tags.join("/"))
}

// ---------------------------------------------------------------------------
// Daily records
// ---------------------------------------------------------------------------

/// Dates kept in [`PlayerProfile::daily_records`].
pub const DAILY_RECORDS_KEPT: usize = 14;

/// Best result of one date's Daily Seeded Run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyRecord {
    pub date: DailyDate,
    pub best_floor: u32,
    pub victory: bool,
    /// Fastest victory time on that date.
    pub best_time_secs: Option<f32>,
    pub peak_speed: f32,
    /// Daily runs deployed on that date.
    pub attempts: u32,
}

/// The record of `date`, if any.
pub fn daily_record(records: &[DailyRecord], date: DailyDate) -> Option<&DailyRecord> {
    records.iter().find(|r| r.date == date)
}

/// The record of `date`, created if needed (`None` when the date is older
/// than every kept record and the list is full).
fn daily_entry(records: &mut Vec<DailyRecord>, date: DailyDate) -> Option<&mut DailyRecord> {
    if !records.iter().any(|r| r.date == date) {
        records.push(DailyRecord { date, ..default() });
        trim_daily_records(records);
    }
    records.iter_mut().find(|r| r.date == date)
}

/// Sort oldest first and keep only the newest [`DAILY_RECORDS_KEPT`] dates.
pub fn trim_daily_records(records: &mut Vec<DailyRecord>) {
    records.sort_by_key(|r| r.date);
    records.dedup_by_key(|r| r.date);
    if records.len() > DAILY_RECORDS_KEPT {
        let extra = records.len() - DAILY_RECORDS_KEPT;
        records.drain(..extra);
    }
}

/// A Daily run was deployed on `date` (counts the attempt).
pub fn begin_daily_attempt(records: &mut Vec<DailyRecord>, date: DailyDate) {
    if let Some(r) = daily_entry(records, date) {
        r.attempts += 1;
    }
}

/// Fold a finished (or Endless-extended) Daily run into its date's record.
pub fn record_daily_result(records: &mut Vec<DailyRecord>, date: DailyDate, run: &RunStats, victory: bool) {
    let Some(r) = daily_entry(records, date) else { return };
    r.attempts = r.attempts.max(1);
    r.best_floor = r.best_floor.max(run.floor);
    if run.peak_speed.is_finite() {
        r.peak_speed = r.peak_speed.max(run.peak_speed);
    }
    if victory && run.time_elapsed.is_finite() {
        r.victory = true;
        r.best_time_secs = Some(r.best_time_secs.map_or(run.time_elapsed, |t| t.min(run.time_elapsed)));
    }
}

/// Validate a saved cumulative split vector: exact length, finite strictly-positive
/// finish time, non-negative monotonic entries.
fn valid_cumulative_splits(splits: &[f32], expected_len: usize) -> bool {
    if splits.len() != expected_len {
        return false;
    }
    let mut prev = 0.0f32;
    for &v in splits {
        if !v.is_finite() || v < prev {
            return false;
        }
        prev = v;
    }
    splits.last().is_some_and(|&last| last > 0.0)
}

/// Clamp loaded M18 records (corrupt/non-positive times dropped, daily list trimmed).
pub fn sanitize_trial_records(p: &mut PlayerProfile) {
    trim_daily_records(&mut p.daily_records);
    for r in &mut p.daily_records {
        if !r.peak_speed.is_finite() || r.peak_speed < 0.0 {
            r.peak_speed = 0.0;
        }
        if r.best_time_secs.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            r.best_time_secs = None;
        }
    }
    if !valid_cumulative_splits(&p.standard_pb_splits, FINAL_FLOOR as usize) {
        p.standard_pb_splits.clear();
    }
    for s in &mut p.best_floor_segments {
        if s.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            *s = None;
        }
    }
    if p.time_trial_best_secs.is_some_and(|v| !v.is_finite() || v <= 0.0) {
        p.time_trial_best_secs = None;
        p.time_trial_best_splits.clear();
    }
    if !valid_cumulative_splits(&p.time_trial_best_splits, TRIAL_CHECKPOINTS + 1) {
        p.time_trial_best_splits.clear();
    }
}

// ---------------------------------------------------------------------------
// Splits: formatting, deltas, colors, PB records
// ---------------------------------------------------------------------------

fn tenths(secs: f32) -> u64 {
    if secs.is_finite() { (secs.max(0.0) * 10.0 + 1e-3).floor() as u64 } else { 0 }
}

/// `MM:SS.d` (truncated to tenths, like a stopwatch; minutes grow past 99).
pub fn format_split_time(secs: f32) -> String {
    let t = tenths(secs);
    format!("{:02}:{:02}.{}", t / 600, (t / 10) % 60, t % 10)
}

/// Signed delta vs a PB: `-0:04.2` ahead, `+0:02.1` behind (rounded to tenths).
pub fn format_split_delta(delta: f32) -> String {
    let d = if delta.is_finite() { delta } else { 0.0 };
    let t = (d.abs() * 10.0).round() as u64;
    let sign = if d < 0.0 && t > 0 { '-' } else { '+' };
    format!("{sign}{}:{:02}.{}", t / 600, (t / 10) % 60, t % 10)
}

/// How a split compares with the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitTone {
    /// Ahead of (or tied with) the PB pace.
    Ahead,
    /// Fastest-ever segment (gold).
    BestSegment,
    /// Behind the PB pace.
    Behind,
    /// Nothing to compare against yet.
    Neutral,
}

/// Classify a split: a segment strictly faster than the best segment is gold,
/// otherwise the sign of the cumulative delta decides.
pub fn split_tone(delta: Option<f32>, segment: Option<f32>, best_segment: Option<f32>) -> SplitTone {
    if let (Some(s), Some(b)) = (segment, best_segment)
        && s < b
    {
        return SplitTone::BestSegment;
    }
    match delta {
        Some(d) if d <= 0.0 => SplitTone::Ahead,
        Some(_) => SplitTone::Behind,
        None => SplitTone::Neutral,
    }
}

/// Split gold.
pub const SPLIT_GOLD: Color = Color::srgb(1.0, 0.8, 0.28);

pub fn split_tone_color(t: SplitTone) -> Color {
    match t {
        SplitTone::Ahead => ACID_GREEN,
        SplitTone::BestSegment => SPLIT_GOLD,
        SplitTone::Behind => PUNK_RED,
        SplitTone::Neutral => BONE_TEXT,
    }
}

/// Single segment duration at index `i` without allocating a `Vec`.
pub fn segment_at(splits: &[f32], i: usize) -> Option<f32> {
    let &t = splits.get(i)?;
    let prev = if i == 0 { 0.0 } else { *splits.get(i - 1)? };
    Some(t - prev)
}

/// Per-segment durations of cumulative splits.
pub fn segments_from_splits(splits: &[f32]) -> Vec<f32> {
    splits.iter().scan(0.0, |prev, &s| {
        let seg = s - *prev;
        *prev = s;
        Some(seg)
    })
    .collect()
}

/// Fold a recorded Standard run's floor splits: best segments always, the PB
/// split set only for a faster 5-floor victory.
pub fn record_standard_splits(p: &mut PlayerProfile, splits: &[f32], victory: bool) {
    if !splits.iter().all(|s| s.is_finite()) {
        return;
    }
    for (i, seg) in segments_from_splits(splits).into_iter().take(FINAL_FLOOR as usize).enumerate() {
        if seg > 0.0 {
            let best = &mut p.best_floor_segments[i];
            *best = Some(best.map_or(seg, |b| b.min(seg)));
        }
    }
    let n = FINAL_FLOOR as usize;
    if victory && splits.len() >= n {
        let run = &splits[..n];
        if run[n - 1] > 0.0 && (!valid_cumulative_splits(&p.standard_pb_splits, n) || run[n - 1] < p.standard_pb_splits[n - 1]) {
            p.standard_pb_splits = run.to_vec();
        }
    }
}

/// Outcome of a Time Trial completion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrialResult {
    pub new_pb: bool,
    /// Final time minus the previous PB (`None` on the first completion).
    pub delta: Option<f32>,
    pub previous_best: Option<f32>,
}

/// Fold a Time Trial completion into the profile: course PB + its splits and
/// the lifetime movement records (never `runs_played` / `victories` / feats).
pub fn record_time_trial(p: &mut PlayerProfile, time: f32, splits: &[f32], peak_speed: f32, bhop_chain: u32) -> TrialResult {
    let previous_best = p.time_trial_best_secs;
    let new_pb = time.is_finite() && previous_best.is_none_or(|b| time < b);
    if new_pb {
        p.time_trial_best_secs = Some(time);
        p.time_trial_best_splits = splits.to_vec();
    }
    if peak_speed.is_finite() {
        p.stats.best_speed = p.stats.best_speed.max(peak_speed);
    }
    p.stats.best_bhop_chain = p.stats.best_bhop_chain.max(bhop_chain);
    TrialResult { new_pb, delta: previous_best.map(|b| time - b), previous_best }
}

// ---------------------------------------------------------------------------
// Time Trial course
// ---------------------------------------------------------------------------

/// Run seed of the Time Trial (the grid is handcrafted, the seed only tags it).
pub const TRIAL_SEED: u64 = 0x71A1_5EED_C0DE;
/// Signal-Mortar rack in the Time Trial (auto-refilled).
pub const TRIAL_ROCKETS: u32 = 99;
/// Checkpoints before the finish.
pub const TRIAL_CHECKPOINTS: usize = 3;
/// Course size (cells).
pub const TRIAL_WIDTH: i32 = 27;
pub const TRIAL_DEPTH: i32 = 17;
/// Feet below this height inside a pit cell = rescued back to the last checkpoint.
pub const PIT_RESCUE_Y: f32 = -0.3;

/// The course map: `#` wall, `.` floor, `^` catwalk (y = 2.4), `~` lava pit,
/// `A` open-roof atrium, `N/S/E/W` ramps 0 -> 2.4 rising that way.
///
/// 1. Start airlock (2,14) -> straightaway + chicane -> CP 1 (x = 13).
/// 2. Jump-pad bay: pad (20,12) vaults onto the catwalk (or walk the ramp
///    (22,11); rocket-jump the 2.4 m ledge anywhere), catwalk west over a
///    strafe gap (13) with a recovery ramp (12) -> CP 2 (x = 8).
/// 3. Ramp descent (2-3,8) into the open-roof pit hall: jump / dash /
///    rocket-jump the lava trench (10-11), launch over it from the pad (9,4),
///    or take the slow north lane -> CP 3 (x = 15).
/// 4. Signal Atrium: ramp (20,4) or atrium pad (22,6) onto the tower pedestal = FINISH.
pub const TRIAL_MAP: [&str; 17] = [
    "###########################",
    "########......#####AAAAAAA#",
    "#........####......AAAAAAA#",
    "#.........~~.......AAAAAAA#",
    "#.........~~.......AEAAAAA#",
    "#.........~~.......AAAAAAA#",
    "#.........~~.......AAAAAAA#",
    "#....##############AAAAAAA#",
    "##SS#######################",
    "##^^^^^^^^^^W.^^^^^^^^^####",
    "##^^^^^^^^^^W.^^^^^^^^^####",
    "##################....N####",
    "##################.....####",
    "#.....#...........#....####",
    "#.................#....####",
    "#.........#............####",
    "###########################",
];

/// Lay the handcrafted course onto a [`LevelGrid`] (inherits the M16 world
/// architecture, jump pads, ramps, physics and rendering).
pub fn build_time_trial_grid() -> LevelGrid {
    let mut g = LevelGrid::from_ascii(&TRIAL_MAP);
    g.floor = 1;
    g.seed = TRIAL_SEED;
    g.run_seed = TRIAL_SEED;
    // Open the start corridor into the bay (the map row keeps the bay wall
    // readable) and carve the north detour lane's mouths.
    for z in 13..=15 {
        g.set(IVec2::new(18, z), Cell::Floor { height: 0.0 });
    }
    // Signal Atrium: 7x7 open roof, 3x3 tower pedestal, west ramp + south pad.
    let amin = IVec2::new(19, 1);
    for z in 0..ATRIUM_SIZE {
        for x in 0..ATRIUM_SIZE {
            let c = amin + IVec2::new(x, z);
            if matches!(g.cell(c), Cell::Wall) {
                g.set(c, Cell::AtriumFloor);
            }
            open_roof(&mut g, c);
        }
    }
    let tower = amin + IVec2::splat(ATRIUM_SIZE / 2);
    g.tower = tower;
    for z in -1..=1 {
        for x in -1..=1 {
            g.set(tower + IVec2::new(x, z), Cell::Floor { height: CATWALK_HEIGHT });
        }
    }
    g.set(tower - IVec2::X * 2, Cell::Ramp { low_height: 0.0, high_height: CATWALK_HEIGHT, dir: RampDir::East });
    g.pillars.push(cell_xz(tower));
    g.pillar_radii.push(TOWER_MAST_RADIUS);
    for (dx, dz) in [(1, 1), (5, 1), (1, 5), (5, 5)] {
        g.pillars.push(cell_xz(amin + IVec2::new(dx, dz)));
        g.pillar_radii.push(PILLAR_RADIUS);
    }
    g.rooms.push(Room { min: amin, size: IVec2::splat(ATRIUM_SIZE), feature: RoomFeature::None });
    // Pit hall + jump-pad bay / east catwalk: open roof so pad and
    // rocket-jump arcs have headroom.
    for (xs, zs) in [(8..=13, 1..=6), (17..=22, 9..=15)] {
        for z in zs {
            for x in xs.clone() {
                if !g.cell(IVec2::new(x, z)).is_wall() {
                    open_roof(&mut g, IVec2::new(x, z));
                }
            }
        }
    }
    g.spawn = IVec2::new(2, 14);
    g.spawn_yaw = yaw_facing(Vec2::X);
    g.jump_pads = vec![
        // Bay vault north onto the catwalk (a cell back from the lip, so the
        // arc clears the 2.4 m ledge from a standstill or at a run).
        JumpPadSpec { cell: IVec2::new(20, 12), launch_vel: Vec3::new(0.0, 14.0, -6.0) },
        // Trench launcher east over the lava.
        JumpPadSpec { cell: IVec2::new(9, 4), launch_vel: Vec3::new(14.0, 11.0, 0.0) },
        // Atrium launcher north onto the pedestal.
        JumpPadSpec { cell: tower + IVec2::Y * 2, launch_vel: Vec3::new(0.0, JUMP_PAD_ATRIUM_UP, -5.0) },
    ];
    g.lights = vec![
        IVec2::new(3, 14),
        IVec2::new(8, 14),
        IVec2::new(14, 14),
        IVec2::new(15, 10),
        IVec2::new(7, 9),
        IVec2::new(2, 9),
        IVec2::new(2, 4),
        IVec2::new(6, 3),
        IVec2::new(16, 4),
    ];
    g
}

fn open_roof(g: &mut LevelGrid, c: IVec2) {
    if g.in_bounds(c) {
        let i = (c.y * g.width + c.x) as usize;
        g.roof_open[i] = true;
    }
}

fn cell_xz(c: IVec2) -> Vec2 {
    Vec2::new((c.x as f32 + 0.5) * CELL_SIZE, (c.y as f32 + 0.5) * CELL_SIZE)
}

/// A checkpoint ring (or the finish gate): crossing any of its cells triggers it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrialGate {
    pub label: &'static str,
    pub cells: Vec<IVec2>,
    /// Where a pit rescue / respawn after this gate puts the player.
    pub respawn: IVec2,
    pub respawn_yaw: f32,
    /// Ring center (world) and radius.
    pub ring_center: Vec3,
    pub ring_radius: f32,
    /// Ring normal is the travel axis (`false` = horizontal halo, finish).
    pub vertical: bool,
}

impl TrialGate {
    pub fn contains(&self, c: IVec2) -> bool {
        self.cells.contains(&c)
    }
}

fn column(x: i32, zs: std::ops::RangeInclusive<i32>) -> Vec<IVec2> {
    zs.map(|z| IVec2::new(x, z)).collect()
}

fn gate_center(cells: &[IVec2], y: f32) -> Vec3 {
    let sum = cells.iter().fold(Vec2::ZERO, |a, &c| a + cell_xz(c));
    let c = sum / cells.len().max(1) as f32;
    Vec3::new(c.x, y, c.y)
}

static GATES: std::sync::LazyLock<[TrialGate; TRIAL_CHECKPOINTS + 1]> = std::sync::LazyLock::new(build_time_trial_gates);

/// The 3 checkpoints + finish of [`build_time_trial_grid`], in order (built once).
pub fn time_trial_gates() -> &'static [TrialGate; TRIAL_CHECKPOINTS + 1] {
    &GATES
}

fn build_time_trial_gates() -> [TrialGate; TRIAL_CHECKPOINTS + 1] {
    let east = yaw_facing(Vec2::X);
    let west = yaw_facing(Vec2::NEG_X);
    let cp1 = column(13, 13..=15);
    let cp2 = column(8, 9..=10);
    let cp3 = column(15, 2..=6);
    let fin: Vec<IVec2> = (3..=5).flat_map(|z| (21..=23).map(move |x| IVec2::new(x, z))).collect();
    [
        TrialGate { label: "CP 1", ring_center: gate_center(&cp1, 2.9), cells: cp1, respawn: IVec2::new(13, 14), respawn_yaw: east, ring_radius: 2.6, vertical: true },
        TrialGate {
            label: "CP 2",
            ring_center: gate_center(&cp2, CATWALK_HEIGHT + 1.75),
            cells: cp2,
            respawn: IVec2::new(8, 9),
            respawn_yaw: west,
            ring_radius: 1.6,
            vertical: true,
        },
        TrialGate { label: "CP 3", ring_center: gate_center(&cp3, 2.9), cells: cp3, respawn: IVec2::new(15, 4), respawn_yaw: east, ring_radius: 2.6, vertical: true },
        TrialGate {
            label: "FINISH",
            ring_center: gate_center(&fin, CATWALK_HEIGHT + 0.25),
            cells: fin,
            respawn: IVec2::new(21, 4),
            respawn_yaw: east,
            ring_radius: 5.2,
            vertical: false,
        },
    ]
}

/// Live Time Trial attempt.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct TimeTrialState {
    /// Attempt clock (advances only in `Playing`, frozen once finished).
    pub elapsed: f32,
    /// Next gate to cross (`0..=3`; `4` once finished).
    pub next_gate: usize,
    /// Clock at each gate crossed (3 checkpoints + finish).
    pub splits: Vec<f32>,
    pub finished: bool,
    /// Result of the finished attempt.
    pub result: Option<TrialResult>,
    /// PB splits that stood before this attempt finished (so a new PB still shows deltas).
    pub previous_splits: Vec<f32>,
    /// Pit rescues this attempt.
    pub rescues: u32,
    /// `R` / RETRY: reset the attempt on the next `Playing` tick.
    pub reset_requested: bool,
}

impl TimeTrialState {
    pub fn final_time(&self) -> Option<f32> {
        if self.finished { self.splits.last().copied() } else { None }
    }

    /// Gates cleared so far.
    pub fn cleared(&self) -> usize {
        self.next_gate.min(TRIAL_CHECKPOINTS + 1)
    }
}

/// Run condition: the live run is a Time Trial.
pub fn is_time_trial(run: Option<Res<RunStats>>) -> bool {
    run.is_some_and(|r| r.mode == RunMode::TimeTrial)
}

/// Run condition: demons, jammers, the SOS uplink and floor skips stay off in the Time Trial.
pub fn not_time_trial(run: Option<Res<RunStats>>) -> bool {
    !is_time_trial(run)
}

/// Time Trial rule set on (already reset) run modifiers: 5G Overclock, Blast
/// Thrusters and Airplane Mode Phase installed, 5 bars everywhere (jump pads
/// always powered, overclock live) and no rocket self-damage.
pub fn apply_time_trial_rules(mods: &mut PlayerModifiers, run: &mut RunStats) {
    let mut scratch = Player::default();
    for id in [UpgradeId::FiveGOverclock, UpgradeId::BlastThrusters, UpgradeId::AirplaneModePhase] {
        crate::upgrades::apply_upgrade(id, mods, &mut scratch, run);
    }
    mods.min_signal_floor = 5.0;
    mods.rocket_self_damage_mult = 0.0;
}

/// Main menu "TIME TRIAL": the handcrafted course with the trial rule set.
pub fn start_time_trial(
    run: &mut RunStats,
    mods: &mut PlayerModifiers,
    current: &mut RunLoadout,
    trial: &mut TimeTrialState,
    next: &mut NextState<AppState>,
    skin: HandsetSkin,
) {
    start_run(run, mods, next, TRIAL_SEED);
    run.mode = RunMode::TimeTrial;
    *current = RunLoadout { skin, ..RunLoadout::default() };
    apply_time_trial_rules(mods, run);
    *trial = TimeTrialState { reset_requested: true, ..default() };
}

/// Put the player at `cell` facing `yaw`, standing still (sub-frame
/// teleport: no swept jump-pad contact along the jump).
pub fn place_player(grid: &LevelGrid, p: &mut Player, tf: &mut Transform, cell: IVec2, yaw: f32) {
    let pos = grid.cell_center(cell);
    tf.translation = pos;
    tf.rotation = Quat::from_rotation_y(yaw);
    p.yaw = yaw;
    p.prev_yaw = yaw;
    p.pitch = 0.0;
    p.velocity = Vec3::ZERO;
    p.grounded = true;
    p.dash_timer = 0.0;
    p.coyote_timer = 0.0;
    p.jump_buffer_timer = 0.0;
    p.bhop_chain = 0;
    p.ground_time = 0.0;
    p.strafe_tilt = 0.0;
    p.hazard_timer = 0.0;
    p.prev_pos = Some(pos);
}

/// Respawn point after clearing `cleared` gates (0 = the start airlock).
pub fn respawn_for(grid: &LevelGrid, cleared: usize) -> (IVec2, f32) {
    match cleared {
        0 => (grid.spawn, grid.spawn_yaw),
        n => {
            let g = &time_trial_gates()[(n - 1).min(TRIAL_CHECKPOINTS)];
            (g.respawn, g.respawn_yaw)
        }
    }
}

/// Keep the trial loadout topped up: full HP, a 99-rocket rack.
fn sustain_player(p: &mut Player) {
    if p.max_rockets != TRIAL_ROCKETS {
        p.max_rockets = TRIAL_ROCKETS;
    }
    if p.ammo_rockets != TRIAL_ROCKETS {
        p.ammo_rockets = TRIAL_ROCKETS;
    }
    if p.hp != p.max_hp {
        p.hp = p.max_hp;
    }
}

// ---------------------------------------------------------------------------
// ECS
// ---------------------------------------------------------------------------

/// Shared checkpoint ring meshes + materials (built once at startup).
#[derive(Resource, Clone)]
pub struct TrialAssets {
    pub ring: Handle<Mesh>,
    pub idle: Handle<StandardMaterial>,
    pub next: Handle<StandardMaterial>,
    pub cleared: Handle<StandardMaterial>,
    pub finish: Handle<StandardMaterial>,
}

/// A checkpoint ring / finish halo (index into [`time_trial_gates`]).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrialGateMarker(pub usize);

fn glow(color: Color, emissive: LinearRgba) -> StandardMaterial {
    StandardMaterial { base_color: color, emissive, unlit: false, perceptual_roughness: 0.4, ..default() }
}

fn create_trial_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(TrialAssets {
        ring: meshes.add(Torus::new(0.88, 1.0)),
        idle: materials.add(glow(Color::srgb(0.2, 0.22, 0.24), LinearRgba::rgb(0.05, 0.12, 0.14))),
        next: materials.add(glow(Color::srgb(0.3, 0.95, 1.0), LinearRgba::rgb(0.8, 4.2, 5.0))),
        cleared: materials.add(glow(Color::srgb(0.25, 1.0, 0.45), LinearRgba::rgb(0.6, 3.6, 1.0))),
        finish: materials.add(glow(Color::srgb(1.0, 0.8, 0.28), LinearRgba::rgb(5.0, 3.4, 0.8))),
    });
}

/// Ring material for gate `i` given the attempt state.
pub fn gate_material(a: &TrialAssets, i: usize, next_gate: usize) -> Handle<StandardMaterial> {
    if i < next_gate {
        a.cleared.clone()
    } else if i == next_gate {
        if i == TRIAL_CHECKPOINTS { a.finish.clone() } else { a.next.clone() }
    } else {
        a.idle.clone()
    }
}

/// Spawn the rings once the Time Trial grid is (re)built.
fn spawn_trial_course(mut commands: Commands, assets: Option<Res<TrialAssets>>, existing: Query<Entity, With<TrialGateMarker>>) {
    let Some(a) = assets else { return };
    for e in &existing {
        commands.entity(e).despawn();
    }
    for (i, gate) in time_trial_gates().iter().enumerate() {
        let rot = if gate.vertical { Quat::from_rotation_z(std::f32::consts::FRAC_PI_2) } else { Quat::IDENTITY };
        commands.spawn((
            LevelEntity,
            TrialGateMarker(i),
            Mesh3d(a.ring.clone()),
            MeshMaterial3d(gate_material(&a, i, 0)),
            Transform::from_translation(gate.ring_center).with_rotation(rot).with_scale(Vec3::splat(gate.ring_radius)),
        ));
    }
}

/// Swap ring materials when the attempt state changes (handles only, no allocations).
fn update_gate_visuals(trial: Res<TimeTrialState>, assets: Option<Res<TrialAssets>>, mut rings: Query<(&TrialGateMarker, &mut MeshMaterial3d<StandardMaterial>)>) {
    let Some(a) = assets else { return };
    for (m, mut mat) in &mut rings {
        let want = gate_material(&a, m.0, trial.next_gate);
        if mat.0 != want {
            mat.0 = want;
        }
    }
}

/// `R` while playing the Time Trial: instant reset.
fn trial_restart_key(keys: Res<ButtonInput<KeyCode>>, mut trial: ResMut<TimeTrialState>) {
    if keys.just_pressed(KeyCode::KeyR) {
        trial.reset_requested = true;
    }
}

/// Reset requests, the attempt clock and the trial loadout.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn trial_tick(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    mut trial: ResMut<TimeTrialState>,
    mut run: ResMut<RunStats>,
    mut ws: ResMut<WeaponState>,
    mut players: Query<(&mut Player, &mut Transform)>,
    stale_fx: Query<Entity, Or<(With<PlayerProjectile>, With<Fx>)>>,
) {
    let Ok((mut p, mut tf)) = players.single_mut() else { return };
    if trial.reset_requested {
        for e in &stale_fx {
            commands.entity(e).try_despawn();
        }
        *trial = TimeTrialState::default();
        run.time_elapsed = 0.0;
        run.peak_speed = 0.0;
        run.max_bhop_chain = 0;
        run.floor_splits.clear();
        place_player(&grid, &mut p, &mut tf, grid.spawn, grid.spawn_yaw);
        p.dash_cooldown = 0.0;
        p.invuln_timer = 0.0;
        ws.current = Weapon::Rocket;
        ws.previous = Weapon::Rocket;
        ws.cooldown = 0.0;
        ws.swap_timer = 0.0;
    } else if !trial.finished {
        trial.elapsed += time.delta_secs();
    }
    sustain_player(&mut p);
}

/// Falling into a lava pit rescues the player to the last cleared checkpoint
/// (runs before `hazard_damage`, so the pit never burns in the trial).
fn trial_pit_rescue(
    grid: Res<LevelGrid>,
    mut trial: ResMut<TimeTrialState>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    mut players: Query<(&mut Player, &mut Transform)>,
) {
    let Ok((mut p, mut tf)) = players.single_mut() else { return };
    p.hazard_timer = 0.0;
    let c = grid.world_to_cell(tf.translation);
    if !matches!(grid.cell(c), Cell::HazardPit) || tf.translation.y > PIT_RESCUE_Y {
        return;
    }
    let cleared = trial.cleared();
    let (cell, yaw) = respawn_for(&grid, cleared);
    place_player(&grid, &mut p, &mut tf, cell, yaw);
    trial.rescues += 1;
    let at = if cleared == 0 { "START".to_string() } else { time_trial_gates()[cleared - 1].label.to_string() };
    feed.toast(format!("PIT RESCUE - BACK TO {at}"), HAZARD_YELLOW);
    sfx.play(SfxEvent::Denied);
}

/// Toast line of a gate crossing: `CP 2  00:31.4  -0:01.2`.
pub fn gate_toast(label: &str, time: f32, delta: Option<f32>) -> String {
    match delta {
        Some(d) => format!("{label}  {}  {}", format_split_time(time), format_split_delta(d)),
        None => format!("{label}  {}", format_split_time(time)),
    }
}

/// Tone of gate `i` crossed at `time` against the stored PB splits (allocation-free).
pub fn trial_split_tone(splits: &[f32], i: usize, best: &[f32]) -> (Option<f32>, SplitTone) {
    let Some(&t) = splits.get(i) else { return (None, SplitTone::Neutral) };
    let delta = best.get(i).map(|b| t - b);
    let seg = segment_at(splits, i);
    let best_seg = segment_at(best, i);
    (delta, split_tone(delta, seg, best_seg))
}

/// Sequential gate triggering; the finish records the attempt and opens the
/// Time Trial finish card (`Victory`).
#[allow(clippy::too_many_arguments)]
fn trial_gates(
    grid: Res<LevelGrid>,
    mut trial: ResMut<TimeTrialState>,
    run: Res<RunStats>,
    mut store: Option<ResMut<ProfileStore>>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    mut next: ResMut<NextState<AppState>>,
    players: Query<&Transform, With<Player>>,
) {
    if trial.finished || trial.reset_requested {
        return;
    }
    let Ok(tf) = players.single() else { return };
    let Some(gate) = time_trial_gates().get(trial.next_gate) else { return };
    if !gate.contains(grid.world_to_cell(tf.translation)) {
        return;
    }
    let i = trial.next_gate;
    let t = trial.elapsed;
    trial.splits.push(t);
    trial.next_gate += 1;
    let best: Vec<f32> = store.as_ref().map(|s| s.profile.time_trial_best_splits.clone()).unwrap_or_default();
    let (delta, tone) = trial_split_tone(&trial.splits, i, &best);
    if i < TRIAL_CHECKPOINTS {
        feed.toast(gate_toast(gate.label, t, delta), split_tone_color(tone));
        sfx.play(SfxEvent::Pickup);
        return;
    }
    trial.finished = true;
    trial.previous_splits = best;
    let result = match store.as_deref_mut() {
        Some(s) => {
            let r = record_time_trial(&mut s.profile, t, &trial.splits, run.peak_speed, run.max_bhop_chain);
            if let Err(e) = s.try_save() {
                warn!("profile save failed (marked dirty for retry): {e}");
            }
            r
        }
        None => TrialResult { new_pb: true, delta: None, previous_best: None },
    };
    trial.result = Some(result);
    let tail = if result.new_pb { "  NEW PB!".to_string() } else { delta.map(|d| format!("  {}", format_split_delta(d))).unwrap_or_default() };
    feed.toast(format!("FINISH  {}{tail}", format_split_time(t)), if result.new_pb { SPLIT_GOLD } else { split_tone_color(tone) });
    sfx.play(SfxEvent::Extract);
    next.set(AppState::Victory);
}

// ---------------------------------------------------------------------------
// HUD split plate model (pure; `hud.rs` renders it)
// ---------------------------------------------------------------------------

/// Split timer plate shows when enabled in Settings, and always in Daily / Time Trial.
pub fn split_timer_visible(settings: &UserSettings, mode: RunMode) -> bool {
    settings.speedrun_splits || mode != RunMode::Standard
}

/// Text + colors of the split timer plate.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitPlateView {
    pub tag: String,
    pub tag_color: Color,
    pub time: String,
    pub progress: String,
    pub delta: String,
    pub delta_color: Color,
}

/// Live split plate content for the current run.
pub fn split_plate_view(run: &RunStats, trial: &TimeTrialState, profile: &PlayerProfile) -> SplitPlateView {
    if run.mode == RunMode::TimeTrial {
        let progress = match time_trial_gates().get(trial.next_gate) {
            _ if trial.finished => "COURSE CLEAR".to_string(),
            Some(g) => format!("NEXT {}  [{}/{}]", g.label, trial.cleared(), TRIAL_CHECKPOINTS + 1),
            None => "COURSE CLEAR".to_string(),
        };
        let best = &profile.time_trial_best_splits;
        let (delta, delta_color) = match trial.splits.len().checked_sub(1) {
            Some(i) => {
                let (d, tone) = trial_split_tone(&trial.splits, i, best);
                match d {
                    Some(d) => (format!("{}  {}", time_trial_gates()[i].label, format_split_delta(d)), split_tone_color(tone)),
                    None => (format!("{}  FIRST RUN", time_trial_gates()[i].label), BONE_TEXT),
                }
            }
            None => match profile.time_trial_best_secs {
                Some(b) => (format!("PB {}", format_split_time(b)), ASH_DIM),
                None => ("NO PB - SET ONE".to_string(), ASH_DIM),
            },
        };
        return SplitPlateView {
            tag: "TIME TRIAL  [R] RESET".to_string(),
            tag_color: ARC_CYAN,
            time: format_split_time(trial.elapsed),
            progress,
            delta,
            delta_color,
        };
    }
    let (tag, tag_color) = match run.mode {
        RunMode::Daily(d) => (format!("DAILY {}", d.format_iso()), HAZARD_YELLOW),
        _ => ("SPEEDRUN // VS PB".to_string(), ACID_GREEN),
    };
    let pb = &profile.standard_pb_splits;
    let idx = run.floor.saturating_sub(1) as usize;
    let progress = if run.endless {
        format!("ENDLESS FL {}", run.floor)
    } else {
        match pb.get(idx) {
            Some(&t) => format!("FLOOR {}/{}  PB {}", run.floor, FINAL_FLOOR, format_split_time(t)),
            None => format!("FLOOR {}/{}", run.floor, FINAL_FLOOR),
        }
    };
    // Last finished floor vs its PB split; otherwise the live pace of this floor.
    let (delta, delta_color) = match (run.floor_splits.len().checked_sub(1), run.endless) {
        (Some(i), false) if i < pb.len() && i + 1 == idx => {
            let d = run.floor_splits[i] - pb[i];
            let seg = segment_at(&run.floor_splits, i);
            let tone = split_tone(Some(d), seg, profile.best_floor_segments.get(i).copied().flatten());
            (format!("FL {}  {}", i + 1, format_split_delta(d)), split_tone_color(tone))
        }
        _ => match pb.get(idx) {
            Some(&t) if !run.endless => {
                let d = run.time_elapsed - t;
                (format!("PACE  {}", format_split_delta(d)), split_tone_color(split_tone(Some(d), None, None)))
            }
            _ => ("NO PB YET".to_string(), ASH_DIM),
        },
    };
    SplitPlateView { tag, tag_color, time: format_split_time(run.time_elapsed), progress, delta, delta_color }
}

/// `F1 01:02.3 | F2 02:10.1 | ...` end-screen floor split summary.
pub fn floor_splits_line(splits: &[f32]) -> String {
    let parts: Vec<String> = splits.iter().take(8).enumerate().map(|(i, &t)| format!("F{} {}", i + 1, format_split_time(t))).collect();
    if parts.is_empty() { "SPLITS  --".to_string() } else { format!("SPLITS  {}", parts.join("  |  ")) }
}

/// `CP1 00:10.2 (-0:00.4) | ...` Time Trial finish card split summary.
pub fn trial_splits_line(splits: &[f32], best: &[f32]) -> String {
    let gates = time_trial_gates();
    let parts: Vec<String> = splits
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let label = gates.get(i).map_or("?", |g| g.label);
            match best.get(i) {
                Some(b) => format!("{label} {} ({})", format_split_time(t), format_split_delta(t - b)),
                None => format!("{label} {}", format_split_time(t)),
            }
        })
        .collect();
    parts.join("  |  ")
}

pub struct TrialsPlugin;

impl Plugin for TrialsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimeTrialState>()
            .add_systems(Startup, create_trial_assets)
            .add_systems(
                Update,
                spawn_trial_course.in_set(GameSet::World).run_if(resource_exists_and_changed::<LevelGrid>).run_if(is_time_trial),
            )
            .add_systems(
                Update,
                (trial_restart_key, trial_tick, trial_pit_rescue, trial_gates, update_gate_visuals)
                    .chain()
                    .in_set(GameSet::World)
                    .before(crate::player::hazard_damage)
                    .run_if(resource_exists::<LevelGrid>)
                    .run_if(is_time_trial),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_calendar_round_trips_known_dates() {
        assert_eq!(DailyDate::from_days_since_epoch(0), DailyDate::new(1970, 1, 1));
        assert_eq!(DailyDate::new(1970, 1, 1).days_since_epoch(), 0);
        // 2024-02-29 = day 19782; 2026-11-24 = day 20781; 2000-03-01 = 11017.
        assert_eq!(DailyDate::from_days_since_epoch(19_782), DailyDate::new(2024, 2, 29));
        assert_eq!(DailyDate::from_days_since_epoch(19_783), DailyDate::new(2024, 3, 1));
        assert_eq!(DailyDate::from_days_since_epoch(20_781), DailyDate::new(2026, 11, 24));
        assert_eq!(DailyDate::new(2000, 3, 1).days_since_epoch(), 11_017);
        assert_eq!(DailyDate::from_days_since_epoch(-1), DailyDate::new(1969, 12, 31));
        for d in (-800_000..800_000).step_by(997) {
            assert_eq!(DailyDate::from_days_since_epoch(d).days_since_epoch(), d);
        }
        assert_eq!(DailyDate::new(2026, 11, 24).format_iso(), "2026-11-24");
        assert_eq!(DailyDate::new(987, 3, 5).format_iso(), "0987-03-05");
        assert_eq!(DailyDate::new(2026, 11, 4).format_short(), "04 NOV");
        let today = DailyDate::today_utc();
        assert!(today.year >= 2024 && (1..=12).contains(&today.month) && (1..=31).contains(&today.day));
    }

    #[test]
    fn daily_seed_and_loadout_are_stable_and_vary_by_date() {
        let d = DailyDate::new(2026, 11, 24);
        assert_eq!(daily_seed(d), daily_seed(DailyDate::new(2026, 11, 24)));
        assert_eq!(daily_loadout(d), daily_loadout(d));
        let mut seeds = std::collections::BTreeSet::new();
        let mut chips = std::collections::BTreeSet::new();
        let mut counts = std::collections::BTreeSet::new();
        for k in 0..60 {
            let date = DailyDate::from_days_since_epoch(d.days_since_epoch() + k);
            seeds.insert(daily_seed(date));
            let l = daily_loadout(date);
            assert!(!l.jam_demo && l.catalog == default_catalog() && l.catalog.len() == 12);
            assert!((1..=2).contains(&l.contracts.len()) && l.contract_count() as usize == l.contracts.len(), "{l:?}");
            chips.insert(l.chip);
            counts.insert(l.contracts.len());
        }
        assert_eq!(seeds.len(), 60, "every date seeds a different bunker");
        assert!(chips.len() >= 3 && counts.len() == 2, "dailies rotate chips and contract counts");
        assert_eq!(daily_rules_line(&RunLoadout { contracts: vec![CarrierContract::RoamingCharges], ..default() }), "STOCK + ROAM");
    }

    #[test]
    fn daily_records_keep_fourteen_dates_and_best_results() {
        let mut recs = Vec::new();
        let base = DailyDate::new(2026, 11, 1);
        let run = |floor: u32, t: f32, speed: f32| RunStats { floor, time_elapsed: t, peak_speed: speed, ..default() };
        begin_daily_attempt(&mut recs, base);
        record_daily_result(&mut recs, base, &run(3, 400.0, 20.0), false);
        begin_daily_attempt(&mut recs, base);
        record_daily_result(&mut recs, base, &run(5, 900.0, 18.0), true);
        record_daily_result(&mut recs, base, &run(5, 700.0, 25.0), true);
        record_daily_result(&mut recs, base, &run(2, 100.0, 1.0), false);
        let r = daily_record(&recs, base).unwrap();
        assert_eq!((r.attempts, r.best_floor, r.victory, r.best_time_secs, r.peak_speed), (2, 5, true, Some(700.0), 25.0));
        // Endless extension of a victory raises the best floor without a new attempt.
        record_daily_result(&mut recs, base, &run(8, 1200.0, 10.0), false);
        assert_eq!(daily_record(&recs, base).unwrap().best_floor, 8);
        assert_eq!(daily_record(&recs, base).unwrap().attempts, 2);
        for k in 1..20 {
            let date = DailyDate::from_days_since_epoch(base.days_since_epoch() + k);
            begin_daily_attempt(&mut recs, date);
        }
        assert_eq!(recs.len(), DAILY_RECORDS_KEPT);
        assert!(daily_record(&recs, base).is_none(), "oldest dates drop out");
        assert!(recs.windows(2).all(|w| w[0].date < w[1].date), "sorted oldest first");
        assert_eq!(recs.last().unwrap().date, DailyDate::new(2026, 11, 20));
        // A late result for an expired date is not resurrected past the cap.
        let before = recs.clone();
        record_daily_result(&mut recs, base, &run(1, 10.0, 1.0), false);
        assert_eq!(recs, before, "an expired date never touches the kept records");
    }

    #[test]
    fn split_formatting_and_delta_signs() {
        assert_eq!(format_split_time(0.0), "00:00.0");
        assert_eq!(format_split_time(83.47), "01:23.4");
        assert_eq!(format_split_time(599.99), "09:59.9");
        assert_eq!(format_split_time(6000.0), "100:00.0");
        assert_eq!(format_split_time(-3.0), "00:00.0");
        assert_eq!(format_split_time(f32::NAN), "00:00.0");
        assert_eq!(format_split_delta(-4.2), "-0:04.2");
        assert_eq!(format_split_delta(2.1), "+0:02.1");
        assert_eq!(format_split_delta(-75.25), "-1:15.3");
        assert_eq!(format_split_delta(0.0), "+0:00.0");
        assert_eq!(format_split_delta(-0.01), "+0:00.0", "no negative zero");
        assert_eq!(format_split_delta(f32::INFINITY), "+0:00.0");
    }

    #[test]
    fn split_tones_and_colors() {
        assert_eq!(split_tone(Some(-1.0), Some(30.0), Some(31.0)), SplitTone::BestSegment);
        assert_eq!(split_tone(Some(2.0), Some(30.0), Some(29.0)), SplitTone::Behind);
        assert_eq!(split_tone(Some(-2.0), Some(30.0), Some(29.0)), SplitTone::Ahead);
        assert_eq!(split_tone(Some(0.0), None, None), SplitTone::Ahead);
        assert_eq!(split_tone(None, Some(1.0), None), SplitTone::Neutral);
        assert_eq!(split_tone(Some(3.0), Some(10.0), Some(12.0)), SplitTone::BestSegment, "gold beats a slower cumulative");
        assert_eq!(split_tone_color(SplitTone::Ahead), ACID_GREEN);
        assert_eq!(split_tone_color(SplitTone::BestSegment), SPLIT_GOLD);
        assert_eq!(split_tone_color(SplitTone::Behind), PUNK_RED);
        assert_eq!(segments_from_splits(&[10.0, 25.0, 45.0]), vec![10.0, 15.0, 20.0]);
    }

    #[test]
    fn standard_pb_splits_and_best_segments() {
        let mut p = PlayerProfile::default();
        record_standard_splits(&mut p, &[60.0, 150.0], false);
        assert!(p.standard_pb_splits.is_empty(), "a death never sets the PB");
        assert_eq!(p.best_floor_segments, [Some(60.0), Some(90.0), None, None, None]);
        record_standard_splits(&mut p, &[70.0, 140.0, 200.0, 260.0, 330.0], true);
        assert_eq!(p.standard_pb_splits, vec![70.0, 140.0, 200.0, 260.0, 330.0]);
        assert_eq!(p.best_floor_segments, [Some(60.0), Some(70.0), Some(60.0), Some(60.0), Some(70.0)]);
        record_standard_splits(&mut p, &[50.0, 140.0, 210.0, 280.0, 350.0], true);
        assert_eq!(p.standard_pb_splits[4], 330.0, "slower victory keeps the PB");
        assert_eq!(p.best_floor_segments[0], Some(50.0));
        record_standard_splits(&mut p, &[50.0, 110.0, 170.0, 230.0, 300.0, 360.0], true);
        assert_eq!(p.standard_pb_splits, vec![50.0, 110.0, 170.0, 230.0, 300.0], "endless splits are cut at floor 5");
    }

    #[test]
    fn time_trial_records_pb_and_movement_only() {
        let mut p = PlayerProfile::default();
        let r = record_time_trial(&mut p, 50.0, &[10.0, 20.0, 35.0, 50.0], 31.0, 6);
        assert_eq!(r, TrialResult { new_pb: true, delta: None, previous_best: None });
        let r = record_time_trial(&mut p, 55.0, &[9.0, 22.0, 40.0, 55.0], 20.0, 2);
        assert!(!r.new_pb && r.delta == Some(5.0));
        assert_eq!(p.time_trial_best_splits, vec![10.0, 20.0, 35.0, 50.0]);
        let r = record_time_trial(&mut p, 44.5, &[8.0, 18.0, 30.0, 44.5], 12.0, 1);
        assert!(r.new_pb && r.delta == Some(-5.5));
        assert_eq!(p.time_trial_best_secs, Some(44.5));
        assert_eq!((p.stats.best_speed, p.stats.best_bhop_chain), (31.0, 6));
        assert_eq!((p.stats.runs_played, p.stats.victories), (0, 0));
        assert_eq!(trial_splits_line(&[8.0, 19.0], &[8.5, 18.0]), "CP 1 00:08.0 (-0:00.5)  |  CP 2 00:19.0 (+0:01.0)");
        assert_eq!(floor_splits_line(&[61.0, 125.5]), "SPLITS  F1 01:01.0  |  F2 02:05.5");
    }

    #[test]
    fn time_trial_course_is_valid_and_reachable() {
        let g = build_time_trial_grid();
        assert_eq!((g.width, g.depth), (TRIAL_WIDTH, TRIAL_DEPTH));
        assert!(g.validate(), "spawn walks to the tower");
        assert!(matches!(g.cell(g.spawn), Cell::Floor { height } if height == 0.0));
        assert_eq!(g.cell(g.tower), Cell::Floor { height: CATWALK_HEIGHT });
        assert!(g.is_roof_open(g.tower) && g.jammers.is_empty() && g.barrels.is_empty());
        assert_eq!(g.rooms.len(), 1, "only the atrium: no floor loot");
        // Border is solid.
        for x in 0..g.width {
            assert!(g.cell(IVec2::new(x, 0)).is_wall() && g.cell(IVec2::new(x, g.depth - 1)).is_wall());
        }
        for z in 0..g.depth {
            assert!(g.cell(IVec2::new(0, z)).is_wall() && g.cell(IVec2::new(g.width - 1, z)).is_wall());
        }
        // Pads sit on standable floor and launch onto standable floor.
        assert_eq!(g.jump_pads.len(), 3);
        for pad in &g.jump_pads {
            assert!(matches!(g.cell(pad.cell), Cell::Floor { height } if height == 0.0) || matches!(g.cell(pad.cell), Cell::AtriumFloor));
            assert!(pad.launch_vel.y > 9.0);
        }
        assert!(matches!(g.cell(g.jump_pads[0].cell + IVec2::NEG_Y * 2), Cell::Floor { height } if height > 2.0), "bay pad vaults onto the catwalk");
        // Every gate is walk-reachable from the start, in course order, and
        // each gate fully spans its corridor (no way around it).
        let gates = time_trial_gates();
        let d = g.walk_distances(g.spawn);
        let dist = |c: IVec2| d[(c.y * g.width + c.x) as usize];
        let mut last = 0;
        for gate in gates {
            for &c in &gate.cells {
                assert!(!g.cell(c).is_wall(), "{} cell {c} open", gate.label);
            }
            let near = gate.cells.iter().filter_map(|&c| dist(c)).min().unwrap_or_else(|| panic!("{} reachable", gate.label));
            assert!(near > last, "{} after the previous gate", gate.label);
            last = near;
            assert!(!g.cell(gate.respawn).is_wall() && !matches!(g.cell(gate.respawn), Cell::HazardPit));
            assert!(g.is_reachable(gate.respawn, g.tower), "{} respawn reaches the finish", gate.label);
        }
        for gate in &gates[..TRIAL_CHECKPOINTS] {
            // Blocking the gate's cells disconnects the start from the finish.
            let mut cut = g.clone();
            for &c in &gate.cells {
                cut.set(c, Cell::Wall);
            }
            assert!(!cut.is_reachable(g.spawn, g.tower), "{} cannot be bypassed", gate.label);
        }
        // The lava trench has a launch pad and lies on the route.
        assert!((10..=11).all(|x| matches!(g.cell(IVec2::new(x, 4)), Cell::HazardPit)));
        assert_eq!(g.jump_pads[1].cell, IVec2::new(9, 4));
        // Pit hall + atrium are open-roof (rocket-jump headroom).
        assert!(g.is_roof_open(IVec2::new(10, 4)) && g.is_roof_open(IVec2::new(19, 1)));
        assert_eq!(respawn_for(&g, 0), (g.spawn, g.spawn_yaw));
        assert_eq!(respawn_for(&g, 2).0, IVec2::new(8, 9));
    }

    #[test]
    fn trial_rules_install_movement_kit() {
        let mut mods = PlayerModifiers::default();
        let mut run = RunStats::default();
        apply_time_trial_rules(&mut mods, &mut run);
        assert_eq!(run.upgrades, vec![UpgradeId::FiveGOverclock, UpgradeId::BlastThrusters, UpgradeId::AirplaneModePhase]);
        assert_eq!((mods.min_signal_floor, mods.rocket_self_damage_mult), (5.0, 0.0));
        assert!(mods.dash_invulnerable && mods.overclock_move_bonus > 0.0 && mods.rocket_jump_mult > 1.0);
        let mut p = Player { hp: 3.0, ammo_rockets: 0, ..default() };
        sustain_player(&mut p);
        assert_eq!((p.hp, p.ammo_rockets, p.max_rockets), (p.max_hp, TRIAL_ROCKETS, TRIAL_ROCKETS));
    }

    #[test]
    fn split_plate_views() {
        let mut p = PlayerProfile::default();
        let s = UserSettings::default();
        assert!(!split_timer_visible(&s, RunMode::Standard));
        assert!(split_timer_visible(&s, RunMode::TimeTrial) && split_timer_visible(&s, RunMode::Daily(DailyDate::EPOCH)));
        assert!(split_timer_visible(&UserSettings { speedrun_splits: true, ..default() }, RunMode::Standard));
        let mut run = RunStats { time_elapsed: 65.0, ..default() };
        let v = split_plate_view(&run, &TimeTrialState::default(), &p);
        assert_eq!((v.time.as_str(), v.progress.as_str(), v.delta.as_str()), ("01:05.0", "FLOOR 1/5", "NO PB YET"));
        p.standard_pb_splits = vec![60.0, 120.0, 180.0, 240.0, 300.0];
        p.best_floor_segments = [Some(50.0), Some(55.0), None, None, None];
        let v = split_plate_view(&run, &TimeTrialState::default(), &p);
        assert_eq!((v.progress.as_str(), v.delta.as_str(), v.delta_color), ("FLOOR 1/5  PB 01:00.0", "PACE  +0:05.0", PUNK_RED));
        run.time_elapsed = 58.0;
        run.advance_floor();
        let v = split_plate_view(&run, &TimeTrialState::default(), &p);
        assert_eq!((v.delta.as_str(), v.delta_color), ("FL 1  -0:02.0", ACID_GREEN));
        run.mode = RunMode::Daily(DailyDate::new(2026, 11, 24));
        assert_eq!(split_plate_view(&run, &TimeTrialState::default(), &p).tag, "DAILY 2026-11-24");
        run.mode = RunMode::TimeTrial;
        let trial = TimeTrialState { elapsed: 12.34, next_gate: 1, splits: vec![9.0], ..default() };
        let v = split_plate_view(&run, &trial, &p);
        assert_eq!((v.time.as_str(), v.progress.as_str(), v.delta.as_str()), ("00:12.3", "NEXT CP 2  [1/4]", "CP 1  FIRST RUN"));
        p.time_trial_best_splits = vec![10.0, 20.0, 30.0, 40.0];
        let v = split_plate_view(&run, &trial, &p);
        assert_eq!((v.delta.as_str(), v.delta_color), ("CP 1  -0:01.0", SPLIT_GOLD));
    }

    #[test]
    fn legacy_profile_without_m18_fields_loads() {
        let old = r#"{
            "schema_version": 1,
            "unlocked_feats": ["FirstExtraction"],
            "stats": {"runs_played": 9, "victories": 1, "best_speed": 28.5, "best_bhop_chain": 7, "best_endless_floor": 7},
            "history": [{"floor": 6, "victory": true, "kills": 80, "jammers": 9, "time_secs": 800.0, "chip": "StockUac",
                         "contracts_count": 1, "skin": "UacOrange", "endless": true, "peak_speed": 28.5, "max_bhop_chain": 7}],
            "settings": {"mouse_sensitivity": 1.2, "crosshair_style": "Dot"}
        }"#;
        let p = crate::meta::parse_profile(old).expect("M17 profile loads");
        assert!(p.daily_records.is_empty() && p.standard_pb_splits.is_empty() && p.time_trial_best_splits.is_empty());
        assert_eq!(p.best_floor_segments, [None; 5]);
        assert_eq!(p.time_trial_best_secs, None);
        assert!(!p.settings.speedrun_splits);
        assert_eq!(p.history[0].daily_date, None);
        assert_eq!((p.stats.runs_played, p.settings.mouse_sensitivity), (9, 1.2));
        // Round trip with every M18 field populated.
        let mut q = p.clone();
        q.settings.speedrun_splits = true;
        q.daily_records = vec![DailyRecord { date: DailyDate::new(2026, 11, 24), best_floor: 3, attempts: 2, ..default() }];
        q.standard_pb_splits = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        q.best_floor_segments = [Some(1.0), None, Some(3.0), None, None];
        q.time_trial_best_secs = Some(42.0);
        q.time_trial_best_splits = vec![10.0, 20.0, 30.0, 42.0];
        q.history[0].daily_date = Some(DailyDate::new(2026, 11, 24));
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(crate::meta::parse_profile(&json).unwrap(), q);
        // Junk records are dropped on load.
        let bad = r#"{"time_trial_best_secs": -5.0, "time_trial_best_splits": [1.0], "standard_pb_splits": [-1.0]}"#;
        let b = crate::meta::parse_profile(bad).unwrap();
        assert!(b.time_trial_best_secs.is_none() && b.time_trial_best_splits.is_empty() && b.standard_pb_splits.is_empty());
    }
}
