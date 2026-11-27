//! Procedural retro synth SFX.
//!
//! [`synthesize_wav`] is pure Rust and always compiled (and unit-tested): it
//! renders every [`SfxEvent`] into a 16-bit mono PCM RIFF/WAVE buffer from
//! oscillators, noise and envelopes. Actual playback is compiled only with
//! `--features audio` (needs ALSA on Linux) and always on Android (AAudio via
//! cpal, see `Cargo.toml`): the WAV buffers become cached
//! `AudioSource` assets and draining [`SfxQueue`] spawns `AudioPlayer`s.
//! Gameplay code only ever pushes into [`SfxQueue`], so it compiles identically
//! with or without the feature.

use std::f32::consts::TAU;

use bevy::prelude::*;

use crate::boss::{BossHud, BossPhase};
use crate::extraction::ExtractionState;
use crate::meta::UserSettings;
use crate::signal::SignalState;
use crate::state::{AppState, RunStats};

pub const SAMPLE_RATE: u32 = 22_050;

/// Sound effects the game can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SfxEvent {
    Shotgun,
    Plasma,
    Dash,
    Jump,
    /// Chained bunny hop above base speed (kinetic chirp, brighter than `Jump`).
    Bhop,
    Hurt,
    Ping,
    Overload,
    JammerDestroyed,
    UplinkStart,
    Extract,
    Hit,
    EnemyDeath,
    EnemyFire,
    Pickup,
    WeaponSwap,
    BaronAlert,
    Denied,
    UpgradeSelect,
    /// Signal-Mortar launch: pneumatic ignition thump + propellant whoosh.
    RocketFire,
    /// Rocket detonation: deep sub-bass boom + low-passed debris crackle.
    RocketExplode,
    /// Signal Jump Pad launch: pneumatic burst + rising synth sweep.
    JumpPad,
    /// Phase Stalker blink shimmer: glitchy static with a warped pitch.
    PhaseBlink,
    /// Relay Sniper lock-on: sharp rising laser whine.
    SniperLock,
    /// Carrier Daemon Signal Shield surge / shatter: resonant crystalline chord.
    BossShield,
    /// Gritty Mars-regolith boot step (ground floor).
    StepRegolith,
    /// Hollow metallic clang of a boot on a catwalk / ramp.
    StepMetal,
    /// Punchy low-end touchdown thud after a fall or jump.
    LandThud,
    /// Crisp high-pitched brass casing bounce clink.
    ShellClink,
    /// Propellant rush / whoosh layered over a Signal-Mortar launch.
    RocketWhoosh,
    /// Eerie RF static hum + harmonic pulse emitted by nearby jammers.
    JammerHum,
    /// Time Trial pit rescue: crisp upward synth recovery chime (never the
    /// negative `Denied` buzz).
    PitRescue,
    /// Quiet menu focus tick (controller focus moved). Throttled by [`UiSfxGate`].
    UiTick,
    /// Short menu confirm blip. Throttled by [`UiSfxGate`].
    UiConfirm,
    /// Diegetic phone notification: crisp, restrained double-chirp (ambient
    /// carrier texts, deploy "SEARCHING...", incoming boss call).
    PhonePing,
    /// Random kill Fatality: chunky sub thump + bone crunch + bright snap.
    Fatality,
    /// Milestone 31: Style Rank climbed a tier (D..A): bright two-note stab.
    StyleRankUp,
    /// Style Rank reached S / SS / SSS: extra-crisp rising arpeggio + crack.
    StylePeak,
    /// Shootable CRT monitor shattered: glass burst + phosphor whine.
    CrtShatter,
    /// Lithium UPS rack detonation: deep boom + EMP zap + crackle.
    UpsDetonate,
    /// Damaged UPS rack warning hiss + spark crackle.
    UpsHiss,
    /// Milestone 33: Phone Punch swing / impact: air whip + knuckle thud.
    PhonePunch,
    /// Milestone 33: projectile parry: metallic clang + rising golden ring.
    PhoneParry,
    /// Milestone 33: Camera Flash Strobe: shutter snap + high-voltage strobe pop.
    CameraFlash,
}

impl SfxEvent {
    pub const ALL: [SfxEvent; 44] = [
        SfxEvent::Shotgun,
        SfxEvent::Plasma,
        SfxEvent::Dash,
        SfxEvent::Jump,
        SfxEvent::Bhop,
        SfxEvent::Hurt,
        SfxEvent::Ping,
        SfxEvent::Overload,
        SfxEvent::JammerDestroyed,
        SfxEvent::UplinkStart,
        SfxEvent::Extract,
        SfxEvent::Hit,
        SfxEvent::EnemyDeath,
        SfxEvent::EnemyFire,
        SfxEvent::Pickup,
        SfxEvent::WeaponSwap,
        SfxEvent::BaronAlert,
        SfxEvent::Denied,
        SfxEvent::UpgradeSelect,
        SfxEvent::RocketFire,
        SfxEvent::RocketExplode,
        SfxEvent::JumpPad,
        SfxEvent::PhaseBlink,
        SfxEvent::SniperLock,
        SfxEvent::BossShield,
        SfxEvent::StepRegolith,
        SfxEvent::StepMetal,
        SfxEvent::LandThud,
        SfxEvent::ShellClink,
        SfxEvent::RocketWhoosh,
        SfxEvent::JammerHum,
        SfxEvent::PitRescue,
        SfxEvent::UiTick,
        SfxEvent::UiConfirm,
        SfxEvent::PhonePing,
        SfxEvent::Fatality,
        SfxEvent::StyleRankUp,
        SfxEvent::StylePeak,
        SfxEvent::CrtShatter,
        SfxEvent::UpsDetonate,
        SfxEvent::UpsHiss,
        SfxEvent::PhonePunch,
        SfxEvent::PhoneParry,
        SfxEvent::CameraFlash,
    ];

    /// Warnings that must always be heard keep their direction but are pulled
    /// to at most this distance (m) from the listener: never culled or faint.
    pub fn spatial_clamp_dist(self) -> Option<f32> {
        match self {
            SfxEvent::BaronAlert => Some(9.0),
            SfxEvent::BossShield => Some(12.0),
            _ => None,
        }
    }

    /// Playback volume (0..1).
    pub fn volume(self) -> f32 {
        match self {
            SfxEvent::Hit | SfxEvent::Pickup | SfxEvent::WeaponSwap => 0.45,
            SfxEvent::Plasma | SfxEvent::Jump | SfxEvent::Bhop => 0.5,
            SfxEvent::StepRegolith | SfxEvent::StepMetal => 0.28,
            SfxEvent::ShellClink => 0.32,
            SfxEvent::LandThud | SfxEvent::RocketWhoosh => 0.55,
            // Milestone 28 headroom: keep RocketExplode punchier than RocketFire
            // while reserving headroom so Overdrive stems + close hum + blast +
            // enemy fire stay cleanly <= -1 dBFS with zero clipping.
            SfxEvent::JammerHum => 0.24,
            SfxEvent::RocketFire => 0.40,
            SfxEvent::RocketExplode => 0.44,
            SfxEvent::EnemyFire => 0.36,
            SfxEvent::PitRescue => 0.6,
            SfxEvent::UiTick => 0.18,
            SfxEvent::UiConfirm => 0.3,
            // Milestone 30: notification chirp sits politely under combat;
            // the Fatality crunch lands with kills (often beside a mortar
            // blast), so it stays below RocketExplode to keep -1 dBFS.
            SfxEvent::PhonePing => 0.22,
            SfxEvent::Fatality => 0.3,
            // Milestone 31: stingers sit just above the music bed; the UPS
            // blast stays at the mortar's level so a chain + mortar + peak
            // stinger over the 6G layer keeps -1 dBFS.
            SfxEvent::StyleRankUp => 0.16,
            SfxEvent::StylePeak => 0.18,
            SfxEvent::CrtShatter => 0.17,
            SfxEvent::UpsDetonate => 0.2,
            SfxEvent::UpsHiss => 0.14,
            // Milestone 33: phone abilities sit with the guns, under the mortar.
            SfxEvent::PhonePunch => 0.34,
            SfxEvent::PhoneParry => 0.3,
            SfxEvent::CameraFlash => 0.26,
            _ => 0.75,
        }
    }
}

/// One requested sound: 2D (player-origin, `pos == None`) or world-positioned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SfxRequest {
    pub event: SfxEvent,
    pub pos: Option<Vec3>,
}

/// Frame queue of requested sounds (drained by the audio backend each frame).
#[derive(Resource, Default, Debug)]
pub struct SfxQueue(pub Vec<SfxRequest>);

impl SfxQueue {
    /// 2D / player-origin sound (weapons, UI, movement, hit markers...).
    pub fn play(&mut self, sfx: SfxEvent) {
        self.0.push(SfxRequest { event: sfx, pos: None });
    }

    /// World-positioned 3D sound: panned and attenuated relative to the
    /// listener on the player's head camera.
    pub fn play_at(&mut self, sfx: SfxEvent, pos: Vec3) {
        self.0.push(SfxRequest { event: sfx, pos: Some(pos) });
    }

    /// Queued events in request order (positions dropped).
    pub fn events(&self) -> impl Iterator<Item = SfxEvent> + '_ {
        self.0.iter().map(|r| r.event)
    }
}

// ---------------------------------------------------------------------------
// Spatial audio model
// ---------------------------------------------------------------------------

/// Distance between the listener's ears (metres, on the camera's local X).
pub const SPATIAL_LISTENER_GAP: f32 = 0.4;
/// Distance (m) up to which a positional sound plays at full volume. Bevy /
/// rodio attenuate with `min(1, 1 / (d * scale)^2)`, so the spatial scale is
/// `1 / SPATIAL_REF_DIST`: 10 m = 1.0, 16 m = 0.39, 25 m = 0.16, 35 m = 0.08.
pub const SPATIAL_REF_DIST: f32 = 10.0;
/// Beyond this distance (m) positional sounds are culled; they fade out
/// smoothly over the last 40% before it so nothing pops at the boundary.
pub const SPATIAL_MAX_DIST: f32 = 55.0;

/// World-to-audio position scale handed to Bevy's `SpatialScale`.
pub const fn spatial_scale_factor() -> f32 {
    1.0 / SPATIAL_REF_DIST
}

/// Inverse-square rolloff (same law as the rodio spatial sink), 1 inside [`SPATIAL_REF_DIST`].
pub fn spatial_rolloff(dist: f32) -> f32 {
    if !dist.is_finite() {
        return 0.0;
    }
    (SPATIAL_REF_DIST / dist.max(1e-3)).powi(2).min(1.0)
}

/// Extra smooth fade-to-zero over `0.6 * max_dist ..= max_dist` (0 beyond).
pub fn spatial_distance_fade(dist: f32, max_dist: f32) -> f32 {
    if !dist.is_finite() || !max_dist.is_finite() || max_dist <= 0.0 || dist >= max_dist {
        return 0.0;
    }
    let start = max_dist * 0.6;
    if dist <= start {
        return 1.0;
    }
    let x = (dist - start) / (max_dist - start);
    1.0 - x * x * (3.0 - 2.0 * x)
}

/// Overall gain (0..1, rolloff times edge fade) and stereo pan (-1 = hard
/// left, +1 = hard right) of a sound at `src_pos` heard from `listener_pos`
/// whose right ear points along `listener_right`.
pub fn spatial_gain_and_pan(listener_pos: Vec3, listener_right: Vec3, src_pos: Vec3, max_dist: f32) -> (f32, f32) {
    let d = src_pos - listener_pos;
    let dist = d.length();
    if !dist.is_finite() {
        return (0.0, 0.0);
    }
    let gain = spatial_rolloff(dist) * spatial_distance_fade(dist, max_dist);
    let pan = if dist < 1e-3 { 0.0 } else { (d / dist).dot(listener_right.normalize_or_zero()).clamp(-1.0, 1.0) };
    (gain, pan)
}

/// Where a positional `event` at `src` is actually emitted: unchanged, or
/// for [`SfxEvent::spatial_clamp_dist`] warnings pulled toward the listener.
pub fn emitter_position(event: SfxEvent, listener: Vec3, src: Vec3) -> Vec3 {
    match event.spatial_clamp_dist() {
        Some(max) if src.distance(listener) > max => listener + (src - listener).normalize_or_zero() * max,
        _ => src,
    }
}

/// One instance per [`SfxEvent`] per frame (8 pellet hits = 1 tick, never
/// clipping): keeps first-request order, and for each event prefers a 2D
/// instance, else the spatial instance nearest the listener. `out` is
/// cleared and reused (no per-frame allocation once warm).
pub fn dedup_sfx_requests(requests: &[SfxRequest], listener: Option<Vec3>, out: &mut Vec<SfxRequest>) {
    out.clear();
    let dist = |r: &SfxRequest| match (r.pos, listener) {
        (Some(p), Some(l)) => p.distance_squared(l),
        _ => f32::INFINITY,
    };
    for r in requests {
        // Ignore requests with non-finite coordinates so they never shadow valid emitters.
        if r.pos.is_some_and(|p| !p.is_finite()) {
            continue;
        }
        match out.iter_mut().find(|o| o.event == r.event) {
            None => out.push(*r),
            Some(o) => {
                let better = match (o.pos, r.pos) {
                    (None, _) => false,
                    (Some(_), None) => true,
                    (Some(_), Some(_)) => dist(r) < dist(o),
                };
                if better {
                    *o = *r;
                }
            }
        }
    }
}

/// Nearest of `sources` within `radius` of `from` (Jammer hum emitter).
pub fn nearest_within(from: Vec3, sources: impl IntoIterator<Item = Vec3>, radius: f32) -> Option<Vec3> {
    sources
        .into_iter()
        .map(|p| (p.distance_squared(from), p))
        .filter(|(d2, _)| d2.is_finite() && *d2 <= radius * radius)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
}

/// Horizontal metres of grounded running between footsteps.
pub const STEP_STRIDE: f32 = 2.4;
/// Feet above this height (m) are on a catwalk / tower deck (metal steps).
pub const STEP_METAL_HEIGHT: f32 = 0.35;
/// Downward touchdown speed (m/s) above which a landing thuds.
pub const LAND_THUD_SPEED: f32 = 3.5;
/// Downward speed (m/s) a casing must hit the floor with to clink.
pub const CLINK_MIN_SPEED: f32 = 1.2;

/// Footstep sound for the surface under the feet: metal on ramps and raised
/// catwalks / decks, regolith on the bunker floor.
pub fn footstep_sfx(cell: crate::level::Cell, feet_y: f32) -> SfxEvent {
    if matches!(cell, crate::level::Cell::Ramp { .. }) || feet_y > STEP_METAL_HEIGHT { SfxEvent::StepMetal } else { SfxEvent::StepRegolith }
}

/// Advance the stride accumulator by `moved` metres; true when a step lands.
pub fn advance_stride(stride: &mut f32, moved: f32) -> bool {
    if !moved.is_finite() || moved <= 0.0 {
        return false;
    }
    *stride += moved;
    if *stride >= STEP_STRIDE {
        *stride = (*stride - STEP_STRIDE).min(STEP_STRIDE * 0.5);
        true
    } else {
        false
    }
}

/// A floor bounce that should clink: falling fast enough, now rebounding.
pub fn is_clink_bounce(prev_vy: f32, new_vy: f32) -> bool {
    prev_vy < -CLINK_MIN_SPEED && new_vy > 0.0
}

/// Seconds between Jammer hum pulses.
pub const JAMMER_HUM_INTERVAL: f32 = 1.8;
/// Hearing radius (m) of the Jammer hum.
pub const JAMMER_HUM_RADIUS: f32 = 18.0;

