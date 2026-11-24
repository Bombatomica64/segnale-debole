//! Meta-progression: persistent player choices, unlocks and opt-in challenges.
//!
//! Design rule: the profile persists *options* (starter OS chips, carrier
//! contracts, handset skins, app licenses, the 12-card active catalog) and
//! lifetime stats, never raw combat power. Every run still starts on Floor 1
//! with the same 0-bar tension; a chip is a sidegrade with a real trade-off.
//!
//! Flow:
//! - [`PlayerProfile`] (serde, persisted by [`ProfileStore`]) holds earned
//!   [`FeatId`]s; every unlock is *derived* from feats (plus the
//!   `jam_demo_unlock_all` switch, which never mutates the earned set).
//! - At run start [`PlayerProfile::build_loadout`] snapshots the equipped
//!   choices into the [`RunLoadout`] resource and [`apply_loadout_to_run`]
//!   folds chip/contract effects into [`PlayerModifiers`].
//! - On `OnEnter(GameOver | Victory)` [`record_run_outcome`] updates lifetime
//!   stats, run history, evaluates feats, stores a [`RunUnlockSummary`] and
//!   saves the profile atomically (when it has a disk path).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::player::Player;
use crate::state::{start_run, AppState, PlayerModifiers, RecordedRunTotals, RunMode, RunStats, UpgradeId};
use crate::trials::{DailyDate, DailyRecord};

/// Current on-disk profile schema.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;
/// Cards in the active App Store catalog drafted from during a run.
pub const ACTIVE_CATALOG_SIZE: usize = 12;
/// Runs kept in [`PlayerProfile::history`].
pub const RUN_HISTORY_LEN: usize = 8;
/// "5G Express" feat threshold (seconds of `Playing` time).
pub const SPEED_VICTORY_SECS: f32 = 600.0;
/// Cumulative thresholds for the lifetime feats.
pub const JAMMER_HUNTER_TOTAL: u32 = 8;
pub const GHOST_STATIC_TOTAL: u32 = 10;
pub const FIEND_BUSTER_TOTAL: u32 = 5;
pub const DEAD_ZONE_TANK_TOTAL: u32 = 3;
/// Starter chip stat deltas.
pub const SURVEY_OS_MAX_HP_DELTA: f32 = -15.0;
pub const FARADAY_ARMOR_DELTA: f32 = 25.0;

// ---------------------------------------------------------------------------
// Feats
// ---------------------------------------------------------------------------

/// Achievements that unlock chips, skins and signature apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FeatId {
    FirstExtraction,
    SignalScout,
    JammerHunter,
    GhostInTheStatic,
    FiendBuster,
    DeadZoneTank,
    BunkerCleared,
    ContractCleared,
    AllContractsCleared,
    SpeedExtraction,
}

impl FeatId {
    pub const ALL: [FeatId; 10] = [
        FeatId::FirstExtraction,
        FeatId::SignalScout,
        FeatId::JammerHunter,
        FeatId::GhostInTheStatic,
        FeatId::FiendBuster,
        FeatId::DeadZoneTank,
        FeatId::BunkerCleared,
        FeatId::ContractCleared,
        FeatId::AllContractsCleared,
        FeatId::SpeedExtraction,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FeatId::FirstExtraction => "First Uplink",
            FeatId::SignalScout => "Signal Scout",
            FeatId::JammerHunter => "Jammer Hunter",
            FeatId::GhostInTheStatic => "Ghost in the Static",
            FeatId::FiendBuster => "Fiend Buster",
            FeatId::DeadZoneTank => "Faraday Tested",
            FeatId::BunkerCleared => "Full Signal",
            FeatId::ContractCleared => "Carrier Compliant",
            FeatId::AllContractsCleared => "Zero-Bar Legend",
            FeatId::SpeedExtraction => "5G Express",
        }
    }

    pub fn requirement(self) -> &'static str {
        match self {
            FeatId::FirstExtraction => "Extract from Floor 1.",
            FeatId::SignalScout => "Reach Floor 3 in a single run.",
            FeatId::JammerHunter => "Destroy 8 Demon Jammers (lifetime).",
            FeatId::GhostInTheStatic => "Score 10 kills in natural 0-bar blackouts (lifetime).",
            FeatId::FiendBuster => "Kill 5 Jammer Fiends (lifetime).",
            FeatId::DeadZoneTank => "Complete 3 SOS uplinks (lifetime).",
            FeatId::BunkerCleared => "Clear Floor 5.",
            FeatId::ContractCleared => "Win a run with 1+ Carrier Contract active.",
            FeatId::AllContractsCleared => "Win a run with all 4 Carrier Contracts active.",
            FeatId::SpeedExtraction => "Win a run in under 10 minutes.",
        }
    }

    /// Signature apps licensed by this feat.
    pub fn reward_apps(self) -> Vec<UpgradeId> {
        UpgradeId::ALL.into_iter().filter(|u| u.unlock_feat() == Some(self)).collect()
    }

    pub fn reward_chips(self) -> Vec<StarterChip> {
        StarterChip::ALL.into_iter().filter(|c| c.unlock_feat() == Some(self)).collect()
    }

    pub fn reward_skins(self) -> Vec<HandsetSkin> {
        HandsetSkin::ALL.into_iter().filter(|s| s.unlock_feat() == Some(self)).collect()
    }

    /// Human-readable list of everything the feat unlocks.
    pub fn reward_summary(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.reward_chips().into_iter().map(|c| format!("{} chip", c.name())));
        parts.extend(self.reward_skins().into_iter().map(|s| format!("{} skin", s.name())));
        parts.extend(self.reward_apps().into_iter().map(|a| format!("{} app", a.name())));
        if parts.is_empty() { "Prestige badge".to_string() } else { parts.join(" + ") }
    }
}

// ---------------------------------------------------------------------------
// Starter OS chips
// ---------------------------------------------------------------------------

/// Run-start sidegrades: each upside carries a real trade-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum StarterChip {
    #[default]
    StockUac,
    SurveyOs,
    OverclockRadio,
    FaradayCase,
}

impl StarterChip {
    pub const ALL: [StarterChip; 4] = [StarterChip::StockUac, StarterChip::SurveyOs, StarterChip::OverclockRadio, StarterChip::FaradayCase];

    pub fn name(self) -> &'static str {
        match self {
            StarterChip::StockUac => "Stock UAC OS",
            StarterChip::SurveyOs => "Survey OS",
            StarterChip::OverclockRadio => "Overclock Radio",
            StarterChip::FaradayCase => "Faraday Case",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            StarterChip::StockUac => "STOCK",
            StarterChip::SurveyOs => "SURVEY",
            StarterChip::OverclockRadio => "OVRCLK",
            StarterChip::FaradayCase => "FARADAY",
        }
    }

    pub fn upside(self) -> &'static str {
        match self {
            StarterChip::StockUac => "Neutral baseline. No surprises.",
            StarterChip::SurveyOs => "-30% Ping cooldown, +25% wall signal penetration.",
            StarterChip::OverclockRadio => "+20% fire rate, +15% move speed at 4-5 bars.",
            StarterChip::FaradayCase => "+25 starting armor, -25% damage taken in 0-1 bar Dead Zones.",
        }
    }

    pub fn tradeoff(self) -> &'static str {
        match self {
            StarterChip::StockUac => "None.",
            StarterChip::SurveyOs => "-15 max HP (85 / 85).",
            StarterChip::OverclockRadio => "-25% pickup yield.",
            StarterChip::FaradayCase => "+25% Ping & 5G Overload cooldowns.",
        }
    }

    /// The feat that licenses this chip (`None` = available from the start).
    pub fn unlock_feat(self) -> Option<FeatId> {
        match self {
            StarterChip::StockUac => None,
            StarterChip::SurveyOs => Some(FeatId::SignalScout),
            StarterChip::OverclockRadio => Some(FeatId::BunkerCleared),
            StarterChip::FaradayCase => Some(FeatId::DeadZoneTank),
        }
    }

    /// Fold the chip's modifier effects into fresh run modifiers.
    pub fn apply_modifiers(self, mods: &mut PlayerModifiers) {
        match self {
            StarterChip::StockUac => {}
            StarterChip::SurveyOs => {
                mods.ping_cooldown_mult *= 0.70;
                mods.wall_penetration = 1.0 - (1.0 - mods.wall_penetration) * 0.75;
            }
            StarterChip::OverclockRadio => {
                mods.overclock_bonus += 0.20;
                mods.overclock_move_bonus += 0.15;
                mods.pickup_mult *= 0.75;
            }
            StarterChip::FaradayCase => {
                mods.dead_zone_damage_mult *= 0.75;
                mods.ping_cooldown_mult *= 1.25;
                mods.emp_cooldown_mult *= 1.25;
            }
        }
    }

    pub fn max_hp_delta(self) -> f32 {
        if self == StarterChip::SurveyOs { SURVEY_OS_MAX_HP_DELTA } else { 0.0 }
    }

    pub fn armor_delta(self) -> f32 {
        if self == StarterChip::FaradayCase { FARADAY_ARMOR_DELTA } else { 0.0 }
    }

    /// Starting vitals for a freshly spawned run player.
    pub fn apply_to_player(self, p: &mut Player) {
        p.max_hp = (p.max_hp + self.max_hp_delta()).max(1.0);
        p.hp = p.max_hp;
        p.armor = (p.armor + self.armor_delta()).clamp(0.0, p.max_armor);
    }
}

// ---------------------------------------------------------------------------
// Carrier contracts
// ---------------------------------------------------------------------------

/// Stackable opt-in difficulty modifiers (needed for the prestige feats).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CarrierContract {
    RoamingCharges,
    PriorityJamming,
    OvertimeUplink,
    LowBatteryMode,
}

impl CarrierContract {
    pub const ALL: [CarrierContract; 4] =
        [CarrierContract::RoamingCharges, CarrierContract::PriorityJamming, CarrierContract::OvertimeUplink, CarrierContract::LowBatteryMode];

