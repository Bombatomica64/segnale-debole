//! Menus (main / App Store upgrade select / pause / game over / victory) and
//! the in-game HUD: status + SOS uplink bar, crosshair hitmarker, killfeed
//! toasts, VEL speedometer + HP / armor / stamina vitals, weapon & ammo deck and a subtle
//! Dead-Zone edge frame. Signal bars, radar and phone apps live on the 3D
//! phone screen (`phone_screen.rs`), not here.
//!
//! Everything is built from the gritty industrial-punk kit in `ui_kit.rs`
//! (hard corners, bolted-plate borders, tape strips, stencil titles).
//!
//! Buttons use a small manual hit test (`ComputedNode::contains_point` with the
//! window's physical cursor position) because Bevy 0.20 deprecated `Interaction`
//! in favour of picking, which this build does not enable.

use std::fmt::Write as _;

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;

use crate::assets_gen::FloorTheme;
use crate::audio::{SfxEvent, SfxQueue};
use crate::boss::BossHud;
use crate::compendium::{apply_custom_label, pick_run_seed, seed_key_char, shareable_code, CustomSeedState};
use crate::extraction::{ExtractionState, UPLINK_HOLD_SECS};
use crate::fx::HudFeed;
use crate::gamepad::{DefaultFocus, GamepadFocus, GamepadInput};
use crate::meta::{live_settings, record_game_over, record_victory, start_run_with_loadout, CrosshairColor, CrosshairStyle, ProfileStore, RunLoadout};
use crate::meta_ui::{
    end_loadout_line, loadout_summary_line, loadout_tag_parts, unlock_reward_lines, MetaBox, MetaUiPlugin, NO_UNLOCKS_LINE,
};
use crate::player::{Player, DASH_COOLDOWN};
use crate::signal::SignalState;
use crate::state::{complete_floor, continue_into_endless, AppState, GameSet, PlayerModifiers, RunMode, RunStats, SettingsReturn, FINAL_FLOOR};
use crate::trials::{
    begin_daily_attempt, daily_loadout, daily_record, daily_rules_line, floor_splits_line, format_split_delta, format_split_time, split_plate_view,
    split_timer_visible, start_daily_run, start_time_trial, trial_splits_line, DailyDate, TimeTrialState, SPLIT_GOLD,
};
use crate::ui_kit::{
    hard_shadow, hazard_strip, hero_title, hud_plate, inset_border, keycap, plate_border, plate_rect, punk_button, punk_screen_root,
    punk_screen_root_tinted, recessed_bezel, screen_rails, section_crate, signal_bars, stamped_tag, stencil_title, stencil_title_layers,
    terminal_chassis, tiled_image, ButtonStyle, PunkButtonColors, Tape, UiKitPlugin, UiKitTextures, ACID_GREEN, ARC_CYAN, ASH_DIM, BONE_TEXT, HAZARD_DARK, HAZARD_YELLOW, INK_BLACK, IRON_DARK, IRON_MID, PUNK_RED, STEEL_BORDER, STEEL_LIGHT,
};
use crate::upgrades::{draft_upgrades, next_rank_preview, pick_upgrade, UpgradeChoices, UpgradeFocus};
use crate::weapons::{doppler_active, Weapon, WeaponState};

pub const COLOR_BG: Color = INK_BLACK;
pub const COLOR_TITLE: Color = PUNK_RED;
pub const COLOR_TEXT: Color = BONE_TEXT;
pub const COLOR_DIM: Color = ASH_DIM;
pub const COLOR_SIGNAL_RED: Color = Color::srgb(1.0, 0.15, 0.1);
pub const COLOR_SIGNAL_AMBER: Color = Color::srgb(1.0, 0.7, 0.1);
pub const COLOR_SIGNAL_GREEN: Color = Color::srgb(0.2, 1.0, 0.75);
pub const COLOR_CYAN: Color = Color::srgb(0.3, 0.95, 1.0);
pub const COLOR_STORE_BG: Color = Color::srgba(0.03, 0.025, 0.02, 0.94);
pub const COLOR_CARD: Color = IRON_MID;
pub const COLOR_CARD_HOVER: Color = Color::srgb(0.2, 0.17, 0.15);

/// Color for a 0..=5 bar reading.
pub fn signal_color(bars: u8) -> Color {
    match bars {
        0..=1 => COLOR_SIGNAL_RED,
        2..=3 => COLOR_SIGNAL_AMBER,
        _ => COLOR_SIGNAL_GREEN,
    }
}

/// What a clickable menu button (or its keyboard shortcut) does.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Deploy a run with the profile's equipped loadout.
    StartRun,
    /// Tear down the current run and immediately redeploy on Floor 1.
    RestartRun,
    Continue,
    BackToMenu,
    OpenLoadout,
    OpenContractsFeats,
    /// Open the Settings screen (remembers where to return).
    OpenSettings,
    /// Leave Settings for [`SettingsReturn`].
    CloseSettings,
    /// Victory: keep descending into Endless Overdrive (Floor 6+).
    ContinueEndless,
    /// Main menu: today's Daily Seeded Run.
    StartDaily,
    /// Main menu: the movement Time Trial course.
    StartTimeTrial,
    /// Time Trial finish card: instant retry (no level rebuild).
    RetryTimeTrial,
    /// Main menu: the 3D Bestiary & Armory viewer.
    OpenCompendium,
    /// Main menu SEED switch: open the inline seed editor / lock the code.
    ToggleSeedEdit,
    /// Main menu `RND`: arm a fresh random seed code.
    RollSeed,
    /// Main menu `CLR`: back to random seeds.
    ClearSeed,
    /// Pause / end screens: arm (and on the web, link) the run's seed code.
    ShareSeed,
}

/// A clickable menu switch (see [`crate::ui_kit::punk_button`]).
#[derive(Component)]
pub struct MenuButton;

/// Keyboard shortcut for a state, given which keys were just pressed:
/// - `MainMenu`: ENTER play, L loadout, C contracts & feats
/// - `Paused`: ESC / ENTER resume, R restart, L loadout, M main menu
/// - `Loadout`: ENTER deploy, ESC back, C contracts & feats
/// - `ContractsFeats`: ENTER deploy, ESC back, L loadout
/// - `GameOver` / `Victory`: ENTER main menu, L loadout
/// - `Settings`: ESC back
///
/// The `O` (settings) and `E` (endless) hotkeys live in [`menu_hotkey_action`].
pub fn menu_key_action(state: AppState, enter: bool, esc: bool, l: bool, c: bool, r: bool, m: bool) -> Option<MenuAction> {
    use MenuAction::*;
    match state {
        AppState::MainMenu if enter => Some(StartRun),
        AppState::MainMenu if l => Some(OpenLoadout),
        AppState::MainMenu if c => Some(OpenContractsFeats),
        AppState::Paused if esc || enter => Some(Continue),
        AppState::Paused if r => Some(RestartRun),
        AppState::Paused if l => Some(OpenLoadout),
        AppState::Paused if m => Some(BackToMenu),
        AppState::Loadout | AppState::ContractsFeats if enter => Some(StartRun),
        AppState::Loadout | AppState::ContractsFeats if esc => Some(BackToMenu),
        AppState::Loadout if c => Some(OpenContractsFeats),
        AppState::ContractsFeats if l => Some(OpenLoadout),
        AppState::GameOver | AppState::Victory if enter => Some(BackToMenu),
        AppState::GameOver | AppState::Victory if l => Some(OpenLoadout),
        AppState::Settings if esc => Some(CloseSettings),
        _ => None,
    }
}

/// Extra menu hotkeys: `O` opens Settings from `MainMenu` / `Paused`, `E`
/// keeps descending into Endless Overdrive from `Victory`.
pub fn menu_hotkey_action(state: AppState, o: bool, e: bool) -> Option<MenuAction> {
    match state {
        AppState::MainMenu | AppState::Paused if o => Some(MenuAction::OpenSettings),
        AppState::Victory if e => Some(MenuAction::ContinueEndless),
        _ => None,
    }
}

/// Mode hotkeys: `D` Daily Run / `T` Time Trial from `MainMenu`, `R` retry
/// from the Time Trial finish card (`Victory` in [`RunMode::TimeTrial`]).
pub fn mode_hotkey_action(state: AppState, mode: RunMode, d: bool, t: bool, r: bool) -> Option<MenuAction> {
    match state {
        AppState::MainMenu if d => Some(MenuAction::StartDaily),
        AppState::MainMenu if t => Some(MenuAction::StartTimeTrial),
        AppState::Victory if r && mode == RunMode::TimeTrial => Some(MenuAction::RetryTimeTrial),
        _ => None,
    }
}

/// Milestone 21 hotkeys: `B` opens the Bestiary & Armory from `MainMenu`;
/// `S` shares the run's seed from `Paused` / `GameOver` / `Victory` (not in
/// the seedless Time Trial). `S` on the main menu opens the seed editor (see
/// [`seed_entry_keys`]), and while that editor is open every main menu
/// hotkey is suppressed so typing `DOOM` or `BOSS` never launches a screen.
pub fn seed_hotkey_action(state: AppState, mode: RunMode, b: bool, s: bool) -> Option<MenuAction> {
    match state {
        AppState::MainMenu if b => Some(MenuAction::OpenCompendium),
        AppState::Paused | AppState::GameOver | AppState::Victory if s && mode != RunMode::TimeTrial => Some(MenuAction::ShareSeed),
        _ => None,
    }
}

/// Controller shortcuts (focus + `A` covers every button; these are extras):
/// - `Start`: PLAY from `MainMenu`, DEPLOY from `Loadout` / `ContractsFeats`
///   (`Start` in `Paused` resumes via `gamepad::gamepad_pause_toggle`)
/// - `B`: resume from `Paused`, back to the main menu from `Loadout`,
///   `ContractsFeats`, `GameOver` and `Victory` (Settings handles its own `B`)
pub fn pad_menu_action(state: AppState, start: bool, back: bool) -> Option<MenuAction> {
    use MenuAction::*;
    match state {
        AppState::MainMenu | AppState::Loadout | AppState::ContractsFeats if start => Some(StartRun),
        AppState::Paused if back => Some(Continue),
        AppState::Loadout | AppState::ContractsFeats | AppState::GameOver | AppState::Victory if back => Some(BackToMenu),
        _ => None,
    }
}

/// App Store card for offer `n` (0..3).
#[derive(Component)]
struct UpgradeCard(usize);

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((UiKitPlugin, MetaUiPlugin, crate::perf::PerfOverlayPlugin))
            .add_systems(OnEnter(AppState::MainMenu), spawn_main_menu)
            .add_systems(OnEnter(AppState::Playing), spawn_hud)
            .add_systems(OnEnter(AppState::Paused), spawn_pause_menu)
            .add_systems(OnEnter(AppState::UpgradeSelect), spawn_app_store.after(draft_upgrades))
            // End screens read the run summary the profile recorder just stored.
            .add_systems(OnEnter(AppState::GameOver), spawn_game_over.after(record_game_over))
            .add_systems(OnEnter(AppState::Victory), spawn_victory.after(record_victory))
            .add_systems(Update, (menu_buttons, menu_keys).run_if(not(in_state(AppState::Playing))))
            .add_systems(Update, seed_entry_keys.after(menu_keys).after(menu_buttons).run_if(in_state(AppState::MainMenu)))
            .add_systems(Update, refresh_seed_ui.after(seed_entry_keys).run_if(not(in_state(AppState::Playing))))
            .add_systems(Update, upgrade_cards.run_if(in_state(AppState::UpgradeSelect)))
            .init_resource::<VelGauge>()
            .add_systems(
                Update,
                (
                    track_vel_gauge.before(update_hud).before(update_hud_boxes),
                    update_hud,
                    update_hud_boxes,
                    refresh_hud_crosshair,
                    update_split_plate,
                    pause_input,
                    debug_skip_floor,
                )
                    .in_set(GameSet::Ui),
            );
    }
}

pub fn text(s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (Text::new(s), TextFont::from_font_size(size), TextColor(color))
}

/// Centered row of punk switches. A [`ButtonStyle::HeroAcid`] switch is the
/// row's call to action and is set ~1.5x larger than its neighbours.
pub fn button_row(commands: &mut Commands, parent: Entity, buttons: &[(&str, MenuAction, ButtonStyle)], font: f32, top: f32) {
    let row = commands
        .spawn((
            ChildOf(parent),
            Node {
                margin: UiRect::top(Val::Px(top)),
                column_gap: Val::Px(20.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .id();
    for (label, action, style) in buttons {
        commands.spawn((ChildOf(row), punk_button(label, *action, button_font(*style, font), *style)));
    }
}

/// Font of a switch in a row whose secondary switches use `font`.
pub fn button_font(style: ButtonStyle, font: f32) -> f32 {
    if style == ButtonStyle::HeroAcid { (font * 1.5).round() } else { font }
}

/// Full-width row that centers its children (keeps stencil titles and tags
/// from stretching inside `Stretch` columns).
pub fn centered(commands: &mut Commands, parent: Entity) -> Entity {
    commands.spawn((ChildOf(parent), Node { width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() })).id()
}

/// A stamped tag wrapped in a node with a vertical margin.
fn spaced_tag(commands: &mut Commands, parent: Entity, label: &str, (bg, fg): (Color, Color), font: f32, margin: f32) {
    commands.spawn((ChildOf(parent), Node { margin: UiRect::vertical(Val::Px(margin)), ..default() }, children![stamped_tag(label, bg, fg, font)]));
}

/// Firmware stamp of the title lockup.
pub const OS_STAMP: &str = "UAC-OS v0.7";

/// One field-manual binding: the keycaps and what they do.
pub type KeyBind = (&'static [&'static str], &'static str);

/// Field-manual rows: `(category, bindings)`.
pub const CONTROLS: [(&str, &[KeyBind]); 4] = [
    ("MOVE", &[(&["WASD"], "move"), (&["MOUSE"], "look"), (&["SPACE"], "jump / hold to bhop"), (&["SHIFT"], "airplane-mode dash")]),
    ("FIGHT", &[(&["LMB"], "fire"), (&["1", "2", "3", "TAB", "WHEEL"], "shotgun / plasma / mortar")]),
    ("PHONE", &[(&["RMB"], "ping scan (1+ bar)"), (&["Q"], "5G overload EMP (3+ bars)"), (&["E"], "SOS uplink at the tower")]),
    ("SYSTEM", &[(&["CLICK"], "capture mouse"), (&["ESC", "P"], "pause"), (&["F9"], "debug: skip floor")]),
];

fn spawn_main_menu(
    mut commands: Commands,
    store: Res<ProfileStore>,
    seed: Res<CustomSeedState>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
) {
    let tex = tex.as_deref();
    let msaa = msaa.map(|m| m.0).unwrap_or(bevy::render::view::Msaa::Sample2);
    // Menu needs its own camera: the player camera only exists in a run.
    commands.spawn((
        DespawnOnExit(AppState::MainMenu),
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::Custom(COLOR_BG), ..default() },
        msaa,
    ));
    let root = punk_screen_root(&mut commands, AppState::MainMenu, tex, INK_BLACK, 1.0);
    screen_rails(&mut commands, root, tex, 12.0, false);

    // One poster column, exactly as wide as the wordmark: every block below
    // the title shares its left and right edges.
    let column = commands
        .spawn((ChildOf(root), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, row_gap: Val::Px(6.0), ..default() }))
        .id();

    // Hero lockup: firmware stamp + a dying 1-bar signal above the wordmark,
    // the caution tape tag + genre line below it.
    let lockup = commands
        .spawn((ChildOf(column), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, row_gap: Val::Px(4.0), ..default() }))
        .id();
    let stamp_row = commands
        .spawn((ChildOf(lockup), Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::FlexEnd, ..default() }))
        .id();
    commands.spawn((
        ChildOf(stamp_row),
        Node { column_gap: Val::Px(8.0), align_items: AlignItems::Center, ..default() },
        children![stamped_tag(OS_STAMP, STEEL_BORDER, BONE_TEXT, 12.0), text("// BUNKER 7 // FIELD TERMINAL", 12.0, ASH_DIM)],
    ));
    let weak = commands.spawn((ChildOf(stamp_row), Node { column_gap: Val::Px(10.0), align_items: AlignItems::FlexEnd, ..default() })).id();
    commands.spawn((ChildOf(weak), text("SIGNAL 1/5", 12.0, PUNK_RED)));
    signal_bars(&mut commands, weak, 1, PUNK_RED, 6.0);
    hero_title(&mut commands, lockup, "SEGNALE DEBOLE", 84.0, BONE_TEXT, PUNK_RED);
    commands.spawn((
        ChildOf(lockup),
        Node { margin: UiRect::top(Val::Px(2.0)), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() },
        children![
            stamped_tag("// WEAK SIGNAL // MARS BUNKER 7 //", HAZARD_YELLOW, INK_BLACK, 16.0),
            text("ROGUELIKE BOOMER SHOOTER  //  5 FLOORS  //  1 PHONE", 12.0, ASH_DIM),
        ],
    ));

    // Briefing on a spray-red margin rule; the objective line is the loud one.
    commands.spawn((
        ChildOf(column),
        Node { padding: UiRect::left(Val::Px(14.0)), border: UiRect::left(Val::Px(3.0)), flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() },
        BorderColor::all(PUNK_RED),
        children![
            text(
                "Mars Research Bunker 7 has gone dark. Hell came through the server racks.\n\
                 Your left hand grips a cracked phone hunting for a single bar.\n\
                 Your right hand grips a pump shotgun.",
                16.0,
                BONE_TEXT.with_alpha(0.82)
            ),
            text("> Find the 5-bar Signal Tower, destroy the Demon Jammers, hold the SOS uplink. Get out.", 16.0, HAZARD_YELLOW),
        ],
    ));

    // Field manual: keycap chips on a brushed-steel plate.
    let (manual, manual_head) = section_crate(&mut commands, column, "FIELD MANUAL // CONTROLS", ASH_DIM, Node::default());
    if let Some(t) = tex {
        commands.entity(manual).insert(tiled_image(t.plate_bg.clone(), Color::WHITE.with_alpha(0.4), 2.0));
    }
    commands.spawn((ChildOf(manual_head), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(manual_head), text("REV 07-B", 11.0, ASH_DIM.with_alpha(0.75))));
    for (category, binds) in CONTROLS {
        let row = commands.spawn((ChildOf(manual), Node { align_items: AlignItems::Center, column_gap: Val::Px(16.0), ..default() })).id();
        commands.spawn((ChildOf(row), Node { width: Val::Px(52.0), flex_shrink: 0.0, ..default() }, children![text(category, 11.0, HAZARD_YELLOW)]));
        for (keys, what) in binds {
            let bind = commands.spawn((ChildOf(row), Node { align_items: AlignItems::Center, column_gap: Val::Px(4.0), ..default() })).id();
            for key in *keys {
                commands.spawn((ChildOf(bind), keycap(key, 12.0)));
            }
            commands.spawn((ChildOf(bind), Node { margin: UiRect::left(Val::Px(3.0)), ..default() }, children![text(*what, 13.0, BONE_TEXT.with_alpha(0.72))]));
        }
    }

    // Active loadout telemetry crate (+ JAM DEMO caution tag in its header).
    let (loadout, loadout_head) = section_crate(&mut commands, column, "ACTIVE LOADOUT", ARC_CYAN, Node::default());
    commands.spawn((ChildOf(loadout_head), Node { flex_grow: 1.0, ..default() }));
    if store.profile.jam_demo_unlock_all {
        commands.spawn((ChildOf(loadout_head), MetaBox::JamPill, stamped_tag("JAM DEMO: ALL UNLOCKED", HAZARD_YELLOW, INK_BLACK, 11.0)));
    }
    commands.spawn((ChildOf(loadout), text(loadout_summary_line(&store.profile), 14.0, ARC_CYAN)));

    // PLAY is the slab that fills the column; the two side doors stay small.
    let actions = commands
        .spawn((ChildOf(column), Node { margin: UiRect::top(Val::Px(3.0)), column_gap: Val::Px(20.0), align_items: AlignItems::Center, ..default() }))
        .id();
    commands.spawn((
        ChildOf(actions),
        Node { flex_grow: 1.0, flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, ..default() },
        children![(punk_button("PLAY  [ENTER]", MenuAction::StartRun, 30.0, ButtonStyle::HeroAcid), DefaultFocus)],
    ));
    commands.spawn((ChildOf(actions), punk_button("LOADOUT  [L]", MenuAction::OpenLoadout, 19.0, ButtonStyle::HazardAmber)));
    commands.spawn((ChildOf(actions), punk_button("CONTRACTS & FEATS  [C]", MenuAction::OpenContractsFeats, 19.0, ButtonStyle::DangerRed)));
    commands.spawn((ChildOf(actions), punk_button("SETTINGS  [O]", MenuAction::OpenSettings, 19.0, ButtonStyle::ScrapSteel)));

    // Alternate modes: one compact strip of two doors, each with its live subtitle.
    let modes = commands.spawn((ChildOf(column), Node { column_gap: Val::Px(14.0), align_items: AlignItems::Center, ..default() })).id();
    let today = DailyDate::today_utc();
    let (daily_sub, trial_sub) = mode_subtitles(&store.profile, today);
    for (label, action, style, sub, color) in [
        ("DAILY RUN  [D]", MenuAction::StartDaily, ButtonStyle::HazardAmber, daily_sub, HAZARD_YELLOW),
        ("TIME TRIAL  [T]", MenuAction::StartTimeTrial, ButtonStyle::PrimaryAcid, trial_sub, ARC_CYAN),
    ] {
        commands.spawn((ChildOf(modes), punk_button(label, action, 16.0, style)));
        commands.spawn((
            ChildOf(modes),
            Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() },
            children![text(sub, 12.0, color.with_alpha(0.9))],
        ));
    }
    commands.spawn((ChildOf(modes), punk_button("BESTIARY & ARMORY  [B]", MenuAction::OpenCompendium, 16.0, ButtonStyle::DangerRed)));
    spawn_input_hint(&mut commands, column);

    // Run seed: compact selector strip (armed code or RANDOM) + RND / CLR.
    let seed_row = commands
        .spawn((
            ChildOf(column),
            Node {
                column_gap: Val::Px(10.0),
                align_items: AlignItems::Center,
                padding: UiRect::axes(Val::Px(10.0), Val::Px(3.0)),
                border: plate_rect((3.0, 1.0, 1.0, 1.0)),
                ..default()
            },
            BorderColor { left: ARC_CYAN, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.62)),
        ))
        .id();
    commands.spawn((ChildOf(seed_row), text("RUN SEED", 11.0, ASH_DIM)));
    commands.spawn((ChildOf(seed_row), punk_button(&seed_button_label(&seed), MenuAction::ToggleSeedEdit, 14.0, ButtonStyle::PrimaryAcid)));
    commands.spawn((ChildOf(seed_row), punk_button("RND", MenuAction::RollSeed, 14.0, ButtonStyle::ScrapSteel)));
    commands.spawn((ChildOf(seed_row), punk_button("CLR", MenuAction::ClearSeed, 14.0, ButtonStyle::ScrapSteel)));
    commands.spawn((ChildOf(seed_row), SeedStatusText, text(seed_status_line(&seed), 12.0, ARC_CYAN.with_alpha(0.85))));
}

