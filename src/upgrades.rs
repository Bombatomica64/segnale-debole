//! Roguelike "App Store" upgrades: drafting 3 distinct offers, applying the
//! chosen one to [`PlayerModifiers`] / [`Player`], and the between-floor patch.
//! The 3-card UI lives in `hud.rs` and calls [`pick_upgrade`].

use bevy::prelude::*;
use rand::Rng;
use rand::seq::SliceRandom;

use crate::apps::{
    emp_radius, ping_cooldown, CALLBACK_SECS, DECOY_LURE_RADIUS, DECOY_SECS, HIJACK_SECS, SIGNAL_SPOOF_COOLDOWN, SIGNAL_SPOOF_SECS,
    STORE_FORWARD_DAMAGE, STORE_FORWARD_MAX_PACKETS,
};
use crate::bot_input::BotControl;
use crate::meta::{default_catalog, RunLoadout};
use crate::player::{Player, DASH_COOLDOWN};
use crate::signal::bars_from_strength;
use crate::weapons::SHOTGUN_PELLETS;
use crate::state::{AppState, PlayerModifiers, RunStats, UpgradeId};

pub const BASE_MAX_SHELLS: u32 = 50;
pub const BASE_MAX_PLASMA: u32 = 200;
pub const BASE_MAX_ROCKETS: u32 = 20;

/// The three offers of the current App Store visit.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct UpgradeChoices(pub Vec<UpgradeId>);

impl UpgradeId {
    /// Non-stackable upgrades only toggle behaviour; re-buying is pointless.
    pub fn is_stackable(self) -> bool {
        !matches!(
            self,
            UpgradeId::AirplaneModePhase
                | UpgradeId::EmpOvercharge
                | UpgradeId::StoreAndForward
                | UpgradeId::EmergencyCallback
                | UpgradeId::MissedCall
                | UpgradeId::SignalSpoofer
                | UpgradeId::JammerHijack
        )
    }

    /// Short "app icon" glyph for the store card.
    pub fn icon(self) -> &'static str {
        match self {
            UpgradeId::DualSimBarrels => "[SIM]",
            UpgradeId::FiveGOverclock => "[5G+]",
            UpgradeId::CopperAntenna => "[ANT]",
            UpgradeId::SignalSiphon => "[+HP]",
            UpgradeId::FirewallDaemon => "[FW]",
            UpgradeId::PacketSniffer => "[PKT]",
            UpgradeId::EmpOvercharge => "[EMP]",
            UpgradeId::AirplaneModePhase => "[>>]",
            UpgradeId::HighCapacityCell => "[BAT]",
            UpgradeId::SubGhzPenetration => "[GHz]",
            UpgradeId::DopplerOverdrive => "[DOP]",
            UpgradeId::BlastThrusters => "[RKT]",
            UpgradeId::StoreAndForward => "[PKT+]",
            UpgradeId::EmergencyCallback => "[CALL]",
            UpgradeId::MissedCall => "[DECOY]",
            UpgradeId::SignalSpoofer => "[SPOOF]",
            UpgradeId::JammerHijack => "[HIJK]",
        }
    }
}

/// Draft 3 distinct upgrades from the full pool, skipping owned non-stackables
/// when the pool allows.
pub fn draft_choices(owned: &[UpgradeId], rng: &mut impl Rng) -> Vec<UpgradeId> {
    draft_from_pool(UpgradeId::ALL.to_vec(), owned, rng)
}

/// Draft 3 distinct upgrades from a run's active catalog only (an empty
/// catalog falls back to the 12 core apps). Never offers a card outside it.
pub fn draft_choices_from_catalog(catalog: &[UpgradeId], owned: &[UpgradeId], rng: &mut impl Rng) -> Vec<UpgradeId> {
    let mut pool: Vec<UpgradeId> = Vec::new();
    for &u in catalog {
        if !pool.contains(&u) {
            pool.push(u);
        }
    }
    if pool.is_empty() {
        pool = default_catalog();
    }
    draft_from_pool(pool, owned, rng)
}