    pub fn name(self) -> &'static str {
        match self {
            CarrierContract::RoamingCharges => "Roaming Charges",
            CarrierContract::PriorityJamming => "Priority Jamming",
            CarrierContract::OvertimeUplink => "Overtime Uplink",
            CarrierContract::LowBatteryMode => "Low-Battery Mode",
        }
    }

    pub fn short_tag(self) -> &'static str {
        match self {
            CarrierContract::RoamingCharges => "ROAM",
            CarrierContract::PriorityJamming => "PRIO",
            CarrierContract::OvertimeUplink => "OVRT",
            CarrierContract::LowBatteryMode => "LOBAT",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            CarrierContract::RoamingCharges => "+30% Jammer suppression radius; walls drop 20% more signal.",
            CarrierContract::PriorityJamming => "A Jammer Fiend crashes every SOS uplink; more Fiends in the rush.",
            CarrierContract::OvertimeUplink => "+35% SOS uplink duration; Demon Rush waves 20% faster.",
            CarrierContract::LowBatteryMode => "No HP field repair between floors (armor & ammo still top up).",
        }
    }

    pub fn apply_modifiers(self, mods: &mut PlayerModifiers) {
        match self {
            CarrierContract::RoamingCharges => {
                mods.jammer_radius_mult *= 1.30;
                mods.wall_attenuation_exp *= 1.20;
            }
            CarrierContract::PriorityJamming => mods.priority_jamming = true,
            CarrierContract::OvertimeUplink => {
                mods.upload_duration_mult *= 1.35;
                mods.wave_interval_mult *= 0.80;
            }
            CarrierContract::LowBatteryMode => mods.skip_floor_heal = true,
        }
    }
}

// ---------------------------------------------------------------------------
// Handset skins
// ---------------------------------------------------------------------------

/// One PBR slot of a handset skin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkinMaterial {
    pub color: Color,
    pub roughness: f32,
    pub metallic: f32,
}

impl SkinMaterial {
    pub const fn new(r: f32, g: f32, b: f32, roughness: f32, metallic: f32) -> Self {
        SkinMaterial { color: Color::srgb(r, g, b), roughness, metallic }
    }

    pub fn to_material(self) -> StandardMaterial {
        StandardMaterial { base_color: self.color, perceptual_roughness: self.roughness, metallic: self.metallic, ..default() }
    }
}

/// Exact colors of the in-hand phone chassis parts (also used by UI swatches).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkinPalette {
    pub bezel: SkinMaterial,
    pub frame: SkinMaterial,
    pub bumper: SkinMaterial,
    pub ptt: SkinMaterial,
    pub coil: SkinMaterial,
    pub bolts: SkinMaterial,
}

/// Which chassis part a [`SkinPalette`] slot styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkinSlot {
    Bezel,
    Frame,
    Bumper,
    Ptt,
    Coil,
    Bolts,
}

impl SkinPalette {
    pub fn slot(&self, slot: SkinSlot) -> SkinMaterial {
        match slot {
            SkinSlot::Bezel => self.bezel,
            SkinSlot::Frame => self.frame,
            SkinSlot::Bumper => self.bumper,
            SkinSlot::Ptt => self.ptt,
            SkinSlot::Coil => self.coil,
            SkinSlot::Bolts => self.bolts,
        }
    }
}

/// Cosmetic chassis/bumper skins for the 3D left-hand phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum HandsetSkin {
    #[default]
    UacOrange,
    MarsHazard,
    StealthCarbon,
    ChromeGold,
}

impl HandsetSkin {
    pub const ALL: [HandsetSkin; 4] = [HandsetSkin::UacOrange, HandsetSkin::MarsHazard, HandsetSkin::StealthCarbon, HandsetSkin::ChromeGold];

    pub fn name(self) -> &'static str {
        match self {
            HandsetSkin::UacOrange => "UAC Standard Orange",
            HandsetSkin::MarsHazard => "Mars Hazard Yellow",
            HandsetSkin::StealthCarbon => "Stealth Carbon",
            HandsetSkin::ChromeGold => "5G Chrome Gold",
        }
    }

    pub fn unlock_feat(self) -> Option<FeatId> {
        match self {
            HandsetSkin::UacOrange => None,
            HandsetSkin::MarsHazard => Some(FeatId::FirstExtraction),
            HandsetSkin::StealthCarbon => Some(FeatId::BunkerCleared),
            HandsetSkin::ChromeGold => Some(FeatId::ContractCleared),
        }
    }

    pub fn palette(self) -> SkinPalette {
        match self {
            // Dark bezel, titanium rails, tactical orange bumpers & PTT, copper coil.
            HandsetSkin::UacOrange => SkinPalette {
                bezel: SkinMaterial::new(0.07, 0.075, 0.085, 0.75, 0.35),
                frame: SkinMaterial::new(0.22, 0.24, 0.27, 0.42, 0.82),
                bumper: SkinMaterial::new(0.82, 0.42, 0.04, 0.62, 0.1),
                ptt: SkinMaterial::new(0.88, 0.52, 0.04, 0.5, 0.2),
                coil: SkinMaterial::new(0.62, 0.34, 0.16, 0.35, 0.85),
                bolts: SkinMaterial::new(0.68, 0.72, 0.76, 0.25, 0.95),
            },
            // Gunmetal bezel, caution-yellow bumpers & PTT, brushed steel rails, amber coil.
            HandsetSkin::MarsHazard => SkinPalette {
                bezel: SkinMaterial::new(0.13, 0.14, 0.15, 0.55, 0.7),
                frame: SkinMaterial::new(0.5, 0.52, 0.55, 0.32, 0.9),
                bumper: SkinMaterial::new(0.95, 0.76, 0.05, 0.55, 0.12),
                ptt: SkinMaterial::new(1.0, 0.82, 0.1, 0.45, 0.15),
                coil: SkinMaterial::new(0.9, 0.55, 0.12, 0.3, 0.85),
                bolts: SkinMaterial::new(0.6, 0.62, 0.64, 0.3, 0.9),
            },
            // Matte obsidian-carbon bezel, gunmetal bumpers, charcoal rails, cool cyan PTT & coil.
            HandsetSkin::StealthCarbon => SkinPalette {
                bezel: SkinMaterial::new(0.03, 0.032, 0.036, 0.88, 0.25),
                frame: SkinMaterial::new(0.1, 0.105, 0.115, 0.6, 0.6),
                bumper: SkinMaterial::new(0.16, 0.17, 0.19, 0.5, 0.7),
                ptt: SkinMaterial::new(0.1, 0.75, 0.9, 0.35, 0.3),
                coil: SkinMaterial::new(0.15, 0.8, 0.95, 0.3, 0.6),
                bolts: SkinMaterial::new(0.3, 0.32, 0.35, 0.35, 0.85),
            },
            // Polished chrome rails, bright satin gold bumpers & coil, prestige gold PTT, obsidian bezel.
            // Uses satin metallic (0.55..0.58) so highlights gleam cleanly under viewmodel point lights without an IBL map.
            HandsetSkin::ChromeGold => SkinPalette {
                bezel: SkinMaterial::new(0.05, 0.05, 0.06, 0.38, 0.45),
                frame: SkinMaterial::new(0.88, 0.91, 0.95, 0.22, 0.55),
                bumper: SkinMaterial::new(0.98, 0.78, 0.26, 0.24, 0.58),
                ptt: SkinMaterial::new(1.0, 0.86, 0.35, 0.20, 0.50),
                coil: SkinMaterial::new(0.96, 0.76, 0.28, 0.22, 0.58),
                bolts: SkinMaterial::new(1.0, 0.90, 0.50, 0.18, 0.55),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// App licenses
// ---------------------------------------------------------------------------

impl UpgradeId {
    /// The 12 core apps every profile owns from the start.
    pub fn is_core(self) -> bool {
        self.unlock_feat().is_none()
    }

    /// The feat licensing a signature app (`None` = core app).
    pub fn unlock_feat(self) -> Option<FeatId> {
        match self {
            UpgradeId::StoreAndForward => Some(FeatId::GhostInTheStatic),
            UpgradeId::EmergencyCallback => Some(FeatId::FiendBuster),
            UpgradeId::MissedCall => Some(FeatId::FirstExtraction),
            UpgradeId::SignalSpoofer => Some(FeatId::SignalScout),
            UpgradeId::JammerHijack => Some(FeatId::JammerHunter),
            UpgradeId::DualSimBarrels
            | UpgradeId::FiveGOverclock
            | UpgradeId::CopperAntenna
            | UpgradeId::SignalSiphon
            | UpgradeId::FirewallDaemon
            | UpgradeId::PacketSniffer
            | UpgradeId::EmpOvercharge
            | UpgradeId::AirplaneModePhase
            | UpgradeId::HighCapacityCell
            | UpgradeId::SubGhzPenetration
            | UpgradeId::DopplerOverdrive
            | UpgradeId::BlastThrusters => None,
        }
    }
}

/// The default active catalog: the 12 core apps in declaration order.
pub fn default_catalog() -> Vec<UpgradeId> {
    UpgradeId::ALL.into_iter().filter(|u| u.is_core()).collect()
}

// ---------------------------------------------------------------------------
// Run loadout
// ---------------------------------------------------------------------------

/// Snapshot of the equipped meta choices for the current run.
#[derive(Resource, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLoadout {
    pub chip: StarterChip,
    pub skin: HandsetSkin,
    pub contracts: Vec<CarrierContract>,
    /// Active App Store catalog (drafts never leave it).
    pub catalog: Vec<UpgradeId>,
    /// True if deployed with Jam Demo Unlock All enabled (exhibition run:
    /// stats and feats are not permanently recorded).
    #[serde(default)]
    pub jam_demo: bool,
}

impl Default for RunLoadout {
    fn default() -> Self {
        RunLoadout {
            chip: StarterChip::StockUac,
            skin: HandsetSkin::UacOrange,
            contracts: Vec::new(),
            catalog: default_catalog(),
            jam_demo: false,
        }
    }
}

impl RunLoadout {
    pub fn has_contract(&self, c: CarrierContract) -> bool {
        self.contracts.contains(&c)
    }

    /// Distinct contract count (duplicates never stack twice).
    pub fn contract_count(&self) -> u32 {
        self.contracts.iter().collect::<BTreeSet<_>>().len() as u32
    }
}

/// Fold chip + contract effects into (already reset) run modifiers.
pub fn apply_loadout_to_run(loadout: &RunLoadout, mods: &mut PlayerModifiers) {
    loadout.chip.apply_modifiers(mods);
    for c in loadout.contracts.iter().collect::<BTreeSet<_>>() {
        c.apply_modifiers(mods);
    }
}

/// [`start_run`] + install `loadout` as the run's [`RunLoadout`] and apply its
/// modifiers. Starting vitals (chip HP/armor) apply when the player spawns.
pub fn start_run_with_loadout(
    run: &mut RunStats,
    mods: &mut PlayerModifiers,
    current: &mut RunLoadout,
    loadout: RunLoadout,
    next: &mut NextState<AppState>,
    seed: u64,
) {
    start_run(run, mods, next, seed);
    *current = loadout;
    apply_loadout_to_run(current, mods);
}

// ---------------------------------------------------------------------------
// User settings (input, camera, audio, reticle)
// ---------------------------------------------------------------------------

/// HUD reticle shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CrosshairStyle {
    #[default]
    Cross,
    Dot,
    Circle,
    Chevron,
}

impl CrosshairStyle {
    pub const ALL: [CrosshairStyle; 4] = [CrosshairStyle::Cross, CrosshairStyle::Dot, CrosshairStyle::Circle, CrosshairStyle::Chevron];

    pub fn name(self) -> &'static str {
        match self {
            CrosshairStyle::Cross => "CROSS",
            CrosshairStyle::Dot => "DOT",
            CrosshairStyle::Circle => "CIRCLE",
            CrosshairStyle::Chevron => "CHEVRON",
        }
    }

    /// The next style in [`CrosshairStyle::ALL`] (wraps around).
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// HUD reticle tint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CrosshairColor {
    #[default]
    White,
    Cyan,
    AcidGreen,
    Amber,
    Crimson,
}

