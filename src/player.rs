//! Quake / CPMA boomer-shooter FPS controller: mouse look, Quake ground
//! friction + acceleration, CPMA-style air-strafing and air control, hold-Space
//! auto-hop (bunny hopping) with jump buffer and coyote time, Airplane Mode
//! dash (which carries into a bhop), view bob / speed FOV / strafe roll and
//! hazard damage. The `Player` entity's translation is the feet position and
//! its rotation holds the yaw; the child `PlayerCamera` holds pitch + roll.
//!
//! The horizontal velocity model is split into pure, unit-tested helpers
//! ([`apply_ground_friction`], [`accelerate`], [`air_accelerate`],
//! [`apply_air_control`], [`apply_air_drag`]); [`move_player`] only wires
//! input, timers, dash and jump around them.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::apps::{HijackedHotspot, HIJACK_STRENGTH};
use crate::assets_gen::{FloorTheme, GameAssets};
use crate::audio::{advance_stride, footstep_sfx, SfxEvent, SfxQueue, LAND_THUD_SPEED, STEP_STRIDE};
use crate::bot_input::{take_flag, BotControl};
use crate::enemies::{fiend_jammer_fields_iter, Enemy};
use crate::gamepad::GamepadInput;
use crate::fx::{spawn_burst, spawn_flash_light, spawn_particles, spawn_shockwave, BurstColor, FxAssets, HudFeed};
use crate::level::{DemonJammer, JumpPad, JumpPadGlow, JumpPadLight, LevelGrid, SignalTower, PAD_DORMANT_LIGHT, PAD_POWERED_LIGHT};
use crate::meta::{live_settings, ProfileStore, RunLoadout, StarterChip, UserSettings, BASE_FOV_RANGE, EFFECT_SCALE_RANGE, MOUSE_SENS_RANGE};
use crate::physics::{resolve_player_movement, JUMP_VELOCITY, PLAYER_HEIGHT, PLAYER_RADIUS, PLAYER_STEP_HEIGHT};
use crate::physics::count_wall_occlusions;
use crate::signal::{active_jammer_fields_iter, boost_reading, compute_signal_at, hotspot_boost, overclock_move_factor, HotspotField, JammerField, SignalState};
use crate::state::{AppState, GameSet, PlayerModifiers, RunStats};

pub const PLAYER_EYE_HEIGHT: f32 = 1.6;
/// Base run speed (m/s) before upgrades / 5G Overclock.
pub const MOVE_SPEED: f32 = 11.0;
pub const DASH_SPEED: f32 = 30.0;
pub const DASH_DURATION: f32 = 0.16;
pub const DASH_COOLDOWN: f32 = 2.0;
pub const MOUSE_SENSITIVITY: f32 = 0.0022;
pub const HAZARD_DAMAGE: f32 = 10.0;
pub const HAZARD_TICK: f32 = 0.5;
pub const BASE_FOV_DEG: f32 = 85.0;

// --- Quake / CPMA movement tuning -------------------------------------------

/// Quake `sv_friction`: fraction of speed shed per second on the ground.
pub const GROUND_FRICTION: f32 = 8.0;
/// Quake `sv_stopspeed`: below this, friction acts as if moving this fast
/// (so slow drifts stop crisply instead of decaying forever).
pub const STOP_SPEED: f32 = 3.5;
/// Quake `sv_accelerate` on the ground.
pub const GROUND_ACCEL: f32 = 12.0;
/// VQ3-style air acceleration (forward / diagonal strafe-jumping).
pub const AIR_ACCEL: f32 = 2.4;
/// CPMA `strafeaccelerate`: pure A/D air-strafing.
pub const AIR_STRAFE_ACCEL: f32 = 70.0;
/// CPMA `wishspeed` cap while pure air-strafing (m/s, at base speed).
pub const AIR_STRAFE_WISH_SPEED: f32 = 2.8;
/// CPMA `aircontrol` (W-only air steering). Applied to the unit heading, so
/// the turn rate is speed-independent (CPMA's 150 normalized by its 320 ups).
pub const AIR_CONTROL: f32 = 0.4;
/// Horizontal air speed (m/s, at base speed) below which there is no drag.
pub const AIR_DRAG_START: f32 = 22.0;
/// Quadratic air-drag coefficient above [`AIR_DRAG_START`]: `k * excess^2` m/s^2
/// (1.9 m/s^2 at 30 m/s, 3 m/s^2 at 32 m/s, 5.9 m/s^2 at 36 m/s). Peak 60 Hz
/// air-strafe gain at 32 m/s is ~3.7 m/s^2, so skilled chains settle ~33 m/s.
pub const AIR_DRAG_K: f32 = 0.03;
/// Soft-capped air speed ceiling (m/s, at base speed).
pub const MAX_AIR_SPEED: f32 = 36.0;
/// Absolute air speed ceiling regardless of upgrades (physics stability).
pub const AIR_SPEED_HARD_CAP: f32 = 48.0;
/// Grace period to still jump after running off a ledge.
pub const COYOTE_TIME: f32 = 0.10;
/// Seconds grounded before footsteps count (longer than a bhop's landing substep).
const FOOTSTEP_MIN_GROUND_TIME: f32 = 0.06;
/// Horizontal speed (m/s) below which the player is standing, not walking.
const FOOTSTEP_MIN_SPEED: f32 = 1.5;

/// A `Space` tap this early before landing still hops on touchdown.
pub const JUMP_BUFFER_TIME: f32 = 0.12;
/// Bhop chain breaks after standing on the ground this long without jumping.
pub const BHOP_RESET_TIME: f32 = 0.12;
/// A hop extends the chain above `base_max_speed *` this.
pub const BHOP_CHAIN_SPEED: f32 = 1.05;
/// The chain breaks below `base_max_speed *` this.
pub const BHOP_BREAK_SPEED: f32 = 0.95;
/// Chained hops above `base_max_speed *` this play the kinetic bhop chirp.
pub const BHOP_SFX_SPEED: f32 = 1.15;
/// Maximum strafe camera roll (radians, ~1.4 deg).
pub const MAX_STRAFE_TILT: f32 = 0.025;
/// Strafe roll damping rate (1/s).
pub const STRAFE_TILT_RATE: f32 = 9.0;
/// Extra FOV at [`SPEED_FOV_FULL`] horizontal speed.
pub const SPEED_FOV_MAX_DEG: f32 = 14.0;
/// Horizontal speed (m/s) at which the speed FOV is fully widened.
pub const SPEED_FOV_FULL: f32 = 32.0;
/// FOV kick while an Airplane Mode dash is active.
pub const DASH_FOV_DEG: f32 = 12.0;
/// Cap on the combined speed + dash FOV bonus.
pub const MAX_FOV_BONUS_DEG: f32 = 18.0;
/// Longest movement substep (s): every frame is split into uniform substeps of
/// at most 1/120 s, so 30 / 60 / 120 / 144 Hz play converges to the same speed.
pub const MOVE_SUBSTEP_DT: f32 = 1.0 / 120.0;
/// Extra air drag above the (Overclock-scaled) air speed ceiling, relative to
/// the normal quadratic drag: speed bleeds smoothly down to a lowered ceiling.
pub const OVER_CAP_DRAG_MULT: f32 = 2.5;
/// Minimum bleed (m/s^2) while above the air speed ceiling.
pub const OVER_CAP_MIN_BLEED: f32 = 6.0;

// --- Signal Jump Pads ----------------------------------------------------------
/// Horizontal trigger radius of a jump pad (m).
pub const JUMP_PAD_RADIUS: f32 = 1.35;
/// Feet must be within this height of the pad surface (m).
pub const JUMP_PAD_VERTICAL: f32 = 1.2;
/// Seconds before a pad can launch again.
pub const JUMP_PAD_COOLDOWN: f32 = 0.35;
/// Signal bars at a pad's position that power it.
pub const JUMP_PAD_MIN_BARS: u8 = 2;
/// Seconds between pad power re-evaluations.
pub const JUMP_PAD_EVAL_SECS: f32 = 0.25;
/// Seconds between "pad offline" toasts.
pub const JUMP_PAD_TOAST_SECS: f32 = 3.0;
/// Launch shockwave glow `(color, alpha)`.
pub const JUMP_PAD_GLOW: (Color, f32) = (Color::srgb(0.25, 1.0, 0.7), 0.8);

/// A pad is powered at [`JUMP_PAD_MIN_BARS`]+ bars.
pub fn jump_pad_powered(bars: u8) -> bool {
    bars >= JUMP_PAD_MIN_BARS
}

/// Whether feet at `feet` stand on the pad at `pad`.
pub fn on_jump_pad(feet: Vec3, pad: Vec3) -> bool {
    Vec2::new(feet.x - pad.x, feet.z - pad.z).length() <= JUMP_PAD_RADIUS && (feet.y - pad.y).abs() <= JUMP_PAD_VERTICAL
}

/// Whether the horizontal/vertical segment `from -> to` crossed the pad trigger
/// cylinder (prevents high-speed bhoppers from skipping across a pad between ticks).
pub fn crossed_jump_pad(from: Vec3, to: Vec3, pad: Vec3) -> bool {
    let seg = Vec2::new(to.x - from.x, to.z - from.z);
    let len_sq = seg.length_squared();
    let t = if len_sq > 1e-6 {
        (Vec2::new(pad.x - from.x, pad.z - from.z).dot(seg) / len_sq).clamp(0.0, 1.0)
    } else {
        1.0
    };
    on_jump_pad(from.lerp(to, t), pad)
}

