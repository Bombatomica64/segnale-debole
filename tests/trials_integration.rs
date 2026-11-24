//! Milestone 18 headless ECS integration tests: speedrun timer freezing and
//! floor splits, Daily Seeded Runs, and the movement Time Trial (course,
//! sequential checkpoints, pit rescue, instant `R` reset, finish records and
//! the menu / HUD wiring).

use bevy::ecs::system::RunSystemOnce;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::prelude::*;

use segnale_debole::bot_input::BotControl;
use segnale_debole::enemies::Enemy;
use segnale_debole::extraction::ExtractionState;
use segnale_debole::hud::{HudPlugin, SplitPlate, SplitText};
use segnale_debole::level::{generate_level, DemonJammer, JumpPad, LevelGrid, CATWALK_HEIGHT, PIT_DEPTH};
use segnale_debole::meta::{ProfileStore, RunLoadout, TIME_TRIAL_SUMMARY};
use segnale_debole::player::Player;
use segnale_debole::signal::SignalState;
use segnale_debole::state::{complete_floor, start_run, AppState, PlayerModifiers, RunMode, RunStats, SettingsReturn, UpgradeId};
use segnale_debole::trials::{
    begin_daily_attempt, daily_loadout, daily_record, daily_seed, start_daily_run, start_time_trial, time_trial_gates, DailyDate, TimeTrialState,
    TrialGateMarker, TRIAL_CHECKPOINTS, TRIAL_ROCKETS, TRIAL_WIDTH,
};
use segnale_debole::weapons::{compute_rocket_jump, Weapon, WeaponState};
use segnale_debole::{build_headless_test_app, HeadlessGamePlugin, HEADLESS_DT};

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn app_state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

fn run_stats(app: &App) -> RunStats {
    app.world().resource::<RunStats>().clone()
}

fn trial(app: &App) -> TimeTrialState {
    app.world().resource::<TimeTrialState>().clone()
}

fn store(app: &App) -> ProfileStore {
    app.world().resource::<ProfileStore>().clone()
}

fn player(app: &mut App) -> Player {
    let mut q = app.world_mut().query::<&Player>();
    q.single(app.world()).expect("exactly one player").clone()
}

fn player_pos(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).expect("exactly one player").translation
}

fn with_player(app: &mut App, f: impl FnOnce(&mut Player, &mut Transform)) {
    let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
    let (mut p, mut tf) = q.single_mut(app.world_mut()).expect("exactly one player");
    f(&mut p, &mut tf);
}

fn teleport(app: &mut App, feet: Vec3) {
    with_player(app, |p, tf| {
        tf.translation = feet;
        p.velocity = Vec3::ZERO;
        p.prev_pos = Some(feet);
    });
}

fn count<C: Component>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<(), With<C>>();
    q.iter(app.world()).count()
}

fn entity_count(app: &mut App) -> usize {
    let mut q = app.world_mut().query::<Entity>();
    q.iter(app.world()).count()
}

