//! Global game state machine, per-run statistics and roguelike modifier stubs.
//!
//! Flow: `MainMenu -> Playing -> (UpgradeSelect -> Playing)* -> Victory`, with
//! `GameOver` reachable from `Playing`. The pre-run screens `Loadout` and
//! `ContractsFeats` are reachable from `MainMenu`, `GameOver` and `Victory`
//! and launch runs directly (they tear the previous run down like `MainMenu`). Floor transitions go through
//! [`RunStats::advance_floor`], which tells the caller where to go next; the level
//! module rebuilds the labyrinth automatically on the next `OnEnter(Playing)`
//! whenever `RunStats::floor` differs from the floor of the current `LevelGrid`.
//!
//! Endless Overdrive: from `Victory` the player may keep descending
//! ([`continue_into_endless`]): `Victory -> UpgradeSelect -> Playing` on Floor 6+,
//! where every cleared floor simply advances (no further victories). Every
//! [`FINAL_FLOOR`]-th floor is a boss floor ([`is_boss_floor`]).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::enemies::KIND_COUNT;
use crate::trials::DailyDate;

/// Last floor of a standard run. Clearing it means victory.
pub const FINAL_FLOOR: u32 = 5;

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    #[default]
    MainMenu,
    Playing,
    /// In-run pause overlay (freezes simulation while keeping the 3D scene alive).
    Paused,
    UpgradeSelect,
    GameOver,
    Victory,
    /// Pre-run loadout editor (starter chip, handset skin, App Store catalog).
    Loadout,
    /// Carrier contracts toggles, lifetime stats and the feats list.
    ContractsFeats,
    /// Input / camera / audio / reticle options. Reachable from `MainMenu`
    /// and `Paused`; it is NOT a pre-run screen, so opening it mid-run keeps
    /// the player and the level alive (see [`SettingsReturn`]).
    Settings,
    /// 3D Bestiary & Armory viewer (see `compendium.rs`): a pre-run screen
    /// reachable from `MainMenu`.
    Compendium,
    /// Global Daily / Time Trial leaderboards + account & callsign (see
    /// `leaderboard.rs`). Reachable from `MainMenu` and the end cards; it is a
    /// pre-run screen (entering it tears down a finished run).
    Leaderboard,
}

/// Where `Settings` returns to (`MainMenu` or `Paused`).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsReturn(pub AppState);

impl Default for SettingsReturn {
    fn default() -> Self {
        SettingsReturn(AppState::MainMenu)
    }
}

impl AppState {
    /// Pre-run screens where no run is alive (player and level torn down).
    pub fn is_pre_run(self) -> bool {
        matches!(self, AppState::MainMenu | AppState::Loadout | AppState::ContractsFeats | AppState::Compendium | AppState::Leaderboard)
    }
}

/// Ordered system sets for everything that runs every frame while `Playing`.
///
/// Milestone 2 slots weapons/enemies into `Combat` and extraction into `World`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameSet {
    /// Raw input: mouse look, cursor capture, movement intent.
    Input,
    /// Player physics integration against the `LevelGrid`.
    Movement,
    /// World simulation: timers, animated props, hazards, extraction.
    World,
    /// Signal field solve (reads final player/jammer positions of the frame).
    Signal,
    /// Weapons, enemies, projectiles (Milestone 2).
    Combat,
    /// HUD refresh (reads everything above).
    Ui,
}

/// The seventeen roguelike "App Store" upgrades: 12 core apps + 5 signature
/// apps licensed by feats (see `meta.rs`). `upgrades.rs` applies them to
/// [`PlayerModifiers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UpgradeId {
    DualSimBarrels,
    FiveGOverclock,
    CopperAntenna,
    SignalSiphon,
    FirewallDaemon,
    PacketSniffer,
    EmpOvercharge,
    AirplaneModePhase,
    HighCapacityCell,
    SubGhzPenetration,
    DopplerOverdrive,
    BlastThrusters,
    StoreAndForward,
    EmergencyCallback,
    MissedCall,
    SignalSpoofer,
    JammerHijack,
}