/// Jump pad launch: vertical speed at least `launch_vel.y`, horizontal pad
/// boost blended into the existing momentum so the result is at least the
/// pad's own horizontal speed and never slower than the incoming bhop speed
/// (clamped only to [`AIR_SPEED_HARD_CAP`]). Detaches from the ground.
pub fn apply_jump_pad(p: &mut Player, launch_vel: Vec3) {
    let v = Vec2::new(p.velocity.x, p.velocity.z);
    let pad = Vec2::new(launch_vel.x, launch_vel.z);
    let target = v.length().max(pad.length()).min(AIR_SPEED_HARD_CAP);
    let mut h = v + pad;
    if h.length() < target {
        h = h.normalize_or(pad.normalize_or(v.normalize_or_zero())) * target;
    }
    if h.length() > AIR_SPEED_HARD_CAP {
        h = h.normalize() * AIR_SPEED_HARD_CAP;
    }
    p.velocity = Vec3::new(h.x, p.velocity.y.max(launch_vel.y), h.y);
    p.grounded = false;
    p.coyote_timer = 0.0;
    p.dash_timer = 0.0;
    p.jump_buffer_timer = 0.0;
}

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
    pub ammo_rockets: u32,
    pub max_rockets: u32,
    /// Seconds until the next dash is available.
    pub dash_cooldown: f32,
    /// Remaining seconds of the active dash burst.
    pub dash_timer: f32,
    pub invuln_timer: f32,
    pub velocity: Vec3,
    pub yaw: f32,
    /// Yaw at the end of the previous `move_player` (mouse turns are spread
    /// across that frame's movement substeps).
    pub prev_yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
    pub hazard_timer: f32,
    pub bob_phase: f32,
    /// Seconds since the last damage taken (for HUD flashes).
    pub since_damage: f32,
    /// True on the frame a dash begins (Airplane Mode shockwave hook).
    pub dash_started: bool,
    /// Seconds left to still jump after leaving the ground.
    pub coyote_timer: f32,
    /// Seconds left on a buffered `Space` tap.
    pub jump_buffer_timer: f32,
    /// Consecutive speed-carrying hops (HUD `BHOP xN`).
    pub bhop_chain: u32,
    /// Seconds continuously grounded.
    pub ground_time: f32,
    /// Damped strafe camera roll (radians; negative = leaning right).
    pub strafe_tilt: f32,
    /// Feet position before this frame's `move_player` step (for swept pad contact).
    pub prev_pos: Option<Vec3>,
    /// Grounded running distance (m) since the last footstep.
    pub stride: f32,
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
            ammo_rockets: 10,
            max_rockets: crate::upgrades::BASE_MAX_ROCKETS,
            dash_cooldown: 0.0,
            dash_timer: 0.0,
            invuln_timer: 0.0,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            prev_yaw: 0.0,
            pitch: 0.0,
            grounded: false,
            hazard_timer: 0.0,
            bob_phase: 0.0,
            since_damage: 999.0,
            dash_started: false,
            coyote_timer: 0.0,
            jump_buffer_timer: 0.0,
            bhop_chain: 0,
            ground_time: 0.0,
            strafe_tilt: 0.0,
            prev_pos: None,
            stride: 0.0,
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

/// Web build: the browser's Pointer Lock state as last observed
/// (`document.pointerLockElement`). Native never inserts it.
///
/// In browsers `Esc` exits Pointer Lock itself and the key event usually never
/// reaches the page, so `hud::pause_input` cannot see it; alt-tab and focus
/// loss release the lock the same way.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserPointerLock {
    pub locked: bool,
}

/// Pause when the browser drops a lock we actually held while playing. A lock
/// that was never granted (pending request, browser cooldown after `Esc`) does
/// not pause; the player clicks to recapture instead. Our own releases happen
/// in `OnExit(Playing)`, so they are never seen from `Playing`.
pub fn browser_unlock_should_pause(was_locked: bool, now_locked: bool, playing: bool) -> bool {
    playing && was_locked && !now_locked
}

/// Mouse look needs our capture flag and, in the browser, a granted lock.
pub fn mouse_look_active(captured: CursorCaptured, browser: Option<&BrowserPointerLock>) -> bool {
    captured.0 && browser.is_none_or(|b| b.locked)
}

/// Track the browser Pointer Lock and pause on an external unlock (see
/// [`browser_unlock_should_pause`]). A transition already queued this frame
/// (floor clear, death, ...) wins over the pause.
pub fn sync_browser_pointer_lock(
    In(now_locked): In<bool>,
    state: Res<State<AppState>>,
    mut lock: ResMut<BrowserPointerLock>,
    mut next: ResMut<NextState<AppState>>,
) {
    let playing = *state.get() == AppState::Playing;
    if browser_unlock_should_pause(lock.locked, now_locked, playing) && matches!(*next, NextState::Unchanged) {
        info!("pointer lock released by the browser - pausing");
        next.set(AppState::Paused);
    }
    if lock.locked != now_locked {
        lock.locked = now_locked;
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_pointer_locked() -> bool {
    web_sys::window().and_then(|w| w.document()).and_then(|d| d.pointer_lock_element()).is_some()
}

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        // Runs in every state so the observed lock is fresh when `Playing`
        // is re-entered (a stale `true` would pause instantly on resume).
        #[cfg(target_arch = "wasm32")]
        app.init_resource::<BrowserPointerLock>()
            .add_systems(Update, browser_pointer_locked.pipe(sync_browser_pointer_lock).before(GameSet::Input));
        app.init_resource::<CursorCaptured>()
            .add_systems(OnEnter(AppState::Playing), capture_cursor)
            .add_systems(OnExit(AppState::Playing), release_cursor)
            // Every pre-run screen ends the previous run, so the next
            // "DEPLOY" spawns a fresh player with the newly equipped chip.
            // (`Playing <-> UpgradeSelect` keeps the player alive.)
            .add_systems(OnEnter(AppState::MainMenu), despawn_player)
            .add_systems(OnEnter(AppState::Loadout), despawn_player)
            .add_systems(OnEnter(AppState::ContractsFeats), despawn_player)
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
            .add_systems(Startup, prewarm_pad_fades)
            .add_systems(Update, move_player.in_set(GameSet::Movement))
            .add_systems(Update, update_jump_pads.in_set(GameSet::Movement).after(move_player).run_if(resource_exists::<LevelGrid>))
            .add_systems(Update, (view_bob, hazard_damage).chain().in_set(GameSet::World));
    }
}

/// Fresh run player with the starter chip's vitals (Survey OS 85 HP, Faraday +25 armor).
pub fn new_run_player(chip: StarterChip, yaw: f32) -> Player {
    let mut p = Player { yaw, prev_yaw: yaw, ..default() };
    chip.apply_to_player(&mut p);
    p
}

fn spawn_player(commands: &mut Commands, grid: &LevelGrid, chip: StarterChip, settings: &UserSettings, msaa: bevy::render::view::Msaa) {
    let pos = grid.spawn_world();
    commands
        .spawn((
            new_run_player(chip, grid.spawn_yaw),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(grid.spawn_yaw)),
            Visibility::default(),
        ))
        .with_children(|p| {
            p.spawn((
                PlayerCamera,
                Camera3d::default(),
                msaa,
                Projection::Perspective(PerspectiveProjection {
                    fov: base_fov_rad(settings),
                    near: 0.05,
                    far: 400.0,
                    ..default()
                }),
                theme_fog(grid.floor),
                Transform::from_xyz(0.0, PLAYER_EYE_HEIGHT, 0.0),
            ));
        });
}

/// The camera's distance fog for a floor's sector theme.
pub fn theme_fog(floor: u32) -> DistanceFog {
    let spec = FloorTheme::for_floor(floor).spec();
    DistanceFog {
        color: spec.fog_color,
        directional_light_color: spec.fog_dir_color,
        directional_light_exponent: 20.0,
        falloff: FogFalloff::Linear { start: 10.0, end: 70.0 },
    }
}

/// Spawn the player once a level exists (player persists across floors).
fn ensure_player(
    mut commands: Commands,
    grid: Option<Res<LevelGrid>>,
    loadout: Option<Res<RunLoadout>>,
    store: Option<Res<ProfileStore>>,
    render_msaa: Option<Res<crate::perf::RenderMsaa>>,
    players: Query<(), With<Player>>,
) {
    if let Some(grid) = grid
        && players.is_empty()
    {
        let chip = loadout.map(|l| l.chip).unwrap_or_default();
        let msaa = render_msaa.map(|m| m.0).unwrap_or_default();
        spawn_player(&mut commands, &grid, chip, &live_settings(store.as_deref()), msaa);
    }
}

