//! Layer 2: live, rendered autonomous playtest bot (`SEGNALE_BOT_TEST=<dir>`).
//!
//! The bot plays a real run through [`BotControl`] virtual input only — real
//! player physics (ramps, wall sliding, dash), real weapons, Phone Apps, live
//! demon AI, SOS rush and the App Store:
//!
//! menu -> Floor 1 spawn -> BFS-navigate to the best Demon Jammer and shoot it
//! (Shotgun) -> Plasma + Ping Scan -> navigate to the Signal Tower -> trigger
//! the SOS uplink -> hold the ring vs. the Demon Rush (Shotgun / Plasma / Ping
//! / 5G Overload / Dash, Jammer Fiends first) -> orbital extraction -> install
//! an upgrade -> Floor 2 -> exit.
//!
//! It saves a 9-frame storyboard plus `playtest_report.json` (telemetry and
//! pass/fail) into the output directory and exits with code 0 on pass, 1 on
//! fail. Damage is real and fully tracked; only a *fatal* blow is converted
//! into 1 HP and counted as `would_have_died`, so balance problems stay visible
//! in the report instead of aborting the automated run.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::time::TimeUpdateStrategy;

use crate::apps::{emp_radius, PhoneApps, OVERLOAD_MIN_BARS, PING_MIN_BARS};
use crate::bot_input::BotControl;
use crate::enemies::{Enemy, EnemyKind, EnemyProjectile};
use crate::extraction::ExtractionState;
use crate::level::{yaw_facing, Cell, DemonJammer, LevelGrid, RampDir, CELL_SIZE};
use crate::physics::has_line_of_sight;
use crate::player::{check_death, hazard_damage, Player, PLAYER_EYE_HEIGHT};
use crate::signal::{SignalState, TOWER_RING_RADIUS};
use crate::state::{start_run, AppState, GameSet, PlayerModifiers, RunStats};
use crate::weapons::{Pickup, PickupKind, Weapon, WeaponState, SWAP_SECS};

/// Hard cap on simulated seconds for the whole playtest.
pub const BOT_TIMEOUT_SECS: f32 = 200.0;
/// Rolling window for stuck detection.
pub const STUCK_WINDOW_SECS: f32 = 2.0;
pub const STUCK_MIN_TRAVEL: f32 = 0.75;
const TURN_RATE: f32 = 14.0;
const STORYBOARD: [&str; 9] = [
    "01_menu.png",
    "02_spawn.png",
    "03_combat_shotgun.png",
    "04_ping_and_plasma.png",
    "05_sos_rush.png",
    "06_5g_overload.png",
    "07_extraction_beam.png",
    "08_upgrade_store.png",
    "09_floor2.png",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotPhase {
    Menu,
    Spawn,
    HuntJammer,
    GoToTower,
    Sos,
    Extracting,
    Store,
    Floor2,
    Finish,
}

/// Telemetry written to `playtest_report.json`.
#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    pub frames: u64,
    pub sim_seconds: f32,
    pub floor_reached: u32,
    pub kills: u32,
    pub jammers_destroyed: u32,
    pub shots_fired: u32,
    pub plasma_fired: u32,
    pub pings_cast: u32,
    pub overloads_cast: u32,
    pub dashes_used: u32,
    pub damage_taken: f32,
    pub near_deaths: u32,
    pub would_have_died: u32,
    /// Sim second of the first blow that would have killed the player.
    pub first_fatal_at: Option<f32>,
    pub signal_min_bars: Option<u8>,
    pub signal_max_bars: Option<u8>,
    pub sos_triggered: bool,
    pub sos_completed: bool,
    pub upgrade_picked: Option<String>,
    pub stuck_events: u32,
    pub phase_log: Vec<(String, f32)>,
}

#[derive(Resource)]
pub struct PlaytestBot {
    /// Screenshot/report directory (`None` = headless run: storyboard moments
    /// are recorded but nothing is captured or written).
    pub out_dir: Option<PathBuf>,
    /// Final `(passed, report_json)` once the run finished.
    pub report: Option<(bool, String)>,
    pub seed: u64,
    pub phase: BotPhase,
    phase_t: f32,
    started: Instant,
    pub tel: Telemetry,
    pub failures: Vec<String>,
    shots: Vec<String>,
    // Navigation.
    path: Vec<IVec2>,
    replan_t: f32,
    samples: VecDeque<(f32, Vec3)>,
    recover_t: f32,
    recover_dir: Vec3,
    recover_side: f32,
    // Combat / storyboard bookkeeping.
    weapon_pref: Weapon,
    ping_shot: Option<f32>,
    overload_shot: Option<f32>,
    plasma_shown: u32,
    finish_frames: u32,
    // Telemetry deltas.
    prev_vitals: Option<(f32, f32)>,
    prev_ammo: Option<(u32, u32)>,
    prev_cd: (f32, f32),
}

impl PlaytestBot {
    pub fn new(out_dir: Option<PathBuf>, seed: u64) -> Self {
        PlaytestBot {
            out_dir,
            report: None,
            seed,
            phase: BotPhase::Menu,
            phase_t: 0.0,
            started: Instant::now(),
            tel: Telemetry::default(),
            failures: Vec::new(),
            shots: Vec::new(),
            path: Vec::new(),
            replan_t: 0.0,
            samples: VecDeque::new(),
            recover_t: 0.0,
            recover_dir: Vec3::ZERO,
            recover_side: 1.0,
            weapon_pref: Weapon::Shotgun,
            ping_shot: None,
            overload_shot: None,
            plasma_shown: 0,
            finish_frames: 0,
            prev_vitals: None,
            prev_ammo: None,
            prev_cd: (0.0, 0.0),
        }
    }

    fn set_phase(&mut self, p: BotPhase) {
        if self.phase != p {
            info!("playtest bot: {:?} -> {:?} at {:.1}s", self.phase, p, self.tel.sim_seconds);
            self.tel.phase_log.push((format!("{p:?}"), self.tel.sim_seconds));
            self.phase = p;
            self.phase_t = 0.0;
            self.path.clear();
            self.samples.clear();
            self.recover_t = 0.0;
        }
    }

