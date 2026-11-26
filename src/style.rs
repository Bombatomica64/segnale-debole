//! Milestone 31: DMC-style **Bandwidth Style Rank** meter.
//!
//! Every stylish action (demon kills, multi-kills, airborne / dash / bhop
//! kills, ping-marked kills, jammer kills, boss kills, fatalities, CRT
//! shatters, UPS rack detonations and their chain reactions) pushes a
//! [`StyleAction`] into the [`StyleQueue`]. [`style_update`] folds the queue
//! into the run's [`StyleMeter`]:
//!
//! - **7-tier telecom ladder** ([`StyleRank`]): `D / EDGE` .. `SSS / 6G+`.
//! - **Freshness**: repeating the same weapon / source back to back earns less
//!   ([`FRESH_REPEAT_STEP`], floored at [`FRESH_MIN`]); switching weapons or
//!   mixing props / apps / fatalities earns a [`FRESH_SWITCH_BONUS`].
//! - **Decay**: after [`DECAY_GRACE`] idle seconds the points bleed at a
//!   rank-scaled rate ([`StyleRank::decay_rate`]), much faster at `S`/`SS`/`SSS`
//!   so holding `6G` / `6G+` demands nonstop aggression.
//! - **Damage penalty**: taking damage drops the meter to the floor of the
//!   previous tier (a clean one-rank demotion, `D` empties).
//! - **Run-scoped**: reset on every pre-run screen and on a fresh run's first
//!   `OnEnter(Playing)` (Floor 1 at `t = 0`, which also covers Pause -> RESTART
//!   [R]); preserved across `Paused <-> Playing` and `UpgradeSelect -> Playing`.
//!   The best tier reached is kept on [`RunStats::peak_style_rank`] for the
//!   end cards.
//!
//! The Time Trial never scores style (no demons, clean speedrun HUD).

use bevy::prelude::*;

use crate::audio::{SfxEvent, SfxQueue};
use crate::enemies::EnemyKind;
use crate::player::Player;
use crate::rumble::{RumblePulse, RumbleQueue};
use crate::state::{AppState, GameSet, RunMode, RunStats};
use crate::weapons::Weapon;

/// The seven-tier DMC-inspired telecom ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StyleRank {
    D,
    C,
    B,
    A,
    S,
    SS,
    SSS,
}

/// Points cap (a little headroom above the `SSS` threshold).
pub const STYLE_MAX_POINTS: f32 = 1600.0;
/// Idle seconds before the meter starts to bleed.
pub const DECAY_GRACE: f32 = 1.2;
/// Seconds over which decay ramps in after the grace (smooth, no cliff).
pub const DECAY_RAMP: f32 = 0.5;
/// Seconds after a hit during which further hits do not demote again
/// (hazard ticks / shotgun pellets never cascade through the ladder).
pub const HIT_GRACE: f32 = 0.35;
/// Consecutive kills inside this window (s) chain into multi-kills.
pub const MULTI_KILL_WINDOW: f32 = 0.9;
/// Freshness loss per back-to-back repeat of the same source.
pub const FRESH_REPEAT_STEP: f32 = 0.22;
/// Freshness floor (spamming one gun still earns something).
pub const FRESH_MIN: f32 = 0.35;
/// Gain multiplier for switching weapon / mixing sources.
pub const FRESH_SWITCH_BONUS: f32 = 1.25;
/// After this long without a sourced action the freshness history resets.
pub const FRESH_MEMORY: f32 = 6.0;
/// Horizontal speed (m/s) for a high-speed bhop kill.
pub const STYLE_BHOP_SPEED: f32 = 18.0;
/// Seconds a style feed line stays up.
pub const FEED_SECS: f32 = 2.6;
/// Style feed lines shown at once.
pub const FEED_LINES: usize = 3;

impl StyleRank {
    pub const ALL: [StyleRank; 7] = [StyleRank::D, StyleRank::C, StyleRank::B, StyleRank::A, StyleRank::S, StyleRank::SS, StyleRank::SSS];

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(i: u8) -> StyleRank {
        StyleRank::ALL[(i as usize).min(6)]
    }