/// Live seed status line of the main menu.
#[derive(Component)]
pub struct SeedStatusText;

/// Label of the main menu SEED switch.
pub fn seed_button_label(seed: &CustomSeedState) -> String {
    format!("SEED: {}  [S]", seed.readout())
}

fn seed_status_line(seed: &CustomSeedState) -> String {
    seed.status_msg.clone().unwrap_or_else(|| "SAME CODE = SAME BUNKER // TYPE A WORD OR ROLL ONE".to_string())
}

/// Label of the pause / end screen SHARE SEED switch (confirms once armed).
pub fn share_button_label(run: &RunStats, seed: &CustomSeedState) -> String {
    match shareable_code(run) {
        Some(code) if seed.code == code && !seed.editing => format!("SEED {code} ARMED  [S]"),
        _ => "SHARE SEED  [S]".to_string(),
    }
}

/// Inline seed editor on the main menu (runs after [`menu_keys`], which is
/// muted while editing): `S` opens it, `A-Z` / `0-9` / `-` type, `Backspace`
/// deletes, `Enter` locks, `Esc` closes. Consumes ordered `KeyboardInput`
/// messages when present so fast same-frame typing preserves character order.
pub fn seed_entry_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut key_events: MessageReader<bevy::input::keyboard::KeyboardInput>,
    mut seed: ResMut<CustomSeedState>,
) {
    let pressed_ordered: Vec<KeyCode> = key_events
        .read()
        .filter(|e| e.state == bevy::input::ButtonState::Pressed && !e.repeat)
        .map(|e| e.key_code)
        .collect();
    if !seed.editing {
        if pressed_ordered.contains(&KeyCode::KeyS) || (pressed_ordered.is_empty() && keys.just_pressed(KeyCode::KeyS)) {
            seed.toggle_edit();
        }
        return;
    }
    if !pressed_ordered.is_empty() {
        for k in pressed_ordered {
            match k {
                KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Escape => {
                    seed.lock();
                    #[cfg(target_arch = "wasm32")]
                    crate::compendium::sync_url_hash_seed(&seed.code);
                    return;
                }
                KeyCode::Backspace => seed.backspace(),
                other => {
                    if let Some(c) = seed_key_char(other) {
                        seed.push_char(c);
                    }
                }
            }
        }
        return;
    }
    if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter, KeyCode::Escape]) {
        seed.lock();
        #[cfg(target_arch = "wasm32")]
        crate::compendium::sync_url_hash_seed(&seed.code);
        return;
    }
    if keys.just_pressed(KeyCode::Backspace) {
        seed.backspace();
    }
    for k in keys.get_just_pressed() {
        if let Some(c) = seed_key_char(*k) {
            seed.push_char(c);
        }
    }
}

/// Keep the SEED switch label, the seed status line and the SHARE SEED
/// switch in sync with [`CustomSeedState`].
fn refresh_seed_ui(
    seed: Res<CustomSeedState>,
    run: Res<RunStats>,
    buttons: Query<(&MenuAction, &Children)>,
    mut status: Query<&mut Text, With<SeedStatusText>>,
    mut labels: Query<&mut Text, Without<SeedStatusText>>,
) {
    if !seed.is_changed() {
        return;
    }
    for (action, children) in &buttons {
        let want = match action {
            MenuAction::ToggleSeedEdit => seed_button_label(&seed),
            MenuAction::ShareSeed => share_button_label(&run, &seed),
            _ => continue,
        };
        for c in children {
            if let Ok(mut t) = labels.get_mut(*c)
                && t.0 != want
            {
                t.0 = want.clone();
            }
        }
    }
    let line = seed_status_line(&seed);
    for mut t in &mut status {
        if t.0 != line {
            t.0 = line.clone();
        }
    }
}

/// Main menu subtitles of the DAILY RUN and TIME TRIAL doors.
pub fn mode_subtitles(profile: &crate::meta::PlayerProfile, today: DailyDate) -> (String, String) {
    let rules = daily_rules_line(&daily_loadout(today));
    let best = match daily_record(&profile.daily_records, today) {
        Some(r) if r.victory => format!("BEST {}", r.best_time_secs.map_or_else(|| "CLEAR".to_string(), format_split_time)),
        Some(r) => format!("BEST FL {}", r.best_floor.max(1)),
        None => "NOT PLAYED".to_string(),
    };
    let daily = format!("{} // {rules} // {best}", today.format_short());
    let trial = match profile.time_trial_best_secs {
        Some(t) => format!("COURSE PB {}", format_split_time(t)),
        None => "3 CP + TOWER // NO PB YET".to_string(),
    };
    (daily, trial)
}

// ---------------------------------------------------------------------------
// App Store (upgrade select)
// ---------------------------------------------------------------------------

const UPGRADE_CARD_BORDER: (f32, f32, f32, f32) = (5.0, 1.0, 1.0, 4.0);
/// Near-black phosphor well of the NEXT RANK terminal strip.
const COLOR_PHOSPHOR_WELL: Color = Color::srgb(0.012, 0.07, 0.035);

fn upgrade_card_border(hovered: bool) -> BorderColor {
    if hovered { plate_border(ACID_GREEN, BONE_TEXT) } else { plate_border(HAZARD_YELLOW, STEEL_BORDER) }
}

/// `SLOT 01`..`SLOT 03` header label of offer `i` (0-based).
pub fn slot_label(i: usize) -> String {
    format!("SLOT {:02}", i + 1)
}

/// "APP STORE // FIRMWARE": three bolted upgrade cards (click or 1/2/3).
pub fn spawn_app_store(
    mut commands: Commands,
    run: Res<RunStats>,
    mods: Res<PlayerModifiers>,
    choices: Res<UpgradeChoices>,
    tex: Option<Res<UiKitTextures>>,
) {
    let tex = tex.as_deref();
    let root = punk_screen_root(&mut commands, AppState::UpgradeSelect, tex, COLOR_STORE_BG, 0.85);
    let phone = terminal_chassis(&mut commands, root, tex, 1120.0, false);
    commands.spawn((
        ChildOf(phone),
        Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, ..default() },
        children![
            text("UAC-PHONE 5G // HACKED FIRMWARE", 13.0, ACID_GREEN),
            text("||||| -55 dBm   SOS SENT", 13.0, ACID_GREEN),
        ],
    ));
    let title = centered(&mut commands, phone);
    stencil_title(&mut commands, title, "APP STORE // FIRMWARE", 44.0, BONE_TEXT, PUNK_RED);
    let sub = centered(&mut commands, phone);
    commands.spawn((
        ChildOf(sub),
        text(format!("Full 5-bar uplink. One download slot unlocked.  Next: SECTOR {} / {}", run.floor, FINAL_FLOOR), 17.0, BONE_TEXT),
    ));
    let row = commands
        .spawn((
            ChildOf(phone),
            Node {
                margin: UiRect { top: Val::Px(8.0), bottom: Val::Px(12.0), ..default() },
                column_gap: Val::Px(24.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Stretch,
                ..default()
            },
        ))
        .id();
    for (i, id) in choices.0.iter().enumerate() {
        let owned = run.upgrades.iter().filter(|u| *u == id).count();
        let (tag, tag_bg) = if owned > 0 { (format!("INSTALLED x{owned} - UPDATE"), HAZARD_YELLOW) } else { ("NEW".to_string(), ACID_GREEN) };
        // The card: a bolted plate on a deep ink slab.
        let card = commands
            .spawn((
                ChildOf(row),
                UpgradeCard(i),
                Node { width: Val::Px(336.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, border: plate_rect(UPGRADE_CARD_BORDER), ..default() },
                upgrade_card_border(false),
                BackgroundColor(COLOR_CARD),
                hard_shadow(8.0),
            ))
            .id();
        if let Some(t) = tex {
            commands.entity(card).insert(tiled_image(t.noise_strip.clone(), Color::WHITE.with_alpha(0.05), 1.0));
        }
        // Slot header bar: stamped slot plate | install state.
        commands.spawn((
            ChildOf(card),
            Node {
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Stretch,
                border: UiRect::bottom(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(HAZARD_YELLOW),
            BackgroundColor(INK_BLACK),
            children![
                (
                    Node { padding: UiRect::axes(Val::Px(12.0), Val::Px(3.0)), align_items: AlignItems::Center, ..default() },
                    BackgroundColor(HAZARD_YELLOW),
                    children![text(slot_label(i), 15.0, INK_BLACK)],
                ),
                (
                    Node { padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)), column_gap: Val::Px(8.0), align_items: AlignItems::Center, ..default() },
                    children![text(format!("KEY [{}]", i + 1), 11.0, ASH_DIM), stamped_tag(&tag, tag_bg, INK_BLACK, 12.0)],
                ),
            ],
        ));
        let body = commands
            .spawn((
                ChildOf(card),
                Node {
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect { left: Val::Px(14.0), right: Val::Px(14.0), top: Val::Px(12.0), bottom: Val::Px(14.0) },
                    row_gap: Val::Px(9.0),
                    ..default()
                },
            ))
            .id();
        // Recessed icon bay: vent slits either side of a stepped frame
        // (steel lip -> ink gap -> caution plate).
        let bay = commands
            .spawn((
                ChildOf(body),
                recessed_bezel(Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::vertical(Val::Px(8.0)),
                    column_gap: Val::Px(16.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                }),
            ))
            .id();
        let vents = |commands: &mut Commands| {
            let v = commands.spawn((ChildOf(bay), Node { column_gap: Val::Px(4.0), ..default() })).id();
            for _ in 0..4 {
                commands.spawn((ChildOf(v), Node { width: Val::Px(3.0), height: Val::Px(30.0), ..default() }, BackgroundColor(STEEL_BORDER.with_alpha(0.7))));
            }
        };
        vents(&mut commands);
        commands.spawn((
            ChildOf(bay),
            Node { padding: UiRect::all(Val::Px(3.0)), border: UiRect::all(Val::Px(2.0)), ..default() },
            BorderColor { left: STEEL_LIGHT, top: STEEL_LIGHT, right: STEEL_BORDER, bottom: STEEL_BORDER },
            BackgroundColor(INK_BLACK),
            children![(
                Node {
                    width: Val::Px(104.0),
                    height: Val::Px(50.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    border: plate_rect((0.0, 0.0, 0.0, 4.0)),
                    ..default()
                },
                BorderColor::all(HAZARD_DARK.mix(&HAZARD_YELLOW, 0.35)),
                BackgroundColor(HAZARD_YELLOW),
                children![text(id.icon(), 24.0, INK_BLACK)],
            )],
        ));
        vents(&mut commands);
        // Name over a short caution dash, then the pitch.
        commands.spawn((
            ChildOf(body),
            Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(5.0), margin: UiRect::top(Val::Px(2.0)), ..default() },
            children![
                text(id.name().to_uppercase(), 22.0, BONE_TEXT),
                (Node { width: Val::Px(44.0), height: Val::Px(3.0), ..default() }, BackgroundColor(HAZARD_YELLOW)),
            ],
        ));
        commands.spawn((
            ChildOf(body),
            Node { width: Val::Percent(100.0), min_height: Val::Px(57.0), ..default() },
            children![text(id.description(), 15.0, ASH_DIM.mix(&BONE_TEXT, 0.3))],
        ));
        commands.spawn((ChildOf(body), Node { flex_grow: 1.0, ..default() }));
        // NEXT RANK > phosphor terminal strip.
        commands.spawn((
            ChildOf(body),
            Node { width: Val::Percent(100.0), flex_direction: FlexDirection::Column, border: plate_rect((4.0, 1.0, 1.0, 2.0)), ..default() },
            BorderColor { left: ACID_GREEN, top: ACID_GREEN.with_alpha(0.4), right: ACID_GREEN.with_alpha(0.4), bottom: INK_BLACK },
            BackgroundColor(COLOR_PHOSPHOR_WELL),
            children![
                (
                    Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() },
                    BackgroundColor(ACID_GREEN.with_alpha(0.14)),
                    children![
                        (
                            Node { padding: UiRect::axes(Val::Px(7.0), Val::Px(1.0)), ..default() },
                            BackgroundColor(ACID_GREEN),
                            children![text("NEXT RANK >", 11.0, INK_BLACK)],
                        ),
                        (Node { padding: UiRect::right(Val::Px(7.0)), ..default() }, children![text(if owned > 0 { "PATCH" } else { "v1.0" }, 10.0, ACID_GREEN.with_alpha(0.7))]),
                    ],
                ),
                (
                    Node { padding: UiRect { left: Val::Px(8.0), right: Val::Px(6.0), top: Val::Px(5.0), bottom: Val::Px(6.0) }, ..default() },
                    children![text(next_rank_preview(*id, &mods), 15.0, ACID_GREEN)],
                ),
            ],
        ));
        commands.spawn((
            ChildOf(body),
            Node {
                width: Val::Percent(100.0),
                margin: UiRect::top(Val::Px(3.0)),
                padding: UiRect::vertical(Val::Px(6.0)),
                justify_content: JustifyContent::Center,
                border: plate_rect((0.0, 0.0, 2.0, 4.0)),
                ..default()
            },
            BorderColor { top: ACID_GREEN.mix(&BONE_TEXT, 0.6), bottom: INK_BLACK, left: INK_BLACK, right: INK_BLACK },
            BackgroundColor(ACID_GREEN),
            hard_shadow(4.0),
            children![text(format!("INSTALL  [{}]", i + 1), 19.0, INK_BLACK)],
        ));
    }
    let foot = centered(&mut commands, phone);
    commands.spawn((ChildOf(foot), text(installed_list(&run), 14.0, ASH_DIM)));
    let hint = centered(&mut commands, phone);
    spaced_tag(&mut commands, hint, "CLICK A CARD OR PRESS 1 / 2 / 3  //  PAD: LEFT / RIGHT + A", (HAZARD_YELLOW, INK_BLACK), 13.0, 0.0);
}

fn installed_list(run: &RunStats) -> String {
    if run.upgrades.is_empty() {
        "INSTALLED APPS: none".into()
    } else {
        let names: Vec<&str> = run.upgrades.iter().map(|u| u.name()).collect();
        format!("INSTALLED APPS: {}", names.join(", "))
    }
}

#[allow(clippy::too_many_arguments)]
fn upgrade_cards(
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    choices: Res<UpgradeChoices>,
    mut cards: Query<(&ComputedNode, &UiGlobalTransform, &UpgradeCard, &mut BackgroundColor, &mut BorderColor)>,
    mut mods: ResMut<PlayerModifiers>,
    mut run: ResMut<RunStats>,
    mut players: Query<&mut Player>,
    mut sfx: ResMut<SfxQueue>,
    mut next: ResMut<NextState<AppState>>,
    (pad, pad_card): (Res<GamepadInput>, Res<UpgradeFocus>),
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    let mut chosen = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3].iter().position(|k| keys.just_pressed(*k));
    for (node, tf, card, mut bg, mut border) in &mut cards {
        let pointer_hovered = cursor.is_some_and(|c| node.contains_point(*tf, c));
        // Controller navigation owns the highlight until the mouse moves, but
        // mouse releases always activate the card under the actual pointer.
        let lit = if pad.active { card.0 == pad_card.0 } else { pointer_hovered };
        bg.0 = if lit { COLOR_CARD_HOVER } else { COLOR_CARD };
        let want = upgrade_card_border(lit);
        if *border != want {
            *border = want;
        }
        if pointer_hovered && mouse.just_released(MouseButton::Left) {
            chosen = Some(card.0);
        }
    }
    if let Some(i) = chosen {
        let mut player = players.single_mut().ok();
        if pick_upgrade(i, &choices, &mut mods, player.as_deref_mut(), &mut run, &mut next).is_some() {
            sfx.play(SfxEvent::UpgradeSelect);
        }
    }
}

// ---------------------------------------------------------------------------
// End screens
// ---------------------------------------------------------------------------

fn summary(run: &RunStats) -> String {
    let t = run.time_elapsed as u32;
    format!(
        "{}  |  Kills {}  |  Jammers destroyed {}  |  Time {:02}:{:02}",
        floor_label(run),
        run.kills,
        run.jammers_destroyed,
        t / 60,
        t % 60
    )
}

/// `FLOOR 3/5`, or `ENDLESS FL 8` once past Victory.
pub fn floor_label(run: &RunStats) -> String {
    if run.endless { format!("ENDLESS FL {}", run.floor) } else { format!("FLOOR {}/{}", run.floor, FINAL_FLOOR) }
}

/// End-screen movement telemetry line.
pub fn movement_line(run: &RunStats) -> String {
    format!("PEAK SPEED {:.1} M/S  |  MAX BHOP CHAIN x{}", run.peak_speed.max(0.0), run.max_bhop_chain)
}

/// Endless Overdrive accent (hot magenta: distinct from every floor theme tag).
pub const COLOR_ENDLESS: Color = Color::srgb(1.0, 0.25, 0.85);

