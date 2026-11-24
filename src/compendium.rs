//! Milestone 21: shareable custom run seeds + the 3D Bestiary & Armory viewer.
//!
//! **Seeds.** Every run carries a short shareable code ([`format_seed_code`]):
//! random runs roll 32-bit seeds shown as `XXXX-XXXX`, and any code typed on
//! the main menu ([`CustomSeedState`]) maps to a seed through [`seed_from_code`]:
//! pure hex parses directly (`A7F3-9C12`, `0xBEEF`), anything else (`MARS7`,
//! `DOOM-RUN`) hashes deterministically (FNV-1a + splitmix64), so the same word
//! builds the same bunker on every platform. On the web build a `#seed=<code>`
//! URL fragment arms the seed at startup ([`parse_url_hash_seed`]).
//!
//! **Compendium.** `AppState::Compendium` stages the real pre-baked demon models
//! ([`EnemyAssets::models`] / [`EnemyAssets::rigs`]) and the real sculpted gun
//! viewmodels ([`crate::weapons::spawn_gun_showcase`]) on an octagonal
//! turntable plinth on the left half of the screen, lit by exactly two lights,
//! next to a UAC dossier card. Everything spawned carries [`CompendiumEntity`]
//! and is despawned on exit.

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;

use crate::enemies::{EnemyAssets, EnemyKind, KIND_COUNT};
use crate::fx::OVERLAY_LAYER;
use crate::hud::text;
use crate::meta::ProfileStore;
use crate::sculpt;
use crate::state::{AppState, RunMode, RunStats};
use crate::ui_kit::{
    corner_brackets, hard_shadow, plate_border, plate_rect, punk_screen_root_tinted, screen_rails, stamped_tag, ButtonStyle, PunkButtonColors,
    UiKitTextures, ACID_GREEN, ARC_CYAN, ASH_DIM, BONE_TEXT, HAZARD_YELLOW, INK_BLACK, IRON_DARK, PUNK_RED, STEEL_BORDER,
};
use crate::weapons::{self, Weapon};

// ===========================================================================
// Seed codec (pure)
// ===========================================================================

/// Longest seed code the main menu accepts.
pub const SEED_CODE_MAX: usize = 16;

fn seed_char_ok(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Trimmed, uppercased, filtered code (no length cap).
fn canonical_seed(raw: &str) -> String {
    raw.trim().chars().filter(|c| seed_char_ok(*c)).map(|c| c.to_ascii_uppercase()).collect()
}

/// Sanitize typed / pasted input: trim, uppercase, keep up to
/// [`SEED_CODE_MAX`] ASCII alphanumeric / `-` / `_` characters.
pub fn normalize_seed_input(raw: &str) -> String {
    canonical_seed(raw).chars().take(SEED_CODE_MAX).collect()
}

/// SplitMix64 finalizer (stable across platforms).
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01B3))
}

/// Seed of a code. Empty / whitespace (no alphanumerics) is `None`; pure hex
/// (`0x...` or 1-16 hex digits with optional dashes) parses directly; any other
/// word hashes deterministically (case-insensitive).
pub fn seed_from_code(code: &str) -> Option<u64> {
    let c = canonical_seed(code);
    if !c.chars().any(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    let body = c.strip_prefix("0X").unwrap_or(&c);
    let digits: String = body.chars().filter(|ch| *ch != '-').collect();
    if !digits.is_empty() && digits.len() <= 16 && digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return u64::from_str_radix(&digits, 16).ok();
    }
    Some(splitmix64(fnv1a64(c.as_bytes())))
}

/// Compact shareable code of a seed: `XXXX-XXXX` for 32-bit seeds (every
/// random run), 16 hex digits otherwise. Always round-trips:
/// `seed_from_code(&format_seed_code(s)) == Some(s)`.
pub fn format_seed_code(seed: u64) -> String {
    if seed <= u32::MAX as u64 { format!("{:04X}-{:04X}", seed >> 16, seed & 0xFFFF) } else { format!("{seed:016X}") }
}

/// A fresh random run seed (32-bit, so its code stays `XXXX-XXXX`).
pub fn random_run_seed(entropy: u64) -> u64 {
    splitmix64(entropy) & 0xFFFF_FFFF
}

/// Seed + label of a run about to start: the armed custom code, or a fresh
/// random 32-bit seed. Returns `(seed, Some(code))` for a custom seed.
pub fn pick_run_seed(state: &CustomSeedState, entropy: u64) -> (u64, Option<String>) {
    match state.armed() {
        Some(seed) => (seed, Some(normalize_seed_input(&state.code))),
        None => (random_run_seed(entropy), None),
    }
}

/// Mark a freshly started run as a custom-seed run (label = the typed code).
pub fn apply_custom_label(run: &mut RunStats, label: Option<String>) {
    if let Some(code) = label {
        run.seed_label = Some(code);
        run.custom_seed = true;
    }
}

/// The code a finished / paused run shares: its label when it maps back to
/// the run's seed, else the formatted seed. `None` for seedless modes.
pub fn shareable_code(run: &RunStats) -> Option<String> {
    if run.mode == RunMode::TimeTrial {
        return None;
    }
    match &run.seed_label {
        Some(l) if seed_from_code(l) == Some(run.run_seed) => Some(l.clone()),
        _ => Some(format_seed_code(run.run_seed)),
    }
}

/// `#seed=<value>` (also `seed=` among `&`-separated pairs) of a URL fragment.
pub fn parse_url_hash_seed(hash: &str) -> Option<String> {
    let h = hash.trim().trim_start_matches('#');
    h.split(['&', ';']).find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if !k.trim().eq_ignore_ascii_case("seed") {
            return None;
        }
        let code = normalize_seed_input(&percent_decode(v));
        seed_from_code(&code).map(|_| code)
    })
}

/// URL fragment that arms `code` when opened (`seed=MARS7`).
pub fn share_hash(code: &str) -> String {
    format!("seed={}", normalize_seed_input(code))
}

/// Minimal `%XX` decoder (seed codes are plain ASCII).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Typed character of a seed-entry key (`A-Z`, `0-9`, `-`).
pub fn seed_key_char(key: KeyCode) -> Option<char> {
    use KeyCode::*;
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX,
        KeyY, KeyZ,
    ];
    let digits = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    let numpad = [Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9];
    if let Some(i) = letters.iter().position(|k| *k == key) {
        return Some((b'A' + i as u8) as char);
    }
    if let Some(i) = digits.iter().position(|k| *k == key).or_else(|| numpad.iter().position(|k| *k == key)) {
        return Some((b'0' + i as u8) as char);
    }
    matches!(key, Minus | NumpadSubtract).then_some('-')
}

/// Main-menu custom seed: the typed code, whether the inline editor is open,
/// and a one-line status for the menu / end screens.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomSeedState {
    pub code: String,
    pub editing: bool,
    pub status_msg: Option<String>,
}

impl CustomSeedState {
    /// Seed the next run will use (`None` = random).
    pub fn armed(&self) -> Option<u64> {
        if self.editing { None } else { seed_from_code(&self.code) }
    }

    pub fn push_char(&mut self, c: char) {
        if self.code.len() < SEED_CODE_MAX && seed_char_ok(c) {
            self.code.push(c.to_ascii_uppercase());
        }
    }

    pub fn backspace(&mut self) {
        self.code.pop();
    }

    /// `S` / SEED switch: open the editor, or lock what was typed.
    pub fn toggle_edit(&mut self) {
        if self.editing {
            self.lock();
        } else {
            self.editing = true;
            self.status_msg = Some("TYPE A-Z 0-9 -  //  ENTER LOCK  //  ESC DONE".into());
        }
    }