    fn has_shot(&self, name: &str) -> bool {
        self.shots.iter().any(|s| s == name)
    }

    fn shot(&mut self, commands: &mut Commands, name: &str) {
        if self.has_shot(name) {
            return;
        }
        if let Some(dir) = &self.out_dir {
            let path = dir.join(name);
            info!("playtest bot: screenshot -> {}", path.display());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        } else {
            info!("playtest bot: storyboard moment {name} (headless)");
        }
        self.shots.push(name.to_string());
    }

    fn fail(&mut self, why: impl Into<String>) {
        let why = why.into();
        warn!("playtest bot FAILURE: {why}");
        self.failures.push(why);
        self.set_phase(BotPhase::Finish);
    }
}

pub struct PlaytestBotPlugin {
    /// `None` = headless (integration tests): no screenshots / report file.
    pub out_dir: Option<PathBuf>,
    pub seed: u64,
    /// Fixed sim step per rendered frame (`None` = real time).
    pub fixed_dt: Option<f32>,
}

impl PlaytestBotPlugin {
    /// Configure from `SEGNALE_BOT_TEST`, `SEGNALE_BOT_SEED`, `SEGNALE_BOT_DT`.
    pub fn from_env(out_dir: String) -> Self {
        let seed = std::env::var("SEGNALE_BOT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1234);
        let dt = std::env::var("SEGNALE_BOT_DT").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0 / 30.0);
        PlaytestBotPlugin { out_dir: Some(PathBuf::from(out_dir)), seed, fixed_dt: (dt > 0.0).then_some(dt) }
    }
}

impl Plugin for PlaytestBotPlugin {
    fn build(&self, app: &mut App) {
        if let Some(dir) = &self.out_dir {
            if let Err(e) = std::fs::create_dir_all(dir) {
                panic!("playtest bot: cannot create {}: {e}", dir.display());
            }
            for name in STORYBOARD {
                let _ = std::fs::remove_file(dir.join(name));
            }
            let _ = std::fs::remove_file(dir.join("playtest_report.json"));
        }
        if let Some(dt) = self.fixed_dt {
            // Deterministic sim speed regardless of software-rendering frame rate.
            app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(dt)));
        }
        app.insert_resource(PlaytestBot::new(self.out_dir.clone(), self.seed))
            .init_resource::<BotControl>()
            .add_systems(Update, bot_menu.run_if(in_state(AppState::MainMenu)))
            .add_systems(Update, bot_brain.before(GameSet::Input).run_if(in_state(AppState::Playing)))
            .add_systems(Update, bot_telemetry.after(GameSet::Combat).before(GameSet::Ui).run_if(in_state(AppState::Playing)))
            .add_systems(Update, guard_fatal_blow.in_set(GameSet::World).after(hazard_damage).before(check_death))
            .add_systems(Update, bot_store.run_if(in_state(AppState::UpgradeSelect)))
            .add_systems(Last, bot_watchdog);
    }
}

// ---------------------------------------------------------------------------
// Navigation helpers (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Shortest walkable cell path `from -> goal` (excluding `from`), honouring
/// ramps, step heights and hazard pits exactly like the demon AI.
pub fn bfs_path(g: &LevelGrid, from: IVec2, goal: IVec2) -> Option<Vec<IVec2>> {
    if !g.in_bounds(from) || !g.in_bounds(goal) {
        return None;
    }
    if from == goal {
        return Some(Vec::new());
    }
    let idx = |c: IVec2| (c.y * g.width + c.x) as usize;
    let mut prev: Vec<Option<IVec2>> = vec![None; (g.width * g.depth) as usize];
    let mut seen = vec![false; prev.len()];
    seen[idx(from)] = true;
    let mut q = VecDeque::from([from]);
    while let Some(c) = q.pop_front() {
        for o in RampDir::ALL.map(RampDir::offset) {
            let n = c + o;
            if g.in_bounds(n) && !seen[idx(n)] && g.can_walk(c, n) {
                seen[idx(n)] = true;
                prev[idx(n)] = Some(c);
                if n == goal {
                    let mut path = vec![n];
                    let mut cur = c;
                    while cur != from {
                        path.push(cur);
                        cur = prev[idx(cur)].expect("bfs parent chain");
                    }
                    path.reverse();
                    return Some(path);
                }
                q.push_back(n);
            }
        }
    }
    None
}

