//! Milestone 30: random over-the-top kill Fatalities and style callouts.
//!
//! Every ordinary demon kill rolls a [`FATALITY_CHANCE`] for one of four
//! telecom-flavoured finishers; point-blank shotgun overkills and direct
//! Signal-Mortar hits always get one, and the Carrier Daemon always ends in
//! `CALL DECLINED.`. A short [`FATALITY_COOLDOWN`] keeps multi-kills punchy
//! instead of stuttering. Kills without a fatality may earn a restrained
//! style callout (`AIRPLANE MODE`, `5G OVERKILL`, `FULL BARS`).
//!
//! Everything here is purely cosmetic: particles, decals, a sound, a HUD toast
//! and a camera roll / FOV punch that never touches the aim direction or the
//! fixed-step simulation (no hit-stop, no time scaling).

use bevy::prelude::*;

use crate::decals::{DecalKind, DecalQueue, DecalRequest};
use crate::fx::{spawn_death_gibs, spawn_flash_light, spawn_particles, spawn_shockwave, BurstColor, FxAssets, GibMat};

/// Chance of a fatality on an ordinary kill.
pub const FATALITY_CHANCE: f32 = 0.25;
/// Minimum seconds between two (non-boss) fatalities.
pub const FATALITY_COOLDOWN: f32 = 1.8;
/// Shotgun kills at or inside this distance (m) with overkill damage are guaranteed fatalities.
pub const POINT_BLANK_DIST: f32 = 5.5;
/// A blast counts as overkill when its raw damage beats the remaining HP by this factor.
pub const OVERKILL_FACTOR: f32 = 1.25;
/// Minimum seconds between two style callouts.
pub const STYLE_COOLDOWN: f32 = 4.0;
/// Horizontal speed (m/s) for a `5G OVERKILL` callout.
pub const STYLE_SPEED: f32 = 18.0;

/// Fatality shockwave glows (prewarmed: no material is ever allocated mid-fight).
pub const FATAL_CYAN_GLOW: (Color, f32) = (Color::srgb(0.3, 0.95, 1.0), 0.8);
pub const FATAL_CRIMSON_GLOW: (Color, f32) = (Color::srgb(1.0, 0.14, 0.08), 0.75);
pub const FATAL_ACID_GLOW: (Color, f32) = (Color::srgb(0.55, 1.0, 0.25), 0.8);

/// How the killing blow landed (recorded on the demon by the weapon).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KillCause {
    #[default]
    Generic,
    /// Shotgun blast within [`POINT_BLANK_DIST`] that overkilled the demon.
    PointBlankOverkill,
    /// Direct Signal-Mortar hit.
    MortarDirect,
}

impl KillCause {
    /// Guaranteed fatality causes.
    pub fn guaranteed(self) -> bool {
        self != KillCause::Generic
    }
}