    /// Close the editor and arm the typed code (empty = random).
    pub fn lock(&mut self) {
        self.editing = false;
        self.code = normalize_seed_input(&self.code);
        self.status_msg = Some(match seed_from_code(&self.code) {
            Some(_) => format!("SEED {} ARMED // SAME CODE = SAME BUNKER", self.code),
            None => "RANDOM SEED // ROLLS A NEW BUNKER EVERY RUN".into(),
        });
    }

    /// Back to random seeds.
    pub fn clear(&mut self) {
        *self = CustomSeedState { status_msg: Some("SEED CLEARED // RANDOM BUNKER".into()), ..default() };
    }

    /// Arm a fresh random code (`RND`).
    pub fn roll(&mut self, entropy: u64) {
        self.code = format_seed_code(random_run_seed(entropy));
        self.editing = false;
        self.status_msg = Some(format!("ROLLED {} // SHARE IT WITH A FRIEND", self.code));
    }

    /// `SHARE SEED` from a pause / end screen: arm the run's code for a replay.
    pub fn arm_from_run(&mut self, run: &RunStats) -> Option<String> {
        let code = shareable_code(run)?;
        self.code = code.clone();
        self.editing = false;
        self.status_msg = Some(format!("SEED {code} ARMED // LINK: #{}", share_hash(&code)));
        Some(code)
    }

    /// Main menu readout: `RANDOM`, the armed code, or the code + caret.
    pub fn readout(&self) -> String {
        if self.editing {
            format!("{}_", self.code)
        } else if self.code.is_empty() {
            "RANDOM".into()
        } else {
            self.code.clone()
        }
    }
}

/// Browser `#seed=` fragment of the current page (web build only).
#[cfg(target_arch = "wasm32")]
pub fn read_url_hash() -> Option<String> {
    web_sys::window()?.location().hash().ok()
}

/// Write `#seed=<code>` into the address bar so the page URL itself is the
/// share link (works without clipboard permissions). Empty code clears it.
#[cfg(target_arch = "wasm32")]
pub fn sync_url_hash_seed(code: &str) {
    if let Some(w) = web_sys::window() {
        let code = normalize_seed_input(code);
        let hash = if code.is_empty() { String::new() } else { share_hash(&code) };
        let _ = w.location().set_hash(&hash);
    }
}

#[cfg(target_arch = "wasm32")]
fn arm_seed_from_url(mut seed: ResMut<CustomSeedState>) {
    if let Some(code) = read_url_hash().as_deref().and_then(parse_url_hash_seed) {
        seed.status_msg = Some(format!("SEED {code} ARMED FROM SHARED LINK"));
        seed.code = code;
        seed.editing = false;
    }
}

// ===========================================================================
// Exhibits (pure data)
// ===========================================================================

/// Compendium category tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Category {
    #[default]
    Bestiary,
    Armory,
}

impl Category {
    pub fn exhibits(self) -> &'static [ExhibitId] {
        match self {
            Category::Bestiary => &ExhibitId::BESTIARY,
            Category::Armory => &ExhibitId::ARMORY,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Category::Bestiary => "BESTIARY",
            Category::Armory => "ARMORY",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Category::Bestiary => Category::Armory,
            Category::Armory => Category::Bestiary,
        }
    }
}

/// The ten live 3D exhibits.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExhibitId {
    Imp,
    Cacodemon,
    JammerFiend,
    HellBaron,
    PhaseStalker,
    RelaySniper,
    CarrierDaemon,
    Shotgun,
    Plasma,
    Mortar,
}

/// One dossier stat row: label, value and an optional 0..1 bar fill.
#[derive(Debug, Clone, PartialEq)]
pub struct DossierRow {
    pub label: &'static str,
    pub value: String,
    pub fill: Option<f32>,
}

fn row(label: &'static str, value: String, fill: Option<f32>) -> DossierRow {
    DossierRow { label, value, fill: fill.map(|f| f.clamp(0.0, 1.0)) }
}

impl ExhibitId {
    pub const BESTIARY: [ExhibitId; 7] = [
        ExhibitId::Imp,
        ExhibitId::Cacodemon,
        ExhibitId::JammerFiend,
        ExhibitId::HellBaron,
        ExhibitId::PhaseStalker,
        ExhibitId::RelaySniper,
        ExhibitId::CarrierDaemon,
    ];
    pub const ARMORY: [ExhibitId; 3] = [ExhibitId::Shotgun, ExhibitId::Plasma, ExhibitId::Mortar];
    pub const ALL: [ExhibitId; 10] = [
        ExhibitId::Imp,
        ExhibitId::Cacodemon,
        ExhibitId::JammerFiend,
        ExhibitId::HellBaron,
        ExhibitId::PhaseStalker,
        ExhibitId::RelaySniper,
        ExhibitId::CarrierDaemon,
        ExhibitId::Shotgun,
        ExhibitId::Plasma,
        ExhibitId::Mortar,
    ];

    pub fn category(self) -> Category {
        if self.weapon().is_some() { Category::Armory } else { Category::Bestiary }
    }

    pub fn enemy_kind(self) -> Option<EnemyKind> {
        Some(match self {
            ExhibitId::Imp => EnemyKind::Imp,
            ExhibitId::Cacodemon => EnemyKind::Cacodemon,
            ExhibitId::JammerFiend => EnemyKind::JammerFiend,
            ExhibitId::HellBaron => EnemyKind::HellBaron,
            ExhibitId::PhaseStalker => EnemyKind::PhaseStalker,
            ExhibitId::RelaySniper => EnemyKind::RelaySniper,
            ExhibitId::CarrierDaemon => EnemyKind::CarrierDaemon,
            _ => return None,
        })
    }

    pub fn weapon(self) -> Option<Weapon> {
        match self {
            ExhibitId::Shotgun => Some(Weapon::Shotgun),
            ExhibitId::Plasma => Some(Weapon::Plasma),
            ExhibitId::Mortar => Some(Weapon::Rocket),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ExhibitId::Imp => "STATIC IMP",
            ExhibitId::Cacodemon => "DEAD-ZONE CACODEMON",
            ExhibitId::JammerFiend => "JAMMER FIEND",
            ExhibitId::HellBaron => "HELL BARON",
            ExhibitId::PhaseStalker => "PHASE STALKER",
            ExhibitId::RelaySniper => "CATWALK RELAY SNIPER",
            ExhibitId::CarrierDaemon => "THE CARRIER DAEMON",
            ExhibitId::Shotgun => "MARS PUMP SHOTGUN",
            ExhibitId::Plasma => "UAC PLASMA REPEATER",
            ExhibitId::Mortar => "SIGNAL-MORTAR LAUNCHER",
        }
    }