impl UpgradeId {
    pub const ALL: [UpgradeId; 17] = [
        UpgradeId::DualSimBarrels,
        UpgradeId::FiveGOverclock,
        UpgradeId::CopperAntenna,
        UpgradeId::SignalSiphon,
        UpgradeId::FirewallDaemon,
        UpgradeId::PacketSniffer,
        UpgradeId::EmpOvercharge,
        UpgradeId::AirplaneModePhase,
        UpgradeId::HighCapacityCell,
        UpgradeId::SubGhzPenetration,
        UpgradeId::DopplerOverdrive,
        UpgradeId::BlastThrusters,
        UpgradeId::StoreAndForward,
        UpgradeId::EmergencyCallback,
        UpgradeId::MissedCall,
        UpgradeId::SignalSpoofer,
        UpgradeId::JammerHijack,
    ];

    pub fn name(self) -> &'static str {
        match self {
            UpgradeId::DualSimBarrels => "Dual-SIM Barrels",
            UpgradeId::FiveGOverclock => "5G Overclock",
            UpgradeId::CopperAntenna => "External Copper Antenna",
            UpgradeId::SignalSiphon => "Signal Siphon App",
            UpgradeId::FirewallDaemon => "Firewall Daemon",
            UpgradeId::PacketSniffer => "Packet Sniffer",
            UpgradeId::EmpOvercharge => "EMP Overcharge",
            UpgradeId::AirplaneModePhase => "Airplane Mode Phase",
            UpgradeId::HighCapacityCell => "High-Capacity Cell",
            UpgradeId::SubGhzPenetration => "Sub-GHz Penetration",
            UpgradeId::DopplerOverdrive => "Doppler Overdrive",
            UpgradeId::BlastThrusters => "Blast Thrusters",
            UpgradeId::StoreAndForward => "Store & Forward",
            UpgradeId::EmergencyCallback => "Emergency Callback",
            UpgradeId::MissedCall => "Missed Call",
            UpgradeId::SignalSpoofer => "Signal Spoofer",
            UpgradeId::JammerHijack => "Jammer Hijack",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            UpgradeId::DualSimBarrels => "Shotgun fires +3 pellets with a tighter choke.",
            UpgradeId::FiveGOverclock => "At 4-5 bars: +35% fire rate, +25% move speed.",
            UpgradeId::CopperAntenna => "+1.25 minimum signal bars everywhere.",
            UpgradeId::SignalSiphon => "While you have 3+ bars, demon kills restore +8 HP.",
            UpgradeId::FirewallDaemon => "+40 armor & max armor; regenerates up to 6/s while SOS advances (scaled by upload speed).",
            UpgradeId::PacketSniffer => "Ping cooldown halved; marked demons take +35% bonus damage.",
            UpgradeId::EmpOvercharge => "5G Overload radius +40%, more damage, chains.",
            UpgradeId::AirplaneModePhase => "Dash grants invuln, a static shockwave, and -15% cooldown.",
            UpgradeId::HighCapacityCell => "Ammo caps +35%; all pickups +30% fuller.",
            UpgradeId::SubGhzPenetration => "Walls attenuate 55% less; Ping projects a floor route to the Tower.",
            UpgradeId::DopplerOverdrive => "At 20+ m/s: +30% weapon damage and 20% tighter shotgun choke.",
            UpgradeId::BlastThrusters => "Rocket jump +25% boost, +20% blast radius, -50% self-damage.",
            UpgradeId::StoreAndForward => "Kills in natural 0-bar blackouts queue up to 3 packets; hitting 3+ bars fires 20-dmg arcs.",
            UpgradeId::EmergencyCallback => "Kill a ping-marked Jammer Fiend to unlock 1 any-signal 5G Overload for 6s.",
            UpgradeId::MissedCall => "Pressing Ping at 0 bars drops a ringing decoy handset drawing nearby Imps for 2.5s.",
            UpgradeId::SignalSpoofer => "Dash in a 0-1 bar Dead Zone to spoof 4 bars for 2.5s (6s cooldown).",
            UpgradeId::JammerHijack => "Destroying a Jammer or Fiend creates a 5-bar Signal Hotspot for 8s.",
        }
    }
}