/// New floor: put the existing player at the airlock, standing still with a
/// level, un-bobbed, base-FOV camera (no speed FOV / bob carried over).
#[allow(clippy::type_complexity)]
fn snap_player_to_spawn(
    grid: Res<LevelGrid>,
    store: Option<Res<ProfileStore>>,
    mut q: Query<(&mut Player, &mut Transform), Without<PlayerCamera>>,
    mut cams: Query<(&mut Transform, &mut Projection, Option<&mut DistanceFog>), With<PlayerCamera>>,
) {
    for (mut p, mut tf) in &mut q {
        tf.translation = grid.spawn_world();
        p.yaw = grid.spawn_yaw;
        p.prev_yaw = p.yaw;
        p.pitch = 0.0;
        p.velocity = Vec3::ZERO;
        p.grounded = true;
        p.dash_timer = 0.0;
        p.coyote_timer = 0.0;
        p.jump_buffer_timer = 0.0;
        p.bhop_chain = 0;
        p.ground_time = 0.0;
        p.stride = 0.0;
        p.strafe_tilt = 0.0;
        p.bob_phase = 0.0;
        p.prev_pos = Some(tf.translation);
        tf.rotation = Quat::from_rotation_y(p.yaw);
    }
    for (mut cam, mut proj, fog) in &mut cams {
        if let Some(mut fog) = fog {
            *fog = theme_fog(grid.floor);
        }
        cam.translation = Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0);
        cam.rotation = Quat::IDENTITY;
        if let Projection::Perspective(pp) = proj.as_mut() {
            pp.fov = base_fov_rad(&live_settings(store.as_deref()));
        }
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

/// Click to recapture the cursor while playing (Esc pauses via `hud::pause_input`;
/// in the browser also via [`sync_browser_pointer_lock`]). In the browser a
/// click also re-requests a lock that was never granted.
fn cursor_capture_input(
    mouse: Res<ButtonInput<MouseButton>>,
    browser: Option<Res<BrowserPointerLock>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut captured: ResMut<CursorCaptured>,
) {
    let Ok(mut c) = cursor.single_mut() else { return };
    if mouse.just_pressed(MouseButton::Left) && !mouse_look_active(*captured, browser.as_deref()) {
        set_cursor(&mut c, true);
        captured.0 = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn mouse_look(
    motion: Res<AccumulatedMouseMotion>,
    captured: Res<CursorCaptured>,
    browser: Option<Res<BrowserPointerLock>>,
    store: Option<Res<ProfileStore>>,
    bot: Option<Res<BotControl>>,
    pad: Option<Res<GamepadInput>>,
    mut players: Query<(&mut Player, &mut Transform), Without<PlayerCamera>>,
    mut cams: Query<&mut Transform, With<PlayerCamera>>,
) {
    let Ok((mut p, mut tf)) = players.single_mut() else { return };
    if mouse_look_active(*captured, browser.as_deref()) && motion.delta != Vec2::ZERO {
        let sens = effective_mouse_sensitivity(&live_settings(store.as_deref()));
        p.yaw -= motion.delta.x * sens;
        p.pitch = (p.pitch - motion.delta.y * sens).clamp(-1.5, 1.5);
    }
    // Right Stick: already curved, sensitivity- and dt-scaled radians; needs
    // no cursor capture (Steam Deck / Android / web work without a click).
    if let Some(look) = pad.map(|g| g.look_delta)
        && look != Vec2::ZERO
    {
        p.yaw -= look.x;
        p.pitch = (p.pitch + look.y).clamp(-1.5, 1.5);
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
        cam.rotation = camera_rotation(p.pitch, p.strafe_tilt);
    }
}

/// Head camera local rotation: pitch, then roll about the view axis.
pub fn camera_rotation(pitch: f32, roll: f32) -> Quat {
    Quat::from_rotation_x(pitch) * Quat::from_rotation_z(roll)
}

/// Live world-space eye pose from the player body `Transform` (feet + yaw) and
/// the child `PlayerCamera` local `Transform` (eye height, bob, pitch, roll).
/// Unlike `GlobalTransform` (propagated in `PostUpdate`), this already
/// reflects this frame's `mouse_look` / `move_player` / `view_bob`.
pub fn live_eye_pose(body: &Transform, cam_local: &Transform) -> Transform {
    body.mul_transform(*cam_local)
}

/// Frame-rate-independent exponential smoothing factor for a damping `rate` (1/s).
pub fn smooth_factor(rate: f32, dt: f32) -> f32 {
    1.0 - (-rate * dt).exp()
}

/// WASD intent in world space (unit length or zero) for a given yaw.
pub fn wish_direction(yaw: f32, forward: f32, right: f32) -> Vec3 {
    let rot = Quat::from_rotation_y(yaw);
    let f = rot * Vec3::NEG_Z;
    let r = rot * Vec3::X;
    (f * forward + r * right).normalize_or_zero()
}

// ---------------------------------------------------------------------------
// Quake / CPMA horizontal movement (pure helpers, XZ plane as `Vec2(x, z)`)
// ---------------------------------------------------------------------------

/// Upgrade / Overclock multiplier relative to the stock run speed.
pub fn speed_scale(base_max_speed: f32) -> f32 {
    (base_max_speed / MOVE_SPEED).max(0.25)
}

/// Quake `PM_Friction`: shed `max(speed, STOP_SPEED) * GROUND_FRICTION` m/s per
/// second, never reversing direction.
pub fn apply_ground_friction(vel_xz: Vec2, dt: f32) -> Vec2 {
    let speed = vel_xz.length();
    if speed < 1e-4 {
        return Vec2::ZERO;
    }
    let drop = speed.max(STOP_SPEED) * GROUND_FRICTION * dt;
    vel_xz * ((speed - drop).max(0.0) / speed)
}

/// Quake `PM_Accelerate`: add speed along `wish_dir` only until the velocity's
/// projection on it reaches `wish_speed` (perpendicular speed is untouched,
/// which is what makes strafe-jumping gain speed).
pub fn accelerate(vel_xz: Vec2, wish_dir: Vec2, wish_speed: f32, accel: f32, dt: f32) -> Vec2 {
    if wish_dir == Vec2::ZERO {
        return vel_xz;
    }
    let current_speed = vel_xz.dot(wish_dir);
    let add_speed = wish_speed - current_speed;
    if add_speed <= 0.0 {
        return vel_xz;
    }
    vel_xz + wish_dir * (accel * wish_speed * dt).min(add_speed)
}

/// Airborne acceleration. Pure A/D (`pure_strafe`) uses the CPMA strafe curve
/// (huge accel, tiny wish speed: turning the mouse while strafing curves the
/// trajectory and builds speed); anything else uses VQ3 air accel.
/// At or above the air speed ceiling it can still bend the path, but never
/// adds speed.
pub fn air_accelerate(vel_xz: Vec2, wish_dir: Vec2, pure_strafe: bool, base_max_speed: f32, dt: f32) -> Vec2 {
    let v = if pure_strafe {
        let wish_speed = (AIR_STRAFE_WISH_SPEED * speed_scale(base_max_speed)).min(base_max_speed);
        accelerate(vel_xz, wish_dir, wish_speed, AIR_STRAFE_ACCEL, dt)
    } else {
        accelerate(vel_xz, wish_dir, base_max_speed, AIR_ACCEL, dt)
    };
    let speed = vel_xz.length();
    if speed >= air_speed_cap(base_max_speed) && v.length() > speed {
        return v.normalize_or_zero() * speed;
    }
    v
}

/// CPMA `PM_Aircontrol` (W only, airborne): rotate the heading toward
/// `wish_dir` while preserving speed. Only steers while moving forward
/// (`dot > 0`); the pull grows with `dot^2`, so small corrections are crisp
/// and backwards input does nothing.
pub fn apply_air_control(vel_xz: Vec2, wish_dir: Vec2, dt: f32) -> Vec2 {
    let speed = vel_xz.length();
    if speed < 1e-3 || wish_dir == Vec2::ZERO {
        return vel_xz;
    }
    let dir = vel_xz / speed;
    let dot = dir.dot(wish_dir);
    if dot <= 0.0 {
        return vel_xz;
    }
    let k = 32.0 * AIR_CONTROL * dot * dot * dt;
    (dir + wish_dir * k).normalize_or(dir) * speed
}

/// Air speed limits: free below `AIR_DRAG_START`, soft quadratic drag above it.
/// Above the soft ceiling ([`air_speed_cap`]) speed is not snapped down: it
/// bleeds smoothly ([`OVER_CAP_DRAG_MULT`] x the drag, at least
/// [`OVER_CAP_MIN_BLEED`] m/s^2) until it settles on the ceiling, so losing a
/// 5G Overclock bar mid-hop eases 45 -> 36 m/s instead of a one-frame snap.
/// [`AIR_SPEED_HARD_CAP`] is the only immediate clamp. All thresholds scale
/// with upgrades / 5G Overclock.
pub fn apply_air_drag(vel_xz: Vec2, base_max_speed: f32, dt: f32) -> Vec2 {
    let scale = speed_scale(base_max_speed);
    let start = AIR_DRAG_START * scale;
    let cap = air_speed_cap(base_max_speed);
    let raw = vel_xz.length();
    let speed = raw.min(AIR_SPEED_HARD_CAP);
    if speed <= start {
        return vel_xz;
    }
    let excess = speed - start;
    let drag = AIR_DRAG_K / scale * excess * excess;
    let slowed = if speed > cap {
        (speed - (drag * OVER_CAP_DRAG_MULT).max(OVER_CAP_MIN_BLEED) * dt).max(cap)
    } else {
        (speed - drag * dt).max(start)
    };
    vel_xz * (slowed / raw)
}

/// Hard horizontal air speed ceiling for a given base run speed.
pub fn air_speed_cap(base_max_speed: f32) -> f32 {
    (MAX_AIR_SPEED * speed_scale(base_max_speed)).min(AIR_SPEED_HARD_CAP)
}

/// One tick of horizontal movement (no dash): Quake friction + accel on the
/// ground, CPMA air strafe / air control + soft drag in the air.
pub fn step_horizontal(vel_xz: Vec2, wish_dir: Vec2, input: MoveInput, grounded: bool, base_max_speed: f32, dt: f32) -> Vec2 {
    if grounded {
        let v = apply_ground_friction(vel_xz, dt);
        return accelerate(v, wish_dir, base_max_speed, GROUND_ACCEL, dt);
    }
    let mut v = air_accelerate(vel_xz, wish_dir, input.pure_strafe(), base_max_speed, dt);
    if input.air_control() {
        v = apply_air_control(v, wish_dir, dt);
    }
    apply_air_drag(v, base_max_speed, dt)
}

/// Local movement keys for one tick (`forward`: W/S, `right`: D/A, each -1..1).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MoveInput {
    pub forward: f32,
    pub right: f32,
}

/// Keyboard intent plus the Left Stick (`x` = right, `y` = forward), each
/// axis clamped to -1..1. Exact zeros survive (a pure-strafe stick keeps the
/// CPMA air-strafe / air-control modes, see `gamepad::stick_move_axes`).
pub fn pad_blend_move(keys: MoveInput, stick: Vec2) -> MoveInput {
    if !stick.is_finite() {
        return keys;
    }
    MoveInput { forward: (keys.forward + stick.y).clamp(-1.0, 1.0), right: (keys.right + stick.x).clamp(-1.0, 1.0) }
}

impl MoveInput {
    /// Holding only A or D: CPMA air-strafe.
    pub fn pure_strafe(self) -> bool {
        self.forward == 0.0 && self.right != 0.0
    }

    /// Holding only W: CPMA air control.
    pub fn air_control(self) -> bool {
        self.forward > 0.0 && self.right == 0.0
    }
}

/// Dash velocity along `dir`: at least `DASH_SPEED`, but never slower than the
/// current horizontal speed in *any* direction (up to `AIR_SPEED_HARD_CAP`), so
/// a 90 deg Airplane Mode dash redirects a full-speed bhop around a corner.
pub fn dash_velocity(vel_xz: Vec2, dir: Vec2) -> Vec2 {
    dir * DASH_SPEED.max(vel_xz.length()).min(AIR_SPEED_HARD_CAP)
}

/// Target strafe roll (radians): leans into the strafe key / sideways drift,
/// stronger in the air. Negative = lean right.
pub fn strafe_tilt_target(input_right: f32, side_speed_frac: f32, grounded: bool) -> f32 {
    let side = (input_right.clamp(-1.0, 1.0) * 0.65 + side_speed_frac.clamp(-1.0, 1.0) * 0.35).clamp(-1.0, 1.0);
    -side * MAX_STRAFE_TILT * if grounded { 0.6 } else { 1.0 }
}

/// Extra FOV (degrees) for horizontal speed and an active dash: speed widens
/// up to +14 deg at 32 m/s, blended with the +12 deg dash kick, capped at +18.
pub fn fov_bonus_deg(horizontal_speed: f32, dashing: bool) -> f32 {
    let t = ((horizontal_speed - MOVE_SPEED) / (SPEED_FOV_FULL - MOVE_SPEED)).clamp(0.0, 1.0);
    let speed_fov = SPEED_FOV_MAX_DEG * t;
    let dash_fov = if dashing { DASH_FOV_DEG } else { 0.0 };
    (speed_fov.max(dash_fov) + 0.25 * speed_fov.min(dash_fov)).min(MAX_FOV_BONUS_DEG)
}

/// Radians of mouse look per pixel of motion (user sensitivity applied).
pub fn effective_mouse_sensitivity(settings: &UserSettings) -> f32 {
    let m = if settings.mouse_sensitivity.is_finite() { settings.mouse_sensitivity.clamp(MOUSE_SENS_RANGE.0, MOUSE_SENS_RANGE.1) } else { 1.0 };
    MOUSE_SENSITIVITY * m
}

/// The user's resting FOV (vertical, radians).
pub fn base_fov_rad(settings: &UserSettings) -> f32 {
    let deg = if settings.base_fov_deg.is_finite() { settings.base_fov_deg.clamp(BASE_FOV_RANGE.0, BASE_FOV_RANGE.1) } else { BASE_FOV_DEG };
    deg.to_radians()
}

/// [`fov_bonus_deg`] honoring the Dynamic Speed FOV toggle and intensity.
pub fn effective_fov_bonus_deg(horizontal_speed: f32, dashing: bool, settings: &UserSettings) -> f32 {
    if !settings.speed_fov_enabled {
        return 0.0;
    }
    let scale = if settings.speed_fov_scale.is_finite() { settings.speed_fov_scale.clamp(EFFECT_SCALE_RANGE.0, EFFECT_SCALE_RANGE.1) } else { 1.0 };
    fov_bonus_deg(horizontal_speed, dashing) * scale
}

/// [`strafe_tilt_target`] honoring the Strafe Roll toggle and intensity.
pub fn effective_strafe_tilt_target(input_right: f32, side_speed_frac: f32, grounded: bool, settings: &UserSettings) -> f32 {
    if !settings.strafe_roll_enabled {
        return 0.0;
    }
    let scale = if settings.strafe_roll_scale.is_finite() { settings.strafe_roll_scale.clamp(EFFECT_SCALE_RANGE.0, EFFECT_SCALE_RANGE.1) } else { 1.0 };
    strafe_tilt_target(input_right, side_speed_frac, grounded) * scale
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn move_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    grid: Option<Res<LevelGrid>>,
    modifiers: Res<PlayerModifiers>,
    signal: Res<SignalState>,
    mut sfx: ResMut<SfxQueue>,
    mut bot: Option<ResMut<BotControl>>,
    (store, run, pad): (Option<Res<ProfileStore>>, Option<ResMut<RunStats>>, Option<Res<GamepadInput>>),
    mut q: Query<(&mut Player, &mut Transform)>,
) {
    let Some(grid) = grid else { return };
    let Ok((mut p, mut tf)) = q.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);

    let axis = |pos: KeyCode, neg: KeyCode| (keys.pressed(pos) as i32 - keys.pressed(neg) as i32) as f32;
    let mut input = MoveInput {
        forward: (axis(KeyCode::KeyW, KeyCode::KeyS) + axis(KeyCode::ArrowUp, KeyCode::ArrowDown)).clamp(-1.0, 1.0),
        right: axis(KeyCode::KeyD, KeyCode::KeyA),
    };
    // Left Stick blends with WASD (clamped, so both together never exceed a key).
    let pad = pad.as_deref().cloned().unwrap_or_default();
    input = pad_blend_move(input, pad.move_axes);
    if let Some(b) = &bot
        && b.strafe_axes != Vec2::ZERO
    {
        input = MoveInput { forward: b.strafe_axes.y.clamp(-1.0, 1.0), right: b.strafe_axes.x.clamp(-1.0, 1.0) };
    }
    let bot_jump = bot.as_ref().is_some_and(|b| b.jump);
    let bot_wish = bot.as_ref().map(|b| b.wish_dir).unwrap_or(Vec3::ZERO);
    if bot_wish != Vec3::ZERO {
        // World-space bot intent: no local keys, so no strafe / air-control curves.
        input = MoveInput::default();
    }
    // 5G Overclock: faster in strong signal.
    let base_max_speed = MOVE_SPEED * modifiers.move_speed_mult * overclock_move_factor(signal.bars, &modifiers);

    p.dash_cooldown = (p.dash_cooldown - dt).max(0.0);
    p.invuln_timer = (p.invuln_timer - dt).max(0.0);
    p.since_damage += dt;
    // Edge inputs fire on the first substep only; held keys act on every one.
    let space_tap = keys.just_pressed(KeyCode::Space) || pad.jump_pressed;
    let shift = keys.just_pressed(KeyCode::ShiftLeft) || keys.just_pressed(KeyCode::ShiftRight) || pad.dash || take_flag(&mut bot, |b| &mut b.dash);
    let space_held = keys.pressed(KeyCode::Space) || bot_jump || pad.jump;
    p.dash_started = false;
    p.prev_pos = Some(tf.translation);

    // Uniform substeps of at most `MOVE_SUBSTEP_DT` (the epsilon keeps an
    // exact 60 Hz frame at two substeps despite float rounding).
    let substeps = ((dt / MOVE_SUBSTEP_DT - 1e-3).ceil().max(1.0)) as u32;
    let sub_dt = dt / substeps as f32;
    let mut thud = false;
    for i in 0..substeps {
        // Spread this frame's mouse turn across the substeps.
        let yaw_i = p.prev_yaw + (p.yaw - p.prev_yaw) * ((i + 1) as f32 / substeps as f32);
        let wish = if bot_wish != Vec3::ZERO {
            Vec3::new(bot_wish.x, 0.0, bot_wish.z).normalize_or_zero()
        } else {
            wish_direction(yaw_i, input.forward, input.right)
        };
        let wish_xz = Vec2::new(wish.x, wish.z);

        p.jump_buffer_timer = (p.jump_buffer_timer - sub_dt).max(0.0);
        if i == 0 && space_tap {
            p.jump_buffer_timer = JUMP_BUFFER_TIME;
        }
        p.coyote_timer = if p.grounded { COYOTE_TIME } else { (p.coyote_timer - sub_dt).max(0.0) };

        let mut vel = p.velocity;
        if i == 0 && shift && p.dash_ready() {
            // The dash is a one-shot: aim it with the latest view, not the
            // first substep's interpolated yaw.
            let dash_wish = if bot_wish != Vec3::ZERO { wish } else { wish_direction(p.yaw, input.forward, input.right) };
            let dir = if dash_wish != Vec3::ZERO { dash_wish } else { Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z };
            p.dash_timer = DASH_DURATION;
            p.dash_started = true;
            sfx.play(SfxEvent::Dash);
            p.dash_cooldown = DASH_COOLDOWN * modifiers.dash_cooldown_mult;
            if modifiers.dash_invulnerable {
                p.invuln_timer = p.invuln_timer.max(DASH_DURATION + 0.25);
            }
            let dv = dash_velocity(Vec2::new(vel.x, vel.z), Vec2::new(dir.x, dir.z));
            vel.x = dv.x;
            vel.z = dv.y;
            vel.y = vel.y.max(0.0);
        }

        // Jump BEFORE friction: a hop on the landing substep never touches the
        // ground friction, so hold-Space bunny hopping keeps 100% of its speed.
        let wants_jump = space_held || p.jump_buffer_timer > 0.0;
        let can_jump = p.grounded || p.coyote_timer > 0.0;
        if wants_jump && can_jump {
            vel.y = JUMP_VELOCITY;
            p.grounded = false;
            p.coyote_timer = 0.0;
            p.jump_buffer_timer = 0.0;
            // Dash-to-bhop: cancel the burst but keep its full horizontal speed.
            p.dash_timer = 0.0;
            let speed = Vec2::new(vel.x, vel.z).length();
            if speed > base_max_speed * BHOP_CHAIN_SPEED {
                p.bhop_chain += 1;
            }
            // The chirp celebrates the same linked-hop milestone as the HUD `BHOP x2+` stamp.
            let chained = p.bhop_chain >= 2 && speed > base_max_speed * BHOP_SFX_SPEED;
            sfx.play(if chained { SfxEvent::Bhop } else { SfxEvent::Jump });
        }

        let h = Vec2::new(vel.x, vel.z);
        let h = if p.dash_timer > 0.0 {
            // Dash burst: velocity locked; when it ends airborne the speed is kept.
            p.dash_timer -= sub_dt;
            h
        } else {
            step_horizontal(h, wish_xz, input, p.grounded, base_max_speed, sub_dt)
        };
        vel.x = h.x;
        vel.z = h.y;

        let (was_grounded, fall_vy) = (p.grounded, vel.y);
        let r = resolve_player_movement(&grid, tf.translation, vel, sub_dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, p.grounded);
        if r.grounded && !was_grounded {
            // Touchdown: thud unless the next substep chains a zero-friction hop.
            let chaining = space_held || p.jump_buffer_timer > 0.0;
            thud |= !chaining && -fall_vy > LAND_THUD_SPEED;
            // The first step after landing comes a half stride later.
            p.stride = STEP_STRIDE * 0.5;
        }
        tf.translation = r.pos;
        p.velocity = r.vel;
        p.grounded = r.grounded;
        // Touching the ground anywhere in the step (even a catwalk edge crossed
        // at 30 m/s) keeps the coyote window, so a buffered / held hop fires.
        if r.grounded || r.touched_ground {
            p.coyote_timer = COYOTE_TIME;
        }
        if r.on_hazard {
            p.hazard_timer -= sub_dt;
        } else {
            p.hazard_timer = 0.0;
        }

        // Bhop chain bookkeeping.
        p.ground_time = if r.grounded { p.ground_time + sub_dt } else { 0.0 };
        let speed = Vec2::new(r.vel.x, r.vel.z).length();
        if (r.grounded && p.ground_time > BHOP_RESET_TIME) || speed < base_max_speed * BHOP_BREAK_SPEED {
            p.bhop_chain = 0;
        }
    }
    p.prev_yaw = p.yaw;
    if thud {
        sfx.play(SfxEvent::LandThud);
    }
    // Footsteps: grounded running only (a clean bhop never settles on the
    // ground long enough, and a dash slide is silent).
    let moved = p.prev_pos.map_or(0.0, |prev| Vec2::new(tf.translation.x - prev.x, tf.translation.z - prev.z).length());
    if p.grounded
        && p.ground_time >= FOOTSTEP_MIN_GROUND_TIME
        && p.dash_timer <= 0.0
        && Vec2::new(p.velocity.x, p.velocity.z).length() > FOOTSTEP_MIN_SPEED
        && advance_stride(&mut p.stride, moved)
    {
        sfx.play(footstep_sfx(grid.cell(grid.world_to_cell(tf.translation)), tf.translation.y));
    }

    // Strafe roll toward the strafe key / sideways drift (applied by `view_bob`).
    let right_xz = Vec2::new(p.yaw.cos(), -p.yaw.sin());
    let side_frac = Vec2::new(p.velocity.x, p.velocity.z).dot(right_xz) / base_max_speed.max(1.0);
    let target = effective_strafe_tilt_target(input.right, side_frac, p.grounded, &live_settings(store.as_deref()));
    p.strafe_tilt += (target - p.strafe_tilt) * smooth_factor(STRAFE_TILT_RATE, dt);

    // Movement telemetry (peak speed / longest bhop chain) for records.
    if let Some(mut run) = run {
        let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
        if speed > run.peak_speed || p.bhop_chain > run.max_bhop_chain {
            run.track_movement(speed, p.bhop_chain);
        }
    }
}

fn view_bob(
    time: Res<Time>,
    store: Option<Res<ProfileStore>>,
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
    cam.translation = cam.translation.lerp(target, smooth_factor(14.0, time.delta_secs()));
    // Pitch from mouse look + damped strafe roll, composed in one place.
    cam.rotation = camera_rotation(p.pitch, p.strafe_tilt);
    // FOV widens with speed, kicks while dashing.
    if let Projection::Perspective(pp) = proj.as_mut() {
        let settings = live_settings(store.as_deref());
        let want = base_fov_rad(&settings) + effective_fov_bonus_deg(speed, p.dash_timer > 0.0, &settings).to_radians();
        pp.fov += (want - pp.fov) * smooth_factor(10.0, time.delta_secs());
    }
}

fn prewarm_pad_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[JUMP_PAD_GLOW]);
}