    /// Giant HUD letter.
    pub fn letter(self) -> &'static str {
        match self {
            StyleRank::D => "D",
            StyleRank::C => "C",
            StyleRank::B => "B",
            StyleRank::A => "A",
            StyleRank::S => "S",
            StyleRank::SS => "SS",
            StyleRank::SSS => "SSS",
        }
    }

    /// Cellular generation tag.
    pub fn tag(self) -> &'static str {
        match self {
            StyleRank::D => "EDGE",
            StyleRank::C => "3G",
            StyleRank::B => "4G",
            StyleRank::A => "5G",
            StyleRank::S => "FIBER",
            StyleRank::SS => "6G",
            StyleRank::SSS => "6G+",
        }
    }

    /// Punchy subtitle.
    pub fn subtitle(self) -> &'static str {
        match self {
            StyleRank::D => "DEAD SLOW",
            StyleRank::C => "CONNECTED",
            StyleRank::B => "BROADBAND",
            StyleRank::A => "ACCELERATED",
            StyleRank::S => "STYLISH!!",
            StyleRank::SS => "SIGNAL SUPREME!!",
            StyleRank::SSS => "SMOKING SEXY SIGNAL!!",
        }
    }

    /// Lowest point total of this tier (`D` starts above zero).
    pub fn threshold(self) -> f32 {
        match self {
            StyleRank::D => 0.0,
            StyleRank::C => 120.0,
            StyleRank::B => 260.0,
            StyleRank::A => 440.0,
            StyleRank::S => 660.0,
            StyleRank::SS => 920.0,
            StyleRank::SSS => 1220.0,
        }
    }

    /// Top of this tier (the next threshold, or the cap).
    pub fn ceiling(self) -> f32 {
        self.next().map_or(STYLE_MAX_POINTS, StyleRank::threshold)
    }

    pub fn next(self) -> Option<StyleRank> {
        StyleRank::ALL.get(self as usize + 1).copied()
    }

    pub fn prev(self) -> Option<StyleRank> {
        (self as usize).checked_sub(1).map(|i| StyleRank::ALL[i])
    }

    /// Idle bleed (points / s): faster at the top so `6G+` must be earned nonstop.
    pub fn decay_rate(self) -> f32 {
        match self {
            StyleRank::D => 14.0,
            StyleRank::C => 20.0,
            StyleRank::B => 26.0,
            StyleRank::A => 34.0,
            StyleRank::S => 60.0,
            StyleRank::SS => 85.0,
            StyleRank::SSS => 120.0,
        }
    }

    /// Tier of a point total (`None` = empty meter).
    pub fn from_points(points: f32) -> Option<StyleRank> {
        if points.is_nan() || points <= 0.0 {
            return None;
        }
        StyleRank::ALL.iter().rev().copied().find(|r| points >= r.threshold())
    }

    /// `S` and above: the 6G Overclock music layer and the crisp peak stinger.
    pub fn is_overclock(self) -> bool {
        self >= StyleRank::S
    }

    /// Glow color of the rank letter.
    pub fn color(self) -> Color {
        match self {
            StyleRank::D => Color::srgb(0.62, 0.58, 0.55),
            StyleRank::C => Color::srgb(0.35, 0.95, 1.0),
            StyleRank::B => Color::srgb(0.4, 1.0, 0.45),
            StyleRank::A => Color::srgb(1.0, 0.82, 0.25),
            StyleRank::S => Color::srgb(1.0, 0.5, 0.12),
            StyleRank::SS => Color::srgb(1.0, 0.18, 0.32),
            StyleRank::SSS => Color::srgb(1.0, 0.35, 0.95),
        }
    }
}

/// What dealt the stylish blow (weapon freshness tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StyleSource {
    #[default]
    Other,
    Shotgun,
    Plasma,
    Rocket,
    /// 5G Overload EMP / chain lightning / Store & Forward arcs.
    Overload,
    /// Airplane Mode dash shockwave.
    DashShock,
    /// Coolant barrel blast.
    Barrel,
    /// CRT / UPS rack (props.rs).
    Prop,
}

impl StyleSource {
    pub fn of_weapon(w: Weapon) -> StyleSource {
        match w {
            Weapon::Shotgun => StyleSource::Shotgun,
            Weapon::Plasma => StyleSource::Plasma,
            Weapon::Rocket => StyleSource::Rocket,
        }
    }

