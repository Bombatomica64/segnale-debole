//! Developer hooks driven by environment variables (no effect when unset):
//! - `SEGNALE_AUTOSTART=1`: skip the main menu and start a run immediately.
//! - `SEGNALE_SHOTS=<dir>`: (implies autostart) capture `spawn.png`, then
//!   teleport near the Signal Tower and capture `tower.png`, then a jammer
//!   (`jammer.png`), the SOS uplink HUD (`uplink.png`), a staged firefight with
//!   all four demons and the dual-wield viewmodels (`combat.png`), and finally
//!   the App Store upgrade screen (`upgrades.png`), then jumps to the Floor 5
//!   boss arena for `boss.png` (shielded Carrier Daemon + 2 Shield Pylons, a
//!   Phase Stalker and a Relay Sniper, Signal-Mortar in hand, boss plate +
//!   held uplink + alert banner all live) and exits. Used for headless
//!   visual checks (xvfb). The player is invulnerable during the tour.
//! - `SEGNALE_BOT_TEST=<dir>`: autonomous playtest bot (see `playtest_bot.rs`):
//!   plays menu -> Floor 1 -> SOS -> App Store -> Floor 2 through virtual input,
//!   saves a storyboard + `playtest_report.json` and exits 0 (pass) / 1 (fail).
//!   `SEGNALE_BOT_SEED` picks the run seed, `SEGNALE_BOT_DT` the fixed sim step
//!   per frame (default 1/30 s; `0` = real time).
//! - `SEGNALE_META_SHOTS=<dir>`: meta-progression screenshot tour on an
//!   in-memory showcase profile (jam demo on, Survey OS + 5G Chrome Gold, two
//!   signature apps drafted, ROAM + PRIO contracts, 3 feats, lifetime stats):
//!   `01_main_menu.png`, `02_loadout.png`, `03_contracts_feats.png`, `03b_settings.png`,
//!   `04_in_hand_gold.png` (live Floor 1 spawn), `04b_pause_menu.png`,
//!   `05_victory_unlocks.png` (a staged earned Floor 5 win) and
//!   `06_game_over.png` (a separate staged Floor 2 death with its own
//!   summary), then exits. Never touches the on-disk profile.

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

use crate::apps::PhoneApps;
use crate::boss::{spawn_pylon, BossPhase, BossState, PHASE1_THRESHOLD};
use crate::fx::HudFeed;
use crate::meta::{
    start_run_with_loadout, CarrierContract, FeatId, HandsetSkin, LifetimeStats, PlayerProfile, ProfileStore, RunLoadout, StarterChip,
};
use crate::enemies::{spawn_enemy, Enemy, EnemyAssets, EnemyKind};
use crate::extraction::ExtractionState;
use crate::level::{yaw_facing, LevelGrid};
use crate::physics::sample_floor_height;
use crate::player::Player;
use crate::state::{complete_floor, AppState, GameSet, PlayerModifiers, RunStats, UpgradeId, FINAL_FLOOR};
use crate::weapons::{Weapon, WeaponState};

/// Screenshot staging only: freezes demon AI so a posed shot stays posed.
#[derive(Resource)]
pub struct DevFreezeAi;

#[derive(Resource)]
struct DevConfig {
    autostart: bool,
    shots: Option<String>,
    timer: f32,
    stage: u32,
}

pub struct DevPlugin;

impl Plugin for DevPlugin {
    fn build(&self, app: &mut App) {
        if let Ok(dir) = std::env::var("SEGNALE_BOT_TEST") {
            app.add_plugins(crate::playtest_bot::PlaytestBotPlugin::from_env(dir));
            return;
        }
        if let Ok(dir) = std::env::var("SEGNALE_META_SHOTS") {
            let showcase = meta_showcase_profile();
            match app.world_mut().get_resource_mut::<ProfileStore>() {
                Some(mut store) => {
                    store.path = None;
                    store.profile = showcase;
                }
                None => {
                    app.insert_resource(ProfileStore::in_memory(showcase));
                }
            }
            app.insert_resource(MetaShots { dir, stage: 0, timer: 0.0 }).add_systems(Update, meta_shots_tour.after(GameSet::Ui));
            return;
        }
        let shots = std::env::var("SEGNALE_SHOTS").ok();
        let autostart = shots.is_some() || std::env::var("SEGNALE_AUTOSTART").is_ok_and(|v| v == "1");
        if !autostart {
            return;
        }
        app.insert_resource(DevConfig { autostart, shots, timer: 0.0, stage: 0 })
            .add_systems(Update, autostart_run.run_if(in_state(AppState::MainMenu)))
            .add_systems(Update, screenshot_tour.after(GameSet::Ui).run_if(in_state(AppState::Playing)))
            .add_systems(Update, screenshot_store.run_if(in_state(AppState::UpgradeSelect)));
    }
}