/// Every [`JAMMER_HUM_INTERVAL`] while playing, hum from the nearest living
/// Demon Jammer / Jammer Fiend within [`JAMMER_HUM_RADIUS`] so jammers can be
/// hunted by ear.
pub fn jammer_hum(
    time: Res<Time>,
    mut timer: Local<f32>,
    grid: Option<Res<crate::level::LevelGrid>>,
    mut sfx: ResMut<SfxQueue>,
    players: Query<&GlobalTransform, With<crate::player::Player>>,
    jammers: Query<(&crate::level::DemonJammer, &GlobalTransform)>,
    fiends: Query<(&crate::enemies::Enemy, &GlobalTransform)>,
) {
    if grid.is_some_and(|g| g.is_changed()) {
        *timer = 0.0;
    }
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = JAMMER_HUM_INTERVAL;
    let Ok(player) = players.single() else { return };
    let from = player.translation();
    let towers = jammers.iter().filter(|(j, _)| j.hp > 0.0).map(|(_, g)| g.translation() + Vec3::Y * 2.0);
    let demons = fiends
        .iter()
        .filter(|(e, _)| e.is_alive() && e.kind == crate::enemies::EnemyKind::JammerFiend)
        .map(|(_, g)| g.translation());
    if let Some(pos) = nearest_within(from, towers.chain(demons), JAMMER_HUM_RADIUS) {
        sfx.play_at(SfxEvent::JammerHum, pos);
    }
}

// ---------------------------------------------------------------------------
// Synthesis
// ---------------------------------------------------------------------------

/// Tiny deterministic xorshift noise source.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn square(phase: f32) -> f32 {
    if phase.fract() < 0.5 { 1.0 } else { -1.0 }
}

fn saw(phase: f32) -> f32 {
    phase.fract() * 2.0 - 1.0
}

fn tri(phase: f32) -> f32 {
    1.0 - 4.0 * (phase.fract() - 0.5).abs()
}

/// Attack/decay envelope over normalized time `t` in 0..1.
fn env(t: f32, attack: f32, curve: f32) -> f32 {
    if t < attack { t / attack.max(1e-4) } else { (1.0 - (t - attack) / (1.0 - attack).max(1e-4)).max(0.0).powf(curve) }
}

/// Render `secs` of audio with `f(t_seconds, t_normalized, noise) -> sample`.
fn render(secs: f32, mut f: impl FnMut(f32, f32, &mut Noise) -> f32) -> Vec<f32> {
    let n = (secs * SAMPLE_RATE as f32) as usize;
    let mut noise = Noise(0x9E37_79B9);
    (0..n)
        .map(|i| {
            let ts = i as f32 / SAMPLE_RATE as f32;
            f(ts, i as f32 / n as f32, &mut noise)
        })
        .collect()
}

/// Oscillator with a time-varying frequency (integrates phase).
struct Osc {
    phase: f32,
}

impl Osc {
    fn step(&mut self, freq: f32) -> f32 {
        self.phase += freq / SAMPLE_RATE as f32;
        self.phase
    }
}

/// One-pole low-pass for noise shaping.
struct Lp {
    y: f32,
}

impl Lp {
    fn run(&mut self, x: f32, k: f32) -> f32 {
        self.y += (x - self.y) * k;
        self.y
    }
}

fn synth_samples(sfx: SfxEvent) -> Vec<f32> {
    let mut o = Osc { phase: 0.0 };
    let mut o2 = Osc { phase: 0.0 };
    let mut lp = Lp { y: 0.0 };
    match sfx {
        SfxEvent::Shotgun => render(0.42, |ts, t, n| {
            let boom = o.step(110.0 * (1.0 - t * 0.7));
            let body = lp.run(n.next(), 0.35 - t * 0.3) * 1.6 + (boom * TAU).sin() * 0.6;
            let click = if ts > 0.26 && ts < 0.29 { square(ts * 900.0) * 0.25 } else { 0.0 };
            body * env(t, 0.01, 2.5) + click
        }),
        SfxEvent::Plasma => render(0.16, |_, t, n| {
            let p = o.step(1400.0 - 900.0 * t);
            (square(p) * 0.5 + n.next() * 0.1) * env(t, 0.02, 1.5)
        }),
        SfxEvent::Dash => render(0.28, |_, t, n| {
            let p = o.step(300.0 + 900.0 * t);
            (lp.run(n.next(), 0.25) * 1.2 + saw(p) * 0.2) * env(t, 0.15, 1.2)
        }),
        SfxEvent::Jump => render(0.14, |_, t, _| {
            let p = o.step(220.0 + 400.0 * t);
            square(p) * 0.35 * env(t, 0.05, 1.0)
        }),
        SfxEvent::Bhop => render(0.13, |ts, t, n| {
            // Noise tick + fast upward square/triangle chirp, an octave above `Jump`.
            let p = o.step(520.0 + 1500.0 * t.sqrt());
            let shimmer = tri(o2.step(1040.0 + 2200.0 * t)) * 0.18;
            let tick = if ts < 0.005 { n.next() * 0.45 } else { 0.0 };
            (square(p) * 0.3 + shimmer + tick) * env(t, 0.03, 1.8)
        }),
        SfxEvent::Hurt => render(0.3, |_, t, n| {
            let p = o.step(240.0 - 150.0 * t);
            (saw(p) * 0.6 + n.next() * 0.25) * env(t, 0.02, 1.4)
        }),
        SfxEvent::Ping => render(0.9, |ts, t, _| {
            let p = o.step(1320.0);
            let echo = if ts > 0.3 { (o2.step(1320.0) * TAU).sin() * 0.35 * env((ts - 0.3) / 0.6, 0.02, 3.0) } else { 0.0 };
            (p * TAU).sin() * 0.7 * env(t.min(0.33) * 3.0, 0.02, 3.0) + echo
        }),
        SfxEvent::Overload => render(0.9, |_, t, n| {
            let p = o.step(60.0 + 40.0 * (1.0 - t));
            let crackle = if n.next() > 0.85 { n.next() } else { 0.0 };
            ((p * TAU).sin() * 0.8 + saw(o2.step(90.0)) * 0.3 + crackle * 0.8) * env(t, 0.03, 1.6)
        }),
        SfxEvent::JammerDestroyed => render(0.9, |_, t, n| {
            let p = o.step(800.0 * (1.0 - t) + 40.0);
            (lp.run(n.next(), 0.2) * 1.4 + square(p) * 0.3) * env(t, 0.01, 1.8)
        }),
        SfxEvent::UplinkStart => render(0.8, |ts, t, _| {
            let note = [523.25, 659.25, 783.99, 1046.5][((ts / 0.2) as usize).min(3)];
            square(o.step(note)) * 0.35 * env(t, 0.02, 0.6)
        }),
        SfxEvent::Extract => render(1.5, |_, t, n| {
            let p = o.step(200.0 + 1400.0 * t * t);
            ((p * TAU).sin() * 0.6 + lp.run(n.next(), 0.05 + t * 0.4) * 0.6) * env(t, 0.3, 1.2)
        }),
        SfxEvent::Hit => render(0.06, |_, t, _| square(o.step(1800.0)) * 0.3 * env(t, 0.01, 2.0)),
        SfxEvent::EnemyDeath => render(0.55, |_, t, n| {
            let p = o.step(180.0 - 140.0 * t);
            (saw(p) * 0.5 + lp.run(n.next(), 0.3) * 0.7) * env(t, 0.02, 1.5)
        }),
        SfxEvent::EnemyFire => render(0.3, |_, t, n| {
            let p = o.step(500.0 - 300.0 * t);
            (tri(p) * 0.4 + lp.run(n.next(), 0.15) * 0.6) * env(t, 0.05, 1.5)
        }),
        SfxEvent::Pickup => render(0.18, |ts, t, _| {
            let f = if ts < 0.08 { 880.0 } else { 1318.5 };
            square(o.step(f)) * 0.3 * env(t, 0.02, 1.0)
        }),
        SfxEvent::WeaponSwap => render(0.12, |_, t, n| (n.next() * 0.3 + square(o.step(160.0)) * 0.2) * env(t, 0.05, 2.0)),
        SfxEvent::BaronAlert => render(1.2, |ts, t, _| {
            let f = if ((ts * 4.0) as u32).is_multiple_of(2) { 440.0 } else { 330.0 };
            saw(o.step(f)) * 0.45 * env(t, 0.02, 0.5)
        }),
        SfxEvent::Denied => render(0.2, |_, t, _| square(o.step(110.0)) * 0.35 * env(t, 0.02, 0.8)),
        SfxEvent::UpgradeSelect => render(0.5, |ts, t, _| {
            let note = [659.25, 987.77, 1318.5][((ts / 0.12) as usize).min(2)];
            (tri(o.step(note)) * 0.5) * env(t, 0.02, 0.8)
        }),
        SfxEvent::RocketFire => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.5, move |ts, t, n| {
                // Pneumatic thump: fast downward sine kick in the first 60ms.
                let thump = if ts < 0.09 { (o.step(140.0 - 900.0 * ts) * TAU).sin() * env(ts / 0.09, 0.05, 2.0) } else { 0.0 };
                // Propellant whoosh: band-ish noise (high-pass via two low-passes) rising then fading.
                let x = n.next();
                let band = lp.run(x, 0.45) - lp2.run(x, 0.06);
                let whoosh = band * 1.4 * env(t, 0.18, 1.3);
                let hiss = saw(o2.step(70.0 + 60.0 * t)) * 0.12 * env(t, 0.1, 1.5);
                thump * 0.9 + whoosh + hiss
            })
        }
        SfxEvent::RocketExplode => {
            let mut lp2 = Lp { y: 0.0 };
            render(1.1, move |ts, t, n| {
                // Sub-bass boom: pitch-dropping sine with a long tail.
                let boom = (o.step(58.0 * (1.0 - 0.55 * t) + 18.0) * TAU).sin() * env(t, 0.008, 1.8);
                // Low-passed debris body + sparse crackle that darkens over time.
                let body = lp.run(n.next(), 0.22 - 0.18 * t) * 1.8 * env(t, 0.004, 2.2);
                let crack = if ts > 0.05 && n.next() > 0.9 { n.next() } else { 0.0 };
                let crackle = lp2.run(crack, 0.5 - 0.4 * t) * 0.9 * env(t, 0.05, 1.4);
                boom * 0.95 + body + crackle
            })
        }
        SfxEvent::JumpPad => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.42, move |ts, t, n| {
                // Pneumatic burst: band-passed noise puff in the first 80ms.
                let x = n.next();
                let puff = if ts < 0.08 { (lp.run(x, 0.5) - lp2.run(x, 0.05)) * 1.6 * env(ts / 0.08, 0.05, 1.5) } else { 0.0 };
                // Energetic rising synth sweep (saw + square fifth).
                let p = o.step(180.0 + 1300.0 * t * t.sqrt());
                let fifth = square(o2.step((180.0 + 1300.0 * t * t.sqrt()) * 1.5)) * 0.15;
                puff + (saw(p) * 0.35 + fifth) * env(t, 0.06, 1.1)
            })
        }
        SfxEvent::PhaseBlink => {
            let mut hold = 0.0f32;
            render(0.38, move |ts, t, n| {
                // Pitch warps down then snaps up; sample-and-hold static bursts.
                let warp = 900.0 * (1.0 - t * 1.6).abs() + 160.0;
                let p = o.step(warp + 120.0 * (ts * 55.0).sin());
                if ((ts * 2400.0) as u32).is_multiple_of(7) {
                    hold = n.next();
                }
                let glitch = if ((ts * 30.0) as u32).is_multiple_of(2) { square(p) * 0.35 } else { tri(o2.step(warp * 1.5)) * 0.3 };
                (glitch + hold * 0.4) * env(t, 0.04, 1.4)
            })
        }
        SfxEvent::SniperLock => render(0.7, |ts, t, _| {
            // Rising whine + a lock-confirm double beep at the end.
            let p = o.step(600.0 + 2600.0 * t * t);
            let beep = if ts > 0.56 && ((ts * 40.0) as u32).is_multiple_of(2) { square(o2.step(3200.0)) * 0.25 } else { 0.0 };
            ((p * TAU).sin() * 0.45 + saw(p) * 0.12) * env(t, 0.6, 0.6) + beep
        }),
        SfxEvent::BossShield => render(1.1, |ts, t, n| {
            // Crystalline chord (stacked fifths) with shimmer + a glassy noise crack.
            let base = 220.0 * (1.0 + 0.04 * (ts * 9.0).sin());
            let chord = (o.step(base) * TAU).sin() * 0.4 + (o2.step(base * 1.5) * TAU).sin() * 0.3 + tri(base * 4.0 * ts) * 0.15;
            let crack = if ts < 0.12 { lp.run(n.next(), 0.7) * (1.0 - ts / 0.12) * 0.9 } else { 0.0 };
            chord * env(t, 0.05, 1.3) + crack
        }),
        SfxEvent::StepRegolith => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.13, move |ts, t, n| {
                // Granular crunch: sparse noise grains, band-limited, over a soft heel thump.
                let x = n.next();
                let grain = if n.next() > 0.45 { x } else { 0.0 };
                let crunch = (lp.run(grain, 0.5) - lp2.run(grain, 0.04)) * 1.5;
                let thump = (o.step(110.0 - 50.0 * t) * TAU).sin() * 0.55 * decay(ts, 45.0);
                (crunch * env(t, 0.06, 1.6) + thump) * env(t, 0.01, 1.0)
            })
        }
        SfxEvent::StepMetal => render(0.24, |ts, t, n| {
            // Hollow catwalk clang: inharmonic grating partials + a sharp heel click.
            let p = o.step(392.0);
            let ring = (p * TAU).sin() * 0.45 + (p * 2.76 * TAU).sin() * 0.3 * decay(ts, 18.0) + (o2.step(1_563.0) * TAU).sin() * 0.2 * decay(ts, 30.0);
            let click = if ts < 0.006 { n.next() * 0.8 } else { 0.0 };
            ring * decay(ts, 14.0) * env(t, 0.01, 0.8) + click
        }),
        SfxEvent::LandThud => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.34, move |ts, t, n| {
                // Pitch-dropping body thump + low-passed dust / gear rattle.
                let body = (o.step(34.0 + 70.0 * decay(ts, 22.0)) * TAU).sin() * decay(ts, 11.0);
                let dust = lp.run(n.next(), 0.12) * 1.4 * decay(ts, 16.0);
                let rattle = lp2.run(if n.next() > 0.8 { n.next() } else { 0.0 }, 0.4) * 0.5 * decay(ts, 25.0);
                (body * 0.95 + dust + rattle) * env(t, 0.01, 1.2)
            })
        }
        SfxEvent::ShellClink => render(0.17, |ts, _t, _| {
            // Bright brass tube partials; a smaller second tap 55 ms later.
            let p = o.step(3_150.0);
            let tone = (p * TAU).sin() * 0.5 + (p * 1.59 * TAU).sin() * 0.3 + (p * 2.31 * TAU).sin() * 0.18;
            let tap1 = decay(ts, 38.0);
            let tap2 = if ts > 0.055 { decay(ts - 0.055, 46.0) * 0.55 } else { 0.0 };
            tone * (tap1 + tap2)
        }),
        SfxEvent::RocketWhoosh => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.62, move |_, t, n| {
                // Band-passed rush whose centre sweeps up then settles; slight flutter.
                let x = n.next();
                let k = 0.08 + 0.42 * (1.0 - (2.0 * t - 0.6).abs()).max(0.0);
                let band = lp.run(x, k) - lp2.run(x, k * 0.18);
                let flutter = 0.8 + 0.2 * (o.step(23.0) * TAU).sin();
                band * 2.2 * flutter * env(t, 0.22, 1.5)
            })
        }
        SfxEvent::JammerHum => {
            let mut hold = 0.0f32;
            render(0.9, move |ts, t, n| {
                // Mains-style hum (fundamental + odd harmonics), pulsing at 6 Hz,
                // with sample-and-hold RF static and a high harmonic whine.
                let p = o.step(98.0);
                let hum = (p * TAU).sin() * 0.45 + saw(p * 3.0) * 0.12 + (p * 5.0 * TAU).sin() * 0.12;
                let pulse = 0.55 + 0.45 * (TAU * 6.0 * ts).sin().max(0.0);
                if ((ts * 1800.0) as u32).is_multiple_of(5) {
                    hold = n.next();
                }
                let stat = lp.run(hold, 0.35) * 0.35;
                let whine = (o2.step(1_960.0 + 40.0 * (TAU * 3.0 * ts).sin()) * TAU).sin() * 0.08;
                (hum * pulse + stat + whine) * env(t, 0.12, 0.9)
            })
        }
        SfxEvent::PitRescue => render(0.42, |ts, t, _| {
            // Rising major arpeggio (E5 -> G#5 -> B5 -> E6) on a soft triangle
            // with a sine octave shimmer: "recovered", not "denied".
            let note = [659.25, 830.61, 987.77, 1318.5][((ts / 0.07) as usize).min(3)];
            let body = tri(o.step(note)) * 0.5 + (o2.step(note * 2.0) * TAU).sin() * 0.18;
            body * env(t, 0.03, 1.1)
        }),
        SfxEvent::UiTick => render(0.035, |_, t, n| {
            // Tiny high square click with a breath of noise.
            (square(o.step(2_400.0)) * 0.35 + n.next() * 0.06) * env(t, 0.04, 3.0)
        }),
        SfxEvent::UiConfirm => render(0.11, |ts, t, _| {
            // Two-step upward blip (A5 -> E6).
            let f = if ts < 0.045 { 880.0 } else { 1318.5 };
            (square(o.step(f)) * 0.3 + tri(o2.step(f * 0.5)) * 0.2) * env(t, 0.03, 1.4)
        }),
        SfxEvent::PhonePing => render(0.32, |ts, t, _| {
            // Two short sine chirps (B6 then E7), each with its own fast
            // attack / decay, plus a soft octave-down body.
            let (start, f) = if ts < 0.13 { (0.0, 1975.5) } else { (0.13, 2637.0) };
            let local = ts - start;
            let chirp = (o.step(f * (1.0 + 0.04 * decay(local, 40.0))) * TAU).sin();
            let body = tri(o2.step(f * 0.5)) * 0.25;
            let gate = (local / 0.006).min(1.0) * decay(local, 22.0);
            (chirp * 0.7 + body) * gate * env(t, 0.01, 0.6)
        }),
        SfxEvent::Fatality => {
            let mut hold = 0.0f32;
            render(0.58, move |ts, t, n| {
                // Sub thump with a falling pitch, sample-and-hold bone crunch,
                // and a bright snap transient on top.
                let thump = (o.step(62.0 + 120.0 * decay(ts, 18.0)) * TAU).sin() * decay(ts, 6.5);
                if ((ts * 2400.0) as u32).is_multiple_of(7) {
                    hold = n.next();
                }
                let crunch = lp.run(hold, 0.5) * decay((ts - 0.02).max(0.0), 14.0) * 0.8;
                let snap = if ts < 0.018 { n.next() * 0.9 * (1.0 - ts / 0.018) } else { 0.0 };
                let ring = saw(o2.step(196.0 - 60.0 * t)) * 0.12 * decay(ts, 9.0);
                (thump * 0.95 + crunch + snap + ring) * env(t, 0.004, 1.2)
            })
        }
        SfxEvent::StyleRankUp => render(0.24, |ts, t, _| {
            // E5 -> B5 square/saw stab with an octave sine sparkle.
            let f = if ts < 0.07 { 659.25 } else { 987.77 };
            let local = if ts < 0.07 { ts } else { ts - 0.07 };
            let body = square(o.step(f)) * 0.32 + saw(o2.step(f * 1.005)) * 0.22;
            let sparkle = (TAU * f * 2.0 * ts).sin() * 0.18 * decay(local, 14.0);
            (body * (local / 0.004).min(1.0) * decay(local, 9.0) + sparkle) * env(t, 0.01, 1.1)
        }),
        SfxEvent::StylePeak => render(0.46, |ts, t, n| {
            // Rising E5-G#5-B5-E6 arpeggio (supersaw pair) + white-noise crack.
            const NOTES: [f32; 4] = [659.25, 830.61, 987.77, 1318.5];
            let k = ((ts / 0.06) as usize).min(3);
            let local = ts - k as f32 * 0.06;
            let f = NOTES[k];
            let lead = saw(o.step(f)) * 0.3 + saw(o2.step(f * 1.008)) * 0.3;
            let gate = (local / 0.003).min(1.0) * if k < 3 { decay(local, 10.0) } else { decay(local, 4.5) };
            let crack = if ts < 0.012 { n.next() * 0.7 * (1.0 - ts / 0.012) } else { 0.0 };
            let shimmer = (TAU * 2637.0 * ts).sin() * 0.1 * decay(ts, 6.0);
            (lp.run(lead, 0.55) * gate + crack + shimmer) * env(t, 0.004, 0.9)
        }),
        SfxEvent::CrtShatter => {
            let mut hold = 0.0f32;
            render(0.44, move |ts, t, n| {
                // Glass: sample-and-hold high-passed noise bursts + inharmonic
                // pings, a dying 7.8 kHz flyback whine and a dull tube thunk.
                let x = n.next();
                if ((ts * 9000.0) as u32).is_multiple_of(3) {
                    hold = x;
                }
                let hp = hold - lp.run(hold, 0.3);
                let glass = hp * decay(ts, 11.0) * 0.9;
                let ping = ((TAU * 3150.0 * ts).sin() * 0.25 + (TAU * 4730.0 * ts).sin() * 0.18 + (TAU * 6020.0 * ts).sin() * 0.12) * decay(ts, 16.0);
                let whine = (o.step(7800.0 - 2600.0 * t) * TAU).sin() * 0.12 * decay(ts, 7.0);
                let thunk = (o2.step(140.0 - 60.0 * t) * TAU).sin() * 0.45 * decay(ts, 22.0);
                (glass + ping + whine + thunk) * env(t, 0.003, 1.0)
            })
        }
        SfxEvent::UpsDetonate => {
            let mut hold = 0.0f32;
            render(0.95, move |ts, t, n| {
                // Deep falling boom, an FM "EMP" zap sweeping down and a
                // low-passed debris crackle tail.
                let boom = (o.step(38.0 + 90.0 * decay(ts, 9.0)) * TAU).sin() * decay(ts, 3.6);
                let zap_f = 1800.0 * decay(ts, 6.0) + 120.0;
                let zap = (o2.step(zap_f) * TAU + 2.5 * (TAU * zap_f * 0.5 * ts).sin()).sin() * 0.35 * decay(ts, 8.0);
                if ((ts * 1400.0) as u32).is_multiple_of(5) {
                    hold = n.next();
                }
                let crackle = lp.run(hold, 0.35) * 0.6 * decay((ts - 0.05).max(0.0), 4.0);
                let snap = if ts < 0.02 { n.next() * 0.8 * (1.0 - ts / 0.02) } else { 0.0 };
                (boom * 0.95 + zap + crackle + snap) * env(t, 0.003, 1.3)
            })
        }
        SfxEvent::UpsHiss => render(0.26, |ts, t, n| {
            // Pressurised hiss: band-limited noise with spark ticks.
            let x = n.next();
            let hiss = (x - lp.run(x, 0.12)) * 0.6;
            let tick = if ((ts * 60.0) as u32).is_multiple_of(4) && (ts * 60.0).fract() < 0.08 { n.next() * 0.6 } else { 0.0 };
            (hiss + tick) * env(t, 0.06, 1.4)
        }),
        SfxEvent::PhonePunch => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.22, move |ts, t, n| {
                // Fast band-passed air whip sweeping up, then a punchy
                // pitch-dropping knuckle thud and a plastic bezel tick.
                let x = n.next();
                let k = 0.1 + 0.5 * (ts / 0.06).min(1.0);
                let whip = if ts < 0.07 { (lp.run(x, k) - lp2.run(x, k * 0.2)) * 1.8 * (ts / 0.07) } else { 0.0 };
                let hit_t = (ts - 0.05).max(0.0);
                let thud = if ts >= 0.05 { (o.step(70.0 + 160.0 * decay(hit_t, 30.0)) * TAU).sin() * decay(hit_t, 16.0) } else { 0.0 };
                let tick = if (0.05..0.056).contains(&ts) { n.next() * 0.7 } else { 0.0 };
                let body = tri(o2.step(240.0 - 90.0 * t)) * 0.2 * decay(hit_t, 22.0);
                (whip + thud * 0.95 + tick + body) * env(t, 0.004, 1.1)
            })
        }
        SfxEvent::PhoneParry => render(0.52, |ts, t, n| {
            // Hard metallic clang (inharmonic partials) + a rising golden
            // two-note ring (A5 -> E6) and a bright noise crack.
            let clang = ((o.step(523.0) * TAU).sin() * 0.35 + (TAU * 1_391.0 * ts).sin() * 0.22 + (TAU * 2_217.0 * ts).sin() * 0.14) * decay(ts, 9.0);
            let f = if ts < 0.09 { 880.0 } else { 1_318.5 };
            let local = if ts < 0.09 { ts } else { ts - 0.09 };
            let ring = (saw(o2.step(f)) * 0.18 + (TAU * f * 2.0 * ts).sin() * 0.12) * (local / 0.004).min(1.0) * decay(local, 6.0);
            let crack = if ts < 0.014 { n.next() * 0.85 * (1.0 - ts / 0.014) } else { 0.0 };
            (clang + ring + crack) * env(t, 0.003, 0.9)
        }),
        SfxEvent::CameraFlash => {
            let mut lp2 = Lp { y: 0.0 };
            render(0.4, move |ts, t, n| {
                // Shutter: two tight mechanical clicks; then the strobe's
                // capacitor whine sweeping up into a bright pop + fizz.
                let x = n.next();
                let click = |at: f32| if (at..at + 0.005).contains(&ts) { 1.0 - (ts - at) / 0.005 } else { 0.0 };
                let shutter = (x - lp.run(x, 0.25)) * (click(0.0) + click(0.045) * 0.8) * 1.6;
                let whine = if ts < 0.09 { (o.step(2_400.0 + 5_200.0 * (ts / 0.09)) * TAU).sin() * 0.16 * (ts / 0.09) } else { 0.0 };
                let pop_t = (ts - 0.09).max(0.0);
                let pop = if ts >= 0.09 { (o2.step(180.0 + 600.0 * decay(pop_t, 40.0)) * TAU).sin() * 0.6 * decay(pop_t, 18.0) } else { 0.0 };
                let fizz = if ts >= 0.09 { (x - lp2.run(x, 0.4)) * 0.45 * decay(pop_t, 9.0) } else { 0.0 };
                (shutter + whine + pop + fizz) * env(t, 0.003, 1.0)
            })
        }
    }
}