    /// Short selector-strip label.
    pub fn short(self) -> &'static str {
        match self {
            ExhibitId::Imp => "IMP",
            ExhibitId::Cacodemon => "CACO",
            ExhibitId::JammerFiend => "FIEND",
            ExhibitId::HellBaron => "BARON",
            ExhibitId::PhaseStalker => "STALKER",
            ExhibitId::RelaySniper => "SNIPER",
            ExhibitId::CarrierDaemon => "CARRIER",
            ExhibitId::Shotgun => "SHOTGUN",
            ExhibitId::Plasma => "PLASMA",
            ExhibitId::Mortar => "MORTAR",
        }
    }

    pub fn classification(self) -> &'static str {
        match self {
            ExhibitId::Imp => "GRUNT // FLOOR 1+",
            ExhibitId::Cacodemon => "FLYER // FLOOR 1+",
            ExhibitId::JammerFiend => "JAMMER // FLOOR 1+",
            ExhibitId::HellBaron => "ELITE // SOS UPLINK WAVES",
            ExhibitId::PhaseStalker => "FLANKER // FLOOR 2+",
            ExhibitId::RelaySniper => "SENTINEL // FLOOR 2+",
            ExhibitId::CarrierDaemon => "APEX BOSS // FLOOR 5",
            ExhibitId::Shotgun => "SLOT 1 // HITSCAN SPREAD",
            ExhibitId::Plasma => "SLOT 2 // AUTO BOLTS",
            ExhibitId::Mortar => "SLOT 3 // SPLASH ORDNANCE",
        }
    }

    /// Threat rating 1..=5 (demons only).
    pub fn threat(self) -> u8 {
        match self {
            ExhibitId::Imp => 1,
            ExhibitId::Cacodemon => 2,
            ExhibitId::JammerFiend | ExhibitId::PhaseStalker => 3,
            ExhibitId::RelaySniper | ExhibitId::HellBaron => 4,
            ExhibitId::CarrierDaemon => 5,
            _ => 0,
        }
    }

    pub fn lore(self) -> &'static str {
        match self {
            ExhibitId::Imp => "Server-rack static given claws. Hurls ember bolts in packs and goes Static-Enraged inside 0-1 bar Dead Zones.",
            ExhibitId::Cacodemon => "A floating eye-maw that hoards dead air. Drifts over pits and lobs slow cyan plasma orbs from above.",
            ExhibitId::JammerFiend => "Hauls a spinning transceiver that projects a mobile jamming field: your bars die wherever it walks.",
            ExhibitId::HellBaron => "Molten-gold siege brute summoned by the SOS uplink. Soaks a magazine and punishes anyone who camps the ring.",
            ExhibitId::PhaseStalker => "Obsidian flanker that blinks sideways through Dead Zones, shimmering for a heartbeat before every jump.",
            ExhibitId::RelaySniper => "Tripod relay perched on catwalks. A crimson laser telegraphs its rail lock-on before the shot lands.",
            ExhibitId::CarrierDaemon => "The broadcast colossus behind the blackout. Shield pylons, a spinning halo and twin signal cannons.",
            ExhibitId::Shotgun => "Mars-forged pump gun: black-oxide receiver, walnut stock, 8 hitscan pellets per blast.",
            ExhibitId::Plasma => "Twin-rail accelerator spitting fast cyan bolts. The coils glow hotter with every shot.",
            ExhibitId::Mortar => "Heavy industrial launcher with an open breech. Its splash doubles as a Quake-grade jump pad.",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            ExhibitId::Imp => "One point-blank pump blast drops it. Keep strafing: packs flank while you reload.",
            ExhibitId::Cacodemon => "Plasma shreds it at range. Strafe the orbs sideways, never backpedal into a corner.",
            ExhibitId::JammerFiend => "Ping-mark it first, then focus fire: its death restores signal (Jammer Hijack = hotspot).",
            ExhibitId::HellBaron => "Mortar splash + bhop kiting. Never trade melee range with it.",
            ExhibitId::PhaseStalker => "Hold 2+ bars: it can only blink through Dead Zones. Pre-aim the shimmer.",
            ExhibitId::RelaySniper => "Break line of sight when the laser locks, then rocket-jump onto its catwalk.",
            ExhibitId::CarrierDaemon => "Pop the shield pylons first, then dump rockets into the exposed Signal Core.",
            ExhibitId::Shotgun => "Above 20 m/s Doppler Overdrive tightens the choke: bhop in and blast point-blank.",
            ExhibitId::Plasma => "Lead strafing targets mid-bhop; the stream punishes Cacodemons and Stalkers.",
            ExhibitId::Mortar => "Fire at your feet mid-jump for a 13 m/s launch. Blast Thrusters halves self-damage.",
        }
    }

    /// Turntable framing `(uniform scale, lift)` so a 1.9 m Imp, a 4.4 m
    /// Carrier Daemon and a 0.9 m viewmodel gun all fill the viewing bay.
    pub fn framing(self) -> (f32, Vec3) {
        match self {
            ExhibitId::Imp => (1.45, Vec3::ZERO),
            ExhibitId::Cacodemon => (1.3, Vec3::Y * 1.55),
            ExhibitId::JammerFiend => (1.15, Vec3::ZERO),
            ExhibitId::HellBaron => (0.85, Vec3::ZERO),
            ExhibitId::PhaseStalker => (1.3, Vec3::ZERO),
            ExhibitId::RelaySniper => (1.02, Vec3::ZERO),
            ExhibitId::CarrierDaemon => (0.64, Vec3::ZERO),
            // Gun models span z -0.6..0.3 around their grip: recenter the barrel.
            ExhibitId::Shotgun | ExhibitId::Plasma | ExhibitId::Mortar => (3.8, Vec3::new(0.0, 1.45, 0.55)),
        }
    }

    /// The four dossier stat rows (lifetime kills per demon kind from the profile).
    pub fn dossier_rows(self, lifetime_kills: &[u32; KIND_COUNT]) -> [DossierRow; 4] {
        if let Some(kind) = self.enemy_kind() {
            let floor = if kind == EnemyKind::CarrierDaemon { crate::state::FINAL_FLOOR } else { 1 };
            let s = kind.stats(floor);
            let kills = lifetime_kills[kind.index()];
            let threat = self.threat();
            return [
                row("HP", format!("{:.0}", s.hp), Some((s.hp / 1200.0).sqrt())),
                row("SPEED", format!("{:.1} M/S", s.speed), Some(s.speed / 7.0)),
                row("THREAT", format!("{}{}", "#".repeat(threat as usize), "-".repeat(5 - threat as usize)), Some(threat as f32 / 5.0)),
                row("LIFETIME KILLS", kills.to_string(), Some((kills as f32 + 1.0).log10() / 3.0)),
            ];
        }
        match self.weapon().unwrap_or(Weapon::Shotgun) {
            Weapon::Shotgun => [
                row("DAMAGE", format!("{}x{:.1} = {:.0}", weapons::SHOTGUN_PELLETS, weapons::PELLET_DAMAGE, weapons::SHOTGUN_PELLETS as f32 * weapons::PELLET_DAMAGE), Some(0.72)),
                row("CADENCE", format!("{:.1} /S", 1.0 / weapons::SHOTGUN_COOLDOWN), Some(0.2)),
                row("RANGE", format!("{:.0} M HITSCAN", weapons::HITSCAN_RANGE), Some(0.6)),
                row("SPEC", "PUMP // DOPPLER CHOKE".into(), None),
            ],
            Weapon::Plasma => [
                row("DAMAGE", format!("{:.0} / BOLT", weapons::PLASMA_DAMAGE), Some(0.18)),
                row("CADENCE", format!("{:.1} /S", 1.0 / weapons::PLASMA_COOLDOWN), Some(0.95)),
                row("BOLT SPEED", format!("{:.0} M/S", weapons::PLASMA_SPEED), Some(0.8)),
                row("SPEC", format!("{:.0} DPS STREAM", weapons::PLASMA_DAMAGE / weapons::PLASMA_COOLDOWN), None),
            ],
            Weapon::Rocket => [
                row("DAMAGE", format!("{:.0} + {:.0} SPLASH", weapons::ROCKET_DIRECT_DAMAGE, weapons::ROCKET_SPLASH_DAMAGE), Some(1.0)),
                row("CADENCE", format!("{:.1} /S", 1.0 / weapons::ROCKET_COOLDOWN), Some(0.15)),
                row("SPLASH", format!("{:.1} M RADIUS", weapons::ROCKET_SPLASH_RADIUS), Some(0.55)),
                row("SPEC", format!("ROCKET JUMP {:.0} M/S", weapons::ROCKET_JUMP_UP), None),
            ],
        }
    }
}