impl CrosshairColor {
    pub const ALL: [CrosshairColor; 5] =
        [CrosshairColor::White, CrosshairColor::Cyan, CrosshairColor::AcidGreen, CrosshairColor::Amber, CrosshairColor::Crimson];

    pub fn name(self) -> &'static str {
        match self {
            CrosshairColor::White => "WHITE",
            CrosshairColor::Cyan => "CYAN",
            CrosshairColor::AcidGreen => "ACID",
            CrosshairColor::Amber => "AMBER",
            CrosshairColor::Crimson => "CRIMSON",
        }
    }

    /// Reticle color (slightly translucent so it never hides a demon).
    pub fn color(self) -> Color {
        match self {
            CrosshairColor::White => Color::srgba(1.0, 0.94, 0.86, 0.88),
            CrosshairColor::Cyan => Color::srgba(0.3, 0.95, 1.0, 0.92),
            CrosshairColor::AcidGreen => Color::srgba(0.55, 1.0, 0.2, 0.92),
            CrosshairColor::Amber => Color::srgba(1.0, 0.72, 0.12, 0.92),
            CrosshairColor::Crimson => Color::srgba(1.0, 0.18, 0.12, 0.92),
        }
    }
}

/// Clamp ranges of the numeric [`UserSettings`].
pub const MOUSE_SENS_RANGE: (f32, f32) = (0.25, 3.0);
pub const BASE_FOV_RANGE: (f32, f32) = (70.0, 110.0);
pub const EFFECT_SCALE_RANGE: (f32, f32) = (0.0, 1.5);
pub const VOLUME_RANGE: (f32, f32) = (0.0, 1.0);

/// Player-facing options persisted inside the profile (never gameplay power).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    /// Multiplier on `player::MOUSE_SENSITIVITY`.
    pub mouse_sensitivity: f32,
    /// Resting vertical FOV in degrees.
    pub base_fov_deg: f32,
    /// Dynamic FOV widening with speed / dash.
    pub speed_fov_enabled: bool,
    pub speed_fov_scale: f32,
    /// Camera roll into strafes.
    pub strafe_roll_enabled: bool,
    pub strafe_roll_scale: f32,
    pub audio_master_volume: f32,
    pub audio_muted: bool,
    pub crosshair_style: CrosshairStyle,
    pub crosshair_color: CrosshairColor,
    /// Speedrun split timer plate on Standard runs (always on in Daily / Time Trial).
    #[serde(default)]
    pub speedrun_splits: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        UserSettings {
            mouse_sensitivity: 1.0,
            base_fov_deg: 85.0,
            speed_fov_enabled: true,
            speed_fov_scale: 1.0,
            strafe_roll_enabled: true,
            strafe_roll_scale: 1.0,
            audio_master_volume: 1.0,
            audio_muted: false,
            crosshair_style: CrosshairStyle::Cross,
            crosshair_color: CrosshairColor::White,
            speedrun_splits: false,
        }
    }
}

/// Clamp `v` into `(lo, hi)`, replacing NaN / infinities with `default`.
fn sane(v: f32, (lo, hi): (f32, f32), default: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { default }
}

impl UserSettings {
    /// Clamp every float into its range (NaN / Inf fall back to defaults).
    pub fn sanitize(&mut self) {
        let d = UserSettings::default();
        self.mouse_sensitivity = sane(self.mouse_sensitivity, MOUSE_SENS_RANGE, d.mouse_sensitivity);
        self.base_fov_deg = sane(self.base_fov_deg, BASE_FOV_RANGE, d.base_fov_deg);
        self.speed_fov_scale = sane(self.speed_fov_scale, EFFECT_SCALE_RANGE, d.speed_fov_scale);
        self.strafe_roll_scale = sane(self.strafe_roll_scale, EFFECT_SCALE_RANGE, d.strafe_roll_scale);
        self.audio_master_volume = sane(self.audio_master_volume, VOLUME_RANGE, d.audio_master_volume);
    }

    /// Sanitized copy.
    pub fn sanitized(&self) -> Self {
        let mut s = self.clone();
        s.sanitize();
        s
    }
}

