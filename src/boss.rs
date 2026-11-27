//! THE CARRIER DAEMON: the Floor 5 finale and Endless boss-floor (10, 15,
//! 20...) rematch.
//!
//! Spawned in the Signal Atrium the moment the SOS uplink starts on a boss
//! floor; the uplink then holds at 99% until it dies (see
//! [`crate::extraction`]). A pure phase machine drives the fight:
//!
//! `Phase1 (100-65%) -> Shielded1 -> Phase2 (65-30%) -> Shielded2 -> Overdrive`
//!
//! Crossing a threshold clamps HP there and raises the Signal Shield (full
//! immunity) while two destructible [`BossPylon`]s stand and tether to the
//! boss. Destroying both shatters the shield: 1.8 s stun, supply drop and the
//! next phase. Every mesh / material is pre-baked in [`EnemyAssets`].

use bevy::prelude::*;
use rand::Rng;

use crate::audio::{SfxEvent, SfxQueue};
use crate::enemies::{spawn_enemy, spawn_enemy_projectile, spawn_pos_for, spawn_rig, standable, Enemy, EnemyAssets, EnemyKind};
use crate::fx::{spawn_burst, spawn_flash_light, spawn_shockwave, BurstColor, FxAssets, HudFeed};
use crate::level::{Cell, CullableLight, LevelEntity, LevelGrid};
use crate::physics::has_line_of_sight;
use crate::player::Player;
use crate::state::GameSet;
use crate::weapons::{spawn_pickup, CombatDamage, PickupAssets, PickupKind};

pub const BOSS_ARRIVAL_TEXT: &str = "WARNING: THE CARRIER DAEMON HAS ARRIVED!";
/// Uplink banner / alert while the SOS upload is held at 99% for the boss.
pub const BOSS_HOLD_TEXT: &str = "UPLINK HELD AT 99% // KILL THE CARRIER DAEMON TO EXTRACT";
pub const BOSS_DEATH_TEXT: &str = "CARRIER DAEMON TERMINATED - UPLINK UNLOCKED";
pub const BOSS_SHIELDED_TEXT: &str = "SHIELDED - DESTROY THE PYLONS";
/// HP fraction ending Phase I (Signal Shield #1).
pub const PHASE1_THRESHOLD: f32 = 0.65;
/// HP fraction ending Phase II (Signal Shield #2).
pub const PHASE2_THRESHOLD: f32 = 0.30;
/// Stun when a Signal Shield shatters.
pub const BOSS_SHIELD_STUN: f32 = 1.8;
/// Seconds after raising a shield before "0 pylons alive" can shatter it
/// (the pylons are spawned through deferred commands).
const PYLON_GRACE: f32 = 0.3;
/// Pylon hit volumes `(height above base, radius)`: overlapping so a
/// horizontal ray at every height from the base plinth (0.05) to the crystal
/// crown (2.95) hits — no gap at the upper collar (y ~ 2.06).
pub const PYLON_SPHERES: [(f32, f32); 3] = [(0.55, 0.85), (1.45, 0.65), (2.48, 0.62)];
/// Height of the tether emitter on a pylon.
const PYLON_TOP: f32 = 2.6;
/// Height of the boss's recessed Signal Core (attack origin; matches the sculpt).
const CORE_HEIGHT: f32 = 2.74;
/// Height of the upper Signal Shield collar the pylon tethers lock onto (kept
/// above the chest core so the beams never cross over it).
const TETHER_ANCHOR_HEIGHT: f32 = 3.55;
/// Signal Shield shell centre height and rest scale (fits the colossus + halo).
const SHELL_HEIGHT: f32 = 2.55;
const SHELL_SCALE: Vec3 = Vec3::new(2.25, 2.85, 2.25);

const SHIELD_GLOW: (Color, f32) = (Color::srgb(0.55, 0.6, 1.0), 0.85);
const SHATTER_GLOW: (Color, f32) = (Color::srgb(1.0, 0.75, 0.3), 0.85);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BossPhase {
    #[default]
    Phase1,
    Shielded1,
    Phase2,
    Shielded2,
    Overdrive,
}

impl BossPhase {
    pub fn shielded(self) -> bool {
        matches!(self, BossPhase::Shielded1 | BossPhase::Shielded2)
    }

    /// HP fraction damage cannot push the boss below in this phase.
    pub fn hp_floor_frac(self) -> f32 {
        match self {
            BossPhase::Phase1 | BossPhase::Shielded1 => PHASE1_THRESHOLD,
            BossPhase::Phase2 | BossPhase::Shielded2 => PHASE2_THRESHOLD,
            BossPhase::Overdrive => 0.0,
        }
    }

    /// Phase after the boss's HP dropped to `hp_frac` (threshold crossings).
    pub fn after_hp(self, hp_frac: f32) -> BossPhase {
        match self {
            BossPhase::Phase1 if hp_frac <= PHASE1_THRESHOLD => BossPhase::Shielded1,
            BossPhase::Phase2 if hp_frac <= PHASE2_THRESHOLD => BossPhase::Shielded2,
            p => p,
        }
    }

    /// Phase after every Shield Pylon of a shielded phase was destroyed.
    pub fn after_pylons_cleared(self) -> BossPhase {
        match self {
            BossPhase::Shielded1 => BossPhase::Phase2,
            BossPhase::Shielded2 => BossPhase::Overdrive,
            p => p,
        }
    }

    pub fn speed_mult(self) -> f32 {
        match self {
            BossPhase::Phase1 | BossPhase::Shielded1 => 1.0,
            BossPhase::Phase2 | BossPhase::Shielded2 => 1.25,
            BossPhase::Overdrive => 1.45,
        }
    }