/// Whether a shotgun blast of `raw` damage at `dist` m that killed a demon
/// with `hp_before` HP left is a point-blank overkill.
pub fn is_point_blank_overkill(raw: f32, hp_before: f32, dist: f32) -> bool {
    dist <= POINT_BLANK_DIST && raw >= hp_before.max(0.0) * OVERKILL_FACTOR + 1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FatalityKind {
    /// Gib fountain / cranial pop.
    Disconnected,
    /// 5G arc vaporize.
    RoamingCharges,
    /// Ragdoll launch / wall splat along the shot.
    NoService,
    /// Implosion slam.
    SignalDropped,
    /// Carrier Daemon kill.
    CallDeclined,
}

impl FatalityKind {
    /// The four random finishers (the boss always gets [`FatalityKind::CallDeclined`]).
    pub const RANDOM: [FatalityKind; 4] = [FatalityKind::Disconnected, FatalityKind::RoamingCharges, FatalityKind::NoService, FatalityKind::SignalDropped];

    /// Big HUD word.
    pub fn title(self) -> &'static str {
        match self {
            FatalityKind::Disconnected => "DISCONNECTED",
            FatalityKind::RoamingCharges => "ROAMING CHARGES",
            FatalityKind::NoService => "NO SERVICE",
            FatalityKind::SignalDropped => "SIGNAL DROPPED",
            FatalityKind::CallDeclined => "CALL DECLINED.",
        }
    }

    /// Cheeky telecom callout line for the HUD feed.
    pub fn callout(self) -> &'static str {
        match self {
            FatalityKind::Disconnected => "FATALITY // DISCONNECTED - CRANIAL PACKET LOSS",
            FatalityKind::RoamingCharges => "FATALITY // ROAMING CHARGES - 19.99 EUR/MB, PAID IN FULL",
            FatalityKind::NoService => "FATALITY // NO SERVICE - SENT TO VOICEMAIL",
            FatalityKind::SignalDropped => "FATALITY // SIGNAL DROPPED - CONNECTION IMPLODED",
            FatalityKind::CallDeclined => "FATALITY // CALL DECLINED. - CARRIER DAEMON BLOCKED",
        }
    }

    pub fn color(self) -> Color {
        match self {
            FatalityKind::Disconnected => Color::srgb(1.0, 0.3, 0.22),
            FatalityKind::RoamingCharges => Color::srgb(0.35, 0.95, 1.0),
            FatalityKind::NoService => Color::srgb(1.0, 0.55, 0.15),
            FatalityKind::SignalDropped => Color::srgb(0.85, 0.4, 1.0),
            FatalityKind::CallDeclined => Color::srgb(0.55, 1.0, 0.25),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleCallout {
    /// Kill while airborne or dashing.
    AirplaneMode,
    /// Kill while moving at >= [`STYLE_SPEED`].
    FiveGOverkill,
    /// Kill at 5 bars.
    FullBars,
}

impl StyleCallout {
    pub fn text(self) -> &'static str {
        match self {
            StyleCallout::AirplaneMode => "AIRPLANE MODE",
            StyleCallout::FiveGOverkill => "5G OVERKILL",
            StyleCallout::FullBars => "FULL BARS",
        }
    }
}

/// Fatality / style throttles (reset with every run).
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct FatalityState {
    pub cooldown: f32,
    pub style_cooldown: f32,
    pub last: Option<FatalityKind>,
    /// Fatalities this run (telemetry / tests).
    pub count: u32,
    /// Style callouts this run.
    pub styles: u32,
}

impl FatalityState {
    pub fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.cooldown = (self.cooldown - dt).max(0.0);
        self.style_cooldown = (self.style_cooldown - dt).max(0.0);
    }

    /// Decide a kill's fatality. `roll` in `[0, 1)` is the chance roll, `pick`
    /// any integer choosing the finisher (the previous one is never repeated
    /// back to back). Boss kills always fire and ignore the cooldown.
    pub fn roll(&mut self, cause: KillCause, boss: bool, roll: f32, pick: u32) -> Option<FatalityKind> {
        if boss {
            self.cooldown = FATALITY_COOLDOWN;
            self.last = Some(FatalityKind::CallDeclined);
            self.count += 1;
            return Some(FatalityKind::CallDeclined);
        }
        if self.cooldown > 0.0 || !(cause.guaranteed() || roll < FATALITY_CHANCE) {
            return None;
        }
        let mut kind = FatalityKind::RANDOM[pick as usize % 4];
        if Some(kind) == self.last {
            kind = FatalityKind::RANDOM[(pick as usize + 1) % 4];
        }
        self.cooldown = FATALITY_COOLDOWN;
        self.last = Some(kind);
        self.count += 1;
        Some(kind)
    }

    /// Style callout for a kill without a fatality (rate limited).
    pub fn style(&mut self, airborne_or_dashing: bool, speed: f32, bars: u8) -> Option<StyleCallout> {
        if self.style_cooldown > 0.0 {
            return None;
        }
        let s = if airborne_or_dashing {
            StyleCallout::AirplaneMode
        } else if speed >= STYLE_SPEED {
            StyleCallout::FiveGOverkill
        } else if bars >= 5 {
            StyleCallout::FullBars
        } else {
            return None;
        };
        self.style_cooldown = STYLE_COOLDOWN;
        self.styles += 1;
        Some(s)
    }
}

