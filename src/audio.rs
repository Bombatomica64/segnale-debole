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
}

impl SfxEvent {
    pub const ALL: [SfxEvent; 22] = [
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

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SfxQueue>().add_systems(Last, drain_sfx);
        #[cfg(feature = "audio")]
        app.add_systems(Startup, backend::cache_sounds);
    }
}

#[cfg(feature = "audio")]
mod backend {
    use std::collections::HashMap;

    use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
    use bevy::prelude::*;

    use super::{synthesize_wav, SfxEvent};

    #[derive(Resource)]
    pub struct SfxHandles(pub HashMap<SfxEvent, Handle<AudioSource>>);

    pub fn cache_sounds(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let map = SfxEvent::ALL
            .into_iter()
            .map(|e| (e, sources.add(AudioSource { bytes: synthesize_wav(e).into() })))
            .collect();
        commands.insert_resource(SfxHandles(map));
    }

    pub fn play(commands: &mut Commands, handles: &SfxHandles, sfx: SfxEvent) {
        if let Some(h) = handles.0.get(&sfx) {
            commands.spawn((AudioPlayer(h.clone()), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(sfx.volume()))));
        }
    }
}

#[cfg(feature = "audio")]
fn drain_sfx(mut commands: Commands, mut queue: ResMut<SfxQueue>, handles: Option<Res<backend::SfxHandles>>) {
    let mut played: Vec<SfxEvent> = Vec::new();
    for sfx in queue.0.drain(..) {
        // One instance of each sound per frame (8 pellets = 1 bang).
        if played.contains(&sfx) {
            continue;
        }
        played.push(sfx);
        if let Some(h) = &handles {
            backend::play(&mut commands, h, sfx);
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
        assert_eq!(SfxEvent::ALL.len(), 22);
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
    fn jump_pad_sfx_is_distinct_and_deterministic() {
        let pad = synthesize_wav(SfxEvent::JumpPad);
        assert_eq!(pad, synthesize_wav(SfxEvent::JumpPad));
        assert!(SfxEvent::ALL.contains(&SfxEvent::JumpPad));
        for other in [SfxEvent::Jump, SfxEvent::Bhop, SfxEvent::Dash, SfxEvent::RocketFire] {
            assert_ne!(pad, synthesize_wav(other), "{other:?}");
        }
    }
}