/// Signal Jump Pads: re-evaluate each pad's power from the signal field at
/// its position (same solver, jammer and fiend fields as the player's bars),
/// swap its shared glow material + light color, and launch a living player
/// standing on a powered, cooled-down pad.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_jump_pads(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    mods: Res<PlayerModifiers>,
    (assets, fx): (Res<GameAssets>, Res<FxAssets>),
    mut materials: ResMut<Assets<StandardMaterial>>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    mut players: Query<(&mut Player, &Transform)>,
    mut pads: Query<(&mut JumpPad, &Children)>,
    mut glows: Query<&mut MeshMaterial3d<StandardMaterial>, With<JumpPadGlow>>,
    mut lights: Query<&mut PointLight, With<JumpPadLight>>,
    (tower, jammers, fiends, hotspots): (
        Query<&SignalTower>,
        Query<(&DemonJammer, &GlobalTransform)>,
        Query<(&Enemy, &GlobalTransform)>,
        Query<(&HijackedHotspot, &Transform)>,
    ),
    (mut eval_timer, mut toast_timer, mut fields, mut spots): (Local<f32>, Local<f32>, Local<Vec<JammerField>>, Local<Vec<HotspotField>>),
) {
    let dt = time.delta_secs();
    *toast_timer = (*toast_timer - dt).max(0.0);
    *eval_timer -= dt;
    let mut eval = false;
    if *eval_timer <= 0.0 || pads.iter().any(|(p, _)| !p.primed) {
        *eval_timer = JUMP_PAD_EVAL_SECS;
        eval = true;
        fields.clear();
        fields.extend(active_jammer_fields_iter(jammers.iter()));
        fields.extend(fiend_jammer_fields_iter(fiends.iter()));
        spots.clear();
        spots.extend(hotspots.iter().map(|(h, tf)| HotspotField { pos: tf.translation, radius: h.radius, strength: HIJACK_STRENGTH }));
    }
    let tower_ref = grid.tower_world() + Vec3::Y * 1.5;
    let beacon = tower.single().map(|t| t.beacon).unwrap_or(tower_ref);
    let mut player = players.single_mut().ok();
    for (mut pad, children) in &mut pads {
        pad.cooldown = (pad.cooldown - dt).max(0.0);
        if eval {
            let eye = pad.pos + Vec3::Y * PLAYER_EYE_HEIGHT;
            let walls = count_wall_occlusions(&grid, eye, beacon);
            let base = compute_signal_at(eye, tower_ref, &fields, walls, &mods);
            let boosted = boost_reading(base, hotspot_boost(eye, &spots));
            let powered = jump_pad_powered(boosted.bars);
            if powered != pad.powered || !pad.primed {
                if powered && pad.primed {
                    // A cleared jammer or hotspot brought this pad online.
                    spawn_burst(&mut commands, &fx, pad.pos + Vec3::Y * 0.4, BurstColor::Green, 10, 5.0, 0.07);
                }
                pad.powered = powered;
                pad.primed = true;
                for c in children.iter() {
                    if let Ok(mut m) = glows.get_mut(c) {
                        m.0 = if powered { assets.pad_powered.clone() } else { assets.pad_dormant.clone() };
                    }
                    if let Ok(mut l) = lights.get_mut(c) {
                        l.color = if powered { PAD_POWERED_LIGHT } else { PAD_DORMANT_LIGHT };
                    }
                }
            }
        }
        let Some((p, ptf)) = player.as_mut() else { continue };
        let prev = p.prev_pos.unwrap_or(ptf.translation);
        if p.is_dead() || pad.cooldown > 0.0 || !crossed_jump_pad(prev, ptf.translation, pad.pos) {
            continue;
        }
        if !pad.powered {
            if *toast_timer <= 0.0 {
                *toast_timer = JUMP_PAD_TOAST_SECS;
                feed.toast("JUMP PAD OFFLINE - PAD NEEDS 2+ BARS", PAD_DORMANT_LIGHT);
            }
            continue;
        }
        apply_jump_pad(p, pad.launch_vel);
        p.stride = 0.0;
        pad.cooldown = JUMP_PAD_COOLDOWN;
        // Launching off a powered pad replaces any same-frame ground footstep / landing thud.
        sfx.0.retain(|r| !matches!(r.event, SfxEvent::StepRegolith | SfxEvent::StepMetal | SfxEvent::LandThud));
        sfx.play_at(SfxEvent::JumpPad, pad.pos);
        let base = pad.pos + Vec3::Y * 0.25;
        spawn_shockwave(&mut commands, &fx, &mut materials, base, 2.4, 0.3, JUMP_PAD_GLOW.0, JUMP_PAD_GLOW.1, true);
        spawn_particles(&mut commands, &fx, base, BurstColor::Cyan, 8, 7.0, 0.06);
        spawn_burst(&mut commands, &fx, base + Vec3::Y * 0.5, BurstColor::Green, 6, 4.0, 0.06);
        spawn_flash_light(&mut commands, base + Vec3::Y, PAD_POWERED_LIGHT, 300_000.0, 8.0, 0.15);
    }
}