    /// Seconds between attack telegraphs.
    pub fn attack_cooldown(self) -> f32 {
        match self {
            BossPhase::Phase1 => 2.6,
            BossPhase::Shielded1 => 3.2,
            BossPhase::Phase2 => 2.0,
            BossPhase::Shielded2 => 2.6,
            BossPhase::Overdrive => 1.1,
        }
    }

    /// Half-angle (radians) of the 5-bolt Hellfire Arc Barrage.
    pub fn barrage_spread(self) -> f32 {
        match self {
            BossPhase::Phase1 | BossPhase::Shielded1 => 0.36,
            BossPhase::Phase2 | BossPhase::Shielded2 => 0.24,
            BossPhase::Overdrive => 0.28,
        }
    }

    /// Bolts in one EMP Volley ring.
    pub fn emp_bolts(self) -> usize {
        match self {
            BossPhase::Phase1 | BossPhase::Shielded1 => 10,
            BossPhase::Phase2 | BossPhase::Shielded2 => 12,
            BossPhase::Overdrive => 14,
        }
    }
}

/// HUD phase tag: `PHASE I`, `SHIELDED // 2 PYLONS`, `PHASE II`, `OVERDRIVE`.
pub fn boss_phase_tag(phase: BossPhase, pylons: usize) -> String {
    match phase {
        BossPhase::Phase1 => "PHASE I".into(),
        BossPhase::Phase2 => "PHASE II".into(),
        BossPhase::Overdrive => "OVERDRIVE".into(),
        BossPhase::Shielded1 | BossPhase::Shielded2 => format!("SHIELDED // {pylons} PYLON{}", if pylons == 1 { "" } else { "S" }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BossAttack {
    HellfireBarrage,
    EmpVolley,
}

/// Barrage telegraph length outside / inside Overdrive (seconds).
pub const BOSS_TELEGRAPH_SECS: f32 = 0.6;
pub const BOSS_OVERDRIVE_TELEGRAPH_SECS: f32 = 0.35;

/// Barrage telegraph length of a phase.
pub fn boss_telegraph_secs(phase: BossPhase) -> f32 {
    if phase == BossPhase::Overdrive { BOSS_OVERDRIVE_TELEGRAPH_SECS } else { BOSS_TELEGRAPH_SECS }
}

/// Carrier Daemon fight state (on the boss's root entity).
#[derive(Component, Debug, Clone)]
pub struct BossState {
    pub phase: BossPhase,
    pub attack_timer: f32,
    /// Remaining attack telegraph (> 0 while charging `next_attack`).
    pub telegraph: f32,
    pub next_attack: BossAttack,
    pub pylons_alive: usize,
    /// Grace after a shield goes up before it can shatter.
    pub raise_grace: f32,
    pub last_shield_hits: u32,
    pub spark_cooldown: f32,
    pub toast_cooldown: f32,
    pub supply_idx: usize,
    /// Translucent shield shell child.
    pub shell: Entity,
}

impl BossState {
    pub fn new(shell: Entity) -> Self {
        BossState {
            phase: BossPhase::Phase1,
            attack_timer: 2.0,
            telegraph: 0.0,
            next_attack: BossAttack::HellfireBarrage,
            pylons_alive: 0,
            raise_grace: 0.0,
            last_shield_hits: 0,
            spark_cooldown: 0.0,
            toast_cooldown: 0.0,
            supply_idx: 0,
            shell,
        }
    }
}

/// What a phase step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BossTransition {
    None,
    /// A threshold was crossed: Signal Shield up, spawn 2 pylons.
    RaiseShield,
    /// Every pylon fell: shield shattered, boss stunned.
    Shatter,
}

/// Pure phase machine step: apply the boss's HP and the living pylon count,
/// update the shield / HP floor / speed on `enemy`. Dead bosses never change.
pub fn advance_boss_phase(state: &mut BossState, enemy: &mut Enemy, pylons_alive: usize, dt: f32) -> BossTransition {
    state.raise_grace = (state.raise_grace - dt).max(0.0);
    state.pylons_alive = pylons_alive;
    if !enemy.is_alive() {
        return BossTransition::None;
    }
    let frac = enemy.hp / enemy.max_hp.max(1.0);
    let mut out = BossTransition::None;
    if state.phase.shielded() {
        if state.raise_grace <= 0.0 && pylons_alive == 0 {
            state.phase = state.phase.after_pylons_cleared();
            enemy.stun_timer = enemy.stun_timer.max(BOSS_SHIELD_STUN);
            out = BossTransition::Shatter;
        }
    } else {
        let next = state.phase.after_hp(frac);
        if next != state.phase {
            state.phase = next;
            state.raise_grace = PYLON_GRACE;
            state.telegraph = 0.0;
            state.attack_timer = state.attack_timer.max(1.5);
            out = BossTransition::RaiseShield;
        }
    }
    let floor_hp = enemy.max_hp * state.phase.hp_floor_frac();
    enemy.hp_floor = floor_hp;
    if state.phase.shielded() {
        // Clamp exactly on the threshold crossing (direct writes included).
        enemy.hp = enemy.hp.max(floor_hp);
    }
    enemy.shielded = state.phase.shielded();
    enemy.speed_mult = state.phase.speed_mult();
    out
}

/// A destructible Signal Shield pylon tethered to the boss.
#[derive(Component, Debug, Clone)]
pub struct BossPylon {
    pub hp: f32,
    pub max_hp: f32,
    pub hit_flash: f32,
    pub boss: Entity,
}

impl BossPylon {
    pub fn is_alive(&self) -> bool {
        self.hp > 0.0
    }
}

/// Shield Pylon HP on a floor.
pub fn pylon_hp(floor: u32) -> f32 {
    110.0 + 15.0 * floor.max(1) as f32
}

/// Damage a pylon (weapons, rockets, barrels, EMP). Returns true if this destroyed it.
pub fn damage_pylon(p: &mut BossPylon, amount: f32) -> bool {
    if !p.is_alive() || amount <= 0.0 {
        return false;
    }
    p.hp -= amount;
    p.hit_flash = 0.15;
    !p.is_alive()
}

/// Ray vs a pylon's hit spheres standing on `base`.
pub fn pylon_ray_hit(base: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    PYLON_SPHERES
        .iter()
        .filter_map(|&(h, r)| crate::enemies::ray_sphere(origin, dir, base + Vec3::Y * h, r))
        .reduce(f32::min)
}

/// Cyan-violet tether beam (child of a pylon, stretched to the boss core).
#[derive(Component)]
pub struct BossTether;

/// Translucent shield shell (child of the boss root).
#[derive(Component)]
pub struct BossShieldShell;

/// Carrier Daemon multi-tiered broadcast halo rig (spun in `enemy_visuals`).
#[derive(Component)]
pub struct BossHaloPivot;

/// Carrier Daemon twin shoulder plasma cannon rig (breathes / recoils in `enemy_visuals`).
#[derive(Component)]
pub struct BossCannonPivot;

/// Shield Pylon orbiting containment ring (spun in [`carrier_daemon_system`]).
#[derive(Component)]
pub struct PylonRingPivot;

/// Baked Shield Pylon obelisk (child of the [`BossPylon`] root). Only this
/// visual pulses on hit flash, so the root — and the [`BossTether`] living in
/// its space — always stays unscaled and locked onto the boss.
#[derive(Component)]
pub struct PylonVisual;

/// Height the pylon ring orbits around.
const PYLON_RING_HEIGHT: f32 = 2.55;

/// Boss plate HUD snapshot (updated every frame by [`carrier_daemon_system`]).
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct BossHud {
    pub active: bool,
    pub phase: BossPhase,
    pub hp_frac: f32,
    pub pylons: usize,
}

impl BossHud {
    pub fn tag(&self) -> String {
        boss_phase_tag(self.phase, self.pylons)
    }
}

/// Boss components + shield shell on a freshly spawned Carrier Daemon root.
pub fn attach_boss(commands: &mut Commands, a: &EnemyAssets, root: Entity, enemy: &mut Enemy) {
    let shell = commands
        .spawn((
            ChildOf(root),
            BossShieldShell,
            Mesh3d(a.boss_shield_mesh.clone()),
            MeshMaterial3d(a.boss_shield.clone()),
            Transform::from_xyz(0.0, SHELL_HEIGHT, 0.0).with_scale(SHELL_SCALE),
            Visibility::Hidden,
        ))
        .id();
    enemy.hp_floor = enemy.max_hp * BossPhase::Phase1.hp_floor_frac();
    commands.entity(root).insert(BossState::new(shell));
}

/// Spawn one Shield Pylon (pre-baked relay obelisk + orbiting ring + tether + light).
pub fn spawn_pylon(commands: &mut Commands, a: &EnemyAssets, pos: Vec3, boss: Entity, floor: u32) -> Entity {
    let hp = pylon_hp(floor);
    let root = commands
        .spawn((LevelEntity, BossPylon { hp, max_hp: hp, hit_flash: 0.0, boss }, Transform::from_translation(pos), Visibility::default()))
        .id();
    // The visual carries the first baked part itself; further materials are its children.
    let mut visual = commands.spawn((ChildOf(root), PylonVisual, Transform::IDENTITY, Visibility::default()));
    if let Some(first) = a.pylon_model.first() {
        visual.insert((Mesh3d(first.mesh.clone()), MeshMaterial3d(first.material.clone())));
    }
    let visual = visual.id();
    for part in a.pylon_model.iter().skip(1) {
        commands.spawn((ChildOf(visual), Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), Transform::IDENTITY));
    }
    spawn_rig(commands, root, &a.pylon_ring);
    commands.spawn((
        ChildOf(root),
        BossTether,
        Mesh3d(a.cylinder.clone()),
        MeshMaterial3d(a.boss_tether.clone()),
        Transform::from_xyz(0.0, PYLON_TOP, 0.0),
        Visibility::Hidden,
    ));
    commands.spawn((
        ChildOf(root),
        CullableLight::for_range(9.0),
        PointLight { color: Color::srgb(0.55, 0.6, 1.0), intensity: 350_000.0, range: 9.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, 2.0, 0.0),
    ));
    root
}

fn walkable_ground(grid: &LevelGrid, c: IVec2) -> bool {
    matches!(grid.cell(c), Cell::AtriumFloor | Cell::Floor { .. }) && !grid.jump_pads.iter().any(|p| p.cell == c)
}

/// Signal Atrium cells a boss-sized body (or pylon) can stand on, reachable
/// from the airlock and clear of the tower pedestal.
fn atrium_cells(grid: &LevelGrid, clearance: f32) -> Vec<IVec2> {
    let Some(atrium) = grid.rooms.first() else { return Vec::new() };
    (0..atrium.size.y)
        .flat_map(|z| (0..atrium.size.x).map(move |x| atrium.min + IVec2::new(x, z)))
        .filter(|&c| walkable_ground(grid, c) && (c - grid.tower).abs().max_element() >= 2)
        .filter(|&c| standable(grid, grid.cell_center(c), clearance).is_some() && grid.is_reachable(grid.spawn, c))
        .collect()
}

/// Where the Carrier Daemon lands: the Signal Atrium cell farthest from the
/// player that fits its 1.2 m body (any reachable rush cell as a fallback).
pub fn carrier_spawn_cell(grid: &LevelGrid, player_pos: Vec3) -> Option<IVec2> {
    let far = |cells: Vec<IVec2>| cells.into_iter().max_by(|a, b| grid.cell_center(*a).distance(player_pos).total_cmp(&grid.cell_center(*b).distance(player_pos)));
    far(atrium_cells(grid, 1.3)).or_else(|| {
        far((0..grid.depth)
            .flat_map(|z| (0..grid.width).map(move |x| IVec2::new(x, z)))
            .filter(|&c| walkable_ground(grid, c) && c != grid.tower && standable(grid, grid.cell_center(c), 1.3).is_some())
            .collect())
    })
}

/// Two Shield Pylon spots on opposite sides of the Signal Atrium: east / west
/// for the first shield, opposite diagonal corners for the second. Falls back
/// to cells around `fallback` (the boss) on maps without an atrium.
pub fn pylon_positions(grid: &LevelGrid, fallback: Vec3, second: bool) -> [Vec3; 2] {
    let mut cells = atrium_cells(grid, 0.9);
    let center = if cells.len() < 2 {
        let bc = grid.world_to_cell(fallback);
        cells = (-4..=4)
            .flat_map(|z| (-4..=4).map(move |x| bc + IVec2::new(x, z)))
            .filter(|&c| {
                walkable_ground(grid, c)
                    && c != grid.tower
                    && standable(grid, grid.cell_center(c), 0.9).is_some()
                    && grid.is_reachable(grid.spawn, c)
            })
            .collect();
        fallback
    } else {
        grid.tower_world()
    };
    let axis = if second { Vec3::new(1.0, 0.0, 1.0).normalize() } else { Vec3::X };
    let perp = Vec3::new(-axis.z, 0.0, axis.x);
    let pick = |dir: Vec3, avoid: Option<IVec2>| {
        cells
            .iter()
            .copied()
            .filter(|c| Some(*c) != avoid)
            .max_by(|a, b| {
                let score = |c: &IVec2| {
                    let d = grid.cell_center(*c) - center;
                    // Corners: as far along the axis as possible; sides: also stay near the middle line.
                    d.dot(dir) - if second { 0.0 } else { 0.35 * d.dot(perp).abs() }
                };
                score(a).total_cmp(&score(b))
            })
    };
    let a = pick(axis, None);
    let b = pick(-axis, a).or(a);
    let at = |c: Option<IVec2>| match c {
        Some(c) => grid.cell_center(c),
        None => fallback,
    };
    [at(a), at(b)]
}

fn prewarm_boss_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[SHIELD_GLOW, SHATTER_GLOW]);
}