// ===========================================================================
// Viewer state
// ===========================================================================

/// Current tab / exhibit and turntable yaw (kept between visits).
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct CompendiumState {
    pub category: Category,
    /// Index inside `category.exhibits()`.
    pub index: usize,
    pub yaw: f32,
    /// Last cursor position of a left-drag on the viewport (logical px).
    drag_last: Option<Vec2>,
}

impl Default for CompendiumState {
    fn default() -> Self {
        CompendiumState { category: Category::Bestiary, index: 0, yaw: DEFAULT_YAW, drag_last: None }
    }
}

/// Starting yaw: three-quarter view of the demon's face.
pub const DEFAULT_YAW: f32 = -0.5;
/// Auto-rotation speed (rad/s) and the manual `Q` / `E` spin speed.
pub const AUTO_SPIN: f32 = 0.45;
pub const MANUAL_SPIN: f32 = 2.2;
/// Radians per logical pixel of viewport drag.
pub const DRAG_SPIN: f32 = 0.012;

impl CompendiumState {
    pub fn current(&self) -> ExhibitId {
        let list = self.category.exhibits();
        list[self.index.min(list.len() - 1)]
    }

    pub fn next(&mut self) {
        self.index = (self.index + 1) % self.category.exhibits().len();
    }

    pub fn prev(&mut self) {
        let n = self.category.exhibits().len();
        self.index = (self.index + n - 1) % n;
    }

    pub fn toggle_category(&mut self) {
        self.category = self.category.other();
        self.index = 0;
    }

    pub fn select(&mut self, id: ExhibitId) {
        self.category = id.category();
        self.index = self.category.exhibits().iter().position(|e| *e == id).unwrap_or(0);
    }

    /// `03/07`.
    pub fn counter(&self) -> String {
        format!("{:02}/{:02}", self.index + 1, self.category.exhibits().len())
    }
}

/// Everything the Compendium stage spawns (camera, lights, plinth, turntable,
/// exhibits, UI): despawned on `OnExit(AppState::Compendium)`.
#[derive(Component)]
pub struct CompendiumEntity;

/// The spinning turntable (deck + every exhibit model).
#[derive(Component)]
pub struct TurntableRoot;

/// Shared plinth meshes / materials (baked once at startup).
#[derive(Resource, Clone)]
pub struct CompendiumAssets {
    pub plinth: Handle<Mesh>,
    pub deck: Handle<Mesh>,
    pub rim: Handle<Mesh>,
    pub floor: Handle<Mesh>,
    pub plinth_mat: Handle<StandardMaterial>,
    pub deck_mat: Handle<StandardMaterial>,
    pub rim_mat: Handle<StandardMaterial>,
    pub floor_mat: Handle<StandardMaterial>,
}

/// Plinth / deck / glowing rim / floor meshes (all Solari-convertible).
pub fn stage_meshes() -> [Mesh; 4] {
    [
        sculpt::tapered_prism(8, 1.75, 1.55, 0.55, true),
        sculpt::tapered_prism(8, 1.42, 1.42, 0.08, true),
        sculpt::armored_ring(1.5, 0.06, 0.035, 32, true),
        sculpt::tapered_prism(24, 7.0, 7.0, 0.04, true),
    ]
}

fn create_compendium_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let [plinth, deck, rim, floor] = stage_meshes().map(|m| meshes.add(m));
    let mut m = |base: Color, rough: f32, metal: f32, emissive: LinearRgba| {
        materials.add(StandardMaterial { base_color: base, perceptual_roughness: rough, metallic: metal, emissive, ..default() })
    };
    commands.insert_resource(CompendiumAssets {
        plinth,
        deck,
        rim,
        floor,
        plinth_mat: m(Color::srgb(0.15, 0.14, 0.14), 0.5, 0.75, LinearRgba::BLACK),
        deck_mat: m(Color::srgb(0.28, 0.27, 0.27), 0.38, 0.85, LinearRgba::BLACK),
        rim_mat: m(Color::srgb(1.0, 0.6, 0.1), 0.4, 0.1, LinearRgba::rgb(2.2, 0.85, 0.06)),
        floor_mat: m(Color::srgb(0.05, 0.045, 0.045), 0.9, 0.2, LinearRgba::BLACK),
    });
}

// ===========================================================================
// Plugin
// ===========================================================================

pub struct CompendiumPlugin;

impl Plugin for CompendiumPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CustomSeedState>()
            .init_resource::<CompendiumState>()
            .add_systems(Startup, create_compendium_assets)
            .add_systems(OnEnter(AppState::Compendium), spawn_compendium)
            .add_systems(OnExit(AppState::Compendium), despawn_compendium)
            .add_systems(
                Update,
                (compendium_input, compendium_clicks, spin_turntable, refresh_compendium).chain().run_if(in_state(AppState::Compendium)),
            );
        #[cfg(target_arch = "wasm32")]
        app.add_systems(Startup, arm_seed_from_url);
    }
}

// ===========================================================================
// Stage + UI
// ===========================================================================

/// Exhibit model holder (visible only while selected).
#[derive(Component)]
pub struct ExhibitModel(pub ExhibitId);

/// What a Compendium switch does.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompAction {
    Prev,
    Next,
    Back,
    Tab(Category),
    Select(ExhibitId),
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum DossierText {
    Header,
    Counter,
    Title,
    Badge,
    StatLabel(u8),
    StatValue(u8),
    Lore,
    Tip,
}

#[derive(Component)]
struct CompendiumCamera;

#[derive(Component)]
struct StatTrack(u8);

#[derive(Component)]
struct StatFill(u8);

/// Camera: framed so the turntable (origin) sits at the center of the left
/// half of a 16:9 screen.
pub const CAMERA_FOV_DEG: f32 = 30.0;
pub const CAMERA_DIST: f32 = 11.0;
pub const LOOK_HEIGHT: f32 = 1.45;

/// Horizontal camera offset that puts the turntable axis at NDC x = -0.5.
pub fn camera_offset_x(aspect: f32) -> f32 {
    0.5 * CAMERA_DIST * (CAMERA_FOV_DEG.to_radians() * 0.5).tan() * aspect
}