pub fn hazard_damage(signal: Res<SignalState>, mods: Res<PlayerModifiers>, mut players: Query<&mut Player>) {
    let Ok(mut p) = players.single_mut() else { return };
    if p.hazard_timer < 0.0 {
        let scale = if signal.in_dead_zone { mods.dead_zone_damage_mult } else { 1.0 };
        p.apply_damage(HAZARD_DAMAGE * scale);
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
    fn browser_unlock_pauses_only_a_held_lock_while_playing() {
        assert!(browser_unlock_should_pause(true, false, true));
        assert!(!browser_unlock_should_pause(false, false, true), "never-granted lock must not pause");
        assert!(!browser_unlock_should_pause(true, true, true));
        assert!(!browser_unlock_should_pause(true, false, false), "our own OnExit release");
        let lock = |locked| Some(BrowserPointerLock { locked });
        assert!(mouse_look_active(CursorCaptured(true), None), "native ignores the browser lock");
        assert!(mouse_look_active(CursorCaptured(true), lock(true).as_ref()));
        assert!(!mouse_look_active(CursorCaptured(true), lock(false).as_ref()));
        assert!(!mouse_look_active(CursorCaptured(false), lock(true).as_ref()));
    }

    #[test]
    fn sync_browser_pointer_lock_pauses_and_respects_queued_transitions() {
        use bevy::ecs::system::RunSystemOnce;
        let world_in = |state: AppState, locked: bool| {
            let mut w = World::new();
            w.insert_resource(State::new(state));
            w.init_resource::<NextState<AppState>>();
            w.insert_resource(BrowserPointerLock { locked });
            w
        };
        let pending = |w: &World| match w.resource::<NextState<AppState>>() {
            NextState::Pending(s) | NextState::PendingIfDifferent(s) => Some(*s),
            NextState::Unchanged => None,
        };

        // Lock granted (pending request resolves): tracked, no pause.
        let mut w = world_in(AppState::Playing, false);
        w.run_system_once_with(sync_browser_pointer_lock, true).unwrap();
        assert!(w.resource::<BrowserPointerLock>().locked && pending(&w).is_none());

        // Browser drops the held lock (Esc / focus loss): pause.
        w.run_system_once_with(sync_browser_pointer_lock, false).unwrap();
        assert_eq!(pending(&w), Some(AppState::Paused));
        assert!(!w.resource::<BrowserPointerLock>().locked);

        // A transition already queued this frame wins.
        let mut w = world_in(AppState::Playing, true);
        w.resource_mut::<NextState<AppState>>().set(AppState::UpgradeSelect);
        w.run_system_once_with(sync_browser_pointer_lock, false).unwrap();
        assert_eq!(pending(&w), Some(AppState::UpgradeSelect));

        // Outside `Playing` (our own release on exit) only tracks the state.
        let mut w = world_in(AppState::Paused, true);
        w.run_system_once_with(sync_browser_pointer_lock, false).unwrap();
        assert!(pending(&w).is_none() && !w.resource::<BrowserPointerLock>().locked);
    }

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
    fn starter_chip_sets_spawn_vitals() {
        let stock = new_run_player(StarterChip::StockUac, 0.5);
        assert_eq!((stock.hp, stock.max_hp, stock.armor, stock.yaw), (100.0, 100.0, 25.0, 0.5));
        let survey = new_run_player(StarterChip::SurveyOs, 0.0);
        assert_eq!((survey.hp, survey.max_hp), (85.0, 85.0));
        let faraday = new_run_player(StarterChip::FaradayCase, 0.0);
        assert_eq!((faraday.hp, faraday.armor), (100.0, 50.0));
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

    // --- Quake / CPMA movement --------------------------------------------

    const DT: f32 = 1.0 / 60.0;

    fn xz(v: Vec3) -> Vec2 {
        Vec2::new(v.x, v.z)
    }

    /// Wish direction (XZ) for local keys at `yaw`.
    fn wish_xz(yaw: f32, input: MoveInput) -> Vec2 {
        xz(wish_direction(yaw, input.forward, input.right))
    }

    #[test]
    fn pure_air_strafe_with_mouse_sweep_builds_speed() {
        let d = MoveInput { forward: 0.0, right: 1.0 };
        assert!(d.pure_strafe() && !d.air_control());
        // Airborne at run speed facing -Z, hold D and sweep the mouse right at
        // a human ~145 deg/s for 2.5 s (about three and a half hops of air).
        let mut yaw = 0.0f32;
        let mut v = Vec2::new(0.0, -MOVE_SPEED);
        let mut prev = v.length();
        for i in 0..150 {
            yaw -= 2.5 * DT;
            v = step_horizontal(v, wish_xz(yaw, d), d, false, MOVE_SPEED, DT);
            if i > 5 {
                assert!(v.length() >= prev - 1e-4, "strafe lost speed at tick {i}");
            }
            prev = v.length();
        }
        assert!(v.length() > 16.0, "air-strafe only reached {} m/s", v.length());
        // The trajectory curved with the mouse: velocity now points well right of -Z.
        assert!(v.x > 0.0);

        // Holding D without turning only drifts sideways: no free speed.
        let mut v = Vec2::new(0.0, -MOVE_SPEED);
        for _ in 0..150 {
            v = step_horizontal(v, wish_xz(0.0, d), d, false, MOVE_SPEED, DT);
        }
        assert!(v.length() < MOVE_SPEED + 0.5, "{}", v.length());
    }

    #[test]
    fn diagonal_strafe_jumping_also_gains_speed() {
        // W+D with the mouse turning right (VQ3 strafe-jump).
        let wd = MoveInput { forward: 1.0, right: 1.0 };
        assert!(!wd.pure_strafe() && !wd.air_control());
        let mut yaw = 0.0f32;
        let mut v = Vec2::new(0.0, -MOVE_SPEED);
        for _ in 0..150 {
            yaw -= 1.2 * DT;
            v = step_horizontal(v, wish_xz(yaw, wd), wd, false, MOVE_SPEED, DT);
        }
        assert!(v.length() > MOVE_SPEED + 1.0, "diagonal strafe reached {}", v.length());
    }

    #[test]
    fn cpma_air_control_redirects_heading_and_keeps_speed() {
        let w = MoveInput { forward: 1.0, right: 0.0 };
        assert!(w.air_control());
        // Flying north at 15 m/s, the view turns 40 deg right; hold only W.
        let yaw = -40f32.to_radians();
        let wish = wish_xz(yaw, w);
        let mut v = Vec2::new(0.0, -15.0);
        for _ in 0..30 {
            v = step_horizontal(v, wish, w, false, MOVE_SPEED, DT);
        }
        assert!((v.length() - 15.0).abs() < 1e-3, "speed changed to {}", v.length());
        let angle = v.normalize().angle_to(wish).abs().to_degrees();
        assert!(angle < 4.0, "still {angle} deg off after 0.5 s");
        // Smooth, not a snap: one tick turns only a few degrees.
        let one = apply_air_control(Vec2::new(0.0, -15.0), wish, DT);
        let turned = one.normalize().angle_to(Vec2::NEG_Y).abs().to_degrees();
        assert!(turned > 0.5 && turned < 8.0, "turned {turned} deg in one tick");
        // Never steers backwards.
        assert_eq!(apply_air_control(Vec2::new(0.0, -15.0), Vec2::Y, DT), Vec2::new(0.0, -15.0));
    }

    #[test]
    fn ground_friction_is_crisp_and_ground_accel_snappy() {
        // Idle: from run speed to a dead stop in under 0.3 s, never reversing.
        let mut v = Vec2::new(MOVE_SPEED, 0.0);
        for _ in 0..18 {
            v = step_horizontal(v, Vec2::ZERO, MoveInput::default(), true, MOVE_SPEED, DT);
            assert!(v.x >= 0.0);
        }
        assert_eq!(v, Vec2::ZERO, "ice-skating: {v:?}");
        assert_eq!(apply_ground_friction(Vec2::ZERO, DT), Vec2::ZERO);
        // Run: from rest to ~full speed in 0.25 s, settling exactly at run speed.
        let w = MoveInput { forward: 1.0, right: 0.0 };
        let mut v = Vec2::ZERO;
        for _ in 0..15 {
            v = step_horizontal(v, Vec2::NEG_Y, w, true, MOVE_SPEED, DT);
        }
        assert!(v.length() > MOVE_SPEED * 0.95, "{}", v.length());
        for _ in 0..60 {
            v = step_horizontal(v, Vec2::NEG_Y, w, true, MOVE_SPEED, DT);
        }
        assert!((v.length() - MOVE_SPEED).abs() < 1e-3, "{}", v.length());
        // Quake accel never pushes past the wish speed along the wish dir.
        assert_eq!(accelerate(Vec2::new(0.0, -20.0), Vec2::NEG_Y, MOVE_SPEED, GROUND_ACCEL, DT), Vec2::new(0.0, -20.0));
    }

    #[test]
    fn soft_air_drag_settles_high_speed_and_respects_ceilings() {
        // Free below 22 m/s.
        assert_eq!(apply_air_drag(Vec2::new(21.9, 0.0), MOVE_SPEED, DT), Vec2::new(21.9, 0.0));
        // Gentle above: 30 m/s keeps > 28 m/s after a full second, settles toward 22.
        let mut v = Vec2::new(30.0, 0.0);
        let mut prev = 30.0;
        for i in 0..240 {
            v = apply_air_drag(v, MOVE_SPEED, DT);
            assert!(v.length() <= prev && v.length() >= AIR_DRAG_START);
            prev = v.length();
            if i == 59 {
                assert!(v.length() > 28.0, "too draggy: {}", v.length());
            }
        }
        assert!(v.length() < 27.0, "{}", v.length());
        // The absolute physics-safety ceiling is the only immediate clamp.
        assert!(apply_air_drag(Vec2::new(0.0, 90.0), MOVE_SPEED, DT).length() <= AIR_SPEED_HARD_CAP + 1e-3);
        // 5G Overclock (+25%) scales the drag start and the ceiling up...
        let oc = MOVE_SPEED * 1.25;
        assert_eq!(apply_air_drag(Vec2::new(27.0, 0.0), oc, DT), Vec2::new(27.0, 0.0));
        assert!((air_speed_cap(oc) - 45.0).abs() < 1e-3);
        assert!(apply_air_drag(Vec2::new(0.0, 90.0), oc, DT).length() > MAX_AIR_SPEED + 8.0);
        // ...but never past the absolute physics-safety ceiling.
        assert_eq!(air_speed_cap(MOVE_SPEED * 2.0), AIR_SPEED_HARD_CAP);
        assert!(apply_air_drag(Vec2::new(0.0, 90.0), MOVE_SPEED * 2.0, DT).length() <= AIR_SPEED_HARD_CAP + 1e-3);
    }

    #[test]
    fn overclock_drop_bleeds_speed_smoothly_onto_the_lower_ceiling() {
        // Overclocked hop at its 45 m/s ceiling, then the signal drops a bar
        // (ceiling back to 36 m/s): no one-frame snap, a smooth bleed instead.
        let mut v = Vec2::new(0.0, -45.0);
        let mut prev = 45.0;
        let mut settled_at = None;
        for i in 0..120 {
            v = apply_air_drag(v, MOVE_SPEED, DT);
            let s = v.length();
            assert!(s <= prev + 1e-5, "tick {i}: {s}");
            assert!(prev - s < 1.0, "snapped {prev} -> {s} in one tick");
            if settled_at.is_none() && s <= MAX_AIR_SPEED + 1e-3 {
                settled_at = Some(i);
            }
            prev = s;
        }
        let settled_at = settled_at.expect("never settled on the ceiling");
        assert!((10..=45).contains(&settled_at), "settled after {settled_at} ticks");
        // Gentle minimum bleed just above the ceiling.
        let just_over = apply_air_drag(Vec2::new(MAX_AIR_SPEED + 0.5, 0.0), MOVE_SPEED, DT).x;
        assert!(just_over < MAX_AIR_SPEED + 0.5 - OVER_CAP_MIN_BLEED * DT + 1e-4);
        // Air-strafing at / above the ceiling bends the path but adds no speed.
        let d = MoveInput { forward: 0.0, right: 1.0 };
        let fast = Vec2::new(0.0, -40.0);
        let bent = air_accelerate(fast, Vec2::X, d.pure_strafe(), MOVE_SPEED, DT);
        assert!(bent.length() <= fast.length() + 1e-4 && bent.x > 0.0, "{bent:?}");
        // Below the ceiling it still gains.
        let slow = Vec2::new(0.0, -15.0);
        assert!(air_accelerate(slow, Vec2::X, true, MOVE_SPEED, DT).length() > slow.length());
    }

    #[test]
    fn dash_never_slows_a_faster_bhop() {
        assert_eq!(dash_velocity(Vec2::new(0.0, -5.0), Vec2::NEG_Y), Vec2::new(0.0, -DASH_SPEED));
        assert_eq!(dash_velocity(Vec2::new(0.0, -40.0), Vec2::NEG_Y), Vec2::new(0.0, -40.0));
        // A 90 deg side dash redirects the full bhop speed around the corner.
        assert_eq!(dash_velocity(Vec2::new(40.0, 0.0), Vec2::NEG_Y), Vec2::new(0.0, -40.0));
        assert_eq!(dash_velocity(Vec2::new(-33.0, 0.0), Vec2::Y), Vec2::new(0.0, 33.0));
        // Never past the physics-safety ceiling.
        assert_eq!(dash_velocity(Vec2::new(90.0, 0.0), Vec2::NEG_Y), Vec2::new(0.0, -AIR_SPEED_HARD_CAP));
    }

    #[test]
    fn live_eye_pose_and_exponential_smoothing() {
        let body = Transform::from_xyz(10.0, 2.4, -3.0).with_rotation(Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2));
        let cam = Transform::from_xyz(0.0, PLAYER_EYE_HEIGHT, 0.0).with_rotation(camera_rotation(0.2, 0.0));
        let eye = live_eye_pose(&body, &cam);
        assert!((eye.translation - Vec3::new(10.0, 2.4 + PLAYER_EYE_HEIGHT, -3.0)).length() < 1e-5);
        // Yaw -90 deg faces +X; pitch tilts it up.
        let fwd = eye.rotation * Vec3::NEG_Z;
        assert!(fwd.x > 0.9 && fwd.y > 0.15 && fwd.z.abs() < 1e-4, "{fwd:?}");
        // Smoothing composes across frame rates: two half steps == one full step.
        let a = smooth_factor(10.0, 1.0 / 30.0);
        let half = smooth_factor(10.0, 1.0 / 60.0);
        assert!((a - (1.0 - (1.0 - half) * (1.0 - half))).abs() < 1e-6);
        assert!(smooth_factor(10.0, 10.0) <= 1.0 && smooth_factor(10.0, 0.0) == 0.0);
    }

    #[test]
    fn fov_bonus_blends_speed_and_dash() {
        assert_eq!(fov_bonus_deg(0.0, false), 0.0);
        assert_eq!(fov_bonus_deg(MOVE_SPEED, false), 0.0);
        assert!((fov_bonus_deg(SPEED_FOV_FULL, false) - SPEED_FOV_MAX_DEG).abs() < 1e-4);
        assert!(fov_bonus_deg(20.0, false) > 4.0 && fov_bonus_deg(20.0, false) < 8.0);
        assert_eq!(fov_bonus_deg(0.0, true), DASH_FOV_DEG);
        assert!((fov_bonus_deg(SPEED_FOV_FULL, true) - (SPEED_FOV_MAX_DEG + 0.25 * DASH_FOV_DEG)).abs() < 1e-4);
        assert!(fov_bonus_deg(1000.0, true) <= MAX_FOV_BONUS_DEG);
    }

    #[test]
    fn user_settings_scale_look_fov_and_roll() {
        let d = UserSettings::default();
        assert_eq!(effective_mouse_sensitivity(&d), MOUSE_SENSITIVITY);
        assert!((base_fov_rad(&d) - BASE_FOV_DEG.to_radians()).abs() < 1e-6);
        assert_eq!(effective_fov_bonus_deg(25.0, true, &d), fov_bonus_deg(25.0, true));
        assert_eq!(effective_strafe_tilt_target(1.0, 0.5, false, &d), strafe_tilt_target(1.0, 0.5, false));

        let s = UserSettings { mouse_sensitivity: 2.0, base_fov_deg: 100.0, speed_fov_scale: 0.5, strafe_roll_scale: 1.5, ..d.clone() };
        assert!((effective_mouse_sensitivity(&s) - 2.0 * MOUSE_SENSITIVITY).abs() < 1e-9);
        assert!((base_fov_rad(&s) - 100f32.to_radians()).abs() < 1e-6);
        assert!((effective_fov_bonus_deg(SPEED_FOV_FULL, false, &s) - 0.5 * SPEED_FOV_MAX_DEG).abs() < 1e-4);
        assert!((effective_strafe_tilt_target(1.0, 0.0, false, &s) - 1.5 * strafe_tilt_target(1.0, 0.0, false)).abs() < 1e-6);

        let off = UserSettings { speed_fov_enabled: false, strafe_roll_enabled: false, ..d.clone() };
        assert_eq!(effective_fov_bonus_deg(40.0, true, &off), 0.0);
        assert_eq!(effective_strafe_tilt_target(1.0, 1.0, false, &off), 0.0);

        // Out-of-range / NaN values never leak into the camera.
        let wild = UserSettings { mouse_sensitivity: f32::NAN, base_fov_deg: 400.0, speed_fov_scale: f32::INFINITY, ..d };
        assert_eq!(effective_mouse_sensitivity(&wild), MOUSE_SENSITIVITY);
        assert!((base_fov_rad(&wild) - 110f32.to_radians()).abs() < 1e-6);
        assert_eq!(effective_fov_bonus_deg(SPEED_FOV_FULL, false, &wild), SPEED_FOV_MAX_DEG);
    }

    #[test]
    fn strafe_tilt_leans_into_the_strafe_and_stays_subtle() {
        assert!(strafe_tilt_target(1.0, 0.0, false) < 0.0, "D leans right (negative roll)");
        assert!(strafe_tilt_target(-1.0, 0.0, false) > 0.0);
        assert_eq!(strafe_tilt_target(0.0, 0.0, true), 0.0);
        assert!((strafe_tilt_target(5.0, 5.0, false).abs() - MAX_STRAFE_TILT).abs() < 1e-6);
        assert!(strafe_tilt_target(1.0, 1.0, true).abs() < strafe_tilt_target(1.0, 1.0, false).abs());
        // Pitch survives the roll: the view axis still points where the mouse aims.
        let fwd = camera_rotation(0.3, MAX_STRAFE_TILT) * Vec3::NEG_Z;
        assert!((fwd - Quat::from_rotation_x(0.3) * Vec3::NEG_Z).length() < 1e-5);
    }

    // --- `move_player` harness (real system, manual clock + keys) ----------

    fn harness(rows: &[&str], feet: Vec3, vel: Vec3) -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<PlayerModifiers>()
            .init_resource::<SignalState>()
            .init_resource::<SfxQueue>()
            .insert_resource(LevelGrid::from_ascii(rows))
            .add_systems(Update, move_player);
        app.world_mut().spawn((Player { velocity: vel, grounded: feet.y.abs() < 1e-3, ..default() }, Transform::from_translation(feet)));
        app
    }

    /// One 60 Hz tick; `just_pressed` lasts exactly one tick like real input.
    fn tick(app: &mut App) -> Player {
        tick_dt(app, DT)
    }

    fn tick_dt(app: &mut App, dt: f32) -> Player {
        app.world_mut().resource_mut::<Time>().advance_by(std::time::Duration::from_secs_f32(dt));
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        hp(app)
    }

    fn hp(app: &mut App) -> Player {
        app.world_mut().query::<&Player>().single(app.world()).unwrap().clone()
    }

    fn keys(app: &mut App) -> Mut<'_, ButtonInput<KeyCode>> {
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>()
    }

    fn sfx(app: &mut App) -> Vec<SfxEvent> {
        std::mem::take(&mut app.world_mut().resource_mut::<SfxQueue>().0).into_iter().map(|r| r.event).collect()
    }

    /// 88 m east-west corridor.
    const CORRIDOR: [&str; 3] = ["######################", "#....................#", "######################"];

    #[test]
    fn hold_space_auto_hop_keeps_full_speed_across_landings() {
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.0, 6.0), Vec3::new(20.0, 0.0, 0.0));
        keys(&mut app).press(KeyCode::Space);
        let mut hops = 0;
        let mut sounds = Vec::new();
        for _ in 0..150 {
            // Landing and re-hopping can share one frame (substeps), so count
            // hops as vertical velocity turning upwards.
            let prev_vy = hp(&mut app).velocity.y;
            let p = tick(&mut app);
            if prev_vy <= 0.0 && p.velocity.y > 0.0 {
                hops += 1;
            }
            sounds.extend(sfx(&mut app));
            assert!(xz(p.velocity).length() > 20.0 - 1e-3, "landing friction ate speed: {}", xz(p.velocity).length());
        }
        assert!(hops >= 3, "only {hops} hops");
        let p = hp(&mut app);
        assert!(p.bhop_chain >= 3, "chain {}", p.bhop_chain);
        // First hop (chain x1) is a plain jump; from `BHOP x2` on, the chirp.
        let hops_sfx: Vec<_> = sounds.iter().filter(|s| matches!(s, SfxEvent::Jump | SfxEvent::Bhop)).collect();
        assert_eq!(hops_sfx.first(), Some(&&SfxEvent::Jump), "{sounds:?}");
        assert!(hops_sfx[1..].iter().all(|s| **s == SfxEvent::Bhop) && hops_sfx.len() >= 3, "{sounds:?}");
        // A clean bhop chain is silent underfoot: no footsteps, no landing thuds.
        assert!(!sounds.iter().any(|s| matches!(s, SfxEvent::StepRegolith | SfxEvent::StepMetal | SfxEvent::LandThud)), "{sounds:?}");

        // Letting go: the player lands, friction bites and the chain breaks.
        keys(&mut app).release(KeyCode::Space);
        for _ in 0..90 {
            tick(&mut app);
        }
        let p = hp(&mut app);
        assert!(p.grounded && xz(p.velocity).length() < 1.0 && p.bhop_chain == 0, "{p:?}");
    }

    fn feet(app: &mut App) -> Vec3 {
        app.world_mut().query_filtered::<&Transform, With<Player>>().single(app.world()).unwrap().translation
    }

    #[test]
    fn running_steps_once_per_stride_on_regolith() {
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.0, 6.0), Vec3::ZERO);
        tick(&mut app);
        let x0 = feet(&mut app).x;
        keys(&mut app).press(KeyCode::KeyD);
        let mut sounds = Vec::new();
        for _ in 0..120 {
            tick(&mut app);
            sounds.extend(sfx(&mut app));
        }
        let run = feet(&mut app).x - x0;
        assert!(run > 8.0, "ran {run} m");
        let steps = sounds.iter().filter(|s| **s == SfxEvent::StepRegolith).count();
        let expected = (run / STEP_STRIDE) as usize;
        assert!(steps + 1 >= expected && steps <= expected + 1, "{steps} steps over {run} m");
        assert!(!sounds.iter().any(|s| matches!(s, SfxEvent::StepMetal | SfxEvent::LandThud)), "{sounds:?}");
        // Standing still: silence.
        keys(&mut app).release(KeyCode::KeyD);
        for _ in 0..60 {
            tick(&mut app);
        }
        sfx(&mut app);
        for _ in 0..60 {
            tick(&mut app);
        }
        assert!(sfx(&mut app).is_empty());
    }

    #[test]
    fn hard_landing_thuds_once_and_soft_drop_is_silent() {
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 4.0, 6.0), Vec3::ZERO);
        let mut sounds = Vec::new();
        for _ in 0..90 {
            tick(&mut app);
            sounds.extend(sfx(&mut app));
        }
        assert!(hp(&mut app).grounded);
        assert_eq!(sounds.iter().filter(|s| **s == SfxEvent::LandThud).count(), 1, "{sounds:?}");
        assert!(!sounds.iter().any(|s| matches!(s, SfxEvent::StepRegolith | SfxEvent::StepMetal)), "falling is not walking: {sounds:?}");

        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.15, 6.0), Vec3::ZERO);
        let mut sounds = Vec::new();
        for _ in 0..30 {
            tick(&mut app);
            sounds.extend(sfx(&mut app));
        }
        assert!(hp(&mut app).grounded && sounds.is_empty(), "{sounds:?}");
    }

    /// Hop once at 20 m/s and fall until about `ticks_before_land` ticks remain, then tap Space.
    fn buffered_landing(ticks_before_land: u32) -> Player {
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.0, 6.0), Vec3::new(20.0, 0.0, 0.0));
        keys(&mut app).press(KeyCode::Space);
        tick(&mut app);
        keys(&mut app).release(KeyCode::Space);
        // Airtime is 2 * 8 / 22 s ~= 43.6 ticks.
        let air_ticks = (2.0 * JUMP_VELOCITY / crate::physics::GRAVITY / DT) as u32;
        for _ in 0..air_ticks - ticks_before_land {
            tick(&mut app);
        }
        assert!(!hp(&mut app).grounded);
        keys(&mut app).press(KeyCode::Space);
        tick(&mut app);
        keys(&mut app).release(KeyCode::Space);
        // Run through the landing; return the state 0.15 s after touchdown.
        for _ in 0..(ticks_before_land + 9) {
            tick(&mut app);
        }
        hp(&mut app)
    }

    #[test]
    fn jump_buffer_hops_on_touchdown_without_friction() {
        // Tapped ~0.08 s before landing: hops straight off the floor at full speed.
        let p = buffered_landing(5);
        assert!(!p.grounded && p.velocity.y > 0.0, "buffered hop did not fire: {p:?}");
        assert!(xz(p.velocity).length() > 20.0 - 1e-3);
        // Tapped ~0.25 s early: outside the 0.12 s buffer, so the player lands
        // and ground friction bleeds speed.
        let p = buffered_landing(15);
        assert!(p.grounded, "{p:?}");
        assert!(xz(p.velocity).length() < 18.0, "{}", xz(p.velocity).length());
    }

    /// Run east off a 2.4 m catwalk edge; tap Space `ticks_after` ticks after leaving it.
    fn coyote_jump(ticks_after: u32) -> (Player, Player) {
        let rows = ["##########", "#^^^.....#", "##########"];
        let mut app = harness(&rows, Vec3::new(9.0, crate::level::CATWALK_HEIGHT, 6.0), Vec3::new(8.0, 0.0, 0.0));
        with(&mut app, |p| p.grounded = true);
        // Facing -Z (yaw 0), D runs east toward the edge.
        keys(&mut app).press(KeyCode::KeyD);
        let mut guard = 0;
        while hp(&mut app).grounded {
            tick(&mut app);
            guard += 1;
            assert!(guard < 120, "never left the catwalk");
        }
        for _ in 0..ticks_after {
            tick(&mut app);
        }
        let before = hp(&mut app);
        keys(&mut app).press(KeyCode::Space);
        let after = tick(&mut app);
        (before, after)
    }

    fn with(app: &mut App, f: impl FnOnce(&mut Player)) {
        let mut q = app.world_mut().query::<&mut Player>();
        f(&mut q.single_mut(app.world_mut()).unwrap());
    }

    #[test]
    fn coyote_time_allows_a_late_jump_but_never_a_double_jump() {
        // 4 ticks (~0.067 s) after running off the edge: still jumps.
        let (before, after) = coyote_jump(4);
        assert!(before.velocity.y < 0.0 && after.velocity.y > JUMP_VELOCITY - 1.0, "coyote jump refused: {after:?}");
        // 8 ticks (~0.13 s): too late.
        let (before, after) = coyote_jump(8);
        assert!(after.velocity.y < before.velocity.y, "jumped after coyote time: {after:?}");

        // Holding Space after a jump never re-jumps mid-air.
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.0, 6.0), Vec3::ZERO);
        keys(&mut app).press(KeyCode::Space);
        let mut last_vy = tick(&mut app).velocity.y;
        assert!(last_vy > 0.0);
        for _ in 0..30 {
            let p = tick(&mut app);
            assert!(p.velocity.y < last_vy, "double jump at vy {}", p.velocity.y);
            assert_eq!(p.coyote_timer, 0.0);
            last_vy = p.velocity.y;
        }
    }

    #[test]
    fn dash_into_jump_carries_dash_speed_into_the_air() {
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.0, 6.0), Vec3::ZERO);
        with(&mut app, |p| {
            p.yaw = -std::f32::consts::FRAC_PI_2; // face +X
            p.prev_yaw = p.yaw;
        });
        keys(&mut app).press(KeyCode::ShiftLeft);
        keys(&mut app).press(KeyCode::Space);
        let p = tick(&mut app);
        assert!(p.dash_started && p.dash_timer == 0.0, "dash not cancelled into the hop: {p:?}");
        assert!(p.velocity.y > 0.0 && (xz(p.velocity).length() - DASH_SPEED).abs() < 0.1, "{p:?}");
        assert!(p.bhop_chain >= 1);
        keys(&mut app).release(KeyCode::ShiftLeft);
        keys(&mut app).release(KeyCode::Space);
        for _ in 0..20 {
            tick(&mut app);
        }
        let p = hp(&mut app);
        assert!(!p.grounded && xz(p.velocity).length() > 28.5, "dash speed bled: {}", xz(p.velocity).length());
    }

    #[test]
    fn dash_ending_mid_air_keeps_its_speed() {
        let mut app = harness(&CORRIDOR, Vec3::new(6.0, 0.0, 6.0), Vec3::ZERO);
        with(&mut app, |p| {
            p.yaw = -std::f32::consts::FRAC_PI_2;
            p.prev_yaw = p.yaw;
        });
        keys(&mut app).press(KeyCode::Space);
        tick(&mut app);
        keys(&mut app).release(KeyCode::Space);
        tick(&mut app);
        keys(&mut app).press(KeyCode::ShiftLeft);
        let p = tick(&mut app);
        assert!(p.dash_timer > 0.0 && !p.grounded);
        for _ in 0..12 {
            tick(&mut app);
        }
        let p = hp(&mut app);
        assert!(p.dash_timer <= 0.0 && !p.grounded, "{p:?}");
        assert!(xz(p.velocity).length() > 29.0, "dash end clamped speed to {}", xz(p.velocity).length());
    }

    /// Hold Space + D in a 64 m arena and sweep the view right at 2.5 rad/s
    /// for 4 s, sampled at `hz`. Returns the final horizontal speed.
    fn strafe_replay(hz: f32) -> f32 {
        let wall = "#".repeat(18);
        let row = format!("#{}#", ".".repeat(16));
        let rows: Vec<&str> = std::iter::once(wall.as_str()).chain(std::iter::repeat_n(row.as_str(), 16)).chain(std::iter::once(wall.as_str())).collect();
        let mut app = harness(&rows, Vec3::new(36.0, 0.0, 36.0), Vec3::new(0.0, 0.0, -MOVE_SPEED));
        keys(&mut app).press(KeyCode::Space);
        keys(&mut app).press(KeyCode::KeyD);
        let frames = (4.0 * hz).round() as u32;
        let dt = 1.0 / hz;
        for f in 1..=frames {
            // `mouse_look` sets the yaw for the end of the frame.
            let t = f as f32 * dt;
            with(&mut app, |p| p.yaw = -2.5 * t);
            let p = tick_dt(&mut app, dt);
            // Held keys: `clear()` keeps them pressed; re-press to be explicit.
            keys(&mut app).press(KeyCode::Space);
            keys(&mut app).press(KeyCode::KeyD);
            assert!(p.velocity.is_finite());
        }
        xz(hp(&mut app).velocity).length()
    }

    #[test]
    fn movement_converges_across_frame_rates() {
        let speeds: Vec<f32> = [30.0, 60.0, 120.0, 144.0].into_iter().map(strafe_replay).collect();
        let (lo, hi) = speeds.iter().fold((f32::MAX, f32::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)));
        println!("30/60/120/144 Hz air-strafe speeds: {speeds:?}");
        assert!(lo > MOVE_SPEED + 3.0, "scenario never built speed: {speeds:?}");
        assert!(hi - lo < 1.0, "frame rate changes the outcome: {speeds:?}");
    }

    #[test]
    fn fast_touchdown_on_a_catwalk_edge_still_hops() {
        // Dropping onto the catwalk 0.15 m past its edge at 30 m/s: the feet
        // touch down and leave again inside one 1/120 s substep. Holding Space,
        // the touchdown keeps the coyote window and the hop fires.
        let rows = ["###########", "#^^^......#", "###########"];
        let feet = Vec3::new(16.15, crate::level::CATWALK_HEIGHT + 0.01, 6.0);
        let mut app = harness(&rows, feet, Vec3::new(30.0, -3.0, 0.0));
        keys(&mut app).press(KeyCode::Space);
        let p = tick(&mut app);
        assert!(p.velocity.y > JUMP_VELOCITY - 1.0, "touchdown at the edge lost the hop: {p:?}");
        assert!(xz(p.velocity).length() > 29.9, "{p:?}");
    }

    #[test]
    fn floor_transition_resets_camera_feel_and_motion() {
        let mut app = App::new();
        app.insert_resource(LevelGrid::from_ascii(&CORRIDOR)).add_systems(Update, snap_player_to_spawn);
        app.world_mut()
            .spawn((
                Player { velocity: Vec3::new(30.0, 4.0, 0.0), grounded: false, bob_phase: 3.0, prev_yaw: 2.0, bhop_chain: 4, ..default() },
                Transform::from_xyz(50.0, 3.0, 6.0),
            ))
            .with_children(|c| {
                c.spawn((
                    PlayerCamera,
                    Projection::Perspective(PerspectiveProjection { fov: 100f32.to_radians(), ..default() }),
                    Transform::from_xyz(0.03, PLAYER_EYE_HEIGHT + 0.06, 0.0).with_rotation(camera_rotation(0.4, 0.02)),
                ));
            });
        app.update();
        let p = hp(&mut app);
        assert!(p.grounded && p.velocity == Vec3::ZERO && p.bob_phase == 0.0 && p.bhop_chain == 0 && p.prev_yaw == p.yaw, "{p:?}");
        let mut q = app.world_mut().query_filtered::<(&Transform, &Projection), With<PlayerCamera>>();
        let (cam, proj) = q.single(app.world()).unwrap();
        assert_eq!(cam.translation, Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0));
        assert_eq!(cam.rotation, Quat::IDENTITY);
        let Projection::Perspective(pp) = proj else { panic!() };
        assert!((pp.fov - BASE_FOV_DEG.to_radians()).abs() < 1e-6);
    }

    #[test]
    fn jump_pad_power_gate_is_two_bars() {
        assert!(!jump_pad_powered(0));
        assert!(!jump_pad_powered(1));
        assert!(jump_pad_powered(2));
        assert!(jump_pad_powered(5));
    }

    #[test]
    fn jump_pad_trigger_volume() {
        let pad = Vec3::new(10.0, 0.0, 10.0);
        assert!(on_jump_pad(pad, pad));
        assert!(on_jump_pad(pad + Vec3::new(1.3, 0.0, 0.0), pad));
        assert!(!on_jump_pad(pad + Vec3::new(1.0, 0.0, 1.0), pad), "outside the 1.35 m radius");
        assert!(on_jump_pad(pad + Vec3::Y * 1.1, pad));
        assert!(!on_jump_pad(pad + Vec3::Y * 1.3, pad), "flying over it");
        assert!(!on_jump_pad(pad - Vec3::Y * 2.4, pad), "under it");
        // Swept pass: both endpoints outside 1.35 m radius at 1.2 m offset, midpoint crosses pad.
        let from = pad + Vec3::new(-0.9, 0.0, 1.2);
        let to = pad + Vec3::new(0.9, 0.0, 1.2);
        assert!(!on_jump_pad(from, pad) && !on_jump_pad(to, pad));
        assert!(crossed_jump_pad(from, to, pad), "fast grazing bhop still triggers pad");
        assert!(!crossed_jump_pad(pad + Vec3::new(-2.0, 0.0, 2.0), pad + Vec3::new(2.0, 0.0, 2.0), pad));
    }

    #[test]
    fn jump_pad_launch_from_rest_uses_pad_velocity_and_detaches() {
        let mut p = Player { grounded: true, coyote_timer: COYOTE_TIME, dash_timer: 0.1, jump_buffer_timer: 0.1, ..default() };
        let launch = Vec3::new(-5.0, 13.5, 0.0);
        apply_jump_pad(&mut p, launch);
        assert!((p.velocity - launch).length() < 1e-4, "{:?}", p.velocity);
        assert!(!p.grounded);
        assert_eq!((p.coyote_timer, p.dash_timer, p.jump_buffer_timer), (0.0, 0.0, 0.0));
    }

    #[test]
    fn jump_pad_never_slows_a_bhopper_and_respects_hard_cap() {
        let launch = Vec3::new(9.0, 10.0, 0.0);
        for v in [Vec2::new(30.0, 0.0), Vec2::new(-30.0, 0.0), Vec2::new(0.0, 30.0), Vec2::new(-6.0, 3.0), Vec2::new(46.0, 0.0)] {
            let mut p = Player { velocity: Vec3::new(v.x, -4.0, v.y), ..default() };
            apply_jump_pad(&mut p, launch);
            let h = Vec2::new(p.velocity.x, p.velocity.z).length();
            assert!(h + 1e-3 >= v.length().min(AIR_SPEED_HARD_CAP), "{v}: slowed to {h}");
            assert!(h + 1e-3 >= 9.0, "{v}: at least the pad's push ({h})");
            assert!(h <= AIR_SPEED_HARD_CAP + 1e-3, "{v}: hard cap ({h})");
            assert_eq!(p.velocity.y, 10.0);
        }
        // Already rising faster than the pad: vertical speed is kept.
        let mut p = Player { velocity: Vec3::new(0.0, 16.0, 0.0), ..default() };
        apply_jump_pad(&mut p, launch);
        assert_eq!(p.velocity.y, 16.0);
        // Aligned with the push: boost adds on top (capped).
        let mut p = Player { velocity: Vec3::new(20.0, 0.0, 0.0), ..default() };
        apply_jump_pad(&mut p, launch);
        assert!((p.velocity.x - 29.0).abs() < 1e-4);
    }

    #[test]
    fn theme_fog_follows_the_floor_theme() {
        for floor in 1..=10 {
            let spec = FloorTheme::for_floor(floor).spec();
            let fog = theme_fog(floor);
            assert_eq!(fog.color, spec.fog_color);
            assert_eq!(fog.directional_light_color, spec.fog_dir_color);
        }
        assert_ne!(theme_fog(1).color, theme_fog(2).color);
        assert_eq!(theme_fog(1).color, theme_fog(6).color);
    }
}
