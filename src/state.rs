//! Global game state machine, per-run statistics and roguelike modifier stubs.
//!
//! Flow: `MainMenu -> Playing -> (UpgradeSelect -> Playing)* -> Victory`, with
//! `GameOver` reachable from `Playing`. The pre-run screens `Loadout` and
//! `ContractsFeats` are reachable from `MainMenu`, `GameOver` and `Victory`
//! and launch runs directly (they tear the previous run down like `MainMenu`). Floor transitions go through
//! [`RunStats::advance_floor`], which tells the caller where to go next; the level
//! module rebuilds the labyrinth automatically on the next `OnEnter(Playing)`
//! whenever `RunStats::floor` differs from the floor of the current `LevelGrid`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

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
}

impl AppState {
    /// Pre-run screens where no run is alive (player and level torn down).
    pub fn is_pre_run(self) -> bool {
        matches!(self, AppState::MainMenu | AppState::Loadout | AppState::ContractsFeats)
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

/// The fifteen roguelike "App Store" upgrades: 10 core apps + 5 signature
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
    StoreAndForward,
    EmergencyCallback,
    MissedCall,
    SignalSpoofer,
    JammerHijack,
}

impl UpgradeId {
    pub const ALL: [UpgradeId; 15] = [
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
            UpgradeId::FirewallDaemon => "+40 armor & max armor; regenerates 6/s while SOS advances in the ring.",
            UpgradeId::PacketSniffer => "Ping cooldown halved; marked demons take +35% bonus damage.",
            UpgradeId::EmpOvercharge => "5G Overload radius +40%, more damage, chains.",
            UpgradeId::AirplaneModePhase => "Dash grants invuln, a static shockwave, and -15% cooldown.",
            UpgradeId::HighCapacityCell => "Ammo caps +35%; all pickups +30% fuller.",
            UpgradeId::SubGhzPenetration => "Walls attenuate 55% less; Ping projects a floor route to the Tower.",
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

#[derive(Resource, Debug, Clone)]
pub struct RunStats {
    /// Current floor, `1..=FINAL_FLOOR`.
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
        }
    }
}

impl RunStats {
    /// Reset everything for a fresh run with the given seed.
    pub fn start_new_run(&mut self, seed: u64) {
        *self = RunStats {
            run_seed: seed,
            ..Default::default()
        };
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
        if self.floor >= FINAL_FLOOR {
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

pub struct StatePlugin;

impl Plugin for StatePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>()
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
    fn floor_seeds_differ() {
        let mut run = RunStats::default();
        let a = run.floor_seed();
        run.floor = 2;
        assert_ne!(a, run.floor_seed());
    }
}
