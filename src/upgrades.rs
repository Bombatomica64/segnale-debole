//! Roguelike "App Store" upgrades: drafting 3 distinct offers, applying the
//! chosen one to [`PlayerModifiers`] / [`Player`], and the between-floor patch.
//! The 3-card UI lives in `hud.rs` and calls [`pick_upgrade`].

use bevy::prelude::*;
use rand::Rng;
use rand::seq::SliceRandom;

use crate::bot_input::BotControl;
use crate::player::Player;
use crate::state::{AppState, PlayerModifiers, RunStats, UpgradeId};

pub const BASE_MAX_SHELLS: u32 = 50;
pub const BASE_MAX_PLASMA: u32 = 200;

/// The three offers of the current App Store visit.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct UpgradeChoices(pub Vec<UpgradeId>);

impl UpgradeId {
    /// Non-stackable upgrades only toggle behaviour; re-buying is pointless.
    pub fn stackable(self) -> bool {
        !matches!(self, UpgradeId::AirplaneModePhase | UpgradeId::EmpOvercharge)
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
        }
    }
}

/// Draft 3 distinct upgrades, skipping owned non-stackables when the pool allows.
pub fn draft_choices(owned: &[UpgradeId], rng: &mut impl Rng) -> Vec<UpgradeId> {
    let (mut fresh, mut stale): (Vec<UpgradeId>, Vec<UpgradeId>) =
        UpgradeId::ALL.into_iter().partition(|u| u.stackable() || !owned.contains(u));
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
            mods.fire_rate_mult *= 1.15;
            mods.pickup_mult *= 1.3;
            player.max_shells = (BASE_MAX_SHELLS as f32 * mods.ammo_capacity_mult).round() as u32;
            player.max_plasma = (BASE_MAX_PLASMA as f32 * mods.ammo_capacity_mult).round() as u32;
        }
        UpgradeId::SubGhzPenetration => {
            // 45% less attenuation, multiplicative when stacked.
            mods.wall_penetration = 1.0 - (1.0 - mods.wall_penetration) * 0.55;
        }
    }
    run.upgrades.push(id);
}

/// Field repairs between floors: +25 HP, +15 armor, ammo topped up to >= 50%.
pub fn between_floor_patch(player: &mut Player) {
    player.heal(25.0);
    player.armor = (player.armor + 15.0).min(player.max_armor);
    player.ammo_shells = player.ammo_shells.max(player.max_shells / 2);
    player.ammo_plasma = player.ammo_plasma.max(player.max_plasma / 2);
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
    between_floor_patch(p);
    next.set(AppState::Playing);
    Some(id)
}

pub fn draft_upgrades(mut choices: ResMut<UpgradeChoices>, run: Res<RunStats>) {
    choices.0 = draft_choices(&run.upgrades, &mut rand::rng());
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
        let owned = [UpgradeId::AirplaneModePhase, UpgradeId::EmpOvercharge];
        for _ in 0..200 {
            let c = draft_choices(&owned, &mut rng);
            assert!(!c.contains(&UpgradeId::AirplaneModePhase));
            assert!(!c.contains(&UpgradeId::EmpOvercharge));
        }
        // Stackables can still be offered again.
        let owned = [UpgradeId::DualSimBarrels];
        let seen = (0..300).any(|_| draft_choices(&owned, &mut rng).contains(&UpgradeId::DualSimBarrels));
        assert!(seen);
    }

    #[test]
    fn all_ten_upgrades_apply() {
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
        assert!((m.fire_rate_mult - 1.15).abs() < 1e-6);
        assert!(m.pickup_mult > 1.0);

        apply_upgrade(UpgradeId::SubGhzPenetration, &mut m, &mut p, &mut r);
        assert!((m.wall_penetration - 0.45).abs() < 1e-6);
        apply_upgrade(UpgradeId::SubGhzPenetration, &mut m, &mut p, &mut r);
        assert!(m.wall_penetration > 0.45 && m.wall_penetration < 1.0);
        assert_eq!(r.upgrades.len(), 11);
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