fn draft_from_pool(pool: Vec<UpgradeId>, owned: &[UpgradeId], rng: &mut impl Rng) -> Vec<UpgradeId> {
    let (mut fresh, mut stale): (Vec<UpgradeId>, Vec<UpgradeId>) = pool.into_iter().partition(|u| u.is_stackable() || !owned.contains(u));
    fresh.shuffle(rng);
    stale.shuffle(rng);
    fresh.into_iter().chain(stale).take(3).collect()
}

/// Apply one upgrade. Capacity changes take effect immediately.
pub fn apply_upgrade(id: UpgradeId, mods: &mut PlayerModifiers, player: &mut Player, run: &mut RunStats) {
    match id {
        UpgradeId::DualSimBarrels => {
            mods.extra_pellets += 3;
            mods.pellet_spread_mult *= 0.75;
        }
        UpgradeId::FiveGOverclock => {
            mods.overclock_bonus += 0.35;
            mods.overclock_move_bonus += 0.25;
        }
        UpgradeId::CopperAntenna => mods.min_signal_floor = (mods.min_signal_floor + 1.25).min(4.0),
        UpgradeId::SignalSiphon => mods.siphon_heal += 8.0,
        UpgradeId::FirewallDaemon => {
            mods.max_armor_bonus += 40.0;
            mods.armor_regen_while_uploading += 6.0;
            player.max_armor += 40.0;
            player.armor = (player.armor + 40.0).min(player.max_armor);
        }
        UpgradeId::PacketSniffer => {
            mods.ping_cooldown_mult *= 0.5;
            mods.ping_mark_damage_bonus += 0.35;
        }
        UpgradeId::EmpOvercharge => {
            mods.emp_radius_mult *= 1.4;
            mods.emp_damage_mult *= 1.35;
            mods.emp_chains = true;
        }
        UpgradeId::AirplaneModePhase => {
            mods.dash_invulnerable = true;
            mods.dash_shockwave = true;
            mods.dash_cooldown_mult *= 0.85;
        }
        UpgradeId::HighCapacityCell => {
            mods.ammo_capacity_mult *= 1.35;
            mods.pickup_mult *= 1.3;
            player.max_shells = (BASE_MAX_SHELLS as f32 * mods.ammo_capacity_mult).round() as u32;
            player.max_plasma = (BASE_MAX_PLASMA as f32 * mods.ammo_capacity_mult).round() as u32;
            player.max_rockets = (BASE_MAX_ROCKETS as f32 * mods.ammo_capacity_mult).round() as u32;
        }
        UpgradeId::SubGhzPenetration => {
            // 55% less attenuation, multiplicative when stacked.
            mods.wall_penetration = 1.0 - (1.0 - mods.wall_penetration) * 0.45;
            mods.sub_ghz_route = true;
        }
        UpgradeId::DopplerOverdrive => {
            mods.doppler_damage_bonus += 0.30;
            mods.doppler_spread_mult *= 0.80;
        }
        UpgradeId::BlastThrusters => {
            mods.rocket_jump_mult += 0.25;
            mods.rocket_splash_mult += 0.20;
            mods.rocket_self_damage_mult *= 0.50;
        }
        UpgradeId::StoreAndForward => mods.store_and_forward = true,
        UpgradeId::EmergencyCallback => mods.emergency_callback = true,
        UpgradeId::MissedCall => mods.missed_call = true,
        UpgradeId::SignalSpoofer => mods.signal_spoofer = true,
        UpgradeId::JammerHijack => mods.jammer_hijack = true,
    }
    run.upgrades.push(id);
}