fn autostart_run(
    cfg: Res<DevConfig>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    mut next: ResMut<NextState<AppState>>,
) {
    if cfg.autostart {
        crate::state::start_run(&mut run, &mut mods, &mut next, 1234);
    }
}

#[allow(clippy::too_many_arguments)]
fn screenshot_tour(
    mut commands: Commands,
    time: Res<Time>,
    mut cfg: ResMut<DevConfig>,
    grid: Option<Res<LevelGrid>>,
    mut player: Query<(&mut Player, &mut Transform)>,
    (mut ext, mut ws, mut run, mut next): (ResMut<ExtractionState>, ResMut<WeaponState>, ResMut<RunStats>, ResMut<NextState<AppState>>),
    (ea, mut apps, mut feed): (Res<EnemyAssets>, ResMut<PhoneApps>, ResMut<HudFeed>),
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut bosses: Query<(Entity, &mut Enemy, &mut BossState)>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(dir) = cfg.shots.clone() else { return };
    let Some(grid) = grid else { return };
    cfg.timer += time.delta_secs();
    if let Ok((mut p, _)) = player.single_mut() {
        p.invuln_timer = 999.0;
        p.hp = p.max_hp;
    }
    let shot = |commands: &mut Commands, name: &str| {
        let path = format!("{dir}/{name}");
        info!("dev: screenshot -> {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    };
    let mut place = |cell: IVec2, look_at: Vec3| {
        if let Ok((mut p, mut tf)) = player.single_mut() {
            let pos = grid.cell_center(cell);
            tf.translation = pos;
            let d = look_at - pos;
            p.yaw = yaw_facing(Vec2::new(d.x, d.z));
            let eye = crate::player::PLAYER_EYE_HEIGHT;
            p.pitch = (d.y - eye).atan2(Vec2::new(d.x, d.z).length()).clamp(-1.2, 1.2);
            p.velocity = Vec3::ZERO;
        }
    };
    let tower_view_cell = {
        // A walkable atrium cell two cells out from the pedestal edge.
        let candidates = [IVec2::new(0, 3), IVec2::new(3, 0), IVec2::new(0, -3), IVec2::new(-3, 0)];
        candidates.into_iter().map(|o| grid.tower + o).find(|c| !grid.cell(*c).is_wall()).unwrap_or(grid.spawn)
    };
    // Teleports and captures happen on separate frames so each shot shows the new view.
    match (cfg.stage, cfg.timer) {
        (0, t) if t > 4.0 => {
            shot(&mut commands, "spawn.png");
            cfg.stage = 1;
        }
        (1, t) if t > 5.0 => {
            place(tower_view_cell, grid.tower_world() + Vec3::Y * 4.0);
            cfg.stage = 2;
        }
        (2, t) if t > 8.0 => {
            shot(&mut commands, "tower.png");
            cfg.stage = 3;
        }
        (3, t) if t > 9.0 => {
            let j = grid.jammers[0];
            let near = [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
                .into_iter()
                .flat_map(|o| [j + o * 2, j + o])
                .find(|c| matches!(grid.cell(*c), crate::level::Cell::Floor { .. }) && grid.is_reachable(*c, j))
                .unwrap_or(j);
            place(near, grid.cell_center(j) + Vec3::Y * 1.5);
            cfg.stage = 4;
        }
        (4, t) if t > 12.0 => {
            shot(&mut commands, "jammer.png");
            cfg.stage = 5;
        }
        (5, t) if t > 13.0 => {
            let ring_cell = [IVec2::new(0, 2), IVec2::new(2, 0), IVec2::new(0, -2), IVec2::new(-2, 0)]
                .into_iter()
                .map(|o| grid.tower + o)
                .find(|c| !grid.cell(*c).is_wall())
                .unwrap_or(tower_view_cell);
            place(ring_cell, grid.tower_world() + Vec3::Y * 3.0);
            *ext = ExtractionState::Uploading {
                progress: 0.42,
                duration: 28.0,
                in_ring: true,
                jammed: false,
                wave_timer: 0.5,
                baron_spawned: false,
                boss_hold: false,
            };
            cfg.stage = 6;
        }
        (6, t) if t > 16.5 => {
            shot(&mut commands, "uplink.png");
            cfg.stage = 7;
        }
        (7, t) if t > 17.5 => {
            // Staged firefight along the atrium edge (clear of the tower pedestal):
            // one of each demon facing the player, AI frozen for a stable shot.
            *ext = ExtractionState::Searching { hold: 0.0 };
            commands.insert_resource(DevFreezeAi);
            let atrium = grid.rooms[0];
            let from = atrium.min;
            let start = grid.cell_center(from);
            let fwd = Vec3::Z;
            let side = Vec3::X;
            let floor = grid.floor;
            for (kind, dist, lat) in [
                (EnemyKind::Imp, 6.5, 0.8),
                (EnemyKind::Cacodemon, 10.0, 3.2),
                (EnemyKind::JammerFiend, 13.0, -0.6),
                (EnemyKind::HellBaron, 18.0, 2.4),
            ] {
                let mut pos = start + fwd * dist + side * lat;
                pos.y = sample_floor_height(&grid, pos.x, pos.z).unwrap_or(0.0) + 0.05;
                if kind == EnemyKind::Cacodemon {
                    pos.y += 2.4;
                }
                let e = spawn_enemy(&mut commands, &ea, &mut materials, kind, pos, floor, true);
                let to = start - pos;
                commands.entity(e).insert(Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw_facing(Vec2::new(to.x, to.z)))));
            }
            place(from, start + fwd * 12.0 + side * 1.2 + Vec3::Y * 1.4);
            ws.current = Weapon::Shotgun;
            cfg.stage = 8;
        }
        (8, t) if t > 18.0 => {
            apps.force_ping = true;
            cfg.stage = 9;
        }
        (9, t) if t > 19.6 => {
            ws.force_fire = true;
            ws.cooldown = 0.0;
            cfg.stage = 10;
        }
        (10, _) => {
            shot(&mut commands, "combat.png");
            cfg.stage = 11;
        }
        (11, t) if t > 21.0 => {
            complete_floor(&mut run, &mut next);
            cfg.stage = 12;
        }
        // ---- Floor 5 boss arena (entered from `screenshot_store`) ----
        (20, t) if t > 2.5 && grid.floor == FINAL_FLOOR => {
            // Stand on the Signal Ring's west lane looking north across the
            // atrium: the boss dead ahead, the sniper perched on the pedestal.
            let floor = grid.floor;
            let start = boss_arena_point(&grid, 0.0, 0.0);
            for (kind, dist, lat) in [(EnemyKind::CarrierDaemon, 11.5, 0.6), (EnemyKind::PhaseStalker, 6.0, -1.3), (EnemyKind::RelaySniper, 9.5, 3.0)] {
                let pos = boss_arena_point(&grid, dist, lat) + Vec3::Y * 0.05;
                // The boss stays un-alerted so no barrage crosses the frame.
                let e = spawn_enemy(&mut commands, &ea, &mut materials, kind, pos, floor, kind != EnemyKind::CarrierDaemon);
                let to = start - pos;
                commands.entity(e).insert(Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw_facing(Vec2::new(to.x, to.z)))));
            }
            commands.insert_resource(DevFreezeAi);
            place(grid.world_to_cell(start), boss_arena_point(&grid, 14.0, 1.0).with_y(3.2));
            if let Ok((mut p, _)) = player.single_mut() {
                p.ammo_rockets = p.max_rockets;
            }
            ws.current = Weapon::Rocket;
            ws.previous = Weapon::Rocket;
            ws.swap_timer = 0.0;
            *ext = ExtractionState::Uploading {
                progress: crate::extraction::BOSS_HOLD_PROGRESS,
                duration: 44.0,
                in_ring: true,
                jammed: false,
                wave_timer: 999.0,
                baron_spawned: true,
                boss_hold: true,
            };
            cfg.stage = 21;
        }
        (21, t) if t > 3.0 => {
            // Raise the first Signal Shield with both pylons flanking the boss.
            if let Ok((be, mut e, mut st)) = bosses.single_mut() {
                e.hp = e.max_hp * PHASE1_THRESHOLD;
                st.phase = BossPhase::Shielded1;
                st.raise_grace = 0.5;
                for (dist, lat) in [(5.0, -4.4), (9.8, 5.2)] {
                    spawn_pylon(&mut commands, &ea, boss_arena_point(&grid, dist, lat), be, grid.floor);
                }
                feed.alert("SIGNAL SHIELD UP - DESTROY THE 2 PYLONS!", Color::srgb(0.55, 0.65, 1.0), 6.0);
            }
            cfg.stage = 22;
        }
        (22, t) if t > 5.0 => {
            shot(&mut commands, "boss.png");
            cfg.stage = 23;
        }
        (23, t) if t > 6.5 => {
            exit.write(AppExit::Success);
            cfg.stage = 24;
        }
        _ => {}
    }
}