fn comp_button(label: &str, action: CompAction, font: f32, style: ButtonStyle) -> impl Bundle {
    let colors = PunkButtonColors::of(style);
    (
        action,
        colors,
        Node {
            padding: UiRect::axes(Val::Px(font * 0.9), Val::Px(font * 0.35)),
            border: plate_rect((5.0, 1.0, 1.0, 3.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        colors.border(false),
        BackgroundColor(colors.idle_bg),
        hard_shadow(3.0),
        children![text(label, font, colors.idle_text)],
    )
}

/// Ensure a Compendium showcase material uses forward rendering (if a prior
/// Solari RT run flipped a shared demon material to `Deferred`, clone a forward
/// copy once so the forward-only Compendium camera always renders it).
fn showcase_material(
    handle: &Handle<StandardMaterial>,
    materials: &mut Assets<StandardMaterial>,
    cloned: &mut Vec<(AssetId<StandardMaterial>, Handle<StandardMaterial>)>,
) -> Handle<StandardMaterial> {
    if let Some((_, h)) = cloned.iter().find(|(id, _)| *id == handle.id()) {
        return h.clone();
    }
    if let Some(m) = materials.get(handle.id())
        && m.opaque_render_method == bevy::material::OpaqueRendererMethod::Deferred
    {
        let mut fwd = m.clone();
        fwd.opaque_render_method = bevy::material::OpaqueRendererMethod::Forward;
        let h = materials.add(fwd);
        cloned.push((handle.id(), h.clone()));
        return h;
    }
    handle.clone()
}

fn compendium_camera_transform(aspect: f32) -> Transform {
    let a = if aspect.is_finite() && aspect > 0.2 { aspect } else { 16.0 / 9.0 };
    // Clamp so portrait/narrow windows keep the turntable inside the left bay.
    let cx = camera_offset_x(a.min(2.4));
    Transform::from_xyz(cx, LOOK_HEIGHT + 1.1, CAMERA_DIST).looking_at(Vec3::new(cx, LOOK_HEIGHT, 0.0), Vec3::Y)
}

#[allow(clippy::too_many_arguments)]
fn spawn_compendium(
    mut commands: Commands,
    mut state: ResMut<CompendiumState>,
    window: Query<&Window, With<PrimaryWindow>>,
    enemy: Option<Res<EnemyAssets>>,
    stage: Option<Res<CompendiumAssets>>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    state.yaw = DEFAULT_YAW;
    state.drag_last = None;
    let overlay = RenderLayers::layer(OVERLAY_LAYER);
    let msaa = msaa.map(|m| m.0).unwrap_or(Msaa::Sample2);
    let aspect = window.single().ok().filter(|w| w.height() > 0.0).map_or(16.0 / 9.0, |w| w.width() / w.height());
    commands.spawn((
        CompendiumEntity,
        CompendiumCamera,
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::Custom(Color::srgb(0.035, 0.03, 0.03)), ..default() },
        Projection::Perspective(PerspectiveProjection { fov: CAMERA_FOV_DEG.to_radians(), ..default() }),
        msaa,
        overlay.clone(),
        AmbientLight { color: Color::srgb(1.0, 0.85, 0.78), brightness: 260.0, ..default() },
        compendium_camera_transform(aspect),
    ));
    // Exactly two lights on OVERLAY_LAYER: warm key above-left-front, cool rim behind-right.
    for (color, intensity, pos) in [
        (Color::srgb(1.0, 0.86, 0.72), 4_200_000.0, Vec3::new(-3.2, 6.0, 5.5)),
        (Color::srgb(0.45, 0.78, 1.0), 3_400_000.0, Vec3::new(3.8, 4.2, -4.5)),
    ] {
        commands.spawn((
            CompendiumEntity,
            PointLight { color, intensity, range: 30.0, shadow_maps_enabled: false, ..default() },
            Transform::from_translation(pos),
            overlay.clone(),
        ));
    }
    // Place all Compendium meshes on OVERLAY_LAYER so Solari RT never switches
    // stage/demon materials to deferred rendering.
    if let Some(a) = stage.as_deref() {
        commands.spawn((CompendiumEntity, overlay.clone(), Mesh3d(a.floor.clone()), MeshMaterial3d(a.floor_mat.clone()), Transform::from_xyz(0.0, -0.58, 0.0)));
        commands.spawn((CompendiumEntity, overlay.clone(), Mesh3d(a.plinth.clone()), MeshMaterial3d(a.plinth_mat.clone()), Transform::from_xyz(0.0, -0.3, 0.0)));
        commands.spawn((CompendiumEntity, overlay.clone(), Mesh3d(a.rim.clone()), MeshMaterial3d(a.rim_mat.clone()), Transform::from_xyz(0.0, -0.03, 0.0)));
    }
    let table = commands.spawn((CompendiumEntity, TurntableRoot, Transform::from_rotation(Quat::from_rotation_y(state.yaw)), Visibility::default())).id();
    if let Some(a) = stage.as_deref() {
        commands.spawn((ChildOf(table), overlay.clone(), Mesh3d(a.deck.clone()), MeshMaterial3d(a.deck_mat.clone()), Transform::from_xyz(0.0, 0.0, 0.0)));
    }
    let current = state.current();
    let vis = |id: ExhibitId| if id == current { Visibility::Inherited } else { Visibility::Hidden };
    // Demons: the real pre-baked models + rigs (rotated +PI so they face the viewer).
    let mut fwd_cache = Vec::new();
    if let Some(ea) = enemy.as_deref() {
        for id in ExhibitId::BESTIARY {
            let Some(kind) = id.enemy_kind() else { continue };
            let (s, lift) = id.framing();
            let holder = commands
                .spawn((
                    ChildOf(table),
                    ExhibitModel(id),
                    Transform::from_translation(lift + Vec3::Y * 0.04)
                        .with_rotation(Quat::from_rotation_y(std::f32::consts::PI))
                        .with_scale(Vec3::splat(s)),
                    vis(id),
                ))
                .id();
            for part in &ea.models[kind.index()] {
                let mat = showcase_material(&part.material, &mut materials, &mut fwd_cache);
                commands.spawn((ChildOf(holder), overlay.clone(), Mesh3d(part.mesh.clone()), MeshMaterial3d(mat), Transform::IDENTITY));
            }
            for rig in &ea.rigs[kind.index()] {
                let pivot = commands.spawn((ChildOf(holder), rig.rest, Visibility::default())).id();
                for part in &rig.parts {
                    let mat = showcase_material(&part.material, &mut materials, &mut fwd_cache);
                    commands.spawn((ChildOf(pivot), overlay.clone(), Mesh3d(part.mesh.clone()), MeshMaterial3d(mat), Transform::IDENTITY));
                }
            }
        }
    }
    // Guns: the real sculpted viewmodels, built by the viewmodel code itself.
    let (s, lift) = ExhibitId::Shotgun.framing();
    let rack = commands.spawn((ChildOf(table), Transform::from_translation(lift).with_scale(Vec3::splat(s)), Visibility::default())).id();
    let guns = weapons::spawn_gun_showcase(&mut commands, rack, &mut meshes, &mut materials, &mut images);
    for (pivot, id) in guns.into_iter().zip(ExhibitId::ARMORY) {
        commands.entity(pivot).insert((ExhibitModel(id), vis(id)));
    }

    spawn_dossier_ui(&mut commands, tex.as_deref());
}

fn spawn_dossier_ui(commands: &mut Commands, tex: Option<&UiKitTextures>) {
    let root = punk_screen_root_tinted(commands, AppState::Compendium, tex, Color::NONE, Color::NONE, ("ARCHIVE LINK: LIVE", ACID_GREEN));
    commands.entity(root).insert((
        CompendiumEntity,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Stretch,
            padding: UiRect { left: Val::Px(26.0), right: Val::Px(30.0), top: Val::Px(44.0), bottom: Val::Px(36.0) },
            column_gap: Val::Px(24.0),
            ..default()
        },
    ));
    screen_rails(commands, root, tex, 12.0, false);

    // ---- Left: transparent viewport over the 3D turntable ----
    let left = commands
        .spawn((
            ChildOf(root),
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(10.0)),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(STEEL_BORDER.with_alpha(0.35)),
        ))
        .id();
    commands.spawn((
        ChildOf(left),
        Node { column_gap: Val::Px(10.0), align_items: AlignItems::Center, ..default() },
        children![stamped_tag("LIVE TURNTABLE", PUNK_RED, INK_BLACK, 12.0), text("// SPECIMEN BAY 07 // CONTAINMENT FIELD NOMINAL", 11.0, ASH_DIM)],
    ));
    let bottom = commands.spawn((ChildOf(left), Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(6.0), ..default() })).id();
    commands.spawn((ChildOf(bottom), text("HOLD Q / E OR DRAG TO SPIN  //  A D OR < > CYCLE  //  TAB SWITCHES ARCHIVE", 11.0, ASH_DIM)));
    let strip = commands.spawn((ChildOf(bottom), Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(6.0), row_gap: Val::Px(6.0), ..default() })).id();
    for (i, id) in ExhibitId::ALL.into_iter().enumerate() {
        let n = if id.category() == Category::Bestiary { i + 1 } else { i - 6 };
        commands.spawn((ChildOf(strip), comp_button(&format!("{n:02} {}", id.short()), CompAction::Select(id), 12.0, ButtonStyle::ScrapSteel)));
    }
    corner_brackets(commands, left, HAZARD_YELLOW.with_alpha(0.8), 18.0, 3.0, (1.0, 1.0, 1.0, 1.0));

    // ---- Right: UAC dossier card ----
    let card = commands
        .spawn((
            ChildOf(root),
            Node {
                width: Val::Px(520.0),
                flex_shrink: 0.0,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(9.0),
                padding: UiRect::axes(Val::Px(20.0), Val::Px(14.0)),
                border: plate_rect((5.0, 2.0, 2.0, 5.0)),
                ..default()
            },
            plate_border(ARC_CYAN, STEEL_BORDER),
            BackgroundColor(IRON_DARK.with_alpha(0.94)),
            hard_shadow(7.0),
        ))
        .id();
    commands.spawn((
        ChildOf(card),
        Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() },
        children![(DossierText::Header, text("", 12.0, ARC_CYAN)), (DossierText::Counter, text("", 14.0, HAZARD_YELLOW))],
    ));
    let tabs = commands.spawn((ChildOf(card), Node { column_gap: Val::Px(10.0), ..default() })).id();
    for cat in [Category::Bestiary, Category::Armory] {
        commands.spawn((ChildOf(tabs), comp_button(&format!("{}  [TAB]", cat.name()), CompAction::Tab(cat), 14.0, ButtonStyle::HazardAmber)));
    }
    commands.spawn((
        ChildOf(card),
        Node { margin: UiRect::top(Val::Px(4.0)), border: UiRect::bottom(Val::Px(2.0)), padding: UiRect::bottom(Val::Px(6.0)), ..default() },
        BorderColor::all(PUNK_RED.with_alpha(0.7)),
        children![(DossierText::Title, text("", 30.0, BONE_TEXT))],
    ));
    commands.spawn((
        ChildOf(card),
        Node { padding: UiRect::axes(Val::Px(8.0), Val::Px(2.0)), border: UiRect::bottom(Val::Px(2.0)), align_self: AlignSelf::FlexStart, ..default() },
        BorderColor::all(INK_BLACK.with_alpha(0.85)),
        BackgroundColor(HAZARD_YELLOW),
        children![(DossierText::Badge, text("", 13.0, INK_BLACK))],
    ));
    let stats = commands
        .spawn((
            ChildOf(card),
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(7.0),
                padding: UiRect::all(Val::Px(10.0)),
                border: plate_rect((3.0, 1.0, 1.0, 1.0)),
                ..default()
            },
            BorderColor { left: ARC_CYAN, ..crate::ui_kit::inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.7)),
        ))
        .id();
    for i in 0..4u8 {
        let r = commands.spawn((ChildOf(stats), Node { align_items: AlignItems::Center, column_gap: Val::Px(10.0), ..default() })).id();
        commands.spawn((ChildOf(r), Node { width: Val::Px(124.0), flex_shrink: 0.0, ..default() }, children![(DossierText::StatLabel(i), text("", 12.0, ASH_DIM))]));
        let track = commands
            .spawn((
                ChildOf(r),
                StatTrack(i),
                Node { width: Val::Px(150.0), height: Val::Px(9.0), flex_shrink: 0.0, border: UiRect::all(Val::Px(1.0)), ..default() },
                BorderColor::all(STEEL_BORDER),
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.05)),
            ))
            .id();
        commands.spawn((ChildOf(track), StatFill(i), Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(ACID_GREEN)));
        commands.spawn((ChildOf(r), DossierText::StatValue(i), text("", 14.0, BONE_TEXT)));
    }
    commands.spawn((ChildOf(card), DossierText::Lore, text("", 14.0, BONE_TEXT.with_alpha(0.85))));
    commands.spawn((
        ChildOf(card),
        Node { padding: UiRect::left(Val::Px(10.0)), border: UiRect::left(Val::Px(3.0)), ..default() },
        BorderColor::all(ACID_GREEN),
        children![(DossierText::Tip, text("", 13.0, ACID_GREEN))],
    ));
    commands.spawn((ChildOf(card), Node { flex_grow: 1.0, ..default() }));
    let nav = commands.spawn((ChildOf(card), Node { column_gap: Val::Px(10.0), justify_content: JustifyContent::SpaceBetween, ..default() })).id();
    commands.spawn((ChildOf(nav), comp_button("PREV  [A]", CompAction::Prev, 15.0, ButtonStyle::ScrapSteel)));
    commands.spawn((ChildOf(nav), comp_button("NEXT  [D]", CompAction::Next, 15.0, ButtonStyle::PrimaryAcid)));
    commands.spawn((ChildOf(nav), comp_button("BACK  [ESC]", CompAction::Back, 15.0, ButtonStyle::DangerRed)));
}