/// Spawn a fatality's bespoke VFX + decals (`center` = body center, `feet`
/// = floor point under it, `dir` = horizontal shot direction).
#[allow(clippy::too_many_arguments)]
pub fn spawn_fatality_fx(
    commands: &mut Commands,
    fx: &FxAssets,
    materials: &mut Assets<StandardMaterial>,
    decals: &mut DecalQueue,
    kind: FatalityKind,
    center: Vec3,
    feet: Vec3,
    dir: Vec3,
    wall: Option<(Vec3, Vec3)>,
) {
    let dir = Vec3::new(dir.x, 0.0, dir.z).normalize_or(Vec3::X);
    match kind {
        FatalityKind::Disconnected => {
            // Gib fountain: bone / core chunks + crimson spray shot upward, spark shower.
            let top = center + Vec3::Y * 0.5;
            spawn_death_gibs(commands, fx, top, &[GibMat::Flesh, GibMat::Bone, GibMat::Flesh], 7, 0.14, 11.0);
            spawn_particles(commands, fx, top, BurstColor::Gore, 16, 11.0, 0.09);
            spawn_particles(commands, fx, top + Vec3::Y * 0.3, BurstColor::Spark, 12, 9.0, 0.05);
            spawn_flash_light(commands, top, Color::srgb(1.0, 0.25, 0.15), 900_000.0, 10.0, 0.18);
            decals.push(DecalRequest::floor(DecalKind::BloodCrimson, feet, 2.2));
        }
        FatalityKind::RoamingCharges => {
            // 5G arc vaporize: expanding electric ring, blue plasma sparks rising.
            spawn_shockwave(commands, fx, materials, feet + Vec3::Y * 0.2, 4.5, 0.45, FATAL_CYAN_GLOW.0, FATAL_CYAN_GLOW.1, true);
            spawn_particles(commands, fx, center, BurstColor::Cyan, 18, 10.0, 0.07);
            spawn_particles(commands, fx, center + Vec3::Y * 0.6, BurstColor::Spark, 8, 7.0, 0.05);
            spawn_flash_light(commands, center, Color::srgb(0.3, 0.9, 1.0), 1_100_000.0, 12.0, 0.22);
            decals.push(DecalRequest::floor(DecalKind::Scorch, feet, 1.8));
            decals.push(DecalRequest::floor(DecalKind::PlasmaBurn, feet, 1.1));
        }
        FatalityKind::NoService => {
            // Ragdoll launch: a dense spinning volley thrown along the shot,
            // a crimson smoke burst and a splat on the wall it hits.
            spawn_directional_gibs(commands, fx, center, dir, 9, 13.0, 0.13);
            spawn_particles(commands, fx, center + dir * 0.6, BurstColor::Fire, 10, 8.0, 0.07);
            spawn_particles(commands, fx, center, BurstColor::Smoke, 6, 2.5, 0.22);
            spawn_particles(commands, fx, center + dir * 0.4, BurstColor::Gore, 10, 7.0, 0.08);
            spawn_flash_light(commands, center, Color::srgb(1.0, 0.45, 0.12), 800_000.0, 10.0, 0.18);
            if let Some((point, normal)) = wall {
                decals.push(DecalRequest::surface(DecalKind::BloodCrimson, point, normal, 1.4));
            }
            decals.push(DecalRequest::floor(DecalKind::BloodCrimson, feet + dir * 1.2, 1.2));
        }
        FatalityKind::SignalDropped => {
            // Implosion slam: hot cyan-crimson flash, shockwave, ring of splatter.
            spawn_shockwave(commands, fx, materials, center, 2.4, 0.22, FATAL_CRIMSON_GLOW.0, FATAL_CRIMSON_GLOW.1, false);
            spawn_shockwave(commands, fx, materials, feet + Vec3::Y * 0.15, 3.6, 0.35, FATAL_CYAN_GLOW.0, FATAL_CYAN_GLOW.1, true);
            spawn_particles(commands, fx, center, BurstColor::Gore, 12, 6.0, 0.08);
            spawn_particles(commands, fx, center, BurstColor::Cyan, 8, 6.0, 0.05);
            spawn_flash_light(commands, center, Color::srgb(0.9, 0.4, 1.0), 1_000_000.0, 11.0, 0.16);
            for k in 0..6 {
                let a = k as f32 * std::f32::consts::TAU / 6.0;
                decals.push(DecalRequest::floor(DecalKind::BloodCrimson, feet + Vec3::new(a.cos(), 0.0, a.sin()) * 1.4, 0.55));
            }
        }
        FatalityKind::CallDeclined => {
            // Dramatic multi-ring burst on top of the boss's own death blast.
            for (r, life, glow) in [(6.0, 0.5, FATAL_ACID_GLOW), (10.0, 0.7, FATAL_CYAN_GLOW), (14.0, 0.9, FATAL_CRIMSON_GLOW)] {
                spawn_shockwave(commands, fx, materials, feet + Vec3::Y * 0.3, r, life, glow.0, glow.1, true);
            }
            spawn_particles(commands, fx, center, BurstColor::Green, 20, 12.0, 0.08);
            spawn_flash_light(commands, center, Color::srgb(0.55, 1.0, 0.3), 3_000_000.0, 24.0, 0.5);
        }
    }
}