/// Stat modifiers accumulated from upgrades. All values are neutral by default.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct PlayerModifiers {
    /// Fraction (0..1) of per-wall signal attenuation that is ignored.
    pub wall_penetration: f32,
    /// Minimum signal strength in bars (0..5) regardless of walls / jammers.
    pub min_signal_floor: f32,
    pub extra_pellets: u32,
    pub pellet_spread_mult: f32,
    pub fire_rate_mult: f32,
    pub move_speed_mult: f32,
    /// 5G Overclock: bonus applied only at >= 4 bars.
    pub overclock_bonus: f32,
    /// 5G Overclock move-speed bonus at 4-5 bars.
    pub overclock_move_bonus: f32,
    pub siphon_heal: f32,
    pub max_armor_bonus: f32,
    pub armor_regen_while_uploading: f32,
    pub ping_cooldown_mult: f32,
    pub ping_mark_damage_bonus: f32,
    pub emp_radius_mult: f32,
    pub emp_damage_mult: f32,
    pub emp_chains: bool,
    pub dash_invulnerable: bool,
    pub dash_shockwave: bool,
    pub dash_cooldown_mult: f32,
    pub ammo_capacity_mult: f32,
    pub pickup_mult: f32,
    /// Sub-GHz Penetration: Ping Scan projects a walkable floor route to the Tower.
    pub sub_ghz_route: bool,
    /// Store & Forward: natural 0-bar kills queue packets discharged at 3+ bars.
    pub store_and_forward: bool,
    /// Emergency Callback: ping-marked Jammer Fiend kills unlock an any-signal Overload.
    pub emergency_callback: bool,
    /// Missed Call: Ping at 0 bars drops a ringing decoy handset that lures Imps.
    pub missed_call: bool,
    /// 5G Overload cooldown multiplier (Faraday Case chip).
    pub emp_cooldown_mult: f32,
    /// Incoming enemy damage multiplier while in a 0-1 bar Dead Zone (Faraday Case).
    pub dead_zone_damage_mult: f32,
    /// Signal Spoofer: dashing at 0-1 bars spoofs 4 bars for a few seconds.
    pub signal_spoofer: bool,
    /// Jammer Hijack: destroyed Jammers / Fiends leave a 5-bar hotspot.
    pub jammer_hijack: bool,
    /// Roaming Charges contract: Jammer / Fiend suppression radius multiplier.
    pub jammer_radius_mult: f32,
    /// Roaming Charges contract: exponent on the per-wall attenuation (more loss).
    pub wall_attenuation_exp: f32,
    /// Priority Jamming contract: extra Jammer Fiend pressure during the SOS.
    pub priority_jamming: bool,
    /// Overtime Uplink contract: SOS uplink duration multiplier.
    pub upload_duration_mult: f32,
    /// Overtime Uplink contract: Demon Rush wave interval multiplier.
    pub wave_interval_mult: f32,
    /// Low-Battery Mode contract: no HP field repair between floors.
    pub skip_floor_heal: bool,
    /// Doppler Overdrive: weapon damage bonus while moving >= 20 m/s.
    pub doppler_damage_bonus: f32,
    /// Doppler Overdrive: shotgun spread multiplier while moving >= 20 m/s.
    pub doppler_spread_mult: f32,
    /// Blast Thrusters: rocket-jump impulse multiplier.
    pub rocket_jump_mult: f32,
    /// Blast Thrusters: rocket splash radius multiplier.
    pub rocket_splash_mult: f32,
    /// Blast Thrusters: rocket self-damage multiplier.
    pub rocket_self_damage_mult: f32,
}

impl Default for PlayerModifiers {
    fn default() -> Self {
        Self {
            wall_penetration: 0.0,
            min_signal_floor: 0.0,
            extra_pellets: 0,
            pellet_spread_mult: 1.0,
            fire_rate_mult: 1.0,
            move_speed_mult: 1.0,
            overclock_bonus: 0.0,
            overclock_move_bonus: 0.0,
            siphon_heal: 0.0,
            max_armor_bonus: 0.0,
            armor_regen_while_uploading: 0.0,
            ping_cooldown_mult: 1.0,
            ping_mark_damage_bonus: 0.0,
            emp_radius_mult: 1.0,
            emp_damage_mult: 1.0,
            emp_chains: false,
            dash_invulnerable: false,
            dash_shockwave: false,
            dash_cooldown_mult: 1.0,
            ammo_capacity_mult: 1.0,
            pickup_mult: 1.0,
            sub_ghz_route: false,
            store_and_forward: false,
            emergency_callback: false,
            missed_call: false,
            emp_cooldown_mult: 1.0,
            dead_zone_damage_mult: 1.0,
            signal_spoofer: false,
            jammer_hijack: false,
            jammer_radius_mult: 1.0,
            wall_attenuation_exp: 1.0,
            priority_jamming: false,
            upload_duration_mult: 1.0,
            wave_interval_mult: 1.0,
            skip_floor_heal: false,
            doppler_damage_bonus: 0.0,
            doppler_spread_mult: 1.0,
            rocket_jump_mult: 1.0,
            rocket_splash_mult: 1.0,
            rocket_self_damage_mult: 1.0,
        }
    }
}