/// Short "what the next rank gives you" line for an App Store card, computed
/// from the current modifiers (so stacked ranks preview the real next step).
pub fn next_rank_preview(id: UpgradeId, mods: &PlayerModifiers) -> String {
    let mut after = mods.clone();
    let mut scratch = Player::default();
    apply_upgrade(id, &mut after, &mut scratch, &mut RunStats::default());
    let pct = |x: f32| (x * 100.0).round() as i32;
    let owned_toggle = |on: bool| (!id.is_stackable() && on).then(|| "Already installed".to_string());
    match id {
        UpgradeId::DualSimBarrels => {
            format!("{} -> {} pellets", SHOTGUN_PELLETS + mods.extra_pellets, SHOTGUN_PELLETS + after.extra_pellets)
        }
        UpgradeId::FiveGOverclock => format!("+{}% -> +{}% fire @4-5 bars", pct(mods.overclock_bonus), pct(after.overclock_bonus)),
        UpgradeId::CopperAntenna => {
            let (a, b) = (bars_from_strength(mods.min_signal_floor), bars_from_strength(after.min_signal_floor));
            if a == b && mods.min_signal_floor >= after.min_signal_floor {
                format!("Min {a} bars (MAX)")
            } else {
                format!("Min {a} -> {b} bar{}", if b == 1 { "" } else { "s" })
            }
        }
        UpgradeId::SignalSiphon => format!("+{} -> +{} HP per kill", mods.siphon_heal as i32, after.siphon_heal as i32),
        UpgradeId::FirewallDaemon => format!(
            "+{} -> +{} armor, {} -> {}/s",
            mods.max_armor_bonus as i32,
            after.max_armor_bonus as i32,
            mods.armor_regen_while_uploading as i32,
            after.armor_regen_while_uploading as i32
        ),
        UpgradeId::PacketSniffer => format!(
            "{:.1}s -> {:.1}s ping, +{}% dmg",
            ping_cooldown(mods),
            ping_cooldown(&after),
            pct(after.ping_mark_damage_bonus)
        ),
        UpgradeId::EmpOvercharge => owned_toggle(mods.emp_chains)
            .unwrap_or_else(|| format!("EMP {:.0}m -> {:.0}m, chains x3", emp_radius(mods), emp_radius(&after))),
        UpgradeId::AirplaneModePhase => owned_toggle(mods.dash_invulnerable).unwrap_or_else(|| {
            format!("Dash {:.1}s -> {:.1}s + invuln", DASH_COOLDOWN * mods.dash_cooldown_mult, DASH_COOLDOWN * after.dash_cooldown_mult)
        }),
        UpgradeId::HighCapacityCell => {
            let cap = |m: &PlayerModifiers| (BASE_MAX_SHELLS as f32 * m.ammo_capacity_mult).round() as u32;
            format!("{} -> {} shells, +{}% pickups", cap(mods), cap(&after), pct(after.pickup_mult - 1.0))
        }
        UpgradeId::SubGhzPenetration => format!(
            "Wall loss {}% -> {}% + route",
            pct(1.0 - mods.wall_penetration),
            pct(1.0 - after.wall_penetration)
        ),
        UpgradeId::DopplerOverdrive => {
            format!("+{}% dmg, -{}% choke @20m/s", pct(after.doppler_damage_bonus), pct(1.0 - after.doppler_spread_mult))
        }
        UpgradeId::BlastThrusters => {
            format!(
                "x{:.2} jmp, {:.0}% rds, {:.0}% self",
                after.rocket_jump_mult,
                after.rocket_splash_mult * 100.0,
                after.rocket_self_damage_mult * 100.0
            )
        }
        UpgradeId::StoreAndForward => owned_toggle(mods.store_and_forward)
            .unwrap_or_else(|| format!("Max {STORE_FORWARD_MAX_PACKETS} blackout packets, {STORE_FORWARD_DAMAGE:.0} dmg")),
        UpgradeId::EmergencyCallback => {
            owned_toggle(mods.emergency_callback).unwrap_or_else(|| format!("{CALLBACK_SECS:.0}s any-signal EMP window"))
        }
        UpgradeId::MissedCall => {
            owned_toggle(mods.missed_call).unwrap_or_else(|| format!("{DECOY_SECS:.1}s decoy, {DECOY_LURE_RADIUS:.0}m Imp lure"))
        }
        UpgradeId::SignalSpoofer => {
            owned_toggle(mods.signal_spoofer).unwrap_or_else(|| format!("Dash @0-1b -> 4b {SIGNAL_SPOOF_SECS:.1}s ({SIGNAL_SPOOF_COOLDOWN:.0}s CD)"))
        }
        UpgradeId::JammerHijack => {
            owned_toggle(mods.jammer_hijack).unwrap_or_else(|| format!("Jammer kill -> {HIJACK_SECS:.0}s 5-bar zone"))
        }
    }
}