/// Spinning gib & debris volley thrown along `dir` (a "ragdoll launch").
pub fn spawn_directional_gibs(commands: &mut Commands, fx: &FxAssets, center: Vec3, dir: Vec3, count: u32, speed: f32, size: f32) {
    // Deterministic fan around `dir` (no RNG needed for a convincing volley).
    let side = Vec3::Y.cross(dir).normalize_or(Vec3::Z);
    let mats = [GibMat::Flesh, GibMat::Bone, GibMat::DarkMetal];
    let meshes = [&fx.gib_chunk, &fx.gib_horn, &fx.gib_plate, &fx.gib_shard];
    for i in 0..count {
        let t = i as f32 / count.max(1) as f32 - 0.5;
        let v = (dir + side * t * 0.9 + Vec3::Y * (0.35 + 0.25 * ((i * 7) % 5) as f32 / 5.0)).normalize_or(dir) * speed * (0.7 + 0.3 * ((i * 3) % 4) as f32 / 4.0);
        let spin = Vec3::new(9.0 + t * 6.0, -7.0 + i as f32, 11.0 - t * 8.0);
        let s = size * (0.8 + 0.1 * (i % 4) as f32);
        commands.spawn((
            crate::level::LevelEntity,
            crate::fx::Fx::new(1.6, crate::fx::FxMotion::Debris { vel: v, spin, gravity: 19.0, bounce: 0.3, clinks: 0 }, Vec3::splat(s)),
            Mesh3d(meshes[i as usize % meshes.len()].clone()),
            MeshMaterial3d(fx.gib_mats[mats[i as usize % mats.len()].index()].clone()),
            Transform::from_translation(center + side * t * 0.4).with_scale(Vec3::splat(s)),
        ));
    }
}

/// Prewarm the fatality shockwave palettes so no material is allocated mid-fight.
pub fn prewarm_fatality_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[FATAL_CYAN_GLOW, FATAL_CRIMSON_GLOW, FATAL_ACID_GLOW]);
}

pub fn reset_fatality_state(mut state: ResMut<FatalityState>) {
    *state = FatalityState::default();
}