    pub fn is_weapon(self) -> bool {
        matches!(self, StyleSource::Shotgun | StyleSource::Plasma | StyleSource::Rocket)
    }
}

/// One stylish action, pushed by combat / props systems.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StyleAction {
    Kill {
        kind: EnemyKind,
        source: StyleSource,
        /// Airborne or mid-dash.
        airborne: bool,
        /// Horizontal speed at the kill (m/s).
        speed: f32,
        marked: bool,
        fatality: bool,
    },
    /// A Demon Jammer tower went down.
    JammerDestroyed,
    /// A CRT monitor shattered (small tick).
    CrtShatter,
    /// A Lithium UPS rack detonated (`chain`: set off by another rack's blast).
    UpsDetonation { chain: bool },
}

/// Frame queue of style actions (drained by [`style_update`]).
#[derive(Resource, Debug, Default)]
pub struct StyleQueue(pub Vec<StyleAction>);

impl StyleQueue {
    pub fn push(&mut self, a: StyleAction) {
        self.0.push(a);
    }
}

/// Base points of a demon kill.
pub fn kill_points(kind: EnemyKind) -> f32 {
    match kind {
        EnemyKind::Imp => 42.0,
        EnemyKind::Cacodemon => 58.0,
        EnemyKind::JammerFiend => 70.0,
        EnemyKind::HellBaron => 110.0,
        EnemyKind::PhaseStalker => 64.0,
        EnemyKind::RelaySniper => 64.0,
        EnemyKind::CarrierDaemon => 260.0,
    }
}