/// Fire one boss attack pattern from the Signal Core toward `target`.
fn fire_boss_attack(commands: &mut Commands, fx: &FxAssets, e: &Enemy, phase: BossPhase, attack: BossAttack, core: Vec3, target: Vec3) {
    let fwd = (target - core).normalize_or(Vec3::NEG_Z);
    match attack {
        BossAttack::HellfireBarrage => {
            let spread = phase.barrage_spread();
            for k in 0..5 {
                let a = -spread + spread * 0.5 * k as f32;
                let d = Quat::from_rotation_y(a) * fwd;
                spawn_enemy_projectile(commands, fx, core + d * 1.6, d * e.stats.proj_speed, e.stats.proj_damage, e.stats.proj_radius, BurstColor::Fire);
            }
        }
        BossAttack::EmpVolley => {
            let n = phase.emp_bolts();
            let flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z);
            let tilt = fwd.y.clamp(-0.4, 0.2);
            for k in 0..n {
                let d = (Quat::from_rotation_y(k as f32 * std::f32::consts::TAU / n as f32) * flat + Vec3::Y * tilt).normalize();
                spawn_enemy_projectile(commands, fx, core + d * 1.6, d * (e.stats.proj_speed + 3.0), e.stats.proj_damage * 0.7, 0.3, BurstColor::Cyan);
            }
        }
    }
}