fn godmode(app: &mut App) {
    with_player(app, |p, _| {
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
}

fn set_state(app: &mut App, s: AppState) {
    app.world_mut().resource_mut::<NextState<AppState>>().set(s);
    step(app, 1);
    assert_eq!(app_state(app), s);
}

fn tap(app: &mut App, key: KeyCode) {
    for state in [ButtonState::Pressed, ButtonState::Released] {
        app.world_mut().write_message(KeyboardInput {
            key_code: key,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
    }
    step(app, 2);
}

fn begin_time_trial(app: &mut App) {
    app.world_mut()
        .run_system_once(
            |mut run: ResMut<RunStats>,
             mut mods: ResMut<PlayerModifiers>,
             mut loadout: ResMut<RunLoadout>,
             mut trial: ResMut<TimeTrialState>,
             mut next: ResMut<NextState<AppState>>| {
                start_time_trial(&mut run, &mut mods, &mut loadout, &mut trial, &mut next, Default::default());
            },
        )
        .unwrap();
    step(app, 3);
    assert_eq!(app_state(app), AppState::Playing);
}

/// Teleport the player's feet onto a grid cell's center.
fn goto_cell(app: &mut App, c: IVec2) {
    let feet = cell_feet(app, c);
    teleport(app, feet);
}

/// Feet position at the center of a grid cell.
fn cell_feet(app: &App, c: IVec2) -> Vec3 {
    app.world().resource::<LevelGrid>().cell_center(c) + Vec3::Y * 0.02
}

#[test]
fn speedrun_clock_freezes_in_pause_settings_and_upgrade_select_and_records_splits() {
    let mut app = build_headless_test_app();
    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut next: ResMut<NextState<AppState>>| {
            start_run(&mut run, &mut mods, &mut next, 4242);
        })
        .unwrap();
    step(&mut app, 3);
    godmode(&mut app);
    step(&mut app, 60);
    let t0 = run_stats(&app).time_elapsed;
    assert!(t0 > 0.9, "clock runs while playing ({t0})");

    set_state(&mut app, AppState::Paused);
    let paused = run_stats(&app).time_elapsed;
    step(&mut app, 45);
    assert_eq!(run_stats(&app).time_elapsed, paused, "pause freezes the clock");

    app.world_mut().resource_mut::<SettingsReturn>().0 = AppState::Paused;
    set_state(&mut app, AppState::Settings);
    step(&mut app, 45);
    assert_eq!(run_stats(&app).time_elapsed, paused, "settings mid-run freezes the clock");

    set_state(&mut app, AppState::Playing);
    godmode(&mut app);
    step(&mut app, 30);
    let resumed = run_stats(&app).time_elapsed;
    assert!((resumed - paused - 31.0 * HEADLESS_DT).abs() < 3.0 * HEADLESS_DT, "resumes where it stopped ({paused} -> {resumed})");

    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>| {
            complete_floor(&mut run, &mut next);
        })
        .unwrap();
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    let run = run_stats(&app);
    assert_eq!(run.floor_splits.len(), 1, "a cleared floor records its split");
    assert!((run.floor_splits[0] - run.time_elapsed).abs() < 1e-4);
    step(&mut app, 45);
    assert_eq!(run_stats(&app).time_elapsed, run.time_elapsed, "the App Store freezes the clock");
}

#[test]
fn daily_run_deploys_the_dates_bunker_and_records_the_attempt() {
    let mut app = build_headless_test_app();
    let date = DailyDate::new(2026, 11, 24);
    app.world_mut()
        .run_system_once(
            move |mut run: ResMut<RunStats>,
                  mut mods: ResMut<PlayerModifiers>,
                  mut loadout: ResMut<RunLoadout>,
                  mut store: ResMut<ProfileStore>,
                  mut next: ResMut<NextState<AppState>>| {
                begin_daily_attempt(&mut store.profile.daily_records, date);
                start_daily_run(&mut run, &mut mods, &mut loadout, &mut next, date, Default::default());
            },
        )
        .unwrap();
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    let run = run_stats(&app);
    assert_eq!((run.mode, run.run_seed, run.floor), (RunMode::Daily(date), daily_seed(date), 1));
    assert_eq!(*app.world().resource::<RunLoadout>(), daily_loadout(date));
    let g = app.world().resource::<LevelGrid>().clone();
    let expected = generate_level(1, run.floor_seed());
    assert_eq!((g.seed, g.spawn, g.tower, g.jammers.clone()), (expected.seed, expected.spawn, expected.tower, expected.jammers.clone()), "same bunker for everyone");
    assert!(count::<Enemy>(&mut app) > 0, "a daily is a real run");

    step(&mut app, 30);
    with_player(&mut app, |p, _| {
        p.invuln_timer = 0.0;
        p.hp = 0.0;
    });
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::GameOver);
    let s = store(&app);
    let rec = daily_record(&s.profile.daily_records, date).expect("today's record");
    assert_eq!((rec.attempts, rec.best_floor, rec.victory), (1, 1, false));
    assert_eq!(s.profile.stats.runs_played, 1, "dailies count as real runs");
    assert_eq!(s.profile.history.last().unwrap().daily_date, Some(date));
}

#[test]
fn time_trial_course_checkpoints_rescue_reset_and_finish() {
    let mut app = build_headless_test_app();
    begin_time_trial(&mut app);
    step(&mut app, 20);

    // Course + rules.
    let g = app.world().resource::<LevelGrid>().clone();
    assert_eq!(g.width, TRIAL_WIDTH);
    assert_eq!(run_stats(&app).mode, RunMode::TimeTrial);
    assert_eq!((count::<Enemy>(&mut app), count::<DemonJammer>(&mut app)), (0, 0), "no demons, no jammers");
    assert_eq!(count::<TrialGateMarker>(&mut app), TRIAL_CHECKPOINTS + 1);
    assert_eq!(app.world().resource::<SignalState>().bars, 5, "signal pinned to 5 bars");
    let mut pads = app.world_mut().query::<&JumpPad>();
    assert!(pads.iter(app.world()).count() == 3 && pads.iter(app.world()).all(|p| p.powered), "every jump pad powered");
    let p = player(&mut app);
    assert_eq!((p.ammo_rockets, p.max_rockets), (TRIAL_ROCKETS, TRIAL_ROCKETS));
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Rocket);
    let mods = app.world().resource::<PlayerModifiers>().clone();
    let jump = compute_rocket_jump(Vec3::ZERO, Vec3::new(0.0, 0.2, 0.0), 4.0, &mods).unwrap();
    assert_eq!(jump.self_damage, 0.0, "rocket jumps are free");
    assert!(jump.impulse.y > 0.0);
    assert!(run_stats(&app).upgrades.contains(&UpgradeId::BlastThrusters));
    let t = trial(&app);
    assert!(t.elapsed > 0.2 && t.next_gate == 0 && !t.reset_requested);

    // The SOS uplink never starts, even parked at the mast.
    let gates = time_trial_gates();
    goto_cell(&mut app, g.tower + IVec2::X);
    step(&mut app, 240);
    assert!(matches!(*app.world().resource::<ExtractionState>(), ExtractionState::Searching { .. }), "no SOS in the trial");
    assert_eq!(trial(&app).next_gate, 0, "the finish needs every checkpoint first");

    // Gates trigger strictly in order.
    goto_cell(&mut app, gates[1].cells[0]);
    step(&mut app, 2);
    assert_eq!(trial(&app).next_gate, 0, "CP 2 before CP 1 does nothing");
    goto_cell(&mut app, gates[0].cells[1]);
    step(&mut app, 2);
    assert_eq!(trial(&app).next_gate, 1);
    assert_eq!(trial(&app).splits.len(), 1);
    goto_cell(&mut app, gates[2].cells[0]);
    step(&mut app, 2);
    assert_eq!(trial(&app).next_gate, 1, "CP 3 before CP 2 does nothing");
    goto_cell(&mut app, gates[1].cells[1]);
    step(&mut app, 2);
    assert_eq!(trial(&app).next_gate, 2);

    // Pit rescue: back to CP 2, no damage, still playing.
    teleport(&mut app, Vec3::new(10.5 * 4.0, PIT_DEPTH, 4.5 * 4.0));
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Playing);
    let pos = player_pos(&mut app);
    assert_eq!(g.world_to_cell(pos), gates[1].respawn, "rescued to CP 2");
    assert!(pos.y > CATWALK_HEIGHT - 0.1);
    assert_eq!(trial(&app).rescues, 1);
    assert_eq!(player(&mut app).hp, player(&mut app).max_hp);

    // Instant R reset: clock, checkpoints, position, velocity, ammo.
    with_player(&mut app, |p, _| {
        p.velocity = Vec3::new(20.0, 3.0, 0.0);
        p.dash_cooldown = 1.5;
        p.ammo_rockets = 4;
    });
    let entities_before = entity_count(&mut app);
    tap(&mut app, KeyCode::KeyR);
    let t = trial(&app);
    assert!(t.elapsed < 4.0 * HEADLESS_DT && t.next_gate == 0 && t.splits.is_empty(), "{t:?}");
    let p = player(&mut app);
    assert_eq!(g.world_to_cell(player_pos(&mut app)), g.spawn);
    assert!(p.velocity.length() < 1.0 && p.dash_cooldown == 0.0 && p.ammo_rockets == TRIAL_ROCKETS);
    assert!((p.yaw - g.spawn_yaw).abs() < 1e-4);
    assert_eq!(app.world().resource::<LevelGrid>().seed, g.seed);
    let entities_after = entity_count(&mut app);
    assert!(entities_after.abs_diff(entities_before) < 40, "no level rebuild ({entities_before} -> {entities_after})");

    // Full run: CP 1 -> CP 2 -> CP 3 -> FINISH.
    for gate in gates.iter() {
        step(&mut app, 30);
        goto_cell(&mut app, gate.cells[0]);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    let t = trial(&app);
    assert!(t.finished && t.splits.len() == TRIAL_CHECKPOINTS + 1);
    assert!(t.splits.windows(2).all(|w| w[0] < w[1]));
    let final_time = t.final_time().unwrap();
    let s = store(&app);
    assert_eq!(s.profile.time_trial_best_secs, Some(final_time));
    assert_eq!(s.profile.time_trial_best_splits, t.splits);
    assert_eq!((s.profile.stats.runs_played, s.profile.stats.victories), (0, 0), "trials never count as runs");
    assert!(s.profile.history.is_empty() && s.profile.unlocked_feats.is_empty());
    assert_eq!(s.last_run_summary.as_ref().and_then(|r| r.skipped_reason.clone()).as_deref(), Some(TIME_TRIAL_SUMMARY));
    assert!(t.result.unwrap().new_pb);

    // The clock froze at the finish.
    step(&mut app, 30);
    assert_eq!(trial(&app).elapsed, t.elapsed);
}