/// Look of an end screen.
struct EndLook {
    title: &'static str,
    title_color: Color,
    /// Slab copy between the title and its ink shadow.
    title_slab: Color,
    flavor: &'static str,
    /// Carrier stamp next to the signal bars.
    stamp: &'static str,
    /// Lit signal bars (0 = dead, 5 = full uplink).
    bars: u8,
    bg: Color,
    /// Multiplied into the CRT backdrop (alpha = overlay strength).
    backdrop: Color,
    /// Title band fill and its RF static tint.
    band: (Color, Color),
    accent: Color,
    tape: Tape,
    /// Top-right RF link stamp.
    link: &'static str,
    /// Color of the installed-apps line.
    apps: Color,
    buttons: [(&'static str, MenuAction, ButtonStyle); 2],
}

/// NO SERVICE: bleak crimson dead-zone static under danger tape. No green.
fn no_service_look() -> EndLook {
    EndLook {
        title: "NO SERVICE",
        title_color: PUNK_RED,
        title_slab: Color::srgb(0.3, 0.03, 0.02),
        flavor: "Connection lost. The bunker keeps your body.",
        stamp: "0 BARS  //  CARRIER LOST  //  SOS NOT DELIVERED",
        bars: 0,
        bg: Color::srgb(0.05, 0.008, 0.008),
        backdrop: Color::srgba(1.0, 0.36, 0.3, 0.9),
        band: (Color::srgba(0.0, 0.0, 0.0, 0.5), Color::srgba(1.0, 0.16, 0.1, 0.2)),
        accent: PUNK_RED,
        tape: Tape::Danger,
        link: "RF-LINK: DEAD",
        apps: ASH_DIM.mix(&BONE_TEXT, 0.35),
        buttons: [("MAIN MENU  [ENTER]", MenuAction::BackToMenu, ButtonStyle::DangerRed), ("LOADOUT  [L]", MenuAction::OpenLoadout, ButtonStyle::HazardAmber)],
    }
}

/// FULL SIGNAL: electric acid-phosphor uplink under signal tape. No red.
fn full_signal_look() -> EndLook {
    EndLook {
        title: "FULL SIGNAL",
        title_color: ACID_GREEN,
        title_slab: Color::srgb(0.02, 0.24, 0.1),
        flavor: "SOS received. Orbital extraction confirmed. Bunker 7 sealed.",
        stamp: "5 BARS  //  UPLINK LOCKED  //  SOS DELIVERED",
        bars: 5,
        bg: Color::srgb(0.006, 0.045, 0.02),
        backdrop: Color::srgba(0.42, 1.0, 0.58, 0.9),
        band: (ACID_GREEN.with_alpha(0.1), Color::srgba(0.3, 1.0, 0.5, 0.12)),
        accent: ACID_GREEN,
        tape: Tape::Signal,
        link: "RF-LINK: LOCKED",
        apps: ACID_GREEN,
        buttons: [("MAIN MENU  [ENTER]", MenuAction::BackToMenu, ButtonStyle::HeroAcid), ("LOADOUT  [L]", MenuAction::OpenLoadout, ButtonStyle::HazardAmber)],
    }
}

/// Shared GAME OVER / VICTORY layout: tape rails, a full-width static band
/// carrying the signal bars + stencil title, flavor, the recessed run readout,
/// the RUN UNLOCKS crate and the action switches.
#[allow(clippy::too_many_arguments)]
fn spawn_end_screen(
    commands: &mut Commands,
    state: AppState,
    look: EndLook,
    endless_cta: bool,
    run: &RunStats,
    loadout: &RunLoadout,
    (store, seed): (&ProfileStore, &CustomSeedState),
    tex: Option<&UiKitTextures>,
) {
    // Victory stacks the Endless switch + its subtitle under a full unlock
    // crate: it runs a tighter title and button row so the stack clears both
    // tape rails at 720p.
    let compact = endless_cta;
    // The floor-splits line (+ Daily badge) costs a readout row: tighten the
    // title so the stack still clears both tape rails at 720p.
    let show_splits = split_timer_visible(&store.profile.settings, run.mode);
    let root = punk_screen_root_tinted(commands, state, tex, look.bg, look.backdrop, (look.link, look.accent));
    screen_rails(commands, root, tex, 16.0, look.tape);

    // Title band: RF static edge to edge, hairline accent rules.
    let band = commands
        .spawn((
            ChildOf(root),
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                padding: UiRect { top: Val::Px(6.0), bottom: Val::Px(8.0), ..default() },
                border: UiRect::vertical(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(look.accent.with_alpha(0.55)),
            BackgroundColor(look.band.0),
        ))
        .id();
    if let Some(t) = tex {
        commands.entity(band).insert(tiled_image(t.noise_strip.clone(), look.band.1, 2.0));
    }
    let stamp = commands.spawn((ChildOf(band), Node { column_gap: Val::Px(14.0), align_items: AlignItems::FlexEnd, ..default() })).id();
    signal_bars(commands, stamp, look.bars, look.accent, 8.0);
    commands.spawn((ChildOf(stamp), text(look.stamp, 13.0, look.accent)));
    let title_font = match (compact, show_splits) {
        (true, true) => 62.0,
        (true, false) => 76.0,
        (false, true) => 76.0,
        (false, false) => 84.0,
    };
    stencil_title_layers(commands, band, look.title, title_font, look.title_color, look.title_color, &[(8.0, Color::BLACK), (4.0, look.title_slab)]);

    commands.spawn((ChildOf(root), text(look.flavor, if show_splits { 17.0 } else { 19.0 }, BONE_TEXT)));
    let readout = commands
        .spawn((
            ChildOf(root),
            Node {
                max_width: Val::Px(1000.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(2.0),
                padding: UiRect::axes(Val::Px(22.0), Val::Px(6.0)),
                border: plate_rect((4.0, 2.0, 2.0, 2.0)),
                ..default()
            },
            BorderColor { left: look.accent, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.74)),
        ))
        .id();
    commands.spawn((ChildOf(readout), text(summary(run), 18.0, BONE_TEXT)));
    commands.spawn((ChildOf(readout), text(movement_line(run), 14.0, ARC_CYAN)));
    commands.spawn((ChildOf(readout), text(installed_list(run), 14.0, look.apps)));
    let loadout_line = with_seed(end_loadout_line(loadout), run);
    match run.mode.daily_date() {
        Some(date) => {
            let row = commands.spawn((ChildOf(readout), Node { column_gap: Val::Px(8.0), align_items: AlignItems::Center, ..default() })).id();
            commands.spawn((ChildOf(row), stamped_tag(&format!("DAILY {}", date.format_iso()), HAZARD_YELLOW, INK_BLACK, 11.0)));
            commands.spawn((ChildOf(row), text(loadout_line, 13.0, ASH_DIM)));
        }
        None => {
            commands.spawn((ChildOf(readout), text(loadout_line, 13.0, ASH_DIM)));
        }
    }
    if show_splits {
        commands.spawn((ChildOf(readout), text(floor_splits_line(&run.floor_splits), 13.0, ACID_GREEN.with_alpha(0.85))));
    }

    let summary = store.last_run_summary.clone().unwrap_or_default();
    let (panel, header) = section_crate(
        commands,
        root,
        "RUN UNLOCKS",
        look.accent,
        Node { max_width: Val::Px(1000.0), min_width: Val::Px(600.0), align_items: AlignItems::Center, ..default() },
    );
    commands.entity(panel).insert((MetaBox::UnlockPanel, hard_shadow(6.0)));
    commands.spawn((ChildOf(header), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(header), text(if summary.is_empty() { "NO DATA" } else { "UNLOCKED" }, 11.0, ASH_DIM)));
    if let Some(reason) = &summary.skipped_reason {
        commands.spawn((ChildOf(panel), text(reason.clone(), 15.0, HAZARD_YELLOW)));
    } else if summary.is_empty() {
        commands.spawn((ChildOf(panel), Node { margin: UiRect::vertical(Val::Px(4.0)), ..default() }, children![text(NO_UNLOCKS_LINE, 15.0, ASH_DIM)]));
    } else {
        commands.spawn((
            ChildOf(panel),
            text(
                format!("NEW UNLOCKS THIS RUN  //  {} FEAT{}", summary.new_feats.len(), if summary.new_feats.len() == 1 { "" } else { "S" }),
                18.0,
                COLOR_GOLD,
            ),
        ));
        let feats = commands
            .spawn((
                ChildOf(panel),
                Node { flex_wrap: FlexWrap::Wrap, justify_content: JustifyContent::Center, column_gap: Val::Px(8.0), row_gap: Val::Px(5.0), ..default() },
            ))
            .id();
        for f in &summary.new_feats {
            commands.spawn((ChildOf(feats), stamped_tag(&format!("[DONE] {}", f.name()), COLOR_GOLD, INK_BLACK, 14.0)));
        }
        // One tight block, so the reward lines read as a list.
        let rewards = commands
            .spawn((ChildOf(panel), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(2.0), ..default() }))
            .id();
        for line in unlock_reward_lines(&summary) {
            commands.spawn((ChildOf(rewards), text(line, 15.0, ARC_CYAN)));
        }
    }
    if let Some(err) = &store.last_save_error {
        commands.spawn((ChildOf(panel), text(format!("SAVE PENDING ({err}) - will retry at menu"), 13.0, PUNK_RED)));
    }
    if endless_cta {
        // The loud alternative to cashing out: keep the run (HP, armor, ammo,
        // apps) and descend into Endless Overdrive.
        let cta = commands
            .spawn((
                ChildOf(root),
                Node {
                    margin: UiRect::top(Val::Px(4.0)),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(4.0),
                    ..default()
                },
            ))
            .id();
        commands.spawn((ChildOf(cta), punk_button(ENDLESS_CTA, MenuAction::ContinueEndless, 24.0, ButtonStyle::HazardAmber)));
        commands.spawn((
            ChildOf(cta),
            text("Floor 6+ // keep HP, armor, ammo & apps // boss rematch every 5th floor", 13.0, COLOR_ENDLESS),
        ));
    }
    let mut buttons = look.buttons.to_vec();
    let share = share_button_label(run, seed);
    if shareable_code(run).is_some() {
        buttons.push((&share, MenuAction::ShareSeed, ButtonStyle::ScrapSteel));
    }
    button_row(commands, root, &buttons, if compact { 18.0 } else { 21.0 }, if compact { 2.0 } else { 10.0 });
}

/// `<line>  //  SEED <code>` (unchanged for seedless runs).
pub fn with_seed(line: String, run: &RunStats) -> String {
    match shareable_code(run) {
        Some(code) => format!("{line}  //  SEED {code}"),
        None => line,
    }
}

/// Victory call to action of Endless Overdrive.
pub const ENDLESS_CTA: &str = "KEEP DESCENDING (ENDLESS)  [E]";

fn spawn_game_over(
    mut commands: Commands,
    run: Res<RunStats>,
    loadout: Res<RunLoadout>,
    store: Res<ProfileStore>,
    seed: Res<CustomSeedState>,
    tex: Option<Res<UiKitTextures>>,
) {
    spawn_end_screen(&mut commands, AppState::GameOver, no_service_look(), false, &run, &loadout, (&store, &seed), tex.as_deref());
}

fn spawn_victory(
    mut commands: Commands,
    run: Res<RunStats>,
    loadout: Res<RunLoadout>,
    store: Res<ProfileStore>,
    trial: Res<TimeTrialState>,
    seed: Res<CustomSeedState>,
    tex: Option<Res<UiKitTextures>>,
) {
    if run.mode == RunMode::TimeTrial {
        spawn_trial_finish(&mut commands, &run, &trial, &store, tex.as_deref());
        return;
    }
    spawn_end_screen(&mut commands, AppState::Victory, full_signal_look(), true, &run, &loadout, (&store, &seed), tex.as_deref());
}

/// Time Trial finish card: final time, PB delta / NEW PB, movement telemetry,
/// checkpoint splits and RETRY [R] / MAIN MENU [ENTER].
fn spawn_trial_finish(commands: &mut Commands, run: &RunStats, trial: &TimeTrialState, store: &ProfileStore, tex: Option<&UiKitTextures>) {
    let look = full_signal_look();
    let root = punk_screen_root_tinted(commands, AppState::Victory, tex, look.bg, look.backdrop, ("RF-LINK: COURSE CLEAR", ARC_CYAN));
    screen_rails(commands, root, tex, 16.0, look.tape);
    let band = commands
        .spawn((
            ChildOf(root),
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                padding: UiRect { top: Val::Px(6.0), bottom: Val::Px(8.0), ..default() },
                border: UiRect::vertical(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(ARC_CYAN.with_alpha(0.55)),
            BackgroundColor(look.band.0),
        ))
        .id();
    if let Some(t) = tex {
        commands.entity(band).insert(tiled_image(t.noise_strip.clone(), look.band.1, 2.0));
    }
    let stamp = commands.spawn((ChildOf(band), Node { column_gap: Val::Px(14.0), align_items: AlignItems::FlexEnd, ..default() })).id();
    signal_bars(commands, stamp, 5, ARC_CYAN, 8.0);
    commands.spawn((ChildOf(stamp), text("TIME TRIAL  //  SIGNAL TOWER REACHED  //  3 CP + FINISH", 13.0, ARC_CYAN)));
    stencil_title_layers(commands, band, "COURSE CLEAR", 72.0, ACID_GREEN, ACID_GREEN, &[(8.0, Color::BLACK), (4.0, look.title_slab)]);

    let final_time = trial.final_time().unwrap_or(trial.elapsed);
    let result = trial.result;
    commands.spawn((ChildOf(root), text(format_split_time(final_time), 64.0, BONE_TEXT)));
    let (pb_line, pb_color) = match result {
        Some(r) if r.new_pb => match r.previous_best {
            Some(prev) => (format!("NEW PB!  {}  (WAS {})", format_split_delta(final_time - prev), format_split_time(prev)), SPLIT_GOLD),
            None => ("NEW PB!  FIRST CLEAR ON RECORD".to_string(), SPLIT_GOLD),
        },
        Some(r) => match (r.delta, r.previous_best) {
            (Some(d), Some(prev)) => (format!("{}  VS PB {}", format_split_delta(d), format_split_time(prev)), PUNK_RED),
            _ => ("NO PB ON RECORD".to_string(), ASH_DIM),
        },
        None => ("NO PB ON RECORD".to_string(), ASH_DIM),
    };
    commands.spawn((ChildOf(root), stamped_tag(&pb_line, pb_color, INK_BLACK, 18.0)));
    let readout = commands
        .spawn((
            ChildOf(root),
            Node {
                max_width: Val::Px(1000.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(3.0),
                padding: UiRect::axes(Val::Px(22.0), Val::Px(8.0)),
                border: plate_rect((4.0, 2.0, 2.0, 2.0)),
                ..default()
            },
            BorderColor { left: ARC_CYAN, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.74)),
        ))
        .id();
    commands.spawn((ChildOf(readout), text(movement_line(run), 16.0, ARC_CYAN)));
    // Compare against the PB splits that stood before this attempt finished.
    commands.spawn((ChildOf(readout), text(trial_splits_line(&trial.splits, &trial.previous_splits), 14.0, BONE_TEXT)));
    commands.spawn((
        ChildOf(readout),
        text(format!("PIT RESCUES {}  |  RUNS & FEATS UNTOUCHED - MOVEMENT RECORDS ONLY", trial.rescues), 12.0, ASH_DIM),
    ));
    if let Some(err) = &store.last_save_error {
        commands.spawn((ChildOf(root), text(format!("SAVE PENDING ({err}) - will retry at menu"), 13.0, PUNK_RED)));
    }
    button_row(
        commands,
        root,
        &[("RETRY  [R]", MenuAction::RetryTimeTrial, ButtonStyle::HeroAcid), ("MAIN MENU  [ENTER]", MenuAction::BackToMenu, ButtonStyle::ScrapSteel)],
        20.0,
        8.0,
    );
}

/// In-run pause overlay (`Esc` / `P`): keeps the frozen 3D scene behind a dark
/// tactical overlay + hazard-taped terminal and lets the player resume,
/// restart, edit loadout, or quit.
fn spawn_pause_menu(mut commands: Commands, run: Res<RunStats>, loadout: Res<RunLoadout>, seed: Res<CustomSeedState>, tex: Option<Res<UiKitTextures>>) {
    let tex = tex.as_deref();
    let root = punk_screen_root(&mut commands, AppState::Paused, tex, Color::srgba(0.02, 0.015, 0.012, 0.74), 0.3);
    let card = terminal_chassis(&mut commands, root, tex, 700.0, false);
    commands.spawn((
        ChildOf(card),
        Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, ..default() },
        children![text("UAC-PHONE // STANDBY", 13.0, ASH_DIM), text("SIMULATION PAUSED", 13.0, HAZARD_YELLOW)],
    ));
    let title = centered(&mut commands, card);
    stencil_title(&mut commands, title, "PAUSED", 68.0, BONE_TEXT, PUNK_RED);
    // Run readout in a recessed slot: read, not pressed.
    let readout = commands
        .spawn((
            ChildOf(card),
            Node {
                margin: UiRect::top(Val::Px(4.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                border: plate_rect((4.0, 2.0, 2.0, 2.0)),
                ..default()
            },
            BorderColor { left: HAZARD_YELLOW, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.74)),
        ))
        .id();
    commands.spawn((ChildOf(readout), text(summary(&run), 17.0, BONE_TEXT)));
    commands.spawn((ChildOf(readout), text(with_seed(end_loadout_line(&loadout), &run), 13.0, HAZARD_YELLOW)));
    commands.spawn((ChildOf(readout), Node { max_width: Val::Px(620.0), ..default() }, children![text(installed_list(&run), 13.0, ASH_DIM)]));
    let wrap = centered(&mut commands, card);
    let btns = commands
        .spawn((
            ChildOf(wrap),
            Node {
                margin: UiRect { top: Val::Px(10.0), bottom: Val::Px(12.0), ..default() },
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                row_gap: Val::Px(10.0),
                width: Val::Px(380.0),
                ..default()
            },
        ))
        .id();
    // RESUME is the one loud switch; the exits step down in weight.
    for (label, action, style, font) in [
        ("RESUME  [ESC]", MenuAction::Continue, ButtonStyle::HeroAcid, 24.0),
        ("RESTART RUN  [R]", MenuAction::RestartRun, ButtonStyle::HazardAmber, 18.0),
        ("SETTINGS  [O]", MenuAction::OpenSettings, ButtonStyle::ScrapSteel, 18.0),
        ("LOADOUT  [L]", MenuAction::OpenLoadout, ButtonStyle::ScrapSteel, 18.0),
        ("MAIN MENU  [M]", MenuAction::BackToMenu, ButtonStyle::DangerRed, 18.0),
    ] {
        commands.spawn((ChildOf(btns), punk_button(label, action, font, style)));
    }
    if shareable_code(&run).is_some() {
        commands.spawn((ChildOf(btns), punk_button(&share_button_label(&run, &seed), MenuAction::ShareSeed, 15.0, ButtonStyle::ScrapSteel)));
    }
    spawn_input_hint(&mut commands, card);
}

/// Android input requirement shown under the Main Menu and Pause actions.
/// There are no touch controls: a controller or mouse & keyboard is required.
pub const ANDROID_INPUT_HINT: &str = "CONTROLLER OR MOUSE & KEYBOARD REQUIRED // NO TOUCH CONTROLS";

/// The platform's menu input hint (Android only; desktop and web menus stay
/// uncluttered).
pub const fn platform_input_hint(is_android: bool) -> Option<&'static str> {
    if is_android { Some(ANDROID_INPUT_HINT) } else { None }
}

/// Marker for the [`ANDROID_INPUT_HINT`] line.
#[derive(Component)]
pub struct InputHint;

/// Append the platform input hint (if any) as a small centered line at the
/// end of `parent`'s column.
fn spawn_input_hint(commands: &mut Commands, parent: Entity) {
    let Some(hint) = platform_input_hint(crate::perf::IS_ANDROID) else { return };
    commands.spawn((
        ChildOf(parent),
        InputHint,
        Node { margin: UiRect::top(Val::Px(4.0)), justify_content: JustifyContent::Center, ..default() },
        children![text(hint, 11.0, ASH_DIM.with_alpha(0.85))],
    ));
}

/// Menu action. "START" snapshots the profile's equipped loadout (chip, skin,
/// contracts, catalog) into the run.
#[allow(clippy::too_many_arguments)]
fn perform(
    commands: &mut Commands,
    players: &Query<Entity, With<Player>>,
    action: MenuAction,
    run: &mut RunStats,
    mods: &mut PlayerModifiers,
    (store, loadout, trial, seed): (&mut ProfileStore, &mut RunLoadout, &mut TimeTrialState, &mut CustomSeedState),
    time: &Time,
    (state, ret): (AppState, &mut SettingsReturn),
    next: &mut NextState<AppState>,
) {
    // Abandoning an active Endless run from Pause preserves earned Endless records/deltas.
    let finalize_endless = |run: &mut RunStats, loadout: &RunLoadout, store: &mut ProfileStore| {
        if state == AppState::Paused && run.endless && !run.run_recorded {
            crate::meta::record_run_outcome(false, run, loadout, store);
        }
    };
    match action {
        MenuAction::StartRun => {
            if seed.editing {
                seed.lock();
                #[cfg(target_arch = "wasm32")]
                crate::compendium::sync_url_hash_seed(&seed.code);
            }
            let (s, label) = pick_run_seed(seed, time.elapsed().as_nanos() as u64 ^ 0xA5A5_5A5A_1234_5678);
            start_run_with_loadout(run, mods, loadout, store.profile.build_loadout(), next, s);
            apply_custom_label(run, label);
        }
        MenuAction::RestartRun if run.mode == RunMode::TimeTrial => {
            // Instant reset on the same course (no level rebuild).
            trial.reset_requested = true;
            next.set(AppState::Playing);
        }
        MenuAction::RestartRun => {
            finalize_endless(run, loadout, store);
            for e in players.iter() {
                commands.entity(e).despawn();
            }
            if let RunMode::Daily(date) = run.mode {
                // Same date = same seed: force a fresh build of Floor 1.
                commands.remove_resource::<crate::level::LevelGrid>();
                begin_daily_attempt(&mut store.profile.daily_records, date);
                let _ = store.try_save();
                start_daily_run(run, mods, loadout, next, date, store.profile.build_loadout().skin);
            } else if run.custom_seed {
                // A custom seed restarts on the very same bunker.
                commands.remove_resource::<crate::level::LevelGrid>();
                let (s, label) = (run.run_seed, run.seed_label.clone());
                start_run_with_loadout(run, mods, loadout, store.profile.build_loadout(), next, s);
                apply_custom_label(run, label);
            } else {
                let (s, label) = pick_run_seed(seed, time.elapsed().as_nanos() as u64 ^ run.run_seed.wrapping_add(0x9E37_79B9));
                if label.is_some() {
                    commands.remove_resource::<crate::level::LevelGrid>();
                }
                start_run_with_loadout(run, mods, loadout, store.profile.build_loadout(), next, s);
                apply_custom_label(run, label);
            }
        }
        MenuAction::StartDaily => {
            let date = DailyDate::today_utc();
            begin_daily_attempt(&mut store.profile.daily_records, date);
            if let Err(e) = store.try_save() {
                warn!("profile save failed (marked dirty for retry): {e}");
            }
            start_daily_run(run, mods, loadout, next, date, store.profile.build_loadout().skin);
        }
        MenuAction::StartTimeTrial => {
            start_time_trial(run, mods, loadout, trial, next, store.profile.build_loadout().skin);
        }
        MenuAction::RetryTimeTrial => {
            if run.mode == RunMode::TimeTrial {
                trial.reset_requested = true;
                run.run_recorded = false;
                next.set(AppState::Playing);
            }
        }
        MenuAction::Continue => next.set(AppState::Playing),
        MenuAction::BackToMenu => {
            finalize_endless(run, loadout, store);
            next.set(AppState::MainMenu);
        }
        MenuAction::OpenLoadout => {
            finalize_endless(run, loadout, store);
            next.set(AppState::Loadout);
        }
        MenuAction::OpenContractsFeats => next.set(AppState::ContractsFeats),
        MenuAction::OpenSettings => {
            if state != AppState::Settings {
                ret.0 = if state == AppState::Paused { AppState::Paused } else { AppState::MainMenu };
            }
            next.set(AppState::Settings);
        }
        MenuAction::CloseSettings => next.set(ret.0),
        MenuAction::ContinueEndless => {
            if state == AppState::Victory && run.mode != RunMode::TimeTrial {
                continue_into_endless(run, next);
            }
        }
        MenuAction::OpenCompendium => next.set(AppState::Compendium),
        MenuAction::ToggleSeedEdit => {
            seed.toggle_edit();
            if !seed.editing {
                #[cfg(target_arch = "wasm32")]
                crate::compendium::sync_url_hash_seed(&seed.code);
            }
        }
        MenuAction::RollSeed => {
            seed.roll(time.elapsed().as_nanos() as u64 ^ 0x5EED_C0DE);
            #[cfg(target_arch = "wasm32")]
            crate::compendium::sync_url_hash_seed(&seed.code);
        }
        MenuAction::ClearSeed => {
            seed.clear();
            #[cfg(target_arch = "wasm32")]
            crate::compendium::sync_url_hash_seed("");
        }
        MenuAction::ShareSeed => {
            if let Some(_code) = seed.arm_from_run(run) {
                #[cfg(target_arch = "wasm32")]
                crate::compendium::sync_url_hash_seed(&_code);
            }
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn menu_buttons(
    mut commands: Commands,
    players: Query<Entity, With<Player>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut buttons: Query<
        (Entity, &ComputedNode, &UiGlobalTransform, &MenuAction, &PunkButtonColors, &mut BackgroundColor, &mut BorderColor, Option<&Children>),
        With<MenuButton>,
    >,
    (pad, focus): (Res<GamepadInput>, Res<GamepadFocus>),
    mut labels: Query<&mut TextColor>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    (mut store, mut loadout, mut trial, mut seed): (ResMut<ProfileStore>, ResMut<RunLoadout>, ResMut<TimeTrialState>, ResMut<CustomSeedState>),
    time: Res<Time>,
    (state, mut ret): (Res<State<AppState>>, ResMut<SettingsReturn>),
    mut next: ResMut<NextState<AppState>>,
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    for (e, node, tf, action, colors, mut bg, mut border, children) in &mut buttons {
        // Controller navigation owns the highlight until the mouse moves.
        let pointer_hovered = cursor.is_some_and(|c| node.contains_point(*tf, c));
        let hovered = if pad.active { focus.shows(&pad, e) } else { pointer_hovered };
        // Punk hover snap: solid accent fill, inverted label, bright frame.
        let want = colors.bg(hovered);
        if bg.0 != want {
            bg.0 = want;
        }
        let want = colors.border(hovered);
        if *border != want {
            *border = want;
        }
        for child in children.into_iter().flatten() {
            if let Ok(mut c) = labels.get_mut(*child) {
                let want = colors.text(hovered);
                if c.0 != want {
                    c.0 = want;
                }
            }
        }
        if (pointer_hovered && mouse.just_released(MouseButton::Left)) || focus.activate == Some(e) {
            perform(
                &mut commands,
                &players,
                *action,
                &mut run,
                &mut mods,
                (&mut store, &mut loadout, &mut trial, &mut seed),
                &time,
                (*state.get(), &mut ret),
                &mut next,
            );
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn menu_keys(
    mut commands: Commands,
    players: Query<Entity, With<Player>>,
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<AppState>>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    (mut store, mut loadout, mut trial, mut seed): (ResMut<ProfileStore>, ResMut<RunLoadout>, ResMut<TimeTrialState>, ResMut<CustomSeedState>),
    time: Res<Time>,
    mut ret: ResMut<SettingsReturn>,
    mut next: ResMut<NextState<AppState>>,
    pad: Res<GamepadInput>,
) {
    // Controller shortcuts (never typed text, so the seed editor allows them).
    if let Some(action) = pad_menu_action(*state.get(), pad.pause, pad.back) {
        perform(
            &mut commands,
            &players,
            action,
            &mut run,
            &mut mods,
            (&mut store, &mut loadout, &mut trial, &mut seed),
            &time,
            (*state.get(), &mut ret),
            &mut next,
        );
        return;
    }
    // Typing a seed (`DOOM`, `BOSS`...) never fires main menu hotkeys.
    if *state.get() == AppState::MainMenu && seed.editing {
        return;
    }
    let enter = keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]);
    let action = menu_key_action(
        *state.get(),
        enter,
        keys.just_pressed(KeyCode::Escape),
        keys.just_pressed(KeyCode::KeyL),
        keys.just_pressed(KeyCode::KeyC),
        keys.just_pressed(KeyCode::KeyR),
        keys.just_pressed(KeyCode::KeyM),
    )
    .or_else(|| menu_hotkey_action(*state.get(), keys.just_pressed(KeyCode::KeyO), keys.just_pressed(KeyCode::KeyE)))
    .or_else(|| {
        mode_hotkey_action(
            *state.get(),
            run.mode,
            keys.just_pressed(KeyCode::KeyD),
            keys.just_pressed(KeyCode::KeyT),
            keys.just_pressed(KeyCode::KeyR),
        )
    })
    .or_else(|| seed_hotkey_action(*state.get(), run.mode, keys.just_pressed(KeyCode::KeyB), keys.just_pressed(KeyCode::KeyS)));
    if let Some(action) = action {
        perform(
            &mut commands,
            &players,
            action,
            &mut run,
            &mut mods,
            (&mut store, &mut loadout, &mut trial, &mut seed),
            &time,
            (*state.get(), &mut ret),
            &mut next,
        );
    }
}

// ---------------------------------------------------------------------------
// In-game HUD
// ---------------------------------------------------------------------------
//
// Layout (the 3D phone in the left hand carries signal / radar / apps, so the
// bottom-left stays clear):
// - top-left: floor + kills/jammers panel
// - top-center: status panel (+ SOS uplink bar while uploading), alert below
// - top-right: killfeed toasts, each on its own backing chip
// - bottom-center: vitals panel (HP / ARMOR / STAMINA bars), the docked VEL
//   telemetry strip above it (with its stamped `BHOP xN` tag) and the prompt
// - bottom-right: weapon & ammo deck (active card, shell pips, plasma bar or
//   warhead pips, 3 slot chips, 5G overclock + Doppler pills)
// - edges: subtle dark-red Dead-Zone frame (no scanlines across the view)

const PANEL_BG: Color = Color::srgba(0.085, 0.072, 0.066, 0.8);
const PANEL_EDGE: Color = Color::srgba(0.32, 0.28, 0.25, 0.45);
const TRACK_BG: Color = Color::srgba(0.02, 0.018, 0.016, 0.8);
const TRACK_BORDER: Color = Color::srgba(0.32, 0.28, 0.25, 0.7);
const COLOR_HP_GREEN: Color = Color::srgb(0.16, 0.9, 0.46);
const COLOR_ARMOR: Color = Color::srgb(0.46, 0.74, 0.88);
pub const COLOR_GOLD: Color = Color::srgb(1.0, 0.8, 0.28);
const COLOR_SHOTGUN: Color = Color::srgb(1.0, 0.56, 0.14);
/// Signal-Mortar accent: warm amber-orange, hotter than the shotgun's.
pub const COLOR_ROCKET: Color = Color::srgb(1.0, 0.4, 0.1);
const COLOR_WARHEAD: Color = Color::srgb(0.3, 0.32, 0.34);
const COLOR_WARHEAD_TIP: Color = Color::srgb(1.0, 0.62, 0.18);
const COLOR_SHELL: Color = Color::srgb(0.82, 0.13, 0.09);
const COLOR_BRASS: Color = Color::srgb(0.95, 0.72, 0.3);
const COLOR_PIP_OFF: Color = Color::srgba(1.0, 1.0, 1.0, 0.07);
const COLOR_PIP_OFF_BORDER: Color = Color::srgba(1.0, 1.0, 1.0, 0.16);

const TOAST_SLOTS: u8 = 5;
/// Shell pips shown for the shotgun magazine.
pub const SHELL_PIPS: u8 = 10;
/// Warhead pips shown for the Signal-Mortar rack.
pub const ROCKET_PIPS: u8 = 10;
const DECK_WIDTH: f32 = 340.0;
/// Weapon card frame (left, right, top, bottom px) and horizontal padding.
const CARD_BORDER: (f32, f32, f32, f32) = (5.0, 1.0, 1.0, 3.0);
const CARD_PAD_X: f32 = 12.0;
/// Weapon title font of the card header.
const WEAPON_TITLE_FONT: f32 = 15.0;
/// Buff pills docked right in the card header.
const OVERCLOCK_PILL: &str = "5G OC";
const DOPPLER_PILL: &str = "DOPPLER";
const PILL_FONT: f32 = 11.0;
const PILL_PAD_X: f32 = 6.0;
const PILL_GAP: f32 = 5.0;
/// Slot badge metrics: frame, padding, keycap, gaps and the two font sizes.
const SLOT_BORDER: (f32, f32, f32, f32) = (4.0, 1.0, 1.0, 2.0);
const SLOT_GAP: f32 = 6.0;
const SLOT_PAD_X: f32 = 5.0;
const SLOT_INNER_GAP: f32 = 5.0;
const SLOT_KEY_WIDTH: f32 = 13.0;
const SLOT_LABEL_FONT: f32 = 10.0;
const SLOT_AMMO_FONT: f32 = 13.0;
/// Advance of the HUD's monospace face (Fira Mono) as a fraction of font size.
#[cfg(test)]
const MONO_ADVANCE: f32 = 0.6;

/// Width (px) of `chars` monospace glyphs at `font` px.
#[cfg(test)]
fn mono_width(chars: usize, font: f32) -> f32 {
    chars as f32 * font * MONO_ADVANCE
}

/// Inner (content) width of one slot badge: the deck split three ways.
#[cfg(test)]
fn slot_inner_width() -> f32 {
    let n = Weapon::ALL.len() as f32;
    (DECK_WIDTH - SLOT_GAP * (n - 1.0)) / n - SLOT_BORDER.0 - SLOT_BORDER.1 - 2.0 * SLOT_PAD_X
}

/// Width one slot badge row needs for `name` and an ammo count of `digits`.
#[cfg(test)]
fn slot_row_width(name: &str, digits: usize) -> f32 {
    SLOT_KEY_WIDTH + SLOT_INNER_GAP + mono_width(name.len(), SLOT_LABEL_FONT) + SLOT_INNER_GAP + mono_width(digits, SLOT_AMMO_FONT)
}

/// Slot badge reserve color: red when dry, bone on the equipped weapon, dim otherwise.
fn slot_ammo_color(n: u32, active: bool) -> Color {
    if n == 0 {
        COLOR_SIGNAL_RED
    } else if active {
        COLOR_TEXT
    } else {
        COLOR_DIM
    }
}
const VITALS_WIDTH: f32 = 312.0;
const UPLINK_WIDTH: f32 = 440.0;
/// Width of the Carrier Daemon health bar.
const BOSS_BAR_WIDTH: f32 = 460.0;
/// Boss health-bar tick marks at the phase thresholds.
pub const BOSS_PHASE_TICKS: [f32; 2] = [crate::boss::PHASE2_THRESHOLD, crate::boss::PHASE1_THRESHOLD];

/// Boss health fill color: crimson -> molten gold as it drops, glowing
/// cyan-violet while the Signal Shield is up.
pub fn boss_bar_color(hud: &BossHud, t: f32) -> Color {
    if hud.phase.shielded() {
        Color::srgb(0.45 + 0.1 * (t * 6.0).sin(), 0.55, 1.0)
    } else if hud.phase == crate::boss::BossPhase::Overdrive {
        Color::srgb(1.0, 0.55 + 0.2 * (t * 10.0).sin().abs(), 0.08)
    } else {
        PUNK_RED.mix(&HAZARD_YELLOW, (1.0 - hud.hp_frac).clamp(0.0, 1.0) * 0.8)
    }
}

/// Boss phase tag color.
pub fn boss_tag_color(hud: &BossHud) -> Color {
    match hud.phase {
        p if p.shielded() => Color::srgb(0.55, 0.65, 1.0),
        crate::boss::BossPhase::Overdrive => HAZARD_YELLOW,
        _ => PUNK_RED,
    }
}

/// Top status line while uploading (boss floors hold at 99%).
pub fn uplink_status_text(ext: &ExtractionState) -> &'static str {
    match ext {
        ExtractionState::Uploading { boss_hold: true, .. } => crate::boss::BOSS_HOLD_TEXT,
        _ => "DEMON RUSH - HOLD THE SIGNAL",
    }
}
/// Height of the danger tape rails that clamp the status plate during a rush.
const RUSH_RAIL_HEIGHT: f32 = 6.0;

/// `cur / max` clamped to `0..=1` (0 when `max` is not positive).
pub fn fraction(cur: f32, max: f32) -> f32 {
    if max <= 0.0 { 0.0 } else { (cur / max).clamp(0.0, 1.0) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpBand {
    Healthy,
    Wounded,
    Critical,
}

/// HP color band: healthy above 55%, wounded 25%..=55%, critical below 25%.
pub fn hp_band(frac: f32) -> HpBand {
    if frac > 0.55 {
        HpBand::Healthy
    } else if frac >= 0.25 {
        HpBand::Wounded
    } else {
        HpBand::Critical
    }
}

/// HP bar color; the critical band pulses.
pub fn hp_color(frac: f32, t: f32) -> Color {
    match hp_band(frac) {
        HpBand::Healthy => COLOR_HP_GREEN,
        HpBand::Wounded => COLOR_SIGNAL_AMBER,
        HpBand::Critical => COLOR_SIGNAL_RED.with_alpha(0.5 + 0.5 * (t * 9.0).sin().abs()),
    }
}

/// Dash stamina: 1.0 = ready, 0.0 = just dashed.
pub fn stamina_fill(dash_cooldown: f32, max_cd: f32) -> f32 {
    if max_cd <= 0.0 { 1.0 } else { (1.0 - dash_cooldown / max_cd).clamp(0.0, 1.0) }
}

/// How many of `pips` light up for `cur / max` ammo (any ammo lights >= 1).
pub fn lit_pips(cur: u32, max: u32, pips: u8) -> u8 {
    if cur == 0 || max == 0 {
        return 0;
    }
    ((cur as f32 / max as f32 * pips as f32).ceil() as u8).min(pips)
}

/// Top of the VEL momentum track (m/s); the readout badge snaps solid here.
pub const VEL_GAUGE_MAX: f32 = 32.0;
/// Segments in the VEL momentum track (2 m/s each).
pub const VEL_SEGMENTS: u8 = 16;
/// Hysteresis (m/s) before the VEL gauge drops back a tier / un-snaps the badge.
pub const VEL_HYSTERESIS: f32 = 0.5;
/// Width (px) of the docked VEL strip and the vitals plate under it.
const VEL_PLATE_WIDTH: f32 = 312.0;

/// Momentum band of the VEL gauge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpeedTier {
    /// Below 12 m/s: walking / plain running.
    #[default]
    Walk,
    /// 12..20 m/s: first hops.
    Flow,
    /// 20..28 m/s: a real bhop chain.
    Surge,
    /// 28+ m/s: dash-carry territory.
    Redline,
}

/// Speed band: `< 12` walk, `12..20` flow, `20..28` surge, `>= 28` redline
/// (the bands line up with the tachometer's zone breaks).
pub fn speed_tier(speed: f32) -> SpeedTier {
    if speed >= 28.0 {
        SpeedTier::Redline
    } else if speed >= 20.0 {
        SpeedTier::Surge
    } else if speed >= 12.0 {
        SpeedTier::Flow
    } else {
        SpeedTier::Walk
    }
}

/// [`speed_tier`] with [`VEL_HYSTERESIS`]: climbs a tier as soon as the
/// threshold is reached, but only drops once speed is half a m/s below it, so
/// the rail / readout / stamp never flicker while hovering at 12 / 20 / 28.
pub fn speed_tier_hysteresis(prev: SpeedTier, speed: f32) -> SpeedTier {
    let raw = speed_tier(speed);
    if raw >= prev { raw } else { speed_tier(speed + VEL_HYSTERESIS).min(prev) }
}

/// Readout badge snapped solid at [`VEL_GAUGE_MAX`]+, with the same hysteresis.
pub fn vel_maxed_hysteresis(prev: bool, speed: f32) -> bool {
    speed >= VEL_GAUGE_MAX || (prev && speed >= VEL_GAUGE_MAX - VEL_HYSTERESIS)
}

/// VEL accent per tier, always steady (no strobing): muted ash, arc cyan,
/// acid green, hazard yellow.
pub fn speed_color(tier: SpeedTier) -> Color {
    match tier {
        SpeedTier::Walk => ASH_DIM,
        SpeedTier::Flow => ARC_CYAN,
        SpeedTier::Surge => ACID_GREEN,
        SpeedTier::Redline => HAZARD_YELLOW,
    }
}

/// Fixed tachometer zone of segment `i` (2 m/s each): ash 0-12, cyan 12-20,
/// green 20-28, hazard yellow 28-32.
pub fn segment_zone_color(i: u8) -> Color {
    match i {
        0..=5 => ASH_DIM,
        6..=9 => ARC_CYAN,
        10..=13 => ACID_GREEN,
        _ => HAZARD_YELLOW,
    }
}

/// Segment fill: lit segments at full zone color, unlit ones keep a faint
/// (16%) zone tint so the tachometer scale is always legible.
pub fn segment_color(i: u8, lit: u8) -> Color {
    segment_zone_color(i).with_alpha(if i < lit { 1.0 } else { 0.16 })
}

/// Extra right margin after segments 5 / 9 / 13: 4 px zone breaks at 12, 20, 28 m/s.
pub fn segment_zone_break(i: u8) -> bool {
    matches!(i, 5 | 9 | 13)
}

/// Momentum track fill `0..=1` over `0..VEL_GAUGE_MAX` m/s.
pub fn speed_fill(speed: f32) -> f32 {
    fraction(speed, VEL_GAUGE_MAX)
}

/// Lit segments of the momentum track (any motion lights at least one).
pub fn speed_segments(speed: f32) -> u8 {
    if speed < 0.05 { 0 } else { ((speed_fill(speed) * VEL_SEGMENTS as f32).ceil() as u8).clamp(1, VEL_SEGMENTS) }
}

/// Constant-width integer VEL readout, e.g. `18 m/s`, ` 5 m/s`.
pub fn speed_label(speed: f32) -> String {
    format!("{:>2.0} m/s", speed.max(0.0))
}

/// Stamped chain tag atop the VEL strip: `BHOP x2`, `BHOP x3`, ... (none below 2).
pub fn bhop_chain_label(chain: u32) -> Option<String> {
    (chain >= 2).then(|| format!("BHOP x{chain}"))
}

/// Debounced VEL gauge state shared by the HUD text and box updaters.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct VelGauge {
    /// Horizontal speed this frame (m/s).
    pub speed: f32,
    /// Hysteresis-filtered tier (rail, readout and stamp color).
    pub tier: SpeedTier,
    /// Readout badge snapped solid hazard yellow (32+ m/s).
    pub maxed: bool,
}

impl VelGauge {
    /// Advance with this frame's horizontal speed.
    pub fn update(&mut self, speed: f32) {
        self.speed = speed;
        self.tier = speed_tier_hysteresis(self.tier, speed);
        self.maxed = vel_maxed_hysteresis(self.maxed, speed);
    }

    pub fn color(&self) -> Color {
        speed_color(self.tier)
    }

    /// Readout text color: ink on the solid badge, bone while walking, tier color otherwise.
    pub fn value_color(&self) -> Color {
        if self.maxed {
            INK_BLACK
        } else if self.tier == SpeedTier::Walk {
            BONE_TEXT
        } else {
            self.color()
        }
    }

    /// Readout badge background: solid hazard yellow at 32+ m/s, clear below.
    pub fn badge_color(&self) -> Color {
        if self.maxed { HAZARD_YELLOW } else { Color::NONE }
    }
}

fn track_vel_gauge(mut gauge: ResMut<VelGauge>, players: Query<&Player>) {
    if let Ok(p) = players.single() {
        gauge.update(Vec2::new(p.velocity.x, p.velocity.z).length());
    }
}

/// The 5G overclock damage bonus is live.
pub fn overclock_active(bars: u8, bonus: f32) -> bool {
    bars >= 4 && bonus > 0.0
}

fn weapon_accent(w: Weapon) -> Color {
    match w {
        Weapon::Shotgun => COLOR_SHOTGUN,
        Weapon::Plasma => COLOR_CYAN,
        Weapon::Rocket => COLOR_ROCKET,
    }
}

fn weapon_title(w: Weapon) -> &'static str {
    match w {
        Weapon::Shotgun => "SHOTGUN",
        Weapon::Plasma => "PLASMA REPEATER",
        Weapon::Rocket => "SIGNAL-MORTAR",
    }
}

/// Short weapon-deck slot name.
fn weapon_slot_name(w: Weapon) -> &'static str {
    match w {
        Weapon::Shotgun => "SHOTGUN",
        Weapon::Plasma => "PLASMA",
        Weapon::Rocket => "MORTAR",
    }
}

fn ammo(p: &Player, w: Weapon) -> (u32, u32) {
    match w {
        Weapon::Shotgun => (p.ammo_shells, p.max_shells),
        Weapon::Plasma => (p.ammo_plasma, p.max_plasma),
        Weapon::Rocket => (p.ammo_rockets, p.max_rockets),
    }
}

/// Floor plate subtitle naming the floor's sector theme.
pub fn sector_label(floor: u32) -> String {
    format!("SECTOR {:02} // {}", floor, FloorTheme::for_floor(floor).name())
}

/// Every HUD text element, updated by one query.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum HudText {
    /// Boss plate header (`THE CARRIER DAEMON`).
    BossName,
    /// Boss plate phase tag (`PHASE I`, `SHIELDED // 2 PYLONS`...).
    BossPhase,
    Floor,
    /// Sector theme subtitle (`SECTOR 01 // RUST BUNKER`).
    Sector,
    Stats,
    /// Starter chip label of the run loadout.
    LoadoutChip,
    /// Active contract tags (empty without contracts).
    LoadoutContracts,
    Status,
    Prompt,
    Alert,
    /// VEL speedometer readout.
    VelValue,
    /// `BHOP xN` chain stamp.
    VelChain,
    Toast(u8),
    Hitmarker,
    UplinkLabel,
    HpValue,
    ArmorValue,
    StaminaValue,
    WeaponName,
    AmmoCount,
    AmmoMax,
    /// Slot badge weapon name.
    Slot(u8),
    /// Slot badge keycap digit.
    SlotKey(u8),
    /// Slot badge reserve count.
    SlotAmmo(u8),
}

/// Every HUD box (background/size/border driven), updated by one query.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum HudBox {
    DamageFlash,
    DeadZoneFrame,
    /// Status plate: its rail goes alert-red during the SOS uplink.
    StatusPlate,
    /// Danger tape rail above / below the status plate (SOS uplink only).
    RushRail,
    UplinkPanel,
    UplinkFill,
    /// Carrier Daemon boss plate (only while a living boss is present).
    BossPlate,
    /// Boss health fill (crimson / molten gold, cyan-violet while shielded).
    BossFill,
    /// Boss health track: its end caps follow the phase tag color.
    BossTrack,
    AlertPanel,
    ToastChip(u8),
    /// Square bullet between the chip label and the contract tags.
    LoadoutDot,
    /// Docked VEL telemetry strip above the vitals plate (tier-colored rail).
    VelPlate,
    /// Framed bracket track around the VEL tachometer segments.
    VelTrack,
    /// Readout badge (snaps solid hazard yellow at 32+ m/s).
    VelValueBadge,
    /// Stamped `BHOP xN` tag bolted atop the VEL strip (hidden below x2).
    VelChainTag,
    /// One segment of the VEL tachometer.
    VelSeg(u8),
    HpFill,
    ArmorFill,
    StaminaFill,
    ShellRow,
    ShellPip(u8),
    PlasmaTrack,
    PlasmaFill,
    RocketRow,
    RocketPip(u8),
    WeaponCard,
    SlotChip(u8),
    /// Keycap block of a slot badge (weapon accent while equipped).
    SlotKey(u8),
    /// Reserve gauge fill of a slot badge.
    SlotFill(u8),
    OverclockPill,
    /// Lit while Doppler Overdrive is active (20+ m/s).
    DopplerPill,
}

fn abs_full() -> Node {
    Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }
}