/// Spawn a demon on a standable spot near `pos` (Phase II escort).
fn spawn_escort(commands: &mut Commands, ea: &EnemyAssets, materials: &mut Assets<StandardMaterial>, grid: &LevelGrid, kind: EnemyKind, pos: Vec3, floor: u32) {
    let base = grid.world_to_cell(pos);
    let cell = (1..=3)
        .flat_map(|r| [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y].map(|o| base + o * r))
        .find(|c| walkable_ground(grid, *c) && standable(grid, grid.cell_center(*c), 0.6).is_some());
    if let Some(c) = cell {
        spawn_enemy(commands, ea, materials, kind, spawn_pos_for(kind, grid, c), floor, true);
    }
}

/// The Carrier Daemon fight: phase machine, pylons + tethers, shield shell,
/// shield feedback, attack patterns, stray-pylon cleanup and the HUD plate.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn carrier_daemon_system(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    (ea, fx, pickups): (Res<EnemyAssets>, Res<FxAssets>, Option<Res<PickupAssets>>),
    mut materials: ResMut<Assets<StandardMaterial>>,
    (mut sfx, mut feed, mut hud, freeze): (ResMut<SfxQueue>, ResMut<HudFeed>, ResMut<BossHud>, Option<Res<crate::dev::DevFreezeAi>>),
    players: Query<&Transform, With<Player>>,
    mut bosses: Query<(Entity, &mut Enemy, &mut BossState, &Transform), Without<Player>>,
    mut pylons: Query<(Entity, &mut BossPylon, &Transform, &Children), (Without<Enemy>, Without<Player>)>,
    mut tethers: Query<(&mut Transform, &mut Visibility), (With<BossTether>, Without<BossPylon>, Without<Enemy>, Without<Player>)>,
    mut visuals: Query<
        &mut Transform,
        (With<PylonVisual>, Without<PylonRingPivot>, Without<BossShieldShell>, Without<BossTether>, Without<BossPylon>, Without<Enemy>, Without<Player>),
    >,
    mut shells: Query<(&mut Transform, &mut Visibility), (With<BossShieldShell>, Without<BossTether>, Without<BossPylon>, Without<Enemy>, Without<Player>)>,
    mut rings: Query<
        &mut Transform,
        (With<PylonRingPivot>, Without<BossShieldShell>, Without<BossTether>, Without<BossPylon>, Without<Enemy>, Without<Player>),
    >,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    let t = time.elapsed_secs();
    let ppos = players.single().map(|p| p.translation).ok();
    let mut rng = rand::rng();

    // Pylons: deaths, strays, hit flash and tethers.
    for (pe, mut py, ptf, children) in &mut pylons {
        let boss = bosses.get(py.boss).ok().filter(|(_, e, _, _)| e.is_alive()).map(|(_, e, _, tf)| (e.stats.eye_height, tf.translation));
        let Some((_, boss_pos)) = boss else {
            // Boss gone (killed / vaporized): no stray pylons or tethers.
            commands.entity(pe).try_despawn();
            continue;
        };
        let base = ptf.translation;
        if !py.is_alive() {
            commands.entity(pe).try_despawn();
            spawn_burst(&mut commands, &fx, base + Vec3::Y * 1.6, BurstColor::Cyan, 26, 9.0, 0.22);
            spawn_burst(&mut commands, &fx, base + Vec3::Y * 2.4, BurstColor::Spark, 14, 7.0, 0.12);
            spawn_flash_light(&mut commands, base + Vec3::Y * 2.0, Color::srgb(0.5, 0.6, 1.0), 2_500_000.0, 16.0, 0.35);
            sfx.play_at(SfxEvent::JammerDestroyed, base + Vec3::Y * 2.0);
            feed.hit(true);
            feed.toast("SHIELD PYLON DOWN", Color::srgb(0.55, 0.7, 1.0));
            continue;
        }
        py.hit_flash = (py.hit_flash - dt).max(0.0);
        // Hit flash pulses only the obelisk visual; the root (and the tether in
        // its space) stays unscaled so the beam stays locked onto the boss.
        let s = 1.0 + py.hit_flash * 1.2;
        let flash_scale = Vec3::new(s, 1.0, s);
        let top = base + Vec3::Y * PYLON_TOP;
        let delta = boss_pos + Vec3::Y * TETHER_ANCHOR_HEIGHT - top;
        let len_sq = delta.length_squared();
        for c in children.iter() {
            if let Ok(mut vtf) = visuals.get_mut(c)
                && vtf.scale != flash_scale
            {
                vtf.scale = flash_scale;
            }
            if let Ok((mut ttf, mut tvis)) = tethers.get_mut(c) {
                let want = if len_sq < 1e-4 { Visibility::Hidden } else { Visibility::Inherited };
                if *tvis != want {
                    *tvis = want;
                }
                if len_sq < 1e-4 {
                    continue;
                }
                let len = len_sq.sqrt();
                // Slim, crisp electric-cobalt beam.
                let w = 0.032 + 0.012 * (t * 12.0 + base.x).sin().abs();
                ttf.translation = Vec3::Y * PYLON_TOP + delta * 0.5;
                ttf.rotation = Quat::from_rotation_arc(Vec3::Y, delta / len);
                ttf.scale = Vec3::new(w, len, w);
            }
            if let Ok(mut rtf) = rings.get_mut(c) {
                // Precessing orbit of the containment ring around the crystal crown.
                let spin = t * 1.8 + base.x;
                rtf.rotation = Quat::from_rotation_y(spin) * Quat::from_rotation_x(0.35 + 0.08 * (t * 2.3).sin());
                rtf.translation = Vec3::Y * (PYLON_RING_HEIGHT + 0.05 * (t * 2.0 + base.z).sin());
            }
        }
    }

    let mut snapshot = BossHud::default();
    for (be, mut e, mut st, tf) in &mut bosses {
        if !e.is_alive() || e.kind != EnemyKind::CarrierDaemon {
            continue;
        }
        let alive = pylons.iter().filter(|(_, p, _, _)| p.boss == be && p.is_alive()).count();
        let pos = tf.translation;
        let core = pos + Vec3::Y * CORE_HEIGHT;
        let floor = grid.floor;
        match advance_boss_phase(&mut st, &mut e, alive, dt) {
            BossTransition::RaiseShield => {
                let second = st.phase == BossPhase::Shielded2;
                for p in pylon_positions(&grid, pos, second) {
                    spawn_pylon(&mut commands, &ea, p, be, floor);
                    spawn_burst(&mut commands, &fx, p + Vec3::Y * 1.5, BurstColor::Cyan, 16, 6.0, 0.16);
                }
                st.pylons_alive = 2;
                spawn_shockwave(&mut commands, &fx, &mut materials, pos + Vec3::Y * 0.3, 9.0, 0.5, SHIELD_GLOW.0, SHIELD_GLOW.1, true);
                spawn_burst(&mut commands, &fx, core, BurstColor::Spark, 20, 8.0, 0.16);
                sfx.play_at(SfxEvent::BossShield, core);
                feed.alert("SIGNAL SHIELD UP - DESTROY THE 2 PYLONS!", Color::srgb(0.55, 0.65, 1.0), 3.0);
            }
            BossTransition::Shatter => {
                spawn_shockwave(&mut commands, &fx, &mut materials, pos + Vec3::Y * 0.3, 14.0, 0.6, SHATTER_GLOW.0, SHATTER_GLOW.1, true);
                spawn_shockwave(&mut commands, &fx, &mut materials, core, 5.0, 0.35, SHIELD_GLOW.0, SHIELD_GLOW.1, false);
                spawn_burst(&mut commands, &fx, core, BurstColor::Cyan, 34, 11.0, 0.22);
                spawn_burst(&mut commands, &fx, core, BurstColor::Gold, 20, 8.0, 0.2);
                spawn_flash_light(&mut commands, core, Color::srgb(0.6, 0.7, 1.0), 6_000_000.0, 30.0, 0.5);
                sfx.play_at(SfxEvent::BossShield, core);
                let overdrive = st.phase == BossPhase::Overdrive;
                feed.alert(if overdrive { "SHIELD SHATTERED - CARRIER DAEMON OVERDRIVE!" } else { "SHIELD SHATTERED - PHASE II!" }, Color::srgb(1.0, 0.75, 0.3), 3.0);
                // Supply drop on the Signal Ring.
                if let Some(pa) = pickups.as_deref() {
                    let ring = crate::extraction::ring_supply_cells(&grid);
                    if !ring.is_empty() {
                        for kind in [PickupKind::Health, if overdrive { PickupKind::Armor } else { PickupKind::Rockets }, PickupKind::Shells] {
                            let c = ring[st.supply_idx % ring.len()];
                            st.supply_idx += 1;
                            spawn_pickup(&mut commands, pa, kind, grid.cell_center(c), true);
                        }
                    }
                }
                if !overdrive {
                    // Phase II escort: an Imp and a Phase Stalker.
                    spawn_escort(&mut commands, &ea, &mut materials, &grid, EnemyKind::Imp, pos, floor);
                    spawn_escort(&mut commands, &ea, &mut materials, &grid, EnemyKind::PhaseStalker, pos, floor);
                }
            }
            BossTransition::None => {}
        }

        // Shield feedback: sparks on absorbed hits + a throttled hint.
        st.spark_cooldown = (st.spark_cooldown - dt).max(0.0);
        st.toast_cooldown = (st.toast_cooldown - dt).max(0.0);
        if e.shield_hits != st.last_shield_hits {
            st.last_shield_hits = e.shield_hits;
            if st.spark_cooldown <= 0.0 {
                st.spark_cooldown = 0.12;
                spawn_burst(&mut commands, &fx, core + Vec3::new(rng.random_range(-0.8..0.8), rng.random_range(-0.6..0.8), 0.0), BurstColor::Cyan, 8, 5.0, 0.08);
                sfx.play(SfxEvent::Denied);
            }
            if st.toast_cooldown <= 0.0 {
                st.toast_cooldown = 2.5;
                feed.toast(BOSS_SHIELDED_TEXT, Color::srgb(0.55, 0.65, 1.0));
            }
        }

        // Shield shell.
        if let Ok((mut stf, mut vis)) = shells.get_mut(st.shell) {
            let want = if e.shielded { Visibility::Inherited } else { Visibility::Hidden };
            if *vis != want {
                *vis = want;
            }
            if e.shielded {
                let k = 1.0 + 0.05 * (t * 6.0).sin() + 0.25 * (st.spark_cooldown / 0.12);
                stf.scale = SHELL_SCALE * k;
                stf.rotation = Quat::from_rotation_y(t * 0.8);
            }
        }

        // Attack patterns (telegraph -> fire), alternating Hellfire / EMP.
        // Frozen by `DevFreezeAi` (showcase shots never fire mid-pose).
        if let Some(pp) = ppos
            && freeze.is_none()
            && e.alerted
            && e.stun_timer <= 0.0
        {
            let chest = pp + Vec3::Y * 1.1;
            let dist = Vec2::new(pp.x - pos.x, pp.z - pos.z).length();
            if st.telegraph > 0.0 {
                st.telegraph -= dt;
                if st.telegraph <= 0.0 {
                    st.telegraph = 0.0;
                    let attack = st.next_attack;
                    fire_boss_attack(&mut commands, &fx, &e, st.phase, attack, core, chest);
                    if st.phase == BossPhase::Overdrive {
                        // Overdrive crossfire (telegraphed with dual Gold + Cyan bursts).
                        let other = if attack == BossAttack::EmpVolley { BossAttack::HellfireBarrage } else { BossAttack::EmpVolley };
                        fire_boss_attack(&mut commands, &fx, &e, st.phase, other, core + Vec3::Y * 0.6, chest);
                    }
                    e.trigger_attack_anim(false);
                    sfx.play_at(SfxEvent::EnemyFire, core);
                    st.next_attack = if st.next_attack == BossAttack::HellfireBarrage { BossAttack::EmpVolley } else { BossAttack::HellfireBarrage };
                }
            } else {
                st.attack_timer -= dt;
                if st.attack_timer <= 0.0 && dist < e.stats.range {
                    // Commit to the attack pattern at windup so the telegraph
                    // always matches the fired barrage.
                    if st.next_attack == BossAttack::HellfireBarrage && !has_line_of_sight(&grid, core, chest) {
                        st.next_attack = BossAttack::EmpVolley;
                    }
                    st.attack_timer = st.phase.attack_cooldown() * rng.random_range(0.9..1.15);
                    st.telegraph = boss_telegraph_secs(st.phase);
                    let tip = core + (chest - core).normalize_or_zero() * 1.4;
                    if st.phase == BossPhase::Overdrive {
                        spawn_burst(&mut commands, &fx, tip, BurstColor::Gold, 10, 3.5, 0.14);
                        spawn_burst(&mut commands, &fx, tip + Vec3::Y * 0.5, BurstColor::Cyan, 10, 3.5, 0.14);
                    } else {
                        let c = if st.next_attack == BossAttack::HellfireBarrage { BurstColor::Gold } else { BurstColor::Cyan };
                        spawn_burst(&mut commands, &fx, tip, c, 12, 3.5, 0.14);
                    }
                }
            }
        }

        snapshot = BossHud { active: true, phase: st.phase, hp_frac: (e.hp / e.max_hp).clamp(0.0, 1.0), pylons: st.pylons_alive };
    }
    if *hud != snapshot {
        *hud = snapshot;
    }
}