/// Boss floors: 5, 10, 15, 20, ... (Floor 10+ are Endless boss rematches).
pub fn is_boss_floor(floor: u32) -> bool {
    floor >= FINAL_FLOOR && floor.is_multiple_of(FINAL_FLOOR)
}

/// Lifetime counters of a run already folded into the profile (at Victory),
/// so an Endless continuation only adds what happened afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordedRunTotals {
    pub kills: u32,
    pub jammers: u32,
    pub fiends: u32,
    pub blackout_kills: u32,
    pub sos_completed: u32,
    /// Per-kind kills (indexed by [`crate::enemies::EnemyKind::index`]).
    pub enemy_kills: [u32; KIND_COUNT],
}

impl RecordedRunTotals {
    pub fn of(run: &RunStats) -> Self {
        RecordedRunTotals {
            kills: run.kills,
            jammers: run.jammers_destroyed,
            fiends: run.fiends_killed,
            blackout_kills: run.blackout_kills,
            sos_completed: run.sos_completed,
            enemy_kills: run.enemy_kills,
        }
    }

    /// Saturating per-counter difference `self - earlier`.
    pub fn delta_since(&self, earlier: &RecordedRunTotals) -> Self {
        RecordedRunTotals {
            kills: self.kills.saturating_sub(earlier.kills),
            jammers: self.jammers.saturating_sub(earlier.jammers),
            fiends: self.fiends.saturating_sub(earlier.fiends),
            blackout_kills: self.blackout_kills.saturating_sub(earlier.blackout_kills),
            sos_completed: self.sos_completed.saturating_sub(earlier.sos_completed),
            enemy_kills: std::array::from_fn(|i| self.enemy_kills[i].saturating_sub(earlier.enemy_kills[i])),
        }
    }
}

/// Where to go after clearing a floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorAdvance {
    /// The run continues on this (new) floor; go through `UpgradeSelect`.
    NextFloor(u32),
    /// The final floor was cleared.
    Victory,
}

/// What kind of run is live. Every mode shares the same `Playing` loop; the
/// mode only swaps the seed / loadout source (Daily), or the level and rules
/// (Time Trial, see `trials.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RunMode {
    /// Regular run (main menu PLAY, playtest bot, Endless Overdrive).
    #[default]
    Standard,
    /// Daily Seeded Run: the same bunker and ruleset for everyone on that UTC date.
    Daily(DailyDate),
    /// Movement / rocket-jump time trial on the handcrafted course.
    TimeTrial,
}

impl RunMode {
    pub fn is_time_trial(self) -> bool {
        self == RunMode::TimeTrial
    }