/// Segment cuts every 10% across a bar track; the 50% cut is heavier so the
/// half-way mark reads at a glance.
fn spawn_ticks(commands: &mut Commands, track: Entity) {
    for n in 1..10 {
        let major = n == 5;
        commands.spawn((
            ChildOf(track),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(n as f32 * 10.0),
                top: Val::Px(0.0),
                width: Val::Px(if major { 3.0 } else { 2.0 }),
                height: Val::Percent(100.0),
                margin: UiRect::left(Val::Px(-1.0)),
                ..default()
            },
            BackgroundColor(INK_BLACK.with_alpha(if major { 0.95 } else { 0.78 })),
        ));
    }
}

/// Instrument ruler under a bar: a 1px tick every 10%, taller bone ticks at
/// 0 / 50 / 100%. Lives outside the track, so the fill never hides it.
fn spawn_ruler(commands: &mut Commands, parent: Entity) {
    let ruler = commands.spawn((ChildOf(parent), Node { width: Val::Percent(100.0), height: Val::Px(RULER_HEIGHT), ..default() })).id();
    for n in 0..=10u8 {
        let major = n % 5 == 0;
        commands.spawn((
            ChildOf(ruler),
            Node {
                position_type: PositionType::Absolute,
                // Pin the last tick to the right edge so it never overflows.
                left: if n == 10 { Val::Auto } else { Val::Percent(n as f32 * 10.0) },
                right: if n == 10 { Val::Px(0.0) } else { Val::Auto },
                top: Val::Px(0.0),
                width: Val::Px(if major { 2.0 } else { 1.0 }),
                height: Val::Px(if major { RULER_HEIGHT } else { RULER_HEIGHT - 2.0 }),
                ..default()
            },
            BackgroundColor(if major { BONE_TEXT.with_alpha(0.7) } else { ASH_DIM.with_alpha(0.6) }),
        ));
    }
}