/// Minimum seconds between two [`SfxEvent::UiTick`]s (a held direction
/// repeats every 0.22 s; mashing faster than this stays quiet).
pub const UI_TICK_MIN_GAP: f32 = 0.09;
/// Minimum seconds between two [`SfxEvent::UiConfirm`]s.
pub const UI_CONFIRM_MIN_GAP: f32 = 0.15;

/// Debounces menu UI cues so a held direction / mashed button never spams.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct UiSfxGate {
    last_tick: f32,
    last_confirm: f32,
}

impl Default for UiSfxGate {
    fn default() -> Self {
        UiSfxGate { last_tick: f32::NEG_INFINITY, last_confirm: f32::NEG_INFINITY }
    }
}

impl UiSfxGate {
    /// Whether `cue` may sound at `now` (seconds); records it when allowed.
    /// A confirm also swallows a tick in the same instant.
    pub fn allow(&mut self, cue: SfxEvent, now: f32) -> bool {
        let (last, gap) = match cue {
            SfxEvent::UiTick => (&mut self.last_tick, UI_TICK_MIN_GAP),
            SfxEvent::UiConfirm => (&mut self.last_confirm, UI_CONFIRM_MIN_GAP),
            _ => return true,
        };
        if now.is_finite() && now - *last < gap && now >= *last {
            return false;
        }
        *last = now;
        if cue == SfxEvent::UiConfirm {
            self.last_tick = now;
        }
        true
    }

    /// Queue `cue` if the gate allows it.
    pub fn play(&mut self, queue: &mut SfxQueue, cue: SfxEvent, now: f32) {
        if self.allow(cue, now) {
            queue.play(cue);
        }
    }
}

/// Wrap mono `f32` samples (-1..1) in a 16-bit PCM RIFF/WAVE container.
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Synthesize a complete WAV file for one sound effect.
pub fn synthesize_wav(sfx: SfxEvent) -> Vec<u8> {
    let mut s = synth_samples(sfx);
    // Normalize peaks to 0.9 and add 4ms fade-out to avoid clicks.
    let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak > 1e-4 {
        let g = 0.9 / peak;
        s.iter_mut().for_each(|v| *v *= g);
    }
    let fade = (SAMPLE_RATE as usize / 250).min(s.len());
    let n = s.len();
    for i in 0..fade {
        s[n - 1 - i] *= i as f32 / fade as f32;
    }
    encode_wav(&s, SAMPLE_RATE)
}

// ---------------------------------------------------------------------------
// Adaptive 4-layer industrial-synth soundtrack
// ---------------------------------------------------------------------------

/// Tempo of every music stem.
pub const MUSIC_BPM: f32 = 132.0;
/// 16th-note steps per loop (4 bars of 4/4).
pub const MUSIC_STEPS: usize = 64;
/// Samples per 16th-note step (`SAMPLE_RATE * 60 / BPM / 4`, floored so every
/// step, and therefore every stem, has the exact same integer length).
pub const MUSIC_STEP_SAMPLES: usize = (SAMPLE_RATE as usize * 15 * 10) / (MUSIC_BPM as usize * 10);
/// Exact sample count shared by all four stems (phase-locked crossfades).
pub const MUSIC_LOOP_SAMPLES: usize = MUSIC_STEPS * MUSIC_STEP_SAMPLES;
/// Overlap rendered past the loop end and folded back over the start (20 ms).
const MUSIC_SEAM_SAMPLES: usize = SAMPLE_RATE as usize / 50;
/// Peak level every stem is normalized to.
pub const MUSIC_PEAK: f32 = 0.85;
/// Base mix level of a fully weighted stem (before the master volume).
pub const MUSIC_BASE_VOLUME: f32 = 0.20;
/// Exponential crossfade rate of [`step_music_mix`] (1/s).
pub const MUSIC_MIX_RATE: f32 = 3.0;

/// The four phase-locked music stems mixed live by game state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MusicLayer {
    /// Low industrial sub-bass drone with RF static (Dead Zones, menus).
    DeadZoneDrone,
    /// Driving E-Phrygian 16th bass + clock-tick hats + radar pings (exploring).
    SignalPulse,
    /// Industrial-metal combat stem (SOS Demon Rush, Carrier Daemon).
    RushMetal,
    /// Carrier Daemon stem: double-time bass gallop, distorted E Phrygian
    /// Dominant alarm lead and double-kick / snare fills.
    BossOverdrive,
    /// Milestone 31 **6G Overclock** layer: supersaw 16th arpeggio with a
    /// sweeping filter, quarter-note pump and offbeat open hats. Rides on top
    /// of the combat mix while the Style Rank is `S` (FIBER) or higher.
    Overclock6G,
}

/// Number of state-mixed base stems (the 6G layer has its own weight).
pub const MUSIC_LAYERS: usize = 4;

impl MusicLayer {
    /// The four state-mixed base stems.
    pub const ALL: [MusicLayer; MUSIC_LAYERS] = [MusicLayer::DeadZoneDrone, MusicLayer::SignalPulse, MusicLayer::RushMetal, MusicLayer::BossOverdrive];
    /// Every phase-locked stem the backend plays (base stems + 6G layer).
    pub const STEMS: [MusicLayer; MUSIC_LAYERS + 1] =
        [MusicLayer::DeadZoneDrone, MusicLayer::SignalPulse, MusicLayer::RushMetal, MusicLayer::BossOverdrive, MusicLayer::Overclock6G];

    pub fn index(self) -> usize {
        match self {
            MusicLayer::DeadZoneDrone => 0,
            MusicLayer::SignalPulse => 1,
            MusicLayer::RushMetal => 2,
            MusicLayer::BossOverdrive => 3,
            MusicLayer::Overclock6G => 4,
        }
    }
}