/// Reset per-run fatality state whenever a fresh run starts on Floor 1
/// (e.g. `Paused -> RestartRun` or `GameOver -> Loadout -> Playing`), while
/// preserving cooldowns and counters across `Paused -> Continue` and floor
/// transitions (`UpgradeSelect -> Playing`).
pub fn reset_fatality_on_fresh_run(run: Res<crate::state::RunStats>, mut state: ResMut<FatalityState>) {
    if run.floor == 1 && run.time_elapsed == 0.0 {
        *state = FatalityState::default();
    }
}

pub fn tick_fatality_state(time: Res<Time>, mut state: ResMut<FatalityState>) {
    state.tick(time.delta_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_kills_roll_a_quarter_chance_and_respect_the_cooldown() {
        let mut s = FatalityState::default();
        assert_eq!(s.roll(KillCause::Generic, false, 0.5, 0), None, "75% of rolls miss");
        let k = s.roll(KillCause::Generic, false, 0.1, 0);
        assert_eq!(k, Some(FatalityKind::Disconnected));
        // Cooldown blocks even a guaranteed cause.
        assert_eq!(s.roll(KillCause::PointBlankOverkill, false, 0.0, 1), None);
        s.tick(FATALITY_COOLDOWN - 0.01);
        assert_eq!(s.roll(KillCause::Generic, false, 0.0, 1), None);
        s.tick(0.02);
        assert!(s.roll(KillCause::Generic, false, 0.0, 1).is_some());
        assert_eq!(s.count, 2);
    }

    #[test]
    fn overkill_and_mortar_kills_are_guaranteed_and_boss_always_declines() {
        let mut s = FatalityState::default();
        assert!(s.roll(KillCause::PointBlankOverkill, false, 0.99, 2).is_some());
        s.tick(FATALITY_COOLDOWN);
        assert!(s.roll(KillCause::MortarDirect, false, 0.99, 3).is_some());
        // Boss ignores the cooldown.
        assert_eq!(s.roll(KillCause::Generic, true, 0.99, 0), Some(FatalityKind::CallDeclined));
        assert!(is_point_blank_overkill(120.0, 40.0, 3.0));
        assert!(!is_point_blank_overkill(120.0, 40.0, 6.0), "too far");
        assert!(!is_point_blank_overkill(45.0, 40.0, 2.0), "barely lethal is not overkill");
    }

    #[test]
    fn all_four_finishers_are_reachable_and_never_repeat_back_to_back() {
        let mut s = FatalityState::default();
        let mut seen = std::collections::HashSet::new();
        let mut prev = None;
        for pick in 0..40u32 {
            s.tick(FATALITY_COOLDOWN);
            let k = s.roll(KillCause::Generic, false, 0.0, pick * 7 % 5).unwrap();
            assert_ne!(Some(k), prev, "no back-to-back repeat");
            assert_ne!(k, FatalityKind::CallDeclined);
            seen.insert(k);
            prev = Some(k);
        }
        assert_eq!(seen.len(), 4);
        for k in FatalityKind::RANDOM.into_iter().chain([FatalityKind::CallDeclined]) {
            assert!(k.callout().contains(k.title()), "{k:?}");
        }
    }

    #[test]
    fn style_callouts_are_rate_limited_and_prioritized() {
        let mut s = FatalityState::default();
        assert_eq!(s.style(false, 5.0, 3), None, "ordinary kill: no callout");
        assert_eq!(s.style(true, 25.0, 5), Some(StyleCallout::AirplaneMode));
        assert_eq!(s.style(false, 25.0, 5), None, "rate limited");
        s.tick(STYLE_COOLDOWN);
        assert_eq!(s.style(false, 19.0, 5), Some(StyleCallout::FiveGOverkill));
        s.tick(STYLE_COOLDOWN);
        assert_eq!(s.style(false, 3.0, 5), Some(StyleCallout::FullBars));
        assert_eq!(s.styles, 3);
    }
}