/// Field repairs between floors: +25 HP, +15 armor, ammo topped up to >= 50%.
pub fn between_floor_patch(player: &mut Player) {
    patch_player(player, true);
}

/// [`between_floor_patch`] honoring the run's modifiers: the Low-Battery Mode
/// contract skips the HP repair (armor + ammo still top up).
pub fn between_floor_patch_with(player: &mut Player, mods: &PlayerModifiers) {
    patch_player(player, !mods.skip_floor_heal);
}

fn patch_player(player: &mut Player, heal: bool) {
    if heal {
        player.heal(25.0);
    }
    player.armor = (player.armor + 15.0).min(player.max_armor);
    player.ammo_shells = player.ammo_shells.max(player.max_shells / 2);
    player.ammo_plasma = player.ammo_plasma.max(player.max_plasma / 2);
    player.ammo_rockets = player.ammo_rockets.max(player.max_rockets / 2);
}

/// Choose offer `index` (0..3): applies it, patches the player, resumes play.
pub fn pick_upgrade(
    index: usize,
    choices: &UpgradeChoices,
    mods: &mut PlayerModifiers,
    player: Option<&mut Player>,
    run: &mut RunStats,
    next: &mut NextState<AppState>,
) -> Option<UpgradeId> {
    let id = *choices.0.get(index)?;
    let mut scratch = Player::default();
    let p = player.unwrap_or(&mut scratch);
    apply_upgrade(id, mods, p, run);
    between_floor_patch_with(p, mods);
    next.set(AppState::Playing);
    Some(id)
}

/// App Store visit: draft from the run's active 12-card catalog.
pub fn draft_upgrades(mut choices: ResMut<UpgradeChoices>, run: Res<RunStats>, loadout: Option<Res<RunLoadout>>) {
    let catalog = loadout.map(|l| l.catalog.clone()).unwrap_or_default();
    choices.0 = draft_choices_from_catalog(&catalog, &run.upgrades, &mut rand::rng());
}

/// Virtual "click a card" for the playtest bot / integration tests.
pub fn bot_pick_upgrade(
    bot: Option<ResMut<BotControl>>,
    choices: Res<UpgradeChoices>,
    mut mods: ResMut<PlayerModifiers>,
    mut run: ResMut<RunStats>,
    mut players: Query<&mut Player>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(i) = bot.and_then(|mut b| b.pick_upgrade.take()) else { return };
    let mut player = players.single_mut().ok();
    pick_upgrade(i, &choices, &mut mods, player.as_deref_mut(), &mut run, &mut next);
}

pub struct UpgradesPlugin;