const RULER_HEIGHT: f32 = 5.0;

/// Framed bar track: a colored left cap, a matching dimmer right cap and a
/// dark bottom lip, so the bar reads as a bracketed gauge.
fn bar_track(width: Val, height: f32, color: Color, display: Display) -> impl Bundle {
    (
        Node { display, width, height: Val::Px(height), border: plate_rect((3.0, 2.0, 1.0, 2.0)), padding: UiRect::all(Val::Px(1.0)), ..default() },
        BorderColor { left: color, right: color.with_alpha(0.55), top: TRACK_BORDER, bottom: INK_BLACK },
        BackgroundColor(TRACK_BG),
    )
}

#[allow(clippy::too_many_arguments)]
/// Labelled horizontal bar: `LABEL ......... value` over a framed track, with
/// an instrument ruler under tall (primary) bars.
fn spawn_bar(commands: &mut Commands, parent: Entity, label: &str, label_color: Color, value: HudText, fill: HudBox, height: f32, color: Color) {
    let block = commands
        .spawn((ChildOf(parent), Node { width: Val::Percent(100.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }))
        .id();
    commands.spawn((
        ChildOf(block),
        Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::FlexEnd, ..default() },
        children![text(label, 12.0, label_color), (value, text("", 15.0, COLOR_TEXT))],
    ));
    let track = commands.spawn((ChildOf(block), bar_track(Val::Percent(100.0), height, color, Display::Flex))).id();
    commands.spawn((ChildOf(track), fill, Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(color)));
    if height >= 8.0 {
        spawn_ticks(commands, track);
    }
    if height >= 12.0 {
        spawn_ruler(commands, block);
    }
}

/// Docked VEL telemetry strip (31 px) bolted 4 px above the vitals plate:
/// `VEL [tachometer] 18 m/s`. The tachometer is 16 fixed 9 px segments
/// (2 m/s each) in four fixed zones with 4 px breaks at 12 / 20 / 28 m/s,
/// framed by a tier-colored bracket track. A stamped `BHOP xN` tag sits on
/// top of the strip, flush with its left rail.
fn spawn_vel_plate(commands: &mut Commands, root: Entity) {
    let plate = commands
        .spawn((
            ChildOf(root),
            HudBox::VelPlate,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                margin: UiRect::left(Val::Px(-VEL_PLATE_WIDTH / 2.0)),
                width: Val::Px(VEL_PLATE_WIDTH),
                // 14 px screen margin + 120 px vitals plate + 4 px gap.
                bottom: Val::Px(138.0),
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(5.0)),
                border: plate_rect((4.0, 1.0, 1.0, 2.0)),
                ..default()
            },
            plate_border(ASH_DIM, STEEL_BORDER.with_alpha(0.45)),
            BackgroundColor(IRON_DARK.with_alpha(0.8)),
        ))
        .id();
    commands.spawn((ChildOf(plate), text("VEL", 12.0, ASH_DIM)));
    let track = commands
        .spawn((ChildOf(plate), HudBox::VelTrack, bar_track(Val::Px(187.0), 12.0, ASH_DIM, Display::Flex)))
        .id();
    commands.entity(track).entry::<Node>().and_modify(|mut n| n.flex_shrink = 0.0);
    for i in 0..VEL_SEGMENTS {
        let gap = if i + 1 == VEL_SEGMENTS { 0.0 } else if segment_zone_break(i) { 4.0 } else { 2.0 };
        commands.spawn((
            ChildOf(track),
            HudBox::VelSeg(i),
            Node { width: Val::Px(9.0), height: Val::Px(7.0), flex_grow: 0.0, flex_shrink: 0.0, margin: UiRect::right(Val::Px(gap)), ..default() },
            BackgroundColor(segment_color(i, 0)),
        ));
    }
    commands.spawn((
        ChildOf(plate),
        HudBox::VelValueBadge,
        Node { margin: UiRect::left(Val::Auto), padding: UiRect::axes(Val::Px(2.0), Val::Px(0.0)), flex_shrink: 0.0, ..default() },
        BackgroundColor(Color::NONE),
        children![(HudText::VelValue, text(speed_label(0.0), 15.0, BONE_TEXT))],
    ));
    // Stamped chain tag, sitting on top of the 31 px strip (138 + 31 + 2).
    commands.spawn((
        ChildOf(root),
        HudBox::VelChainTag,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            margin: UiRect::left(Val::Px(-VEL_PLATE_WIDTH / 2.0)),
            bottom: Val::Px(171.0),
            padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
            border: UiRect::bottom(Val::Px(2.0)),
            ..default()
        },
        BorderColor::all(INK_BLACK.with_alpha(0.85)),
        BackgroundColor(ASH_DIM),
        children![(HudText::VelChain, text("", 11.0, INK_BLACK))],
    ));
}