    pub fn daily_date(self) -> Option<DailyDate> {
        match self {
            RunMode::Daily(d) => Some(d),
            _ => None,
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct RunStats {
    /// Current floor, `1..=FINAL_FLOOR` (beyond it in Endless Overdrive).
    pub floor: u32,
    pub kills: u32,
    pub jammers_destroyed: u32,
    /// Seconds spent in `Playing` this run.
    pub time_elapsed: f32,
    /// Base seed of the run; each floor derives its own labyrinth seed from it.
    pub run_seed: u64,
    pub upgrades: Vec<UpgradeId>,
    /// Non-vaporized kills scored in a natural 0-bar blackout.
    pub blackout_kills: u32,
    /// Jammer Fiends killed (not vaporized by the extraction beam).
    pub fiends_killed: u32,
    /// SOS uplinks completed (orbital extraction finished).
    pub sos_completed: u32,
    /// Floors cleared this run (including the final one on victory).
    pub floors_cleared: u32,
    /// The run was already folded into the profile (GameOver / Victory).
    pub run_recorded: bool,
    /// F9 debug floor skip was used during this run (disqualifies profile recording).
    pub debug_skipped: bool,
    /// Top horizontal speed reached this run (m/s).
    pub peak_speed: f32,
    /// Longest bunny-hop chain linked this run.
    pub max_bhop_chain: u32,
    /// The run continued past Victory into Endless Overdrive (Floor 6+).
    pub endless: bool,
    /// Counters already folded into the profile at the Floor 5 Victory.
    pub recorded_snapshot: Option<RecordedRunTotals>,
    /// Standard / Daily / Time Trial.
    pub mode: RunMode,
    /// Cumulative `time_elapsed` at the end of each cleared floor (speedrun splits).
    pub floor_splits: Vec<f32>,
    /// Shareable seed code of the run (`MARS7`, `791E-0A5A`, ...); `None` for
    /// seedless modes. See [`crate::compendium::format_seed_code`].
    pub seed_label: Option<String>,
    /// The run was launched from a player-entered custom seed.
    pub custom_seed: bool,
    /// Kills per demon kind this run (indexed by [`crate::enemies::EnemyKind::index`]).
    pub enemy_kills: [u32; KIND_COUNT],
    /// Best Bandwidth Style Rank reached this run (Milestone 31 end-card badge).
    pub peak_style_rank: Option<crate::style::StyleRank>,
}

impl Default for RunStats {
    fn default() -> Self {
        Self {
            floor: 1,
            kills: 0,
            jammers_destroyed: 0,
            time_elapsed: 0.0,
            run_seed: 0x5E6_A1E,
            upgrades: Vec::new(),
            blackout_kills: 0,
            fiends_killed: 0,
            sos_completed: 0,
            floors_cleared: 0,
            run_recorded: false,
            debug_skipped: false,
            peak_speed: 0.0,
            max_bhop_chain: 0,
            endless: false,
            recorded_snapshot: None,
            mode: RunMode::Standard,
            floor_splits: Vec::new(),
            seed_label: None,
            custom_seed: false,
            enemy_kills: [0; KIND_COUNT],
            peak_style_rank: None,
        }
    }
}

impl RunStats {
    /// Reset everything for a fresh run with the given seed.
    /// The run's seed label defaults to its formatted code (see
    /// [`crate::compendium::format_seed_code`]); custom seeds overwrite it.
    pub fn start_new_run(&mut self, seed: u64) {
        *self = RunStats {
            run_seed: seed,
            seed_label: Some(crate::compendium::format_seed_code(seed)),
            ..Default::default()
        };
    }

    /// Fold this frame's movement into the run's telemetry.
    pub fn track_movement(&mut self, horizontal_speed: f32, bhop_chain: u32) {
        if horizontal_speed.is_finite() {
            self.peak_speed = self.peak_speed.max(horizontal_speed);
        }
        self.max_bhop_chain = self.max_bhop_chain.max(bhop_chain);
    }

    /// Deterministic labyrinth seed for the current floor.
    pub fn floor_seed(&self) -> u64 {
        self.run_seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add((self.floor as u64).wrapping_mul(0xD1B5_4A32_D192_ED03))
    }

    /// Mark the current floor cleared. Returns where the game should go next.
    pub fn advance_floor(&mut self) -> FloorAdvance {
        self.floors_cleared += 1;
        self.floor_splits.push(self.time_elapsed);
        if self.endless {
            self.floor += 1;
            FloorAdvance::NextFloor(self.floor)
        } else if self.floor >= FINAL_FLOOR {
            FloorAdvance::Victory
        } else {
            self.floor += 1;
            FloorAdvance::NextFloor(self.floor)
        }
    }
}

/// Begin a fresh run (main menu "START", playtest bot, tests): new seed,
/// neutral modifiers, straight into `Playing` on Floor 1. Meta loadouts
/// (starter chip, contracts, catalog) go through `meta::start_run_with_loadout`.
pub fn start_run(run: &mut RunStats, mods: &mut PlayerModifiers, next: &mut NextState<AppState>, seed: u64) {
    run.start_new_run(seed);
    *mods = PlayerModifiers::default();
    next.set(AppState::Playing);
}

/// Clear the current floor and switch to the right state
/// (`UpgradeSelect` for the next floor, or `Victory`).
pub fn complete_floor(run: &mut RunStats, next: &mut NextState<AppState>) -> FloorAdvance {
    let adv = run.advance_floor();
    match adv {
        FloorAdvance::NextFloor(_) => next.set(AppState::UpgradeSelect),
        FloorAdvance::Victory => next.set(AppState::Victory),
    }
    adv
}

/// Victory "KEEP DESCENDING": enter Endless Overdrive. The player, level and
/// upgrades stay alive; the Floor 5 -> 6 upgrade is drafted first. The Victory
/// record stays in the profile (`recorded_snapshot`), so a later death only
/// adds what happened after Floor 5.
pub fn continue_into_endless(run: &mut RunStats, next: &mut NextState<AppState>) {
    run.endless = true;
    run.floor = run.floor.max(FINAL_FLOOR) + 1;
    run.run_recorded = false;
    next.set(AppState::UpgradeSelect);
}

pub struct StatePlugin;

impl Plugin for StatePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>()
            .init_resource::<SettingsReturn>()
            .init_resource::<RunStats>()
            .init_resource::<PlayerModifiers>()
            .configure_sets(
                Update,
                (
                    GameSet::Input,
                    GameSet::Movement,
                    GameSet::World,
                    GameSet::Signal,
                    GameSet::Combat,
                    GameSet::Ui,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            )
            .add_systems(Update, tick_run_time.in_set(GameSet::World));
    }
}

fn tick_run_time(time: Res<Time>, mut run: ResMut<RunStats>) {
    run.time_elapsed += time.delta_secs();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_advance_reaches_victory() {
        let mut run = RunStats::default();
        for f in 2..=FINAL_FLOOR {
            assert_eq!(run.advance_floor(), FloorAdvance::NextFloor(f));
        }
        assert_eq!(run.advance_floor(), FloorAdvance::Victory);
        assert_eq!(run.floor, FINAL_FLOOR);
        assert_eq!(run.floors_cleared, FINAL_FLOOR, "victory counts the final floor");
    }

    #[test]
    fn endless_never_retriggers_victory_and_boss_floors_repeat() {
        let mut run = RunStats { floor: FINAL_FLOOR, ..default() };
        assert_eq!(run.advance_floor(), FloorAdvance::Victory);
        run.run_recorded = true;
        run.recorded_snapshot = Some(RecordedRunTotals::default());
        let mut next = NextState::<AppState>::default();
        continue_into_endless(&mut run, &mut next);
        assert!(matches!(next, NextState::Pending(AppState::UpgradeSelect)));
        assert!(run.endless && !run.run_recorded && run.recorded_snapshot.is_some());
        assert_eq!(run.floor, FINAL_FLOOR + 1);
        for f in FINAL_FLOOR + 2..=FINAL_FLOOR + 12 {
            assert_eq!(run.advance_floor(), FloorAdvance::NextFloor(f));
        }
        assert!(!is_boss_floor(1) && !is_boss_floor(4) && !is_boss_floor(6) && !is_boss_floor(0));
        assert!(is_boss_floor(5) && is_boss_floor(10) && is_boss_floor(15) && is_boss_floor(20));
        assert!(!AppState::Settings.is_pre_run(), "Settings mid-run keeps the run alive");
        assert_eq!(SettingsReturn::default().0, AppState::MainMenu);
    }

    #[test]
    fn movement_telemetry_and_totals_delta() {
        let mut run = RunStats::default();
        run.track_movement(12.0, 1);
        run.track_movement(31.5, 4);
        run.track_movement(5.0, 0);
        run.track_movement(f32::NAN, 0);
        assert_eq!((run.peak_speed, run.max_bhop_chain), (31.5, 4));
        run.kills = 10;
        run.sos_completed = 5;
        let snap = RecordedRunTotals::of(&run);
        run.kills = 14;
        run.sos_completed = 7;
        let d = RecordedRunTotals::of(&run).delta_since(&snap);
        assert_eq!((d.kills, d.sos_completed, d.jammers), (4, 2, 0));
    }

    #[test]
    fn advance_floor_records_cumulative_splits() {
        let mut run = RunStats::default();
        assert_eq!(run.mode, RunMode::Standard);
        for f in 1..=FINAL_FLOOR {
            run.time_elapsed = f as f32 * 60.0 + 0.5;
            run.advance_floor();
        }
        assert_eq!(run.floor_splits, vec![60.5, 120.5, 180.5, 240.5, 300.5]);
        run.start_new_run(7);
        assert!(run.floor_splits.is_empty() && run.mode == RunMode::Standard, "a fresh run clears splits and mode");
        assert!(RunMode::TimeTrial.is_time_trial() && RunMode::Daily(DailyDate::EPOCH).daily_date() == Some(DailyDate::EPOCH));
    }

    #[test]
    fn floor_seeds_differ() {
        let mut run = RunStats::default();
        let a = run.floor_seed();
        run.floor = 2;
        assert_ne!(a, run.floor_seed());
    }
}
