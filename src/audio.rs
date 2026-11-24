//! Procedural retro synth SFX.
//!
//! [`synthesize_wav`] is pure Rust and always compiled (and unit-tested): it
//! renders every [`SfxEvent`] into a 16-bit mono PCM RIFF/WAVE buffer from
//! oscillators, noise and envelopes. Actual playback is compiled only with
//! `--features audio` (needs ALSA on Linux): the WAV buffers become cached
//! `AudioSource` assets and draining [`SfxQueue`] spawns `AudioPlayer`s.
//! Gameplay code only ever pushes into [`SfxQueue`], so it compiles identically
//! with or without the feature.

use std::f32::consts::TAU;

use bevy::prelude::*;

use crate::boss::BossHud;
use crate::extraction::ExtractionState;
use crate::meta::UserSettings;
use crate::signal::SignalState;
use crate::state::AppState;

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
}

impl SfxEvent {
    pub const ALL: [SfxEvent; 25] = [
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
    ];

    /// Playback volume (0..1).
    pub fn volume(self) -> f32 {
        match self {
            SfxEvent::Hit | SfxEvent::EnemyFire | SfxEvent::Pickup | SfxEvent::WeaponSwap => 0.45,
            SfxEvent::Plasma | SfxEvent::Jump | SfxEvent::Bhop => 0.5,
            _ => 0.75,
        }
    }
}

/// Frame queue of requested sounds (drained by the audio backend each frame).
#[derive(Resource, Default, Debug)]
pub struct SfxQueue(pub Vec<SfxEvent>);

impl SfxQueue {
    pub fn play(&mut self, sfx: SfxEvent) {
        self.0.push(sfx);
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
// Adaptive 3-layer industrial-synth soundtrack
// ---------------------------------------------------------------------------

/// Tempo of every music stem.
pub const MUSIC_BPM: f32 = 132.0;
/// 16th-note steps per loop (4 bars of 4/4).
pub const MUSIC_STEPS: usize = 64;
/// Samples per 16th-note step (`SAMPLE_RATE * 60 / BPM / 4`, floored so every
/// step, and therefore every stem, has the exact same integer length).
pub const MUSIC_STEP_SAMPLES: usize = (SAMPLE_RATE as usize * 15 * 10) / (MUSIC_BPM as usize * 10);
/// Exact sample count shared by all three stems (phase-locked crossfades).
pub const MUSIC_LOOP_SAMPLES: usize = MUSIC_STEPS * MUSIC_STEP_SAMPLES;
/// Overlap rendered past the loop end and folded back over the start (20 ms).
const MUSIC_SEAM_SAMPLES: usize = SAMPLE_RATE as usize / 50;
/// Peak level every stem is normalized to.
pub const MUSIC_PEAK: f32 = 0.85;
/// Base mix level of a fully weighted stem (before the master volume).
pub const MUSIC_BASE_VOLUME: f32 = 0.32;
/// Exponential crossfade rate of [`step_music_mix`] (1/s).
pub const MUSIC_MIX_RATE: f32 = 3.0;

/// The three phase-locked music stems mixed live by game state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MusicLayer {
    /// Low industrial sub-bass drone with RF static (Dead Zones, menus).
    DeadZoneDrone,
    /// Driving E-Phrygian 16th bass + clock-tick hats + radar pings (exploring).
    SignalPulse,
    /// Industrial-metal combat stem (SOS Demon Rush, Carrier Daemon).
    RushMetal,
}

impl MusicLayer {
    pub const ALL: [MusicLayer; 3] = [MusicLayer::DeadZoneDrone, MusicLayer::SignalPulse, MusicLayer::RushMetal];

    pub fn index(self) -> usize {
        match self {
            MusicLayer::DeadZoneDrone => 0,
            MusicLayer::SignalPulse => 1,
            MusicLayer::RushMetal => 2,
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

/// Target stem weights `[drone, pulse, rush]` for the current game situation.
pub fn target_music_weights(state: AppState, bars: u8, uploading: bool, boss_active: bool) -> [f32; 3] {
    match state {
        AppState::MainMenu | AppState::Loadout | AppState::ContractsFeats | AppState::Settings => [0.45, 0.15, 0.0],
        AppState::Paused => [0.25, 0.0, 0.0],
        AppState::UpgradeSelect | AppState::Victory => [0.15, 0.55, 0.0],
        AppState::GameOver => [0.50, 0.0, 0.0],
        AppState::Playing if uploading || boss_active => [0.15, 0.35, 1.0],
        AppState::Playing if bars <= 1 => [0.95, 0.05, 0.0],
        AppState::Playing if bars <= 3 => [0.45, 0.75, 0.0],
        AppState::Playing => [0.20, 0.95, 0.25],
    }
}

/// Exponential crossfade of the stem weights toward `target` (frame-rate independent).
pub fn step_music_mix(current: [f32; 3], target: [f32; 3], dt: f32) -> [f32; 3] {
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

/// Live stem weights `[drone, pulse, rush]`, indexed by [`MusicLayer::index`].
#[derive(Resource, Debug, Clone)]
pub struct MusicMixState {
    pub weights: [f32; 3],
}

impl Default for MusicMixState {
    fn default() -> Self {
        MusicMixState { weights: target_music_weights(AppState::MainMenu, 0, false, false) }
    }
}

/// Crossfade the soundtrack toward the current state / signal / SOS / boss target.
pub fn update_music_mix(
    time: Res<Time>,
    state: Option<Res<State<AppState>>>,
    signal: Option<Res<SignalState>>,
    extraction: Option<Res<ExtractionState>>,
    boss: Option<Res<BossHud>>,
    mut mix: ResMut<MusicMixState>,
) {
    let app_state = state.map(|s| *s.get()).unwrap_or_default();
    let bars = signal.map(|s| s.bars).unwrap_or(0);
    let uploading = extraction.is_some_and(|e| matches!(*e, ExtractionState::Uploading { .. }));
    let boss_active = boss.is_some_and(|b| b.active);
    let target = target_music_weights(app_state, bars, uploading, boss_active);
    mix.weights = step_music_mix(mix.weights, target, time.delta_secs());
}

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SfxQueue>()
            .init_resource::<MusicMixState>()
            .add_systems(Last, (drain_sfx, update_music_mix));
        #[cfg(feature = "audio")]
        app.add_systems(Startup, (backend::cache_sounds, backend::spawn_music_stems))
            .add_systems(Last, backend::apply_music_volumes.after(update_music_mix));
    }
}

#[cfg(feature = "audio")]
mod backend {
    use std::collections::HashMap;

    use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
    use bevy::prelude::*;

    use bevy::audio::{AudioSink, AudioSinkPlayback};

    use super::{effective_music_volume, synthesize_music_wav, synthesize_wav, MusicLayer, MusicMixState, SfxEvent};

    /// One looping soundtrack stem.
    #[derive(Component, Debug, Clone, Copy)]
    pub struct MusicStem(pub MusicLayer);

    /// Start all three stems together (silent) so they stay phase-locked.
    pub fn spawn_music_stems(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        for layer in MusicLayer::ALL {
            let handle = sources.add(AudioSource { bytes: synthesize_music_wav(layer).into() });
            commands.spawn((AudioPlayer(handle), PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)), MusicStem(layer)));
        }
    }