fn spawn_hud(mut commands: Commands, tex: Option<Res<UiKitTextures>>, store: Option<Res<ProfileStore>>, run: Res<RunStats>) {
    let tex = tex.as_deref();
    let settings = live_settings(store.as_deref());
    let root = commands.spawn((DespawnOnExit(AppState::Playing), abs_full())).id();

    // Dead Zone: a thin dark-red edge frame + barely-there tint. The real
    // static lives on the 3D phone's OLED screen.
    commands.spawn((
        ChildOf(root),
        HudBox::DeadZoneFrame,
        Node { border: UiRect::all(Val::Px(4.0)), ..abs_full() },
        BorderColor::all(Color::NONE),
        BackgroundColor(Color::NONE),
    ));
    // Damage flash overlay.
    commands.spawn((ChildOf(root), HudBox::DamageFlash, abs_full(), BackgroundColor(Color::NONE)));

    // Crosshair (user style + color) + hitmarker.
    let reticle = commands
        .spawn((
            ChildOf(root),
            HudCrosshair(settings.crosshair_style, settings.crosshair_color),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(CROSSHAIR_BOX),
                height: Val::Px(CROSSHAIR_BOX),
                margin: UiRect { left: Val::Px(-CROSSHAIR_BOX * 0.5), top: Val::Px(-CROSSHAIR_BOX * 0.5), ..default() },
                ..default()
            },
        ))
        .id();
    spawn_crosshair(&mut commands, reticle, settings.crosshair_style, settings.crosshair_color.color(), 1.0);
    commands.spawn((
        ChildOf(root),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            margin: UiRect { left: Val::Px(-11.0), top: Val::Px(-19.0), ..default() },
            ..default()
        },
        children![(HudText::Hitmarker, text("X", 34.0, Color::NONE))],
    ));

    // Top-left: floor + kills / jammers (+ the custom seed code when one is live).
    let plate = commands.spawn((
        ChildOf(root),
        hud_plate(
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(14.0),
                top: Val::Px(12.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            },
            HAZARD_YELLOW,
        ),
        children![
            (HudText::Floor, text("FLOOR 1/5", 20.0, COLOR_TEXT)),
            (HudText::Sector, text(sector_label(1), 11.0, HAZARD_YELLOW)),
            (HudText::Stats, text("KILLS 0  |  JAMMERS 0", 14.0, COLOR_DIM)),
            (
                Node { column_gap: Val::Px(7.0), align_items: AlignItems::Center, margin: UiRect::top(Val::Px(1.0)), ..default() },
                children![
                    (HudText::LoadoutChip, text("STOCK OS", 12.0, COLOR_DIM)),
                    (
                        HudBox::LoadoutDot,
                        Node { display: Display::None, width: Val::Px(5.0), height: Val::Px(5.0), ..default() },
                        BackgroundColor(COLOR_SIGNAL_AMBER),
                    ),
                    (HudText::LoadoutContracts, text("", 12.0, COLOR_SIGNAL_AMBER)),
                ],
            ),
        ],
    )).id();
    if run.custom_seed
        && let Some(code) = shareable_code(&run)
    {
        commands.spawn((ChildOf(plate), text(format!("SEED {code}"), 11.0, ARC_CYAN)));
    }

    // Top-center: status panel (+ SOS uplink), alert under it.
    let top = commands
        .spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                top: Val::Px(12.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .id();
    // The status plate sits between two tape rails that only show during the
    // SOS uplink (DEMON RUSH), when its left rail also flips to alert red.
    let rush = commands.spawn((ChildOf(top), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, ..default() })).id();
    let rail = hazard_strip(&mut commands, rush, tex, RUSH_RAIL_HEIGHT, Tape::Danger);
    commands.entity(rail).insert((HudBox::RushRail, Node { display: Display::None, width: Val::Percent(100.0), height: Val::Px(RUSH_RAIL_HEIGHT), ..default() }));
    let status = commands
        .spawn((
            ChildOf(rush),
            HudBox::StatusPlate,
            hud_plate(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(6.0), ..default() }, ARC_CYAN),
        ))
        .id();
    commands.spawn((ChildOf(status), HudText::Status, text("SEARCH FOR 5-BAR SIGNAL TOWER", 22.0, COLOR_SIGNAL_AMBER)));
    let uplink = commands
        .spawn((
            ChildOf(status),
            HudBox::UplinkPanel,
            Node { display: Display::None, flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(4.0), ..default() },
        ))
        .id();
    commands.spawn((ChildOf(uplink), HudText::UplinkLabel, text("UPLOADING SOS 0%", 19.0, COLOR_CYAN)));
    let gauge = commands
        .spawn((ChildOf(uplink), Node { width: Val::Px(UPLINK_WIDTH), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }))
        .id();
    let track = commands.spawn((ChildOf(gauge), bar_track(Val::Percent(100.0), 16.0, ARC_CYAN, Display::Flex))).id();
    commands.spawn((ChildOf(track), HudBox::UplinkFill, Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(COLOR_CYAN)));
    spawn_ticks(&mut commands, track);
    spawn_ruler(&mut commands, gauge);
    let rail = hazard_strip(&mut commands, rush, tex, RUSH_RAIL_HEIGHT, Tape::Danger);
    commands.entity(rail).insert((HudBox::RushRail, Node { display: Display::None, width: Val::Percent(100.0), height: Val::Px(RUSH_RAIL_HEIGHT), ..default() }));
    // Boss plate: docked under the status / uplink plate, above the alert.
    let boss = commands
        .spawn((
            ChildOf(top),
            HudBox::BossPlate,
            hud_plate(Node { display: Display::None, flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, PUNK_RED),
        ))
        .id();
    commands.spawn((
        ChildOf(boss),
        Node { width: Val::Px(BOSS_BAR_WIDTH), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() },
        children![
            (HudText::BossName, text(crate::enemies::EnemyKind::CarrierDaemon.name(), 18.0, HAZARD_YELLOW)),
            (HudText::BossPhase, text("PHASE I", 14.0, PUNK_RED)),
        ],
    ));
    let bgauge = commands
        .spawn((ChildOf(boss), Node { width: Val::Px(BOSS_BAR_WIDTH), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }))
        .id();
    let btrack = commands.spawn((ChildOf(bgauge), HudBox::BossTrack, bar_track(Val::Percent(100.0), 14.0, PUNK_RED, Display::Flex))).id();
    commands.spawn((ChildOf(btrack), HudBox::BossFill, Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(PUNK_RED)));
    // Segment cuts + heavy phase-threshold ticks at 30% / 65%.
    for n in 1..20 {
        let pct = n as f32 * 5.0;
        let major = BOSS_PHASE_TICKS.iter().any(|t| (t * 100.0 - pct).abs() < 0.1);
        commands.spawn((
            ChildOf(btrack),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(pct),
                top: Val::Px(0.0),
                width: Val::Px(if major { 3.0 } else { 1.0 }),
                height: Val::Percent(100.0),
                margin: UiRect::left(Val::Px(-1.0)),
                ..default()
            },
            BackgroundColor(if major { BONE_TEXT.with_alpha(0.9) } else { INK_BLACK.with_alpha(0.7) }),
        ));
    }
    // Ruler under the track: the phase thresholds stay readable on the empty
    // (dark) part of the bar, where the in-track cuts vanish.
    let bruler = commands.spawn((ChildOf(bgauge), Node { width: Val::Percent(100.0), height: Val::Px(RULER_HEIGHT), ..default() })).id();
    for n in 0..=20u8 {
        let pct = n as f32 * 5.0;
        let major = BOSS_PHASE_TICKS.iter().any(|t| (t * 100.0 - pct).abs() < 0.1);
        if !major && n % 2 == 1 {
            continue;
        }
        commands.spawn((
            ChildOf(bruler),
            Node {
                position_type: PositionType::Absolute,
                left: if n == 20 { Val::Auto } else { Val::Percent(pct) },
                right: if n == 20 { Val::Px(0.0) } else { Val::Auto },
                top: Val::Px(0.0),
                width: Val::Px(if major { 3.0 } else { 1.0 }),
                height: Val::Px(if major { RULER_HEIGHT } else { RULER_HEIGHT - 2.0 }),
                margin: UiRect::left(Val::Px(if major { -1.0 } else { 0.0 })),
                ..default()
            },
            BackgroundColor(if major { HAZARD_YELLOW } else { ASH_DIM.with_alpha(0.6) }),
        ));
    }
    commands.spawn((
        ChildOf(top),
        HudBox::AlertPanel,
        hud_plate(Node { display: Display::None, ..default() }, PUNK_RED),
        children![(HudText::Alert, text("", 30.0, COLOR_SIGNAL_AMBER))],
    ));

    // Bottom-center prompt (above the vitals panel).
    commands.spawn((
        ChildOf(root),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            // Clears the vitals plate, the docked VEL strip and its BHOP stamp.
            bottom: Val::Px(200.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        children![(HudText::Prompt, text("", 24.0, COLOR_SIGNAL_GREEN))],
    ));

    // Top-right: killfeed / status toasts.
    let feed = commands
        .spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(14.0),
                top: Val::Px(12.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(4.0),
                ..default()
            },
        ))
        .id();
    for i in 0..TOAST_SLOTS {
        commands.spawn((
            ChildOf(feed),
            HudBox::ToastChip(i),
            hud_plate(Node { display: Display::None, padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)), ..default() }, STEEL_BORDER),
            children![(HudText::Toast(i), text("", 15.0, COLOR_TEXT))],
        ));
    }

    // Right edge, below the toast stack: speedrun split timer plate.
    spawn_split_plate(&mut commands, root);

    // Bottom-center: VEL telemetry strip docked above the vitals panel.
    commands.insert_resource(VelGauge::default());
    spawn_vel_plate(&mut commands, root);

    // Bottom-center: vitals panel.
    let vitals = commands
        .spawn((
            ChildOf(root),
            hud_plate(
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    bottom: Val::Px(14.0),
                    width: Val::Px(VITALS_WIDTH),
                    margin: UiRect::left(Val::Px(-VITALS_WIDTH / 2.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(4.0),
                    padding: UiRect { left: Val::Px(12.0), right: Val::Px(12.0), top: Val::Px(7.0), bottom: Val::Px(8.0) },
                    ..default()
                },
                PUNK_RED,
            ),
        ))
        .id();
    spawn_bar(&mut commands, vitals, "HP", COLOR_TEXT, HudText::HpValue, HudBox::HpFill, 14.0, COLOR_HP_GREEN);
    spawn_bar(&mut commands, vitals, "ARMOR", COLOR_ARMOR, HudText::ArmorValue, HudBox::ArmorFill, 8.0, COLOR_ARMOR);
    spawn_bar(&mut commands, vitals, "STAMINA [SHIFT]", COLOR_DIM, HudText::StaminaValue, HudBox::StaminaFill, 6.0, COLOR_GOLD);

    // Bottom-right: weapon & ammo deck.
    let deck = commands
        .spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(14.0),
                bottom: Val::Px(14.0),
                width: Val::Px(DECK_WIDTH),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                ..default()
            },
        ))
        .id();
    let card = commands
        .spawn((
            ChildOf(deck),
            HudBox::WeaponCard,
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::axes(Val::Px(CARD_PAD_X), Val::Px(8.0)),
                border: plate_rect(CARD_BORDER),
                ..default()
            },
            plate_border(COLOR_SHOTGUN, PANEL_EDGE),
            BackgroundColor(PANEL_BG),
        ))
        .id();
    commands.spawn((
        ChildOf(card),
        Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, column_gap: Val::Px(8.0), min_height: Val::Px(20.0), ..default() },
        children![
            (HudText::WeaponName, text("SHOTGUN", WEAPON_TITLE_FONT, COLOR_SHOTGUN), TextLayout::no_wrap()),
            // Buff pills dock right as one cluster, so one or both lit never
            // shifts the title or collides with it.
            (
                Node { column_gap: Val::Px(PILL_GAP), align_items: AlignItems::Center, flex_shrink: 0.0, ..default() },
                children![buff_pill(HudBox::OverclockPill, OVERCLOCK_PILL, ACID_GREEN), buff_pill(HudBox::DopplerPill, DOPPLER_PILL, COLOR_ROCKET)],
            ),
        ],
    ));
    commands.spawn((
        ChildOf(card),
        Node { align_items: AlignItems::Baseline, column_gap: Val::Px(6.0), ..default() },
        children![(HudText::AmmoCount, text("0", 38.0, COLOR_TEXT)), (HudText::AmmoMax, text("/ 0", 17.0, COLOR_DIM))],
    ));
    let pips = commands
        .spawn((ChildOf(card), HudBox::ShellRow, Node { column_gap: Val::Px(4.0), height: Val::Px(14.0), ..default() }))
        .id();
    for i in 0..SHELL_PIPS {
        commands.spawn((
            ChildOf(pips),
            HudBox::ShellPip(i),
            Node {
                width: Val::Px(20.0),
                height: Val::Px(14.0),
                border: UiRect::top(Val::Px(4.0)),
                ..default()
            },
            BorderColor::all(COLOR_BRASS),
            BackgroundColor(COLOR_SHELL),
        ));
    }
    let ptrack = commands
        .spawn((
            ChildOf(card),
            HudBox::PlasmaTrack,
            bar_track(Val::Percent(100.0), 14.0, COLOR_CYAN, Display::None),
        ))
        .id();
    commands.spawn((
        ChildOf(ptrack),
        HudBox::PlasmaFill,
        Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
        BackgroundColor(COLOR_CYAN),
    ));
    spawn_ticks(&mut commands, ptrack);
    // Signal-Mortar rack: steel warhead pips with a glowing amber nose.
    let rockets = commands
        .spawn((
            ChildOf(card),
            HudBox::RocketRow,
            Node { display: Display::None, column_gap: Val::Px(4.0), height: Val::Px(14.0), ..default() },
        ))
        .id();
    for i in 0..ROCKET_PIPS {
        commands.spawn((
            ChildOf(rockets),
            HudBox::RocketPip(i),
            Node { width: Val::Px(20.0), height: Val::Px(14.0), border: UiRect::left(Val::Px(5.0)), ..default() },
            BorderColor::all(COLOR_WARHEAD_TIP),
            BackgroundColor(COLOR_WARHEAD),
        ));
    }
    // Slot deck: three identical two-row telemetry badges. Row 1 is
    // `[keycap] NAME ...... reserve` on one never-wrapping line, row 2 a thin
    // reserve gauge, so every chip keeps the same height and alignment
    // whatever the ammo digits are.
    let slots = commands.spawn((ChildOf(deck), Node { column_gap: Val::Px(SLOT_GAP), ..default() })).id();
    for i in 0..Weapon::ALL.len() as u8 {
        let chip = commands
            .spawn((
                ChildOf(slots),
                HudBox::SlotChip(i),
                Node {
                    flex_grow: 1.0,
                    flex_basis: Val::Px(0.0),
                    min_width: Val::Px(0.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(3.0),
                    padding: UiRect::new(Val::Px(SLOT_PAD_X), Val::Px(SLOT_PAD_X), Val::Px(3.0), Val::Px(4.0)),
                    border: plate_rect(SLOT_BORDER),
                    overflow: Overflow::clip(),
                    ..default()
                },
                plate_border(PANEL_EDGE, PANEL_EDGE),
                BackgroundColor(PANEL_BG),
            ))
            .id();
        commands.spawn((
            ChildOf(chip),
            Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, column_gap: Val::Px(SLOT_INNER_GAP), ..default() },
            children![
                (
                    Node { align_items: AlignItems::Center, column_gap: Val::Px(SLOT_INNER_GAP), ..default() },
                    children![
                        (
                            HudBox::SlotKey(i),
                            Node {
                                width: Val::Px(SLOT_KEY_WIDTH),
                                height: Val::Px(13.0),
                                justify_content: JustifyContent::Center,
                                align_items: AlignItems::Center,
                                border: UiRect::bottom(Val::Px(2.0)),
                                flex_shrink: 0.0,
                                ..default()
                            },
                            BorderColor::all(INK_BLACK),
                            BackgroundColor(COLOR_PIP_OFF),
                            children![(HudText::SlotKey(i), text((i + 1).to_string(), SLOT_LABEL_FONT, COLOR_DIM), TextLayout::no_wrap())],
                        ),
                        (HudText::Slot(i), text("", SLOT_LABEL_FONT, COLOR_DIM), TextLayout::no_wrap()),
                    ],
                ),
                (HudText::SlotAmmo(i), text("", SLOT_AMMO_FONT, COLOR_DIM), TextLayout::no_wrap()),
            ],
        ));
        commands.spawn((
            ChildOf(chip),
            Node { width: Val::Percent(100.0), height: Val::Px(3.0), ..default() },
            BackgroundColor(TRACK_BG),
            children![(HudBox::SlotFill(i), Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(COLOR_PIP_OFF))],
        ));
    }
}

/// Stamped buff pill of the weapon card header (hidden until its buff is live).
/// Split timer plate fields.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitText {
    Tag,
    Time,
    Progress,
    Delta,
}

/// Root of the split timer plate (hidden unless splits are on / non-standard mode).
#[derive(Component)]
pub struct SplitPlate;

/// Top of the split plate: clears the full 5-slot toast stack (12 px + 5 x
/// ~39 px chips, last one ending near 200 px at 720p) with a 14 px gap.
pub const SPLIT_PLATE_TOP: f32 = 214.0;
pub const SPLIT_PLATE_WIDTH: f32 = 236.0;

fn spawn_split_plate(commands: &mut Commands, root: Entity) {
    commands.spawn((
        ChildOf(root),
        SplitPlate,
        hud_plate(
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                right: Val::Px(14.0),
                top: Val::Px(SPLIT_PLATE_TOP),
                width: Val::Px(SPLIT_PLATE_WIDTH),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(1.0),
                ..default()
            },
            ARC_CYAN,
        ),
        children![
            (SplitText::Tag, text("SPLITS", 11.0, ARC_CYAN)),
            (SplitText::Time, text("00:00.0", 30.0, COLOR_TEXT)),
            (SplitText::Progress, text("", 12.0, COLOR_DIM)),
            (SplitText::Delta, text("", 15.0, COLOR_TEXT)),
        ],
    ));
}

/// Live split timer: `RunStats::time_elapsed` / `TimeTrialState::elapsed`
/// only advance in `Playing`, so the plate freezes with the simulation.
fn update_split_plate(
    run: Res<RunStats>,
    trial: Res<TimeTrialState>,
    store: Option<Res<ProfileStore>>,
    mut plates: Query<(&mut Node, &mut BorderColor), With<SplitPlate>>,
    mut texts: Query<(&SplitText, &mut Text, &mut TextColor)>,
) {
    let settings = live_settings(store.as_deref());
    let show = split_timer_visible(&settings, run.mode);
    for (mut node, mut border) in &mut plates {
        set_display(&mut node, show);
        let rail = match run.mode {
            RunMode::Standard => ACID_GREEN,
            RunMode::Daily(_) => HAZARD_YELLOW,
            RunMode::TimeTrial => ARC_CYAN,
        };
        let want = plate_border(rail, STEEL_BORDER.with_alpha(0.45));
        if *border != want {
            *border = want;
        }
    }
    if !show {
        return;
    }
    let default_profile;
    let profile = match store.as_deref() {
        Some(s) => &s.profile,
        None => {
            default_profile = crate::meta::PlayerProfile::default();
            &default_profile
        }
    };
    let v = split_plate_view(&run, &trial, profile);
    for (field, mut t, mut c) in &mut texts {
        let (want, color) = match field {
            SplitText::Tag => (&v.tag, v.tag_color),
            SplitText::Time => (&v.time, COLOR_TEXT),
            SplitText::Progress => (&v.progress, COLOR_DIM),
            SplitText::Delta => (&v.delta, v.delta_color),
        };
        if t.0 != *want {
            t.0.clone_from(want);
        }
        if c.0 != color {
            c.0 = color;
        }
    }
}

fn buff_pill(tag: HudBox, label: &'static str, color: Color) -> impl Bundle {
    (
        tag,
        Node { display: Display::None, padding: UiRect::axes(Val::Px(PILL_PAD_X), Val::Px(1.0)), border: UiRect::bottom(Val::Px(2.0)), flex_shrink: 0.0, ..default() },
        BorderColor::all(INK_BLACK),
        BackgroundColor(color),
        children![(text(label, PILL_FONT, INK_BLACK), TextLayout::no_wrap())],
    )
}

/// Cached `(chip tag, contract tags)` of the run loadout.
type LoadoutTags = Option<(String, Option<String>)>;

#[allow(clippy::too_many_arguments)]
fn update_hud(
    signal: Res<SignalState>,
    run: Res<RunStats>,
    time: Res<Time>,
    feed: Res<HudFeed>,
    ws: Res<WeaponState>,
    ext: Res<ExtractionState>,
    loadout: Res<RunLoadout>,
    gauge: Res<VelGauge>,
    boss: Option<Res<BossHud>>,
    trial: Res<TimeTrialState>,
    players: Query<&Player>,
    mut texts: Query<(&HudText, &mut Text, &mut TextColor)>,
    (mut buf, mut tags): (Local<String>, Local<LoadoutTags>),
) {
    let t = time.elapsed_secs();
    let Ok(p) = players.single() else { return };
    // Loadout tags only change with the loadout: format them once.
    if loadout.is_changed() || tags.is_none() {
        *tags = Some(loadout_tag_parts(&loadout));
    }
    let Some((chip_tag, contract_tags)) = tags.as_ref() else { return };
    let contract_color = loadout_contract_color(loadout.contract_count());
    let accent = weapon_accent(ws.current);
    let (cur, max) = ammo(p, ws.current);
    for (slot, mut txt, mut col) in &mut texts {
        // Every label is written into one reused buffer; `Text` / `TextColor`
        // are only touched (marked changed) when the value really differs.
        let b = &mut *buf;
        b.clear();
        let c: Color = match *slot {
            HudText::BossName => {
                b.push_str(crate::enemies::EnemyKind::CarrierDaemon.name());
                HAZARD_YELLOW
            }
            HudText::BossPhase => {
                let h = boss.as_deref().cloned().unwrap_or_default();
                b.push_str(&h.tag());
                boss_tag_color(&h)
            }
            HudText::Floor => {
                if run.mode == RunMode::TimeTrial {
                    b.push_str("TIME TRIAL");
                    ARC_CYAN
                } else if run.endless {
                    let _ = write!(b, "ENDLESS FL {}", run.floor);
                    COLOR_ENDLESS
                } else {
                    let _ = write!(b, "FLOOR {}/{}", run.floor, FINAL_FLOOR);
                    COLOR_TEXT
                }
            }
            HudText::Sector if run.mode == RunMode::TimeTrial => {
                b.push_str("COURSE // SIGNAL RUN");
                HAZARD_YELLOW
            }
            HudText::Stats if run.mode == RunMode::TimeTrial => {
                let _ = write!(b, "CP {}/{}  |  RESCUES {}", trial.next_gate.min(3), crate::trials::TRIAL_CHECKPOINTS, trial.rescues);
                COLOR_DIM
            }
            HudText::Sector => {
                let _ = write!(b, "SECTOR {:02} // {}", run.floor, FloorTheme::for_floor(run.floor).name());
                HAZARD_YELLOW
            }
            HudText::Stats => {
                let _ = write!(b, "KILLS {}  |  JAMMERS {}", run.kills, run.jammers_destroyed);
                COLOR_DIM
            }
            HudText::LoadoutChip => {
                b.push_str(chip_tag);
                if loadout.chip == crate::meta::StarterChip::StockUac { COLOR_DIM } else { COLOR_CYAN.with_alpha(0.9) }
            }
            HudText::LoadoutContracts => {
                b.push_str(contract_tags.as_deref().unwrap_or_default());
                contract_color
            }
            HudText::Status if run.mode == RunMode::TimeTrial => {
                b.push_str(if trial.next_gate >= crate::trials::TRIAL_CHECKPOINTS {
                    "FINAL STRETCH - LAND ON THE TOWER PAD"
                } else {
                    "TIME TRIAL - 3 CHECKPOINTS, THEN THE TOWER"
                });
                ARC_CYAN
            }
            HudText::Status => {
                let (s, c) = match *ext {
                    ExtractionState::Uploading { .. } => (uplink_status_text(&ext), COLOR_SIGNAL_RED),
                    ExtractionState::Extracting { .. } | ExtractionState::Complete => ("ORBITAL EXTRACTION IN PROGRESS", COLOR_CYAN),
                    _ if signal.near_tower => ("5-BAR SIGNAL - EXTRACTION TOWER IN RANGE", COLOR_SIGNAL_GREEN),
                    _ if signal.in_dead_zone => ("NO SERVICE - DEAD ZONE", COLOR_SIGNAL_RED),
                    _ => ("SEARCH FOR 5-BAR SIGNAL TOWER", COLOR_SIGNAL_AMBER),
                };
                b.push_str(s);
                c
            }
            HudText::Prompt => match *ext {
                _ if run.mode == RunMode::TimeTrial => Color::NONE,
                ExtractionState::Searching { hold } if signal.near_tower => {
                    let a = 0.6 + 0.4 * (t * 5.0).sin();
                    b.push_str("PRESS [E] TO TRIGGER SOS UPLINK");
                    if hold > 0.0 {
                        let _ = write!(b, "  ({:.0}%)", hold / UPLINK_HOLD_SECS * 100.0);
                    }
                    COLOR_SIGNAL_GREEN.with_alpha(a)
                }
                _ => Color::NONE,
            },
            HudText::Alert => match &feed.alert {
                Some(a) => {
                    b.push_str(&a.text);
                    a.color.with_alpha(if (t * 8.0).sin() > -0.3 { 1.0 } else { 0.35 })
                }
                None => Color::NONE,
            },
            HudText::VelValue => {
                b.push_str(&speed_label(gauge.speed));
                gauge.value_color()
            }
            HudText::VelChain => {
                if let Some(label) = bhop_chain_label(p.bhop_chain) {
                    b.push_str(&label);
                }
                INK_BLACK
            }
            HudText::UplinkLabel => match *ext {
                ExtractionState::Uploading { progress, boss_hold: true, .. } => {
                    let _ = write!(b, "UPLINK HELD {:.0}% - KILL THE CARRIER DAEMON", progress * 100.0);
                    COLOR_SIGNAL_AMBER.with_alpha(if (t * 6.0).sin() > 0.0 { 1.0 } else { 0.55 })
                }
                ExtractionState::Uploading { progress, jammed: true, .. } => {
                    let _ = write!(b, "UPLINK JAMMED! KILL THE JAMMER FIEND  {:.0}%", progress * 100.0);
                    COLOR_SIGNAL_RED.with_alpha(if (t * 10.0).sin() > 0.0 { 1.0 } else { 0.4 })
                }
                ExtractionState::Uploading { progress, in_ring: false, .. } => {
                    let _ = write!(b, "RETURN TO SIGNAL RING!  {:.0}%", progress * 100.0);
                    COLOR_SIGNAL_AMBER
                }
                ExtractionState::Uploading { progress, .. } => {
                    let _ = write!(b, "UPLOADING SOS {:.0}%", progress * 100.0);
                    COLOR_CYAN
                }
                _ => COLOR_CYAN,
            },
            HudText::HpValue => {
                let _ = write!(b, "{}/{}", p.hp.ceil().max(0.0) as i32, p.max_hp as i32);
                if hp_band(fraction(p.hp, p.max_hp)) == HpBand::Critical { hp_color(0.0, t) } else { COLOR_TEXT }
            }
            HudText::ArmorValue => {
                let _ = write!(b, "{}/{}", p.armor.ceil().max(0.0) as i32, p.max_armor as i32);
                COLOR_ARMOR
            }
            HudText::StaminaValue => {
                if p.dash_ready() {
                    b.push_str("READY");
                    COLOR_GOLD
                } else {
                    let _ = write!(b, "{:.1}s", p.dash_cooldown);
                    COLOR_CYAN
                }
            }
            HudText::WeaponName => {
                b.push_str(weapon_title(ws.current));
                accent
            }
            HudText::AmmoCount => {
                let _ = write!(b, "{cur}");
                if cur == 0 { COLOR_SIGNAL_RED } else { COLOR_TEXT }
            }
            HudText::AmmoMax => {
                let _ = write!(b, "/ {max}");
                COLOR_DIM
            }
            HudText::Slot(i) => {
                let w = Weapon::ALL[i as usize];
                b.push_str(weapon_slot_name(w));
                if ws.current == w { weapon_accent(w) } else { COLOR_DIM }
            }
            HudText::SlotKey(i) => {
                let _ = write!(b, "{}", i + 1);
                if ws.current == Weapon::ALL[i as usize] { INK_BLACK } else { COLOR_DIM }
            }
            HudText::SlotAmmo(i) => {
                let w = Weapon::ALL[i as usize];
                let (n, _) = ammo(p, w);
                let _ = write!(b, "{n}");
                slot_ammo_color(n, ws.current == w)
            }
            HudText::Toast(i) => match feed.toasts.iter().rev().nth(i as usize) {
                Some(toast) => {
                    b.push_str(&toast.text);
                    toast.color.with_alpha((toast.timer / 0.6).min(1.0))
                }
                None => Color::NONE,
            },
            HudText::Hitmarker => {
                let a = (feed.hitmarker / 0.18).clamp(0.0, 1.0);
                b.push('X');
                if feed.hit_kill { COLOR_SIGNAL_RED.with_alpha(a) } else { Color::srgba(1.0, 1.0, 1.0, a) }
            }
        };
        if txt.0 != *b {
            txt.0.clone_from(b);
        }
        if col.0 != c {
            col.0 = c;
        }
    }
}

