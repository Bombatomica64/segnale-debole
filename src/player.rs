//! Doom-speed FPS controller: mouse look, strafe-running, jump, Airplane Mode
//! dash, view bob, hazard damage. The `Player` entity's translation is the feet
//! position and its rotation holds the yaw; the child `PlayerCamera` holds pitch.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::audio::{SfxEvent, SfxQueue};
use crate::bot_input::{take_flag, BotControl};
use crate::level::LevelGrid;
use crate::physics::{resolve_player_movement, JUMP_VELOCITY, PLAYER_HEIGHT, PLAYER_RADIUS, PLAYER_STEP_HEIGHT};
use crate::signal::{overclock_move_factor, SignalState};
use crate::state::{AppState, GameSet, PlayerModifiers};

pub const PLAYER_EYE_HEIGHT: f32 = 1.6;
pub const MOVE_SPEED: f32 = 11.0;
pub const GROUND_ACCEL: f32 = 16.0;
pub const AIR_ACCEL: f32 = 3.0;
pub const DASH_SPEED: f32 = 30.0;
pub const DASH_DURATION: f32 = 0.16;
pub const DASH_COOLDOWN: f32 = 2.0;
pub const MOUSE_SENSITIVITY: f32 = 0.0022;
pub const HAZARD_DAMAGE: f32 = 10.0;
pub const HAZARD_TICK: f32 = 0.5;
pub const BASE_FOV_DEG: f32 = 85.0;

#[derive(Component, Debug, Clone)]
pub struct Player {
    pub hp: f32,
    pub max_hp: f32,
    pub armor: f32,
    pub max_armor: f32,
    pub ammo_shells: u32,
    pub max_shells: u32,
    pub ammo_plasma: u32,
    pub max_plasma: u32,
    /// Seconds until the next dash is available.
    pub dash_cooldown: f32,
    /// Remaining seconds of the active dash burst.
    pub dash_timer: f32,
    pub invuln_timer: f32,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
    pub hazard_timer: f32,
    pub bob_phase: f32,
    /// Seconds since the last damage taken (for HUD flashes).
    pub since_damage: f32,
    /// True on the frame a dash begins (Airplane Mode shockwave hook).
    pub dash_started: bool,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            hp: 100.0,
            max_hp: 100.0,
            armor: 25.0,
            max_armor: 100.0,
            ammo_shells: 32,
            max_shells: 50,
            ammo_plasma: 80,
            max_plasma: 200,
            dash_cooldown: 0.0,
            dash_timer: 0.0,
            invuln_timer: 0.0,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            grounded: false,
            hazard_timer: 0.0,
            bob_phase: 0.0,
            since_damage: 999.0,
            dash_started: false,
        }
    }
}

impl Player {
    /// Apply damage; armor absorbs half of it while it lasts.
    /// Returns the HP actually lost (0 while invulnerable).
    pub fn apply_damage(&mut self, amount: f32) -> f32 {
        if self.invuln_timer > 0.0 || amount <= 0.0 {
            return 0.0;
        }
        let absorbed = (amount * 0.5).min(self.armor);
        self.armor -= absorbed;
        let lost = (amount - absorbed).min(self.hp);
        self.hp -= lost;
        self.since_damage = 0.0;
        lost
    }

    pub fn is_dead(&self) -> bool {
        self.hp <= 0.0
    }

    pub fn heal(&mut self, amount: f32) {
        self.hp = (self.hp + amount).min(self.max_hp);
    }

    pub fn dash_ready(&self) -> bool {
        self.dash_cooldown <= 0.0
    }
}

/// The FPS head camera (child of the player).
#[derive(Component)]
pub struct PlayerCamera;

/// Whether mouse look is active (cursor captured).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorCaptured(pub bool);

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CursorCaptured>()
            .add_systems(OnEnter(AppState::Playing), capture_cursor)
            .add_systems(OnExit(AppState::Playing), release_cursor)
            .add_systems(OnEnter(AppState::MainMenu), despawn_player)
            .add_systems(
                Update,
                (
                    ensure_player,
                    snap_player_to_spawn.run_if(resource_exists_and_changed::<LevelGrid>),
                    cursor_capture_input,
                    mouse_look,
                )
                    .chain()
                    .in_set(GameSet::Input),
            )
            .add_systems(Update, check_death.in_set(GameSet::World).after(hazard_damage))
            .add_systems(Update, move_player.in_set(GameSet::Movement))
            .add_systems(Update, (view_bob, hazard_damage).chain().in_set(GameSet::World));
    }
}

fn spawn_player(commands: &mut Commands, grid: &LevelGrid) {
    let pos = grid.spawn_world();
    commands
        .spawn((
            Player { yaw: grid.spawn_yaw, ..default() },
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(grid.spawn_yaw)),
            Visibility::default(),
        ))
        .with_children(|p| {
            p.spawn((
                PlayerCamera,
                Camera3d::default(),
                Projection::Perspective(PerspectiveProjection {
                    fov: BASE_FOV_DEG.to_radians(),
                    near: 0.05,
                    far: 400.0,
                    ..default()
                }),
                DistanceFog {
                    color: Color::srgb(0.16, 0.025, 0.02),
                    directional_light_color: Color::srgba(1.0, 0.3, 0.2, 0.4),
                    directional_light_exponent: 20.0,
                    falloff: FogFalloff::Linear { start: 10.0, end: 70.0 },
                },
                Transform::from_xyz(0.0, PLAYER_EYE_HEIGHT, 0.0),
            ));
        });
}