/// The live run's style meter.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct StyleMeter {
    pub points: f32,
    /// Seconds since the last gain (drives decay).
    pub idle: f32,
    /// Remaining hit grace (no further demotion while > 0).
    pub hit_grace: f32,
    /// Meter clock (seconds of `Playing` since the reset).
    pub clock: f32,
    pub last_source: Option<StyleSource>,
    pub last_source_at: f32,
    /// Back-to-back repeats of `last_source`.
    pub repeats: u32,
    pub last_kill_at: f32,
    /// Kills chained inside [`MULTI_KILL_WINDOW`] (0 = single).
    pub multi: u32,
    /// Newest-first style feed lines `(label, age)`.
    pub feed: [Option<(&'static str, f32)>; FEED_LINES],
    /// Rank-up pop envelope (1 on a climb, decays to 0; HUD scale pop).
    pub pop: f32,
    /// Best tier this run (mirrored to [`RunStats::peak_style_rank`]).
    pub peak: Option<StyleRank>,
    /// Hits that demoted the meter (telemetry / tests).
    pub hits_taken: u32,
}

impl Default for StyleMeter {
    fn default() -> Self {
        StyleMeter {
            points: 0.0,
            idle: 0.0,
            hit_grace: 0.0,
            clock: 0.0,
            last_source: None,
            last_source_at: f32::NEG_INFINITY,
            repeats: 0,
            last_kill_at: f32::NEG_INFINITY,
            multi: 0,
            feed: [None; FEED_LINES],
            pop: 0.0,
            peak: None,
            hits_taken: 0,
        }
    }
}

impl StyleMeter {
    pub fn rank(&self) -> Option<StyleRank> {
        StyleRank::from_points(self.points)
    }

    /// 0..1 fill of the current tier (the HUD decay bar).
    pub fn tier_fill(&self) -> f32 {
        match self.rank() {
            None => 0.0,
            Some(r) => ((self.points - r.threshold()) / (r.ceiling() - r.threshold()).max(1.0)).clamp(0.0, 1.0),
        }
    }

    /// Show a line in the style feed (newest first).
    pub fn feed_line(&mut self, label: &'static str) {
        self.feed.rotate_right(1);
        self.feed[0] = Some((label, 0.0));
    }

    /// Freshness multiplier of an action from `src` (and the feed callout of
    /// a switch). Updates the source history.
    pub fn freshness(&mut self, src: StyleSource) -> (f32, Option<&'static str>) {
        if src == StyleSource::Other {
            return (1.0, None);
        }
        if self.clock - self.last_source_at > FRESH_MEMORY {
            self.last_source = None;
            self.repeats = 0;
        }
        let out = match self.last_source {
            None => (1.0, None),
            Some(s) if s == src => {
                self.repeats += 1;
                ((1.0 - FRESH_REPEAT_STEP * self.repeats as f32).max(FRESH_MIN), None)
            }
            Some(s) => {
                self.repeats = 0;
                let label = if s.is_weapon() && src.is_weapon() { "WEAPON SWITCH" } else { "STYLE MIX" };
                (FRESH_SWITCH_BONUS, Some(label))
            }
        };
        self.last_source = Some(src);
        self.last_source_at = self.clock;
        out
    }

    fn gain(&mut self, pts: f32) {
        if pts > 0.0 && pts.is_finite() {
            self.points = (self.points + pts).min(STYLE_MAX_POINTS);
            self.idle = 0.0;
            if let Some(r) = self.rank() {
                self.peak = Some(self.peak.map_or(r, |p| p.max(r)));
            }
        }
    }

    /// Score one action. Returns the points gained.
    pub fn award(&mut self, action: StyleAction) -> f32 {
        let before = self.points;
        match action {
            StyleAction::Kill { kind, source, airborne, speed, marked, fatality } => {
                let mut pts = kill_points(kind);
                let mut label: Option<&'static str> = None;
                // Rapid multi-kills.
                if self.clock - self.last_kill_at <= MULTI_KILL_WINDOW {
                    self.multi += 1;
                    pts += 22.0 * self.multi.min(4) as f32;
                    label = Some(match self.multi {
                        1 => "DOUBLE KILL",
                        2 => "TRIPLE KILL",
                        _ => "MULTI KILL",
                    });
                } else {
                    self.multi = 0;
                }
                self.last_kill_at = self.clock;
                if airborne {
                    pts += 34.0;
                    label = label.or(Some("AIRBORNE KILL"));
                }
                if speed.is_finite() && speed >= STYLE_BHOP_SPEED {
                    pts += 26.0;
                    label = label.or(Some("BHOP KILL"));
                }
                if marked {
                    pts += 16.0;
                    label = label.or(Some("MARKED KILL"));
                }
                if kind == EnemyKind::JammerFiend {
                    pts += 30.0;
                    label = label.or(Some("JAMMER KILL"));
                }
                if kind == EnemyKind::CarrierDaemon {
                    pts += 300.0;
                    label = Some("CARRIER DISCONNECTED");
                }
                if fatality {
                    pts += 70.0;
                    label = label.or(Some("FATALITY"));
                }
                if source == StyleSource::Prop {
                    label = label.or(Some("PROP KILL"));
                }
                let (fresh, switch) = self.freshness(source);
                self.gain(pts * fresh);
                if let Some(l) = switch {
                    self.feed_line(l);
                }
                if let Some(l) = label {
                    self.feed_line(l);
                }
            }
            StyleAction::JammerDestroyed => {
                self.gain(90.0);
                self.feed_line("JAMMER DOWN");
            }
            StyleAction::CrtShatter => {
                self.gain(12.0);
                self.feed_line("CRT SHATTERED");
            }
            StyleAction::UpsDetonation { chain } => {
                let (fresh, switch) = self.freshness(StyleSource::Prop);
                self.gain(if chain { 105.0 } else { 60.0 } * fresh.max(1.0));
                if let Some(l) = switch {
                    self.feed_line(l);
                }
                self.feed_line(if chain { "CHAIN REACTION" } else { "UPS CHAIN BLAST" });
            }
        }
        self.points - before
    }

    /// DMC damage penalty: drop to the floor of the previous tier (`D`
    /// empties). Returns true if the meter was demoted.
    pub fn take_hit(&mut self) -> bool {
        if self.hit_grace > 0.0 || self.points <= 0.0 {
            return false;
        }
        self.hit_grace = HIT_GRACE;
        self.points = match self.rank().and_then(StyleRank::prev) {
            Some(p) => p.threshold().max(1.0),
            None => 0.0,
        };
        self.multi = 0;
        self.hits_taken += 1;
        self.feed_line("SIGNAL LOST");
        true
    }

    /// Advance clocks, decay and the feed.
    pub fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.clock += dt;
        self.idle += dt;
        self.hit_grace = (self.hit_grace - dt).max(0.0);
        self.pop = (self.pop - dt * 2.2).max(0.0);
        if self.idle > DECAY_GRACE
            && let Some(r) = self.rank()
        {
            let ramp = ((self.idle - DECAY_GRACE) / DECAY_RAMP).clamp(0.0, 1.0);
            self.points = (self.points - r.decay_rate() * ramp * dt).max(0.0);
        }
        for slot in &mut self.feed {
            if let Some((_, age)) = slot {
                *age += dt;
                if *age > FEED_SECS {
                    *slot = None;
                }
            }
        }
    }
}