/// `boss.png` staging: a ground point `dist` metres north and `lat` metres
/// east of the camera cell (west lane of the Signal Ring, beside the pedestal).
fn boss_arena_point(grid: &LevelGrid, dist: f32, lat: f32) -> Vec3 {
    let mut p = grid.cell_center(grid.tower + IVec2::new(-2, 1)) + Vec3::NEG_Z * dist + Vec3::X * lat;
    p.y = sample_floor_height(grid, p.x, p.z).unwrap_or(0.0);
    p
}

/// Capture the App Store screen, then jump to the Floor 5 boss arena
/// (`screenshot_tour` stages 20+ take `boss.png` and exit).
fn screenshot_store(
    mut commands: Commands,
    time: Res<Time>,
    mut cfg: ResMut<DevConfig>,
    mut t: Local<f32>,
    mut run: ResMut<RunStats>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(dir) = cfg.shots.clone() else { return };
    let before = *t;
    *t += time.delta_secs();
    if before < 1.5 && *t >= 1.5 {
        let path = format!("{dir}/upgrades.png");
        info!("dev: screenshot -> {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *t > 3.0 && cfg.stage < 20 {
        run.floor = FINAL_FLOOR;
        cfg.stage = 20;
        cfg.timer = 0.0;
        next.set(AppState::Playing);
    }
}

// ---------------------------------------------------------------------------
// SEGNALE_META_SHOTS
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct MetaShots {
    dir: String,
    stage: u32,
    /// Seconds since the current stage began.
    timer: f32,
}

/// The showcase profile: jam demo on, Survey OS + 5G Chrome Gold equipped,
/// Signal Spoofer + Jammer Hijack drafted into the 12-card catalog, ROAM +
/// PRIO contracts signed, 3 earned feats and populated lifetime stats.
pub fn meta_showcase_profile() -> PlayerProfile {
    let mut p = PlayerProfile::default();
    p.unlocked_feats.extend([FeatId::FirstExtraction, FeatId::SignalScout, FeatId::JammerHunter]);
    p.set_jam_demo_unlock_all(true);
    p.equip_chip(StarterChip::SurveyOs);
    p.equip_skin(HandsetSkin::ChromeGold);
    for id in [UpgradeId::CopperAntenna, UpgradeId::HighCapacityCell, UpgradeId::SignalSpoofer, UpgradeId::JammerHijack] {
        p.toggle_catalog_app(id);
    }
    p.toggle_contract(CarrierContract::RoamingCharges);
    p.toggle_contract(CarrierContract::PriorityJamming);
    p.stats = LifetimeStats {
        runs_played: 11,
        victories: 0,
        best_floor: 4,
        total_kills: 486,
        total_jammers: 23,
        total_fiends: 3,
        total_blackout_kills: 7,
        total_sos_completed: 2,
        fastest_victory_secs: None,
        max_contracts_cleared: 0,
        best_speed: 27.4,
        best_bhop_chain: 6,
        best_endless_floor: 0,
    };
    p
}

fn meta_shot(commands: &mut Commands, dir: &str, name: &str) {
    let path = format!("{dir}/{name}");
    info!("dev: meta screenshot -> {path}");
    commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
}

/// Main menu -> Loadout -> Contracts & Feats -> Settings -> live Floor 1 -> Pause ->
/// Victory -> a separate Floor 2 Game Over.
#[allow(clippy::too_many_arguments)]
fn meta_shots_tour(
    mut commands: Commands,
    time: Res<Time>,
    mut cfg: ResMut<MetaShots>,
    state: Res<State<AppState>>,
    store: Res<ProfileStore>,
    (mut run, mut mods, mut loadout): (ResMut<RunStats>, ResMut<PlayerModifiers>, ResMut<RunLoadout>),
    mut players: Query<&mut Player>,
    mut next: ResMut<NextState<AppState>>,
    mut exit: MessageWriter<AppExit>,
) {
    cfg.timer += time.delta_secs();
    for mut p in &mut players {
        p.invuln_timer = 999.0;
        p.hp = p.max_hp;
    }
    let dir = cfg.dir.clone();
    let st = *state.get();
    let t = cfg.timer;
    let advance = match (cfg.stage, st) {
        (0, AppState::MainMenu) if t > 1.5 => {
            meta_shot(&mut commands, &dir, "01_main_menu.png");
            true
        }
        (1, AppState::MainMenu) if t > 0.7 => {
            next.set(AppState::Loadout);
            true
        }
        (2, AppState::Loadout) if t > 1.5 => {
            meta_shot(&mut commands, &dir, "02_loadout.png");
            true
        }
        (3, AppState::Loadout) if t > 0.7 => {
            next.set(AppState::ContractsFeats);
            true
        }
        (4, AppState::ContractsFeats) if t > 1.5 => {
            meta_shot(&mut commands, &dir, "03_contracts_feats.png");
            true
        }
        (5, AppState::ContractsFeats) if t > 0.7 => {
            // Settings opened from the menus (`SettingsReturn` stays MainMenu).
            next.set(AppState::Settings);
            true
        }
        (6, AppState::Settings) if t > 1.5 => {
            meta_shot(&mut commands, &dir, "03b_settings.png");
            true
        }
        (7, AppState::Settings) if t > 0.7 => {
            commands.insert_resource(DevFreezeAi);
            start_run_with_loadout(&mut run, &mut mods, &mut loadout, store.profile.build_loadout(), &mut next, 1234);
            true
        }
        (8, AppState::Playing) if t > 4.0 => {
            meta_shot(&mut commands, &dir, "04_in_hand_gold.png");
            true
        }
        (9, AppState::Playing) if t > 0.8 => {
            next.set(AppState::Paused);
            true
        }
        (10, AppState::Paused) if t > 1.2 => {
            meta_shot(&mut commands, &dir, "04b_pause_menu.png");
            true
        }
        (11, AppState::Paused) if t > 0.7 => {
            // Stage a fast earned contract victory (non-demo) so the Victory
            // screenshot showcases real newly-unlocked feats and rewards.
            loadout.jam_demo = false;
            run.floor = FINAL_FLOOR;
            run.floors_cleared = FINAL_FLOOR - 1;
            run.kills = 97;
            run.jammers_destroyed = 6;
            run.fiends_killed = 2;
            run.blackout_kills = 3;
            run.sos_completed = 5;
            run.time_elapsed = 512.0;
            run.upgrades = vec![UpgradeId::FiveGOverclock, UpgradeId::SignalSpoofer, UpgradeId::JammerHijack, UpgradeId::EmpOvercharge];
            complete_floor(&mut run, &mut next);
            true
        }
        (12, AppState::Victory) if t > 2.0 => {
            meta_shot(&mut commands, &dir, "05_victory_unlocks.png");
            true
        }
        (13, AppState::Victory) if t > 1.0 => {
            // A separate, genuinely lost run (not the victory replayed): died on
            // Floor 2 after one uplink, with a single app installed. It is
            // recorded like any real death, so NO SERVICE shows its own
            // summary and its own (empty) unlock list.
            *run = RunStats {
                floor: 2,
                floors_cleared: 1,
                kills: 23,
                jammers_destroyed: 2,
                blackout_kills: 1,
                sos_completed: 1,
                time_elapsed: 191.0,
                upgrades: vec![UpgradeId::PacketSniffer],
                run_seed: 1234,
                ..default()
            };
            loadout.jam_demo = false;
            next.set(AppState::GameOver);
            true
        }
        (14, AppState::GameOver) if t > 1.5 => {
            meta_shot(&mut commands, &dir, "06_game_over.png");
            true
        }
        (15, AppState::GameOver) if t > 1.5 => {
            exit.write(AppExit::Success);
            true
        }
        _ => false,
    };
    if advance {
        cfg.stage += 1;
        cfg.timer = 0.0;
    }
}