/// Spawn the player once a level exists (player persists across floors).
fn ensure_player(mut commands: Commands, grid: Option<Res<LevelGrid>>, players: Query<(), With<Player>>) {
    if let Some(grid) = grid
        && players.is_empty() {
            spawn_player(&mut commands, &grid);
        }
}

/// New floor: put the existing player at the airlock.
fn snap_player_to_spawn(grid: Res<LevelGrid>, mut q: Query<(&mut Player, &mut Transform)>) {
    for (mut p, mut tf) in &mut q {
        tf.translation = grid.spawn_world();
        p.yaw = grid.spawn_yaw;
        p.pitch = 0.0;
        p.velocity = Vec3::ZERO;
        p.dash_timer = 0.0;
        tf.rotation = Quat::from_rotation_y(p.yaw);
    }
}

fn despawn_player(mut commands: Commands, q: Query<Entity, With<Player>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

fn set_cursor(cursor: &mut CursorOptions, captured: bool) {
    cursor.grab_mode = if captured { CursorGrabMode::Locked } else { CursorGrabMode::None };
    cursor.visible = !captured;
}

fn capture_cursor(mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>, mut captured: ResMut<CursorCaptured>) {
    if let Ok(mut c) = cursor.single_mut() {
        set_cursor(&mut c, true);
        captured.0 = true;
    }
}

fn release_cursor(mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>, mut captured: ResMut<CursorCaptured>) {
    if let Ok(mut c) = cursor.single_mut() {
        set_cursor(&mut c, false);
    }
    captured.0 = false;
}

/// Click to capture, Esc to release.
fn cursor_capture_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut captured: ResMut<CursorCaptured>,
) {
    let Ok(mut c) = cursor.single_mut() else { return };
    if keys.just_pressed(KeyCode::Escape) && captured.0 {
        set_cursor(&mut c, false);
        captured.0 = false;
    } else if mouse.just_pressed(MouseButton::Left) && !captured.0 {
        set_cursor(&mut c, true);
        captured.0 = true;
    }
}

fn mouse_look(
    motion: Res<AccumulatedMouseMotion>,
    captured: Res<CursorCaptured>,
    bot: Option<Res<BotControl>>,
    mut players: Query<(&mut Player, &mut Transform), Without<PlayerCamera>>,
    mut cams: Query<&mut Transform, With<PlayerCamera>>,
) {
    let Ok((mut p, mut tf)) = players.single_mut() else { return };
    if captured.0 && motion.delta != Vec2::ZERO {
        p.yaw -= motion.delta.x * MOUSE_SENSITIVITY;
        p.pitch = (p.pitch - motion.delta.y * MOUSE_SENSITIVITY).clamp(-1.5, 1.5);
    }
    if let Some(bot) = bot {
        if let Some(yaw) = bot.look_yaw {
            p.yaw = yaw;
        }
        if let Some(pitch) = bot.look_pitch {
            p.pitch = pitch.clamp(-1.5, 1.5);
        }
    }
    tf.rotation = Quat::from_rotation_y(p.yaw);
    if let Ok(mut cam) = cams.single_mut() {
        cam.rotation = Quat::from_rotation_x(p.pitch);
    }
}

/// WASD intent in world space (unit length or zero) for a given yaw.
pub fn wish_direction(yaw: f32, forward: f32, right: f32) -> Vec3 {
    let rot = Quat::from_rotation_y(yaw);
    let f = rot * Vec3::NEG_Z;
    let r = rot * Vec3::X;
    (f * forward + r * right).normalize_or_zero()
}