/// Reset the meter on a fresh run's first `OnEnter(Playing)` (Floor 1 at
/// `t = 0`: main menu START, Pause RESTART [R], Daily / Loadout launches).
/// Resuming from Pause or entering the next floor keeps it.
pub fn reset_style_on_fresh_run(run: Res<RunStats>, mut meter: ResMut<StyleMeter>, mut queue: ResMut<StyleQueue>) {
    if run.floor == 1 && run.time_elapsed == 0.0 {
        *meter = StyleMeter::default();
        queue.0.clear();
    }
}

/// Reset on every pre-run screen.
pub fn reset_style(mut meter: ResMut<StyleMeter>, mut queue: ResMut<StyleQueue>) {
    *meter = StyleMeter::default();
    queue.0.clear();
}

/// Fold the frame's actions, the damage penalty and decay into the meter.
#[allow(clippy::too_many_arguments)]
pub fn style_update(
    time: Res<Time>,
    mut meter: ResMut<StyleMeter>,
    mut queue: ResMut<StyleQueue>,
    mut run: ResMut<RunStats>,
    mut sfx: ResMut<SfxQueue>,
    mut rumble: ResMut<RumbleQueue>,
    players: Query<&Player>,
    mut last_hits: Local<Option<u32>>,
) {
    if run.mode == RunMode::TimeTrial {
        queue.0.clear();
        if meter.points != 0.0 {
            *meter = StyleMeter::default();
        }
        *last_hits = players.single().ok().map(|p| p.damage_hits);
        return;
    }
    let before = meter.rank();
    for a in queue.0.drain(..) {
        meter.award(a);
    }
    // DMC damage penalty: driven directly by `Player::damage_hits` so same-frame
    // healing (siphon / medkit) never masks a real hit.
    if let Ok(p) = players.single() {
        let prev = last_hits.unwrap_or(0);
        if p.damage_hits > prev {
            for _ in 0..(p.damage_hits - prev).min(4) {
                meter.take_hit();
            }
        }
        *last_hits = Some(p.damage_hits);
    } else {
        *last_hits = None;
    }
    let after = meter.rank();
    if let Some(r) = after
        && before.is_none_or(|b| r > b)
    {
        meter.pop = 1.0;
        if r > StyleRank::D {
            sfx.play(if r.is_overclock() { SfxEvent::StylePeak } else { SfxEvent::StyleRankUp });
            rumble.push(RumblePulse::StyleRankUp);
        }
    }
    meter.tick(time.delta_secs());
    if let Some(p) = meter.peak
        && run.peak_style_rank.is_none_or(|r| p > r)
    {
        run.peak_style_rank = Some(p);
    }
}

pub struct StylePlugin;