pub struct BossPlugin;

impl Plugin for BossPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BossHud>().add_systems(Startup, prewarm_boss_fades).add_systems(
            Update,
            carrier_daemon_system.in_set(GameSet::Combat).after(CombatDamage).run_if(resource_exists::<LevelGrid>),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::generate_level;

    fn boss() -> (BossState, Enemy) {
        let mut e = Enemy::new(EnemyKind::CarrierDaemon, 5);
        e.hp_floor = e.max_hp * PHASE1_THRESHOLD;
        (BossState::new(Entity::PLACEHOLDER), e)
    }

    #[test]
    fn carrier_daemon_stats_scale_from_floor_five() {
        assert_eq!(EnemyKind::CarrierDaemon.stats(5).hp, 1200.0);
        assert_eq!(EnemyKind::CarrierDaemon.stats(10).hp, 2200.0);
        assert!(EnemyKind::CarrierDaemon.stats(15).hp > EnemyKind::CarrierDaemon.stats(10).hp);
        let s = EnemyKind::CarrierDaemon.stats(5);
        assert_eq!((s.radius, s.height), (1.2, 4.4));
        assert_eq!(crate::weapons::knockback_scale(EnemyKind::CarrierDaemon), 0.05);
        assert_eq!(EnemyKind::CarrierDaemon.name(), "THE CARRIER DAEMON");
    }

    #[test]
    fn pure_phase_machine_runs_all_five_phases() {
        use BossPhase::*;
        assert_eq!(Phase1.after_hp(0.7), Phase1);
        assert_eq!(Phase1.after_hp(0.65), Shielded1);
        assert_eq!(Shielded1.after_hp(0.1), Shielded1, "HP never skips a shield");
        assert_eq!(Shielded1.after_pylons_cleared(), Phase2);
        assert_eq!(Phase2.after_hp(0.31), Phase2);
        assert_eq!(Phase2.after_hp(0.3), Shielded2);
        assert_eq!(Shielded2.after_pylons_cleared(), Overdrive);
        assert_eq!(Overdrive.after_hp(0.01), Overdrive);
        assert_eq!(Phase1.after_pylons_cleared(), Phase1);
        assert!(Shielded1.shielded() && Shielded2.shielded() && !Phase2.shielded() && !Overdrive.shielded());
        assert!(Overdrive.attack_cooldown() < Phase2.attack_cooldown() && Phase2.attack_cooldown() < Phase1.attack_cooldown());
        assert!(Phase2.barrage_spread() < Phase1.barrage_spread(), "tighter barrages in Phase II");
    }

    #[test]
    fn thresholds_clamp_shield_blocks_and_pylons_shatter_with_stun() {
        let mods = crate::state::PlayerModifiers::default();
        let (mut st, mut e) = boss();
        let max = e.max_hp;
        // A huge hit clamps at exactly 65%.
        crate::enemies::damage_enemy(&mut e, 5000.0, false, &mods);
        assert_eq!(e.hp, max * 0.65);
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.016), BossTransition::RaiseShield);
        assert_eq!(st.phase, BossPhase::Shielded1);
        assert!(e.shielded);
        // Full immunity while shielded (feedback counted).
        assert_eq!(crate::enemies::damage_enemy(&mut e, 300.0, true, &mods), 0.0);
        assert_eq!(e.hp, max * 0.65);
        assert_eq!(e.shield_hits, 1);
        // Pylons still spawning (grace) or alive: no shatter.
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.1), BossTransition::None);
        assert_eq!(advance_boss_phase(&mut st, &mut e, 2, 0.5), BossTransition::None);
        assert_eq!(advance_boss_phase(&mut st, &mut e, 1, 0.5), BossTransition::None);
        assert!(e.shielded);
        // Both pylons down: shatter, stun, Phase II, faster.
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.016), BossTransition::Shatter);
        assert_eq!(st.phase, BossPhase::Phase2);
        assert!(!e.shielded && e.stun_timer >= BOSS_SHIELD_STUN);
        assert!(e.speed_mult > 1.0);
        // Phase II clamps at 30%.
        crate::enemies::damage_enemy(&mut e, 5000.0, false, &mods);
        assert!((e.hp - max * 0.3).abs() < 1e-3);
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.016), BossTransition::RaiseShield);
        assert_eq!(st.phase, BossPhase::Shielded2);
        assert_eq!(advance_boss_phase(&mut st, &mut e, 2, 0.5), BossTransition::None);
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.016), BossTransition::Shatter);
        assert_eq!(st.phase, BossPhase::Overdrive);
        // Overdrive: no floor, killable.
        e.stun_timer = 0.0;
        crate::enemies::damage_enemy(&mut e, 5000.0, false, &mods);
        assert!(!e.is_alive());
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.016), BossTransition::None);
    }

    #[test]
    fn dead_boss_never_resurrects_on_a_threshold() {
        let (mut st, mut e) = boss();
        e.hp = 0.0;
        assert_eq!(advance_boss_phase(&mut st, &mut e, 0, 0.016), BossTransition::None);
        assert_eq!(st.phase, BossPhase::Phase1);
        assert!(!e.is_alive());
    }

    #[test]
    fn pylons_take_damage_and_ray_hits() {
        let mut p = BossPylon { hp: pylon_hp(5), max_hp: pylon_hp(5), hit_flash: 0.0, boss: Entity::PLACEHOLDER };
        assert_eq!(pylon_hp(5), 185.0);
        assert!(!damage_pylon(&mut p, 100.0));
        assert!(p.hit_flash > 0.0);
        assert!(damage_pylon(&mut p, 100.0));
        assert!(!damage_pylon(&mut p, 100.0), "already destroyed");
        let base = Vec3::new(0.0, 0.0, -10.0);
        let t = pylon_ray_hit(base, Vec3::new(0.0, 1.5, 0.0), Vec3::NEG_Z).unwrap();
        assert!(t > 9.0 && t < 10.0);
        assert!(pylon_ray_hit(base, Vec3::new(0.0, 6.0, 0.0), Vec3::NEG_Z).is_none());
    }

    #[test]
    fn pylon_hit_spheres_leave_no_unhittable_band() {
        let base = Vec3::new(0.0, 0.0, -10.0);
        // The old gap: upper collar band at y = 2.06 (center + collar outer edge).
        for x in [0.0f32, 0.35, -0.35] {
            assert!(pylon_ray_hit(base, Vec3::new(x, 2.06, 0.0), Vec3::NEG_Z).is_some(), "collar band at x = {x}");
        }
        // Every height through base, shaft, collar and crystal crown, from 4 sides.
        for i in 0..=58 {
            let y = 0.05 + i as f32 * 0.05;
            for dir in [Vec3::NEG_Z, Vec3::Z, Vec3::X, Vec3::NEG_X] {
                let origin = base - dir * 10.0 + Vec3::Y * y;
                assert!(pylon_ray_hit(base, origin, dir).is_some(), "horizontal ray at y = {y:.2} dir {dir}");
            }
            // Shaft outer edge (r = 0.3) from the front.
            assert!(pylon_ray_hit(base, Vec3::new(0.3, y, 0.0), Vec3::NEG_Z).is_some(), "shaft edge at y = {y:.2}");
        }
    }

    #[test]
    fn hud_phase_tags() {
        assert_eq!(boss_phase_tag(BossPhase::Phase1, 0), "PHASE I");
        assert_eq!(boss_phase_tag(BossPhase::Shielded1, 2), "SHIELDED // 2 PYLONS");
        assert_eq!(boss_phase_tag(BossPhase::Shielded2, 1), "SHIELDED // 1 PYLON");
        assert_eq!(boss_phase_tag(BossPhase::Phase2, 0), "PHASE II");
        assert_eq!(boss_phase_tag(BossPhase::Overdrive, 0), "OVERDRIVE");
        let h = BossHud { active: true, phase: BossPhase::Shielded1, hp_frac: 0.65, pylons: 2 };
        assert_eq!(h.tag(), "SHIELDED // 2 PYLONS");
    }

    #[test]
    fn spawn_and_pylon_spots_sit_in_the_atrium_on_opposite_sides() {
        for floor in [5u32, 10, 15] {
            for seed in [1u64, 42, 777] {
                let g = generate_level(floor, seed);
                let player = g.cell_center(g.tower) + Vec3::new(4.0, 0.0, 0.0);
                let c = carrier_spawn_cell(&g, player).expect("boss spawn cell");
                assert!(g.is_reachable(g.spawn, c) && standable(&g, g.cell_center(c), 1.2).is_some(), "floor {floor} seed {seed}");
                let t = g.tower_world();
                for second in [false, true] {
                    let [a, b] = pylon_positions(&g, g.cell_center(c), second);
                    for p in [a, b] {
                        let pc = g.world_to_cell(p);
                        assert!(g.is_reachable(g.spawn, pc) && !matches!(g.cell(pc), Cell::Wall | Cell::HazardPit), "floor {floor} seed {seed}: {pc}");
                    }
                    let (da, db) = ((a - t).with_y(0.0), (b - t).with_y(0.0));
                    assert!(da.normalize().dot(db.normalize()) < 0.0, "floor {floor} seed {seed}: pylons on opposite sides");
                    assert!(a.distance(b) > 8.0);
                }
            }
        }
    }

    #[test]
    fn pylon_spots_fall_back_around_the_boss_without_an_atrium() {
        let g = LevelGrid::from_ascii(&[
            "#########",
            "#.......#",
            "#.......#",
            "#.......#",
            "#.......#",
            "#########",
        ]);
        let boss = g.cell_center(IVec2::new(4, 2));
        let [a, b] = pylon_positions(&g, boss, false);
        assert!(a.x > boss.x && b.x < boss.x);
        assert!(!g.cell(g.world_to_cell(a)).is_wall() && !g.cell(g.world_to_cell(b)).is_wall());
    }
}