#[allow(clippy::too_many_arguments)]
fn move_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    grid: Option<Res<LevelGrid>>,
    modifiers: Res<PlayerModifiers>,
    signal: Res<SignalState>,
    mut sfx: ResMut<SfxQueue>,
    mut bot: Option<ResMut<BotControl>>,
    mut q: Query<(&mut Player, &mut Transform)>,
) {
    let Some(grid) = grid else { return };
    let Ok((mut p, mut tf)) = q.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);

    let axis = |pos: KeyCode, neg: KeyCode| (keys.pressed(pos) as i32 - keys.pressed(neg) as i32) as f32;
    let fwd = axis(KeyCode::KeyW, KeyCode::KeyS) + axis(KeyCode::ArrowUp, KeyCode::ArrowDown);
    let right = axis(KeyCode::KeyD, KeyCode::KeyA);
    let mut wish = wish_direction(p.yaw, fwd.clamp(-1.0, 1.0), right);
    if let Some(b) = &bot
        && b.wish_dir != Vec3::ZERO
    {
        wish = Vec3::new(b.wish_dir.x, 0.0, b.wish_dir.z).normalize_or_zero();
    }
    let bot_jump = bot.as_ref().is_some_and(|b| b.jump);

    p.dash_cooldown = (p.dash_cooldown - dt).max(0.0);
    p.invuln_timer = (p.invuln_timer - dt).max(0.0);
    p.since_damage += dt;

    let mut vel = p.velocity;
    p.dash_started = false;
    let shift = keys.just_pressed(KeyCode::ShiftLeft) || keys.just_pressed(KeyCode::ShiftRight) || take_flag(&mut bot, |b| &mut b.dash);
    if shift && p.dash_ready() {
        let dir = if wish != Vec3::ZERO { wish } else { Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z };
        p.dash_timer = DASH_DURATION;
        p.dash_started = true;
        sfx.play(SfxEvent::Dash);
        p.dash_cooldown = DASH_COOLDOWN * modifiers.dash_cooldown_mult;
        if modifiers.dash_invulnerable {
            p.invuln_timer = p.invuln_timer.max(DASH_DURATION + 0.25);
        }
        vel.x = dir.x * DASH_SPEED;
        vel.z = dir.z * DASH_SPEED;
        vel.y = vel.y.max(0.0);
    }

    if p.dash_timer > 0.0 {
        p.dash_timer -= dt;
    } else {
        // 5G Overclock: faster in strong signal.
        let speed = MOVE_SPEED * modifiers.move_speed_mult * overclock_move_factor(signal.bars, &modifiers);
        let target = wish * speed;
        let accel = if p.grounded { GROUND_ACCEL } else { AIR_ACCEL };
        let k = (accel * dt).min(1.0);
        let h = Vec2::new(vel.x, vel.z).lerp(Vec2::new(target.x, target.z), k);
        vel.x = h.x;
        vel.z = h.y;
    }

    if (keys.pressed(KeyCode::Space) || bot_jump) && p.grounded {
        vel.y = JUMP_VELOCITY;
        sfx.play(SfxEvent::Jump);
    }

    let r = resolve_player_movement(&grid, tf.translation, vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT);
    tf.translation = r.pos;
    p.velocity = r.vel;
    p.grounded = r.grounded;
    if r.on_hazard {
        p.hazard_timer -= dt;
    } else {
        p.hazard_timer = 0.0;
    }
}

fn view_bob(
    time: Res<Time>,
    mut players: Query<&mut Player>,
    mut cams: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
) {
    let Ok(mut p) = players.single_mut() else { return };
    let Ok((mut cam, mut proj)) = cams.single_mut() else { return };
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    let moving = p.grounded && speed > 1.0;
    if moving {
        p.bob_phase += time.delta_secs() * speed * 1.05;
    }
    let amp = if moving { (speed / MOVE_SPEED).min(1.4) } else { 0.0 };
    let target = Vec3::new(
        (p.bob_phase * 0.5).cos() * 0.045 * amp,
        PLAYER_EYE_HEIGHT + p.bob_phase.sin().abs() * 0.07 * amp,
        0.0,
    );
    let k = (time.delta_secs() * 14.0).min(1.0);
    cam.translation = cam.translation.lerp(target, k);
    // FOV kick while dashing.
    if let Projection::Perspective(pp) = proj.as_mut() {
        let want = if p.dash_timer > 0.0 { BASE_FOV_DEG + 12.0 } else { BASE_FOV_DEG };
        pp.fov += (want.to_radians() - pp.fov) * (time.delta_secs() * 10.0).min(1.0);
    }
}

pub fn hazard_damage(mut players: Query<&mut Player>) {
    let Ok(mut p) = players.single_mut() else { return };
    if p.hazard_timer < 0.0 {
        p.apply_damage(HAZARD_DAMAGE);
        p.hazard_timer += HAZARD_TICK;
    }
}

pub fn check_death(players: Query<&Player>, mut next: ResMut<NextState<AppState>>) {
    if let Ok(p) = players.single()
        && p.is_dead() {
            next.set(AppState::GameOver);
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armor_absorbs_half_and_invuln_blocks() {
        let mut p = Player { armor: 10.0, ..default() };
        let lost = p.apply_damage(40.0);
        assert_eq!(p.armor, 0.0);
        assert_eq!(lost, 30.0);
        assert_eq!(p.hp, 70.0);
        p.invuln_timer = 1.0;
        assert_eq!(p.apply_damage(50.0), 0.0);
        assert_eq!(p.hp, 70.0);
    }

    #[test]
    fn wish_direction_follows_yaw() {
        let w = wish_direction(0.0, 1.0, 0.0);
        assert!((w - Vec3::NEG_Z).length() < 1e-5);
        let r = wish_direction(0.0, 0.0, 1.0);
        assert!((r - Vec3::X).length() < 1e-5);
        let diag = wish_direction(0.0, 1.0, 1.0);
        assert!((diag.length() - 1.0).abs() < 1e-5);
    }
}