fn despawn_compendium(mut commands: Commands, q: Query<Entity, With<CompendiumEntity>>) {
    for e in &q {
        commands.entity(e).try_despawn();
    }
}

/// Keyboard: `A`/`D`/arrows cycle, `Tab` switches archive, `Esc` returns,
/// `Q`/`E` spin (in [`spin_turntable`]).
pub fn compendium_key_action(keys: &ButtonInput<KeyCode>, category: Category) -> Option<CompAction> {
    if keys.just_pressed(KeyCode::Escape) {
        Some(CompAction::Back)
    } else if keys.any_just_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]) {
        Some(CompAction::Prev)
    } else if keys.any_just_pressed([KeyCode::KeyD, KeyCode::ArrowRight]) {
        Some(CompAction::Next)
    } else if keys.just_pressed(KeyCode::Tab) {
        Some(CompAction::Tab(category.other()))
    } else {
        None
    }
}

fn apply_action(action: CompAction, state: &mut CompendiumState, next: &mut NextState<AppState>) {
    match action {
        CompAction::Prev => state.prev(),
        CompAction::Next => state.next(),
        CompAction::Back => next.set(AppState::MainMenu),
        CompAction::Tab(cat) if cat != state.category => state.toggle_category(),
        CompAction::Tab(_) => {}
        CompAction::Select(id) => state.select(id),
    }
}

fn compendium_input(keys: Res<ButtonInput<KeyCode>>, mut state: ResMut<CompendiumState>, mut next: ResMut<NextState<AppState>>) {
    if let Some(a) = compendium_key_action(&keys, state.category) {
        apply_action(a, &mut state, &mut next);
    }
}

#[allow(clippy::type_complexity)]
fn compendium_clicks(
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut buttons: Query<(&ComputedNode, &UiGlobalTransform, &CompAction, &PunkButtonColors, &mut BackgroundColor, &mut BorderColor, Option<&Children>)>,
    mut labels: Query<&mut TextColor>,
    mut state: ResMut<CompendiumState>,
    mut next: ResMut<NextState<AppState>>,
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    let mut clicked = None;
    for (node, tf, action, colors, mut bg, mut border, children) in &mut buttons {
        let hovered = cursor.is_some_and(|c| node.contains_point(*tf, c));
        let selected = match *action {
            CompAction::Tab(c) => c == state.category,
            CompAction::Select(id) => id == state.current(),
            _ => false,
        };
        let lit = hovered || selected;
        let want = colors.bg(lit);
        if bg.0 != want {
            bg.0 = want;
        }
        let want = colors.border(lit);
        if *border != want {
            *border = want;
        }
        for child in children.into_iter().flatten() {
            if let Ok(mut c) = labels.get_mut(*child) {
                let want = colors.text(lit);
                if c.0 != want {
                    c.0 = want;
                }
            }
        }
        if hovered && mouse.just_released(MouseButton::Left) {
            clicked = Some(*action);
        }
    }
    if let Some(a) = clicked {
        apply_action(a, &mut state, &mut next);
    }
}