#[test]
fn menu_hotkeys_launch_daily_and_trial_and_retry_keeps_the_course() {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();

    // D: today's Daily.
    tap(&mut app, KeyCode::KeyD);
    assert_eq!(app_state(&app), AppState::Playing);
    let today = DailyDate::today_utc();
    assert_eq!(run_stats(&app).mode, RunMode::Daily(today));
    assert_eq!(daily_record(&store(&app).profile.daily_records, today).map(|r| r.attempts), Some(1));
    step(&mut app, 3);
    let mut plate = app.world_mut().query_filtered::<&Node, With<SplitPlate>>();
    assert_eq!(plate.single(app.world()).unwrap().display, Display::Flex, "split plate always on in a daily");
    set_state(&mut app, AppState::MainMenu);

    // T: the Time Trial; its plate shows the trial clock.
    tap(&mut app, KeyCode::KeyT);
    assert_eq!(app_state(&app), AppState::Playing);
    assert_eq!(run_stats(&app).mode, RunMode::TimeTrial);
    step(&mut app, 10);
    let mut texts = app.world_mut().query::<(&SplitText, &Text)>();
    let tag = texts.iter(app.world()).find(|(f, _)| **f == SplitText::Tag).map(|(_, t)| t.0.clone()).unwrap();
    assert!(tag.starts_with("TIME TRIAL"), "{tag}");

    // Finish, then R on the finish card retries on the same course instantly.
    for gate in time_trial_gates().iter() {
        goto_cell(&mut app, gate.cells[0]);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    let seed = app.world().resource::<LevelGrid>().seed;
    tap(&mut app, KeyCode::KeyR);
    assert_eq!(app_state(&app), AppState::Playing);
    let t = trial(&app);
    assert!(!t.finished && t.next_gate == 0 && t.elapsed < 0.1, "{t:?}");
    assert_eq!(app.world().resource::<LevelGrid>().seed, seed);
    assert_eq!(count::<Player>(&mut app), 1);
    // E never drops a trial into Endless Overdrive.
    for gate in time_trial_gates().iter() {
        goto_cell(&mut app, gate.cells[0]);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    tap(&mut app, KeyCode::KeyE);
    assert_eq!(app_state(&app), AppState::Victory);
    assert!(!run_stats(&app).endless);
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert_eq!(store(&app).profile.stats.runs_played, 0);
}

fn bot(app: &mut App) -> Mut<'_, BotControl> {
    app.world_mut().resource_mut::<BotControl>()
}

/// Walk onto a pad (world-space wish toward its center) and ride it out.
fn ride_pad(app: &mut App, from: IVec2, pad: IVec2, frames: usize) -> Vec<Vec3> {
    goto_cell(app, from);
    step(app, 30);
    let target = cell_feet(app, pad);
    let mut path = Vec::new();
    let mut launched = false;
    for _ in 0..frames {
        let p = player_pos(app);
        launched |= player(app).velocity.y > 5.0;
        let d = Vec3::new(target.x - p.x, 0.0, target.z - p.z);
        bot(app).wish_dir = if !launched && d.length() > 0.3 { d.normalize() } else { Vec3::ZERO };
        step(app, 1);
        path.push(player_pos(app));
    }
    bot(app).release_all();
    path
}

#[test]
fn course_pads_vault_the_catwalk_clear_the_trench_and_rocket_jumps_are_free() {
    let mut app = build_headless_test_app();
    begin_time_trial(&mut app);
    step(&mut app, 20);
    let g = app.world().resource::<LevelGrid>().clone();

    // Bay pad (20,12) vaults onto the 2.4 m catwalk, from a walk and from a standstill.
    let path = ride_pad(&mut app, IVec2::new(20, 14), IVec2::new(20, 12), 150);
    let end = *path.last().unwrap();
    assert!(end.y > CATWALK_HEIGHT - 0.1 && g.world_to_cell(end).y <= 10, "landed on the catwalk: {end}");
    goto_cell(&mut app, IVec2::new(20, 12));
    step(&mut app, 150);
    let end = player_pos(&mut app);
    assert!(end.y > CATWALK_HEIGHT - 0.1 && g.world_to_cell(end).y <= 10, "landed on the catwalk: {end}");

    // Trench pad (9,4) launches east over the lava (no pit rescue).
    let rescues = trial(&app).rescues;
    let path = ride_pad(&mut app, IVec2::new(7, 4), IVec2::new(9, 4), 150);
    let end = *path.last().unwrap();
    assert_eq!(trial(&app).rescues, rescues, "cleared the trench");
    assert!(g.world_to_cell(end).x >= 12, "landed past the lava: {end}");
    assert!(path.iter().map(|p| p.y).fold(f32::MIN, f32::max) > 2.0, "a real arc");

    // Rocket jump at the feet: big vertical kick, zero self-damage.
    goto_cell(&mut app, IVec2::new(4, 4));
    step(&mut app, 30);
    with_player(&mut app, |p, _| p.armor = 0.0);
    {
        let mut b = bot(&mut app);
        b.look_pitch = Some(-1.45);
        b.fire = true;
    }
    let mut max_vy: f32 = 0.0;
    let mut max_y: f32 = 0.0;
    for _ in 0..60 {
        step(&mut app, 1);
        max_vy = max_vy.max(player(&mut app).velocity.y);
        max_y = max_y.max(player_pos(&mut app).y);
    }
    bot(&mut app).fire = false;
    let p = player(&mut app);
    assert!(max_vy > 8.0 && max_y > 1.5, "rocket jump launched the player (vy {max_vy}, y {max_y})");
    assert_eq!(p.hp, p.max_hp, "no self-damage in the trial");
    assert_eq!(p.ammo_rockets, TRIAL_ROCKETS, "rack auto-refills");
}
