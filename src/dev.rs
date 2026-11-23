//! Developer hooks driven by environment variables (no effect when unset):
//! - `SEGNALE_AUTOSTART=1`: skip the main menu and start a run immediately.
//! - `SEGNALE_SHOTS=<dir>`: (implies autostart) capture `spawn.png`, then
//!   teleport near the Signal Tower and capture `tower.png`, then a jammer
//!   (`jammer.png`), the SOS uplink HUD (`uplink.png`), a staged firefight with
//!   all four demons and the dual-wield viewmodels (`combat.png`), and finally
//!   the App Store upgrade screen (`upgrades.png`), then exit. Used for headless
//!   visual checks (xvfb). The player is invulnerable during the tour.
//! - `SEGNALE_BOT_TEST=<dir>`: autonomous playtest bot (see `playtest_bot.rs`):
//!   plays menu -> Floor 1 -> SOS -> App Store -> Floor 2 through virtual input,
//!   saves a storyboard + `playtest_report.json` and exits 0 (pass) / 1 (fail).
//!   `SEGNALE_BOT_SEED` picks the run seed, `SEGNALE_BOT_DT` the fixed sim step
//!   per frame (default 1/30 s; `0` = real time).

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

use crate::apps::PhoneApps;
use crate::enemies::{spawn_enemy, EnemyAssets, EnemyKind};
use crate::extraction::ExtractionState;
use crate::level::{yaw_facing, LevelGrid};
use crate::physics::sample_floor_height;
use crate::player::Player;
use crate::state::{complete_floor, AppState, GameSet, PlayerModifiers, RunStats};
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
    (ea, mut apps): (Res<EnemyAssets>, ResMut<PhoneApps>),
    mut materials: ResMut<Assets<StandardMaterial>>,
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
        _ => {}
    }
}

/// Capture the App Store screen, then exit.
fn screenshot_store(mut commands: Commands, time: Res<Time>, cfg: Res<DevConfig>, mut t: Local<f32>, mut exit: MessageWriter<AppExit>) {
    let Some(dir) = cfg.shots.clone() else { return };
    let before = *t;
    *t += time.delta_secs();
    if before < 1.5 && *t >= 1.5 {
        let path = format!("{dir}/upgrades.png");
        info!("dev: screenshot -> {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *t > 3.0 {
        exit.write(AppExit::Success);
    }
}