/// HUD contract-tag accent: amber, red at 3+ contracts.
fn loadout_contract_color(contracts: u32) -> Color {
    if contracts >= 3 { COLOR_SIGNAL_RED } else { COLOR_SIGNAL_AMBER }
}

/// Status plate frame: calm cyan rail while searching, a full alert-red frame
/// while the SOS uplink (DEMON RUSH) is running.
fn status_plate_border(ext: &ExtractionState) -> BorderColor {
    if matches!(ext, ExtractionState::Uploading { .. }) {
        plate_border(PUNK_RED, PUNK_RED.with_alpha(0.55))
    } else {
        plate_border(ARC_CYAN, STEEL_BORDER.with_alpha(0.45))
    }
}

/// Change-guarded writes: `DerefMut` (and the UI relayout it triggers) only
/// happens when the value really changes.
fn set_display(node: &mut Mut<Node>, show: bool) {
    let want = if show { Display::Flex } else { Display::None };
    if node.display != want {
        node.display = want;
    }
}

fn set_width_pct(node: &mut Mut<Node>, pct: f32) {
    let want = Val::Percent(pct);
    if node.width != want {
        node.width = want;
    }
}

fn set_bg(bg: &mut Mut<BackgroundColor>, c: Color) {
    if bg.0 != c {
        bg.0 = c;
    }
}

#[allow(clippy::too_many_arguments)]
fn update_hud_boxes(
    signal: Res<SignalState>,
    time: Res<Time>,
    ext: Res<ExtractionState>,
    feed: Res<HudFeed>,
    ws: Res<WeaponState>,
    mods: Res<PlayerModifiers>,
    loadout: Res<RunLoadout>,
    gauge: Res<VelGauge>,
    boss: Option<Res<BossHud>>,
    players: Query<&Player>,
    mut boxes: Query<(&HudBox, &mut Node, &mut BackgroundColor, Option<&mut BorderColor>)>,
) {
    let t = time.elapsed_secs();
    let Ok(p) = players.single() else { return };
    let accent = weapon_accent(ws.current);
    let shotgun = ws.current == Weapon::Shotgun;
    let (cur, max) = ammo(p, ws.current);
    let lit = lit_pips(p.ammo_shells, p.max_shells, SHELL_PIPS);
    let rocket_lit = lit_pips(p.ammo_rockets, p.max_rockets, ROCKET_PIPS);
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    let stamina = stamina_fill(p.dash_cooldown, DASH_COOLDOWN * mods.dash_cooldown_mult);
    let vel_lit = speed_segments(gauge.speed);
    let tier_color = gauge.color();
    let toast_look = |i: u8| feed.toasts.iter().rev().nth(i as usize).map(|toast| ((toast.timer / 0.6).min(1.0), toast.color));
    for (b, mut node, mut bg, border) in &mut boxes {
        let set_border = |c: BorderColor| {
            if let Some(mut bc) = border
                && *bc != c
            {
                *bc = c;
            }
        };
        match *b {
            HudBox::DamageFlash => set_bg(&mut bg, Color::srgba(0.8, 0.0, 0.0, (0.45 - p.since_damage).max(0.0))),
            HudBox::DeadZoneFrame => {
                if signal.in_dead_zone {
                    let strength = if signal.bars == 0 { 1.0 } else { 0.6 };
                    set_border(BorderColor::all(Color::srgba(0.55, 0.03, 0.03, strength * (0.4 + 0.2 * (t * 3.0).sin()))));
                    set_bg(&mut bg, Color::srgba(0.4, 0.0, 0.0, 0.03 * strength));
                } else {
                    set_border(BorderColor::all(Color::NONE));
                    set_bg(&mut bg, Color::NONE);
                }
            }
            HudBox::StatusPlate => set_border(status_plate_border(&ext)),
            HudBox::RushRail => set_display(&mut node, matches!(*ext, ExtractionState::Uploading { .. })),
            HudBox::UplinkPanel => set_display(&mut node, matches!(*ext, ExtractionState::Uploading { .. })),
            HudBox::UplinkFill => {
                if let ExtractionState::Uploading { progress, jammed, in_ring, .. } = *ext {
                    set_width_pct(&mut node, progress * 100.0);
                    let c = if jammed {
                        COLOR_SIGNAL_RED.with_alpha(if (t * 10.0).sin() > 0.0 { 1.0 } else { 0.4 })
                    } else if !in_ring {
                        COLOR_SIGNAL_AMBER
                    } else {
                        COLOR_CYAN
                    };
                    set_bg(&mut bg, c);
                }
            }
            HudBox::AlertPanel => set_display(&mut node, feed.alert.is_some()),
            HudBox::BossPlate => {
                let h = boss.as_deref().cloned().unwrap_or_default();
                set_display(&mut node, h.active);
                if h.active {
                    let c = boss_tag_color(&h);
                    set_border(plate_border(c, STEEL_BORDER.with_alpha(0.45)));
                }
            }
            HudBox::BossTrack => {
                if let Some(h) = boss.as_deref().filter(|h| h.active) {
                    let c = boss_tag_color(h);
                    set_border(BorderColor { left: c, right: c.with_alpha(0.55), top: TRACK_BORDER, bottom: INK_BLACK });
                }
            }
            HudBox::BossFill => {
                if let Some(h) = boss.as_deref().filter(|h| h.active) {
                    set_width_pct(&mut node, h.hp_frac * 100.0);
                    set_bg(&mut bg, boss_bar_color(h, t));
                }
            }
            HudBox::LoadoutDot => {
                set_display(&mut node, loadout.contract_count() > 0);
                set_bg(&mut bg, loadout_contract_color(loadout.contract_count()));
            }
            HudBox::ToastChip(i) => {
                let look = toast_look(i);
                set_display(&mut node, look.is_some());
                let (a, c) = look.unwrap_or((0.0, Color::NONE));
                set_bg(&mut bg, PANEL_BG.with_alpha(0.8 * a));
                set_border(plate_border(c.with_alpha(a), PANEL_EDGE.with_alpha(0.45 * a)));
            }
            HudBox::VelPlate => set_border(plate_border(tier_color, STEEL_BORDER.with_alpha(0.45))),
            HudBox::VelTrack => set_border(BorderColor { left: tier_color, right: tier_color.with_alpha(0.55), top: TRACK_BORDER, bottom: INK_BLACK }),
            HudBox::VelValueBadge => set_bg(&mut bg, gauge.badge_color()),
            HudBox::VelChainTag => {
                set_display(&mut node, bhop_chain_label(p.bhop_chain).is_some());
                set_bg(&mut bg, tier_color);
            }
            HudBox::VelSeg(i) => set_bg(&mut bg, segment_color(i, vel_lit)),
            HudBox::HpFill => {
                let f = fraction(p.hp, p.max_hp);
                set_width_pct(&mut node, f * 100.0);
                set_bg(&mut bg, hp_color(f, t));
            }
            HudBox::ArmorFill => set_width_pct(&mut node, fraction(p.armor, p.max_armor) * 100.0),
            HudBox::StaminaFill => {
                set_width_pct(&mut node, stamina * 100.0);
                set_bg(&mut bg, if stamina >= 1.0 { COLOR_GOLD } else { COLOR_CYAN.with_alpha(0.75) });
            }
            HudBox::ShellRow => set_display(&mut node, shotgun),
            HudBox::ShellPip(i) => {
                let on = i < lit;
                set_bg(&mut bg, if on { COLOR_SHELL } else { COLOR_PIP_OFF });
                set_border(BorderColor::all(if on { COLOR_BRASS } else { COLOR_PIP_OFF_BORDER }));
            }
            HudBox::PlasmaTrack => set_display(&mut node, ws.current == Weapon::Plasma),
            HudBox::RocketRow => set_display(&mut node, ws.current == Weapon::Rocket),
            HudBox::RocketPip(i) => {
                let on = i < rocket_lit;
                set_bg(&mut bg, if on { COLOR_WARHEAD } else { COLOR_PIP_OFF });
                set_border(BorderColor::all(if on { COLOR_WARHEAD_TIP } else { COLOR_PIP_OFF_BORDER }));
            }
            HudBox::PlasmaFill => {
                set_width_pct(&mut node, fraction(cur as f32, max as f32) * 100.0);
                set_bg(&mut bg, COLOR_CYAN.with_alpha(0.8 + 0.2 * (t * 4.0).sin()));
            }
            HudBox::WeaponCard => set_border(plate_border(accent, PANEL_EDGE)),
            HudBox::SlotChip(i) => {
                let w = Weapon::ALL[i as usize];
                let active = ws.current == w;
                set_border(if active { plate_border(weapon_accent(w), weapon_accent(w).with_alpha(0.5)) } else { plate_border(PANEL_EDGE, PANEL_EDGE) });
                set_bg(&mut bg, if active { weapon_accent(w).with_alpha(0.18) } else { PANEL_BG });
            }
            HudBox::SlotKey(i) => {
                let w = Weapon::ALL[i as usize];
                set_bg(&mut bg, if ws.current == w { weapon_accent(w) } else { COLOR_PIP_OFF });
            }
            HudBox::SlotFill(i) => {
                let w = Weapon::ALL[i as usize];
                let (n, m) = ammo(p, w);
                set_width_pct(&mut node, fraction(n as f32, m as f32) * 100.0);
                set_bg(&mut bg, weapon_accent(w).with_alpha(if ws.current == w { 1.0 } else { 0.4 }));
            }
            HudBox::OverclockPill => set_display(&mut node, overclock_active(signal.bars, mods.overclock_bonus)),
            HudBox::DopplerPill => set_display(&mut node, doppler_active(speed, &mods)),
        }
    }
}

// ---------------------------------------------------------------------------
// Crosshair
// ---------------------------------------------------------------------------

/// Side (px) of the reticle box centered on the screen.
pub const CROSSHAIR_BOX: f32 = 32.0;

/// The HUD reticle container and the style / color it was built with.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudCrosshair(pub CrosshairStyle, pub CrosshairColor);

/// Thin ink outline so every reticle color reads over bright fire and sky.
const RETICLE_OUTLINE: Color = Color::srgba(0.0, 0.0, 0.0, 0.55);

/// One reticle stroke: an absolutely placed bar centered on `(cx, cy)`.
fn reticle_bar(cx: f32, cy: f32, w: f32, h: f32, color: Color, rot_deg: f32) -> impl Bundle {
    (
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(cx - w * 0.5),
            top: Val::Px(cy - h * 0.5),
            width: Val::Px(w),
            height: Val::Px(h),
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BorderColor::all(RETICLE_OUTLINE),
        BackgroundColor(color),
        UiTransform::from_rotation(Rot2::degrees(rot_deg)),
    )
}

/// Build a reticle of `style` inside `parent` (a box `CROSSHAIR_BOX * scale`
/// px square). Shared by the HUD and the Settings live preview.
pub fn spawn_crosshair(commands: &mut Commands, parent: Entity, style: CrosshairStyle, color: Color, scale: f32) {
    let c = CROSSHAIR_BOX * scale * 0.5;
    let k = scale;
    match style {
        CrosshairStyle::Cross => {
            let (len, thick, gap) = (8.0 * k, 3.0 * k, 4.0 * k);
            let off = gap + len * 0.5;
            for (dx, dy, w, h) in [(-off, 0.0, len, thick), (off, 0.0, len, thick), (0.0, -off, thick, len), (0.0, off, thick, len)] {
                commands.spawn((ChildOf(parent), reticle_bar(c + dx, c + dy, w, h, color, 0.0)));
            }
        }
        CrosshairStyle::Dot => {
            let d = 6.0 * k;
            commands.spawn((
                ChildOf(parent),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(c - d * 0.5),
                    top: Val::Px(c - d * 0.5),
                    width: Val::Px(d),
                    height: Val::Px(d),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BorderColor::all(RETICLE_OUTLINE),
                BackgroundColor(color),
            ));
        }
        CrosshairStyle::Circle => {
            let d = 20.0 * k;
            commands.spawn((
                ChildOf(parent),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(c - d * 0.5),
                    top: Val::Px(c - d * 0.5),
                    width: Val::Px(d),
                    height: Val::Px(d),
                    border: UiRect::all(Val::Px(2.0 * k)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BorderColor::all(color),
                BackgroundColor(Color::NONE),
            ));
            commands.spawn((ChildOf(parent), reticle_bar(c, c, 3.0 * k, 3.0 * k, color, 0.0)));
        }
        CrosshairStyle::Chevron => {
            // A "^" whose apex sits on the aim point (screen Y points down, so
            // left arm tilts -45 deg up-right and right arm tilts +45 deg down-right).
            let (len, thick) = (11.0 * k, 3.0 * k);
            let o = len * 0.5 * std::f32::consts::FRAC_1_SQRT_2;
            commands.spawn((ChildOf(parent), reticle_bar(c - o, c + o, len, thick, color, -45.0)));
            commands.spawn((ChildOf(parent), reticle_bar(c + o, c + o, len, thick, color, 45.0)));
            commands.spawn((ChildOf(parent), reticle_bar(c, c, 2.0 * k, 2.0 * k, color, 0.0)));
        }
    }
}

/// Rebuild the HUD reticle whenever the profile's reticle settings differ
/// from the ones it was built with (a no-op compare otherwise).
fn refresh_hud_crosshair(mut commands: Commands, store: Option<Res<ProfileStore>>, mut q: Query<(Entity, &mut HudCrosshair)>) {
    let s = live_settings(store.as_deref());
    for (e, mut cur) in &mut q {
        let want = HudCrosshair(s.crosshair_style, s.crosshair_color);
        if *cur != want {
            *cur = want;
            commands.entity(e).despawn_children();
            spawn_crosshair(&mut commands, e, want.0, want.1.color(), 1.0);
        }
    }
}

/// Esc or P during play pauses the run and opens the pause overlay.
fn pause_input(keys: Res<ButtonInput<KeyCode>>, mut next: ResMut<NextState<AppState>>) {
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::KeyP) {
        next.set(AppState::Paused);
    }
}