/// Frequency of `semis` semitones above E1 (41.2 Hz).
fn e1_semis(semis: i32) -> f32 {
    41.203 * 2f32.powf(semis as f32 / 12.0)
}

/// Sample index → (loop step 0..64, seconds since the step started).
fn step_at(i: usize) -> (usize, f32) {
    ((i / MUSIC_STEP_SAMPLES) % MUSIC_STEPS, (i % MUSIC_STEP_SAMPLES) as f32 / SAMPLE_RATE as f32)
}

/// One-shot decay `exp(-t * k)`.
fn decay(t: f32, k: f32) -> f32 {
    (-t * k).exp()
}

/// Render `n` samples of one stem; `f(i, loop_phase 0..1, noise)` is driven by
/// the absolute sample index so the material past `MUSIC_LOOP_SAMPLES`
/// continues exactly where the loop restarts.
fn render_music(n: usize, mut f: impl FnMut(usize, f32, &mut Noise) -> f32) -> Vec<f32> {
    let mut noise = Noise(0x2545_F491);
    (0..n).map(|i| f(i, (i % MUSIC_LOOP_SAMPLES) as f32 / MUSIC_LOOP_SAMPLES as f32, &mut noise)).collect()
}

fn music_raw(layer: MusicLayer, n: usize) -> Vec<f32> {
    let mut o = [Osc { phase: 0.0 }, Osc { phase: 0.0 }, Osc { phase: 0.0 }, Osc { phase: 0.0 }, Osc { phase: 0.0 }, Osc { phase: 0.0 }];
    let mut lp = [Lp { y: 0.0 }, Lp { y: 0.0 }, Lp { y: 0.0 }];
    match layer {
        MusicLayer::DeadZoneDrone => {
            let mut crackle = 0.0f32;
            render_music(n, move |_i, lph, nz| {
                // Two slow LFOs with whole-number cycles per loop (seamless).
                let swell = 0.75 + 0.25 * (TAU * lph).sin();
                let wobble = (TAU * 4.0 * lph).sin();
                let sub = (o[0].step(e1_semis(0)) * TAU).sin() * 0.7 + (o[1].step(e1_semis(12) * 1.002) * TAU).sin() * 0.22;
                // Detuned fifth (B1) frequency-modulated by the tritone (A#1).
                let index = 1.2 + 0.9 * wobble;
                let modu = (o[2].step(e1_semis(6) * 0.997) * TAU).sin() * index;
                let shimmer = (o[3].step(e1_semis(7) * 1.004) * TAU + modu).sin() * 0.28 * (0.6 + 0.4 * wobble.abs());
                // Intermittent RF static: random crackle bursts, low-passed.
                let x = nz.next();
                if x > 0.9993 {
                    crackle = 1.0;
                }
                crackle *= 0.9985;
                let st = lp[0].run(nz.next() * crackle, 0.18) * 1.4;
                let hiss = lp[1].run(nz.next(), 0.03) * 0.25 * (0.5 + 0.5 * (TAU * 2.0 * lph).cos());
                (sub + shimmer) * swell + st + hiss
            })
        }
        MusicLayer::SignalPulse => {
            // E Phrygian 16th-note bass: per-bar root, per-step offsets (None = rest).
            const ROOTS: [i32; 4] = [12, 12, 13, 10];
            const SEQ: [Option<i32>; 16] = [
                Some(0), Some(0), Some(12), Some(0), Some(0), Some(7), Some(0), Some(12),
                Some(0), Some(0), Some(12), None, Some(3), Some(0), Some(7), Some(1),
            ];
            render_music(n, move |i, _lph, nz| {
                let (s, tl) = step_at(i);
                let bar = s / 16;
                let bass = match SEQ[s % 16] {
                    Some(off) => {
                        let f = e1_semis(ROOTS[bar] + off);
                        let raw = saw(o[0].step(f)) * 0.6 + square(o[1].step(f * 0.5)) * 0.25;
                        lp[0].run(raw, 0.08 + 0.25 * decay(tl, 18.0)) * decay(tl, 6.0)
                    }
                    None => lp[0].run(0.0, 0.1),
                };
                // Metallic clock-tick hats: high-passed noise + inharmonic squares.
                let x = nz.next();
                let hp = x - lp[1].run(x, 0.35);
                let accent = if s % 4 == 2 { 1.0 } else { 0.55 };
                let metal = square(o[2].step(6_120.0)) * 0.3 + square(o[3].step(8_310.0)) * 0.2;
                let hat = (hp * 0.6 + metal * 0.25) * decay(tl, 70.0) * accent * 0.55;
                // Radar ping on each bar's downbeat: E6 with 2x/3x overtones.
                let downbeat = s.is_multiple_of(16);
                let ping = if downbeat || s % 16 == 10 {
                    let p = o[4].step(1_318.5) * TAU;
                    (p.sin() * 0.5 + (2.0 * p).sin() * 0.2 + (3.0 * p).sin() * 0.1) * decay(tl, 9.0) * if downbeat { 0.5 } else { 0.25 }
                } else {
                    o[4].step(1_318.5);
                    0.0
                };
                bass + hat + ping
            })
        }
        MusicLayer::RushMetal => {
            // Chugging power-bass riff (semitones over E1, None = palm-muted rest).
            const RIFF: [Option<i32>; 16] = [
                Some(0), Some(0), None, Some(0), Some(0), None, Some(1), Some(0),
                Some(0), Some(0), None, Some(0), Some(3), None, Some(1), Some(-2),
            ];
            const KICKS: [bool; 16] = [true, false, false, false, true, false, false, true, true, false, true, false, true, false, false, false];
            render_music(n, move |i, _lph, nz| {
                let (s, tl) = step_at(i);
                let bar = s / 16;
                let st = s % 16;
                // Distorted saw/square power chord (root + fifth + octave).
                let riff = match RIFF[st] {
                    Some(off) => {
                        let root = e1_semis(12 + off + if bar == 3 && st >= 12 { 1 } else { 0 });
                        let raw = saw(o[0].step(root)) + square(o[1].step(root * 1.498)) * 0.7 + saw(o[2].step(root * 2.003)) * 0.5;
                        (lp[0].run(raw, 0.22) * 2.6).tanh() * 0.42 * (0.35 + 0.65 * decay(tl, 7.0))
                    }
                    None => lp[0].run(0.0, 0.3) * 0.4,
                };
                // Punchy kick: fast pitch drop 150 -> 45 Hz.
                let kick = if KICKS[st] {
                    (o[3].step(45.0 + 105.0 * decay(tl, 30.0)) * TAU).sin() * decay(tl, 14.0) * 0.9
                } else {
                    0.0
                };
                // Noise snare on 2 and 4 with a 190 Hz body.
                let x = nz.next();
                let snare = if st == 4 || st == 12 {
                    (x * 0.75 + (TAU * 190.0 * tl).sin() * 0.35) * decay(tl, 22.0) * 0.8
                } else {
                    0.0
                };
                // Driving 16th hats.
                let hp = x - lp[1].run(x, 0.4);
                let hat = hp * decay(tl, 55.0) * if st % 2 == 1 { 0.45 } else { 0.25 };
                // Searing octave + fifth lead stabs (E4/B4) on the off-beat pushes.
                let lead = if matches!(st, 2 | 3 | 10 | 11) && bar % 2 == 1 {
                    let f = e1_semis(36 + if bar == 3 { 1 } else { 0 });
                    let p = o[4].step(f);
                    let p5 = o[5].step(f * 1.5);
                    lp[2].run(saw(p) + saw(p5) * 0.6 + saw(p * 2.0) * 0.4, 0.5) * 0.28 * decay(tl, 5.0)
                } else {
                    lp[2].run(0.0, 0.5) * 0.28
                };
                riff + kick + snare + hat + lead
            })
        }
        MusicLayer::BossOverdrive => {
            // Gallop bass (semitones over E1): 16th-note "da-da-DUM" figure,
            // root per bar walks the Phrygian Dominant degrees E - F - G# - F.
            const ROOTS: [i32; 4] = [12, 13, 16, 13];
            const GALLOP: [i32; 16] = [0, 0, 12, 0, 0, 12, 0, 0, 12, 0, 0, 12, 0, 7, 8, 1];
            // Alarm lead (E Phrygian Dominant: E F G# A B C D), one note per 8th,
            // over E4; the last bar climbs into a shrieking turnaround.
            const LEAD: [[i32; 8]; 4] = [
                [0, 1, 0, 4, 0, 1, 0, 4],
                [0, 1, 4, 5, 4, 1, 0, -1],
                [0, 1, 0, 4, 0, 1, 0, 4],
                [7, 8, 7, 4, 5, 4, 1, 0],
            ];
            render_music(n, move |i, _lph, nz| {
                let (s, tl) = step_at(i);
                let bar = s / 16;
                let st = s % 16;
                // Double-time gallop bass: every 16th, accents on the octave hits.
                let f = e1_semis(ROOTS[bar] + GALLOP[st]);
                let raw = saw(o[0].step(f)) + square(o[1].step(f * 0.501)) * 0.6;
                let accent = if GALLOP[st] != 0 { 1.0 } else { 0.7 };
                // 2 ms attacks on every retrigger keep the step onsets (and the loop wrap) click-free.
                let atk = |t: f32| (t / 0.002).min(1.0);
                let bass = (lp[0].run(raw, 0.12 + 0.3 * decay(tl, 25.0)) * 3.0).tanh() * 0.38 * accent * (0.3 + 0.7 * decay(tl, 9.0) * atk(tl));
                // Distorted alarm lead: detuned saw pair with a siren vibrato, hard-clipped.
                let note = LEAD[bar][st / 2];
                let lf = e1_semis(36 + note);
                let vib = 1.0 + 0.012 * (TAU * 7.0 * (i as f32 / SAMPLE_RATE as f32)).sin();
                let lraw = saw(o[2].step(lf * vib)) + saw(o[3].step(lf * vib * 1.007)) * 0.8 + square(o[4].step(lf * 2.0)) * 0.25;
                let t8 = tl + (st % 2) as f32 * MUSIC_STEP_SAMPLES as f32 / SAMPLE_RATE as f32;
                let lead = (lp[1].run(lraw, 0.35) * 2.4).tanh() * 0.24 * (0.55 + 0.45 * decay(t8, 3.5) * atk(t8));
                // Relentless double kick on every 16th (fills thin it to 8ths).
                let fill = bar == 3 && st >= 12;
                let kick_on = if fill { st % 2 == 0 } else { true };
                let kick = if kick_on {
                    (o[5].step(48.0 + 95.0 * decay(tl, 38.0)) * TAU).sin() * decay(tl, 20.0) * atk(tl) * if st % 4 == 0 { 0.85 } else { 0.55 }
                } else {
                    0.0
                };
                // Snare backbeat on 2 / 4, rolled 16ths through the bar-4 fill.
                let x = nz.next();
                let snare = if st == 4 || st == 12 || fill {
                    let g = if fill { 0.45 + 0.1 * (st - 12) as f32 } else { 0.8 };
                    (lp[2].run(x, 0.6) * 0.8 + (TAU * 210.0 * tl).sin() * 0.3) * decay(tl, 24.0) * atk(tl) * g
                } else {
                    lp[2].run(0.0, 0.6);
                    0.0
                };
                bass + lead + kick + snare
            })
        }
        MusicLayer::Overclock6G => {
            // Per-bar arpeggio chord (semitones over E4), E Phrygian.
            const ARP: [[i32; 4]; 4] = [[0, 3, 7, 12], [1, 5, 8, 13], [0, 3, 7, 12], [-2, 3, 5, 10]];
            render_music(n, move |i, lph, nz| {
                let (s, tl) = step_at(i);
                let bar = s / 16;
                let st = s % 16;
                let atk = |t: f32| (t / 0.002).min(1.0);
                let oct = if st % 8 >= 4 { 12 } else { 0 };
                let f = e1_semis(36 + ARP[bar][st % 4] + oct);
                // Detuned supersaw through a filter that opens twice per loop.
                let raw = saw(o[0].step(f)) + saw(o[1].step(f * 1.006)) * 0.8 + saw(o[2].step(f * 0.994)) * 0.8;
                let cutoff = 0.12 + 0.3 * (0.5 - 0.5 * (TAU * 2.0 * lph).cos());
                let lead = lp[0].run(raw, cutoff) * 0.34 * (0.25 + 0.75 * decay(tl, 7.5) * atk(tl));
                // Quarter-note sidechain pump.
                let q = (s % 4) as f32 * MUSIC_STEP_SAMPLES as f32 / SAMPLE_RATE as f32 + tl;
                let pump = 1.0 - 0.55 * decay(q, 10.0);
                // Offbeat open hats + a glassy octave ping on each bar.
                let x = nz.next();
                let hp = x - lp[1].run(x, 0.45);
                let hat = if st % 4 == 2 { hp * decay(tl, 22.0) * 0.5 * atk(tl) } else { 0.0 };
                let ping = if st == 0 { (o[3].step(f * 2.0) * TAU).sin() * 0.22 * decay(tl, 5.0) * atk(tl) } else {
                    o[3].step(f * 2.0);
                    0.0
                };
                lead * pump + hat + ping
            })
        }
    }
}

/// Render one loop of `layer` as mono `f32` samples: exactly
/// [`MUSIC_LOOP_SAMPLES`] long, seam-crossfaded and normalized to [`MUSIC_PEAK`].
pub fn music_samples(layer: MusicLayer) -> Vec<f32> {
    let raw = music_raw(layer, MUSIC_LOOP_SAMPLES + MUSIC_SEAM_SAMPLES);
    // Fold the 20 ms rendered past the loop end over the start: the sample
    // after the last one is then the true continuation, so the wrap never clicks.
    let mut s: Vec<f32> = raw[..MUSIC_LOOP_SAMPLES].to_vec();
    for (i, (v, tail)) in s.iter_mut().zip(&raw[MUSIC_LOOP_SAMPLES..]).enumerate() {
        let w = i as f32 / MUSIC_SEAM_SAMPLES as f32;
        *v = *v * w + tail * (1.0 - w);
    }
    let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak > 1e-4 {
        let g = MUSIC_PEAK / peak;
        s.iter_mut().for_each(|v| *v *= g);
    }
    s
}

/// Synthesize one seamless music loop as a 16-bit mono WAV.
pub fn synthesize_music_wav(layer: MusicLayer) -> Vec<u8> {
    encode_wav(&music_samples(layer), SAMPLE_RATE)
}

/// Everything the soundtrack reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MusicCues {
    pub state: AppState,
    /// Signal bars at the player (0..=5).
    pub bars: u8,
    /// SOS Upload in progress (Demon Rush).
    pub uploading: bool,
    /// Carrier Daemon fight live.
    pub boss_active: bool,
    /// ...and in its final Overdrive phase.
    pub boss_overdrive: bool,
    /// Movement time trial run.
    pub time_trial: bool,
    /// Style Rank tier (`0` = empty meter, `1..=7` = `D..SSS`).
    pub style_tier: u8,
}

/// Target stem weights `[drone, pulse, rush, boss]` for the current game situation.
pub fn target_music_weights(c: MusicCues) -> [f32; MUSIC_LAYERS] {
    match c.state {
        AppState::MainMenu | AppState::Loadout | AppState::ContractsFeats | AppState::Settings | AppState::Compendium | AppState::Leaderboard => [0.45, 0.15, 0.0, 0.0],
        AppState::Paused => [0.25, 0.0, 0.0, 0.0],
        AppState::UpgradeSelect | AppState::Victory => [0.15, 0.55, 0.0, 0.0],
        AppState::GameOver => [0.50, 0.0, 0.0, 0.0],
        AppState::Playing if c.boss_active && c.boss_overdrive => [0.04, 0.14, 0.62, 0.68],
        AppState::Playing if c.boss_active => [0.10, 0.22, 0.72, 0.60],
        AppState::Playing if c.uploading => [0.15, 0.35, 1.0, 0.0],
        AppState::Playing if c.time_trial => [0.0, 0.85, 0.70, 0.0],
        AppState::Playing if c.bars <= 1 => [0.95, 0.05, 0.0, 0.0],
        AppState::Playing if c.bars <= 3 => [0.45, 0.75, 0.0, 0.0],
        AppState::Playing => [0.20, 0.95, 0.25, 0.0],
    }
}