impl Plugin for UpgradesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UpgradeChoices>()
            .add_systems(OnEnter(AppState::UpgradeSelect), draft_upgrades)
            .add_systems(Update, bot_pick_upgrade.run_if(in_state(AppState::UpgradeSelect)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn fresh() -> (PlayerModifiers, Player, RunStats) {
        (PlayerModifiers::default(), Player::default(), RunStats::default())
    }

    #[test]
    fn draft_is_three_distinct() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..200 {
            let c = draft_choices(&[], &mut rng);
            assert_eq!(c.len(), 3);
            assert!(c[0] != c[1] && c[1] != c[2] && c[0] != c[2]);
        }
    }

    #[test]
    fn draft_avoids_owned_non_stackables() {
        let mut rng = StdRng::seed_from_u64(9);
        let owned = [
            UpgradeId::AirplaneModePhase,
            UpgradeId::EmpOvercharge,
            UpgradeId::StoreAndForward,
            UpgradeId::EmergencyCallback,
            UpgradeId::MissedCall,
        ];
        for _ in 0..200 {
            let c = draft_choices(&owned, &mut rng);
            assert!(owned.iter().all(|o| !c.contains(o)), "{c:?}");
        }
        // Stackables can still be offered again.
        let owned = [UpgradeId::DualSimBarrels];
        let seen = (0..300).any(|_| draft_choices(&owned, &mut rng).contains(&UpgradeId::DualSimBarrels));
        assert!(seen);
    }

    #[test]
    fn all_seventeen_upgrades_apply() {
        assert_eq!(UpgradeId::ALL.len(), 17);
        for id in UpgradeId::ALL {
            let (mut m, mut p, mut r) = fresh();
            let before = m.clone();
            apply_upgrade(id, &mut m, &mut p, &mut r);
            assert_eq!(r.upgrades, vec![id]);
            assert_ne!(m, before, "{id:?} changed nothing");
        }
    }

    #[test]
    fn upgrade_values_match_design() {
        let (mut m, mut p, mut r) = fresh();
        apply_upgrade(UpgradeId::DualSimBarrels, &mut m, &mut p, &mut r);
        assert_eq!(m.extra_pellets, 3);
        assert!(m.pellet_spread_mult < 1.0);

        apply_upgrade(UpgradeId::FiveGOverclock, &mut m, &mut p, &mut r);
        assert!((m.overclock_bonus - 0.35).abs() < 1e-6);
        assert!((m.overclock_move_bonus - 0.25).abs() < 1e-6);

        apply_upgrade(UpgradeId::CopperAntenna, &mut m, &mut p, &mut r);
        assert_eq!(m.min_signal_floor, 1.25);

        apply_upgrade(UpgradeId::SignalSiphon, &mut m, &mut p, &mut r);
        assert_eq!(m.siphon_heal, 8.0);

        let armor_before = p.max_armor;
        apply_upgrade(UpgradeId::FirewallDaemon, &mut m, &mut p, &mut r);
        assert_eq!(p.max_armor, armor_before + 40.0);
        assert!(m.armor_regen_while_uploading > 0.0);
        assert!(p.armor <= p.max_armor);

        apply_upgrade(UpgradeId::PacketSniffer, &mut m, &mut p, &mut r);
        assert_eq!(m.ping_cooldown_mult, 0.5);
        assert!((m.ping_mark_damage_bonus - 0.35).abs() < 1e-6);

        apply_upgrade(UpgradeId::EmpOvercharge, &mut m, &mut p, &mut r);
        assert!((m.emp_radius_mult - 1.4).abs() < 1e-6);
        assert!(m.emp_damage_mult > 1.0 && m.emp_chains);

        apply_upgrade(UpgradeId::AirplaneModePhase, &mut m, &mut p, &mut r);
        assert!(m.dash_invulnerable && m.dash_shockwave);

        apply_upgrade(UpgradeId::HighCapacityCell, &mut m, &mut p, &mut r);
        assert_eq!(p.max_shells, 68);
        assert_eq!(p.max_plasma, 270);
        assert_eq!(p.max_rockets, 27);
        assert_eq!(m.fire_rate_mult, 1.0, "High-Capacity Cell no longer touches fire rate");
        assert!((m.pickup_mult - 1.3).abs() < 1e-6);

        assert!(!m.sub_ghz_route);
        apply_upgrade(UpgradeId::SubGhzPenetration, &mut m, &mut p, &mut r);
        assert!((m.wall_penetration - 0.55).abs() < 1e-6);
        assert!(m.sub_ghz_route);
        apply_upgrade(UpgradeId::SubGhzPenetration, &mut m, &mut p, &mut r);
        // 1 - 0.45^2 = 0.7975
        assert!((m.wall_penetration - 0.7975).abs() < 1e-5);

        apply_upgrade(UpgradeId::StoreAndForward, &mut m, &mut p, &mut r);
        apply_upgrade(UpgradeId::EmergencyCallback, &mut m, &mut p, &mut r);
        apply_upgrade(UpgradeId::MissedCall, &mut m, &mut p, &mut r);
        assert!(m.store_and_forward && m.emergency_callback && m.missed_call);
        assert_eq!(r.upgrades.len(), 14);
    }

    #[test]
    fn high_capacity_cell_keeps_fire_rate_neutral() {
        let (mut m, mut p, mut r) = fresh();
        for _ in 0..3 {
            apply_upgrade(UpgradeId::HighCapacityCell, &mut m, &mut p, &mut r);
        }
        assert_eq!(m.fire_rate_mult, 1.0);
        assert!(m.ammo_capacity_mult > 2.4);
    }

    #[test]
    fn non_stackables_are_the_seven_toggles() {
        let toggles: Vec<UpgradeId> = UpgradeId::ALL.into_iter().filter(|u| !u.is_stackable()).collect();
        assert_eq!(
            toggles,
            vec![
                UpgradeId::EmpOvercharge,
                UpgradeId::AirplaneModePhase,
                UpgradeId::StoreAndForward,
                UpgradeId::EmergencyCallback,
                UpgradeId::MissedCall,
                UpgradeId::SignalSpoofer,
                UpgradeId::JammerHijack
            ]
        );
        for id in UpgradeId::ALL {
            assert!(!id.icon().is_empty() && !id.name().is_empty() && !id.description().is_empty());
        }
    }

    #[test]
    fn next_rank_previews_are_meaningful() {
        let m = PlayerModifiers::default();
        for id in UpgradeId::ALL {
            let p = next_rank_preview(id, &m);
            assert!(p.len() >= 10 && p.len() <= 30, "{id:?}: {p:?} ({} chars)", p.len());
            assert_ne!(p, "Already installed", "{id:?} fresh preview");
        }
        assert_eq!(next_rank_preview(UpgradeId::DualSimBarrels, &m), "8 -> 11 pellets");
        assert_eq!(next_rank_preview(UpgradeId::CopperAntenna, &m), "Min 0 -> 1 bar");
        assert_eq!(next_rank_preview(UpgradeId::PacketSniffer, &m), "4.0s -> 2.0s ping, +35% dmg");
        assert_eq!(next_rank_preview(UpgradeId::StoreAndForward, &m), "Max 3 blackout packets, 20 dmg");
        assert_eq!(next_rank_preview(UpgradeId::SubGhzPenetration, &m), "Wall loss 100% -> 45% + route");
        assert_eq!(next_rank_preview(UpgradeId::HighCapacityCell, &m), "50 -> 68 shells, +30% pickups");
        assert_eq!(next_rank_preview(UpgradeId::FirewallDaemon, &m), "+0 -> +40 armor, 0 -> 6/s");
        assert_eq!(next_rank_preview(UpgradeId::SignalSpoofer, &m), "Dash @0-1b -> 4b 2.5s (6s CD)");
        assert_eq!(next_rank_preview(UpgradeId::JammerHijack, &m), "Jammer kill -> 8s 5-bar zone");
        assert_eq!(next_rank_preview(UpgradeId::DopplerOverdrive, &m), "+30% dmg, -20% choke @20m/s");
        assert_eq!(next_rank_preview(UpgradeId::BlastThrusters, &m), "x1.25 jmp, 120% rds, 50% self");

        // Ranks stack: the preview follows the current modifiers.
        let (mut m2, mut p, mut r) = fresh();
        apply_upgrade(UpgradeId::DualSimBarrels, &mut m2, &mut p, &mut r);
        apply_upgrade(UpgradeId::CopperAntenna, &mut m2, &mut p, &mut r);
        apply_upgrade(UpgradeId::MissedCall, &mut m2, &mut p, &mut r);
        assert_eq!(next_rank_preview(UpgradeId::DualSimBarrels, &m2), "11 -> 14 pellets");
        assert_eq!(next_rank_preview(UpgradeId::CopperAntenna, &m2), "Min 1 -> 2 bars");
        assert_eq!(next_rank_preview(UpgradeId::MissedCall, &m2), "Already installed");
        for _ in 0..4 {
            apply_upgrade(UpgradeId::CopperAntenna, &mut m2, &mut p, &mut r);
        }
        assert_eq!(next_rank_preview(UpgradeId::CopperAntenna, &m2), "Min 4 bars (MAX)");
    }

    #[test]
    fn movement_synergy_upgrades_stack() {
        let (mut m, mut p, mut r) = fresh();
        apply_upgrade(UpgradeId::DopplerOverdrive, &mut m, &mut p, &mut r);
        assert!((m.doppler_damage_bonus - 0.30).abs() < 1e-6);
        assert!((m.doppler_spread_mult - 0.80).abs() < 1e-6);
        apply_upgrade(UpgradeId::DopplerOverdrive, &mut m, &mut p, &mut r);
        assert!((m.doppler_damage_bonus - 0.60).abs() < 1e-6);
        assert!((m.doppler_spread_mult - 0.64).abs() < 1e-6);
        assert_eq!(next_rank_preview(UpgradeId::DopplerOverdrive, &m), "+90% dmg, -49% choke @20m/s");

        apply_upgrade(UpgradeId::BlastThrusters, &mut m, &mut p, &mut r);
        assert!((m.rocket_jump_mult - 1.25).abs() < 1e-6);
        assert!((m.rocket_splash_mult - 1.20).abs() < 1e-6);
        assert!((m.rocket_self_damage_mult - 0.50).abs() < 1e-6);
        apply_upgrade(UpgradeId::BlastThrusters, &mut m, &mut p, &mut r);
        assert!((m.rocket_jump_mult - 1.50).abs() < 1e-6);
        assert!((m.rocket_splash_mult - 1.40).abs() < 1e-6);
        assert!((m.rocket_self_damage_mult - 0.25).abs() < 1e-6);
        assert!(UpgradeId::DopplerOverdrive.is_stackable() && UpgradeId::BlastThrusters.is_stackable());
        assert!(UpgradeId::DopplerOverdrive.is_core() && UpgradeId::BlastThrusters.is_core());
        assert_eq!(UpgradeId::DopplerOverdrive.icon(), "[DOP]");
        assert_eq!(UpgradeId::BlastThrusters.icon(), "[RKT]");
    }

    #[test]
    fn patch_tops_up_rockets() {
        let mut p = Player { ammo_rockets: 0, ..default() };
        between_floor_patch(&mut p);
        assert_eq!(p.ammo_rockets, BASE_MAX_ROCKETS / 2);
        let mut q = Player { ammo_rockets: 18, ..default() };
        between_floor_patch(&mut q);
        assert_eq!(q.ammo_rockets, 18, "never lowers a fuller rack");
    }

    #[test]
    fn patch_heals_and_tops_up() {
        let mut p = Player { hp: 30.0, armor: 0.0, ammo_shells: 0, ammo_plasma: 150, ..default() };
        between_floor_patch(&mut p);
        assert_eq!(p.hp, 55.0);
        assert_eq!(p.armor, 15.0);
        assert_eq!(p.ammo_shells, p.max_shells / 2);
        assert_eq!(p.ammo_plasma, 150);
        p.hp = 95.0;
        between_floor_patch(&mut p);
        assert_eq!(p.hp, p.max_hp);
    }

    #[test]
    fn signature_toggles_install_once() {
        let (mut m, mut p, mut r) = fresh();
        apply_upgrade(UpgradeId::SignalSpoofer, &mut m, &mut p, &mut r);
        apply_upgrade(UpgradeId::JammerHijack, &mut m, &mut p, &mut r);
        assert!(m.signal_spoofer && m.jammer_hijack);
        assert_eq!(next_rank_preview(UpgradeId::SignalSpoofer, &m), "Already installed");
        assert_eq!(next_rank_preview(UpgradeId::JammerHijack, &m), "Already installed");
        assert_eq!(UpgradeId::SignalSpoofer.icon(), "[SPOOF]");
        assert_eq!(UpgradeId::JammerHijack.icon(), "[HIJK]");
    }

    #[test]
    fn catalog_draft_never_leaves_the_catalog() {
        let mut rng = StdRng::seed_from_u64(21);
        let catalog = [
            UpgradeId::SignalSpoofer,
            UpgradeId::JammerHijack,
            UpgradeId::CopperAntenna,
            UpgradeId::DualSimBarrels,
            UpgradeId::SignalSpoofer,
            UpgradeId::PacketSniffer,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..400 {
            let c = draft_choices_from_catalog(&catalog, &[], &mut rng);
            assert_eq!(c.len(), 3);
            assert!(c[0] != c[1] && c[1] != c[2] && c[0] != c[2], "{c:?}");
            assert!(c.iter().all(|u| catalog.contains(u)), "{c:?} outside the catalog");
            seen.extend(c);
        }
        assert_eq!(seen.len(), 5, "every distinct catalog card gets offered");
        // Owned toggles are skipped while enough fresh cards remain.
        let owned = [UpgradeId::SignalSpoofer, UpgradeId::JammerHijack];
        for _ in 0..200 {
            let c = draft_choices_from_catalog(&catalog, &owned, &mut rng);
            assert!(!c.contains(&UpgradeId::SignalSpoofer) && !c.contains(&UpgradeId::JammerHijack), "{c:?}");
        }
        // Empty catalog falls back to the 12 core apps.
        for _ in 0..200 {
            let c = draft_choices_from_catalog(&[], &[], &mut rng);
            assert!(c.iter().all(|u| u.is_core()), "{c:?}");
        }
        // The unrestricted draft still reaches the full pool.
        let full: std::collections::BTreeSet<UpgradeId> = (0..600).flat_map(|_| draft_choices(&[], &mut rng)).collect();
        assert_eq!(full.len(), UpgradeId::ALL.len());
    }

    #[test]
    fn low_battery_mode_skips_floor_heal_only() {
        let lowbat = PlayerModifiers { skip_floor_heal: true, ..default() };
        let mut p = Player { hp: 30.0, armor: 0.0, ammo_shells: 0, ammo_plasma: 0, ..default() };
        between_floor_patch_with(&mut p, &lowbat);
        assert_eq!(p.hp, 30.0, "no HP repair under Low-Battery Mode");
        assert_eq!(p.armor, 15.0);
        assert_eq!(p.ammo_shells, p.max_shells / 2);
        assert_eq!(p.ammo_plasma, p.max_plasma / 2);
        let mut q = Player { hp: 30.0, ..default() };
        between_floor_patch_with(&mut q, &PlayerModifiers::default());
        assert_eq!(q.hp, 55.0);
        // pick_upgrade goes through the contract-aware patch.
        let (mut m, mut pl, mut r) = fresh();
        m.skip_floor_heal = true;
        pl.hp = 40.0;
        let choices = UpgradeChoices(vec![UpgradeId::CopperAntenna, UpgradeId::DualSimBarrels, UpgradeId::SignalSiphon]);
        let mut next = NextState::<AppState>::default();
        pick_upgrade(0, &choices, &mut m, Some(&mut pl), &mut r, &mut next);
        assert_eq!(pl.hp, 40.0);
    }

    #[test]
    fn pick_upgrade_applies_and_resumes() {
        let (mut m, mut p, mut r) = fresh();
        let choices = UpgradeChoices(vec![UpgradeId::SignalSiphon, UpgradeId::DualSimBarrels, UpgradeId::CopperAntenna]);
        let mut next = NextState::<AppState>::default();
        assert_eq!(pick_upgrade(1, &choices, &mut m, Some(&mut p), &mut r, &mut next), Some(UpgradeId::DualSimBarrels));
        assert_eq!(m.extra_pellets, 3);
        assert!(matches!(next, NextState::Pending(AppState::Playing)));
        assert_eq!(pick_upgrade(5, &choices, &mut m, Some(&mut p), &mut r, &mut next), None);
    }
}