/// F9: debug-complete the current floor (exercises the floor transition flow).
/// Marks the run as debug-skipped so it cannot award permanent feats or stats.
fn debug_skip_floor(keys: Res<ButtonInput<KeyCode>>, mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>) {
    if keys.just_pressed(KeyCode::F9) && run.mode != RunMode::TimeTrial {
        run.debug_skipped = true;
        complete_floor(&mut run, &mut next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_input_hint_only_on_android() {
        assert_eq!(platform_input_hint(false), None);
        assert_eq!(platform_input_hint(true), Some(ANDROID_INPUT_HINT));
        assert!(ANDROID_INPUT_HINT.contains("NO TOUCH CONTROLS"));
        assert!(ANDROID_INPUT_HINT.is_ascii(), "default font glyphs only");
        assert_eq!(platform_input_hint(crate::perf::IS_ANDROID).is_some(), crate::perf::IS_ANDROID);
    }

    #[test]
    fn pad_menu_shortcuts_per_screen() {
        use MenuAction::*;
        assert_eq!(pad_menu_action(AppState::MainMenu, true, false), Some(StartRun));
        assert_eq!(pad_menu_action(AppState::MainMenu, false, true), None, "B on the main menu does nothing");
        assert_eq!(pad_menu_action(AppState::Loadout, true, false), Some(StartRun));
        assert_eq!(pad_menu_action(AppState::ContractsFeats, false, true), Some(BackToMenu));
        assert_eq!(pad_menu_action(AppState::Paused, false, true), Some(Continue));
        assert_eq!(pad_menu_action(AppState::Paused, true, false), None, "Start resumes via the core toggle");
        assert_eq!(pad_menu_action(AppState::GameOver, false, true), Some(BackToMenu));
        assert_eq!(pad_menu_action(AppState::Victory, false, true), Some(BackToMenu));
        assert_eq!(pad_menu_action(AppState::Settings, false, true), None, "Settings owns its B");
        assert_eq!(pad_menu_action(AppState::UpgradeSelect, true, true), None);
        assert_eq!(pad_menu_action(AppState::Playing, true, true), None);
    }

    #[test]
    fn endless_floor_plate_and_movement_line() {
        let mut run = RunStats { floor: 3, peak_speed: 27.46, max_bhop_chain: 7, ..default() };
        assert_eq!(floor_label(&run), "FLOOR 3/5");
        run.endless = true;
        run.floor = 8;
        assert_eq!(floor_label(&run), "ENDLESS FL 8");
        assert!(summary(&run).starts_with("ENDLESS FL 8"));
        assert_eq!(movement_line(&run), "PEAK SPEED 27.5 M/S  |  MAX BHOP CHAIN x7");
    }

    #[test]
    fn floor_plate_names_the_sector_theme() {
        assert_eq!(sector_label(1), "SECTOR 01 // RUST BUNKER");
        assert_eq!(sector_label(2), "SECTOR 02 // TOXIC COOLANT");
        assert_eq!(sector_label(3), "SECTOR 03 // CRYO RELAY");
        assert_eq!(sector_label(4), "SECTOR 04 // MAGMA FOUNDRY");
        assert_eq!(sector_label(5), "SECTOR 05 // OBSIDIAN CORE");
        assert_eq!(sector_label(6), "SECTOR 06 // RUST BUNKER");
    }

    #[test]
    fn signal_colors_by_band() {
        assert_eq!(signal_color(0), COLOR_SIGNAL_RED);
        assert_eq!(signal_color(3), COLOR_SIGNAL_AMBER);
        assert_eq!(signal_color(5), COLOR_SIGNAL_GREEN);
    }

    #[test]
    fn installed_list_names_upgrades() {
        let mut run = RunStats::default();
        assert!(installed_list(&run).contains("none"));
        run.upgrades.push(crate::state::UpgradeId::EmpOvercharge);
        assert!(installed_list(&run).contains("EMP Overcharge"));
    }

    #[test]
    fn bar_fraction_clamps() {
        assert_eq!(fraction(25.0, 100.0), 0.25);
        assert_eq!(fraction(150.0, 100.0), 1.0);
        assert_eq!(fraction(-5.0, 100.0), 0.0);
        assert_eq!(fraction(10.0, 0.0), 0.0);
    }

    #[test]
    fn hp_bands_and_colors() {
        assert_eq!(hp_band(1.0), HpBand::Healthy);
        assert_eq!(hp_band(0.56), HpBand::Healthy);
        assert_eq!(hp_band(0.55), HpBand::Wounded);
        assert_eq!(hp_band(0.25), HpBand::Wounded);
        assert_eq!(hp_band(0.24), HpBand::Critical);
        assert_eq!(hp_color(0.9, 0.0), COLOR_HP_GREEN);
        assert_eq!(hp_color(0.4, 0.0), COLOR_SIGNAL_AMBER);
        // Critical pulses: alpha changes over time.
        assert_ne!(hp_color(0.1, 0.0).alpha(), hp_color(0.1, 0.17).alpha());
    }

    #[test]
    fn stamina_fill_tracks_dash_cooldown() {
        assert_eq!(stamina_fill(0.0, DASH_COOLDOWN), 1.0);
        assert_eq!(stamina_fill(DASH_COOLDOWN, DASH_COOLDOWN), 0.0);
        assert!((stamina_fill(0.5, 2.0) - 0.75).abs() < 1e-6);
        // Faster-dash upgrade shortens the max: same remaining = less full.
        assert!(stamina_fill(0.5, 1.0) < stamina_fill(0.5, 2.0));
        assert_eq!(stamina_fill(1.0, 0.0), 1.0);
    }

    #[test]
    fn shell_pips_light_proportionally() {
        assert_eq!(lit_pips(50, 50, SHELL_PIPS), 10);
        assert_eq!(lit_pips(25, 50, SHELL_PIPS), 5);
        assert_eq!(lit_pips(1, 50, SHELL_PIPS), 1);
        assert_eq!(lit_pips(0, 50, SHELL_PIPS), 0);
        assert_eq!(lit_pips(80, 50, SHELL_PIPS), 10);
        assert_eq!(lit_pips(5, 0, SHELL_PIPS), 0);
    }

    #[test]
    fn vel_gauge_tiers_fill_and_labels() {
        assert_eq!(speed_tier(0.0), SpeedTier::Walk);
        assert_eq!(speed_tier(11.0), SpeedTier::Walk);
        assert_eq!(speed_tier(11.99), SpeedTier::Walk);
        assert_eq!(speed_tier(12.0), SpeedTier::Flow);
        assert_eq!(speed_tier(19.99), SpeedTier::Flow);
        assert_eq!(speed_tier(20.0), SpeedTier::Surge);
        assert_eq!(speed_tier(27.99), SpeedTier::Surge);
        assert_eq!(speed_tier(28.0), SpeedTier::Redline);
        assert_eq!(speed_tier(48.0), SpeedTier::Redline);
        // Steady tier colors: no strobing anywhere, including 32+ m/s.
        assert_eq!(speed_color(SpeedTier::Walk), ASH_DIM);
        assert_eq!(speed_color(SpeedTier::Flow), ARC_CYAN);
        assert_eq!(speed_color(SpeedTier::Surge), ACID_GREEN);
        assert_eq!(speed_color(SpeedTier::Redline), HAZARD_YELLOW);

        assert_eq!(speed_fill(0.0), 0.0);
        assert!((speed_fill(16.0) - 0.5).abs() < 1e-6);
        assert_eq!(speed_fill(VEL_GAUGE_MAX), 1.0);
        assert_eq!(speed_fill(45.0), 1.0);
        assert_eq!(speed_segments(0.0), 0);
        assert_eq!(speed_segments(0.5), 1);
        assert_eq!(speed_segments(11.0), 6);
        assert_eq!(speed_segments(32.0), VEL_SEGMENTS);
        assert_eq!(speed_segments(99.0), VEL_SEGMENTS);

        // Constant-width integer readout.
        assert_eq!(speed_label(18.43), "18 m/s");
        assert_eq!(speed_label(0.0), " 0 m/s");
        assert_eq!(speed_label(-0.01), " 0 m/s");
        assert_eq!(speed_label(5.2), " 5 m/s");
        assert_eq!(speed_label(31.96), "32 m/s");
        assert_eq!(speed_label(9.4).len(), speed_label(47.9).len());
        assert_eq!(bhop_chain_label(0), None);
        assert_eq!(bhop_chain_label(1), None);
        assert_eq!(bhop_chain_label(2).as_deref(), Some("BHOP x2"));
        assert_eq!(bhop_chain_label(12).as_deref(), Some("BHOP x12"));
        assert!(bhop_chain_label(7).unwrap().is_ascii() && speed_label(23.4).is_ascii());
    }

    #[test]
    fn vel_tachometer_zones_line_up_with_tiers_and_fit_the_track() {
        // Fixed zones: 6 ash (0-12), 4 cyan (12-20), 4 green (20-28), 2 yellow (28-32).
        let zones: Vec<Color> = (0..VEL_SEGMENTS).map(segment_zone_color).collect();
        assert_eq!(zones.iter().filter(|c| **c == ASH_DIM).count(), 6);
        assert_eq!(zones.iter().filter(|c| **c == ARC_CYAN).count(), 4);
        assert_eq!(zones.iter().filter(|c| **c == ACID_GREEN).count(), 4);
        assert_eq!(zones.iter().filter(|c| **c == HAZARD_YELLOW).count(), 2);
        // Each segment's zone is the tier of the speed range it covers.
        for i in 0..VEL_SEGMENTS {
            let mid = (i as f32 + 0.5) * VEL_GAUGE_MAX / VEL_SEGMENTS as f32;
            assert_eq!(segment_zone_color(i), speed_color(speed_tier(mid)), "segment {i}");
        }
        // Zone breaks fall exactly at the tier thresholds (12 / 20 / 28 m/s).
        for i in 0..VEL_SEGMENTS - 1 {
            let edge = (i + 1) as f32 * VEL_GAUGE_MAX / VEL_SEGMENTS as f32;
            assert_eq!(segment_zone_break(i), [12.0, 20.0, 28.0].contains(&edge), "segment {i}");
        }
        // Lit = full zone color; unlit keeps a faint 16% tint of its zone.
        assert_eq!(segment_color(7, 8), ARC_CYAN);
        assert_eq!(segment_color(7, 7), ARC_CYAN.with_alpha(0.16));
        assert_eq!(segment_color(0, 0).alpha(), 0.16);
        // 16 x 9 px segments + 15 x 2 px gaps + 3 x 2 px extra breaks fill the
        // 187 px track's 180 px interior (3+2 px border, 1+1 px padding).
        let breaks = (0..VEL_SEGMENTS).filter(|i| segment_zone_break(*i)).count() as f32;
        assert_eq!(VEL_SEGMENTS as f32 * 9.0 + (VEL_SEGMENTS - 1) as f32 * 2.0 + breaks * 2.0, 187.0 - 5.0 - 2.0);
    }

    #[test]
    fn vel_gauge_hysteresis_never_flickers_at_tier_edges() {
        // Climbing is immediate.
        assert_eq!(speed_tier_hysteresis(SpeedTier::Walk, 12.0), SpeedTier::Flow);
        assert_eq!(speed_tier_hysteresis(SpeedTier::Walk, 29.0), SpeedTier::Redline);
        // Hovering just under a threshold keeps the higher tier...
        assert_eq!(speed_tier_hysteresis(SpeedTier::Flow, 11.6), SpeedTier::Flow);
        assert_eq!(speed_tier_hysteresis(SpeedTier::Surge, 19.7), SpeedTier::Surge);
        assert_eq!(speed_tier_hysteresis(SpeedTier::Redline, 27.6), SpeedTier::Redline);
        // ...until half a m/s below it, dropping only as far as speed says.
        assert_eq!(speed_tier_hysteresis(SpeedTier::Flow, 11.4), SpeedTier::Walk);
        assert_eq!(speed_tier_hysteresis(SpeedTier::Redline, 19.7), SpeedTier::Surge);
        assert_eq!(speed_tier_hysteresis(SpeedTier::Redline, 5.0), SpeedTier::Walk);

        // Speed jittering +-0.3 m/s around each threshold: the tier and the
        // 32 m/s badge snap change at most once.
        for edge in [12.0, 20.0, 28.0, VEL_GAUGE_MAX] {
            let mut g = VelGauge::default();
            g.update(edge - 2.0);
            let mut tier_flips = 0;
            let mut badge_flips = 0;
            for k in 0..60 {
                let (tier, maxed) = (g.tier, g.maxed);
                g.update(edge + if k % 2 == 0 { 0.3 } else { -0.3 });
                tier_flips += (g.tier != tier) as u32;
                badge_flips += (g.maxed != maxed) as u32;
            }
            assert!(tier_flips <= 1 && badge_flips <= 1, "flicker at {edge}: {tier_flips} / {badge_flips}");
        }

        // Solid snap at 32+: hazard badge with ink text, steady every frame.
        let mut g = VelGauge::default();
        g.update(33.0);
        assert!(g.maxed && g.badge_color() == HAZARD_YELLOW && g.value_color() == INK_BLACK);
        assert_eq!(g.color(), HAZARD_YELLOW);
        g.update(30.0);
        assert!(!g.maxed && g.badge_color() == Color::NONE && g.value_color() == HAZARD_YELLOW);
        g.update(4.0);
        assert_eq!((g.tier, g.value_color(), g.color()), (SpeedTier::Walk, BONE_TEXT, ASH_DIM));
    }

    #[test]
    fn overclock_needs_bars_and_upgrade() {
        assert!(overclock_active(4, 0.25));
        assert!(!overclock_active(3, 0.25));
        assert!(!overclock_active(5, 0.0));
    }

    #[test]
    fn seed_and_compendium_hotkeys_per_state() {
        use MenuAction::*;
        assert_eq!(seed_hotkey_action(AppState::MainMenu, RunMode::Standard, true, false), Some(OpenCompendium));
        assert_eq!(seed_hotkey_action(AppState::MainMenu, RunMode::Standard, false, true), None, "S on the menu opens the editor instead");
        for st in [AppState::Paused, AppState::GameOver, AppState::Victory] {
            assert_eq!(seed_hotkey_action(st, RunMode::Standard, false, true), Some(ShareSeed));
            assert_eq!(seed_hotkey_action(st, RunMode::TimeTrial, false, true), None, "time trials have no seed");
            assert_eq!(seed_hotkey_action(st, RunMode::Standard, true, false), None);
        }
        for st in [AppState::Compendium, AppState::Loadout, AppState::Settings, AppState::Playing] {
            assert_eq!(seed_hotkey_action(st, RunMode::Standard, true, true), None);
        }
        // Seed labels on the menu switch and the end screens.
        let mut seed = CustomSeedState::default();
        assert_eq!(seed_button_label(&seed), "SEED: RANDOM  [S]");
        seed.code = "MARS7".into();
        assert_eq!(seed_button_label(&seed), "SEED: MARS7  [S]");
        let mut run = RunStats::default();
        run.start_new_run(0x791E_0A5A);
        assert_eq!(with_seed("SURVEY OS".into(), &run), "SURVEY OS  //  SEED 791E-0A5A");
        assert_eq!(share_button_label(&run, &seed), "SHARE SEED  [S]");
        seed.arm_from_run(&run);
        assert_eq!(share_button_label(&run, &seed), "SEED 791E-0A5A ARMED  [S]");
        run.mode = RunMode::TimeTrial;
        assert_eq!(with_seed("X".into(), &run), "X");
    }

    #[test]
    fn menu_shortcuts_per_state() {
        use MenuAction::*;
        let k = |s, e, x, l, c| menu_key_action(s, e, x, l, c, false, false);
        assert_eq!(k(AppState::MainMenu, true, false, false, false), Some(StartRun));
        assert_eq!(k(AppState::MainMenu, false, false, true, false), Some(OpenLoadout));
        assert_eq!(k(AppState::MainMenu, false, false, false, true), Some(OpenContractsFeats));
        assert_eq!(k(AppState::MainMenu, false, true, false, false), None);
        assert_eq!(menu_key_action(AppState::Paused, false, true, false, false, false, false), Some(Continue));
        assert_eq!(menu_key_action(AppState::Paused, true, false, false, false, false, false), Some(Continue));
        assert_eq!(menu_key_action(AppState::Paused, false, false, false, false, true, false), Some(RestartRun));
        assert_eq!(menu_key_action(AppState::Paused, false, false, true, false, false, false), Some(OpenLoadout));
        assert_eq!(menu_key_action(AppState::Paused, false, false, false, false, false, true), Some(BackToMenu));
        assert_eq!(k(AppState::Loadout, true, false, false, false), Some(StartRun));
        assert_eq!(k(AppState::Loadout, false, true, false, false), Some(BackToMenu));
        assert_eq!(k(AppState::Loadout, false, false, false, true), Some(OpenContractsFeats));
        assert_eq!(k(AppState::Loadout, false, false, true, false), None);
        assert_eq!(k(AppState::ContractsFeats, false, false, true, false), Some(OpenLoadout));
        assert_eq!(k(AppState::ContractsFeats, false, true, false, false), Some(BackToMenu));
        assert_eq!(k(AppState::GameOver, true, false, false, false), Some(BackToMenu));
        assert_eq!(k(AppState::Victory, false, false, true, false), Some(OpenLoadout));
        assert_eq!(k(AppState::Victory, false, false, false, true), None);
        assert_eq!(k(AppState::Playing, true, true, true, true), None);
        assert_eq!(k(AppState::UpgradeSelect, true, false, true, true), None);
        assert_eq!(k(AppState::Settings, false, true, false, false), Some(CloseSettings));
        assert_eq!(k(AppState::Settings, true, false, true, true), None);
        assert_eq!(menu_hotkey_action(AppState::MainMenu, true, false), Some(OpenSettings));
        assert_eq!(menu_hotkey_action(AppState::Paused, true, false), Some(OpenSettings));
        assert_eq!(menu_hotkey_action(AppState::Victory, false, true), Some(ContinueEndless));
        assert_eq!(menu_hotkey_action(AppState::GameOver, true, true), None);
        assert_eq!(menu_hotkey_action(AppState::Playing, true, true), None);
        assert_eq!(menu_hotkey_action(AppState::Settings, true, false), None);
        assert_eq!(loadout_contract_color(1), COLOR_SIGNAL_AMBER);
        assert_eq!(loadout_contract_color(4), COLOR_SIGNAL_RED);
    }

    #[test]
    fn end_screens_keep_separate_palettes() {
        let (dead, live) = (no_service_look(), full_signal_look());
        // NO SERVICE is crimson with dead bars; FULL SIGNAL phosphor with all 5.
        assert_eq!((dead.bars, live.bars), (0, 5));
        assert_eq!((dead.tape, live.tape), (Tape::Danger, Tape::Signal));
        let red_over_green = |c: Color| {
            let c = c.to_srgba();
            c.red - c.green
        };
        for c in [dead.title_color, dead.accent, dead.backdrop, dead.bg, dead.title_slab, dead.band.1] {
            assert!(red_over_green(c) > 0.0, "NO SERVICE carries no green: {c:?}");
        }
        for c in [live.title_color, live.accent, live.backdrop, live.bg, live.title_slab, live.band.0, live.band.1] {
            assert!(red_over_green(c) < 0.0, "FULL SIGNAL carries no red: {c:?}");
        }
        // Only the win gets the lit hero switch; the loss stays unlit.
        assert_eq!(live.buttons[0].2, ButtonStyle::HeroAcid);
        assert!(dead.buttons.iter().all(|b| !matches!(b.2, ButtonStyle::HeroAcid | ButtonStyle::PrimaryAcid)));
        // Same actions on both: ENTER -> menu, L -> loadout.
        for look in [&dead, &live] {
            assert_eq!(look.buttons.map(|b| b.1), [MenuAction::BackToMenu, MenuAction::OpenLoadout]);
        }
        assert_ne!(dead.link, live.link);
    }

    #[test]
    fn boss_plate_formatting_ticks_and_colors() {
        use crate::boss::BossPhase;
        assert_eq!(BOSS_PHASE_TICKS, [0.30, 0.65]);
        let p1 = BossHud { active: true, phase: BossPhase::Phase1, hp_frac: 1.0, pylons: 0 };
        let sh = BossHud { active: true, phase: BossPhase::Shielded1, hp_frac: 0.65, pylons: 2 };
        assert_eq!(p1.tag(), "PHASE I");
        assert_eq!(sh.tag(), "SHIELDED // 2 PYLONS");
        assert_eq!(boss_bar_color(&p1, 0.0), PUNK_RED);
        let shield = boss_bar_color(&sh, 0.0).to_srgba();
        assert!(shield.blue > shield.red && shield.blue > 0.9, "shielded bar glows cyan-violet");
        let low = boss_bar_color(&BossHud { hp_frac: 0.35, phase: BossPhase::Phase2, ..p1.clone() }, 0.0).to_srgba();
        assert!(low.green > PUNK_RED.to_srgba().green, "shifts toward molten gold");
        let held = ExtractionState::Uploading { progress: 0.99, duration: 44.0, in_ring: true, jammed: false, wave_timer: 1.0, baron_spawned: true, boss_hold: true };
        assert_eq!(uplink_status_text(&held), "DEFEAT THE CARRIER DAEMON TO LOCK EXTRACTION");
        assert_eq!(uplink_status_text(&ExtractionState::start_upload(1)), "DEMON RUSH - HOLD THE SIGNAL");
    }

    #[test]
    fn slot_labels_hero_font_and_rush_frame() {
        assert_eq!([slot_label(0), slot_label(1), slot_label(2)], ["SLOT 01", "SLOT 02", "SLOT 03"]);
        // The hero switch is set larger than the secondary switches of its row.
        assert_eq!(button_font(ButtonStyle::HazardAmber, 20.0), 20.0);
        assert_eq!(button_font(ButtonStyle::HeroAcid, 20.0), 30.0);
        // DEMON RUSH: the status plate's rail and edges go alert red.
        let calm = status_plate_border(&ExtractionState::Searching { hold: 0.0 });
        let rush = status_plate_border(&ExtractionState::Uploading {
            progress: 0.4,
            duration: 28.0,
            in_ring: true,
            jammed: false,
            wave_timer: 0.5,
            baron_spawned: false,
            boss_hold: false,
        });
        assert_eq!((calm.left, rush.left), (ARC_CYAN, PUNK_RED));
        assert_ne!(calm.top, rush.top);
        // Every control row of the field manual has a category and keys.
        assert!(CONTROLS.iter().all(|(cat, binds)| !cat.is_empty() && binds.iter().all(|(keys, what)| !keys.is_empty() && what.is_ascii())));
    }

    #[test]
    fn weapon_titles_and_accents_differ() {
        assert_eq!(weapon_title(Weapon::Plasma), "PLASMA REPEATER");
        assert_ne!(weapon_accent(Weapon::Shotgun), weapon_accent(Weapon::Plasma));
        assert_eq!(weapon_title(Weapon::Rocket), "SIGNAL-MORTAR");
        assert_eq!(weapon_accent(Weapon::Rocket), COLOR_ROCKET);
        assert_ne!(weapon_accent(Weapon::Rocket), weapon_accent(Weapon::Shotgun));
        assert_eq!(weapon_slot_name(Weapon::Rocket), "MORTAR");
        assert_eq!(lit_pips(10, 20, ROCKET_PIPS), 5);
    }

    #[test]
    fn weapon_deck_fits_on_one_line_at_720p() {
        // Every slot badge row (keycap + name + a 3-digit reserve) fits its chip.
        let inner = slot_inner_width();
        for w in Weapon::ALL {
            let need = slot_row_width(weapon_slot_name(w), 3);
            assert!(need <= inner, "{} slot row needs {need} px of {inner}", weapon_slot_name(w));
        }
        // The longest weapon title clears both buff pills lit at once.
        let card_inner = DECK_WIDTH - CARD_BORDER.0 - CARD_BORDER.1 - 2.0 * CARD_PAD_X;
        let pills: f32 = [OVERCLOCK_PILL, DOPPLER_PILL].iter().map(|l| mono_width(l.len(), PILL_FONT) + 2.0 * PILL_PAD_X).sum::<f32>() + PILL_GAP;
        for w in Weapon::ALL {
            let title = mono_width(weapon_title(w).len(), WEAPON_TITLE_FONT);
            assert!(title + 8.0 + pills <= card_inner, "{} header overflows", weapon_title(w));
        }
        // Reserve readout: red when dry, bright only on the equipped weapon.
        assert_eq!(slot_ammo_color(0, true), COLOR_SIGNAL_RED);
        assert_eq!(slot_ammo_color(0, false), COLOR_SIGNAL_RED);
        assert_eq!(slot_ammo_color(12, true), COLOR_TEXT);
        assert_eq!(slot_ammo_color(12, false), COLOR_DIM);
    }
}