    /// Push the live mix weights (times master volume / mute) into each stem's sink.
    pub fn apply_music_volumes(
        mix: Res<MusicMixState>,
        store: Option<Res<crate::meta::ProfileStore>>,
        mut sinks: Query<(&MusicStem, &mut AudioSink)>,
    ) {
        let settings = crate::meta::live_settings(store.as_deref());
        for (stem, mut sink) in &mut sinks {
            sink.set_volume(Volume::Linear(effective_music_volume(mix.weights[stem.0.index()], &settings)));
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

#[cfg(feature = "audio")]
fn drain_sfx(
    mut commands: Commands,
    mut queue: ResMut<SfxQueue>,
    handles: Option<Res<backend::SfxHandles>>,
    store: Option<Res<crate::meta::ProfileStore>>,
) {
    let settings = crate::meta::live_settings(store.as_deref());
    let mut played: Vec<SfxEvent> = Vec::new();
    for sfx in queue.0.drain(..) {
        // One instance of each sound per frame (8 pellets = 1 bang).
        if played.contains(&sfx) {
            continue;
        }
        played.push(sfx);
        let volume = effective_sfx_volume(sfx, &settings);
        if volume > 0.0
            && let Some(h) = &handles
        {
            backend::play(&mut commands, h, sfx, volume);
        }
    }
}

#[cfg(not(feature = "audio"))]
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
        assert_eq!(SfxEvent::ALL.len(), 25);
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
        assert_eq!(SfxEvent::ALL.len(), 25);
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
        assert_eq!(MusicLayer::ALL.len(), 3);
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
        assert_ne!(wavs[0], wavs[1]);
        assert_ne!(wavs[1], wavs[2]);
        assert_ne!(wavs[0], wavs[2]);
        // Character: the drone is the darkest (least sample-to-sample motion),
        // the rush stem the busiest.
        let motion = |l: MusicLayer| {
            let s = music_samples(l);
            s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / s.len() as f32
        };
        let (d, p, r) = (motion(MusicLayer::DeadZoneDrone), motion(MusicLayer::SignalPulse), motion(MusicLayer::RushMetal));
        assert!(d < p && d < r, "drone {d} pulse {p} rush {r}");
    }

    #[test]
    fn target_music_weights_cover_every_state_and_situation() {
        use AppState::*;
        for st in [MainMenu, Loadout, ContractsFeats, Settings] {
            assert_eq!(target_music_weights(st, 5, true, true), [0.45, 0.15, 0.0], "{st:?}");
        }
        assert_eq!(target_music_weights(Paused, 5, true, true), [0.25, 0.0, 0.0]);
        assert_eq!(target_music_weights(UpgradeSelect, 0, false, false), [0.15, 0.55, 0.0]);
        assert_eq!(target_music_weights(Victory, 0, false, false), [0.15, 0.55, 0.0]);
        assert_eq!(target_music_weights(GameOver, 5, true, false), [0.50, 0.0, 0.0]);
        for bars in 0..=1 {
            assert_eq!(target_music_weights(Playing, bars, false, false), [0.95, 0.05, 0.0], "{bars} bars");
        }
        for bars in 2..=3 {
            assert_eq!(target_music_weights(Playing, bars, false, false), [0.45, 0.75, 0.0], "{bars} bars");
        }
        for bars in 4..=5 {
            assert_eq!(target_music_weights(Playing, bars, false, false), [0.20, 0.95, 0.25], "{bars} bars");
        }
        for bars in 0..=5 {
            assert_eq!(target_music_weights(Playing, bars, true, false), [0.15, 0.35, 1.0], "SOS at {bars}");
            assert_eq!(target_music_weights(Playing, bars, false, true), [0.15, 0.35, 1.0], "boss at {bars}");
        }
    }

    #[test]
    fn music_mix_crossfades_smoothly_and_converges() {
        let from = [0.95, 0.05, 0.0];
        let to = [0.15, 0.35, 1.0];
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
        for i in 0..3 {
            assert!((coarse[i] - w[i]).abs() < 1e-4);
        }
        for _ in 0..600 {
            w = step_music_mix(w, to, 1.0 / 60.0);
        }
        for i in 0..3 {
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
}