#[allow(clippy::type_complexity)]
fn spin_turntable(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut state: ResMut<CompendiumState>,
    mut table: Query<&mut Transform, (With<TurntableRoot>, Without<CompendiumCamera>)>,
    mut cam: Query<&mut Transform, (With<CompendiumCamera>, Without<TurntableRoot>)>,
) {
    if let (Ok(w), Ok(mut ctf)) = (window.single(), cam.single_mut())
        && w.height() > 0.0
    {
        let want = compendium_camera_transform(w.width() / w.height());
        if (ctf.translation - want.translation).length_squared() > 1e-6 {
            *ctf = want;
        }
    }
    let dt = time.delta_secs();
    let mut manual = false;
    let mut spin = 0.0;
    if keys.pressed(KeyCode::KeyQ) {
        spin -= MANUAL_SPIN * dt;
        manual = true;
    }
    if keys.pressed(KeyCode::KeyE) {
        spin += MANUAL_SPIN * dt;
        manual = true;
    }
    let cursor = window.single().ok().and_then(|w| w.cursor_position().map(|c| (c, w.width())));
    match cursor {
        Some((c, w)) if mouse.pressed(MouseButton::Left) && (c.x < w * 0.5 || state.drag_last.is_some()) => {
            if let Some(last) = state.drag_last {
                spin += (c.x - last.x) * DRAG_SPIN;
            }
            state.drag_last = Some(c);
            manual = true;
        }
        _ => state.drag_last = None,
    }
    if !manual {
        spin += AUTO_SPIN * dt;
    }
    if spin != 0.0 {
        state.yaw = (state.yaw + spin).rem_euclid(std::f32::consts::TAU);
    }
    for mut tf in &mut table {
        let want = Quat::from_rotation_y(state.yaw);
        if tf.rotation != want {
            tf.rotation = want;
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn refresh_compendium(
    state: Res<CompendiumState>,
    store: Option<Res<ProfileStore>>,
    mut last: Local<Option<(Category, usize)>>,
    mut models: Query<(&ExhibitModel, &mut Visibility)>,
    mut texts: Query<(&DossierText, &mut Text)>,
    mut fills: Query<(&StatFill, &mut Node), Without<StatTrack>>,
    mut tracks: Query<(&StatTrack, &mut Node), Without<StatFill>>,
    mut chips: Query<(&CompAction, &mut Node), (Without<StatTrack>, Without<StatFill>)>,
) {
    let key = (state.category, state.index);
    if *last == Some(key) && !texts.iter().any(|(_, t)| t.0.is_empty()) {
        return;
    }
    *last = Some(key);
    let id = state.current();
    for (m, mut v) in &mut models {
        let want = if m.0 == id { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    let kills = store.as_deref().map(|s| s.profile.stats.enemy_kills).unwrap_or_default();
    let rows = id.dossier_rows(&kills);
    for (slot, mut t) in &mut texts {
        let s = match *slot {
            DossierText::Header => format!("UAC FIELD TERMINAL // {}", state.category.name()),
            DossierText::Counter => state.counter(),
            DossierText::Title => id.title().to_string(),
            DossierText::Badge => id.classification().to_string(),
            DossierText::StatLabel(i) => rows[i as usize].label.to_string(),
            DossierText::StatValue(i) => rows[i as usize].value.clone(),
            DossierText::Lore => id.lore().to_string(),
            DossierText::Tip => format!("TACTICAL // {}", id.tip()),
        };
        if t.0 != s {
            t.0 = s;
        }
    }
    for (f, mut n) in &mut fills {
        n.width = Val::Percent(rows[f.0 as usize].fill.unwrap_or(0.0) * 100.0);
    }
    for (t, mut n) in &mut tracks {
        n.display = if rows[t.0 as usize].fill.is_some() { Display::Flex } else { Display::None };
    }
    for (a, mut n) in &mut chips {
        if let CompAction::Select(e) = *a {
            let want = if e.category() == state.category { Display::Flex } else { Display::None };
            if n.display != want {
                n.display = want;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_uppercases_and_caps() {
        assert_eq!(normalize_seed_input("  mars7 "), "MARS7");
        assert_eq!(normalize_seed_input("doom run!"), "DOOMRUN");
        assert_eq!(normalize_seed_input("a_b-c"), "A_B-C");
        assert_eq!(normalize_seed_input("0123456789abcdefXYZ"), "0123456789ABCDEF");
        assert_eq!(normalize_seed_input("   "), "");
    }

    #[test]
    fn seed_codes_parse_hex_and_hash_words() {
        assert_eq!(seed_from_code(""), None);
        assert_eq!(seed_from_code("  \t "), None);
        assert_eq!(seed_from_code("---"), None);
        assert_eq!(seed_from_code("A7F3-9C12"), Some(0xA7F3_9C12));
        assert_eq!(seed_from_code("a7f39c12"), Some(0xA7F3_9C12));
        assert_eq!(seed_from_code("0xBEEF"), Some(0xBEEF));
        assert_eq!(seed_from_code("FFFFFFFFFFFFFFFF"), Some(u64::MAX));
        // Words hash deterministically and case-insensitively.
        let mars = seed_from_code("MARS7").unwrap();
        assert_eq!(seed_from_code("mars7"), Some(mars));
        assert_eq!(seed_from_code(" Mars7 "), Some(mars));
        assert_ne!(seed_from_code("MARS8"), Some(mars));
        assert_ne!(seed_from_code("DOOM-RUN"), seed_from_code("DOOMRUN"));
        // Pinned value: the same word must build the same bunker on every platform.
        assert_eq!(mars, splitmix64(fnv1a64(b"MARS7")));
        assert_eq!(fnv1a64(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xAF63_DC4C_8601_EC8C);
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn formatted_codes_round_trip() {
        for s in [0u64, 1, 0xBEEF, 0x791E_0A5A, u32::MAX as u64, u32::MAX as u64 + 1, 0xDEAD_BEEF_CAFE_F00D, u64::MAX] {
            let code = format_seed_code(s);
            assert!(code.len() <= SEED_CODE_MAX, "{code}");
            assert_eq!(normalize_seed_input(&code), code);
            assert_eq!(seed_from_code(&code), Some(s), "{code}");
        }
        assert_eq!(format_seed_code(0x791E_0A5A), "791E-0A5A");
        for e in 0..200u64 {
            let s = random_run_seed(e.wrapping_mul(0x1234_5678_9ABC));
            assert!(s <= u32::MAX as u64);
            assert_eq!(format_seed_code(s).len(), 9);
            assert_eq!(seed_from_code(&format_seed_code(s)), Some(s));
        }
    }

    #[test]
    fn url_hash_parsing() {
        assert_eq!(parse_url_hash_seed("#seed=MARS7"), Some("MARS7".into()));
        assert_eq!(parse_url_hash_seed("seed=mars7"), Some("MARS7".into()));
        assert_eq!(parse_url_hash_seed("#foo=1&SEED=a7f3-9c12"), Some("A7F3-9C12".into()));
        assert_eq!(parse_url_hash_seed("#seed=DOOM%2DRUN"), Some("DOOM-RUN".into()));
        assert_eq!(parse_url_hash_seed("#seed="), None);
        assert_eq!(parse_url_hash_seed("#seed=%20%20"), None);
        assert_eq!(parse_url_hash_seed("#level=3"), None);
        assert_eq!(parse_url_hash_seed(""), None);
        assert_eq!(parse_url_hash_seed("#seed=%"), None);
        assert_eq!(share_hash(" mars7 "), "seed=MARS7");
        assert_eq!(parse_url_hash_seed(&format!("#{}", share_hash("791E-0A5A"))), Some("791E-0A5A".into()));
    }

    #[test]
    fn seed_state_editing_locking_and_arming() {
        let mut s = CustomSeedState::default();
        assert_eq!(s.armed(), None);
        assert_eq!(s.readout(), "RANDOM");
        s.toggle_edit();
        assert!(s.editing);
        for c in "doom-run".chars() {
            s.push_char(c);
        }
        assert_eq!(s.readout(), "DOOM-RUN_");
        assert_eq!(s.armed(), None, "nothing is armed while typing");
        s.backspace();
        s.push_char('N');
        s.push_char('!');
        s.lock();
        assert!(!s.editing);
        assert_eq!(s.code, "DOOM-RUN");
        assert_eq!(s.armed(), seed_from_code("DOOM-RUN"));
        for _ in 0..30 {
            s.push_char('Z');
        }
        assert_eq!(s.code.len(), SEED_CODE_MAX);
        s.roll(42);
        assert_eq!(s.armed(), seed_from_code(&s.code));
        assert_eq!(s.code.len(), 9);
        s.clear();
        assert_eq!(s.armed(), None);
        assert!(s.status_msg.is_some());
    }

    #[test]
    fn pick_seed_uses_armed_code_and_labels_runs() {
        let mut s = CustomSeedState { code: "MARS7".into(), ..default() };
        let (seed, label) = pick_run_seed(&s, 7);
        assert_eq!(Some(seed), seed_from_code("MARS7"));
        assert_eq!(label.as_deref(), Some("MARS7"));
        let mut run = RunStats::default();
        run.start_new_run(seed);
        apply_custom_label(&mut run, label);
        assert!(run.custom_seed);
        assert_eq!(shareable_code(&run).as_deref(), Some("MARS7"));
        s.code.clear();
        let (seed, label) = pick_run_seed(&s, 7);
        assert_eq!(label, None);
        run.start_new_run(seed);
        assert!(!run.custom_seed);
        let code = shareable_code(&run).unwrap();
        assert_eq!(seed_from_code(&code), Some(run.run_seed));
        assert_eq!(run.seed_label.as_deref(), Some(code.as_str()));
        // Share from a run, then replay it.
        let mut fresh = CustomSeedState::default();
        assert_eq!(fresh.arm_from_run(&run), Some(code.clone()));
        assert_eq!(fresh.armed(), Some(run.run_seed));
        run.mode = RunMode::TimeTrial;
        assert_eq!(shareable_code(&run), None);
    }

    #[test]
    fn seed_keys_map_letters_digits_and_dash() {
        assert_eq!(seed_key_char(KeyCode::KeyA), Some('A'));
        assert_eq!(seed_key_char(KeyCode::KeyZ), Some('Z'));
        assert_eq!(seed_key_char(KeyCode::Digit7), Some('7'));
        assert_eq!(seed_key_char(KeyCode::Numpad0), Some('0'));
        assert_eq!(seed_key_char(KeyCode::Minus), Some('-'));
        assert_eq!(seed_key_char(KeyCode::Enter), None);
        assert_eq!(seed_key_char(KeyCode::Space), None);
    }

    #[test]
    fn exhibits_cycle_and_switch_tabs() {
        assert_eq!(ExhibitId::BESTIARY.len() + ExhibitId::ARMORY.len(), ExhibitId::ALL.len());
        let mut s = CompendiumState::default();
        assert_eq!(s.current(), ExhibitId::Imp);
        assert_eq!(s.counter(), "01/07");
        s.prev();
        assert_eq!(s.current(), ExhibitId::CarrierDaemon);
        s.next();
        s.next();
        assert_eq!(s.current(), ExhibitId::Cacodemon);
        s.toggle_category();
        assert_eq!((s.category, s.current(), s.counter().as_str()), (Category::Armory, ExhibitId::Shotgun, "01/03"));
        s.prev();
        assert_eq!(s.current(), ExhibitId::Mortar);
        s.select(ExhibitId::RelaySniper);
        assert_eq!((s.category, s.counter().as_str()), (Category::Bestiary, "06/07"));
        for id in ExhibitId::ALL {
            assert_eq!(id.category() == Category::Armory, id.weapon().is_some());
            assert_eq!(id.enemy_kind().is_some(), id.weapon().is_none());
            assert!(!id.lore().is_empty() && !id.tip().is_empty() && !id.classification().is_empty());
        }
        let kinds: Vec<_> = ExhibitId::BESTIARY.iter().filter_map(|e| e.enemy_kind()).collect();
        assert_eq!(kinds, EnemyKind::ALL.to_vec(), "every demon has an exhibit, in table order");
        assert_eq!(ExhibitId::CarrierDaemon.classification(), "APEX BOSS // FLOOR 5");
        assert_eq!(ExhibitId::PhaseStalker.classification(), "FLANKER // FLOOR 2+");
    }

    #[test]
    fn dossier_rows_report_stats_and_lifetime_kills() {
        let mut kills = [0u32; KIND_COUNT];
        kills[EnemyKind::HellBaron.index()] = 42;
        let rows = ExhibitId::HellBaron.dossier_rows(&kills);
        assert_eq!(rows[3].label, "LIFETIME KILLS");
        assert_eq!(rows[3].value, "42");
        assert_eq!(rows[0].value, format!("{:.0}", EnemyKind::HellBaron.stats(1).hp));
        let boss = ExhibitId::CarrierDaemon.dossier_rows(&kills);
        assert_eq!(boss[0].value, "1200");
        assert_eq!(boss[2].fill, Some(1.0));
        let mortar = ExhibitId::Mortar.dossier_rows(&kills);
        assert_eq!(mortar[0].label, "DAMAGE");
        assert_eq!(mortar[1].label, "CADENCE");
        assert_eq!(mortar[3].fill, None, "SPEC is a value-only row");
        for id in ExhibitId::ALL {
            for r in id.dossier_rows(&kills) {
                assert!(r.fill.is_none_or(|f| (0.0..=1.0).contains(&f)), "{id:?} {r:?}");
                assert!(r.value.len() <= 24, "{id:?} {r:?} fits the dossier row");
            }
            assert!(id.title().len() <= 24, "{id:?} title fits the 520px card at 30px");
        }
    }

    #[test]
    fn exhibits_fit_the_viewing_bay() {
        // Scaled heights stay inside the visible band above the plinth.
        let half_h = CAMERA_DIST * (CAMERA_FOV_DEG.to_radians() * 0.5).tan();
        for id in ExhibitId::BESTIARY {
            let kind = id.enemy_kind().unwrap();
            let (s, lift) = id.framing();
            let top = lift.y + kind.stats(1).height * s;
            assert!(top < LOOK_HEIGHT + half_h - 0.4, "{id:?} top {top}");
            assert!(kind.stats(1).height * s > 1.8, "{id:?} reads big enough");
        }
        // The turntable axis lands in the center of the left half at 16:9.
        let cx = camera_offset_x(16.0 / 9.0);
        let half_w = half_h * 16.0 / 9.0;
        assert!(((0.0 - cx) / half_w + 0.5).abs() < 1e-4);
    }

    #[test]
    fn stage_meshes_are_rt_convertible() {
        for m in stage_meshes() {
            assert!(crate::raytracing::rt_mesh_convertible(&m));
        }
    }
}