/// The live settings: the profile's when a [`ProfileStore`] exists, defaults
/// otherwise (bare headless harnesses).
pub fn live_settings(store: Option<&ProfileStore>) -> UserSettings {
    store.map(|s| s.profile.settings.sanitized()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LifetimeStats {
    pub runs_played: u32,
    pub victories: u32,
    pub best_floor: u32,
    pub total_kills: u32,
    pub total_jammers: u32,
    pub total_fiends: u32,
    pub total_blackout_kills: u32,
    pub total_sos_completed: u32,
    pub fastest_victory_secs: Option<f32>,
    pub max_contracts_cleared: u32,
    /// Top horizontal speed ever reached (m/s).
    pub best_speed: f32,
    /// Longest bunny-hop chain ever linked.
    pub best_bhop_chain: u32,
    /// Deepest floor reached in Endless Overdrive (0 = never entered).
    pub best_endless_floor: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    pub floor: u32,
    pub victory: bool,
    pub kills: u32,
    pub jammers: u32,
    pub time_secs: f32,
    pub chip: StarterChip,
    pub contracts_count: u32,
    pub skin: HandsetSkin,
    /// The run continued past Floor 5 into Endless Overdrive.
    pub endless: bool,
    pub peak_speed: f32,
    pub max_bhop_chain: u32,
    /// UTC date of a Daily Seeded Run (`None` = standard run).
    #[serde(default)]
    pub daily_date: Option<DailyDate>,
}

/// What the last finished run unlocked (for the end screens / Milestone B).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunUnlockSummary {
    pub new_feats: Vec<FeatId>,
    pub new_apps: Vec<UpgradeId>,
    pub new_chips: Vec<StarterChip>,
    pub new_skins: Vec<HandsetSkin>,
    pub victory: bool,
    /// Present when the run was an exhibition (debug skip or Jam Demo) and
    /// therefore did not mutate lifetime stats or permanent feats.
    pub skipped_reason: Option<String>,
}

impl RunUnlockSummary {
    pub fn is_empty(&self) -> bool {
        self.new_feats.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerProfile {
    pub schema_version: u32,
    pub unlocked_feats: BTreeSet<FeatId>,
    pub equipped_chip: StarterChip,
    pub equipped_skin: HandsetSkin,
    pub active_contracts: BTreeSet<CarrierContract>,
    pub active_catalog: Vec<UpgradeId>,
    /// Jam/demo switch: everything counts as unlocked, earned feats untouched.
    pub jam_demo_unlock_all: bool,
    pub stats: LifetimeStats,
    /// The last [`RUN_HISTORY_LEN`] runs, oldest first.
    pub history: Vec<RunRecord>,
    /// Input / camera / audio / reticle options.
    pub settings: UserSettings,
    /// Daily Seeded Run results, oldest first (last [`crate::trials::DAILY_RECORDS_KEPT`] dates).
    #[serde(default)]
    pub daily_records: Vec<DailyRecord>,
    /// Cumulative floor splits of the fastest Standard 5-floor victory.
    #[serde(default)]
    pub standard_pb_splits: Vec<f32>,
    /// Fastest single-floor segment per floor across recorded Standard runs.
    #[serde(default)]
    pub best_floor_segments: [Option<f32>; 5],
    /// Fastest Time Trial completion (seconds).
    #[serde(default)]
    pub time_trial_best_secs: Option<f32>,
    /// Checkpoint + finish splits of that fastest Time Trial.
    #[serde(default)]
    pub time_trial_best_splits: Vec<f32>,
}

impl Default for PlayerProfile {
    fn default() -> Self {
        PlayerProfile {
            schema_version: PROFILE_SCHEMA_VERSION,
            unlocked_feats: BTreeSet::new(),
            equipped_chip: StarterChip::StockUac,
            equipped_skin: HandsetSkin::UacOrange,
            active_contracts: BTreeSet::new(),
            active_catalog: default_catalog(),
            jam_demo_unlock_all: false,
            stats: LifetimeStats::default(),
            history: Vec::new(),
            settings: UserSettings::default(),
            daily_records: Vec::new(),
            standard_pb_splits: Vec::new(),
            best_floor_segments: [None; 5],
            time_trial_best_secs: None,
            time_trial_best_splits: Vec::new(),
        }
    }
}

impl PlayerProfile {
    pub fn has_feat(&self, f: FeatId) -> bool {
        self.unlocked_feats.contains(&f)
    }

    fn licensed(&self, feat: Option<FeatId>) -> bool {
        self.jam_demo_unlock_all || feat.is_none_or(|f| self.has_feat(f))
    }

    pub fn is_upgrade_unlocked(&self, id: UpgradeId) -> bool {
        self.licensed(id.unlock_feat())
    }

    pub fn is_chip_unlocked(&self, chip: StarterChip) -> bool {
        self.licensed(chip.unlock_feat())
    }

    pub fn is_skin_unlocked(&self, skin: HandsetSkin) -> bool {
        self.licensed(skin.unlock_feat())
    }

    /// Equip a chip if it is unlocked. Returns whether it is now equipped.
    pub fn equip_chip(&mut self, chip: StarterChip) -> bool {
        if self.is_chip_unlocked(chip) {
            self.equipped_chip = chip;
        }
        self.equipped_chip == chip
    }

    pub fn equip_skin(&mut self, skin: HandsetSkin) -> bool {
        if self.is_skin_unlocked(skin) {
            self.equipped_skin = skin;
        }
        self.equipped_skin == skin
    }

    /// Flip a contract on/off. Returns whether it is now active.
    pub fn toggle_contract(&mut self, c: CarrierContract) -> bool {
        if !self.active_contracts.remove(&c) {
            self.active_contracts.insert(c);
        }
        self.active_contracts.contains(&c)
    }

    /// Distinct unlocked apps of the stored catalog (order kept, capped at 12).
    pub fn normalized_catalog(&self) -> Vec<UpgradeId> {
        let mut out: Vec<UpgradeId> = Vec::new();
        for &id in &self.active_catalog {
            if self.is_upgrade_unlocked(id) && !out.contains(&id) && out.len() < ACTIVE_CATALOG_SIZE {
                out.push(id);
            }
        }
        out
    }

    /// Add/remove an app from the active catalog. Locked apps and additions to
    /// a full catalog are refused. Returns whether the catalog changed.
    pub fn toggle_catalog_app(&mut self, id: UpgradeId) -> bool {
        let mut cat = self.normalized_catalog();
        let changed = if let Some(i) = cat.iter().position(|&u| u == id) {
            cat.remove(i);
            true
        } else if self.is_upgrade_unlocked(id) && cat.len() < ACTIVE_CATALOG_SIZE {
            cat.push(id);
            true
        } else {
            false
        };
        self.active_catalog = cat;
        changed
    }

    pub fn reset_default_catalog(&mut self) {
        self.active_catalog = default_catalog();
    }

    /// The catalog a run drafts from: the normalized selection padded
    /// deterministically with core apps up to [`ACTIVE_CATALOG_SIZE`], in
    /// declaration order.
    pub fn effective_catalog(&self) -> Vec<UpgradeId> {
        let mut cat = self.normalized_catalog();
        for core in default_catalog() {
            if cat.len() >= ACTIVE_CATALOG_SIZE {
                break;
            }
            if !cat.contains(&core) {
                cat.push(core);
            }
        }
        cat.truncate(ACTIVE_CATALOG_SIZE);
        cat.sort();
        cat
    }

    /// Toggle the jam/demo unlock switch. Turning it off clamps equipped
    /// choices and the catalog back to genuinely earned unlocks (feats kept).
    pub fn set_jam_demo_unlock_all(&mut self, on: bool) {
        self.jam_demo_unlock_all = on;
        if !on {
            self.sanitize();
        }
    }

    /// Clamp equipped choices / catalog to what is unlocked and stamp the schema.
    pub fn sanitize(&mut self) {
        self.schema_version = PROFILE_SCHEMA_VERSION;
        if !self.is_chip_unlocked(self.equipped_chip) {
            self.equipped_chip = StarterChip::StockUac;
        }
        if !self.is_skin_unlocked(self.equipped_skin) {
            self.equipped_skin = HandsetSkin::UacOrange;
        }
        self.active_catalog = self.normalized_catalog();
        self.settings.sanitize();
        let st = &mut self.stats;
        if !st.best_speed.is_finite() || st.best_speed < 0.0 {
            st.best_speed = 0.0;
        }
        if self.history.len() > RUN_HISTORY_LEN {
            let extra = self.history.len() - RUN_HISTORY_LEN;
            self.history.drain(..extra);
        }
        crate::trials::sanitize_trial_records(self);
    }

    /// Snapshot the equipped choices for a new run (locked picks fall back).
    pub fn build_loadout(&self) -> RunLoadout {
        RunLoadout {
            chip: if self.is_chip_unlocked(self.equipped_chip) { self.equipped_chip } else { StarterChip::StockUac },
            skin: if self.is_skin_unlocked(self.equipped_skin) { self.equipped_skin } else { HandsetSkin::UacOrange },
            contracts: self.active_contracts.iter().copied().collect(),
            catalog: self.effective_catalog(),
            jam_demo: self.jam_demo_unlock_all,
        }
    }

    /// Feats satisfied by the *current* lifetime stats plus this run's outcome.
    pub fn qualifying_feats(&self, run: &RunStats, loadout: &RunLoadout, victory: bool) -> Vec<FeatId> {
        let s = &self.stats;
        let contracts = loadout.contract_count();
        FeatId::ALL
            .into_iter()
            .filter(|f| match f {
                FeatId::FirstExtraction => run.floors_cleared >= 1,
                FeatId::SignalScout => run.floor >= 3,
                FeatId::JammerHunter => s.total_jammers >= JAMMER_HUNTER_TOTAL,
                FeatId::GhostInTheStatic => s.total_blackout_kills >= GHOST_STATIC_TOTAL,
                FeatId::FiendBuster => s.total_fiends >= FIEND_BUSTER_TOTAL,
                FeatId::DeadZoneTank => s.total_sos_completed >= DEAD_ZONE_TANK_TOTAL,
                FeatId::BunkerCleared => victory,
                FeatId::ContractCleared => victory && contracts >= 1,
                FeatId::AllContractsCleared => victory && contracts >= CarrierContract::ALL.len() as u32,
                FeatId::SpeedExtraction => victory && run.time_elapsed < SPEED_VICTORY_SECS,
            })
            .collect()
    }

    /// Fold a finished run into the profile: lifetime stats, history, feats.
    /// Returns what the run newly unlocked.
    pub fn record_run(&mut self, run: &RunStats, loadout: &RunLoadout, victory: bool) -> RunUnlockSummary {
        let contracts = loadout.contract_count();
        let s = &mut self.stats;
        s.runs_played += 1;
        s.total_kills += run.kills;
        s.total_jammers += run.jammers_destroyed;
        s.total_fiends += run.fiends_killed;
        s.total_blackout_kills += run.blackout_kills;
        s.total_sos_completed += run.sos_completed;
        if victory {
            s.victories += 1;
            s.fastest_victory_secs = Some(s.fastest_victory_secs.map_or(run.time_elapsed, |t| t.min(run.time_elapsed)));
            s.max_contracts_cleared = s.max_contracts_cleared.max(contracts);
        }
        self.record_bests(run);
        self.history.push(RunRecord {
            floor: run.floor,
            victory,
            kills: run.kills,
            jammers: run.jammers_destroyed,
            time_secs: run.time_elapsed,
            chip: loadout.chip,
            contracts_count: contracts,
            skin: loadout.skin,
            endless: run.endless,
            peak_speed: run.peak_speed,
            max_bhop_chain: run.max_bhop_chain,
            daily_date: run.mode.daily_date(),
        });
        if self.history.len() > RUN_HISTORY_LEN {
            let extra = self.history.len() - RUN_HISTORY_LEN;
            self.history.drain(..extra);
        }
        self.unlock_qualifying(run, loadout, victory)
    }

    /// Fold the Endless Overdrive continuation of an already-recorded victory:
    /// only the counters accumulated *after* the Floor 5 snapshot are added,
    /// `runs_played` / `victories` are untouched, the bests are refreshed and the
    /// run's history entry (appended at Victory) is updated in place.
    pub fn record_endless_extension(&mut self, run: &RunStats, snapshot: &RecordedRunTotals, loadout: &RunLoadout) -> RunUnlockSummary {
        let d = RecordedRunTotals::of(run).delta_since(snapshot);
        let s = &mut self.stats;
        s.total_kills += d.kills;
        s.total_jammers += d.jammers;
        s.total_fiends += d.fiends;
        s.total_blackout_kills += d.blackout_kills;
        s.total_sos_completed += d.sos_completed;
        self.record_bests(run);
        let contracts = loadout.contract_count();
        let rec = RunRecord {
            floor: run.floor,
            victory: true,
            kills: run.kills,
            jammers: run.jammers_destroyed,
            time_secs: run.time_elapsed,
            chip: loadout.chip,
            contracts_count: contracts,
            skin: loadout.skin,
            endless: true,
            peak_speed: run.peak_speed,
            max_bhop_chain: run.max_bhop_chain,
            daily_date: run.mode.daily_date(),
        };
        match self.history.last_mut() {
            Some(last) if last.victory && last.chip == rec.chip && last.skin == rec.skin && last.floor <= rec.floor => *last = rec,
            _ => {
                self.history.push(rec);
                if self.history.len() > RUN_HISTORY_LEN {
                    let extra = self.history.len() - RUN_HISTORY_LEN;
                    self.history.drain(..extra);
                }
            }
        }
        // Victory-only feats were already granted at Floor 5.
        self.unlock_qualifying(run, loadout, false)
    }

    /// Best floor / endless floor / speed / bhop chain.
    fn record_bests(&mut self, run: &RunStats) {
        let s = &mut self.stats;
        s.best_floor = s.best_floor.max(run.floor);
        if run.endless {
            s.best_endless_floor = s.best_endless_floor.max(run.floor);
        }
        if run.peak_speed.is_finite() {
            s.best_speed = s.best_speed.max(run.peak_speed);
        }
        s.best_bhop_chain = s.best_bhop_chain.max(run.max_bhop_chain);
    }

    fn unlock_qualifying(&mut self, run: &RunStats, loadout: &RunLoadout, victory: bool) -> RunUnlockSummary {
        let new_feats: Vec<FeatId> = self.qualifying_feats(run, loadout, victory).into_iter().filter(|f| !self.has_feat(*f)).collect();
        self.unlocked_feats.extend(new_feats.iter().copied());
        RunUnlockSummary {
            new_apps: new_feats.iter().flat_map(|f| f.reward_apps()).collect(),
            new_chips: new_feats.iter().flat_map(|f| f.reward_chips()).collect(),
            new_skins: new_feats.iter().flat_map(|f| f.reward_skins()).collect(),
            new_feats,
            victory,
            skipped_reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Parse a profile; any error (corrupt JSON, unknown variant, future schema) yields `None`.
pub fn parse_profile(json: &str) -> Option<PlayerProfile> {
    let mut p: PlayerProfile = serde_json::from_str(json).ok()?;
    if p.schema_version > PROFILE_SCHEMA_VERSION {
        return None;
    }
    p.sanitize();
    Some(p)
}

/// Copy an unreadable or incompatible profile aside (`<file>.bak`, `.bak.1`, ...)
/// without clobbering any earlier backup, so user data is never silently lost.
pub fn backup_corrupt_profile(path: &Path) -> Option<PathBuf> {
    let base = path.file_name()?.to_string_lossy();
    for idx in 0..100u32 {
        let name = if idx == 0 { format!("{base}.bak") } else { format!("{base}.bak.{idx}") };
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return std::fs::copy(path, &candidate).ok().map(|_| candidate);
        }
    }
    None
}

/// Load a profile from disk, falling back to the default when the file is
/// missing or unreadable.
pub fn load_profile(path: &Path) -> PlayerProfile {
    std::fs::read_to_string(path).ok().and_then(|s| parse_profile(&s)).unwrap_or_default()
}

/// Atomic save: write `<file>.tmp` next to the target, then rename over it.
pub fn save_profile(path: &Path, profile: &PlayerProfile) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(profile).map_err(io::Error::other)?;
    let mut tmp_name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "profile.json".into());
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Pure path policy (see [`resolve_profile_path`]).
pub fn profile_path_policy(
    env_profile: Option<&str>,
    windowed: bool,
    bot_or_shots: bool,
    xdg_data_home: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    if let Some(v) = env_profile {
        let v = v.trim();
        return match v.to_ascii_lowercase().as_str() {
            "" | "memory" | "none" => None,
            _ => Some(PathBuf::from(v)),
        };
    }
    if !windowed || bot_or_shots {
        return None;
    }
    let base = xdg_data_home
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.filter(|s| !s.is_empty()).map(|h| Path::new(h).join(".local").join("share")))?;
    Some(base.join("segnale-debole").join("profile.json"))
}

/// Where the profile lives: `SEGNALE_PROFILE` (`memory`/`none` = in-memory)
/// overrides; otherwise only windowed, non-bot, non-screenshot (`SEGNALE_SHOTS`,
/// `SEGNALE_META_SHOTS`) launches touch
/// `$XDG_DATA_HOME/segnale-debole/profile.json` (or `~/.local/share/...`).
pub fn resolve_profile_path(windowed: bool) -> Option<PathBuf> {
    let var = |k: &str| std::env::var(k).ok();
    let bot_or_shots = ["SEGNALE_BOT_TEST", "SEGNALE_SHOTS", "SEGNALE_META_SHOTS", "SEGNALE_TRIAL_SHOTS"].into_iter().any(|k| var(k).is_some());
    profile_path_policy(var("SEGNALE_PROFILE").as_deref(), windowed, bot_or_shots, var("XDG_DATA_HOME").as_deref(), var("HOME").as_deref())
}

/// The live profile, its disk location (`None` = in-memory only) and the
/// summary of the last finished run.
#[derive(Resource, Debug, Clone, Default)]
pub struct ProfileStore {
    pub schema_version: u32,
    pub profile: PlayerProfile,
    pub path: Option<PathBuf>,
    pub last_run_summary: Option<RunUnlockSummary>,
    /// True when the in-memory profile has changes not yet flushed to disk.
    pub dirty: bool,
    /// Last disk-save error (cleared automatically once a retry succeeds).
    pub last_save_error: Option<String>,
    /// Set when an existing file could not be parsed and was backed up aside.
    pub recovery_notice: Option<String>,
}

impl ProfileStore {
    pub fn in_memory(profile: PlayerProfile) -> Self {
        ProfileStore {
            schema_version: PROFILE_SCHEMA_VERSION,
            profile,
            path: None,
            last_run_summary: None,
            dirty: false,
            last_save_error: None,
            recovery_notice: None,
        }
    }

    /// Load from `path`. Missing files start fresh; unreadable or incompatible
    /// files are backed up aside so a later save never silently destroys them.
    pub fn open(path: Option<PathBuf>) -> Self {
        let mut recovery_notice = None;
        let profile = match path.as_deref() {
            None => PlayerProfile::default(),
            Some(p) if !p.exists() => PlayerProfile::default(),
            Some(p) => match std::fs::read_to_string(p).ok().and_then(|s| parse_profile(&s)) {
                Some(loaded) => loaded,
                None => {
                    let msg = match backup_corrupt_profile(p) {
                        Some(bak) => {
                            let name = bak.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "backup".into());
                            format!("Incompatible profile backed up to {name} - started fresh")
                        }
                        None => "Incompatible profile unreadable - started fresh".to_string(),
                    };
                    warn!("{msg}");
                    recovery_notice = Some(msg);
                    PlayerProfile::default()
                }
            },
        };
        ProfileStore {
            schema_version: PROFILE_SCHEMA_VERSION,
            profile,
            path,
            last_run_summary: None,
            dirty: false,
            last_save_error: None,
            recovery_notice,
        }
    }

    /// Persist the profile (no-op for in-memory stores).
    pub fn save(&self) -> io::Result<()> {
        match &self.path {
            Some(p) => save_profile(p, &self.profile),
            None => Ok(()),
        }
    }

    /// Persist the profile and track dirty / error state so failed writes can
    /// be retried later without reapplying edits or recounting runs.
    pub fn try_save(&mut self) -> io::Result<()> {
        match self.save() {
            Ok(()) => {
                self.dirty = false;
                self.last_save_error = None;
                Ok(())
            }
            Err(e) => {
                self.dirty = true;
                self.last_save_error = Some(e.to_string());
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ECS
// ---------------------------------------------------------------------------

/// Fold the finished run into the profile once (guarded by `run_recorded`).
/// Debug-skipped runs and Jam Demo runs are treated as exhibitions: they report
/// why recording was skipped and never mutate permanent stats or feats.
pub fn record_run_outcome(victory: bool, run: &mut RunStats, loadout: &RunLoadout, store: &mut ProfileStore) -> Option<RunUnlockSummary> {
    if run.run_recorded {
        return None;
    }
    run.run_recorded = true;
    if run.mode == RunMode::TimeTrial {
        // Time Trial completions fold their own movement records in
        // (`trials::record_time_trial`); they never count as runs or feats.
        let summary = RunUnlockSummary { victory, skipped_reason: Some(TIME_TRIAL_SUMMARY.to_string()), ..Default::default() };
        store.last_run_summary = Some(summary.clone());
        return Some(summary);
    }
    let exhibition = if run.debug_skipped {
        Some("EXHIBITION: Debug skip (F9) used - stats & feats not recorded")
    } else if loadout.jam_demo {
        Some("EXHIBITION: JAM DEMO active - stats & feats not recorded")
    } else {
        None
    };
    if let Some(reason) = exhibition {
        let summary = RunUnlockSummary { victory, skipped_reason: Some(reason.to_string()), ..Default::default() };
        store.last_run_summary = Some(summary.clone());
        return Some(summary);
    }
    let first_record = run.recorded_snapshot.is_none();
    let summary = match run.recorded_snapshot {
        // Endless continuation of a victory that was already folded in.
        Some(snap) => store.profile.record_endless_extension(run, &snap, loadout),
        None => store.profile.record_run(run, loadout, victory),
    };
    match run.mode {
        RunMode::Daily(date) => crate::trials::record_daily_result(&mut store.profile.daily_records, date, run, victory),
        RunMode::Standard if first_record => crate::trials::record_standard_splits(&mut store.profile, &run.floor_splits, victory),
        _ => {}
    }
    if victory {
        run.recorded_snapshot = Some(RecordedRunTotals::of(run));
    }
    store.last_run_summary = Some(summary.clone());
    if let Err(e) = store.try_save() {
        warn!("profile save failed (marked dirty for retry): {e}");
    }
    Some(summary)
}

/// Run-unlock note of a Time Trial finish (no lifetime runs / feats).
pub const TIME_TRIAL_SUMMARY: &str = "TIME TRIAL: movement records only - runs & feats untouched";

/// `OnEnter(GameOver)`: the end screen orders itself after this system so
/// [`ProfileStore::last_run_summary`] is populated when it spawns.
pub fn record_game_over(mut run: ResMut<RunStats>, loadout: Res<RunLoadout>, mut store: ResMut<ProfileStore>) {
    record_run_outcome(false, &mut run, &loadout, &mut store);
}

/// `OnEnter(Victory)`: see [`record_game_over`].
pub fn record_victory(mut run: ResMut<RunStats>, loadout: Res<RunLoadout>, mut store: ResMut<ProfileStore>) {
    record_run_outcome(true, &mut run, &loadout, &mut store);
}

/// Retry flushing any unsaved in-memory profile snapshot when returning to menus.
fn retry_dirty_save(mut store: ResMut<ProfileStore>) {
    if store.dirty
        && let Err(e) = store.try_save()
    {
        warn!("profile retry save failed: {e}");
    }
}

/// Back at the main menu the previous run's loadout no longer applies.
fn reset_run_loadout(mut loadout: ResMut<RunLoadout>) {
    *loadout = RunLoadout::default();
}

/// Profile + run loadout + run recording. The store starts in-memory unless
/// `SEGNALE_PROFILE` names a file; the windowed `GamePlugin` swaps in the
/// on-disk profile.
pub struct MetaPlugin;

impl Plugin for MetaPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<ProfileStore>() {
            app.insert_resource(ProfileStore::open(resolve_profile_path(false)));
        }
        app.init_resource::<RunLoadout>()
            .add_systems(OnEnter(AppState::MainMenu), (reset_run_loadout, retry_dirty_save))
            .add_systems(OnEnter(AppState::Loadout), retry_dirty_save)
            .add_systems(OnEnter(AppState::ContractsFeats), retry_dirty_save)
            .add_systems(OnEnter(AppState::GameOver), record_game_over)
            .add_systems(OnEnter(AppState::Victory), record_victory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-profiles").join(name);
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn run_with(floor: u32) -> RunStats {
        RunStats { floor, ..default() }
    }

    #[test]
    fn catalog_has_twelve_core_and_five_signature_apps() {
        assert_eq!(UpgradeId::ALL.len(), 17);
        let core = default_catalog();
        assert_eq!(core.len(), ACTIVE_CATALOG_SIZE);
        assert_eq!(core.first(), Some(&UpgradeId::DualSimBarrels));
        assert_eq!(core.last(), Some(&UpgradeId::BlastThrusters));
        assert!(core.contains(&UpgradeId::DopplerOverdrive));
        let sig: Vec<UpgradeId> = UpgradeId::ALL.into_iter().filter(|u| !u.is_core()).collect();
        assert_eq!(
            sig,
            vec![UpgradeId::StoreAndForward, UpgradeId::EmergencyCallback, UpgradeId::MissedCall, UpgradeId::SignalSpoofer, UpgradeId::JammerHijack]
        );
        for f in FeatId::ALL {
            assert!(!f.name().is_empty() && !f.requirement().is_empty() && !f.reward_summary().is_empty());
        }
        assert_eq!(FeatId::FirstExtraction.reward_apps(), vec![UpgradeId::MissedCall]);
        assert_eq!(FeatId::FirstExtraction.reward_skins(), vec![HandsetSkin::MarsHazard]);
        assert_eq!(FeatId::SignalScout.reward_chips(), vec![StarterChip::SurveyOs]);
        assert_eq!(FeatId::SignalScout.reward_apps(), vec![UpgradeId::SignalSpoofer]);
        assert_eq!(FeatId::BunkerCleared.reward_chips(), vec![StarterChip::OverclockRadio]);
        assert_eq!(FeatId::BunkerCleared.reward_skins(), vec![HandsetSkin::StealthCarbon]);
        assert_eq!(FeatId::AllContractsCleared.reward_summary(), "Prestige badge");
        assert_eq!(FeatId::SpeedExtraction.reward_summary(), "Prestige badge");
        assert!(FeatId::BunkerCleared.reward_summary().contains("Overclock Radio"));
    }

    #[test]
    fn default_profile_owns_only_baseline() {
        let p = PlayerProfile::default();
        assert_eq!(p.schema_version, PROFILE_SCHEMA_VERSION);
        for id in UpgradeId::ALL {
            assert_eq!(p.is_upgrade_unlocked(id), id.is_core(), "{id:?}");
        }
        for c in StarterChip::ALL {
            assert_eq!(p.is_chip_unlocked(c), c == StarterChip::StockUac);
            assert!(!c.name().is_empty() && !c.short_name().is_empty() && !c.upside().is_empty() && !c.tradeoff().is_empty());
        }
        for s in HandsetSkin::ALL {
            assert_eq!(p.is_skin_unlocked(s), s == HandsetSkin::UacOrange);
        }
        for c in CarrierContract::ALL {
            assert!(!c.name().is_empty() && !c.short_tag().is_empty() && !c.description().is_empty());
        }
        let l = p.build_loadout();
        assert_eq!(l, RunLoadout::default());
        let mut m = PlayerModifiers::default();
        apply_loadout_to_run(&l, &mut m);
        assert_eq!(m, PlayerModifiers::default(), "default loadout is neutral");
    }

    #[test]
    fn starter_chips_are_sidegrades() {
        let mods = |c: StarterChip| {
            let mut m = PlayerModifiers::default();
            c.apply_modifiers(&mut m);
            m
        };
        let vit = |c: StarterChip| {
            let mut p = Player::default();
            c.apply_to_player(&mut p);
            p
        };
        assert_eq!(mods(StarterChip::StockUac), PlayerModifiers::default());
        assert_eq!((vit(StarterChip::StockUac).max_hp, vit(StarterChip::StockUac).armor), (100.0, 25.0));

        let s = mods(StarterChip::SurveyOs);
        assert!((s.ping_cooldown_mult - 0.70).abs() < 1e-6);
        assert!((s.wall_penetration - 0.25).abs() < 1e-6);
        let sp = vit(StarterChip::SurveyOs);
        assert_eq!((sp.hp, sp.max_hp), (85.0, 85.0), "Survey OS starts at 85/85");

        let o = mods(StarterChip::OverclockRadio);
        assert!((o.overclock_bonus - 0.20).abs() < 1e-6);
        assert!((o.overclock_move_bonus - 0.15).abs() < 1e-6);
        assert!((o.pickup_mult - 0.75).abs() < 1e-6);
        assert_eq!(vit(StarterChip::OverclockRadio).max_hp, 100.0);

        let f = mods(StarterChip::FaradayCase);
        assert!((f.dead_zone_damage_mult - 0.75).abs() < 1e-6);
        assert!((f.ping_cooldown_mult - 1.25).abs() < 1e-6);
        assert!((f.emp_cooldown_mult - 1.25).abs() < 1e-6);
        assert_eq!(vit(StarterChip::FaradayCase).armor, 50.0);
        assert_eq!(vit(StarterChip::FaradayCase).max_hp, 100.0);

        assert_eq!(StarterChip::SurveyOs.unlock_feat(), Some(FeatId::SignalScout));
        assert_eq!(StarterChip::OverclockRadio.unlock_feat(), Some(FeatId::BunkerCleared));
        assert_eq!(StarterChip::FaradayCase.unlock_feat(), Some(FeatId::DeadZoneTank));
    }

    #[test]
    fn contracts_set_their_modifiers_and_never_stack_twice() {
        let mut m = PlayerModifiers::default();
        let l = RunLoadout { contracts: CarrierContract::ALL.to_vec(), ..default() };
        apply_loadout_to_run(&l, &mut m);
        assert!((m.jammer_radius_mult - 1.30).abs() < 1e-6);
        assert!((m.wall_attenuation_exp - 1.20).abs() < 1e-6);
        assert!(m.priority_jamming);
        assert!((m.upload_duration_mult - 1.35).abs() < 1e-6);
        assert!((m.wave_interval_mult - 0.80).abs() < 1e-6);
        assert!(m.skip_floor_heal);
        // Duplicate entries apply once.
        let dup = RunLoadout { contracts: vec![CarrierContract::RoamingCharges; 3], ..default() };
        let mut m2 = PlayerModifiers::default();
        apply_loadout_to_run(&dup, &mut m2);
        assert!((m2.jammer_radius_mult - 1.30).abs() < 1e-6);
        assert_eq!(dup.contract_count(), 1);
    }

    #[test]
    fn skins_have_distinct_palettes() {
        let uac = HandsetSkin::UacOrange.palette();
        assert_eq!(uac.bumper.color, Color::srgb(0.82, 0.42, 0.04), "default keeps the original orange bumpers");
        for (i, a) in HandsetSkin::ALL.iter().enumerate() {
            for b in &HandsetSkin::ALL[i + 1..] {
                assert_ne!(a.palette().bumper.color, b.palette().bumper.color, "{a:?} vs {b:?}");
            }
            let p = a.palette();
            for slot in [SkinSlot::Bezel, SkinSlot::Frame, SkinSlot::Bumper, SkinSlot::Ptt, SkinSlot::Coil, SkinSlot::Bolts] {
                let s = p.slot(slot);
                assert!((0.0..=1.0).contains(&s.roughness) && (0.0..=1.0).contains(&s.metallic));
            }
        }
        assert_eq!(HandsetSkin::ChromeGold.unlock_feat(), Some(FeatId::ContractCleared));
    }

    #[test]
    fn catalog_toggle_normalizes_and_pads() {
        let mut p = PlayerProfile::default();
        // Full catalog: a locked signature app is refused, so is a 11th card.
        assert!(!p.toggle_catalog_app(UpgradeId::SignalSpoofer), "locked");
        p.unlocked_feats.insert(FeatId::SignalScout);
        assert!(!p.toggle_catalog_app(UpgradeId::SignalSpoofer), "catalog full");
        assert!(p.toggle_catalog_app(UpgradeId::DualSimBarrels), "remove");
        assert_eq!(p.normalized_catalog().len(), ACTIVE_CATALOG_SIZE - 1);
        assert!(p.toggle_catalog_app(UpgradeId::SignalSpoofer), "swap in");
        let eff = p.effective_catalog();
        assert_eq!(eff.len(), ACTIVE_CATALOG_SIZE);
        assert!(eff.contains(&UpgradeId::SignalSpoofer) && !eff.contains(&UpgradeId::DualSimBarrels));

        // Junk on disk: duplicates + locked entries are dropped, then padded.
        p.active_catalog = vec![UpgradeId::JammerHijack, UpgradeId::SignalSpoofer, UpgradeId::SignalSpoofer, UpgradeId::CopperAntenna];
        assert_eq!(p.normalized_catalog(), vec![UpgradeId::SignalSpoofer, UpgradeId::CopperAntenna]);
        let eff = p.effective_catalog();
        assert_eq!(eff.len(), ACTIVE_CATALOG_SIZE);
        assert!(eff.contains(&UpgradeId::SignalSpoofer));
        assert!(!eff.contains(&UpgradeId::JammerHijack));
        // Padding is deterministic core-first: the 11 lowest core apps besides Copper Antenna.
        let pad: Vec<UpgradeId> = eff.iter().copied().filter(|u| u.is_core()).collect();
        assert_eq!(pad.len(), 11);
        assert!(!pad.contains(&UpgradeId::BlastThrusters));
        assert_eq!(p.effective_catalog(), eff, "deterministic");

        // Oversized catalogs are capped.
        p.set_jam_demo_unlock_all(true);
        p.active_catalog = UpgradeId::ALL.to_vec();
        assert_eq!(p.effective_catalog().len(), ACTIVE_CATALOG_SIZE);
        p.reset_default_catalog();
        assert_eq!(p.effective_catalog(), default_catalog());
    }

    #[test]
    fn jam_demo_unlock_all_respects_earned_feats() {
        let mut p = PlayerProfile::default();
        p.unlocked_feats.insert(FeatId::FirstExtraction);
        p.set_jam_demo_unlock_all(true);
        assert!(StarterChip::ALL.iter().all(|c| p.is_chip_unlocked(*c)));
        assert!(HandsetSkin::ALL.iter().all(|s| p.is_skin_unlocked(*s)));
        assert!(UpgradeId::ALL.iter().all(|u| p.is_upgrade_unlocked(*u)));
        assert_eq!(p.unlocked_feats.len(), 1, "demo switch never grants feats");
        assert!(p.equip_chip(StarterChip::FaradayCase));
        assert!(p.equip_skin(HandsetSkin::ChromeGold));
        p.toggle_catalog_app(UpgradeId::DualSimBarrels);
        p.toggle_catalog_app(UpgradeId::FiveGOverclock);
        assert!(p.toggle_catalog_app(UpgradeId::JammerHijack));
        assert!(p.toggle_catalog_app(UpgradeId::MissedCall));
        assert_eq!(p.build_loadout().chip, StarterChip::FaradayCase);

        p.set_jam_demo_unlock_all(false);
        assert_eq!(p.unlocked_feats, BTreeSet::from([FeatId::FirstExtraction]));
        assert_eq!(p.equipped_chip, StarterChip::StockUac, "unearned chip clamped");
        assert_eq!(p.equipped_skin, HandsetSkin::UacOrange, "unearned skin clamped");
        assert!(!p.active_catalog.contains(&UpgradeId::JammerHijack), "unearned app dropped");
        assert!(p.active_catalog.contains(&UpgradeId::MissedCall), "earned app kept");
        // Earned skin survives the clamp.
        assert!(p.equip_skin(HandsetSkin::MarsHazard));
        p.set_jam_demo_unlock_all(true);
        p.set_jam_demo_unlock_all(false);
        assert_eq!(p.equipped_skin, HandsetSkin::MarsHazard);
        assert!(!p.equip_chip(StarterChip::SurveyOs));
    }

    #[test]
    fn game_over_on_floor_one_records_without_feats() {
        let mut p = PlayerProfile::default();
        let run = RunStats { kills: 4, jammers_destroyed: 1, time_elapsed: 50.0, ..run_with(1) };
        let s = p.record_run(&run, &RunLoadout::default(), false);
        assert!(s.is_empty() && !s.victory);
        assert_eq!(p.stats.runs_played, 1);
        assert_eq!(p.stats.total_kills, 4);
        assert_eq!(p.stats.best_floor, 1);
        assert_eq!(p.stats.victories, 0);
        assert_eq!(p.stats.fastest_victory_secs, None);
        assert_eq!(p.history.len(), 1);
        assert_eq!(p.history[0].kills, 4);
    }

    #[test]
    fn feats_unlock_from_run_outcomes() {
        let mut p = PlayerProfile::default();
        // Died on Floor 3 after clearing two floors.
        let run = RunStats { floors_cleared: 2, sos_completed: 2, ..run_with(3) };
        let s = p.record_run(&run, &RunLoadout::default(), false);
        assert_eq!(s.new_feats, vec![FeatId::FirstExtraction, FeatId::SignalScout]);
        assert_eq!(s.new_apps, vec![UpgradeId::MissedCall, UpgradeId::SignalSpoofer]);
        assert_eq!(s.new_chips, vec![StarterChip::SurveyOs]);
        assert_eq!(s.new_skins, vec![HandsetSkin::MarsHazard]);
        assert!(p.is_upgrade_unlocked(UpgradeId::SignalSpoofer));
        // Cumulative feats: the second run pushes SOS uplinks to 4 >= 3.
        assert!(!p.has_feat(FeatId::DeadZoneTank));
        let s = p.record_run(&run, &RunLoadout::default(), false);
        assert_eq!(s.new_feats, vec![FeatId::DeadZoneTank]);
        assert_eq!(s.new_chips, vec![StarterChip::FaradayCase]);
        assert!(p.is_chip_unlocked(StarterChip::FaradayCase));
        // Repeating it unlocks nothing new.
        assert!(p.record_run(&run, &RunLoadout::default(), false).is_empty());

        // Cumulative jammers / blackout kills / fiends.
        let grind = RunStats { jammers_destroyed: 8, blackout_kills: 10, fiends_killed: 5, ..run_with(1) };
        let s = p.record_run(&grind, &RunLoadout::default(), false);
        assert_eq!(s.new_feats, vec![FeatId::JammerHunter, FeatId::GhostInTheStatic, FeatId::FiendBuster]);
        assert_eq!(s.new_apps, vec![UpgradeId::JammerHijack, UpgradeId::StoreAndForward, UpgradeId::EmergencyCallback]);

        // Contracted fast victory: Full Signal + Carrier Compliant + 5G Express.
        let win = RunStats { floors_cleared: 5, time_elapsed: 540.0, ..run_with(5) };
        let l = RunLoadout { contracts: vec![CarrierContract::LowBatteryMode], ..default() };
        let s = p.record_run(&win, &l, true);
        assert_eq!(s.new_feats, vec![FeatId::BunkerCleared, FeatId::ContractCleared, FeatId::SpeedExtraction]);
        assert_eq!(s.new_skins, vec![HandsetSkin::StealthCarbon, HandsetSkin::ChromeGold]);
        assert_eq!(s.new_chips, vec![StarterChip::OverclockRadio]);
        assert!(s.victory);
        assert_eq!(p.stats.victories, 1);
        assert_eq!(p.stats.fastest_victory_secs, Some(540.0));
        assert_eq!(p.stats.max_contracts_cleared, 1);

        // All four contracts: the Zero-Bar Legend badge; slow win = no new 5G Express.
        let slow = RunStats { time_elapsed: 900.0, ..win.clone() };
        let all = RunLoadout { contracts: CarrierContract::ALL.to_vec(), ..default() };
        let s = p.record_run(&slow, &all, true);
        assert_eq!(s.new_feats, vec![FeatId::AllContractsCleared]);
        assert_eq!(p.stats.fastest_victory_secs, Some(540.0));
        assert_eq!(p.stats.max_contracts_cleared, 4);
        assert_eq!(p.unlocked_feats.len(), FeatId::ALL.len());
    }

    #[test]
    fn history_keeps_last_eight() {
        let mut p = PlayerProfile::default();
        for k in 0..12 {
            p.record_run(&RunStats { kills: k, ..run_with(1) }, &RunLoadout::default(), false);
        }
        assert_eq!(p.history.len(), RUN_HISTORY_LEN);
        assert_eq!(p.history.first().unwrap().kills, 4);
        assert_eq!(p.history.last().unwrap().kills, 11);
        assert_eq!(p.stats.runs_played, 12);
    }

    #[test]
    fn record_run_outcome_is_guarded() {
        let mut store = ProfileStore::in_memory(PlayerProfile::default());
        let mut run = RunStats { floors_cleared: 1, ..run_with(2) };
        let l = RunLoadout::default();
        let s = record_run_outcome(false, &mut run, &l, &mut store).unwrap();
        assert_eq!(s.new_feats, vec![FeatId::FirstExtraction]);
        assert!(run.run_recorded);
        assert!(record_run_outcome(true, &mut run, &l, &mut store).is_none(), "never recorded twice");
        assert_eq!(store.profile.stats.runs_played, 1);
        assert_eq!(store.last_run_summary.as_ref().unwrap().new_feats, vec![FeatId::FirstExtraction]);
        run.start_new_run(9);
        assert!(!run.run_recorded, "a new run can be recorded again");
    }

    #[test]
    fn json_round_trip_and_atomic_save() {
        let dir = test_dir("round_trip");
        let path = dir.join("nested").join("profile.json");
        let mut p = PlayerProfile::default();
        p.unlocked_feats.extend([FeatId::SignalScout, FeatId::BunkerCleared]);
        p.equip_chip(StarterChip::SurveyOs);
        p.equip_skin(HandsetSkin::StealthCarbon);
        p.toggle_contract(CarrierContract::OvertimeUplink);
        p.toggle_catalog_app(UpgradeId::HighCapacityCell);
        p.toggle_catalog_app(UpgradeId::SignalSpoofer);
        let l = p.build_loadout();
        p.record_run(&RunStats { kills: 7, time_elapsed: 321.5, ..run_with(4) }, &l, false);
        save_profile(&path, &p).unwrap();
        assert!(path.exists());
        assert!(!dir.join("nested").join("profile.json.tmp").exists(), "tmp file renamed away");
        let back = load_profile(&path);
        assert_eq!(back, p);
        assert_eq!(back.build_loadout().catalog, p.build_loadout().catalog);

        let mut store = ProfileStore::open(Some(path.clone()));
        assert_eq!(store.profile, p);
        store.profile.toggle_contract(CarrierContract::LowBatteryMode);
        store.save().unwrap();
        assert!(load_profile(&path).active_contracts.contains(&CarrierContract::LowBatteryMode));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_or_partial_json_falls_back() {
        let dir = test_dir("corrupt");
        let path = dir.join("profile.json");
        assert_eq!(load_profile(&path), PlayerProfile::default(), "missing file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{ not json !!").unwrap();
        assert_eq!(load_profile(&path), PlayerProfile::default(), "corrupt file");
        std::fs::write(&path, r#"{"equipped_chip":"WarpDrive"}"#).unwrap();
        assert_eq!(load_profile(&path), PlayerProfile::default(), "unknown variant");
        // Missing fields default; locked equips are clamped on load.
        let partial = parse_profile(r#"{"unlocked_feats":["FirstExtraction"],"equipped_skin":"ChromeGold","equipped_chip":"SurveyOs"}"#).unwrap();
        assert!(partial.has_feat(FeatId::FirstExtraction));
        assert_eq!(partial.equipped_skin, HandsetSkin::UacOrange);
        assert_eq!(partial.equipped_chip, StarterChip::StockUac);
        assert_eq!(partial.active_catalog, default_catalog());
        assert_eq!(partial.schema_version, PROFILE_SCHEMA_VERSION);
        let empty = parse_profile("{}").unwrap();
        assert_eq!(empty, PlayerProfile::default());
        // In-memory stores never touch the disk.
        assert!(ProfileStore::in_memory(PlayerProfile::default()).save().is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_settings_defaults_and_sanitize() {
        let d = UserSettings::default();
        assert_eq!(d.mouse_sensitivity, 1.0);
        assert_eq!(d.base_fov_deg, 85.0);
        assert!(d.speed_fov_enabled && d.strafe_roll_enabled && !d.audio_muted);
        assert_eq!((d.speed_fov_scale, d.strafe_roll_scale, d.audio_master_volume), (1.0, 1.0, 1.0));
        assert_eq!((d.crosshair_style, d.crosshair_color), (CrosshairStyle::Cross, CrosshairColor::White));
        assert_eq!(PlayerProfile::default().settings, d);

        let mut s = UserSettings {
            mouse_sensitivity: 9.0,
            base_fov_deg: 20.0,
            speed_fov_scale: -1.0,
            strafe_roll_scale: 4.0,
            audio_master_volume: 1.7,
            ..d.clone()
        };
        s.sanitize();
        assert_eq!(
            (s.mouse_sensitivity, s.base_fov_deg, s.speed_fov_scale, s.strafe_roll_scale, s.audio_master_volume),
            (3.0, 70.0, 0.0, 1.5, 1.0)
        );
        let mut n = UserSettings {
            mouse_sensitivity: f32::NAN,
            base_fov_deg: f32::INFINITY,
            speed_fov_scale: f32::NEG_INFINITY,
            strafe_roll_scale: f32::NAN,
            audio_master_volume: f32::NAN,
            ..d.clone()
        };
        n.sanitize();
        assert_eq!(n, d, "NaN / Inf fall back to defaults");

        // The profile sanitizer covers settings too.
        let mut p = PlayerProfile::default();
        p.settings.base_fov_deg = 500.0;
        p.stats.best_speed = f32::NAN;
        p.sanitize();
        assert_eq!(p.settings.base_fov_deg, 110.0);
        assert_eq!(p.stats.best_speed, 0.0);
        assert_eq!(live_settings(None), d);
        for st in CrosshairStyle::ALL {
            assert!(!st.name().is_empty());
            assert_ne!(st.next(), st);
        }
        assert_eq!(CrosshairStyle::Chevron.next(), CrosshairStyle::Cross);
        let colors: BTreeSet<String> = CrosshairColor::ALL.iter().map(|c| format!("{:?}", c.color())).collect();
        assert_eq!(colors.len(), CrosshairColor::ALL.len(), "distinct reticle colors");
    }

    #[test]
    fn old_profiles_without_settings_or_records_still_load() {
        // A Milestone 11 profile: no `settings`, no movement records, no endless fields.
        let old = r#"{
            "schema_version": 1,
            "unlocked_feats": ["FirstExtraction", "SignalScout"],
            "equipped_chip": "SurveyOs",
            "stats": {"runs_played": 4, "victories": 0, "best_floor": 3, "total_kills": 90},
            "history": [{"floor": 3, "victory": false, "kills": 30, "jammers": 4, "time_secs": 200.0, "chip": "SurveyOs", "contracts_count": 0, "skin": "UacOrange"}]
        }"#;
        let p = parse_profile(old).unwrap();
        assert_eq!(p.schema_version, PROFILE_SCHEMA_VERSION);
        assert_eq!(p.settings, UserSettings::default());
        assert_eq!(p.stats.runs_played, 4);
        assert_eq!((p.stats.best_speed, p.stats.best_bhop_chain, p.stats.best_endless_floor), (0.0, 0, 0));
        let h = &p.history[0];
        assert!(!h.endless && h.peak_speed == 0.0 && h.max_bhop_chain == 0);
        assert_eq!(p.equipped_chip, StarterChip::SurveyOs);
        // Partial settings keep the rest at defaults; wild values are clamped.
        let partial = parse_profile(r#"{"settings":{"base_fov_deg":200.0,"crosshair_color":"Amber"}}"#).unwrap();
        assert_eq!(partial.settings.base_fov_deg, 110.0);
        assert_eq!(partial.settings.crosshair_color, CrosshairColor::Amber);
        assert_eq!(partial.settings.mouse_sensitivity, 1.0);
        // Round trip.
        let q = PlayerProfile {
            settings: UserSettings { crosshair_style: CrosshairStyle::Chevron, audio_muted: true, mouse_sensitivity: 1.35, ..default() },
            ..default()
        };
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(parse_profile(&json).unwrap(), q);
    }

    #[test]
    fn endless_death_adds_only_post_victory_deltas() {
        let mut store = ProfileStore::in_memory(PlayerProfile::default());
        let loadout = RunLoadout::default();
        let mut run = RunStats {
            floor: 5,
            kills: 40,
            jammers_destroyed: 20,
            fiends_killed: 6,
            blackout_kills: 3,
            sos_completed: 5,
            floors_cleared: 5,
            time_elapsed: 900.0,
            peak_speed: 24.0,
            max_bhop_chain: 5,
            ..default()
        };
        record_run_outcome(true, &mut run, &loadout, &mut store).unwrap();
        let s = &store.profile.stats;
        assert_eq!((s.runs_played, s.victories, s.total_kills, s.best_floor), (1, 1, 40, 5));
        assert_eq!((s.best_speed, s.best_bhop_chain, s.best_endless_floor), (24.0, 5, 0));
        assert_eq!(run.recorded_snapshot, Some(RecordedRunTotals::of(&run)));
        assert!(store.profile.has_feat(FeatId::BunkerCleared));

        // Keep descending: Floors 6, 7, 8, then die on 8.
        let mut next = NextState::<AppState>::default();
        crate::state::continue_into_endless(&mut run, &mut next);
        assert!(!run.run_recorded);
        run.advance_floor();
        run.advance_floor();
        assert_eq!(run.floor, 8);
        run.kills += 25;
        run.jammers_destroyed += 9;
        run.fiends_killed += 1;
        run.sos_completed += 2;
        run.track_movement(33.5, 11);
        let sum = record_run_outcome(false, &mut run, &loadout, &mut store).unwrap();
        assert!(!sum.victory);
        let s = &store.profile.stats;
        assert_eq!(s.runs_played, 1, "an Endless continuation is the same run");
        assert_eq!(s.victories, 1);
        assert_eq!((s.total_kills, s.total_jammers, s.total_fiends, s.total_sos_completed, s.total_blackout_kills), (65, 29, 7, 7, 3));
        assert_eq!((s.best_floor, s.best_endless_floor), (8, 8));
        assert_eq!((s.best_speed, s.best_bhop_chain), (33.5, 11));
        assert_eq!(store.profile.history.len(), 1, "the victory record is updated in place");
        let h = store.profile.history.last().unwrap();
        assert!(h.victory && h.endless);
        assert_eq!((h.floor, h.kills, h.max_bhop_chain), (8, 65, 11));
        // Guarded: a second call is a no-op.
        assert!(record_run_outcome(false, &mut run, &loadout, &mut store).is_none());
        assert_eq!(store.profile.stats.total_kills, 65);
        // Jammer Hunter (8 lifetime jammers) was already earned at Floor 5.
        assert!(store.profile.has_feat(FeatId::JammerHunter));
    }

    #[test]
    fn profile_path_policy_never_pollutes_tests() {
        assert_eq!(profile_path_policy(None, false, false, Some("/x"), Some("/h")), None, "headless = memory");
        assert_eq!(profile_path_policy(None, true, true, Some("/x"), Some("/h")), None, "bot/shots = memory");
        assert_eq!(profile_path_policy(Some("memory"), true, false, Some("/x"), None), None);
        assert_eq!(profile_path_policy(Some("NONE"), true, false, Some("/x"), None), None);
        assert_eq!(profile_path_policy(Some("/saves/p.json"), false, true, None, None), Some(PathBuf::from("/saves/p.json")));
        assert_eq!(
            profile_path_policy(None, true, false, Some("/xdg"), Some("/h")),
            Some(PathBuf::from("/xdg/segnale-debole/profile.json"))
        );
        assert_eq!(
            profile_path_policy(None, true, false, None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share/segnale-debole/profile.json"))
        );
        assert_eq!(profile_path_policy(None, true, false, Some(""), None), None);
    }
}