/// Pedestal cell next to the tower mast (inside the Signal Ring), reachable from `from`.
pub fn tower_stand_cell(g: &LevelGrid, from: IVec2) -> Option<IVec2> {
    let d = g.walk_distances(from);
    [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .map(|o| g.tower + o)
        .filter(|c| matches!(g.cell(*c), Cell::Floor { .. }))
        .filter_map(|c| d[(c.y * g.width + c.x) as usize].map(|v| (v, c)))
        .min_by_key(|(v, _)| *v)
        .map(|(_, c)| c)
}

/// Best jammer to hunt: cheapest detour `spawn -> jammer -> tower`.
pub fn pick_jammer(g: &LevelGrid, from: IVec2, alive: &[IVec2]) -> Option<IVec2> {
    let ds = g.walk_distances(from);
    let dt = g.walk_distances(g.tower);
    let at = |d: &Vec<Option<u32>>, c: IVec2| -> Option<u32> {
        // Jammer stalks may sit on cells we only stand next to: use the best neighbour.
        std::iter::once(c)
            .chain(RampDir::ALL.map(|r| c + r.offset()))
            .filter(|n| g.in_bounds(*n))
            .filter_map(|n| d[(n.y * g.width + n.x) as usize])
            .min()
    };
    alive.iter().filter_map(|&j| Some((at(&ds, j)? + at(&dt, j)?, j))).min_by_key(|(c, _)| *c).map(|(_, j)| j)
}

/// Escape a JSON string.
fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Hand-formatted report (no serde dependency).
pub fn report_json(passed: bool, failures: &[String], t: &Telemetry, wall_seconds: f32, seed: u64, screenshots: &[String]) -> String {
    let list = |v: &[String]| v.iter().map(|s| json_str(s)).collect::<Vec<_>>().join(", ");
    let opt_u8 = |v: Option<u8>| v.map_or("null".to_string(), |b| b.to_string());
    let phases = t.phase_log.iter().map(|(p, s)| format!("{{\"phase\": {}, \"t\": {:.2}}}", json_str(p), s)).collect::<Vec<_>>().join(", ");
    let mut o = String::new();
    o.push_str("{\n");
    let _ = writeln!(o, "  \"passed\": {passed},");
    let _ = writeln!(o, "  \"failures\": [{}],", list(failures));
    let _ = writeln!(o, "  \"seed\": {seed},");
    let _ = writeln!(o, "  \"frames\": {},", t.frames);
    let _ = writeln!(o, "  \"sim_seconds\": {:.2},", t.sim_seconds);
    let _ = writeln!(o, "  \"wall_seconds\": {wall_seconds:.2},");
    let _ = writeln!(o, "  \"floor_reached\": {},", t.floor_reached);
    let _ = writeln!(o, "  \"kills\": {},", t.kills);
    let _ = writeln!(o, "  \"jammers_destroyed\": {},", t.jammers_destroyed);
    let _ = writeln!(o, "  \"shots_fired\": {},", t.shots_fired);
    let _ = writeln!(o, "  \"plasma_fired\": {},", t.plasma_fired);
    let _ = writeln!(o, "  \"pings_cast\": {},", t.pings_cast);
    let _ = writeln!(o, "  \"overloads_cast\": {},", t.overloads_cast);
    let _ = writeln!(o, "  \"dashes_used\": {},", t.dashes_used);
    let _ = writeln!(o, "  \"damage_taken\": {:.1},", t.damage_taken);
    let _ = writeln!(o, "  \"near_deaths\": {},", t.near_deaths);
    let _ = writeln!(o, "  \"would_have_died\": {},", t.would_have_died);
    let _ = writeln!(o, "  \"first_fatal_at\": {},", t.first_fatal_at.map_or("null".to_string(), |s| format!("{s:.2}")));
    let _ = writeln!(o, "  \"signal_min_bars\": {},", opt_u8(t.signal_min_bars));
    let _ = writeln!(o, "  \"signal_max_bars\": {},", opt_u8(t.signal_max_bars));
    let _ = writeln!(o, "  \"sos_triggered\": {},", t.sos_triggered);
    let _ = writeln!(o, "  \"sos_completed\": {},", t.sos_completed);
    let _ = writeln!(o, "  \"upgrade_picked\": {},", t.upgrade_picked.as_deref().map_or("null".to_string(), json_str));
    let _ = writeln!(o, "  \"stuck_events\": {},", t.stuck_events);
    let _ = writeln!(o, "  \"phase_log\": [{phases}],");
    let _ = writeln!(o, "  \"screenshots\": [{}]", list(screenshots));
    o.push_str("}\n");
    o
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

fn bot_menu(
    mut commands: Commands,
    time: Res<Time>,
    mut bot: ResMut<PlaytestBot>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    mut next: ResMut<NextState<AppState>>,
) {
    if bot.phase != BotPhase::Menu {
        return;
    }
    bot.phase_t += time.delta_secs();
    if bot.phase_t > 1.5 {
        bot.shot(&mut commands, "01_menu.png");
    }
    if bot.phase_t > 2.2 {
        let seed = bot.seed;
        start_run(&mut run, &mut mods, &mut next, seed);
        bot.set_phase(BotPhase::Spawn);
    }
}

/// Live target the bot can shoot at this frame.
struct Target {
    aim: Vec3,
    dist: f32,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn bot_brain(
    mut commands: Commands,
    time: Res<Time>,
    grid: Option<Res<LevelGrid>>,
    (signal, ext, run, apps, ws, mods): (Res<SignalState>, Res<ExtractionState>, Res<RunStats>, Res<PhoneApps>, Res<WeaponState>, Res<PlayerModifiers>),
    mut ctl: ResMut<BotControl>,
    mut bot: ResMut<PlaytestBot>,
    players: Query<(&Transform, &Player)>,
    enemies: Query<(Entity, &Enemy, &GlobalTransform)>,
    jammers: Query<(&DemonJammer, &GlobalTransform)>,
    pickups: Query<(&Pickup, &GlobalTransform)>,
    shots: Query<(&EnemyProjectile, &GlobalTransform)>,
) {
    let dt = time.delta_secs();
    bot.phase_t += dt;
    let (Some(grid), Ok((ptf, player))) = (grid, players.single()) else { return };
    let feet = ptf.translation;
    let eye = feet + Vec3::Y * PLAYER_EYE_HEIGHT;
    let here = grid.world_to_cell(feet);
    ctl.release_all();

    // Visible live demons, nearest first.
    let mut visible: Vec<(Entity, &Enemy, Vec3, f32)> = enemies
        .iter()
        .filter(|(_, e, _)| e.is_alive())
        .map(|(ent, e, g)| (ent, e, e.center(g.translation()), 0.0))
        .map(|(ent, e, c, _)| (ent, e, c, c.distance(eye)))
        .filter(|(_, _, c, d)| *d < 40.0 && has_line_of_sight(&grid, eye, *c))
        .collect();
    visible.sort_by(|a, b| a.3.total_cmp(&b.3));
    let threat = visible.iter().find(|(_, e, _, d)| e.alerted && *d < 20.0).map(|(_, _, c, d)| Target { aim: *c, dist: *d });

    match bot.phase {
        BotPhase::Menu | BotPhase::Finish | BotPhase::Store => {}
        BotPhase::Spawn => {
            look_at_dir(&mut ctl, player, grid.spawn_yaw, -0.02, dt);
            if bot.phase_t > 1.2 {
                bot.shot(&mut commands, "02_spawn.png");
            }
            if bot.phase_t > 1.6 {
                bot.set_phase(BotPhase::HuntJammer);
            }
        }
        BotPhase::HuntJammer => {
            if run.jammers_destroyed >= 1 {
                bot.set_phase(BotPhase::GoToTower);
                return;
            }
            let alive: Vec<(IVec2, Vec3)> = jammers
                .iter()
                .filter(|(j, _)| j.is_active())
                .map(|(_, g)| (grid.world_to_cell(g.translation()), g.translation()))
                .collect();
            let cells: Vec<IVec2> = alive.iter().map(|a| a.0).collect();
            let Some(goal) = pick_jammer(&grid, here, &cells) else {
                bot.fail("no reachable Demon Jammer on Floor 1");
                return;
            };
            let base = alive.iter().find(|a| a.0 == goal).map(|a| a.1).unwrap_or(grid.cell_center(goal));
            let core = base + Vec3::Y * 1.3;
            let jd = core.distance(eye);
            let jam_los = has_line_of_sight(&grid, eye, core);
            if let Some(t) = threat.filter(|t| t.dist < 14.0 || !jam_los) {
                engage(&mut ctl, &mut bot, player, &ws, eye, &t, dt, false);
            } else if jam_los && jd < 11.0 {
                // Jammer engagement: Shotgun.
                let jt = Target { aim: core, dist: jd };
                bot.weapon_pref = Weapon::Shotgun;
                engage(&mut ctl, &mut bot, player, &ws, eye, &jt, dt, true);
                if ws.current == Weapon::Shotgun && ws.muzzle > 0.0 {
                    bot.shot(&mut commands, "03_combat_shotgun.png");
                }
            } else {
                navigate(&mut ctl, &mut bot, &grid, feet, player, goal, true, dt);
            }
        }
        BotPhase::GoToTower => {
            if matches!(*ext, ExtractionState::Uploading { .. }) {
                bot.tel.sos_triggered = true;
                bot.set_phase(BotPhase::Sos);
                return;
            }
            let Some(stand) = tower_stand_cell(&grid, here) else {
                bot.fail(format!("no path from {here} to the Signal Tower pedestal"));
                return;
            };
            // Storyboard 04: Plasma Repeater out + Ping Scan pulse.
            if !bot.has_shot("04_ping_and_plasma.png") {
                if ws.current != Weapon::Plasma {
                    ctl.switch_weapon = Some(Weapon::Plasma);
                }
                bot.weapon_pref = Weapon::Plasma;
                let swapped = ws.current == Weapon::Plasma && ws.swap_timer <= 0.0;
                if let Some(t0) = bot.ping_shot {
                    // Show a few bolts down range while the pulse expands.
                    if bot.plasma_shown < 4 {
                        ctl.fire = true;
                    }
                    if bot.tel.sim_seconds - t0 > 0.3 {
                        bot.shot(&mut commands, "04_ping_and_plasma.png");
                    }
                } else if swapped && signal.bars >= PING_MIN_BARS && apps.ping_cooldown <= 0.0 {
                    ctl.ping = true;
                    bot.ping_shot = Some(bot.tel.sim_seconds);
                }
            }
            if let Some(t) = &threat {
                engage(&mut ctl, &mut bot, player, &ws, eye, t, dt, false);
            } else if signal.near_tower && here == stand {
                ctl.interact = true;
            } else {
                navigate(&mut ctl, &mut bot, &grid, feet, player, stand, true, dt);
                if bot.ping_shot.is_some() && bot.plasma_shown < 4 {
                    ctl.fire = true;
                }
            }
        }
        BotPhase::Sos => {
            let ExtractionState::Uploading { progress, jammed, .. } = *ext else {
                if matches!(*ext, ExtractionState::Extracting { .. } | ExtractionState::Complete) {
                    bot.tel.sos_completed = true;
                    bot.set_phase(BotPhase::Extracting);
                }
                return;
            };
            let tower = grid.tower_world();
            let stand = tower_stand_cell(&grid, here).unwrap_or(here);
            let stand_pos = grid.cell_center(stand);
            // Priority: Jammer Fiends near the ring, then the nearest visible demon.
            let ring_fiend = visible
                .iter()
                .find(|(_, e, c, _)| e.kind == EnemyKind::JammerFiend && Vec2::new(c.x - tower.x, c.z - tower.z).length() < TOWER_RING_RADIUS + 6.0);
            // A jamming fiend out of sight: hunt it down (it sits inside the ring).
            let hidden_jammer = if ring_fiend.is_none() && jammed {
                enemies
                    .iter()
                    .filter(|(_, e, _)| e.kind == EnemyKind::JammerFiend && e.is_alive())
                    .map(|(_, e, g)| e.center(g.translation()))
                    .filter(|c| Vec2::new(c.x - tower.x, c.z - tower.z).length() <= TOWER_RING_RADIUS)
                    .min_by(|a, b| a.distance(feet).total_cmp(&b.distance(feet)))
            } else {
                None
            };
            let target = ring_fiend.or(visible.first()).map(|(_, _, c, d)| Target { aim: *c, dist: *d });
            let near_count = enemies
                .iter()
                .filter(|(_, e, g)| e.is_alive() && e.center(g.translation()).distance(feet + Vec3::Y) < emp_radius(&mods) - 1.0)
                .count();
            // 5G Overload: crowds, fiends, or at least once for the storyboard.
            let want_overload = near_count >= 1 || ring_fiend.is_some_and(|f| f.3 < emp_radius(&mods)) || (bot.overload_shot.is_none() && progress > 0.75);
            if want_overload && signal.bars >= OVERLOAD_MIN_BARS && apps.overload_cooldown <= 0.0 {
                ctl.overload = true;
                if bot.overload_shot.is_none() {
                    bot.overload_shot = Some(bot.tel.sim_seconds);
                }
            }
            if let Some(t0) = bot.overload_shot
                && bot.tel.sim_seconds - t0 > 0.12
            {
                bot.shot(&mut commands, "06_5g_overload.png");
            }
            if signal.bars >= PING_MIN_BARS && apps.ping_cooldown <= 0.0 && !visible.is_empty() {
                ctl.ping = true;
            }
            let fwd = Quat::from_rotation_y(player.yaw) * Vec3::NEG_Z;
            let in_view = visible.iter().filter(|(_, _, c, d)| *d < 30.0 && (*c - eye).normalize_or_zero().dot(fwd) > 0.75).count();
            if (progress > 0.15 && in_view >= 2) || progress > 0.6 {
                bot.shot(&mut commands, "05_sos_rush.png");
            }
            // Dash out of melee range, then the ring pulls us back.
            let melee = visible
                .iter()
                .find(|(_, e, _, d)| *d < e.stats.radius + if e.kind == EnemyKind::HellBaron { 3.5 } else { 2.2 });
            if let Some((_, _, c, _)) = melee
                && player.dash_ready()
            {
                let away = Vec3::new(feet.x - c.x, 0.0, feet.z - c.z).normalize_or_zero();
                let side = Vec3::new(-away.z, 0.0, away.x);
                let to_stand = (stand_pos - feet).with_y(0.0).normalize_or_zero();
                ctl.wish_dir = (side + to_stand * 0.5).normalize_or_zero();
                ctl.dash = true;
            }
            // Hold the pedestal (inside the ring) and orbit-strafe around the mast
            // to dodge fireballs, grabbing health/armor that lands on the pedestal.
            let rel = Vec2::new(feet.x - tower.x, feet.z - tower.z);
            let on_pedestal = feet.y > tower.y - 0.4 && rel.length() < CELL_SIZE * 1.4;
            let off_stand = !on_pedestal;
            // Supplies on the ring: medkits below 65 HP, armor when it runs dry.
            let medkit = pickups
                .iter()
                .filter(|(pk, _)| match pk.kind {
                    PickupKind::Health => player.hp < 65.0,
                    PickupKind::Armor => player.armor < 30.0 || player.hp < 50.0,
                    _ => false,
                })
                .map(|(_, g)| g.translation())
                .filter(|p| Vec2::new(p.x - tower.x, p.z - tower.z).length() < TOWER_RING_RADIUS + 2.0)
                .min_by(|a, b| a.distance(feet).total_cmp(&b.distance(feet)));
            let baron_close = visible.iter().find(|(_, e, _, d)| e.kind == EnemyKind::HellBaron && *d < 7.5).map(|(_, _, c, d)| (*c, *d));
            if let Some((bc, bd)) = baron_close {
                // Never trade melee with a Hell Baron: back off (it is slow), keep shooting,
                // and bend the retreat around the mast so we stay near the ring.
                let away = Vec3::new(feet.x - bc.x, 0.0, feet.z - bc.z).normalize_or_zero();
                let to_tower = Vec3::new(tower.x - feet.x, 0.0, tower.z - feet.z);
                let out = to_tower.length() > TOWER_RING_RADIUS - 2.0;
                let side = Vec3::new(-away.z, 0.0, away.x);
                let side = if side.dot(to_tower) >= 0.0 { side } else { -side };
                ctl.wish_dir = (away + if out { side * 1.2 } else { side * 0.3 }).normalize_or_zero();
                if bd < 4.5 && player.dash_ready() {
                    ctl.dash = true;
                }
                let bt = Target { aim: target.as_ref().filter(|t| t.dist < bd - 0.5).map_or(bc, |t| t.aim), dist: bd };
                aim(&mut ctl, &mut bot, player, &ws, eye, &bt, dt, true);
            } else if let Some(m) = medkit.filter(|m| (m.y - feet.y).abs() > 1.2 || grid.world_to_cell(*m) != here) {
                // Run for the supply drop, shooting on the way.
                if (m.y - feet.y).abs() < 1.2 && m.distance(feet) < CELL_SIZE * 1.2 {
                    ctl.wish_dir = (m - feet).with_y(0.0).normalize_or_zero();
                } else {
                    navigate(&mut ctl, &mut bot, &grid, feet, player, grid.world_to_cell(m), false, dt);
                }
                if let Some(t) = &target {
                    aim(&mut ctl, &mut bot, player, &ws, eye, t, dt, true);
                }
            } else if let Some(f) = hidden_jammer {
                // Uplink jammed by an unseen fiend: go get line of sight on it.
                navigate(&mut ctl, &mut bot, &grid, feet, player, grid.world_to_cell(f), false, dt);
                let ft = Target { aim: f, dist: f.distance(eye) };
                aim(&mut ctl, &mut bot, player, &ws, eye, &ft, dt, true);
                ctl.fire &= has_line_of_sight(&grid, eye, f);
            } else if off_stand {
                navigate(&mut ctl, &mut bot, &grid, feet, player, stand, false, dt);
                if let Some(t) = &target {
                    aim(&mut ctl, &mut bot, player, &ws, eye, t, dt, true);
                }
            } else {
                if let Some(m) = medkit {
                    ctl.wish_dir = (m - feet).with_y(0.0).normalize_or_zero();
                } else if ctl.wish_dir == Vec3::ZERO {
                    let r = rel.length().max(0.1);
                    let radial = Vec3::new(rel.x, 0.0, rel.y) / r;
                    let mut tangent = Vec3::new(-radial.z, 0.0, radial.x) * if (bot.tel.sim_seconds / 2.5) as i32 % 2 == 0 { 1.0 } else { -1.0 };
                    // Kite a close Hell Baron: orbit the mast away from it, flat out.
                    let baron = visible.iter().find(|(_, e, _, d)| e.kind == EnemyKind::HellBaron && *d < 8.0);
                    let speed = if let Some((_, _, c, _)) = baron {
                        let away = Vec3::new(feet.x - c.x, 0.0, feet.z - c.z);
                        if tangent.dot(away) < 0.0 {
                            tangent = -tangent;
                        }
                        1.0
                    } else {
                        0.8
                    };
                    ctl.wish_dir = (tangent + radial * (CELL_SIZE - r) * 0.6).normalize_or_zero() * speed;
                }
                match &target {
                    Some(t) => aim(&mut ctl, &mut bot, player, &ws, eye, t, dt, true),
                    None => {
                        look_at_point(&mut ctl, player, eye, tower + Vec3::Y * 3.0, dt);
                    }
                }
            }
        }
        BotPhase::Extracting => {
            look_at_point(&mut ctl, player, eye, grid.tower_world() + Vec3::Y * 9.0, dt);
            if bot.phase_t > 0.35 {
                bot.shot(&mut commands, "07_extraction_beam.png");
            }
        }
        BotPhase::Floor2 => {
            look_at_dir(&mut ctl, player, grid.spawn_yaw, -0.02, dt);
            if bot.phase_t > 1.5 {
                bot.shot(&mut commands, "09_floor2.png");
            }
            if bot.phase_t > 2.5 {
                bot.set_phase(BotPhase::Finish);
            }
        }
    }
    // Storyboard 03 whenever the Shotgun barks in combat (the jammer may fall to Plasma).
    if matches!(bot.phase, BotPhase::HuntJammer | BotPhase::GoToTower | BotPhase::Sos) && ws.current == Weapon::Shotgun && ws.muzzle > 0.0 {
        bot.shot(&mut commands, "03_combat_shotgun.png");
    }
    // Like a moving player: sidestep fireballs about to land.
    if matches!(bot.phase, BotPhase::HuntJammer | BotPhase::GoToTower | BotPhase::Sos) {
        let incoming: Vec<(Vec3, Vec3, f32)> = shots.iter().map(|(s, g)| (g.translation(), s.vel, s.radius)).collect();
        if let Some(d) = dodge_dir(feet, &incoming) {
            let keep = ctl.wish_dir * 0.35;
            ctl.wish_dir = (d + keep).normalize_or_zero();
        }
    }
}

/// Horizontal sidestep away from the most imminent enemy projectile on a
/// collision course with a player standing at `feet` (`None` if nothing hits).
pub fn dodge_dir(feet: Vec3, shots: &[(Vec3, Vec3, f32)]) -> Option<Vec3> {
    let body = feet + Vec3::Y * 0.9;
    shots
        .iter()
        .filter_map(|&(pos, vel, radius)| {
            let v2 = vel.length_squared();
            if v2 < 1e-4 {
                return None;
            }
            let t = (body - pos).dot(vel) / v2;
            let closest = pos + vel * t;
            let miss = (body - closest).with_y(0.0);
            (t > 0.0 && t < 0.9 && miss.length() < radius + crate::physics::PLAYER_RADIUS + 0.7).then_some((t, miss, vel))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, miss, vel)| {
            let perp = Vec3::new(-vel.z, 0.0, vel.x).normalize_or_zero();
            if perp.dot(miss) >= 0.0 { perp } else { -perp }
        })
}

/// Turn toward `want_yaw` / `want_pitch` at a human-ish turn rate.
fn look_at_dir(ctl: &mut BotControl, p: &Player, want_yaw: f32, want_pitch: f32, dt: f32) -> f32 {
    let diff = (want_yaw - p.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    let max = TURN_RATE * dt;
    ctl.look_yaw = Some(p.yaw + diff.clamp(-max, max));
    let pd = want_pitch - p.pitch;
    ctl.look_pitch = Some(p.pitch + pd.clamp(-max, max));
    diff.abs().max(pd.abs())
}

fn look_at_point(ctl: &mut BotControl, p: &Player, eye: Vec3, target: Vec3, dt: f32) -> f32 {
    let d = target - eye;
    look_at_dir(ctl, p, yaw_facing(Vec2::new(d.x, d.z)), d.y.atan2(Vec2::new(d.x, d.z).length()), dt)
}

/// Aim at a target with the right weapon and pull the trigger once on target.
#[allow(clippy::too_many_arguments)]
fn aim(ctl: &mut BotControl, bot: &mut PlaytestBot, p: &Player, ws: &WeaponState, eye: Vec3, t: &Target, dt: f32, pick_weapon: bool) {
    let err = look_at_point(ctl, p, eye, t.aim, dt);
    if pick_weapon {
        // Hysteresis: Shotgun up close, Plasma at range (or when out of shells).
        if t.dist < 9.0 && p.ammo_shells > 0 {
            bot.weapon_pref = Weapon::Shotgun;
        } else if t.dist > 13.0 && p.ammo_plasma > 0 {
            bot.weapon_pref = Weapon::Plasma;
        }
    }
    if ws.current != bot.weapon_pref && ws.swap_timer <= 0.0 && bot.weapon_pref.has_ammo(p) {
        ctl.switch_weapon = Some(bot.weapon_pref);
    }
    let tolerance = (0.9 / t.dist.max(1.0)).clamp(0.03, 0.25);
    if err < tolerance && ws.swap_timer < SWAP_SECS * 0.4 {
        ctl.fire = true;
    }
}

/// Stand and fight (light strafe to dodge fireballs).
#[allow(clippy::too_many_arguments)]
fn engage(ctl: &mut BotControl, bot: &mut PlaytestBot, p: &Player, ws: &WeaponState, eye: Vec3, t: &Target, dt: f32, fixed_weapon: bool) {
    aim(ctl, bot, p, ws, eye, t, dt, !fixed_weapon);
    let strafe = (bot.tel.sim_seconds * 1.7).sin();
    let fwd = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z;
    ctl.wish_dir = Vec3::new(-fwd.z, 0.0, fwd.x) * strafe * 0.6;
}

/// Follow a BFS path toward `goal`, with stuck detection + jump/strafe recovery.
#[allow(clippy::too_many_arguments)]
fn navigate(ctl: &mut BotControl, bot: &mut PlaytestBot, g: &LevelGrid, feet: Vec3, p: &Player, goal: IVec2, look_along: bool, dt: f32) {
    let here = g.world_to_cell(feet);
    let now = bot.tel.sim_seconds;
    // Recovery manoeuvre in progress.
    if bot.recover_t > 0.0 {
        bot.recover_t -= dt;
        ctl.wish_dir = bot.recover_dir;
        ctl.jump = true;
        return;
    }
    bot.replan_t -= dt;
    if bot.replan_t <= 0.0 || bot.path.first().is_none_or(|c| *c == here) {
        bot.replan_t = 0.4;
        match bfs_path(g, here, goal) {
            Some(path) => bot.path = path,
            None => {
                // Off the walkable graph (e.g. knocked into a pit): hop to the best neighbour.
                bot.path = RampDir::ALL
                    .map(|r| here + r.offset())
                    .into_iter()
                    .filter(|n| !g.cell(*n).is_wall() && !matches!(g.cell(*n), Cell::HazardPit))
                    .min_by_key(|n| bfs_path(g, *n, goal).map_or(u32::MAX, |p| p.len() as u32))
                    .into_iter()
                    .collect();
                ctl.jump = true;
            }
        }
    }
    let Some(&next) = bot.path.first() else {
        return;
    };
    // Look ahead: aim for the furthest of the next cells in a straight line.
    let mut aim_cell = next;
    let mut straight = 1;
    for w in bot.path.windows(2) {
        if w[1] - w[0] == next - here && w[0] == aim_cell {
            aim_cell = w[1];
            straight += 1;
        } else {
            break;
        }
    }
    let target = g.cell_center(aim_cell);
    let dir = Vec3::new(target.x - feet.x, 0.0, target.z - feet.z).normalize_or_zero();
    // Pull toward the lane center when moving between cells (avoids wall hugging).
    let lane = g.cell_center(next);
    let lateral = Vec3::new(lane.x - feet.x, 0.0, lane.z - feet.z);
    let wish = (dir + (lateral - dir * lateral.dot(dir)) * 0.25).normalize_or_zero();
    ctl.wish_dir = wish;
    if look_along && wish != Vec3::ZERO {
        look_at_dir(ctl, p, yaw_facing(Vec2::new(wish.x, wish.z)), -0.03, dt);
    }
    // Airplane Mode dash down long straight corridors.
    if straight >= 3 && p.dash_ready() && p.grounded && feet.distance(target) > 9.0 {
        ctl.dash = true;
    }

    // Stuck detection over a rolling window.
    bot.samples.push_back((now, feet));
    while bot.samples.front().is_some_and(|(t, _)| now - *t > STUCK_WINDOW_SECS) {
        bot.samples.pop_front();
    }
    if let Some(&(t0, p0)) = bot.samples.front()
        && now - t0 >= STUCK_WINDOW_SECS * 0.95
        && p0.with_y(0.0).distance(feet.with_y(0.0)) < STUCK_MIN_TRAVEL
    {
        bot.tel.stuck_events += 1;
        warn!("playtest bot: stuck at cell {here} (goal {goal}), recovering");
        bot.recover_side = -bot.recover_side;
        let side = Vec3::new(-wish.z, 0.0, wish.x) * bot.recover_side;
        bot.recover_dir = (side - wish * 0.3).normalize_or_zero();
        bot.recover_t = 0.6;
        bot.samples.clear();
        bot.replan_t = 0.0;
    }
}

/// Damage / ammo / app / signal telemetry, after all combat of the frame.
fn bot_telemetry(
    mut bot: ResMut<PlaytestBot>,
    run: Res<RunStats>,
    signal: Res<SignalState>,
    apps: Res<PhoneApps>,
    mut players: Query<&mut Player>,
) {
    let Ok(mut p) = players.single_mut() else { return };
    guard(&mut bot, &mut p);
    let t = &mut bot.tel;
    t.signal_min_bars = Some(t.signal_min_bars.map_or(signal.bars, |b| b.min(signal.bars)));
    t.signal_max_bars = Some(t.signal_max_bars.map_or(signal.bars, |b| b.max(signal.bars)));
    t.floor_reached = t.floor_reached.max(run.floor);
    t.kills = run.kills;
    t.jammers_destroyed = t.jammers_destroyed.max(run.jammers_destroyed);
    if p.dash_started {
        t.dashes_used += 1;
    }
    if let Some((shells, plasma)) = bot.prev_ammo {
        if p.ammo_shells < shells {
            bot.tel.shots_fired += shells - p.ammo_shells;
        }
        if p.ammo_plasma < plasma {
            bot.tel.plasma_fired += plasma - p.ammo_plasma;
            if bot.ping_shot.is_some() {
                bot.plasma_shown += plasma - p.ammo_plasma;
            }
        }
    }
    bot.prev_ammo = Some((p.ammo_shells, p.ammo_plasma));
    let (ping_prev, over_prev) = bot.prev_cd;
    if apps.ping_cooldown > ping_prev + 0.01 {
        bot.tel.pings_cast += 1;
    }
    if apps.overload_cooldown > over_prev + 0.01 {
        bot.tel.overloads_cast += 1;
    }
    bot.prev_cd = (apps.ping_cooldown, apps.overload_cooldown);
}

/// Track real damage; convert only a fatal blow into 1 HP (`would_have_died`).
fn guard(bot: &mut PlaytestBot, p: &mut Player) {
    let now = (p.hp, p.armor);
    if let Some((hp, armor)) = bot.prev_vitals {
        let lost = (hp - now.0).max(0.0) + (armor - now.1).max(0.0);
        bot.tel.damage_taken += lost;
        if hp >= 25.0 && now.0 < 25.0 {
            bot.tel.near_deaths += 1;
        }
    }
    if p.hp <= 0.0 {
        // One count per near-death episode (not every tick spent at 1 HP).
        if bot.prev_vitals.is_none_or(|(hp, _)| hp > 1.0) {
            bot.tel.would_have_died += 1;
            bot.tel.first_fatal_at.get_or_insert(bot.tel.sim_seconds);
        }
        warn!("playtest bot: fatal blow absorbed (would have died #{})", bot.tel.would_have_died);
        p.hp = 1.0;
        p.invuln_timer = p.invuln_timer.max(1.0);
    }
    bot.prev_vitals = Some((p.hp, p.armor));
}

fn guard_fatal_blow(mut bot: ResMut<PlaytestBot>, mut players: Query<&mut Player>, state: Res<State<AppState>>) {
    if *state.get() != AppState::Playing {
        return;
    }
    if let Ok(mut p) = players.single_mut() {
        guard(&mut bot, &mut p);
    }
}

fn bot_store(
    mut commands: Commands,
    time: Res<Time>,
    mut bot: ResMut<PlaytestBot>,
    choices: Res<crate::upgrades::UpgradeChoices>,
    mut ctl: ResMut<BotControl>,
) {
    if bot.phase == BotPhase::Finish {
        return;
    }
    bot.set_phase(BotPhase::Store);
    bot.phase_t += time.delta_secs();
    ctl.release_all();
    if bot.phase_t > 1.0 {
        bot.shot(&mut commands, "08_upgrade_store.png");
    }
    if bot.phase_t > 1.8 && bot.tel.upgrade_picked.is_none() {
        bot.tel.upgrade_picked = choices.0.first().map(|u| u.name().to_string());
        ctl.pick_upgrade = Some(0);
    }
}

/// Phase bookkeeping, timeout, final report and exit.
#[allow(clippy::too_many_arguments)]
fn bot_watchdog(
    time: Res<Time>,
    mut bot: ResMut<PlaytestBot>,
    state: Res<State<AppState>>,
    run: Res<RunStats>,
    mut exit: MessageWriter<AppExit>,
) {
    bot.tel.frames += 1;
    bot.tel.sim_seconds += time.delta_secs();
    match state.get() {
        AppState::Playing if bot.phase == BotPhase::Store && run.floor >= 2 => bot.set_phase(BotPhase::Floor2),
        AppState::GameOver if bot.phase != BotPhase::Finish => bot.fail("player died (GameOver) despite the fatal-blow guard"),
        AppState::Victory if bot.phase != BotPhase::Finish => bot.fail("unexpected Victory on a 2-floor playtest"),
        _ => {}
    }
    if bot.phase != BotPhase::Finish && bot.tel.sim_seconds > BOT_TIMEOUT_SECS {
        let phase = bot.phase;
        bot.fail(format!("timeout after {BOT_TIMEOUT_SECS}s (stuck in phase {phase:?})"));
    }
    if bot.phase != BotPhase::Finish {
        return;
    }
    // Let pending screenshots reach the disk before checking them.
    bot.finish_frames += 1;
    if bot.finish_frames < 20 {
        return;
    }
    if bot.finish_frames > 20 {
        return;
    }
    let t = bot.tel.clone();
    let mut failures = bot.failures.clone();
    if t.jammers_destroyed == 0 {
        failures.push("no Demon Jammer destroyed".into());
    }
    if !t.sos_triggered {
        failures.push("never reached the tower / SOS not triggered".into());
    }
    if !t.sos_completed {
        failures.push("SOS uplink never completed".into());
    }
    if t.floor_reached < 2 {
        failures.push(format!("did not reach Floor 2 (floor {})", t.floor_reached));
    }
    if t.upgrade_picked.is_none() {
        failures.push("no upgrade installed".into());
    }
    if t.would_have_died > 0 {
        failures.push(format!("absorbed {} fatal blow(s) during playtest", t.would_have_died));
    }
    let mut saved = Vec::new();
    for name in STORYBOARD {
        match &bot.out_dir {
            Some(dir) if dir.join(name).exists() => saved.push(dir.join(name).display().to_string()),
            Some(_) => failures.push(format!("missing screenshot {name}")),
            None if bot.has_shot(name) => saved.push(name.to_string()),
            None => failures.push(format!("storyboard moment never reached: {name}")),
        }
    }
    failures.dedup();
    let passed = failures.is_empty();
    let json = report_json(passed, &failures, &t, bot.started.elapsed().as_secs_f32(), bot.seed, &saved);
    if let Some(dir) = &bot.out_dir {
        let report = dir.join("playtest_report.json");
        if let Err(e) = std::fs::write(&report, &json) {
            error!("playtest bot: cannot write {}: {e}", report.display());
        }
        info!("playtest bot: report -> {} (passed: {passed})", report.display());
    }
    bot.report = Some((passed, json));
    exit.write(if passed { AppExit::Success } else { AppExit::from_code(1) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::generate_level;

    #[test]
    fn bfs_path_reaches_tower_pedestal_on_every_floor() {
        for floor in 1..=5 {
            for seed in [1u64, 99, 1234] {
                let g = generate_level(floor, seed);
                let stand = tower_stand_cell(&g, g.spawn).expect("pedestal reachable");
                let path = bfs_path(&g, g.spawn, stand).expect("path");
                assert_eq!(*path.last().unwrap(), stand);
                let mut prev = g.spawn;
                for c in &path {
                    assert!(g.can_walk(prev, *c), "path step {prev} -> {c} must be walkable");
                    prev = *c;
                }
                assert!(pick_jammer(&g, g.spawn, &g.jammers).is_some());
            }
        }
    }

    #[test]
    fn dodge_sidesteps_incoming_fireballs_only() {
        let feet = Vec3::ZERO;
        // Fireball 5 m away flying straight at us, slightly to our left.
        let d = dodge_dir(feet, &[(Vec3::new(-0.2, 0.9, -5.0), Vec3::new(0.0, 0.0, 12.0), 0.3)]).expect("dodge");
        assert!(d.y.abs() < 1e-6 && (d.length() - 1.0).abs() < 1e-4);
        assert!(d.x > 0.9, "step away from the side it comes in on: {d}");
        // Flying away / far wide / too far off in time: no dodge.
        assert!(dodge_dir(feet, &[(Vec3::new(0.0, 0.9, -5.0), Vec3::new(0.0, 0.0, -12.0), 0.3)]).is_none());
        assert!(dodge_dir(feet, &[(Vec3::new(5.0, 0.9, -5.0), Vec3::new(0.0, 0.0, 12.0), 0.3)]).is_none());
        assert!(dodge_dir(feet, &[(Vec3::new(0.0, 0.9, -40.0), Vec3::new(0.0, 0.0, 12.0), 0.3)]).is_none());
    }

    #[test]
    fn report_is_valid_looking_json() {
        let t = Telemetry { frames: 10, upgrade_picked: Some("Dual \"SIM\"".into()), signal_min_bars: Some(0), ..default() };
        let j = report_json(false, &["a\\b".into()], &t, 1.0, 7, &["x.png".into()]);
        assert!(j.contains("\"passed\": false"));
        assert!(j.contains("\"failures\": [\"a\\\\b\"]"));
        assert!(j.contains("\"upgrade_picked\": \"Dual \\\"SIM\\\"\""));
        assert!(j.contains("\"signal_max_bars\": null"));
        assert!(j.trim_end().ends_with('}'));
        assert_eq!(j.matches('{').count(), j.matches('}').count());
    }
}