/// Style tier (`1..=7` = `D..SSS`) at which the 6G Overclock layer joins (`S`).
pub const OVERCLOCK_MIN_TIER: u8 = 5;

/// Target weight of the 6G Overclock layer: in a live non-trial run at `S`
/// (FIBER) or higher, peaking at `SS` (6G) / `SSS` (6G+). Under the Carrier
/// Daemon / SOS Demon Rush beds it rides lower to keep -1 dBFS of headroom.
pub fn target_overclock_weight(c: MusicCues) -> f32 {
    if c.state != AppState::Playing || c.time_trial || c.style_tier < OVERCLOCK_MIN_TIER {
        return 0.0;
    }
    let w = match c.style_tier {
        5 => 0.40,
        6 => 0.55,
        _ => 0.62,
    };
    // The Demon Rush / Carrier Daemon beds are already dense: ride lower.
    if c.boss_active || c.uploading { w * 0.4 } else { w }
}

/// Exponential crossfade of the stem weights toward `target` (frame-rate independent).
pub fn step_music_mix(current: [f32; MUSIC_LAYERS], target: [f32; MUSIC_LAYERS], dt: f32) -> [f32; MUSIC_LAYERS] {
    let k = 1.0 - (-MUSIC_MIX_RATE * dt.max(0.0)).exp();
    std::array::from_fn(|i| current[i] + (target[i] - current[i]) * k)
}

/// Final sink volume of a stem with mix `weight` under the user's settings.
pub fn effective_music_volume(weight: f32, settings: &UserSettings) -> f32 {
    if settings.audio_muted {
        return 0.0;
    }
    let master = if settings.audio_master_volume.is_finite() { settings.audio_master_volume.clamp(0.0, 1.0) } else { 1.0 };
    weight.clamp(0.0, 1.0) * MUSIC_BASE_VOLUME * master
}

/// Live stem weights `[drone, pulse, rush, boss]`, indexed by [`MusicLayer::index`].
#[derive(Resource, Debug, Clone)]
pub struct MusicMixState {
    pub weights: [f32; MUSIC_LAYERS],
    /// Live weight of the 6G Overclock layer.
    pub overclock: f32,
}

impl Default for MusicMixState {
    fn default() -> Self {
        MusicMixState { weights: target_music_weights(MusicCues { state: AppState::MainMenu, ..default() }), overclock: 0.0 }
    }
}

impl MusicMixState {
    /// Live weight of any stem.
    pub fn weight(&self, layer: MusicLayer) -> f32 {
        match layer {
            MusicLayer::Overclock6G => self.overclock,
            l => self.weights[l.index()],
        }
    }
}

/// Crossfade the soundtrack toward the current state / signal / SOS / boss / mode target.
#[allow(clippy::too_many_arguments)]
pub fn update_music_mix(
    time: Res<Time>,
    state: Option<Res<State<AppState>>>,
    signal: Option<Res<SignalState>>,
    extraction: Option<Res<ExtractionState>>,
    boss: Option<Res<BossHud>>,
    run: Option<Res<RunStats>>,
    mut mix: ResMut<MusicMixState>,
    style: Option<Res<crate::style::StyleMeter>>,
) {
    let boss = boss.as_deref();
    let cues = MusicCues {
        state: state.map(|s| *s.get()).unwrap_or_default(),
        bars: signal.map(|s| s.bars).unwrap_or(0),
        uploading: extraction.is_some_and(|e| matches!(*e, ExtractionState::Uploading { .. })),
        boss_active: boss.is_some_and(|b| b.active),
        boss_overdrive: boss.is_some_and(|b| b.active && b.phase == BossPhase::Overdrive),
        time_trial: run.is_some_and(|r| r.mode.is_time_trial()),
        style_tier: style.and_then(|s| s.rank()).map_or(0, |r| r.index() + 1),
    };
    let dt = time.delta_secs();
    mix.weights = step_music_mix(mix.weights, target_music_weights(cues), dt);
    // The 6G layer swells in a little faster than it fades (rank-ups land hard).
    let target = target_overclock_weight(cues);
    let rate = if target > mix.overclock { MUSIC_MIX_RATE * 1.6 } else { MUSIC_MIX_RATE };
    mix.overclock += (target - mix.overclock) * (1.0 - (-rate * dt.max(0.0)).exp());
}

/// Game audio is suspended because the OS backgrounded the app (see
/// `lifecycle::handle_app_lifecycle`): every sink is paused, new SFX are
/// dropped and music volumes are forced to 0 until the app runs again.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioSuspended(pub bool);

/// Music stem volume under the suspension flag.
pub fn suspended_music_volume(volume: f32, suspended: AudioSuspended) -> f32 {
    if suspended.0 { 0.0 } else { volume }
}

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SfxQueue>()
            .init_resource::<MusicMixState>()
            .init_resource::<AudioSuspended>()
            .add_systems(Update, jammer_hum.run_if(in_state(AppState::Playing)))
            .add_systems(Last, (drain_sfx, update_music_mix));
        #[cfg(any(feature = "audio", target_os = "android"))]
        app.add_systems(Startup, (backend::cache_sounds, backend::spawn_music_stems))
            .add_systems(Last, (backend::attach_spatial_listener.before(drain_sfx), backend::apply_music_volumes.after(update_music_mix)))
            .add_systems(Last, backend::apply_audio_suspension);
    }
}

#[cfg(any(feature = "audio", target_os = "android"))]
mod backend {
    use std::collections::HashMap;

    use bevy::audio::{
        AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, SpatialAudioSink, SpatialListener, SpatialScale, Volume,
    };
    use bevy::prelude::*;

    use super::{
        effective_music_volume, spatial_scale_factor, suspended_music_volume, synthesize_music_wav, synthesize_wav, AudioSuspended, MusicLayer,
        MusicMixState, SfxEvent, SPATIAL_LISTENER_GAP,
    };
    use crate::player::PlayerCamera;

    /// One looping soundtrack stem.
    #[derive(Component, Debug, Clone, Copy)]
    pub struct MusicStem(pub MusicLayer);

    /// Start every stem together (silent) so they stay phase-locked.
    pub fn spawn_music_stems(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        for layer in MusicLayer::STEMS {
            let handle = sources.add(AudioSource { bytes: synthesize_music_wav(layer).into() });
            commands.spawn((AudioPlayer(handle), PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)), MusicStem(layer)));
        }
    }

    /// Push the live mix weights (times master volume / mute) into each stem's sink.
    pub fn apply_music_volumes(
        mix: Res<MusicMixState>,
        store: Option<Res<crate::meta::ProfileStore>>,
        suspended: Res<AudioSuspended>,
        mut sinks: Query<(&MusicStem, &mut AudioSink)>,
    ) {
        let settings = crate::meta::live_settings(store.as_deref());
        for (stem, mut sink) in &mut sinks {
            let v = effective_music_volume(mix.weight(stem.0), &settings);
            sink.set_volume(Volume::Linear(suspended_music_volume(v, *suspended)));
        }
    }

    /// Pause every sink — both stereo (`AudioSink`, incl. phase-locked music
    /// stems) and positional (`SpatialAudioSink`) — while the app is suspended,
    /// including any sink created after suspension began. Resume all when it
    /// runs again.
    pub fn apply_audio_suspension(
        suspended: Res<AudioSuspended>,
        sinks: Query<&AudioSink>,
        spatial_sinks: Query<&SpatialAudioSink>,
    ) {
        if suspended.0 {
            for sink in &sinks {
                if !sink.is_paused() {
                    sink.pause();
                }
            }
            for sink in &spatial_sinks {
                if !sink.is_paused() {
                    sink.pause();
                }
            }
        } else if suspended.is_changed() {
            for sink in &sinks {
                sink.play();
            }
            for sink in &spatial_sinks {
                sink.play();
            }
        }
    }

    /// The player's head camera is the spatial listener: panning and rolloff
    /// follow its live position and yaw / pitch (it is respawned per run).
    pub fn attach_spatial_listener(mut commands: Commands, cams: Query<Entity, (With<PlayerCamera>, Without<SpatialListener>)>) {
        for cam in &cams {
            commands.entity(cam).insert(SpatialListener::new(SPATIAL_LISTENER_GAP));
        }
    }

    #[derive(Resource)]
    pub struct SfxHandles(pub HashMap<SfxEvent, Handle<AudioSource>>);

    pub fn cache_sounds(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let map = SfxEvent::ALL
            .into_iter()
            .map(|e| (e, sources.add(AudioSource { bytes: synthesize_wav(e).into() })))
            .collect();
        commands.insert_resource(SfxHandles(map));
    }

    pub fn play(commands: &mut Commands, handles: &SfxHandles, sfx: SfxEvent, volume: f32) {
        if let Some(h) = handles.0.get(&sfx) {
            commands.spawn((AudioPlayer(h.clone()), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume))));
        }
    }

    /// World-positioned one-shot. The `GlobalTransform` is set explicitly so
    /// the emitter is correct even before transform propagation runs.
    pub fn play_at(commands: &mut Commands, handles: &SfxHandles, sfx: SfxEvent, volume: f32, pos: Vec3) {
        if let Some(h) = handles.0.get(&sfx) {
            commands.spawn((
                AudioPlayer(h.clone()),
                PlaybackSettings::DESPAWN
                    .with_spatial(true)
                    .with_spatial_scale(SpatialScale::new(spatial_scale_factor()))
                    .with_volume(Volume::Linear(volume)),
                Transform::from_translation(pos),
                GlobalTransform::from_translation(pos),
            ));
        }
    }
}

/// Final playback volume of `sfx` under the user's audio settings
/// (0 when muted, else the sound's own mix level times the master volume).
pub fn effective_sfx_volume(sfx: SfxEvent, settings: &UserSettings) -> f32 {
    if settings.audio_muted {
        return 0.0;
    }
    let master = if settings.audio_master_volume.is_finite() { settings.audio_master_volume.clamp(0.0, 1.0) } else { 1.0 };
    sfx.volume() * master
}

#[cfg(any(feature = "audio", target_os = "android"))]
fn drain_sfx(
    mut commands: Commands,
    mut queue: ResMut<SfxQueue>,
    handles: Option<Res<backend::SfxHandles>>,
    store: Option<Res<crate::meta::ProfileStore>>,
    listeners: Query<&GlobalTransform, With<bevy::audio::SpatialListener>>,
    suspended: Res<AudioSuspended>,
    mut picked: Local<Vec<SfxRequest>>,
) {
    if suspended.0 {
        queue.0.clear();
        return;
    }
    let settings = crate::meta::live_settings(store.as_deref());
    let listener = listeners.iter().next().map(|g| g.translation());
    dedup_sfx_requests(&queue.0, listener, &mut picked);
    queue.0.clear();
    let Some(h) = &handles else { return };
    for r in picked.drain(..) {
        let volume = effective_sfx_volume(r.event, &settings);
        match (r.pos, listener) {
            // Positional: rodio applies the rolloff / panning; we add the edge
            // fade and cull anything past `SPATIAL_MAX_DIST`.
            (Some(pos), Some(ear)) => {
                let pos = emitter_position(r.event, ear, pos);
                let v = volume * spatial_distance_fade(pos.distance(ear), SPATIAL_MAX_DIST);
                if v > 0.0 {
                    backend::play_at(&mut commands, h, r.event, v, pos);
                }
            }
            // 2D, or no listener yet (menus): plain stereo playback.
            _ => {
                if volume > 0.0 {
                    backend::play(&mut commands, h, r.event, volume);
                }
            }
        }
    }
}