impl Plugin for StylePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StyleMeter>()
            .init_resource::<StyleQueue>()
            .add_systems(OnEnter(AppState::Playing), reset_style_on_fresh_run)
            .add_systems(
                Update,
                style_update.in_set(GameSet::Combat).after(crate::enemies::EnemyDeathsSet).after(crate::weapons::CombatDamage),
            );
        for s in [AppState::MainMenu, AppState::Loadout, AppState::ContractsFeats, AppState::Leaderboard, AppState::Compendium] {
            app.add_systems(OnEnter(s), reset_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kill(kind: EnemyKind, source: StyleSource) -> StyleAction {
        StyleAction::Kill { kind, source, airborne: false, speed: 0.0, marked: false, fatality: false }
    }

    #[test]
    fn ladder_has_seven_telecom_tiers_including_6g_and_6g_plus() {
        let tags: Vec<_> = StyleRank::ALL.iter().map(|r| (r.letter(), r.tag(), r.subtitle())).collect();
        assert_eq!(
            tags,
            vec![
                ("D", "EDGE", "DEAD SLOW"),
                ("C", "3G", "CONNECTED"),
                ("B", "4G", "BROADBAND"),
                ("A", "5G", "ACCELERATED"),
                ("S", "FIBER", "STYLISH!!"),
                ("SS", "6G", "SIGNAL SUPREME!!"),
                ("SSS", "6G+", "SMOKING SEXY SIGNAL!!"),
            ]
        );
        for w in StyleRank::ALL.windows(2) {
            assert!(w[0].threshold() < w[1].threshold());
            assert!(w[0].decay_rate() < w[1].decay_rate(), "higher tiers bleed faster");
            assert_eq!(w[0].next(), Some(w[1]));
            assert_eq!(w[1].prev(), Some(w[0]));
        }
        assert!(StyleRank::S.decay_rate() >= StyleRank::A.decay_rate() * 1.5, "S/SS/SSS decay much faster");
        assert_eq!(StyleRank::from_points(0.0), None);
        assert_eq!(StyleRank::from_points(f32::NAN), None);
        assert_eq!(StyleRank::from_points(1.0), Some(StyleRank::D));
        assert_eq!(StyleRank::from_points(925.0), Some(StyleRank::SS));
        assert_eq!(StyleRank::from_points(STYLE_MAX_POINTS), Some(StyleRank::SSS));
        assert!(StyleRank::S.is_overclock() && StyleRank::SSS.is_overclock() && !StyleRank::A.is_overclock());
        for r in StyleRank::ALL {
            assert_eq!(StyleRank::from_index(r.index()), r);
        }
    }

    #[test]
    fn nonstop_mixed_aggression_climbs_to_6g_plus() {
        let mut m = StyleMeter::default();
        let sources = [StyleSource::Shotgun, StyleSource::Rocket, StyleSource::Plasma, StyleSource::Prop];
        let mut reached = Vec::new();
        for i in 0..40 {
            m.award(StyleAction::Kill {
                kind: EnemyKind::HellBaron,
                source: sources[i % sources.len()],
                airborne: i % 2 == 0,
                speed: 20.0,
                marked: false,
                fatality: i % 5 == 0,
            });
            m.tick(0.5);
            if let Some(r) = m.rank()
                && !reached.contains(&r)
            {
                reached.push(r);
            }
        }
        assert_eq!(m.rank(), Some(StyleRank::SSS), "{}", m.points);
        assert!(reached.contains(&StyleRank::SS) && reached.contains(&StyleRank::SSS), "{reached:?}");
        assert_eq!(m.peak, Some(StyleRank::SSS));
        assert!(m.points <= STYLE_MAX_POINTS);
    }

    #[test]
    fn freshness_penalizes_repeats_and_rewards_switching() {
        let mut m = StyleMeter::default();
        let first = m.award(kill(EnemyKind::Imp, StyleSource::Shotgun));
        m.tick(2.0);
        let second = m.award(kill(EnemyKind::Imp, StyleSource::Shotgun));
        m.tick(2.0);
        let third = m.award(kill(EnemyKind::Imp, StyleSource::Shotgun));
        assert!(second < first && third < second, "{first} {second} {third}");
        m.tick(2.0);
        let switched = m.award(kill(EnemyKind::Imp, StyleSource::Rocket));
        assert!(switched > first, "switching weapons earns a bonus: {switched} vs {first}");
        assert!(m.feed.iter().flatten().any(|(l, _)| *l == "WEAPON SWITCH"));
        // Spamming bottoms out at the freshness floor, never zero.
        for _ in 0..10 {
            m.tick(2.0);
            m.award(kill(EnemyKind::Imp, StyleSource::Rocket));
        }
        m.tick(2.0);
        let spam = m.award(kill(EnemyKind::Imp, StyleSource::Rocket));
        assert!((spam - kill_points(EnemyKind::Imp) * FRESH_MIN).abs() < 1e-3, "{spam}");
        // Mixing an app / prop is a STYLE MIX bonus too.
        m.tick(2.0);
        let mix = m.award(kill(EnemyKind::Imp, StyleSource::Overload));
        assert!((mix - kill_points(EnemyKind::Imp) * FRESH_SWITCH_BONUS).abs() < 1e-3, "{mix}");
        assert!(m.feed.iter().flatten().any(|(l, _)| *l == "STYLE MIX"));
        // A long pause forgets the history (fresh again).
        m.tick(FRESH_MEMORY + 1.0);
        let fresh = m.award(kill(EnemyKind::Imp, StyleSource::Overload));
        assert!((fresh - kill_points(EnemyKind::Imp)).abs() < 1e-3, "{fresh}");
    }

    #[test]
    fn bonuses_multi_kills_and_props_score() {
        let mut m = StyleMeter::default();
        let plain = m.clone().award(kill(EnemyKind::Imp, StyleSource::Other));
        let fancy = m.clone().award(StyleAction::Kill {
            kind: EnemyKind::Imp,
            source: StyleSource::Other,
            airborne: true,
            speed: 25.0,
            marked: true,
            fatality: true,
        });
        assert!(fancy > plain + 100.0, "{fancy} vs {plain}");
        m.award(kill(EnemyKind::Imp, StyleSource::Other));
        m.tick(0.2);
        let double = m.award(kill(EnemyKind::Imp, StyleSource::Other));
        assert!(double > plain);
        assert_eq!(m.feed[0].map(|f| f.0), Some("DOUBLE KILL"));
        let mut p = StyleMeter::default();
        let crt = p.award(StyleAction::CrtShatter);
        let ups = p.award(StyleAction::UpsDetonation { chain: false });
        let chain = p.award(StyleAction::UpsDetonation { chain: true });
        assert!(crt > 0.0 && crt < 20.0, "CRT is a small tick: {crt}");
        assert!(ups > crt && chain > ups);
        let labels: Vec<_> = p.feed.iter().flatten().map(|f| f.0).collect();
        assert_eq!(labels, vec!["CHAIN REACTION", "UPS CHAIN BLAST", "CRT SHATTERED"]);
        let boss = StyleMeter::default().award(kill(EnemyKind::CarrierDaemon, StyleSource::Rocket));
        assert!(StyleRank::from_points(boss) >= Some(StyleRank::B), "boss kill is huge: {boss}");
    }

    #[test]
    fn damage_demotes_one_rank_cleanly_with_grace() {
        let mut m = StyleMeter { points: 1000.0, ..default() };
        assert_eq!(m.rank(), Some(StyleRank::SS));
        assert!(m.take_hit());
        assert_eq!(m.rank(), Some(StyleRank::S));
        assert_eq!(m.points, StyleRank::S.threshold());
        assert!(!m.take_hit(), "hit grace stops a cascade");
        m.tick(HIT_GRACE + 0.01);
        assert!(m.take_hit());
        assert_eq!(m.rank(), Some(StyleRank::A));
        let mut d = StyleMeter { points: 40.0, ..default() };
        assert!(d.take_hit());
        assert_eq!(d.rank(), None, "D empties");
        assert!(!StyleMeter::default().take_hit(), "an empty meter has nothing to lose");
        assert_eq!(m.hits_taken, 2);
    }

    #[test]
    fn decay_waits_for_grace_ramps_smoothly_and_is_faster_at_the_top() {
        let mut m = StyleMeter { points: 500.0, ..default() };
        m.tick(DECAY_GRACE * 0.9);
        assert_eq!(m.points, 500.0, "no bleed inside the grace");
        let mut prev = m.points;
        let mut max_step = 0.0f32;
        for _ in 0..120 {
            m.tick(1.0 / 60.0);
            max_step = max_step.max(prev - m.points);
            prev = m.points;
        }
        assert!(m.points < 500.0);
        assert!(max_step <= StyleRank::A.decay_rate() / 60.0 + 1e-3, "smooth: {max_step}");
        // Idle drain time of one full tier: SSS drains far faster than C.
        let drain = |r: StyleRank| {
            let mut m = StyleMeter { points: r.ceiling() - 0.5, idle: DECAY_GRACE + DECAY_RAMP, ..default() };
            let mut t = 0.0;
            while m.rank() == Some(r) && t < 60.0 {
                m.tick(1.0 / 60.0);
                t += 1.0 / 60.0;
            }
            t / (r.ceiling() - r.threshold())
        };
        assert!(drain(StyleRank::SSS) < drain(StyleRank::C) * 0.3);
        // Everything eventually empties.
        for _ in 0..6000 {
            m.tick(1.0 / 30.0);
        }
        assert_eq!(m.rank(), None);
        assert!(m.feed.iter().all(Option::is_none), "feed expires");
    }
}