#[cfg(not(any(feature = "audio", target_os = "android")))]
fn drain_sfx(mut queue: ResMut<SfxQueue>) {
    queue.0.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }
    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    #[test]
    fn master_volume_and_mute_scale_every_sfx() {
        let d = UserSettings::default();
        for sfx in SfxEvent::ALL {
            assert_eq!(effective_sfx_volume(sfx, &d), sfx.volume());
            let half = UserSettings { audio_master_volume: 0.5, ..d.clone() };
            assert!((effective_sfx_volume(sfx, &half) - 0.5 * sfx.volume()).abs() < 1e-6);
            let muted = UserSettings { audio_muted: true, ..d.clone() };
            assert_eq!(effective_sfx_volume(sfx, &muted), 0.0);
            let loud = UserSettings { audio_master_volume: 7.0, ..d.clone() };
            assert_eq!(effective_sfx_volume(sfx, &loud), sfx.volume(), "master clamps at 1.0");
            let nan = UserSettings { audio_master_volume: f32::NAN, ..d.clone() };
            assert_eq!(effective_sfx_volume(sfx, &nan), sfx.volume());
        }
    }

    #[test]
    fn every_sfx_is_a_valid_wav() {
        for sfx in SfxEvent::ALL {
            let w = synthesize_wav(sfx);
            assert!(w.len() > 44 + 1000, "{sfx:?} too short");
            assert_eq!(&w[0..4], b"RIFF");
            assert_eq!(&w[8..12], b"WAVE");
            assert_eq!(&w[12..16], b"fmt ");
            assert_eq!(u16_at(&w, 20), 1, "PCM");
            assert_eq!(u16_at(&w, 22), 1, "mono");
            assert_eq!(u32_at(&w, 24), SAMPLE_RATE);
            assert_eq!(u16_at(&w, 34), 16);
            assert_eq!(&w[36..40], b"data");
            let data = u32_at(&w, 40) as usize;
            assert_eq!(w.len(), 44 + data);
            assert_eq!(u32_at(&w, 4) as usize, w.len() - 8);
            // Not silent, never clipped beyond the normalized peak.
            let samples: Vec<i16> = w[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak > 8000, "{sfx:?} silent (peak {peak})");
            assert!(peak <= (0.91 * i16::MAX as f32) as u16);
        }
    }

    #[test]
    fn synthesis_is_deterministic_and_sfx_differ() {
        assert_eq!(synthesize_wav(SfxEvent::Shotgun), synthesize_wav(SfxEvent::Shotgun));
        assert_ne!(synthesize_wav(SfxEvent::Shotgun), synthesize_wav(SfxEvent::Plasma));
        assert_ne!(synthesize_wav(SfxEvent::Jump), synthesize_wav(SfxEvent::Bhop));
    }

    #[test]
    fn rocket_sfx_are_distinct_and_deterministic() {
        assert_eq!(SfxEvent::ALL.len(), 44);
        let fire = synthesize_wav(SfxEvent::RocketFire);
        let boom = synthesize_wav(SfxEvent::RocketExplode);
        assert_eq!(fire, synthesize_wav(SfxEvent::RocketFire));
        assert_eq!(boom, synthesize_wav(SfxEvent::RocketExplode));
        assert_ne!(fire, boom);
        assert_ne!(fire, synthesize_wav(SfxEvent::Shotgun));
        // The explosion is the long, heavy one.
        assert!(boom.len() > fire.len());
        assert!(boom.len() > synthesize_wav(SfxEvent::Shotgun).len());
    }

    #[test]
    fn milestone13_sfx_are_valid_distinct_and_deterministic() {
        assert_eq!(SfxEvent::ALL.len(), 44);
        let news = [SfxEvent::PhaseBlink, SfxEvent::SniperLock, SfxEvent::BossShield];
        for sfx in news {
            assert!(SfxEvent::ALL.contains(&sfx));
            let w = synthesize_wav(sfx);
            assert_eq!(w, synthesize_wav(sfx), "{sfx:?} deterministic");
            assert_eq!(&w[0..4], b"RIFF");
            assert!(w.len() > 44 + 1000);
            for other in SfxEvent::ALL.into_iter().filter(|o| *o != sfx) {
                assert_ne!(w, synthesize_wav(other), "{sfx:?} vs {other:?}");
            }
        }
    }

    #[test]
    fn milestone27_ui_and_pit_rescue_sfx_are_distinct() {
        for sfx in [SfxEvent::PitRescue, SfxEvent::UiTick, SfxEvent::UiConfirm] {
            assert!(SfxEvent::ALL.contains(&sfx));
            let w = synthesize_wav(sfx);
            assert_eq!(w, synthesize_wav(sfx), "{sfx:?} deterministic");
            for other in SfxEvent::ALL.into_iter().filter(|o| *o != sfx) {
                assert_ne!(w, synthesize_wav(other), "{sfx:?} vs {other:?}");
            }
        }
        // The pit rescue chime rises (positive zero-crossing rate grows), unlike the flat Denied buzz.
        let pcm: Vec<f32> = synth_samples(SfxEvent::PitRescue);
        let crossings = |s: &[f32]| s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let q = pcm.len() / 4;
        assert!(crossings(&pcm[3 * q..]) > crossings(&pcm[..q]), "upward arpeggio");
        // UI cues are short and quiet next to gameplay sounds.
        assert!(synth_samples(SfxEvent::UiTick).len() < synth_samples(SfxEvent::UiConfirm).len());
        assert!(SfxEvent::UiTick.volume() < SfxEvent::UiConfirm.volume() && SfxEvent::UiConfirm.volume() < SfxEvent::Pickup.volume());
    }

    #[test]
    fn ui_sfx_gate_debounces_ticks_and_confirms() {
        let mut g = UiSfxGate::default();
        assert!(g.allow(SfxEvent::UiTick, 1.0));
        assert!(!g.allow(SfxEvent::UiTick, 1.03), "mashed tick is swallowed");
        assert!(g.allow(SfxEvent::UiTick, 1.0 + UI_TICK_MIN_GAP + 0.01));
        // A held direction (0.22 s repeat) ticks on every step, never faster.
        let mut n = 0;
        let mut g = UiSfxGate::default();
        for i in 0..100 {
            n += g.allow(SfxEvent::UiTick, i as f32 * 0.01) as u32;
        }
        assert!(n <= 1 + (1.0 / UI_TICK_MIN_GAP) as u32, "{n}");
        assert!(g.allow(SfxEvent::UiConfirm, 5.0));
        assert!(!g.allow(SfxEvent::UiConfirm, 5.05));
        assert!(!g.allow(SfxEvent::UiTick, 5.0), "confirm swallows a same-instant tick");
        assert!(g.allow(SfxEvent::Shotgun, 5.0), "gameplay sounds are never gated");
        let mut q = SfxQueue::default();
        g.play(&mut q, SfxEvent::UiConfirm, 9.0);
        g.play(&mut q, SfxEvent::UiConfirm, 9.01);
        assert_eq!(q.events().collect::<Vec<_>>(), vec![SfxEvent::UiConfirm]);
    }

    #[test]
    fn jump_pad_sfx_is_distinct_and_deterministic() {
        let pad = synthesize_wav(SfxEvent::JumpPad);
        assert_eq!(pad, synthesize_wav(SfxEvent::JumpPad));
        assert!(SfxEvent::ALL.contains(&SfxEvent::JumpPad));
        for other in [SfxEvent::Jump, SfxEvent::Bhop, SfxEvent::Dash, SfxEvent::RocketFire] {
            assert_ne!(pad, synthesize_wav(other), "{other:?}");
        }
    }

    fn music_pcm(layer: MusicLayer) -> Vec<i16> {
        let w = synthesize_music_wav(layer);
        w[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }

    #[test]
    fn music_layers_are_valid_phase_locked_normalized_wavs() {
        assert_eq!(MusicLayer::ALL.len(), 4);
        for (i, layer) in MusicLayer::ALL.into_iter().enumerate() {
            assert_eq!(layer.index(), i);
        }
        // 4 bars at 132 BPM ~= 7.27 s.
        let secs = MUSIC_LOOP_SAMPLES as f32 / SAMPLE_RATE as f32;
        assert!((secs - 16.0 * 60.0 / MUSIC_BPM).abs() < 0.01, "{secs}");
        for layer in MusicLayer::ALL {
            let w = synthesize_music_wav(layer);
            assert_eq!(&w[0..4], b"RIFF");
            assert_eq!(&w[8..12], b"WAVE");
            assert_eq!(&w[12..16], b"fmt ");
            assert_eq!(u16_at(&w, 20), 1, "PCM");
            assert_eq!(u16_at(&w, 22), 1, "mono");
            assert_eq!(u32_at(&w, 24), SAMPLE_RATE);
            assert_eq!(u16_at(&w, 34), 16);
            assert_eq!(&w[36..40], b"data");
            assert_eq!(u32_at(&w, 40) as usize, MUSIC_LOOP_SAMPLES * 2, "{layer:?}: exact shared length");
            assert_eq!(w.len(), 44 + MUSIC_LOOP_SAMPLES * 2);
            assert_eq!(u32_at(&w, 4) as usize, w.len() - 8);
            let pcm = music_pcm(layer);
            let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak >= (0.84 * i16::MAX as f32) as u16, "{layer:?} normalized to 0.85 (peak {peak})");
            assert!(peak <= (0.86 * i16::MAX as f32) as u16, "{layer:?} never clips (peak {peak})");
            // Not silent: substantial RMS energy throughout.
            let rms = (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
            assert!(rms > 1500.0, "{layer:?} rms {rms}");
            // Every bar carries sound (no dead quarter of the loop).
            for bar in pcm.chunks(MUSIC_LOOP_SAMPLES / 4) {
                assert!(bar.iter().any(|s| s.unsigned_abs() > 4000), "{layer:?} silent bar");
            }
        }
    }

    #[test]
    fn music_layers_loop_seamlessly() {
        for layer in MusicLayer::ALL {
            let s = music_samples(layer);
            assert_eq!(s.len(), MUSIC_LOOP_SAMPLES);
            // The wrap step (last -> first sample) is no bigger than the signal's
            // own typical sample-to-sample motion: no click at the loop point.
            let wrap = (s[0] - s[s.len() - 1]).abs();
            let mut steps: Vec<f32> = s.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
            steps.sort_by(|a, b| a.total_cmp(b));
            let p99 = steps[steps.len() * 99 / 100];
            assert!(wrap <= p99.max(0.02), "{layer:?}: wrap jump {wrap} vs p99 step {p99}");
            // ...and no bigger than the jump at any in-loop step onset (where the
            // same hats/notes retrigger), i.e. the wrap sounds like any other step.
            let onset = (1..MUSIC_STEPS)
                .map(|k| (s[k * MUSIC_STEP_SAMPLES] - s[k * MUSIC_STEP_SAMPLES - 1]).abs())
                .fold(0.0f32, f32::max);
            assert!(wrap <= onset + 1e-3, "{layer:?}: wrap jump {wrap} vs max step onset {onset}");
            // A naive (un-crossfaded) loop would be judged by the same yardstick:
            // the seam really is the continuation of the last sample.
            let raw = music_raw(layer, MUSIC_LOOP_SAMPLES + 1);
            let g = MUSIC_PEAK / raw[..MUSIC_LOOP_SAMPLES].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((s[0] - raw[MUSIC_LOOP_SAMPLES] * g).abs() < 0.05, "{layer:?}: seam continues the tail");
        }
    }

    #[test]
    fn music_is_deterministic_and_layers_differ() {
        let wavs: Vec<Vec<u8>> = MusicLayer::ALL.into_iter().map(synthesize_music_wav).collect();
        for (layer, w) in MusicLayer::ALL.into_iter().zip(&wavs) {
            assert_eq!(*w, synthesize_music_wav(layer), "{layer:?} deterministic");
        }
        for a in 0..wavs.len() {
            for b in a + 1..wavs.len() {
                assert_ne!(wavs[a], wavs[b], "{a} vs {b}");
            }
        }
        // Character: the drone is the darkest (least sample-to-sample motion),
        // the rush stem the busiest.
        let motion = |l: MusicLayer| {
            let s = music_samples(l);
            s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / s.len() as f32
        };
        let (d, p, r, b) = (motion(MusicLayer::DeadZoneDrone), motion(MusicLayer::SignalPulse), motion(MusicLayer::RushMetal), motion(MusicLayer::BossOverdrive));
        assert!(d < p && d < r && d < b, "drone {d} pulse {p} rush {r} boss {b}");
    }

    fn cues(state: AppState, bars: u8) -> MusicCues {
        MusicCues { state, bars, ..default() }
    }

    #[test]
    fn target_music_weights_cover_every_state_and_situation() {
        use AppState::*;
        let all_on = |state| MusicCues { state, bars: 5, uploading: true, boss_active: true, boss_overdrive: true, time_trial: true, style_tier: 7 };
        for st in [MainMenu, Loadout, ContractsFeats, Settings] {
            assert_eq!(target_music_weights(all_on(st)), [0.45, 0.15, 0.0, 0.0], "{st:?}");
        }
        assert_eq!(target_music_weights(all_on(Paused)), [0.25, 0.0, 0.0, 0.0]);
        assert_eq!(target_music_weights(cues(UpgradeSelect, 0)), [0.15, 0.55, 0.0, 0.0]);
        assert_eq!(target_music_weights(cues(Victory, 0)), [0.15, 0.55, 0.0, 0.0]);
        assert_eq!(target_music_weights(all_on(GameOver)), [0.50, 0.0, 0.0, 0.0]);
        for bars in 0..=1 {
            assert_eq!(target_music_weights(cues(Playing, bars)), [0.95, 0.05, 0.0, 0.0], "{bars} bars");
        }
        for bars in 2..=3 {
            assert_eq!(target_music_weights(cues(Playing, bars)), [0.45, 0.75, 0.0, 0.0], "{bars} bars");
        }
        for bars in 4..=5 {
            assert_eq!(target_music_weights(cues(Playing, bars)), [0.20, 0.95, 0.25, 0.0], "{bars} bars");
        }
        for bars in 0..=5 {
            let sos = MusicCues { uploading: true, ..cues(Playing, bars) };
            assert_eq!(target_music_weights(sos), [0.15, 0.35, 1.0, 0.0], "SOS at {bars}");
            // Carrier Daemon: the Overdrive stem joins at 0.60, peaks at 0.68 in Overdrive
            // (keeping 4-stem stacked headroom clean under -1 dBFS at full master).
            let boss = MusicCues { boss_active: true, ..cues(Playing, bars) };
            assert_eq!(target_music_weights(boss), [0.10, 0.22, 0.72, 0.60], "boss at {bars}");
            let od = MusicCues { boss_overdrive: true, ..boss };
            assert_eq!(target_music_weights(od), [0.04, 0.14, 0.62, 0.68], "overdrive at {bars}");
            assert_eq!(target_music_weights(MusicCues { uploading: true, ..od }), [0.04, 0.14, 0.62, 0.68], "boss wins over SOS");
            // A stale overdrive flag without a live boss is ignored.
            assert_eq!(target_music_weights(MusicCues { boss_overdrive: true, ..cues(Playing, bars) })[3], 0.0);
            // Time trial: energetic pulse + rush instead of the dead-zone drone.
            let tt = MusicCues { time_trial: true, ..cues(Playing, bars) };
            assert_eq!(target_music_weights(tt), [0.0, 0.85, 0.70, 0.0], "time trial at {bars}");
        }
        // Only the boss ever brings in the Overdrive stem.
        for st in [MainMenu, Loadout, ContractsFeats, Settings, Paused, UpgradeSelect, Victory, GameOver] {
            assert_eq!(target_music_weights(MusicCues { boss_active: true, boss_overdrive: true, ..cues(st, 3) })[3], 0.0, "{st:?}");
        }
        assert_eq!(MusicMixState::default().weights, [0.45, 0.15, 0.0, 0.0]);
    }

    #[test]
    fn music_mix_crossfades_smoothly_and_converges() {
        let from = [0.95, 0.05, 0.0, 0.0];
        let to = [0.15, 0.35, 1.0, 0.75];
        assert_eq!(step_music_mix(from, to, 0.0), from);
        let mut w = from;
        let mut prev_rush = 0.0;
        for _ in 0..60 {
            let next = step_music_mix(w, to, 1.0 / 60.0);
            // Monotonic, bounded, and no single frame jumps more than 6%.
            assert!(next[2] >= prev_rush && next[2] <= 1.0);
            assert!(next[0] <= w[0] && next[0] >= 0.15);
            assert!((next[2] - w[2]).abs() < 0.06);
            prev_rush = next[2];
            w = next;
        }
        // One second at rate 3/s covers 1 - e^-3 (~95%) of the distance.
        assert!((w[2] - (1.0 - (-3.0f32).exp())).abs() < 1e-3, "{w:?}");
        // Frame-rate independent.
        let mut coarse = from;
        for _ in 0..10 {
            coarse = step_music_mix(coarse, to, 0.1);
        }
        for i in 0..MUSIC_LAYERS {
            assert!((coarse[i] - w[i]).abs() < 1e-4);
        }
        for _ in 0..600 {
            w = step_music_mix(w, to, 1.0 / 60.0);
        }
        for i in 0..MUSIC_LAYERS {
            assert!((w[i] - to[i]).abs() < 1e-4, "{w:?}");
        }
    }

    #[test]
    fn music_volume_scales_with_master_and_mutes() {
        let d = UserSettings::default();
        assert!((effective_music_volume(1.0, &d) - MUSIC_BASE_VOLUME * d.audio_master_volume.clamp(0.0, 1.0)).abs() < 1e-6);
        let full = UserSettings { audio_master_volume: 1.0, ..d.clone() };
        assert!((effective_music_volume(0.5, &full) - 0.5 * MUSIC_BASE_VOLUME).abs() < 1e-6);
        let half = UserSettings { audio_master_volume: 0.5, ..d.clone() };
        assert!((effective_music_volume(1.0, &half) - 0.5 * MUSIC_BASE_VOLUME).abs() < 1e-6);
        assert_eq!(effective_music_volume(3.0, &full), MUSIC_BASE_VOLUME, "weight clamps at 1");
        assert_eq!(effective_music_volume(-1.0, &full), 0.0, "weight clamps at 0");
        let loud = UserSettings { audio_master_volume: 9.0, ..d.clone() };
        assert_eq!(effective_music_volume(1.0, &loud), MUSIC_BASE_VOLUME, "master clamps at 1");
        let muted = UserSettings { audio_muted: true, ..full };
        assert_eq!(effective_music_volume(1.0, &muted), 0.0);
    }

    #[test]
    fn milestone20_sfx_are_valid_distinct_short_and_deterministic() {
        let news = [SfxEvent::StepRegolith, SfxEvent::StepMetal, SfxEvent::LandThud, SfxEvent::ShellClink, SfxEvent::RocketWhoosh, SfxEvent::JammerHum];
        for sfx in news {
            assert!(SfxEvent::ALL.contains(&sfx));
            let w = synthesize_wav(sfx);
            assert_eq!(w, synthesize_wav(sfx), "{sfx:?} deterministic");
            for other in SfxEvent::ALL.into_iter().filter(|o| *o != sfx) {
                assert_ne!(w, synthesize_wav(other), "{sfx:?} vs {other:?}");
            }
            let raw = synth_samples(sfx);
            assert!(raw.iter().all(|v| v.is_finite()), "{sfx:?} finite");
            assert!(raw.iter().any(|v| v.abs() > 1e-3), "{sfx:?} non-silent");
            // RMS energy: a real sound, not a lone click.
            let pcm: Vec<i16> = w[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let rms = (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
            assert!(rms > 1200.0, "{sfx:?} rms {rms}");
        }
        // Footsteps and clinks are short and quiet (they repeat a lot).
        let secs = |e| (synthesize_wav(e).len() - 44) as f32 / 2.0 / SAMPLE_RATE as f32;
        for e in [SfxEvent::StepRegolith, SfxEvent::StepMetal, SfxEvent::ShellClink] {
            assert!(secs(e) < 0.3, "{e:?} {}", secs(e));
            assert!(e.volume() < 0.4, "{e:?}");
        }
        assert!(secs(SfxEvent::JammerHum) < JAMMER_HUM_INTERVAL, "hum never overlaps itself");
        // Distinct timbres: the metal step rings longer than the regolith crunch,
        // the clink is the brightest (most sample-to-sample motion).
        assert!(secs(SfxEvent::StepMetal) > secs(SfxEvent::StepRegolith));
        let motion = |e| {
            let s = synth_samples(e);
            let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / s.len() as f32 / peak
        };
        assert!(motion(SfxEvent::ShellClink) > motion(SfxEvent::LandThud) * 4.0);
        assert!(motion(SfxEvent::ShellClink) > motion(SfxEvent::StepMetal));
    }

    #[test]
    fn every_sfx_sample_is_finite_and_never_clips() {
        for sfx in SfxEvent::ALL {
            let raw = synth_samples(sfx);
            assert!(!raw.is_empty() && raw.iter().all(|v| v.is_finite()), "{sfx:?}");
            let w = synthesize_wav(sfx);
            let pcm: Vec<i16> = w[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            assert!(pcm.iter().all(|s| s.unsigned_abs() <= (0.91 * i16::MAX as f32) as u16), "{sfx:?} clipped");
            assert!(pcm.last().unwrap().unsigned_abs() < 64, "{sfx:?} fades out without a click");
        }
    }

    #[test]
    fn boss_overdrive_stem_is_the_busiest_and_driving() {
        let s = music_samples(MusicLayer::BossOverdrive);
        assert_eq!(s.len(), MUSIC_LOOP_SAMPLES);
        assert!(s.iter().all(|v| v.is_finite()));
        let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - MUSIC_PEAK).abs() < 1e-4, "{peak}");
        // Relentless: every 16th step has energy (double kick + gallop, no rests).
        for k in 0..MUSIC_STEPS {
            let step = &s[k * MUSIC_STEP_SAMPLES..(k + 1) * MUSIC_STEP_SAMPLES];
            let rms = (step.iter().map(|v| v * v).sum::<f32>() / step.len() as f32).sqrt();
            assert!(rms > 0.05, "step {k} rms {rms}");
        }
    }

    #[test]
    fn spatial_gain_rolls_off_with_distance_and_fades_to_zero() {
        let (l, right) = (Vec3::new(3.0, 1.6, -2.0), Vec3::X);
        let gain = |d: f32| spatial_gain_and_pan(l, right, l + Vec3::new(0.0, 0.0, -d), SPATIAL_MAX_DIST).0;
        assert_eq!(gain(0.0), 1.0);
        assert_eq!(gain(SPATIAL_REF_DIST), 1.0);
        assert!((gain(20.0) - 0.25).abs() < 1e-4, "inverse square past the reference");
        // Monotonic falloff: 10-25 m clearly audible, > 35 m quiet, culled at max.
        let mut prev = 1.0;
        for d in (1..120).map(|k| k as f32 * 0.5) {
            let g = gain(d);
            assert!((0.0..=1.0).contains(&g) && g <= prev + 1e-6, "{d}: {g} after {prev}");
            prev = g;
        }
        assert!(gain(25.0) > 0.12);
        assert!(gain(36.0) < 0.08);
        assert_eq!(gain(SPATIAL_MAX_DIST), 0.0);
        assert_eq!(gain(SPATIAL_MAX_DIST + 10.0), 0.0);
        // Rodio's sink applies the same law with the configured scale.
        let scaled = |d: f32| (1.0 / (d * spatial_scale_factor()).powi(2)).min(1.0);
        for d in [5.0, 12.0, 18.0, 30.0] {
            assert!((spatial_rolloff(d) - scaled(d)).abs() < 1e-5, "{d}");
        }
        // Edge fade is smooth: no step at its start.
        assert!((spatial_distance_fade(SPATIAL_MAX_DIST * 0.6, SPATIAL_MAX_DIST) - 1.0).abs() < 1e-6);
        assert!(spatial_distance_fade(SPATIAL_MAX_DIST * 0.6 + 0.1, SPATIAL_MAX_DIST) > 0.99);
        // Degenerate inputs never produce NaN.
        assert_eq!(spatial_gain_and_pan(l, right, Vec3::NAN, 50.0), (0.0, 0.0));
        assert_eq!(spatial_gain_and_pan(l, right, l + Vec3::X, 0.0).0, 0.0);
        assert_eq!(spatial_gain_and_pan(l, Vec3::ZERO, l + Vec3::X * 3.0, 50.0).1, 0.0);
    }

    #[test]
    fn spatial_pan_tracks_listener_yaw() {
        let l = Vec3::new(10.0, 1.6, 10.0);
        let pan = |right: Vec3, src: Vec3| spatial_gain_and_pan(l, right, src, SPATIAL_MAX_DIST).1;
        // Facing -Z (right = +X): east is hard right, west hard left, ahead / above centred.
        assert!((pan(Vec3::X, l + Vec3::X * 6.0) - 1.0).abs() < 1e-5);
        assert!((pan(Vec3::X, l - Vec3::X * 6.0) + 1.0).abs() < 1e-5);
        assert!(pan(Vec3::X, l - Vec3::Z * 6.0).abs() < 1e-5);
        assert!(pan(Vec3::X, l + Vec3::Y * 6.0).abs() < 1e-5);
        assert_eq!(pan(Vec3::X, l), 0.0);
        // Turn 90 deg left (yaw +90: right becomes -Z): the east source is now ahead,
        // a source to the north is on the right.
        let right = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2) * Vec3::X;
        assert!(pan(right, l + Vec3::X * 6.0).abs() < 1e-5);
        assert!((pan(right, l - Vec3::Z * 6.0) - 1.0).abs() < 1e-5);
        // 45 deg off-axis pans partially; scale-invariant in distance.
        let p = pan(Vec3::X, l + Vec3::new(4.0, 0.0, -4.0));
        assert!((p - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        assert!((pan(Vec3::X, l + Vec3::new(20.0, 0.0, -20.0)) - p).abs() < 1e-5);
    }

    #[test]
    fn queue_records_2d_and_positional_requests() {
        let mut q = SfxQueue::default();
        q.play(SfxEvent::Shotgun);
        q.play_at(SfxEvent::EnemyFire, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(q.0, vec![SfxRequest { event: SfxEvent::Shotgun, pos: None }, SfxRequest { event: SfxEvent::EnemyFire, pos: Some(Vec3::new(1.0, 2.0, 3.0)) }]);
        assert_eq!(q.events().collect::<Vec<_>>(), vec![SfxEvent::Shotgun, SfxEvent::EnemyFire]);
    }

    #[test]
    fn dedup_keeps_one_instance_per_event_preferring_2d_then_nearest() {
        let l = Vec3::new(0.0, 1.6, 0.0);
        let at = |e, x: f32| SfxRequest { event: e, pos: Some(Vec3::new(x, 0.0, 0.0)) };
        let flat = |e| SfxRequest { event: e, pos: None };
        let mut out = Vec::new();
        // 8 simultaneous enemy shots at various ranges -> only the nearest plays.
        let shots: Vec<_> = [30.0, 12.0, -4.0, 22.0, 7.0, -40.0, 15.0, 9.0].into_iter().map(|x| at(SfxEvent::EnemyFire, x)).collect();
        dedup_sfx_requests(&shots, Some(l), &mut out);
        assert_eq!(out, vec![at(SfxEvent::EnemyFire, -4.0)]);
        // A 2D instance (e.g. the player's own) wins over any positional one.
        let mixed = [at(SfxEvent::RocketExplode, 2.0), flat(SfxEvent::RocketExplode), at(SfxEvent::RocketExplode, 1.0)];
        dedup_sfx_requests(&mixed, Some(l), &mut out);
        assert_eq!(out, vec![flat(SfxEvent::RocketExplode)]);
        // Distinct events all survive, in first-request order; 8 pellets = 1 hit.
        let mut reqs = vec![flat(SfxEvent::Shotgun)];
        reqs.extend(std::iter::repeat_n(flat(SfxEvent::Hit), 8));
        reqs.extend([at(SfxEvent::EnemyDeath, 5.0), at(SfxEvent::ShellClink, 0.5), at(SfxEvent::EnemyDeath, 3.0)]);
        dedup_sfx_requests(&reqs, Some(l), &mut out);
        assert_eq!(out, vec![flat(SfxEvent::Shotgun), flat(SfxEvent::Hit), at(SfxEvent::EnemyDeath, 3.0), at(SfxEvent::ShellClink, 0.5)]);
        // No listener yet: the first positional instance is kept.
        dedup_sfx_requests(&shots, None, &mut out);
        assert_eq!(out, vec![at(SfxEvent::EnemyFire, 30.0)]);
        // The output buffer is reused, empty input -> empty output.
        dedup_sfx_requests(&[], Some(l), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn warnings_keep_direction_but_are_never_out_of_earshot() {
        let l = Vec3::new(5.0, 1.6, 5.0);
        let far = l + Vec3::new(80.0, 0.0, -60.0);
        let p = emitter_position(SfxEvent::BaronAlert, l, far);
        assert!((p.distance(l) - 9.0).abs() < 1e-4);
        assert!((p - l).normalize().dot((far - l).normalize()) > 0.9999, "same direction");
        assert!(spatial_gain_and_pan(l, Vec3::X, p, SPATIAL_MAX_DIST).0 > 0.99);
        // Near warnings and ordinary combat sounds are untouched (and far ones cull).
        let near = l + Vec3::X * 4.0;
        assert_eq!(emitter_position(SfxEvent::BaronAlert, l, near), near);
        assert_eq!(emitter_position(SfxEvent::EnemyFire, l, far), far);
        assert_eq!(spatial_gain_and_pan(l, Vec3::X, far, SPATIAL_MAX_DIST).0, 0.0);
        assert!(emitter_position(SfxEvent::BossShield, l, far).distance(l) <= 12.0 + 1e-4);
    }

    #[test]
    fn nearest_within_picks_the_closest_source_in_range() {
        let p = Vec3::new(10.0, 0.0, 10.0);
        let srcs = [Vec3::new(30.0, 0.0, 10.0), Vec3::new(10.0, 2.0, 22.0), Vec3::new(4.0, 0.0, 10.0), Vec3::NAN];
        assert_eq!(nearest_within(p, srcs, JAMMER_HUM_RADIUS), Some(Vec3::new(4.0, 0.0, 10.0)));
        assert_eq!(nearest_within(p, [Vec3::new(10.0, 2.0, 22.0)], JAMMER_HUM_RADIUS), Some(Vec3::new(10.0, 2.0, 22.0)));
        assert_eq!(nearest_within(p, [Vec3::new(30.0, 0.0, 10.0)], JAMMER_HUM_RADIUS), None, "out of earshot");
        assert_eq!(nearest_within(p, [], JAMMER_HUM_RADIUS), None);
    }

    #[test]
    fn footstep_surface_and_stride_rules() {
        use crate::level::{Cell, RampDir};
        assert_eq!(footstep_sfx(Cell::Floor { height: 0.0 }, 0.0), SfxEvent::StepRegolith);
        assert_eq!(footstep_sfx(Cell::Floor { height: 0.0 }, 0.3), SfxEvent::StepRegolith);
        assert_eq!(footstep_sfx(Cell::Floor { height: 2.4 }, 2.4), SfxEvent::StepMetal, "catwalk deck");
        assert_eq!(footstep_sfx(Cell::Ramp { low_height: 0.0, high_height: 2.4, dir: RampDir::North }, 0.1), SfxEvent::StepMetal, "ramp foot");
        // One step every STEP_STRIDE metres of travel, whatever the frame slicing.
        let mut stride = 0.0;
        let steps = (0..600).filter(|_| advance_stride(&mut stride, 0.12)).count();
        assert!((29..=30).contains(&steps), "{steps}");
        let mut stride = 0.0;
        assert!(!advance_stride(&mut stride, 0.0) && !advance_stride(&mut stride, f32::NAN) && !advance_stride(&mut stride, -3.0));
        assert_eq!(stride, 0.0);
        // A teleport-sized jump yields a single step, never a burst.
        assert!(advance_stride(&mut stride, 50.0));
        assert!(stride < STEP_STRIDE && !advance_stride(&mut stride, 0.1));
        // Casing clinks: only real rebounds from a fast enough fall.
        assert!(is_clink_bounce(-3.0, 1.2));
        assert!(!is_clink_bounce(-0.8, 0.2), "resting jitter is silent");
        assert!(!is_clink_bounce(-3.0, -3.2), "still falling");
    }

    /// Decode a 16-bit mono PCM WAV written by [`encode_wav`] back to `-1..1`.
    fn decode_wav(bytes: &[u8]) -> Vec<f32> {
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[36..40], b"data");
        bytes[44..].as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as f32 / i16::MAX as f32).collect()
    }

    /// Generalized worst-case mix: the Boss Overdrive bed at every loop phase
    /// plus `cues` (`(event, close_spatial)`), landing together and with
    /// 0-20 ms staggers per cue. Returns `(peak, clipped samples)`.
    fn worst_case_mix_of(cues: &[(SfxEvent, bool)]) -> (f32, usize) {
        let full = UserSettings { audio_master_volume: 1.0, audio_muted: false, ..default() };
        let music = MusicCues { state: AppState::Playing, bars: 5, boss_active: true, boss_overdrive: true, ..default() };
        let weights = target_music_weights(music);
        let stems: Vec<(Vec<f32>, f32)> =
            MusicLayer::ALL.iter().map(|l| (decode_wav(&synthesize_music_wav(*l)), effective_music_volume(weights[l.index()], &full))).collect();
        let loop_len = stems[0].0.len();
        let bed: Vec<f32> = (0..loop_len).map(|i| stems.iter().map(|(s, g)| s[i] * g).sum()).collect();
        let bed_hi = bed.iter().copied().fold(0.0f32, f32::max);
        let bed_lo = bed.iter().copied().fold(0.0f32, f32::min);
        let close = spatial_distance_fade(2.0, SPATIAL_MAX_DIST) * spatial_rolloff(2.0);
        let sfx: Vec<(Vec<f32>, f32)> =
            cues.iter().map(|(e, near)| (decode_wav(&synthesize_wav(*e)), effective_sfx_volume(*e, &full) * if *near { close } else { 1.0 })).collect();
        let len = sfx.iter().map(|(s, _)| s.len()).max().unwrap();
        let (mut peak, mut clipped) = (0.0f32, 0usize);
        // Every cue together, then each cue alone shifted by 5, 10 and 20 ms.
        let mut staggers: Vec<Vec<usize>> = vec![vec![0; cues.len()]];
        for k in 0..cues.len() {
            for d in [220usize, 441, 882] {
                let mut st = vec![0; cues.len()];
                st[k] = d;
                staggers.push(st);
            }
        }
        for st in &staggers {
            for i in 0..len + 882 {
                let fx: f32 = sfx.iter().zip(st).map(|((s, g), d)| i.checked_sub(*d).and_then(|j| s.get(j)).map_or(0.0, |x| x * g)).sum();
                let v = (fx + bed_hi).abs().max((fx + bed_lo).abs());
                peak = peak.max(v);
                clipped += (v >= 1.0) as usize;
            }
        }
        (peak, clipped)
    }

    #[test]
    fn milestone30_phone_ping_and_fatality_are_distinct_and_keep_headroom() {
        for sfx in [SfxEvent::PhonePing, SfxEvent::Fatality] {
            assert!(SfxEvent::ALL.contains(&sfx));
            let w = synthesize_wav(sfx);
            assert_eq!(w, synthesize_wav(sfx), "{sfx:?} deterministic");
            for other in SfxEvent::ALL.into_iter().filter(|o| *o != sfx) {
                assert_ne!(w, synthesize_wav(other), "{sfx:?} vs {other:?}");
            }
        }
        // The ping is a restrained short chirp; the fatality a short heavy hit.
        let secs = |e| (synthesize_wav(e).len() - 44) as f32 / 2.0 / SAMPLE_RATE as f32;
        assert!(secs(SfxEvent::PhonePing) < 0.4 && secs(SfxEvent::Fatality) < 0.7);
        assert!(SfxEvent::PhonePing.volume() < SfxEvent::Pickup.volume());
        assert!(SfxEvent::Fatality.volume() <= SfxEvent::RocketExplode.volume());
        // Worst case: a mortar kill fatality right beside a jammer while a
        // notification lands, over the full Boss Overdrive bed.
        let (peak, clipped) = worst_case_mix_of(&[
            (SfxEvent::JammerHum, true),
            (SfxEvent::RocketExplode, false),
            (SfxEvent::Fatality, false),
            (SfxEvent::PhonePing, false),
        ]);
        let dbfs = 20.0 * peak.log10();
        println!("milestone 30 worst-case mix peak {peak:.4} ({dbfs:.2} dBFS), clipped samples {clipped}");
        assert_eq!(clipped, 0);
        assert!(peak <= 0.89, "peak {peak:.4} ({dbfs:.2} dBFS) must stay <= -1 dBFS");
    }

    /// [`worst_case_mix_of`] over an arbitrary music situation, including the
    /// Milestone 31 6G Overclock stem at its target weight for `music`.
    fn worst_case_mix_with(music: MusicCues, cues: &[(SfxEvent, bool)]) -> (f32, usize) {
        let full = UserSettings { audio_master_volume: 1.0, audio_muted: false, ..default() };
        let weights = target_music_weights(music);
        let oc = target_overclock_weight(music);
        let stems: Vec<(Vec<f32>, f32)> = MusicLayer::STEMS
            .iter()
            .map(|l| {
                let w = if *l == MusicLayer::Overclock6G { oc } else { weights[l.index()] };
                (decode_wav(&synthesize_music_wav(*l)), effective_music_volume(w, &full))
            })
            .collect();
        let loop_len = stems[0].0.len();
        assert!(stems.iter().all(|(s, _)| s.len() == loop_len), "phase-locked stems incl. 6G");
        let bed: Vec<f32> = (0..loop_len).map(|i| stems.iter().map(|(s, g)| s[i] * g).sum()).collect();
        let bed_hi = bed.iter().copied().fold(0.0f32, f32::max);
        let bed_lo = bed.iter().copied().fold(0.0f32, f32::min);
        let close = spatial_distance_fade(2.0, SPATIAL_MAX_DIST) * spatial_rolloff(2.0);
        let sfx: Vec<(Vec<f32>, f32)> =
            cues.iter().map(|(e, near)| (decode_wav(&synthesize_wav(*e)), effective_sfx_volume(*e, &full) * if *near { close } else { 1.0 })).collect();
        let len = sfx.iter().map(|(s, _)| s.len()).max().unwrap();
        let (mut peak, mut clipped) = (0.0f32, 0usize);
        let mut staggers: Vec<Vec<usize>> = vec![vec![0; cues.len()]];
        for k in 0..cues.len() {
            for d in [220usize, 441, 882, 2646] {
                let mut st = vec![0; cues.len()];
                st[k] = d;
                staggers.push(st);
            }
        }
        for st in &staggers {
            for i in 0..len + 2646 {
                let fx: f32 = sfx.iter().zip(st).map(|((s, g), d)| i.checked_sub(*d).and_then(|j| s.get(j)).map_or(0.0, |x| x * g)).sum();
                let v = (fx + bed_hi).abs().max((fx + bed_lo).abs());
                peak = peak.max(v);
                clipped += (v >= 1.0) as usize;
            }
        }
        (peak, clipped)
    }

    #[test]
    fn milestone31_sfx_are_valid_distinct_and_crisp() {
        let news = [SfxEvent::StyleRankUp, SfxEvent::StylePeak, SfxEvent::CrtShatter, SfxEvent::UpsDetonate, SfxEvent::UpsHiss];
        let secs = |e| (synthesize_wav(e).len() - 44) as f32 / 2.0 / SAMPLE_RATE as f32;
        for sfx in news {
            assert!(SfxEvent::ALL.contains(&sfx));
            let w = synthesize_wav(sfx);
            assert_eq!(w, synthesize_wav(sfx), "{sfx:?} deterministic");
            for other in SfxEvent::ALL.into_iter().filter(|o| *o != sfx) {
                assert_ne!(w, synthesize_wav(other), "{sfx:?} vs {other:?}");
            }
            let pcm: Vec<i16> = w[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let rms = (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
            assert!(rms > 1000.0, "{sfx:?} rms {rms}");
            assert!(secs(sfx) < 1.0, "{sfx:?} short");
            assert!(sfx.volume() <= SfxEvent::RocketExplode.volume(), "{sfx:?} sits at or under the mortar");
        }
        // The peak stinger is the longer, brighter one; the UPS blast the heaviest.
        assert!(secs(SfxEvent::StylePeak) > secs(SfxEvent::StyleRankUp));
        assert!(SfxEvent::StylePeak.volume() > SfxEvent::StyleRankUp.volume());
        assert!(secs(SfxEvent::UpsDetonate) > secs(SfxEvent::RocketExplode) * 0.5);
        assert!(SfxEvent::UpsHiss.volume() < SfxEvent::CrtShatter.volume());
    }

    #[test]
    fn milestone33_phone_ability_sfx_are_valid_distinct_and_keep_headroom() {
        let news = [SfxEvent::PhonePunch, SfxEvent::PhoneParry, SfxEvent::CameraFlash];
        let secs = |e| (synthesize_wav(e).len() - 44) as f32 / 2.0 / SAMPLE_RATE as f32;
        assert_eq!(SfxEvent::ALL.len(), 44);
        for sfx in news {
            assert!(SfxEvent::ALL.contains(&sfx));
            let w = synthesize_wav(sfx);
            assert_eq!(w, synthesize_wav(sfx), "{sfx:?} deterministic");
            for other in SfxEvent::ALL.into_iter().filter(|o| *o != sfx) {
                assert_ne!(w, synthesize_wav(other), "{sfx:?} vs {other:?}");
            }
            let pcm: Vec<i16> = w[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let rms = (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
            assert!(rms > 800.0, "{sfx:?} rms {rms}");
            assert!(secs(sfx) < 0.6, "{sfx:?} snappy");
            assert!(sfx.volume() <= SfxEvent::RocketExplode.volume(), "{sfx:?} sits under the mortar");
        }
        // The punch is the snappiest; the parry rings longest.
        assert!(secs(SfxEvent::PhonePunch) < secs(SfxEvent::CameraFlash));
        assert!(secs(SfxEvent::PhoneParry) > secs(SfxEvent::PhonePunch));
        // Worst case: a parry right as a punch lands, a flash pop and a mortar
        // blast, over the boss overdrive bed + 6G+ layer.
        let boss = MusicCues { state: AppState::Playing, bars: 5, boss_active: true, boss_overdrive: true, style_tier: 7, ..default() };
        let cues = [
            (SfxEvent::PhoneParry, false),
            (SfxEvent::PhonePunch, false),
            (SfxEvent::CameraFlash, false),
            (SfxEvent::RocketExplode, false),
            (SfxEvent::JammerHum, true),
        ];
        let (peak, clipped) = worst_case_mix_with(boss, &cues);
        let dbfs = 20.0 * peak.log10();
        println!("milestone 33 worst-case mix: peak {peak:.4} ({dbfs:.2} dBFS), clipped samples {clipped}");
        assert_eq!(clipped, 0);
        assert!(peak <= 0.89, "peak {peak:.4} ({dbfs:.2} dBFS) must stay <= -1 dBFS");
    }

    #[test]
    fn overclock_6g_layer_joins_at_fiber_and_peaks_at_6g() {
        let s = music_samples(MusicLayer::Overclock6G);
        assert_eq!(s.len(), MUSIC_LOOP_SAMPLES, "phase-locked with the base stems");
        assert!(s.iter().all(|v| v.is_finite()));
        assert!((s.iter().fold(0.0f32, |m, v| m.max(v.abs())) - MUSIC_PEAK).abs() < 1e-4);
        assert_eq!(MusicLayer::STEMS.len(), 5);
        assert_eq!(MusicLayer::Overclock6G.index(), 4);
        assert!(!MusicLayer::ALL.contains(&MusicLayer::Overclock6G), "not a state-mixed base stem");
        for base in MusicLayer::ALL {
            assert_ne!(synthesize_music_wav(base), synthesize_music_wav(MusicLayer::Overclock6G));
        }
        // Busier than the drone, every bar carries energy.
        let motion = |l: MusicLayer| {
            let s = music_samples(l);
            s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / s.len() as f32
        };
        assert!(motion(MusicLayer::Overclock6G) > motion(MusicLayer::DeadZoneDrone));
        let play = |tier| MusicCues { state: AppState::Playing, bars: 5, style_tier: tier, ..default() };
        for tier in 0..5 {
            assert_eq!(target_overclock_weight(play(tier)), 0.0, "tier {tier}: below S");
        }
        let (s5, s6, s7) = (target_overclock_weight(play(5)), target_overclock_weight(play(6)), target_overclock_weight(play(7)));
        assert!(s5 > 0.0 && s6 > s5 && s7 >= s6, "{s5} {s6} {s7}");
        assert_eq!(target_overclock_weight(MusicCues { time_trial: true, ..play(7) }), 0.0);
        assert!(target_overclock_weight(MusicCues { boss_active: true, ..play(7) }) < s7, "tamed under the boss bed");
        for st in [AppState::Paused, AppState::MainMenu, AppState::GameOver, AppState::Victory, AppState::UpgradeSelect] {
            assert_eq!(target_overclock_weight(MusicCues { state: st, ..play(7) }), 0.0, "{st:?}");
        }
        let mix = MusicMixState { overclock: 0.5, ..default() };
        assert_eq!(mix.weight(MusicLayer::Overclock6G), 0.5);
        assert_eq!(mix.weight(MusicLayer::DeadZoneDrone), mix.weights[0]);
    }

    #[test]
    fn milestone31_worst_case_mix_keeps_one_db_of_headroom() {
        // Boss Overdrive bed + 6G+ layer: a UPS chain right beside the player,
        // a mortar blast, the SSS peak stinger, a CRT shatter and a fatality.
        let cues = [
            (SfxEvent::UpsDetonate, false),
            (SfxEvent::RocketExplode, false),
            (SfxEvent::StylePeak, false),
            (SfxEvent::CrtShatter, false),
            (SfxEvent::Fatality, false),
            (SfxEvent::JammerHum, true),
        ];
        let boss = MusicCues { state: AppState::Playing, bars: 5, boss_active: true, boss_overdrive: true, style_tier: 7, ..default() };
        let sos = MusicCues { state: AppState::Playing, bars: 5, uploading: true, style_tier: 7, ..default() };
        let roam = MusicCues { state: AppState::Playing, bars: 5, style_tier: 7, ..default() };
        let mut worst = 0.0f32;
        for (name, music) in [("boss overdrive", boss), ("SOS rush", sos), ("full bars", roam)] {
            let (peak, clipped) = worst_case_mix_with(music, &cues);
            let dbfs = 20.0 * peak.log10();
            println!("milestone 31 worst-case mix ({name} + 6G+): peak {peak:.4} ({dbfs:.2} dBFS), clipped samples {clipped}");
            assert_eq!(clipped, 0, "{name}");
            assert!(peak <= 0.89, "{name}: peak {peak:.4} ({dbfs:.2} dBFS) must stay <= -1 dBFS");
            worst = worst.max(peak);
        }
        println!("milestone 31 worst-case overall peak {worst:.4} ({:.2} dBFS)", 20.0 * worst.log10());
    }

    /// Worst-case simultaneous combat mix at full master volume: the four
    /// Boss Overdrive stems at their mix weights, a close Jammer hum, a
    /// Signal-Mortar detonation (`RocketExplode`) and an enemy shot, all
    /// decoded back from their WAV bytes. The SFX land together and with a few
    /// staggered offsets; the music bed is checked at EVERY loop phase (each
    /// SFX sample is paired with the bed's hottest same-sign sample, which
    /// upper-bounds any phase alignment). Every source is mono, duplicated /
    /// panned to both ears with gain <= 1, so the hottest ear sees at most
    /// this sum. Returns `(peak, clipped samples)`.
    fn worst_case_combat_mix() -> (f32, usize) {
        let full = UserSettings { audio_master_volume: 1.0, audio_muted: false, ..default() };
        let cues = MusicCues { state: AppState::Playing, bars: 5, boss_active: true, boss_overdrive: true, ..default() };
        let weights = target_music_weights(cues);
        let stems: Vec<(Vec<f32>, f32)> =
            MusicLayer::ALL.iter().map(|l| (decode_wav(&synthesize_music_wav(*l)), effective_music_volume(weights[l.index()], &full))).collect();
        let loop_len = stems[0].0.len();
        assert!(stems.iter().all(|(s, _)| s.len() == loop_len), "phase-locked stems");
        let bed: Vec<f32> = (0..loop_len).map(|i| stems.iter().map(|(s, g)| s[i] * g).sum()).collect();
        let bed_hi = bed.iter().copied().fold(0.0f32, f32::max);
        let bed_lo = bed.iter().copied().fold(0.0f32, f32::min);
        let close = spatial_distance_fade(2.0, SPATIAL_MAX_DIST) * spatial_rolloff(2.0);
        let sfx: Vec<(Vec<f32>, f32)> = [(SfxEvent::JammerHum, close), (SfxEvent::RocketExplode, 1.0), (SfxEvent::EnemyFire, 1.0)]
            .iter()
            .map(|(e, g)| (decode_wav(&synthesize_wav(*e)), effective_sfx_volume(*e, &full) * g))
            .collect();
        let len = sfx.iter().map(|(s, _)| s.len()).max().unwrap();
        let (mut peak, mut clipped) = (0.0f32, 0usize);
        // SFX offsets in samples: together, then each shifted by 5-20 ms.
        let staggers: [[usize; 3]; 6] = [[0, 0, 0], [0, 220, 0], [0, 0, 220], [0, 0, 330], [441, 0, 110], [0, 110, 0]];
        for st in &staggers {
            for i in 0..len + 441 {
                let fx: f32 = sfx.iter().zip(st).map(|((s, g), d)| i.checked_sub(*d).and_then(|j| s.get(j)).map_or(0.0, |x| x * g)).sum();
                let v = (fx + bed_hi).abs().max((fx + bed_lo).abs());
                peak = peak.max(v);
                clipped += (v >= 1.0) as usize;
            }
        }
        (peak, clipped)
    }

    #[test]
    fn worst_case_combat_mix_keeps_one_db_of_headroom() {
        let (peak, clipped) = worst_case_combat_mix();
        let dbfs = 20.0 * peak.log10();
        println!("worst-case combat mix peak {peak:.4} ({dbfs:.2} dBFS), clipped samples {clipped}");
        assert_eq!(clipped, 0, "no sample reaches full scale");
        assert!(peak <= 0.89, "peak {peak:.4} ({dbfs:.2} dBFS) must stay <= 0.89 (-1 dBFS)");
        // Every cue keeps a clear level of its own, and telegraphs / shots stay
        // distinct relative to the explosion, the hum and the music bed.
        let full = UserSettings::default();
        let v = |e: SfxEvent| effective_sfx_volume(e, &full);
        for e in [SfxEvent::EnemyFire, SfxEvent::SniperLock, SfxEvent::BaronAlert, SfxEvent::Shotgun, SfxEvent::Plasma, SfxEvent::RocketExplode] {
            assert!(v(e) >= 0.3, "{e:?} stays punchy: {}", v(e));
        }
        assert!(v(SfxEvent::EnemyFire) >= v(SfxEvent::JammerHum), "enemy shots cut through the hum");
        assert!(v(SfxEvent::Shotgun) >= v(SfxEvent::RocketExplode) * 0.8, "player shots stay distinct from explosions");
        assert!(v(SfxEvent::EnemyFire) > MUSIC_BASE_VOLUME, "shots sit above the music bed");
    }
}
