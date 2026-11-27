//! Menus (main / App Store upgrade select / pause / game over / victory) and
//! the in-game HUD: status + SOS uplink bar, crosshair hitmarker, killfeed
//! toasts, VEL speedometer + HP / armor / stamina vitals, weapon & ammo deck and a subtle
//! Dead-Zone edge frame. Signal bars, radar and phone apps live on the 3D
//! phone screen (`phone_screen.rs`), not here.
//!
//! The menus are built from the gritty industrial-punk kit in `ui_kit.rs`
//! (hard corners, bolted-plate borders, tape strips, stencil titles). The
//! in-game HUD is frameless instead: shadowed type, dark gauge wells and thin
//! glowing accents from `hud_fx.rs`, matching the floating style emblem.
//!
//! Buttons use a small manual hit test (`ComputedNode::contains_point` with the
//! window's physical cursor position) because Bevy 0.20 deprecated `Interaction`
//! in favour of picking, which this build does not enable.

use std::fmt::Write as _;

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::text::LineHeight;
use bevy::ui::{UiGlobalTransform, Val2};
use bevy::window::PrimaryWindow;

use crate::assets_gen::FloorTheme;
use crate::audio::{SfxEvent, SfxQueue, UiSfxGate};
use crate::boss::BossHud;
use crate::compendium::{apply_custom_label, pick_run_seed, seed_key_char, shareable_code, CustomSeedState};
use crate::extraction::{ExtractionState, UPLINK_HOLD_SECS};
use crate::fx::{HudFeed, TOAST_SECONDS};
use crate::hud_fx::{
    accent_bar, danger_pulse, flash, glow, hud_text, pop_scale, pop_transform, set_glow, shadow_color, slam_curve, status_rule_look, toast_pose,
    HudPulse, HudSnapshot, StatusKind, ToastPose, HUD_GUTTER, HUD_SUB, HUD_TOP, RAIL_WIDTH,
};
use crate::leaderboard::{spawn_account_pill, submit_badge_bundle, LeaderboardState, LeaderboardUiPlugin};
use crate::gamepad::{pad_edit_text, DefaultFocus, DualPrompt, GamepadFocus, GamepadInput, InputMode, PadFocusLock, PAD_TEXT_HINT};
use crate::meta::{live_settings, record_game_over, record_victory, start_run_with_loadout, CrosshairColor, CrosshairStyle, ProfileStore, RunLoadout};
use crate::meta_ui::{
    end_loadout_line, loadout_tag_parts, unlock_reward_lines, MetaBox, MetaUiPlugin, NO_UNLOCKS_LINE,
};
use crate::player::{Player, DASH_COOLDOWN};
use crate::touch::{overlay_visible, TouchInput, UiPointer};
use crate::signal::SignalState;
use crate::style_emblem::{EmblemAtlas, EmblemFrame, EMBLEM_BYTES, EMBLEM_PX};
use crate::state::{complete_floor, continue_into_endless, AppState, GameSet, PlayerModifiers, RunMode, RunStats, SettingsReturn, FINAL_FLOOR};
use crate::trials::{
    begin_daily_attempt, daily_loadout, daily_record, daily_rules_line, floor_splits_line, format_split_delta, format_split_time, split_plate_view,
    split_timer_visible, start_daily_run, start_time_trial, trial_splits_line, DailyDate, TimeTrialState, SPLIT_GOLD,
};
use crate::ui_kit::{
    hard_shadow, hero_title, inset_border, keycap, plate_border, plate_rect, punk_button, punk_screen_root,
    punk_screen_root_tinted, recessed_bezel, screen_rails, section_crate, signal_bars, stamped_tag, stencil_title, stencil_title_layers,
    terminal_chassis, tiled_image, ButtonStyle, PunkButtonColors, Tape, UiKitPlugin, UiKitTextures, ACID_GREEN, ARC_CYAN, ASH_DIM, BONE_TEXT, HAZARD_DARK, HAZARD_YELLOW, INK_BLACK, IRON_MID, PUNK_RED, STEEL_BORDER, STEEL_LIGHT,
};
use crate::upgrades::{draft_upgrades, next_rank_preview, pick_upgrade, UpgradeChoices, UpgradeFocus};
use crate::weapons::{doppler_active, Weapon, WeaponState};

pub const COLOR_BG: Color = INK_BLACK;
/// Main menu root scrim alpha over the live 3D vista (Milestone 30; lowered
/// for the Milestone 35 showcase so the bunker reads clearly).
pub const MENU_ROOT_SCRIM: f32 = 0.44;
/// Grunge backdrop strength on the main menu (kept faint over the vista).
pub const MENU_BACKDROP_ALPHA: f32 = 0.10;
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
    /// Main menu / end screens: the global leaderboards (`K`).
    OpenLeaderboard,
    /// Leaderboard: switch DAILY <-> TIME TRIAL.
    LbTab,
    /// Leaderboard: reload both boards.
    LbRefresh,
    /// Leaderboard: open / commit the callsign editor.
    LbCallsign,
    /// Main menu / leaderboard: SIGN IN (web) / SIGN OUT.
    LbAccount,
    /// Leaderboard: previous / next page of the Top 25.
    LbPrevPage,
    LbNextPage,
    /// Leaderboard touch callsign editor strip (`<` `>` `NEXT` `DEL` `DONE`).
    LbEdit(crate::leaderboard::CallsignEdit),
    /// Main menu / pause: open or close the controls overlay (`H`).
    ToggleControls,
    /// Controls overlay CLOSE switch (`H` / `Esc` / controller `B`).
    CloseControls,
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
        // `Esc` (and the Android system Back, replayed as `Esc`) returns from
        // the end cards like controller `B` (Time Trial finish included).
        AppState::GameOver | AppState::Victory if enter || esc => Some(BackToMenu),
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

/// `K` opens the global leaderboards from `MainMenu` and the end screens.
pub fn leaderboard_hotkey_action(state: AppState, k: bool) -> Option<MenuAction> {
    match state {
        AppState::MainMenu | AppState::GameOver | AppState::Victory if k => Some(MenuAction::OpenLeaderboard),
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

/// Controller `X` retries from the Time Trial finish card (`Victory` in
/// [`RunMode::TimeTrial`]), mirroring keyboard `R`.
pub fn pad_retry_action(state: AppState, mode: RunMode, x: bool) -> Option<MenuAction> {
    (x && state == AppState::Victory && mode == RunMode::TimeTrial).then_some(MenuAction::RetryTimeTrial)
}

/// Controller variant of a `LABEL  [KEY]` switch on `state`: the keyboard
/// keycap is swapped for the controller shortcut that does the same thing
/// (`START` play / resume, `B` back, `X` retry), or dropped when the switch is
/// only reachable by focus + `A`.
pub fn pad_prompt(state: AppState, label: &str, action: MenuAction) -> String {
    let base = label.rsplit_once("  [").map_or(label, |(b, _)| b);
    let tag = match action {
        MenuAction::StartRun if matches!(state, AppState::MainMenu | AppState::Loadout | AppState::ContractsFeats) => Some("START"),
        MenuAction::Continue if state == AppState::Paused => Some("START"),
        MenuAction::BackToMenu if matches!(state, AppState::Loadout | AppState::ContractsFeats | AppState::GameOver | AppState::Victory | AppState::Leaderboard) => {
            Some("B")
        }
        MenuAction::CloseSettings | MenuAction::CloseControls => Some("B"),
        MenuAction::RetryTimeTrial => Some("X"),
        _ => None,
    };
    match tag {
        Some(t) => format!("{base}  [{t}]"),
        None => base.to_string(),
    }
}

/// A punk switch whose keycap follows the active [`InputMode`].
pub fn prompt_button(state: AppState, label: &str, action: MenuAction, font: f32, style: ButtonStyle) -> impl Bundle {
    (punk_button(label, action, font, style), DualPrompt::new(label, pad_prompt(state, label, action)))
}

/// Debug tools (`F9` floor skip): debug builds (incl. every `cargo test`
/// run) or `SEGNALE_DEV` set. Release builds without it never skip floors.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevTools(pub bool);

/// Parse `SEGNALE_DEV` strictly: only `"1"` or `"true"` enables dev tools
/// (`"0"`, `""`, `"false"`, or unset keep release mode locked down).
pub fn parse_dev_env(val: Option<&str>) -> bool {
    matches!(val.map(str::trim), Some(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether the debug tools are on for this build / environment.
pub const fn dev_tools_allowed(debug_build: bool, dev_env: bool) -> bool {
    debug_build || dev_env
}

impl DevTools {
    pub fn from_env() -> Self {
        let dev_env = parse_dev_env(std::env::var("SEGNALE_DEV").ok().as_deref());
        DevTools(dev_tools_allowed(cfg!(debug_assertions), dev_env))
    }
}

/// Reference layout every screen is designed for (logical px).
pub const UI_REFERENCE: (f32, f32) = (1280.0, 720.0);
/// Smallest UI scale (a 640x360 window).
pub const UI_SCALE_MIN: f32 = 0.5;

/// Responsive UI scale for a `w x h` logical window: below the 1280x720
/// reference the whole UI shrinks uniformly so every screen keeps its 720p
/// composition inside itch.io embeds (960x540 -> 0.75, 1024x640 -> 0.8);
/// at 720p and above it stays 1.0 (never upscaled).
pub fn ui_scale_for(w: f32, h: f32) -> f32 {
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return 1.0;
    }
    (w / UI_REFERENCE.0).min(h / UI_REFERENCE.1).clamp(UI_SCALE_MIN, 1.0)
}

/// Keep [`UiScale`] fitted to the primary window (resizes, embeds, Android).
fn fit_ui_scale(window: Query<&Window, With<PrimaryWindow>>, mut scale: ResMut<UiScale>) {
    let Ok(w) = window.single() else { return };
    let want = ui_scale_for(w.width(), w.height());
    if (scale.0 - want).abs() > 1e-3 {
        scale.0 = want;
    }
}

/// App Store card for offer `n` (0..3).
#[derive(Component)]
struct UpgradeCard(usize);

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<DevTools>() {
            app.insert_resource(DevTools::from_env());
        }
        app.add_plugins((UiKitPlugin, MetaUiPlugin, crate::perf::PerfOverlayPlugin, LeaderboardUiPlugin))
            // Live 3D Mars Signal Tower vista behind the main menu (render
            // cameras stay out of headless CoreGameplayPlugin tests).
            .add_plugins(crate::menu_scene::MenuScenePlugin)
            // Milestone 37: the on-screen touch HUD (drawing; `touch.rs` routes).
            .add_plugins(crate::touch_hud::TouchHudPlugin)
            .init_resource::<UiSfxGate>()
            .init_resource::<InputMode>()
            .add_systems(Startup, init_style_emblem)
            .add_systems(OnEnter(AppState::MainMenu), spawn_main_menu)
            .add_systems(OnExit(AppState::MainMenu), (release_seed_pad_lock, close_controls_overlay))
            .init_resource::<ControlsOverlayOpen>()
            .add_systems(OnEnter(AppState::MainMenu), close_controls_overlay)
            .add_systems(OnEnter(AppState::Paused), close_controls_overlay)
            .add_systems(OnExit(AppState::Paused), close_controls_overlay)
            .add_systems(OnEnter(AppState::Playing), spawn_hud)
            .add_systems(OnEnter(AppState::Paused), spawn_pause_menu)
            .add_systems(OnEnter(AppState::UpgradeSelect), spawn_app_store.after(draft_upgrades))
            // End screens read the run summary the profile recorder just stored.
            .add_systems(OnEnter(AppState::GameOver), spawn_game_over.after(record_game_over).after(crate::leaderboard::auto_submit))
            .add_systems(OnEnter(AppState::Victory), spawn_victory.after(record_victory).after(crate::leaderboard::auto_submit))
            .add_systems(Update, (menu_buttons, menu_keys).run_if(not(in_state(AppState::Playing))))
            .add_systems(Update, seed_entry_keys.after(menu_keys).after(menu_buttons).run_if(in_state(AppState::MainMenu)))
            .add_systems(Update, seed_entry_pad.after(seed_entry_keys).run_if(in_state(AppState::MainMenu)))
            .add_systems(Update, refresh_seed_ui.after(seed_entry_pad).run_if(not(in_state(AppState::Playing))))
            .add_systems(
                Update,
                (refresh_field_manual, refresh_controls_overlay.after(menu_keys).after(menu_buttons))
                    .run_if(in_state(AppState::MainMenu).or_else(in_state(AppState::Paused))),
            )
            .init_resource::<UiScale>()
            .add_systems(PreUpdate, fit_ui_scale)
            .add_systems(Update, upgrade_cards.run_if(in_state(AppState::UpgradeSelect)))
            .init_resource::<VelGauge>()
            .init_resource::<HudPulse>()
            .add_systems(
                Update,
                (
                    track_vel_gauge.before(update_hud).before(update_hud_boxes),
                    track_hud_pulse.before(update_hud).before(update_hud_boxes).before(update_split_plate),
                    update_hud,
                    update_hud_boxes,
                    refresh_hud_crosshair,
                    update_split_plate,
                    update_style_hud,
                    pause_input,
                    debug_skip_floor,
                    touch_hud_relayout.after(update_style_hud),
                )
                    .in_set(GameSet::Ui),
            )
            .add_systems(Update, touch_menu_targets);
    }
}

pub fn text(s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (Text::new(s), TextFont::from_font_size(size), TextColor(color))
}

/// Centered row of punk switches. A [`ButtonStyle::HeroAcid`] switch is the
/// row's call to action and is set ~1.5x larger than its neighbours.
pub fn button_row(commands: &mut Commands, parent: Entity, buttons: &[(&str, MenuAction, ButtonStyle)], font: f32, top: f32) {
    button_row_in(commands, parent, AppState::MainMenu, buttons, font, top);
}

/// [`button_row`] whose keycaps follow the active [`InputMode`] on `state`
/// (`SHARE SEED` keeps its own live label, see [`refresh_seed_ui`]).
pub fn button_row_in(commands: &mut Commands, parent: Entity, state: AppState, buttons: &[(&str, MenuAction, ButtonStyle)], font: f32, top: f32) {
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
        let b = commands.spawn((ChildOf(row), punk_button(label, *action, button_font(*style, font), *style))).id();
        if *action != MenuAction::ShareSeed {
            commands.entity(b).insert(DualPrompt::new(*label, pad_prompt(state, label, *action)));
        }
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

/// Firmware stamp of the title lockup.
pub const OS_STAMP: &str = "UAC-OS v1.0";

/// One field-manual binding: the keycaps and what they do.
pub type KeyBind = (&'static [&'static str], &'static str);

/// Field-manual rows: `(category, bindings)`.
pub const CONTROLS: [(&str, &[KeyBind]); 4] = [
    ("MOVE", &[(&["WASD"], "move"), (&["MOUSE"], "look"), (&["SPACE"], "jump / hold to bhop"), (&["SHIFT"], "airplane-mode dash")]),
    ("FIGHT", &[(&["LMB"], "fire"), (&["1", "2", "3", "TAB", "WHEEL"], "shotgun / plasma / mortar")]),
    (
        "PHONE",
        &[
            (&["V", "MMB"], "phone punch / parry (0 bars)"),
            (&["RMB"], "ping scan (1+ bar)"),
            (&["C"], "camera flash strobe (2+ bars)"),
            (&["Q"], "5G overload EMP (3+ bars)"),
            (&["E"], "SOS uplink at the tower"),
        ],
    ),
    ("SYSTEM", &[(&["CLICK"], "capture mouse"), (&["ESC", "P"], "pause")]),
];

/// Controller field-manual rows (same layout as [`CONTROLS`]).
pub const PAD_CONTROLS: [(&str, &[KeyBind]); 4] = [
    ("MOVE", &[(&["LS"], "move"), (&["RS"], "look"), (&["A", "L3"], "jump / hold to bhop"), (&["LB"], "airplane-mode dash")]),
    ("FIGHT", &[(&["RT"], "fire"), (&["Y", "DPAD"], "shotgun / plasma / mortar")]),
    (
        "PHONE",
        &[
            (&["B"], "phone punch / parry (0 bars)"),
            (&["LT"], "ping scan (1+ bar)"),
            (&["D-UP"], "camera flash strobe (2+ bars)"),
            (&["RB"], "5G overload EMP (3+ bars)"),
            (&["X"], "SOS uplink at the tower"),
        ],
    ),
    ("SYSTEM", &[(&["START"], "pause"), (&["A"], "select"), (&["B"], "back")]),
];

/// Touch field-manual rows (Milestone 37; same layout as [`CONTROLS`]).
pub const TOUCH_CONTROLS: [(&str, &[KeyBind]); 4] = [
    ("MOVE", &[(&["L-THUMB"], "floating stick (rim = auto-bhop)"), (&["R-DRAG"], "look"), (&["JUMP"], "jump / hold to bhop"), (&["DASH"], "airplane-mode dash")]),
    ("FIGHT", &[(&["FIRE"], "fire (drag to aim)"), (&["SWAP", "1 2 3"], "next weapon / tap a deck slot")]),
    (
        "PHONE",
        &[
            (&["PUNCH"], "phone punch / parry (0 bars)"),
            (&["PING"], "ping scan (1+ bar)"),
            (&["FLASH"], "camera flash strobe (2+ bars)"),
            (&["5G"], "5G overload EMP (3+ bars)"),
            (&["SOS"], "SOS uplink at the tower"),
        ],
    ),
    ("SYSTEM", &[(&["||", "BACK"], "pause"), (&["TAP"], "select"), (&["SETTINGS"], "touch layout & feel")]),
];

/// One of the field-manual bodies (shown for its [`InputMode`]).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldManual(pub InputMode);

/// Show the field manual of the active input mode.
fn refresh_field_manual(mode: Res<InputMode>, mut q: Query<(Ref<FieldManual>, &mut Node)>) {
    for (m, mut node) in &mut q {
        if mode.is_changed() || m.is_added() {
            let want = if m.0 == *mode { Display::Flex } else { Display::None };
            if node.display != want {
                node.display = want;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_main_menu(
    mut commands: Commands,
    mode: Res<InputMode>,
    store: Res<ProfileStore>,
    lb: Res<LeaderboardState>,
    seed: Res<CustomSeedState>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
) {
    let tex = tex.as_deref();
    let msaa = msaa.map(|m| m.0).unwrap_or(bevy::render::view::Msaa::Sample2);
    // Menu needs its own camera: the player camera only exists in a run. It
    // draws the UI over the live 3D vista (`menu_scene`, camera order -1), so
    // it never clears and renders no 3D layer of its own.
    commands.spawn((
        DespawnOnExit(AppState::MainMenu),
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::None, ..default() },
        msaa,
    ));
    // Light scrim: the 3D bunker vista owns the middle of the screen; only
    // the header, the title and the action dock sit on ink.
    let root = punk_screen_root(&mut commands, AppState::MainMenu, tex, INK_BLACK.with_alpha(MENU_ROOT_SCRIM), MENU_BACKDROP_ALPHA);
    screen_rails(&mut commands, root, tex, 12.0, false);

    // Showcase body: header + title pinned to the top, the action dock to
    // the bottom, nothing in between but the vista.
    let body = commands
        .spawn((
            ChildOf(root),
            ShowcaseBody,
            HiddenByControls,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                // Top clears the corner chrome stamps (`UAC-B7 // SEC-07`, `RF-LINK`)
                // of `punk_screen_root`; bottom lifts the telemetry line off the tape.
                padding: UiRect { left: Val::Px(40.0), right: Val::Px(40.0), top: Val::Px(52.0), bottom: Val::Px(30.0) },
                ..default()
            },
        ))
        .id();
    let top = commands
        .spawn((ChildOf(body), Node { width: Val::Percent(100.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(6.0), ..default() }))
        .id();

    // Header strip: firmware stamp + the dying 1/5 signal | account (+ JAM DEMO).
    let header = commands
        .spawn((ChildOf(top), Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() }))
        .id();
    let stamp = commands.spawn((ChildOf(header), Node { column_gap: Val::Px(10.0), align_items: AlignItems::FlexEnd, ..default() })).id();
    commands.spawn((ChildOf(stamp), stamped_tag(OS_STAMP, STEEL_BORDER, BONE_TEXT, 12.0)));
    signal_bars(&mut commands, stamp, 1, PUNK_RED, 5.0);
    commands.spawn((ChildOf(stamp), text("1/5", 12.0, PUNK_RED)));
    let right = commands.spawn((ChildOf(header), Node { column_gap: Val::Px(10.0), align_items: AlignItems::Center, ..default() })).id();
    if store.profile.jam_demo_unlock_all {
        commands.spawn((ChildOf(right), MetaBox::JamPill, stamped_tag("JAM DEMO", HAZARD_YELLOW, INK_BLACK, 11.0)));
    }
    spawn_account_pill(&mut commands, right, &lb, &store.profile.callsign);

    // Hero title: the wordmark + one tag line.
    let hero = commands
        .spawn((ChildOf(top), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(6.0), ..default() }))
        .id();
    hero_title(&mut commands, hero, "SEGNALE DEBOLE", 92.0, BONE_TEXT, PUNK_RED);
    commands.spawn((ChildOf(hero), stamped_tag(MENU_TAGLINE, HAZARD_YELLOW, INK_BLACK, 16.0)));

    // Bottom action dock: primary row, then a compact utility strip.
    let dock = commands
        .spawn((ChildOf(body), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(10.0), ..default() }))
        .id();
    let primary = commands.spawn((ChildOf(dock), Node { column_gap: Val::Px(10.0), align_items: AlignItems::Center, ..default() })).id();
    commands.spawn((ChildOf(primary), prompt_button(AppState::MainMenu, "PLAY  [ENTER]", MenuAction::StartRun, 26.0, ButtonStyle::HeroAcid), DefaultFocus));
    // Secondary nav wears one quiet steel treatment: PLAY owns the color.
    for (label, action) in [
        ("LOADOUT  [L]", MenuAction::OpenLoadout),
        ("CONTRACTS  [C]", MenuAction::OpenContractsFeats),
        ("SETTINGS  [O]", MenuAction::OpenSettings),
        ("LEADERBOARD  [K]", MenuAction::OpenLeaderboard),
        ("BESTIARY  [B]", MenuAction::OpenCompendium),
    ] {
        commands.spawn((ChildOf(primary), prompt_button(AppState::MainMenu, label, action, 15.0, ButtonStyle::ScrapSteel)));
    }

    // Utility strip on one ink rail: alternate modes | run seed | controls.
    let utility = commands
        .spawn((
            ChildOf(dock),
            Node {
                column_gap: Val::Px(8.0),
                align_items: AlignItems::Center,
                padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                border: plate_rect((3.0, 1.0, 1.0, 1.0)),
                ..default()
            },
            BorderColor { left: ARC_CYAN, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.62)),
        ))
        .id();
    commands.spawn((ChildOf(utility), prompt_button(AppState::MainMenu, "DAILY RUN  [D]", MenuAction::StartDaily, 13.0, ButtonStyle::HazardAmber)));
    commands.spawn((ChildOf(utility), prompt_button(AppState::MainMenu, "TIME TRIAL  [T]", MenuAction::StartTimeTrial, 13.0, ButtonStyle::PrimaryAcid)));
    commands.spawn((ChildOf(utility), Node { width: Val::Px(1.0), height: Val::Px(22.0), margin: UiRect::horizontal(Val::Px(4.0)), ..default() }, BackgroundColor(STEEL_BORDER)));
    commands.spawn((ChildOf(utility), punk_button(&seed_button_label(&seed, *mode), MenuAction::ToggleSeedEdit, 13.0, ButtonStyle::PrimaryAcid)));
    commands.spawn((ChildOf(utility), punk_button("RND", MenuAction::RollSeed, 13.0, ButtonStyle::ScrapSteel)));
    commands.spawn((ChildOf(utility), punk_button("CLR", MenuAction::ClearSeed, 13.0, ButtonStyle::ScrapSteel)));
    commands.spawn((ChildOf(utility), Node { width: Val::Px(1.0), height: Val::Px(22.0), margin: UiRect::horizontal(Val::Px(4.0)), ..default() }, BackgroundColor(STEEL_BORDER)));
    commands.spawn((ChildOf(utility), prompt_button(AppState::MainMenu, CONTROLS_BUTTON, MenuAction::ToggleControls, 13.0, ButtonStyle::ScrapSteel)));

    // One muted telemetry slot: the seed editor / status while active, else
    // today's Daily rules + the Time Trial PB.
    let (daily_sub, trial_sub) = mode_subtitles(&store.profile, DailyDate::today_utc());
    let status_on = seed_status_visible(&seed);
    commands.spawn((
        ChildOf(dock),
        SeedStatusText,
        Node { display: if status_on { Display::Flex } else { Display::None }, margin: UiRect::top(Val::Px(2.0)), ..default() },
        text(seed_status_line(&seed, *mode), 12.0, ARC_CYAN.with_alpha(0.85)),
        telemetry_shadow(),
    ));
    commands.spawn((
        ChildOf(dock),
        ModeTelemetry,
        Node { display: if status_on { Display::None } else { Display::Flex }, margin: UiRect::top(Val::Px(2.0)), ..default() },
        text(format!("DAILY {daily_sub}   //   TRIAL {trial_sub}"), 12.0, ASH_DIM.mix(&BONE_TEXT, 0.45)),
        telemetry_shadow(),
    ));
    spawn_input_hint(&mut commands, dock);

    spawn_controls_overlay(&mut commands, root, AppState::MainMenu, *mode);
}

/// Hard ink drop under the main menu telemetry line (it sits straight on the
/// red Mars terrain, no plate behind it).
fn telemetry_shadow() -> TextShadow {
    TextShadow { offset: Vec2::splat(1.0), color: INK_BLACK.with_alpha(0.9) }
}

/// Main menu tag line under the wordmark.
pub const MENU_TAGLINE: &str = "WEAK SIGNAL // MARS BUNKER 7";

/// Main menu / pause CONTROLS switch.
pub const CONTROLS_BUTTON: &str = "CONTROLS  [H]";

/// The showcase main menu's top-to-bottom body (hidden under the controls overlay).
#[derive(Component)]
pub struct ShowcaseBody;

/// Daily rules + Time Trial PB line (shares its slot with [`SeedStatusText`]).
#[derive(Component)]
pub struct ModeTelemetry;

/// Whether the `FIELD MANUAL // CONTROLS` overlay is open (Main Menu / Pause).
/// Closed on entering and leaving either screen.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlsOverlayOpen(pub bool);

/// Root of the controls overlay modal.
#[derive(Component)]
pub struct ControlsOverlay;

/// Screen content hidden while the controls overlay is open (keeps the
/// controller focus inside the modal).
#[derive(Component)]
pub struct HiddenByControls;

/// `FIELD MANUAL // CONTROLS` modal: both manuals (the active [`InputMode`]
/// picks one live) + CLOSE. Starts hidden; see [`ControlsOverlayOpen`].
fn spawn_controls_overlay(commands: &mut Commands, root: Entity, state: AppState, mode: InputMode) {
    let overlay = commands
        .spawn((
            ChildOf(root),
            ControlsOverlay,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(INK_BLACK.with_alpha(0.55)),
        ))
        .id();
    let (manual, head) = section_crate(
        commands,
        overlay,
        "FIELD MANUAL // CONTROLS",
        HAZARD_YELLOW,
        Node::default(),
    );
    commands.entity(manual).insert((BackgroundColor(INK_BLACK.with_alpha(0.92)), hard_shadow(8.0)));
    commands.spawn((ChildOf(head), Node { flex_grow: 1.0, ..default() }));
    let head_prompt = DualPrompt::new("KEYBOARD & MOUSE", "CONTROLLER").with_touch("TOUCH");
    let head_label = head_prompt.get(mode).to_string();
    commands.spawn((ChildOf(head), head_prompt, text(head_label, 11.0, ARC_CYAN.with_alpha(0.85))));
    // Every manual is built once; the active input mode picks which shows
    // (switching devices mid-screen swaps them live).
    for (m, rows) in [(InputMode::KeyboardMouse, CONTROLS), (InputMode::Gamepad, PAD_CONTROLS), (InputMode::Touch, TOUCH_CONTROLS)] {
        let body = commands
            .spawn((
                ChildOf(manual),
                FieldManual(m),
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(8.0),
                    // Inset on top of the crate's own 9 px so the widest row
                    // clears the frame evenly on both sides.
                    padding: UiRect { left: Val::Px(9.0), right: Val::Px(9.0), top: Val::Px(4.0), bottom: Val::Px(0.0) },
                    display: if m == mode { Display::Flex } else { Display::None },
                    ..default()
                },
            ))
            .id();
        for (category, binds) in rows {
            let row = commands.spawn((ChildOf(body), Node { align_items: AlignItems::FlexStart, column_gap: Val::Px(14.0), ..default() })).id();
            commands.spawn((ChildOf(row), Node { width: Val::Px(56.0), flex_shrink: 0.0, ..default() }, children![text(category, 12.0, HAZARD_YELLOW)]));
            let list = commands
                .spawn((ChildOf(row), Node { max_width: Val::Px(760.0), flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(16.0), row_gap: Val::Px(8.0), ..default() }))
                .id();
            for (keys, what) in binds {
                let bind = commands.spawn((ChildOf(list), Node { align_items: AlignItems::Center, column_gap: Val::Px(4.0), ..default() })).id();
                for key in *keys {
                    commands.spawn((ChildOf(bind), keycap(key, 12.0)));
                }
                commands.spawn((ChildOf(bind), Node { margin: UiRect::left(Val::Px(3.0)), ..default() }, children![text(*what, 13.0, BONE_TEXT.with_alpha(0.78))]));
            }
        }
    }
    let foot = commands.spawn((ChildOf(manual), Node { width: Val::Percent(100.0), margin: UiRect { top: Val::Px(6.0), bottom: Val::Px(6.0), ..default() }, justify_content: JustifyContent::Center, ..default() })).id();
    commands.spawn((ChildOf(foot), prompt_button(state, CONTROLS_CLOSE, MenuAction::CloseControls, 14.0, ButtonStyle::ScrapSteel), DefaultFocus));
}

/// CLOSE switch of the controls overlay.
pub const CONTROLS_CLOSE: &str = "CLOSE  [H / ESC]";

/// Show / hide the controls overlay (and the content under it).
#[allow(clippy::type_complexity)]
fn refresh_controls_overlay(
    open: Res<ControlsOverlayOpen>,
    mut overlay: Query<(Ref<ControlsOverlay>, &mut Node), Without<HiddenByControls>>,
    mut hidden: Query<(Ref<HiddenByControls>, &mut Node), Without<ControlsOverlay>>,
) {
    let all = open.is_changed();
    for (o, mut node) in &mut overlay {
        let want = if open.0 { Display::Flex } else { Display::None };
        if (all || o.is_added()) && node.display != want {
            node.display = want;
        }
    }
    for (h, mut node) in &mut hidden {
        let want = if open.0 { Display::None } else { Display::Flex };
        if (all || h.is_added()) && node.display != want {
            node.display = want;
        }
    }
}

fn close_controls_overlay(mut open: ResMut<ControlsOverlayOpen>) {
    if open.0 {
        open.0 = false;
    }
}

/// `H` toggles the controls overlay on `MainMenu` / `Paused`.
pub fn controls_hotkey_action(state: AppState, h: bool) -> Option<MenuAction> {
    (h && matches!(state, AppState::MainMenu | AppState::Paused)).then_some(MenuAction::ToggleControls)
}

/// While the controls overlay is open, `Esc` / controller `B` close it
/// (instead of resuming from Pause).
pub fn controls_back_closes(state: AppState, open: bool, esc: bool, back: bool) -> bool {
    open && (esc || back) && matches!(state, AppState::MainMenu | AppState::Paused)
}

/// Whether the seed status line replaces the mode telemetry (editing, or a
/// status message to show).
pub fn seed_status_visible(seed: &CustomSeedState) -> bool {
    seed.editing || seed.status_msg.is_some()
}

/// Live seed status line of the main menu.
#[derive(Component)]
pub struct SeedStatusText;

/// Label of the main menu SEED switch.
pub fn seed_button_label(seed: &CustomSeedState, mode: InputMode) -> String {
    format!("SEED: {}{}", seed.readout(), mode.pick3("  [S]", "", ""))
}

/// Seed status line of a touch-only player: no keyboard to type a code with.
pub const TOUCH_SEED_HINT: &str = "SAME CODE = SAME BUNKER // TAP RND TO ROLL ONE";

/// Seed status line: the editor's controls while typing (keyboard or
/// controller), else the last status message.
pub fn seed_status_line(seed: &CustomSeedState, mode: InputMode) -> String {
    if seed.editing && mode.is_pad() {
        return PAD_TEXT_HINT.to_string();
    }
    seed.status_msg.clone().unwrap_or_else(|| {
        mode.pick3("SAME CODE = SAME BUNKER // TYPE A WORD OR ROLL ONE", "SAME CODE = SAME BUNKER // A: EDIT OR ROLL ONE", TOUCH_SEED_HINT).to_string()
    })
}

/// A tap on the SEED switch in touch mode never opens the keyboard-only
/// editor (there is no keyboard): it shows [`TOUCH_SEED_HINT`] instead
/// (`RND` / `CLR` still work). Returns whether the tap was handled here.
pub fn touch_seed_tap(seed: &mut CustomSeedState, mode: InputMode, action: MenuAction) -> bool {
    if action != MenuAction::ToggleSeedEdit || !mode.is_touch() || seed.editing {
        return false;
    }
    seed.status_msg = Some(TOUCH_SEED_HINT.to_string());
    true
}

/// Label of the pause / end screen SHARE SEED switch (confirms once armed).
pub fn share_button_label(run: &RunStats, seed: &CustomSeedState, mode: InputMode) -> String {
    let key = mode.pick3("  [S]", "", "");
    match shareable_code(run) {
        Some(code) if seed.code == code && !seed.editing => format!("SEED {code} ARMED{key}"),
        _ => format!("SHARE SEED{key}"),
    }
}

/// Run-seed characters cycled by the controller (`seed_char_ok`).
pub const SEED_PAD_CHARSET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-";

/// Controller seed editor on the main menu (after [`seed_entry_keys`]):
/// while the editor is open the focus is locked and [`PAD_TEXT_HINT`]
/// applies: Up / Down cycle the last character, Right adds, Left deletes,
/// `A` locks the code, `B` cancels back to the code from before editing.
pub fn seed_entry_pad(
    pad: Res<GamepadInput>,
    mut seed: ResMut<CustomSeedState>,
    mut lock: ResMut<PadFocusLock>,
    mut state: Local<(bool, String)>,
) {
    let (was_editing, backup) = &mut *state;
    if !*was_editing && seed.editing {
        backup.clone_from(&seed.code);
    }
    // The press that opened the editor never also edits / closes it.
    if *was_editing && seed.editing {
        if pad.back {
            let before = std::mem::take(backup);
            seed.code = before;
            seed.lock();
            seed.status_msg = Some("SEED EDIT CANCELLED".into());
        } else if pad.confirm {
            seed.lock();
            #[cfg(target_arch = "wasm32")]
            crate::compendium::sync_url_hash_seed(&seed.code);
        } else {
            let mut buf = seed.code.clone();
            if pad_edit_text(&mut buf, &pad, SEED_PAD_CHARSET, crate::compendium::SEED_CODE_MAX) {
                seed.code = buf;
            }
        }
    }
    *was_editing = seed.editing;
    if lock.0 != seed.editing {
        lock.0 = seed.editing;
    }
}

fn release_seed_pad_lock(mut lock: ResMut<PadFocusLock>) {
    lock.0 = false;
}

/// Inline seed editor on the main menu (runs after [`menu_keys`], which is
/// muted while editing): `S` opens it, `A-Z` / `0-9` / `-` type, `Backspace`
/// deletes, `Enter` locks, `Esc` closes. Consumes ordered `KeyboardInput`
/// messages when present so fast same-frame typing preserves character order.
pub fn seed_entry_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut key_events: MessageReader<bevy::input::keyboard::KeyboardInput>,
    mut seed: ResMut<CustomSeedState>,
    controls: Option<Res<ControlsOverlayOpen>>,
) {
    let pressed_ordered: Vec<KeyCode> = key_events
        .read()
        .filter(|e| e.state == bevy::input::ButtonState::Pressed && !e.repeat)
        .map(|e| e.key_code)
        .collect();
    if !seed.editing {
        // The controls overlay covers the seed strip: `S` stays inert there.
        if controls.is_some_and(|c| c.0) {
            return;
        }
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
    mode: Res<InputMode>,
    run: Res<RunStats>,
    buttons: Query<(&MenuAction, &Children)>,
    mut status: Query<(&mut Text, &mut Node), With<SeedStatusText>>,
    mut labels: Query<&mut Text, Without<SeedStatusText>>,
    mut telemetry: Query<&mut Node, (With<ModeTelemetry>, Without<SeedStatusText>)>,
) {
    if !seed.is_changed() && !mode.is_changed() {
        return;
    }
    for (action, children) in &buttons {
        let want = match action {
            MenuAction::ToggleSeedEdit => seed_button_label(&seed, *mode),
            MenuAction::ShareSeed => share_button_label(&run, &seed, *mode),
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
    let line = seed_status_line(&seed, *mode);
    let on = seed_status_visible(&seed);
    let show = |on: bool| if on { Display::Flex } else { Display::None };
    for (mut t, mut node) in &mut status {
        if t.0 != line {
            t.0 = line.clone();
        }
        if node.display != show(on) {
            node.display = show(on);
        }
    }
    for mut node in &mut telemetry {
        if node.display != show(!on) {
            node.display = show(!on);
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
                width: Val::Percent(100.0),
                column_gap: Val::Px(18.0),
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
                // Fluid: three cards share the row at 960x540 embeds up to 1080p.
                Node {
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: Val::Px(260.0),
                    min_width: Val::Px(220.0),
                    max_width: Val::Px(336.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Stretch,
                    border: plate_rect(UPGRADE_CARD_BORDER),
                    ..default()
                },
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
                    children![
                        (DualPrompt::new(format!("KEY [{}]", i + 1), "").with_touch(""), text(format!("KEY [{}]", i + 1), 11.0, ASH_DIM)),
                        stamped_tag(&tag, tag_bg, INK_BLACK, 12.0)
                    ],
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
            children![(DualPrompt::new(format!("INSTALL  [{}]", i + 1), "INSTALL  [A]"), text(format!("INSTALL  [{}]", i + 1), 19.0, INK_BLACK))],
        ));
    }
    let foot = centered(&mut commands, phone);
    commands.spawn((ChildOf(foot), text(installed_list(&run), 14.0, ASH_DIM)));
    let hint = centered(&mut commands, phone);
    commands.spawn((ChildOf(hint), stamped_tag(UPGRADE_HINT_KBM, HAZARD_YELLOW, INK_BLACK, 13.0), DualPrompt::new(UPGRADE_HINT_KBM, UPGRADE_HINT_PAD).with_touch(UPGRADE_HINT_TOUCH)));
}

/// App Store footer hint per input mode.
pub const UPGRADE_HINT_KBM: &str = "CLICK A CARD OR PRESS 1 / 2 / 3";
pub const UPGRADE_HINT_PAD: &str = "LEFT / RIGHT: CHOOSE  //  A: INSTALL";
pub const UPGRADE_HINT_TOUCH: &str = "TAP A CARD TO INSTALL";

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
    ptr: Res<UiPointer>,
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
    let mut chosen = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3].iter().position(|k| keys.just_pressed(*k));
    let clicked = ptr.nearest_click(cards.iter().map(|(n, tf, c, _, _)| (c.0, n, tf)), None);
    let hovered = ptr.nearest_over(cards.iter().map(|(n, tf, c, _, _)| (c.0, n, tf)), None);
    for (_, _, card, mut bg, mut border) in &mut cards {
        let pointer_hovered = hovered == Some(card.0);
        // Controller navigation owns the highlight until the mouse moves, but
        // mouse releases always activate the card under the actual pointer.
        let lit = if pad.active { card.0 == pad_card.0 } else { pointer_hovered };
        bg.0 = if lit { COLOR_CARD_HOVER } else { COLOR_CARD };
        let want = upgrade_card_border(lit);
        if *border != want {
            *border = want;
        }
    }
    chosen = chosen.or(clicked);
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
    (store, seed, lb, mode): (&ProfileStore, &CustomSeedState, &LeaderboardState, InputMode),
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
                min_width: Val::Px(END_PANEL_MIN_WIDTH),
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
    // Movement line + the peak Style Rank badge (+ the global Daily ranking,
    // SUBMITTING... -> GLOBAL RANK #N) share one row: no extra row at 720p.
    let row = commands.spawn((ChildOf(readout), Node { column_gap: Val::Px(14.0), align_items: AlignItems::Center, ..default() })).id();
    commands.spawn((ChildOf(row), text(movement_line(run), 14.0, ARC_CYAN)));
    if let Some(rank) = run.peak_style_rank {
        commands.spawn((ChildOf(row), StylePeakBadge, stamped_tag(&peak_style_label(rank), rank.color(), INK_BLACK, 13.0)));
    }
    if run.mode.daily_date().is_some() {
        commands.spawn((ChildOf(row), submit_badge_bundle(lb, 14.0)));
    }
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
    let (panel, _) = section_crate(
        commands,
        root,
        "RUN UNLOCKS",
        look.accent,
        Node { max_width: Val::Px(1000.0), min_width: Val::Px(END_PANEL_MIN_WIDTH), align_items: AlignItems::Center, ..default() },
    );
    commands.entity(panel).insert((MetaBox::UnlockPanel, hard_shadow(6.0)));
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
        commands.spawn((ChildOf(cta), prompt_button(state, ENDLESS_CTA, MenuAction::ContinueEndless, 24.0, ButtonStyle::HazardAmber)));
        commands.spawn((
            ChildOf(cta),
            text("FLOOR 6+  //  KEEP HP, ARMOR, AMMO & APPS", 12.0, COLOR_ENDLESS.with_alpha(0.9)),
        ));
    }
    let mut buttons = look.buttons.to_vec();
    let share = share_button_label(run, seed, mode);
    if shareable_code(run).is_some() {
        buttons.push((&share, MenuAction::ShareSeed, ButtonStyle::ScrapSteel));
    }
    if run.mode.daily_date().is_some() {
        buttons.push((LEADERBOARD_BUTTON, MenuAction::OpenLeaderboard, ButtonStyle::PrimaryAcid));
    }
    button_row_in(commands, root, state, &buttons, if compact { 18.0 } else { 21.0 }, if compact { 2.0 } else { 10.0 });
}

/// Shared minimum width of the end-card run readout and RUN UNLOCKS crate, so
/// the two plates stack as one column instead of two ragged widths.
pub const END_PANEL_MIN_WIDTH: f32 = 710.0;

/// Marker on the end-card peak Style Rank badge.
#[derive(Component)]
pub struct StylePeakBadge;

/// End-card badge text: `PEAK STYLE  SS // 6G`.
pub fn peak_style_label(rank: crate::style::StyleRank) -> String {
    format!("PEAK STYLE  {} // {}", rank.letter(), rank.tag())
}

/// `<line>  //  SEED <code>` (unchanged for seedless runs).
pub fn with_seed(line: String, run: &RunStats) -> String {
    match shareable_code(run) {
        Some(code) => format!("{line}  //  SEED {code}"),
        None => line,
    }
}

/// End-card switch to the global leaderboards.
pub const LEADERBOARD_BUTTON: &str = "LEADERBOARD  [K]";

/// Victory call to action of Endless Overdrive.
pub const ENDLESS_CTA: &str = "KEEP DESCENDING (ENDLESS)  [E]";

#[allow(clippy::too_many_arguments)]
fn spawn_game_over(
    mut commands: Commands,
    mode: Res<InputMode>,
    run: Res<RunStats>,
    loadout: Res<RunLoadout>,
    store: Res<ProfileStore>,
    seed: Res<CustomSeedState>,
    lb: Res<LeaderboardState>,
    tex: Option<Res<UiKitTextures>>,
) {
    spawn_end_screen(&mut commands, AppState::GameOver, no_service_look(), false, &run, &loadout, (&store, &seed, &lb, *mode), tex.as_deref());
}

#[allow(clippy::too_many_arguments)]
fn spawn_victory(
    mut commands: Commands,
    mode: Res<InputMode>,
    run: Res<RunStats>,
    loadout: Res<RunLoadout>,
    store: Res<ProfileStore>,
    trial: Res<TimeTrialState>,
    seed: Res<CustomSeedState>,
    lb: Res<LeaderboardState>,
    tex: Option<Res<UiKitTextures>>,
) {
    if run.mode == RunMode::TimeTrial {
        spawn_trial_finish(&mut commands, &run, &trial, (&store, &lb), tex.as_deref());
        return;
    }
    spawn_end_screen(&mut commands, AppState::Victory, full_signal_look(), true, &run, &loadout, (&store, &seed, &lb, *mode), tex.as_deref());
}

/// Time Trial finish card: final time, PB delta / NEW PB, movement telemetry,
/// checkpoint splits and RETRY [R] / MAIN MENU [ENTER].
fn spawn_trial_finish(commands: &mut Commands, run: &RunStats, trial: &TimeTrialState, (store, lb): (&ProfileStore, &LeaderboardState), tex: Option<&UiKitTextures>) {
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
    // Global Time Trial ranking (SUBMITTING... -> GLOBAL RANK #N).
    commands.spawn((ChildOf(root), submit_badge_bundle(lb, 17.0)));
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
    button_row_in(
        commands,
        root,
        AppState::Victory,
        &[
            ("RETRY  [R]", MenuAction::RetryTimeTrial, ButtonStyle::HeroAcid),
            ("MAIN MENU  [ENTER]", MenuAction::BackToMenu, ButtonStyle::ScrapSteel),
            (LEADERBOARD_BUTTON, MenuAction::OpenLeaderboard, ButtonStyle::PrimaryAcid),
        ],
        20.0,
        8.0,
    );
}

/// In-run pause overlay (`Esc` / `P`): a slim terminal over the frozen 3D
/// scene (light scrim) to resume, restart, tweak settings / loadout, read
/// the controls, or quit.
fn spawn_pause_menu(
    mut commands: Commands,
    mode: Res<InputMode>,
    run: Res<RunStats>,
    loadout: Res<RunLoadout>,
    seed: Res<CustomSeedState>,
    tex: Option<Res<UiKitTextures>>,
) {
    let tex = tex.as_deref();
    let root = punk_screen_root(&mut commands, AppState::Paused, tex, Color::srgba(0.02, 0.015, 0.012, PAUSE_SCRIM), 0.16);
    let card = terminal_chassis(&mut commands, root, tex, PAUSE_WIDTH, false);
    commands.entity(card).insert(HiddenByControls);
    let title = centered(&mut commands, card);
    stencil_title(&mut commands, title, "PAUSED", 60.0, BONE_TEXT, PUNK_RED);
    // Compact 2-line run readout in a recessed slot: read, not pressed.
    let readout = commands
        .spawn((
            ChildOf(card),
            Node {
                margin: UiRect::top(Val::Px(4.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(3.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                border: plate_rect((4.0, 2.0, 2.0, 2.0)),
                ..default()
            },
            BorderColor { left: HAZARD_YELLOW, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.74)),
        ))
        .id();
    commands.spawn((ChildOf(readout), text(pause_summary(&run), 16.0, BONE_TEXT)));
    commands.spawn((ChildOf(readout), text(pause_loadout_line(&loadout, &run), 12.0, HAZARD_YELLOW.with_alpha(0.9))));
    let wrap = centered(&mut commands, card);
    let touch = mode.is_touch();
    let btns = commands.spawn((ChildOf(wrap), pause_stack_node(touch))).id();
    // Touch: every switch sits in a wrapper (full width, or one cell of a
    // 2x2 grid) so the finger targets are big and the stack stays short.
    let slot = |commands: &mut Commands, full: bool| -> Entity {
        if !touch {
            return btns;
        }
        commands.spawn((ChildOf(btns), pause_cell_node(full))).id()
    };
    // RESUME is the one loud switch; the exits step down in weight.
    for (label, action, style, font, full) in [
        ("RESUME  [ESC]", MenuAction::Continue, ButtonStyle::HeroAcid, 22.0, true),
        ("RESTART RUN  [R]", MenuAction::RestartRun, ButtonStyle::HazardAmber, 16.0, true),
        ("SETTINGS  [O]", MenuAction::OpenSettings, ButtonStyle::ScrapSteel, 16.0, false),
        ("LOADOUT  [L]", MenuAction::OpenLoadout, ButtonStyle::ScrapSteel, 16.0, false),
        (CONTROLS_BUTTON, MenuAction::ToggleControls, ButtonStyle::ScrapSteel, 16.0, false),
    ] {
        let parent = slot(&mut commands, full);
        commands.spawn((ChildOf(parent), prompt_button(AppState::Paused, label, action, font, style)));
    }
    if shareable_code(&run).is_some() {
        let parent = slot(&mut commands, false);
        commands.spawn((ChildOf(parent), punk_button(&share_button_label(&run, &seed, *mode), MenuAction::ShareSeed, 16.0, ButtonStyle::ScrapSteel)));
    }
    // The red exit always closes the stack (SHARE SEED sits above it).
    let parent = slot(&mut commands, true);
    commands.spawn((ChildOf(parent), prompt_button(AppState::Paused, "MAIN MENU  [M]", MenuAction::BackToMenu, 16.0, ButtonStyle::DangerRed)));
    spawn_input_hint(&mut commands, card);
    spawn_controls_overlay(&mut commands, root, AppState::Paused, *mode);
}

/// Pause switch stack: one column (mouse / controller), or in touch mode a
/// wrapping 460 px row (RESUME / RESTART / MAIN MENU full width, the four
/// utility switches as a 2x2 grid).
pub fn pause_stack_node(touch: bool) -> Node {
    let base = Node {
        margin: UiRect { top: Val::Px(10.0), bottom: Val::Px(10.0), ..default() },
        align_items: AlignItems::Stretch,
        row_gap: Val::Px(8.0),
        ..default()
    };
    if touch {
        Node { flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(8.0), width: Val::Px(PAUSE_TOUCH_STACK), ..base }
    } else {
        Node { flex_direction: FlexDirection::Column, width: Val::Px(340.0), ..base }
    }
}

/// Touch pause grid width (logical px).
pub const PAUSE_TOUCH_STACK: f32 = 460.0;

/// One touch pause cell: full width, or half the grid.
pub fn pause_cell_node(full: bool) -> Node {
    let cell = Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, ..default() };
    if full {
        Node { width: Val::Percent(100.0), ..cell }
    } else {
        Node { flex_basis: Val::Px((PAUSE_TOUCH_STACK - 8.0) * 0.5 - 1.0), flex_grow: 1.0, ..cell }
    }
}

/// Smallest touch target of a menu switch in touch mode (UI px).
pub const TOUCH_MENU_TARGET: f32 = 64.0;

/// `(min_width, min_height)` of a menu switch whose current minimums are
/// `(w, h)`: raised to [`TOUCH_MENU_TARGET`] in touch mode (a hero switch's own
/// larger minimum is kept), and only our own raise is undone otherwise.
pub fn touch_target_min(touch: bool, w: Val, h: Val) -> (Val, Val) {
    let raise = |v: Val| match v {
        Val::Auto => Val::Px(TOUCH_MENU_TARGET),
        Val::Px(px) if px < TOUCH_MENU_TARGET => Val::Px(TOUCH_MENU_TARGET),
        v => v,
    };
    let lower = |v: Val| if v == Val::Px(TOUCH_MENU_TARGET) { Val::Auto } else { v };
    if touch { (raise(w), raise(h)) } else { (lower(w), lower(h)) }
}

/// Keep every menu switch finger-sized in touch mode (and back otherwise).
fn touch_menu_targets(mode: Option<Res<InputMode>>, mut buttons: Query<(Ref<MenuButton>, &mut Node)>) {
    let touch = mode.as_deref().is_some_and(|m| m.is_touch());
    let all = mode.is_some_and(|m| m.is_changed());
    for (tag, mut node) in &mut buttons {
        if !(all || tag.is_added()) {
            continue;
        }
        let (w, h) = touch_target_min(touch, node.min_width, node.min_height);
        if node.min_width != w || node.min_height != h {
            node.min_width = w;
            node.min_height = h;
        }
    }
}

/// Pause scrim alpha: the frozen run stays readable behind the terminal.
pub const PAUSE_SCRIM: f32 = 0.5;
/// Pause terminal width (logical px).
pub const PAUSE_WIDTH: f32 = 520.0;

/// Pause readout line 1: `FLOOR 2/5  |  03:12  |  41 KILLS  |  2 JAMMERS`.
pub fn pause_summary(run: &RunStats) -> String {
    let t = run.time_elapsed as u32;
    format!("{}  |  {:02}:{:02}  |  {} KILLS  |  {} JAMMERS", floor_label(run), t / 60, t % 60, run.kills, run.jammers_destroyed)
}

/// Pause readout line 2: chip + installed app count (+ contracts, seed code).
pub fn pause_loadout_line(loadout: &RunLoadout, run: &RunStats) -> String {
    let apps = run.upgrades.len();
    let mut line = format!("CHIP {}  |  {} APP{}", crate::meta_ui::chip_label(loadout.chip), apps, if apps == 1 { "" } else { "S" });
    if let Some(tags) = crate::meta_ui::contract_tags(loadout) {
        line = format!("{line}  |  {tags}");
    }
    match shareable_code(run) {
        Some(code) => format!("{line}  |  SEED {code}"),
        None => line,
    }
}

/// Android input hint shown under the Main Menu and Pause actions
/// (Milestone 37: touch is first-class; a controller or mouse also works).
pub const ANDROID_INPUT_HINT: &str = "TOUCH CONTROLS ON // CONTROLLER OR MOUSE & KEYBOARD ALSO WORK // BACK PAUSES";

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
    (lb, controls): (&mut LeaderboardState, &mut ControlsOverlayOpen),
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
        MenuAction::ToggleControls => controls.0 = !controls.0,
        MenuAction::CloseControls => controls.0 = false,
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
        MenuAction::OpenLeaderboard => {
            finalize_endless(run, loadout, store);
            next.set(AppState::Leaderboard);
        }
        // Handled by `leaderboard::leaderboard_input` (it owns the client).
        MenuAction::LbTab
        | MenuAction::LbRefresh
        | MenuAction::LbCallsign
        | MenuAction::LbAccount
        | MenuAction::LbPrevPage
        | MenuAction::LbNextPage
        | MenuAction::LbEdit(_) => lb.pending.push(action),
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn menu_buttons(
    mut commands: Commands,
    players: Query<Entity, With<Player>>,
    ptr: Res<UiPointer>,
    mut buttons: Query<
        (Entity, &ComputedNode, &UiGlobalTransform, &MenuAction, &PunkButtonColors, &mut BackgroundColor, &mut BorderColor, Option<&Children>),
        With<MenuButton>,
    >,
    (pad, focus, mode): (Res<GamepadInput>, Res<GamepadFocus>, Option<Res<InputMode>>),
    mut labels: Query<&mut TextColor>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    (mut store, mut loadout, mut trial, mut seed): (ResMut<ProfileStore>, ResMut<RunLoadout>, ResMut<TimeTrialState>, ResMut<CustomSeedState>),
    time: Res<Time>,
    (state, mut ret): (Res<State<AppState>>, ResMut<SettingsReturn>),
    mut next: ResMut<NextState<AppState>>,
    (mut lb, mut controls): (ResMut<LeaderboardState>, ResMut<ControlsOverlayOpen>),
    (mut gate, mut sfx): (Option<ResMut<UiSfxGate>>, Option<ResMut<SfxQueue>>),
) {
    // Grown finger plates can overlap: the nearest center wins (never list
    // order), and a click needs the press to have started on that button.
    let clicked = ptr.nearest_click(buttons.iter().map(|(e, n, tf, ..)| (e, n, tf)), None);
    let lit = ptr.nearest_over(buttons.iter().map(|(e, n, tf, ..)| (e, n, tf)), None);
    let mode = mode.map_or(InputMode::KeyboardMouse, |m| *m);
    for (e, _, _, action, colors, mut bg, mut border, children) in &mut buttons {
        // Controller navigation owns the highlight until the mouse moves (a
        // finger lights its button only while it is down).
        let pointer_hovered = lit == Some(e);
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
        if clicked == Some(e) || focus.activate == Some(e) {
            // The controller path already blipped in `gamepad_menu_focus`;
            // the gate swallows the duplicate.
            if let (Some(g), Some(q)) = (gate.as_deref_mut(), sfx.as_deref_mut()) {
                g.play(q, SfxEvent::UiConfirm, time.elapsed_secs());
            }
            if clicked == Some(e) && touch_seed_tap(&mut seed, mode, *action) {
                return;
            }
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
                (&mut lb, &mut controls),
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
    (mut lb, mut controls): (ResMut<LeaderboardState>, ResMut<ControlsOverlayOpen>),
    (mut gate, mut sfx): (Option<ResMut<UiSfxGate>>, Option<ResMut<SfxQueue>>),
) {
    // The leaderboard owns its keys (`leaderboard::leaderboard_input`).
    if *state.get() == AppState::Leaderboard {
        return;
    }
    let mut blip = |now: f32| {
        if let (Some(g), Some(q)) = (gate.as_deref_mut(), sfx.as_deref_mut()) {
            g.play(q, SfxEvent::UiConfirm, now);
        }
    };
    let pad_editing = *state.get() == AppState::MainMenu && seed.editing;
    // An open controls overlay eats `Esc` / controller `B` first: Pause never
    // resumes from under it.
    if !pad_editing && controls_back_closes(*state.get(), controls.0, keys.just_pressed(KeyCode::Escape), pad.back) {
        blip(time.elapsed_secs());
        controls.0 = false;
        return;
    }
    // Controller shortcuts (never typed text). While the controller seed
    // editor is open `B` cancels the edit instead (see `seed_entry_pad`).
    let pad_action = if pad_editing { None } else { pad_menu_action(*state.get(), pad.pause, pad.back) };
    if let Some(action) = pad_action.or_else(|| pad_retry_action(*state.get(), run.mode, pad.interact)) {
        blip(time.elapsed_secs());
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
            (&mut lb, &mut controls),
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
    .or_else(|| seed_hotkey_action(*state.get(), run.mode, keys.just_pressed(KeyCode::KeyB), keys.just_pressed(KeyCode::KeyS)))
    .or_else(|| leaderboard_hotkey_action(*state.get(), keys.just_pressed(KeyCode::KeyK)))
    .or_else(|| controls_hotkey_action(*state.get(), keys.just_pressed(KeyCode::KeyH)));
    if let Some(action) = action {
        blip(time.elapsed_secs());
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
            (&mut lb, &mut controls),
        );
    }
}

// ---------------------------------------------------------------------------
// In-game HUD
// ---------------------------------------------------------------------------
//
// Layout (the 3D phone in the left hand carries signal / radar / apps, so the
// bottom-left stays clear). The whole HUD is frameless, like the floating
// style emblem: no plates and no backing slabs. Text floats on hard ink
// shadows, every gauge keeps a dark track under its fill, and each cluster
// hangs off one razor-thin glowing accent (see `hud_fx.rs`).
// - top-left: floor strip on a glowing rail (floor, sector, counters, loadout)
// - top-center: status line over a glowing underline (+ SOS uplink bar while
//   uploading), boss bar and alert below
// - top-right: split timer, then the killfeed callouts (right-edge pips)
// - right edge: the style rank emblem
// - bottom-center: VEL tachometer, HP, ARMOR | STAMINA (+ the stamped
//   `BHOP xN` tag), the prompt above
// - bottom-right: weapon & ammo deck (title + buff pills, ammo count, shell
//   pips / plasma bar / warhead pips, 3 slot tabs)
// - edges: subtle dark-red Dead-Zone frame (no scanlines across the view)

/// Dark well under every gauge fill: the only backing the HUD keeps.
const TRACK_BG: Color = Color::srgba(0.02, 0.018, 0.016, 0.86);
/// Ink outline of a gauge track.
const TRACK_EDGE: Color = Color::srgba(0.0, 0.0, 0.0, 0.9);
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
/// Spent ammo pip / idle keycap: a dark cell, so the row keeps its shape over bright floors.
const COLOR_PIP_OFF: Color = Color::srgba(0.02, 0.018, 0.016, 0.7);
const COLOR_PIP_OFF_BORDER: Color = Color::srgba(0.0, 0.0, 0.0, 0.85);
/// Deep ember the dry-magazine count throbs down to.
const COLOR_EMBER: Color = Color::srgb(0.5, 0.05, 0.04);

const TOAST_SLOTS: u8 = 5;
/// Shell pips shown for the shotgun magazine.
pub const SHELL_PIPS: u8 = 10;
/// Warhead pips shown for the Signal-Mortar rack.
pub const ROCKET_PIPS: u8 = 10;
/// Width of the weapon deck (matches the vitals cluster).
const DECK_WIDTH: f32 = 312.0;
/// Laid-out height budget of the weapon deck: title row, ammo count, pips, slot tabs.
pub const DECK_HEIGHT: f32 = 104.0;
/// Weapon title font of the deck header.
const WEAPON_TITLE_FONT: f32 = 14.0;
/// Hero ammo count font.
const AMMO_FONT: f32 = 36.0;
/// Buff pills docked left of the weapon title.
const OVERCLOCK_PILL: &str = "5G OC";
const DOPPLER_PILL: &str = "DOPPLER";
const PILL_FONT: f32 = 11.0;
const PILL_PAD_X: f32 = 6.0;
const PILL_GAP: f32 = 5.0;
/// Ammo pip cell (shells and warheads) and the gap between two.
const PIP_SIZE: (f32, f32) = (18.0, 10.0);
const PIP_GAP: f32 = 3.0;
/// Width of a full pip row (and of the plasma cell bar that replaces it).
const PIP_ROW_WIDTH: f32 = SHELL_PIPS as f32 * PIP_SIZE.0 + (SHELL_PIPS - 1) as f32 * PIP_GAP;
/// Slot tab metrics: padding, keycap, gaps and the two font sizes.
const SLOT_GAP: f32 = 8.0;
const SLOT_PAD_X: f32 = 1.0;
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

/// Inner (content) width of one slot tab: the deck split three ways.
#[cfg(test)]
fn slot_inner_width() -> f32 {
    let n = Weapon::ALL.len() as f32;
    (DECK_WIDTH - SLOT_GAP * (n - 1.0)) / n - 2.0 * SLOT_PAD_X
}

/// Width one slot badge row needs for `name` and an ammo count of `digits`.
#[cfg(test)]
fn slot_row_width(name: &str, digits: usize) -> f32 {
    SLOT_KEY_WIDTH + SLOT_INNER_GAP + mono_width(name.len(), SLOT_LABEL_FONT) + SLOT_INNER_GAP + mono_width(digits, SLOT_AMMO_FONT)
}

/// Slot tab reserve color: red when dry, bone on the equipped weapon, muted otherwise.
fn slot_ammo_color(n: u32, active: bool) -> Color {
    if n == 0 {
        COLOR_SIGNAL_RED
    } else if active {
        COLOR_TEXT
    } else {
        HUD_SUB
    }
}
const VITALS_WIDTH: f32 = 312.0;
/// Laid-out height budget of the vitals cluster: VEL row, HP, ARMOR | STAMINA.
pub const VITALS_HEIGHT: f32 = 84.0;
/// Extra height while the `BHOP xN` stamp shows above the VEL row.
const BHOP_TAG_HEIGHT: f32 = 24.0;
/// Bottom inset of the bottom clusters.
pub const HUD_BOTTOM: f32 = 14.0;
/// Bottom of the interact prompt: clears the vitals cluster and its BHOP stamp.
pub const PROMPT_BOTTOM: f32 = HUD_BOTTOM + VITALS_HEIGHT + BHOP_TAG_HEIGHT + 8.0;
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

/// Top status line while the boss holds the upload (the uplink panel under
/// it carries [`crate::boss::BOSS_HOLD_TEXT`]).
pub const BOSS_HOLD_STATUS: &str = "CARRIER DAEMON ALIVE - EXTRACTION LOCKED";

/// Top status line while uploading (boss floors hold at 99%).
pub fn uplink_status_text(ext: &ExtractionState) -> &'static str {
    match ext {
        ExtractionState::Uploading { boss_hold: true, .. } => BOSS_HOLD_STATUS,
        _ => "DEMON RUSH - SOS UPLINK LIVE",
    }
}

/// Milestone 32 free-roam SOS uplink speed tier (drives the HUD copy + tone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkTier {
    /// Inside the Signal Ring: `SOS OVERDRIVE` (acid green boost).
    Overdrive,
    /// Near the tower (>= 0.80x): cyan.
    Near,
    /// Roaming further (< 0.80x): warm amber, motivating `CLOSER = FASTER`.
    Roaming,
}

/// Tier of a live upload `rate` (see [`crate::extraction::proximity_upload_rate`]).
pub fn uplink_tier(rate: f32) -> UplinkTier {
    if rate >= crate::extraction::UPLINK_OVERDRIVE_RATE {
        UplinkTier::Overdrive
    } else if rate >= crate::extraction::UPLINK_NEAR_RATE {
        UplinkTier::Near
    } else {
        UplinkTier::Roaming
    }
}

/// HUD tone of an uplink tier: boost green, cyan, warm amber (never alarm red).
pub fn uplink_tier_color(tier: UplinkTier) -> Color {
    match tier {
        UplinkTier::Overdrive => ACID_GREEN,
        UplinkTier::Near => COLOR_CYAN,
        UplinkTier::Roaming => COLOR_SIGNAL_AMBER,
    }
}

/// Write the live (unjammed, unheld) uplink label for `progress` / `rate`:
/// `SOS OVERDRIVE  1.25x  42%`, `UPLINKING  0.93x  42%` or
/// `UPLINKING  0.61x  (CLOSER = FASTER)  42%`. Returns its tier.
pub fn write_uplink_speed(b: &mut impl std::fmt::Write, progress: f32, rate: f32) -> UplinkTier {
    let pct = progress * 100.0;
    let tier = uplink_tier(rate);
    let _ = match tier {
        UplinkTier::Overdrive => write!(b, "SOS OVERDRIVE  {rate:.2}x  {pct:.0}%"),
        UplinkTier::Near => write!(b, "UPLINKING  {rate:.2}x  {pct:.0}%"),
        UplinkTier::Roaming => write!(b, "UPLINKING  {rate:.2}x  (CLOSER = FASTER)  {pct:.0}%"),
    };
    tier
}

/// Top-center status line: its kind (drives the underline), text and tone.
pub fn status_line(run: &RunStats, trial: &TimeTrialState, ext: &ExtractionState, signal: &SignalState) -> (StatusKind, &'static str, Color) {
    if run.mode == RunMode::TimeTrial {
        return if trial.next_gate >= crate::trials::TRIAL_CHECKPOINTS {
            (StatusKind::TrialFinal, "FINAL STRETCH - LAND ON THE TOWER PAD", ARC_CYAN)
        } else {
            (StatusKind::Trial, "TIME TRIAL - 3 CHECKPOINTS, THEN THE TOWER", ARC_CYAN)
        };
    }
    match *ext {
        ExtractionState::Uploading { boss_hold, .. } => (if boss_hold { StatusKind::BossHold } else { StatusKind::Rush }, uplink_status_text(ext), COLOR_SIGNAL_RED),
        ExtractionState::Extracting { .. } | ExtractionState::Complete => (StatusKind::Extracting, "ORBITAL EXTRACTION IN PROGRESS", COLOR_CYAN),
        _ if signal.near_tower => (StatusKind::Locked, "5-BAR SIGNAL - EXTRACTION TOWER IN RANGE", COLOR_SIGNAL_GREEN),
        _ if signal.in_dead_zone => (StatusKind::DeadZone, "NO SERVICE - DEAD ZONE", COLOR_SIGNAL_RED),
        _ => (StatusKind::Searching, "SEARCH FOR 5-BAR SIGNAL TOWER", COLOR_SIGNAL_AMBER),
    }
}

/// Accent of the top-left floor rail: alert red in a dead zone or during the
/// rush, cyan on the Time Trial, magenta in Endless, caution amber otherwise.
pub fn floor_rail_color(run: &RunStats, kind: StatusKind) -> Color {
    match kind {
        StatusKind::Rush | StatusKind::BossHold | StatusKind::DeadZone => PUNK_RED,
        StatusKind::Trial | StatusKind::TrialFinal => ARC_CYAN,
        _ if run.endless => COLOR_ENDLESS,
        _ => HAZARD_YELLOW,
    }
}

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

/// Decay the HUD pulse envelopes and re-arm the ones whose value changed.
#[allow(clippy::too_many_arguments)]
fn track_hud_pulse(
    time: Res<Time>,
    mut pulse: ResMut<HudPulse>,
    run: Res<RunStats>,
    trial: Res<TimeTrialState>,
    ext: Res<ExtractionState>,
    signal: Res<SignalState>,
    ws: Res<WeaponState>,
    ghost: Option<Res<crate::trials::TrialGhostStore>>,
    players: Query<&Player>,
) {
    let Ok(p) = players.single() else { return };
    let is_trial = run.mode == RunMode::TimeTrial;
    pulse.tick(time.delta_secs());
    pulse.observe(HudSnapshot {
        stats: if is_trial { [trial.next_gate.min(crate::trials::TRIAL_CHECKPOINTS) as u32, trial.rescues] } else { [run.kills, run.jammers_destroyed] },
        status: status_line(&run, &trial, &ext, &signal).0,
        splits: if is_trial { trial.splits.len() } else { run.floor_splits.len() } as u32,
        ghost_ahead: ghost.as_deref().filter(|_| is_trial).and_then(|g| g.delta).filter(|d| d.is_finite()).map(|d| d <= 0.0),
        weapon: Weapon::ALL.iter().position(|w| *w == ws.current).unwrap_or(0) as u8,
        ammo: ammo(p, ws.current).0,
    });
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
    /// Top-left counter `0` (kills / checkpoints) or `1` (jammers / rescues).
    Stats(u8),
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
    /// Glowing rail of the top-left floor strip (mode / state colored).
    FloorRail,
    /// Glowing underline of the status line.
    StatusRule,
    /// Alert hairline above / below the status + uplink block (SOS uplink only).
    RushRail,
    UplinkPanel,
    UplinkFill,
    /// Carrier Daemon boss bar block (only while a living boss is present).
    BossPlate,
    /// Boss health fill (crimson / molten gold, cyan-violet while shielded).
    BossFill,
    /// Boss health track: its end caps follow the phase tag color.
    BossTrack,
    AlertPanel,
    /// One killfeed callout row (text + pip).
    ToastChip(u8),
    /// Glowing right-edge pip of a callout.
    ToastPip(u8),
    /// Square bullet between the chip label and the contract tags.
    LoadoutDot,
    /// Framed bracket track around the VEL tachometer segments.
    VelTrack,
    /// Readout badge (snaps solid hazard yellow at 32+ m/s).
    VelValueBadge,
    /// Stamped `BHOP xN` tag floating above the VEL row (hidden below x2).
    VelChainTag,
    /// One segment of the VEL tachometer.
    VelSeg(u8),
    /// HP gauge track: glows alert red while HP is critical.
    HpTrack,
    HpFill,
    ArmorFill,
    StaminaFill,
    ShellRow,
    ShellPip(u8),
    PlasmaTrack,
    PlasmaFill,
    RocketRow,
    RocketPip(u8),
    /// One weapon slot tab (the equipped one pops on a swap).
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

/// Track frame colors: the accent on the left cap, dimmer on the right, ink above and below.
fn track_border(color: Color) -> BorderColor {
    BorderColor { left: color, right: color.with_alpha(0.55), top: TRACK_EDGE, bottom: TRACK_EDGE }
}

/// Bracketed gauge track: a colored left cap, a dimmer right cap and an ink
/// outline around a dark well, so the fill stays legible with no plate behind it.
fn bar_track(width: Val, height: f32, color: Color, display: Display) -> impl Bundle {
    (
        Node { display, width, height: Val::Px(height), border: plate_rect((3.0, 2.0, 1.0, 1.0)), padding: UiRect::all(Val::Px(1.0)), ..default() },
        track_border(color),
        BackgroundColor(TRACK_BG),
    )
}

#[allow(clippy::too_many_arguments)]
/// Floating labelled gauge: `LABEL ......... value` over a framed track
/// (`fonts` = label / value size). Returns the track.
fn spawn_bar(commands: &mut Commands, parent: Entity, block: Node, label: &str, label_color: Color, fonts: (f32, f32), value: HudText, fill: HudBox, height: f32, color: Color) -> Entity {
    let block = commands.spawn((ChildOf(parent), Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..block })).id();
    commands.spawn((
        ChildOf(block),
        Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::FlexEnd, ..default() },
        children![(hud_text(label, fonts.0, label_color), bar_prompt(label)), (value, hud_text("", fonts.1, COLOR_TEXT))],
    ));
    let track = commands.spawn((ChildOf(block), bar_track(Val::Percent(100.0), height, color, Display::Flex))).id();
    commands.spawn((ChildOf(track), fill, Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(color)));
    if height >= 10.0 {
        spawn_ticks(commands, track);
    }
    track
}

/// Stamina bar label per input mode (the dash key / button).
pub const STAMINA_LABEL_KBM: &str = "STAMINA [SHIFT]";
pub const STAMINA_LABEL_PAD: &str = "STAMINA [LB]";
pub const STAMINA_LABEL_TOUCH: &str = "STAMINA [DASH]";

/// Controller variant of a vitals bar label (only STAMINA names a key).
fn pad_bar_label(label: &str) -> &str {
    if label == STAMINA_LABEL_KBM { STAMINA_LABEL_PAD } else { label }
}

/// A bar caption's prompt (the stamina keycap follows the input mode).
fn bar_prompt(label: &str) -> DualPrompt {
    let p = DualPrompt::new(label, pad_bar_label(label));
    if label == STAMINA_LABEL_KBM { p.with_touch(STAMINA_LABEL_TOUCH) } else { p }
}

/// Tower SOS interact prompt per input mode.
pub fn sos_prompt(mode: InputMode) -> &'static str {
    mode.pick3("PRESS [E] TO TRIGGER SOS UPLINK", "PRESS [X] TO TRIGGER SOS UPLINK", "TAP [SOS] TO TRIGGER SOS UPLINK")
}

/// Weapon-deck keycap of slot `i` given the equipped slot: `1 2 3` on mouse
/// & keyboard; on a controller `Y` (next) / `<` (D-Pad Left, previous) on
/// the other slots and `*` on the equipped one.
pub fn slot_key_label(mode: InputMode, i: usize, current: usize, slots: usize) -> char {
    if !mode.is_pad() {
        return char::from_digit((i + 1) as u32, 10).unwrap_or('?');
    }
    let slots = slots.max(1);
    if i == current {
        '*'
    } else if i == (current + 1) % slots {
        'Y'
    } else {
        '<'
    }
}

/// VEL telemetry row, the first line of the vitals cluster:
/// `VEL [tachometer] 18 m/s`. The tachometer is 16 fixed 9 px segments
/// (2 m/s each) in four fixed zones with 4 px breaks at 12 / 20 / 28 m/s,
/// framed by a tier-colored bracket track. A stamped `BHOP xN` tag floats
/// above the row, flush left.
fn spawn_vel_plate(commands: &mut Commands, vitals: Entity) {
    commands.spawn((
        ChildOf(vitals),
        HudBox::VelChainTag,
        Node {
            display: Display::None,
            align_self: AlignSelf::FlexStart,
            padding: UiRect::axes(Val::Px(6.0), Val::Px(1.0)),
            border: UiRect::bottom(Val::Px(2.0)),
            ..default()
        },
        BorderColor::all(INK_BLACK.with_alpha(0.85)),
        BackgroundColor(ASH_DIM),
        children![(HudText::VelChain, text("", 11.0, INK_BLACK), TextLayout::no_wrap())],
    ));
    let row = commands.spawn((ChildOf(vitals), Node { width: Val::Percent(100.0), align_items: AlignItems::Center, column_gap: Val::Px(8.0), ..default() })).id();
    commands.spawn((ChildOf(row), hud_text("VEL", 11.0, HUD_SUB)));
    let track = commands
        .spawn((ChildOf(row), HudBox::VelTrack, bar_track(Val::Px(187.0), 12.0, ASH_DIM, Display::Flex)))
        .id();
    commands.entity(track).entry::<Node>().and_modify(|mut n| n.flex_shrink = 0.0);
    for i in 0..VEL_SEGMENTS {
        let gap = if i + 1 == VEL_SEGMENTS { 0.0 } else if segment_zone_break(i) { 4.0 } else { 2.0 };
        commands.spawn((
            ChildOf(track),
            HudBox::VelSeg(i),
            Node { width: Val::Px(9.0), height: Val::Percent(100.0), flex_grow: 0.0, flex_shrink: 0.0, margin: UiRect::right(Val::Px(gap)), ..default() },
            BackgroundColor(segment_color(i, 0)),
        ));
    }
    commands.spawn((
        ChildOf(row),
        HudBox::VelValueBadge,
        Node { margin: UiRect::left(Val::Auto), padding: UiRect::axes(Val::Px(2.0), Val::Px(0.0)), flex_shrink: 0.0, ..default() },
        BackgroundColor(Color::NONE),
        children![(HudText::VelValue, hud_text(speed_label(0.0), 14.0, BONE_TEXT))],
    ));
}

/// The weapon deck root (moves left of the vitals in touch mode, clearing the
/// bottom-right corner for the FIRE arc; its slot tabs become tappable).
#[derive(Component)]
struct TouchDeck;

/// The top-right split timer / killfeed column (drops under the touch PAUSE).
#[derive(Component)]
struct TouchTopRightColumn;

/// Gap between the vitals cluster and the weapon deck in touch mode (px).
const TOUCH_DECK_GAP: f32 = 12.0;
/// Style meter top in touch mode (left edge, under the floor strip).
pub const STYLE_TOUCH_TOP: f32 = 110.0;

/// Width of the compact touch-mode weapon deck (UI px).
pub const TOUCH_DECK_WIDTH: f32 = 228.0;

/// Parts of the weapon deck the touch re-layout restyles.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum DeckPart {
    /// Buff pills + weapon title row (hidden in touch mode).
    Title,
    /// Hero ammo count row (hidden in touch mode: the equipped tab shows the
    /// same count, and the row would sit on the 3D phone).
    Ammo,
    /// Shell pips / plasma track / warhead rack (hidden in touch mode).
    Gauges,
    /// One slot tab (a tappable plate in touch mode).
    Chip(u8),
    /// A slot tab's `[key] NAME .. reserve` row (stacked in touch mode).
    Row(u8),
    /// A slot tab's keycap chip (hidden in touch mode: no keys on a phone).
    Key(u8),
}

/// Touch-mode deck placement `(left, bottom)` in UI px for a UI of `ui`
/// size: docked left of the bottom-center vitals when that fits inside the
/// gutter, else (narrow / portrait) at the left gutter raised to `raise_to`
/// (UI px from the bottom: above the stick column), never at a negative x
/// and never over the vitals.
pub fn touch_deck_place(ui: Vec2, raise_to: f32) -> (f32, f32) {
    let docked = ui.x * 0.5 - VITALS_WIDTH * 0.5 - TOUCH_DECK_GAP - TOUCH_DECK_WIDTH;
    if docked >= HUD_GUTTER {
        (docked, HUD_BOTTOM)
    } else {
        let clear_vitals = HUD_BOTTOM + VITALS_HEIGHT + BHOP_TAG_HEIGHT + TOUCH_DECK_GAP;
        (HUD_GUTTER, raise_to.max(clear_vitals))
    }
}

/// Touch-mode slot tab plate: ink fill, the weapon's accent rail on the left
/// while equipped (else steel), steel top / right, an ink bottom bevel.
pub fn touch_slot_plate(w: Weapon, equipped: bool) -> (Color, BorderColor) {
    let rail = if equipped { weapon_accent(w) } else { STEEL_BORDER };
    (INK_BLACK.with_alpha(0.55), plate_border(rail, STEEL_BORDER))
}

/// Touch mode re-layout of the in-run HUD: a compact deck (no title, gauges,
/// hero ammo row or keycaps; each slot tab a tappable plate with the name
/// over the reserve) left of the vitals and clear of the 3D phone (raised to
/// the left gutter on narrow / portrait screens), style meter on the left
/// edge (the phone-app row owns the right edge), the top-right column under
/// the PAUSE button; publishes the deck's slot tabs (logical px) for the
/// touch router.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn touch_hud_relayout(
    (mode, store, state): (Option<Res<InputMode>>, Option<Res<ProfileStore>>, Res<State<AppState>>),
    ui_scale: Res<UiScale>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut touch: ResMut<TouchInput>,
    mut deck: Query<&mut Node, (With<TouchDeck>, Without<TouchTopRightColumn>, Without<StyleHud>, Without<DeckPart>)>,
    mut column: Query<&mut Node, (With<TouchTopRightColumn>, Without<TouchDeck>, Without<StyleHud>, Without<DeckPart>)>,
    mut style: Query<(&StyleHud, &mut Node), (Without<TouchDeck>, Without<TouchTopRightColumn>, Without<DeckPart>)>,
    mut parts: Query<(&DeckPart, &mut Node, &mut BackgroundColor, &mut BorderColor), (Without<TouchDeck>, Without<TouchTopRightColumn>, Without<StyleHud>)>,
    mut fonts: Query<(&HudText, &mut TextFont)>,
    ws: Option<Res<WeaponState>>,
    slots: Query<(&HudBox, &ComputedNode, &UiGlobalTransform)>,
) {
    let on = overlay_visible(*state.get(), live_settings(store.as_deref()).touch_mode, mode.map_or(InputMode::KeyboardMouse, |m| *m));
    let k = ui_scale.0.max(0.01);
    let viewport = touch.layout.viewport;
    let (left, right, bottom, width) = if on {
        // Raise above the stick column (incl. the AUTO-BHOP pill) if the
        // deck cannot dock beside the vitals.
        let column_top = touch.layout.button(crate::touch::TouchButton::AutoBhop).map_or(touch.layout.stick_rest.y - touch.layout.stick_radius, |b| b.rect().min.y);
        let raise_to = (viewport.y - column_top) / k + TOUCH_DECK_GAP;
        let (x, b) = touch_deck_place(viewport / k, raise_to);
        (Val::Px(x), Val::Auto, Val::Px(b), Val::Px(TOUCH_DECK_WIDTH))
    } else {
        (Val::Auto, Val::Px(HUD_GUTTER), Val::Px(HUD_BOTTOM), Val::Px(DECK_WIDTH))
    };
    for mut n in &mut deck {
        if n.left != left || n.right != right || n.bottom != bottom || n.width != width {
            n.left = left;
            n.right = right;
            n.bottom = bottom;
            n.width = width;
        }
    }
    // Compact deck: no title / gauges / keycaps; each tab a tappable plate
    // with the name stacked over the reserve.
    let current = ws.as_deref().map_or(Weapon::Shotgun, |w| w.current);
    for (part, mut n, mut bg, mut border) in &mut parts {
        match *part {
            DeckPart::Title | DeckPart::Ammo | DeckPart::Gauges | DeckPart::Key(_) => {
                let want = if on { Display::None } else { Display::Flex };
                if n.display != want {
                    n.display = want;
                }
            }
            DeckPart::Row(_) => {
                let (dir, align) = if on { (FlexDirection::Column, AlignItems::FlexStart) } else { (FlexDirection::Row, AlignItems::Center) };
                if n.flex_direction != dir || n.align_items != align {
                    n.flex_direction = dir;
                    n.align_items = align;
                }
            }
            DeckPart::Chip(i) => {
                let w = Weapon::ALL[i as usize];
                let (pad, edge, fill, frame) = if on {
                    let (fill, frame) = touch_slot_plate(w, current == w);
                    (UiRect::axes(Val::Px(6.0), Val::Px(5.0)), plate_rect((3.0, 1.0, 1.0, 1.0)), fill, frame)
                } else {
                    (UiRect::horizontal(Val::Px(SLOT_PAD_X)), UiRect::ZERO, Color::NONE, BorderColor::all(Color::NONE))
                };
                if n.padding != pad || n.border != edge {
                    n.padding = pad;
                    n.border = edge;
                }
                if bg.0 != fill {
                    bg.0 = fill;
                }
                if *border != frame {
                    *border = frame;
                }
            }
        }
    }
    for (t, mut font) in &mut fonts {
        let size = match *t {
            HudText::Slot(_) if on => (9.0 / k).round(),
            HudText::Slot(_) => SLOT_LABEL_FONT,
            HudText::SlotAmmo(_) if on => (10.0 / k).round(),
            HudText::SlotAmmo(_) => SLOT_AMMO_FONT,
            _ => continue,
        };
        if font.font_size != bevy::text::FontSize::Px(size) {
            font.font_size = bevy::text::FontSize::Px(size);
        }
    }

    let top = match touch.layout.button(crate::touch::TouchButton::Pause) {
        Some(p) if on => Val::Px(HUD_TOP.max((p.rect().max.y + 8.0) / k)),
        _ => Val::Px(HUD_TOP),
    };
    for mut n in &mut column {
        if n.top != top {
            n.top = top;
        }
    }
    for (part, mut n) in &mut style {
        if *part != StyleHud::Root {
            continue;
        }
        let (left, right, top, align) = if on {
            (Val::Px(HUD_GUTTER), Val::Auto, Val::Px(STYLE_TOUCH_TOP), AlignItems::FlexStart)
        } else {
            (Val::Auto, Val::Px(STYLE_HUD_RIGHT), Val::Px(STYLE_HUD_TOP), AlignItems::FlexEnd)
        };
        if n.left != left || n.right != right || n.top != top || n.align_items != align {
            n.left = left;
            n.right = right;
            n.top = top;
            n.align_items = align;
        }
    }
    let mut rects = [None; 3];
    if on {
        let sf = windows.single().map_or(1.0, |w| w.scale_factor()).max(0.01);
        for (b, node, tf) in &slots {
            if let HudBox::SlotChip(i) = *b
                && let Some(slot) = rects.get_mut(i as usize)
                && node.size().x > 0.0
            {
                *slot = Some(Rect::from_center_size(tf.translation / sf, node.size() / sf));
            }
        }
    }
    if touch.slot_rects != rects {
        touch.slot_rects = rects;
    }
}

fn spawn_hud(mut commands: Commands, store: Option<Res<ProfileStore>>, run: Res<RunStats>, emblem_art: Option<Res<StyleEmblemArt>>) {
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

    // Top-left: floor strip hanging off a glowing rail (+ the custom seed
    // code when one is live). The rail stretches to the strip's height.
    let strip = commands
        .spawn((
            ChildOf(root),
            Node { position_type: PositionType::Absolute, left: Val::Px(HUD_GUTTER), top: Val::Px(HUD_TOP), column_gap: Val::Px(9.0), ..default() },
        ))
        .id();
    commands.spawn((ChildOf(strip), HudBox::FloorRail, accent_bar(Node { width: Val::Px(RAIL_WIDTH), ..default() }, HAZARD_YELLOW)));
    let plate = commands
        .spawn((
            ChildOf(strip),
            Node { flex_direction: FlexDirection::Column, align_items: AlignItems::FlexStart, ..default() },
            children![
                (HudText::Floor, hud_text("FLOOR 1/5", 20.0, COLOR_TEXT)),
                (HudText::Sector, hud_text(sector_label(1), 11.0, HAZARD_YELLOW)),
                (
                    Node { column_gap: Val::Px(9.0), align_items: AlignItems::Center, margin: UiRect::top(Val::Px(2.0)), ..default() },
                    children![
                        (HudText::Stats(0), hud_text("KILLS 0", 13.0, COLOR_TEXT)),
                        (Node { width: Val::Px(4.0), height: Val::Px(4.0), ..default() }, BackgroundColor(HUD_SUB), hard_shadow(1.0)),
                        (HudText::Stats(1), hud_text("JAMMERS 0", 13.0, COLOR_TEXT)),
                    ],
                ),
                (
                    Node { column_gap: Val::Px(7.0), align_items: AlignItems::Center, margin: UiRect::top(Val::Px(1.0)), ..default() },
                    children![
                        (HudText::LoadoutChip, hud_text("STOCK OS", 11.0, HUD_SUB)),
                        (
                            HudBox::LoadoutDot,
                            Node { display: Display::None, width: Val::Px(5.0), height: Val::Px(5.0), ..default() },
                            BackgroundColor(COLOR_SIGNAL_AMBER),
                            hard_shadow(1.0),
                        ),
                        (HudText::LoadoutContracts, hud_text("", 11.0, COLOR_SIGNAL_AMBER)),
                    ],
                ),
            ],
        ))
        .id();
    if run.custom_seed
        && let Some(code) = shareable_code(&run)
    {
        commands.spawn((ChildOf(plate), hud_text(format!("SEED {code}"), 11.0, ARC_CYAN)));
    }

    // Top-center: status line over its glowing underline (+ SOS uplink), boss
    // bar and alert under it.
    let top = commands
        .spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                top: Val::Px(HUD_TOP),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .id();
    // During the SOS uplink (DEMON RUSH) two alert hairlines clamp the status
    // line and the uplink gauge.
    let rush = commands.spawn((ChildOf(top), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(6.0), ..default() })).id();
    let rush_rail = || (HudBox::RushRail, accent_bar(Node { display: Display::None, width: Val::Px(UPLINK_WIDTH + 36.0), height: Val::Px(2.0), ..default() }, PUNK_RED));
    commands.spawn((ChildOf(rush), rush_rail()));
    // The underline overhangs the text by 10 px a side and follows its width.
    let status = commands.spawn((ChildOf(rush), Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(3.0), ..default() })).id();
    commands.spawn((ChildOf(status), HudText::Status, hud_text("SEARCH FOR 5-BAR SIGNAL TOWER", 20.0, COLOR_SIGNAL_AMBER)));
    commands.spawn((
        ChildOf(status),
        HudBox::StatusRule,
        accent_bar(Node { height: Val::Px(2.0), margin: UiRect::horizontal(Val::Px(-10.0)), ..default() }, COLOR_SIGNAL_AMBER),
    ));
    let uplink = commands
        .spawn((
            ChildOf(rush),
            HudBox::UplinkPanel,
            Node { display: Display::None, flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(4.0), ..default() },
        ))
        .id();
    commands.spawn((ChildOf(uplink), HudText::UplinkLabel, hud_text("SOS OVERDRIVE  1.25x  0%", 17.0, ACID_GREEN)));
    let gauge = commands
        .spawn((ChildOf(uplink), Node { width: Val::Px(UPLINK_WIDTH), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }))
        .id();
    let track = commands.spawn((ChildOf(gauge), bar_track(Val::Percent(100.0), 14.0, ARC_CYAN, Display::Flex))).id();
    commands.spawn((ChildOf(track), HudBox::UplinkFill, Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(COLOR_CYAN)));
    spawn_ticks(&mut commands, track);
    spawn_ruler(&mut commands, gauge);
    commands.spawn((ChildOf(rush), rush_rail()));
    // Boss bar: under the status / uplink block, above the alert.
    let boss = commands
        .spawn((ChildOf(top), HudBox::BossPlate, Node { display: Display::None, flex_direction: FlexDirection::Column, row_gap: Val::Px(3.0), ..default() }))
        .id();
    commands.spawn((
        ChildOf(boss),
        Node { width: Val::Px(BOSS_BAR_WIDTH), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() },
        children![
            (HudText::BossName, hud_text(crate::enemies::EnemyKind::CarrierDaemon.name(), 17.0, HAZARD_YELLOW)),
            (HudText::BossPhase, hud_text("PHASE I", 13.0, PUNK_RED)),
        ],
    ));
    let bgauge = commands
        .spawn((ChildOf(boss), Node { width: Val::Px(BOSS_BAR_WIDTH), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }))
        .id();
    let btrack = commands.spawn((ChildOf(bgauge), HudBox::BossTrack, bar_track(Val::Percent(100.0), 13.0, PUNK_RED, Display::Flex))).id();
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
            BackgroundColor(if major { HAZARD_YELLOW } else { HUD_SUB.with_alpha(0.75) }),
            hard_shadow(1.0),
        ));
    }
    commands.spawn((
        ChildOf(top),
        HudBox::AlertPanel,
        Node { display: Display::None, ..default() },
        children![(HudText::Alert, hud_text("", 28.0, COLOR_SIGNAL_AMBER))],
    ));

    // Bottom-center prompt (above the vitals cluster).
    commands.spawn((
        ChildOf(root),
        Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), bottom: Val::Px(PROMPT_BOTTOM), justify_content: JustifyContent::Center, ..default() },
        children![(HudText::Prompt, hud_text("", 22.0, COLOR_SIGNAL_GREEN))],
    ));

    // Top-right column: the split timer in the corner, the killfeed callouts
    // flowing under it (or from the corner while the timer is hidden).
    let column = commands
        .spawn((
            ChildOf(root),
            TouchTopRightColumn,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(HUD_GUTTER),
                top: Val::Px(HUD_TOP),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(RIGHT_COLUMN_GAP),
                ..default()
            },
        ))
        .id();
    spawn_split_plate(&mut commands, column);
    let feed = commands
        .spawn((ChildOf(column), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::FlexEnd, row_gap: Val::Px(TOAST_GAP), ..default() }))
        .id();
    for i in 0..TOAST_SLOTS {
        // A frameless callout: shadowed text ending on a glowing pip.
        commands.spawn((
            ChildOf(feed),
            HudBox::ToastChip(i),
            Node { display: Display::None, column_gap: Val::Px(7.0), ..default() },
            children![
                (HudText::Toast(i), hud_text("", TOAST_FONT, COLOR_TEXT)),
                (HudBox::ToastPip(i), accent_bar(Node { width: Val::Px(RAIL_WIDTH), ..default() }, Color::NONE)),
            ],
        ));
    }

    // Right edge, below that column and above the weapon deck: the Milestone
    // 31 Bandwidth Style Rank emblem.
    spawn_style_meter(&mut commands, root, emblem_art.as_deref());

    // Bottom-center: vitals cluster (VEL row, HP, ARMOR | STAMINA).
    commands.insert_resource(VelGauge::default());
    commands.insert_resource(HudPulse::default());
    let vitals = commands
        .spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                bottom: Val::Px(HUD_BOTTOM),
                width: Val::Px(VITALS_WIDTH),
                margin: UiRect::left(Val::Px(-VITALS_WIDTH / 2.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(5.0),
                ..default()
            },
        ))
        .id();
    spawn_vel_plate(&mut commands, vitals);
    let wide = Node { width: Val::Percent(100.0), ..default() };
    let hp = spawn_bar(&mut commands, vitals, wide, "HP", COLOR_TEXT, (11.0, 14.0), HudText::HpValue, HudBox::HpFill, 12.0, COLOR_HP_GREEN);
    commands.entity(hp).insert((HudBox::HpTrack, glow(Color::NONE)));
    // Armor and stamina share one row: two half-width gauges.
    let pair = commands.spawn((ChildOf(vitals), Node { width: Val::Percent(100.0), column_gap: Val::Px(10.0), ..default() })).id();
    let half = || Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), min_width: Val::Px(0.0), ..default() };
    spawn_bar(&mut commands, pair, half(), "ARMOR", COLOR_ARMOR, (10.0, 12.0), HudText::ArmorValue, HudBox::ArmorFill, 7.0, COLOR_ARMOR);
    spawn_bar(&mut commands, pair, half(), STAMINA_LABEL_KBM, HUD_SUB, (10.0, 12.0), HudText::StaminaValue, HudBox::StaminaFill, 7.0, COLOR_GOLD);

    // Bottom-right: weapon & ammo deck, right-aligned on the gutter.
    let deck = commands
        .spawn((
            ChildOf(root),
            TouchDeck,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(HUD_GUTTER),
                bottom: Val::Px(HUD_BOTTOM),
                width: Val::Px(DECK_WIDTH),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(4.0),
                ..default()
            },
        ))
        .id();
    commands.spawn((
        ChildOf(deck),
        DeckPart::Title,
        Node { align_items: AlignItems::Center, column_gap: Val::Px(8.0), min_height: Val::Px(18.0), ..default() },
        children![
            // Buff pills dock left of the title as one cluster, so one or both
            // lit never shifts it off the gutter.
            (
                Node { column_gap: Val::Px(PILL_GAP), align_items: AlignItems::Center, flex_shrink: 0.0, ..default() },
                children![buff_pill(HudBox::OverclockPill, OVERCLOCK_PILL, ACID_GREEN), buff_pill(HudBox::DopplerPill, DOPPLER_PILL, COLOR_ROCKET)],
            ),
            (HudText::WeaponName, hud_text("SHOTGUN", WEAPON_TITLE_FONT, COLOR_SHOTGUN)),
        ],
    ));
    commands.spawn((
        ChildOf(deck),
        DeckPart::Ammo,
        Node { align_items: AlignItems::Baseline, column_gap: Val::Px(6.0), ..default() },
        children![
            (HudText::AmmoCount, hud_text("0", AMMO_FONT, COLOR_TEXT), LineHeight::RelativeToFont(1.0)),
            (HudText::AmmoMax, hud_text("/ 0", 15.0, HUD_SUB)),
        ],
    ));
    // The three ammo gauges share one container (the touch deck hides it).
    let gauges = commands
        .spawn((ChildOf(deck), DeckPart::Gauges, Node { flex_direction: FlexDirection::Column, align_items: AlignItems::FlexEnd, ..default() }))
        .id();
    let pips = commands
        .spawn((ChildOf(gauges), HudBox::ShellRow, Node { column_gap: Val::Px(PIP_GAP), height: Val::Px(PIP_SIZE.1), ..default() }))
        .id();
    for i in 0..SHELL_PIPS {
        commands.spawn((
            ChildOf(pips),
            HudBox::ShellPip(i),
            Node { width: Val::Px(PIP_SIZE.0), height: Val::Px(PIP_SIZE.1), border: UiRect { top: Val::Px(3.0), ..UiRect::all(Val::Px(1.0)) }, ..default() },
            shell_pip_border(true),
            BackgroundColor(COLOR_SHELL),
        ));
    }
    let ptrack = commands
        .spawn((ChildOf(gauges), HudBox::PlasmaTrack, bar_track(Val::Px(PIP_ROW_WIDTH), PIP_SIZE.1, COLOR_CYAN, Display::None)))
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
        .spawn((ChildOf(gauges), HudBox::RocketRow, Node { display: Display::None, column_gap: Val::Px(PIP_GAP), height: Val::Px(PIP_SIZE.1), ..default() }))
        .id();
    for i in 0..ROCKET_PIPS {
        commands.spawn((
            ChildOf(rockets),
            HudBox::RocketPip(i),
            Node { width: Val::Px(PIP_SIZE.0), height: Val::Px(PIP_SIZE.1), border: UiRect { left: Val::Px(5.0), ..UiRect::all(Val::Px(1.0)) }, ..default() },
            rocket_pip_border(true),
            BackgroundColor(COLOR_WARHEAD),
        ));
    }
    // Slot deck: three identical frameless two-row tabs. Row 1 is
    // `[keycap] NAME ...... reserve` on one never-wrapping line, row 2 a thin
    // reserve gauge, so every tab keeps the same height and alignment
    // whatever the ammo digits are.
    let slots = commands
        .spawn((ChildOf(deck), Node { width: Val::Percent(100.0), column_gap: Val::Px(SLOT_GAP), margin: UiRect::top(Val::Px(2.0)), ..default() }))
        .id();
    for i in 0..Weapon::ALL.len() as u8 {
        let chip = commands
            .spawn((
                ChildOf(slots),
                HudBox::SlotChip(i),
                DeckPart::Chip(i),
                Node {
                    flex_grow: 1.0,
                    flex_basis: Val::Px(0.0),
                    min_width: Val::Px(0.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(3.0),
                    padding: UiRect::horizontal(Val::Px(SLOT_PAD_X)),
                    ..default()
                },
            ))
            .id();
        commands.spawn((
            ChildOf(chip),
            DeckPart::Row(i),
            Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, column_gap: Val::Px(SLOT_INNER_GAP), ..default() },
            children![
                (
                    Node { align_items: AlignItems::Center, column_gap: Val::Px(SLOT_INNER_GAP), ..default() },
                    children![
                        (
                            HudBox::SlotKey(i),
                            DeckPart::Key(i),
                            Node {
                                width: Val::Px(SLOT_KEY_WIDTH),
                                height: Val::Px(13.0),
                                justify_content: JustifyContent::Center,
                                align_items: AlignItems::Center,
                                border: UiRect::bottom(Val::Px(2.0)),
                                flex_shrink: 0.0,
                                ..default()
                            },
                            BorderColor::all(Color::BLACK),
                            BackgroundColor(COLOR_PIP_OFF),
                            children![(HudText::SlotKey(i), hud_text((i + 1).to_string(), SLOT_LABEL_FONT, HUD_SUB))],
                        ),
                        (HudText::Slot(i), hud_text("", SLOT_LABEL_FONT, HUD_SUB)),
                    ],
                ),
                (HudText::SlotAmmo(i), hud_text("", SLOT_AMMO_FONT, HUD_SUB)),
            ],
        ));
        commands.spawn((
            ChildOf(chip),
            Node { width: Val::Percent(100.0), height: Val::Px(4.0), border: UiRect::all(Val::Px(1.0)), ..default() },
            BorderColor::all(TRACK_EDGE),
            BackgroundColor(TRACK_BG),
            children![(HudBox::SlotFill(i), Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(COLOR_PIP_OFF))],
        ));
    }
}

/// Shell pip frame: a brass rim on top while loaded, ink all round.
fn shell_pip_border(lit: bool) -> BorderColor {
    BorderColor { top: if lit { COLOR_BRASS } else { COLOR_PIP_OFF_BORDER }, ..BorderColor::all(COLOR_PIP_OFF_BORDER) }
}

/// Warhead pip frame: a glowing amber nose while loaded, ink all round.
fn rocket_pip_border(lit: bool) -> BorderColor {
    BorderColor { left: if lit { COLOR_WARHEAD_TIP } else { COLOR_PIP_OFF_BORDER }, ..BorderColor::all(COLOR_PIP_OFF_BORDER) }
}

/// Split timer fields.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitText {
    Tag,
    Time,
    Progress,
    Delta,
    /// Milestone 31 live race delta vs the PB Signal Ghost (Time Trial).
    Ghost,
}

/// Root of the split timer cluster (hidden unless splits are on / non-standard mode).
#[derive(Component)]
pub struct SplitPlate;

/// Glowing accent rule under the timer numerals.
#[derive(Component)]
pub struct SplitRule;

/// Top of the split timer: the top-right corner (the callouts flow under it).
pub const SPLIT_PLATE_TOP: f32 = HUD_TOP;
/// Widest the timer cluster ever gets (its longest line, `FLOOR 5/5  PB 00:00.0`).
pub const SPLIT_PLATE_WIDTH: f32 = 150.0;
/// Laid-out height budget of the timer cluster with its ghost line showing.
pub const SPLIT_PLATE_HEIGHT: f32 = 98.0;
/// Timer numerals font (monospace, so the digits never jitter).
pub const SPLIT_TIME_FONT: f32 = 28.0;
/// Width of the accent rule: the 7 numerals of `00:00.0`.
const SPLIT_RULE_WIDTH: f32 = 118.0;
/// Killfeed callout font, the gap between two and the gap under the timer.
pub const TOAST_FONT: f32 = 13.0;
const TOAST_GAP: f32 = 3.0;
const RIGHT_COLUMN_GAP: f32 = 10.0;
/// Lowest the top-right column ever reaches: timer + a full callout stack.
pub const RIGHT_COLUMN_BOTTOM: f32 = SPLIT_PLATE_TOP + SPLIT_PLATE_HEIGHT + RIGHT_COLUMN_GAP + TOAST_SLOTS as f32 * (TOAST_FONT * 1.2 + TOAST_GAP);

fn spawn_split_plate(commands: &mut Commands, column: Entity) {
    // No plate: a tiny header, shadowed numerals on a glowing rule, tight
    // right-aligned lines under it.
    commands.spawn((
        ChildOf(column),
        SplitPlate,
        Node { display: Display::None, flex_direction: FlexDirection::Column, align_items: AlignItems::FlexEnd, ..default() },
        children![
            (SplitText::Tag, hud_text("SPLITS", 10.0, ARC_CYAN)),
            (SplitText::Time, hud_text("00:00.0", SPLIT_TIME_FONT, COLOR_TEXT), LineHeight::RelativeToFont(1.05)),
            (
                SplitRule,
                accent_bar(
                    Node { width: Val::Px(SPLIT_RULE_WIDTH), height: Val::Px(2.0), margin: UiRect { top: Val::Px(2.0), bottom: Val::Px(4.0), ..default() }, ..default() },
                    ARC_CYAN,
                ),
            ),
            (SplitText::Progress, hud_text("", 11.0, HUD_SUB)),
            (SplitText::Delta, hud_text("", 13.0, COLOR_TEXT)),
            (SplitText::Ghost, Node { display: Display::None, ..default() }, hud_text("", 13.0, ACID_GREEN)),
        ],
    ));
}

// ---------------------------------------------------------------------------
// Milestone 31: Bandwidth Style Rank meter (floating DMC-style emblem)
// ---------------------------------------------------------------------------

/// Top of the style emblem: clears the top-right column (split timer + a full
/// callout stack, [`RIGHT_COLUMN_BOTTOM`]) at 720p, even at the peak of a
/// rank-up slam.
pub const STYLE_HUD_TOP: f32 = 330.0;
/// Right gutter of the floating widget.
pub const STYLE_HUD_RIGHT: f32 = 16.0;
/// Side of the square rank emblem (logical px; the texture is 2x supersampled).
pub const STYLE_EMBLEM_SIZE: f32 = 96.0;
/// Tag + subtitle caption font.
pub const STYLE_CAPTION_FONT: f32 = 11.0;
/// Action callout font.
pub const STYLE_FEED_FONT: f32 = 10.0;
/// Action callouts shown under the caption (the newest of the meter's feed).
pub const STYLE_FEED_SHOWN: usize = 2;
/// Widest the widget ever gets: the longest caption (`6G+ // SMOKING SEXY SIGNAL!!`).
pub const STYLE_HUD_WIDTH: f32 = 190.0;
/// Laid-out height budget: emblem, caption row, [`STYLE_FEED_SHOWN`] callouts.
pub const STYLE_HUD_HEIGHT: f32 = STYLE_EMBLEM_SIZE + 2.0 + 15.0 + STYLE_FEED_SHOWN as f32 * 13.0;
/// Seconds between emblem texture refreshes (the pose animates every frame;
/// envelope triggers and rank changes refresh immediately).
pub const STYLE_EMBLEM_REFRESH: f32 = 1.0 / 30.0;

// Layout contract, checked at compile time: a compact floating emblem far
// smaller than the old 236 x 164 plate, small captions, two callouts, and a
// 2x supersampled texture that covers the emblem at every UI scale.
const _: () = {
    assert!(STYLE_EMBLEM_SIZE >= 92.0 && STYLE_EMBLEM_SIZE <= 100.0);
    assert!(STYLE_HUD_WIDTH < 236.0 && STYLE_HUD_HEIGHT < 144.0);
    assert!(STYLE_FEED_SHOWN == 2 && STYLE_FEED_SHOWN <= crate::style::FEED_LINES);
    assert!(STYLE_CAPTION_FONT >= 10.5 && STYLE_CAPTION_FONT <= 11.0 && STYLE_FEED_FONT >= 9.5 && STYLE_FEED_FONT <= 10.0);
    assert!(EMBLEM_PX as f32 == STYLE_EMBLEM_SIZE * 2.0);
    // Frameless right edge: timer + callouts, then the emblem, then the deck.
    assert!(RIGHT_COLUMN_BOTTOM + 40.0 <= STYLE_HUD_TOP);
    assert!(STYLE_HUD_TOP + STYLE_HUD_HEIGHT + 40.0 <= UI_REFERENCE.1 - HUD_BOTTOM - DECK_HEIGHT);
    assert!(SPLIT_PLATE_WIDTH <= DECK_WIDTH && PIP_ROW_WIDTH <= DECK_WIDTH);
    // The frameless bottom HUD is far shorter than the old plates (120 px
    // vitals plate + 35 px VEL strip, ~172 px weapon deck), and the prompt
    // clears the vitals even with the BHOP stamp up.
    assert!(VITALS_HEIGHT <= 90.0 && DECK_HEIGHT <= 110.0);
    assert!(PROMPT_BOTTOM >= HUD_BOTTOM + VITALS_HEIGHT + BHOP_TAG_HEIGHT);
};

/// Style meter widget parts.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleHud {
    Root,
    /// The procedural rank emblem image (see [`StyleEmblem`]).
    Emblem,
    Tag,
    Subtitle,
    Feed(u8),
}

/// State of the emblem image node: the rank it last drew and how many times
/// its texture was refreshed in place.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct StyleEmblem {
    pub rank: Option<crate::style::StyleRank>,
    pub redraws: u32,
    /// Clock of the last refresh and the envelopes it saw (trigger detection).
    last: f32,
    seen: [f32; 3],
}

/// The pre-baked emblem lettering and the one texture it animates into.
#[derive(Resource)]
pub struct StyleEmblemArt {
    pub atlas: EmblemAtlas,
    pub image: Handle<Image>,
}

/// Bake the rank lettering once and allocate the single emblem texture.
fn init_style_emblem(mut commands: Commands, images: Option<ResMut<Assets<Image>>>) {
    let Some(mut images) = images else { return };
    let mut img = Image::new(
        Extent3d { width: EMBLEM_PX as u32, height: EMBLEM_PX as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        vec![0; EMBLEM_BYTES],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    img.sampler = ImageSampler::linear();
    commands.insert_resource(StyleEmblemArt { atlas: EmblemAtlas::bake(), image: images.add(img) });
}

fn style_caption_shadow() -> TextShadow {
    TextShadow { offset: Vec2::splat(1.4), color: Color::BLACK.with_alpha(crate::hud_fx::SHADOW_ALPHA) }
}

fn spawn_style_meter(commands: &mut Commands, root: Entity, art: Option<&StyleEmblemArt>) {
    // No plate, no border: the emblem and its captions float over the scene on
    // ink outlines and hard text shadows alone.
    let meter = commands
        .spawn((
            ChildOf(root),
            StyleHud::Root,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                right: Val::Px(STYLE_HUD_RIGHT),
                top: Val::Px(STYLE_HUD_TOP),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(1.0),
                ..default()
            },
        ))
        .id();
    if let Some(art) = art {
        commands.spawn((
            ChildOf(meter),
            StyleHud::Emblem,
            StyleEmblem::default(),
            Node { width: Val::Px(STYLE_EMBLEM_SIZE), height: Val::Px(STYLE_EMBLEM_SIZE), flex_shrink: 0.0, margin: UiRect::bottom(Val::Px(1.0)), ..default() },
            ImageNode::new(art.image.clone()),
            UiTransform::IDENTITY,
        ));
    }
    let caption = commands.spawn((ChildOf(meter), Node { align_items: AlignItems::Center, column_gap: Val::Px(5.0), ..default() })).id();
    commands.spawn((ChildOf(caption), StyleHud::Tag, text("EDGE", STYLE_CAPTION_FONT, COLOR_TEXT), style_caption_shadow(), TextLayout::no_wrap()));
    commands.spawn((ChildOf(caption), text("//", STYLE_CAPTION_FONT, COLOR_DIM), style_caption_shadow(), TextLayout::no_wrap()));
    commands.spawn((ChildOf(caption), StyleHud::Subtitle, text("DEAD SLOW", STYLE_CAPTION_FONT, COLOR_TEXT), style_caption_shadow(), TextLayout::no_wrap()));
    for i in 0..STYLE_FEED_SHOWN as u8 {
        commands.spawn((ChildOf(meter), StyleHud::Feed(i), text("", STYLE_FEED_FONT, COLOR_TEXT), TextLayout::no_wrap(), style_caption_shadow()));
    }
}

/// Whether the style meter shows: a live non-trial run with points banked.
pub fn style_hud_visible(points: f32, mode: RunMode) -> bool {
    points > 0.0 && mode != RunMode::TimeTrial
}

/// Rank-up slam scale: lands huge and overshoots (`1.55x -> 0.93x -> 1.0x`).
pub fn style_pop_scale(pop: f32) -> f32 {
    slam_curve(pop, 1.55, 0.93, 1.0)
}

/// Rank-up rotational whip, clockwise degrees (`-14 -> +3 -> 0`).
pub fn style_pop_tilt(pop: f32) -> f32 {
    slam_curve(pop, -14.0, 3.0, 0.0)
}

/// Where the emblem sits this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleEmblemPose {
    pub scale: f32,
    /// Clockwise tilt (degrees).
    pub tilt: f32,
    /// Offset from the laid-out slot (logical px).
    pub offset: Vec2,
}

/// Emblem pose: rank-up slam + whip, action micro-bounce, take-hit tremor and
/// tilt kick, a slow living sway, and an electric micro-tremor at `SS` / `SSS`.
pub fn style_emblem_pose(rank: Option<crate::style::StyleRank>, pop: f32, action_pop: f32, hit_shake: f32, t: f32) -> StyleEmblemPose {
    use crate::style::StyleRank;
    let env = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let (act, hit) = (env(action_pop), env(hit_shake));
    let t = if t.is_finite() { t } else { 0.0 };
    // Action pulse: a crisp kick that rings once.
    let bounce = 1.0 + 0.10 * act * ((1.0 - act) * 3.8).cos();
    let scale = style_pop_scale(pop) * bounce;
    let tremor = match rank {
        Some(StyleRank::SSS) => 1.1,
        Some(StyleRank::SS) => 0.65,
        _ => 0.0,
    };
    let tilt = style_pop_tilt(pop) + (t * 1.3).sin() * 0.8 + hit * hit * 9.0 * (t * 47.0).cos();
    let mut offset = Vec2::new((t * 61.0).sin() * (t * 37.0 + 1.3).sin(), (t * 53.0 + 2.1).sin() * (t * 29.0).sin()) * tremor;
    offset.y += (t * 1.9).sin() * 0.6;
    offset.x += (t * 83.0).sin() * 7.0 * hit;
    // Grow away from the screen edge (the right side stays on its gutter) and mostly downward.
    offset += Vec2::new(-0.5, 0.25) * (scale - 1.0).max(0.0) * STYLE_EMBLEM_SIZE;
    StyleEmblemPose { scale, tilt, offset }
}

/// Opacity of the action callout in `slot` at `age` seconds (newest brightest).
pub fn style_feed_alpha(slot: usize, age: f32) -> f32 {
    (1.0 - (age / crate::style::FEED_SECS).powi(3)).clamp(0.0, 1.0) * (1.0 - slot as f32 * 0.28)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_style_hud(
    time: Res<Time>,
    meter: Res<crate::style::StyleMeter>,
    run: Res<RunStats>,
    art: Option<Res<StyleEmblemArt>>,
    images: Option<ResMut<Assets<Image>>>,
    mut roots: Query<(&StyleHud, &mut Node), Without<StyleEmblem>>,
    mut emblems: Query<(&mut StyleEmblem, &mut UiTransform)>,
    mut texts: Query<(&StyleHud, &mut Text, &mut TextColor)>,
) {
    let show = style_hud_visible(meter.points, run.mode);
    for (part, mut node) in &mut roots {
        if *part == StyleHud::Root {
            set_display(&mut node, show);
        }
    }
    let Some(r) = meter.rank().filter(|_| show) else {
        for (mut emblem, _) in &mut emblems {
            // Redraw from scratch the next time the meter comes back.
            if emblem.rank.is_some() {
                emblem.rank = None;
            }
        }
        return;
    };
    let t = time.elapsed_secs_wrapped();
    let color = r.color();
    if let Ok((mut emblem, mut ui)) = emblems.single_mut() {
        let pose = style_emblem_pose(Some(r), meter.pop, meter.action_pop, meter.hit_shake, t);
        let want = UiTransform { translation: Val2::px(pose.offset.x, pose.offset.y), scale: Vec2::splat(pose.scale), rotation: Rot2::degrees(pose.tilt) };
        if *ui != want {
            *ui = want;
        }
        // Refresh the texture in place at ~30 Hz, at once on a rank change or a fresh envelope.
        let now = [meter.pop, meter.action_pop, meter.hit_shake];
        let triggered = emblem.rank != Some(r) || now.iter().zip(emblem.seen).any(|(n, s)| *n > s + 0.02);
        if (triggered || (t - emblem.last).abs() >= STYLE_EMBLEM_REFRESH)
            && let (Some(art), Some(mut images)) = (art, images)
            && let Some(mut img) = images.get_mut(&art.image)
            && let Some(data) = img.data.as_mut()
        {
            let c = color.to_srgba();
            let frame = EmblemFrame { rank: r.index() as usize, color: [c.red, c.green, c.blue], fill: meter.tier_fill(), time: t, pop: meter.pop, action_pop: meter.action_pop, hit_shake: meter.hit_shake };
            art.atlas.render(&frame, data);
            *emblem = StyleEmblem { rank: Some(r), redraws: emblem.redraws.wrapping_add(1), last: t, seen: now };
        } else if emblem.seen != now {
            // Track the decaying envelopes so the next trigger is seen.
            let seen = [now[0].min(emblem.seen[0]), now[1].min(emblem.seen[1]), now[2].min(emblem.seen[2])];
            if emblem.seen != seen {
                emblem.seen = seen;
            }
        }
    }
    // A demotion stains the caption crimson for a beat.
    let tag_color = color.mix(&COLOR_SIGNAL_RED, meter.hit_shake.clamp(0.0, 1.0));
    for (part, mut txt, mut tc) in &mut texts {
        let (want, c): (&str, Color) = match part {
            StyleHud::Tag => (r.tag(), tag_color),
            StyleHud::Subtitle => (r.subtitle(), BONE_TEXT),
            StyleHud::Feed(i) => match meter.feed.get(*i as usize).copied().flatten() {
                Some((label, age)) => (label, COLOR_TEXT.with_alpha(style_feed_alpha(*i as usize, age))),
                None => ("", COLOR_TEXT),
            },
            _ => continue,
        };
        if txt.0 != want {
            want.clone_into(&mut txt.0);
        }
        if tc.0 != c {
            tc.0 = c;
        }
    }
}

/// Live split timer: `RunStats::time_elapsed` / `TimeTrialState::elapsed`
/// only advance in `Playing`, so the timer freezes with the simulation. A
/// banked split pops the numerals and snaps the rule out from the gutter in
/// the split's tone; the ghost line pops when the PB delta changes sign.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_split_plate(
    run: Res<RunStats>,
    trial: Res<TimeTrialState>,
    store: Option<Res<ProfileStore>>,
    ghost: Option<Res<crate::trials::TrialGhostStore>>,
    pulse: Res<HudPulse>,
    mut plates: Query<&mut Node, (With<SplitPlate>, Without<SplitText>)>,
    mut rules: Query<(&mut BackgroundColor, &mut BoxShadow, &mut UiTransform), (With<SplitRule>, Without<SplitText>)>,
    mut texts: Query<(&SplitText, &mut Text, &mut TextColor, &mut Node, &mut UiTransform)>,
) {
    let settings = live_settings(store.as_deref());
    let show = split_timer_visible(&settings, run.mode);
    for mut node in &mut plates {
        set_display(&mut node, show);
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
    let mut v = split_plate_view(&run, &trial, profile);
    // The menus' muted ash is too dark to float without a plate.
    if v.delta_color == ASH_DIM {
        v.delta_color = HUD_SUB;
    }
    let ghost_line = if run.mode == RunMode::TimeTrial { crate::trials::ghost_delta_text(ghost.as_deref().and_then(|g| g.delta)) } else { None };
    let (ghost_text, ghost_color) = ghost_line.unwrap_or_default();
    let (split, flip) = (pulse.split, pulse.ghost);
    let right = Vec2::new(1.0, 0.0);
    let rail = match run.mode {
        RunMode::Standard => ACID_GREEN,
        RunMode::Daily(_) => HAZARD_YELLOW,
        RunMode::TimeTrial => ARC_CYAN,
    };
    for (mut bg, mut shadow, mut ui) in &mut rules {
        let c = flash(rail.mix(&v.delta_color, split), split);
        set_bg(&mut bg, c);
        set_glow(&mut shadow, c);
        set_ui(&mut ui, pop_transform(Vec2::new(slam_curve(split, 0.2, 1.12, 1.0), 1.0), right));
    }
    for (field, mut t, mut c, mut node, mut ui) in &mut texts {
        let (want, color, scale) = match field {
            SplitText::Tag => (&v.tag, v.tag_color, 1.0),
            SplitText::Time => (&v.time, COLOR_TEXT.mix(&v.delta_color, split), pop_scale(split, 0.14)),
            SplitText::Progress => (&v.progress, HUD_SUB, 1.0),
            SplitText::Delta => (&v.delta, flash(v.delta_color, split), pop_scale(split, 0.2)),
            SplitText::Ghost => (&ghost_text, flash(ghost_color, flip), pop_scale(flip, 0.22)),
        };
        if t.0 != *want {
            t.0.clone_from(want);
        }
        if c.0 != color {
            c.0 = color;
        }
        set_ui(&mut ui, pop_transform(Vec2::splat(scale), right));
        if *field == SplitText::Ghost {
            set_display(&mut node, !want.is_empty());
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
    mut texts: Query<(&HudText, &mut Text, &mut TextColor, &mut UiTransform, Option<&mut TextShadow>)>,
    (mut buf, mut tags, mode, pulse): (Local<String>, Local<LoadoutTags>, Option<Res<InputMode>>, Res<HudPulse>),
) {
    let mode = mode.map(|m| *m).unwrap_or_default();
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
    let is_trial = run.mode == RunMode::TimeTrial;
    for (slot, mut txt, mut col, mut ui, shadow) in &mut texts {
        // Every label is written into one reused buffer; `Text` / `TextColor`
        // are only touched (marked changed) when the value really differs.
        let b = &mut *buf;
        b.clear();
        // Micro-animations ride on `UiTransform` (no relayout).
        let mut pose = UiTransform::IDENTITY;
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
            HudText::Sector if is_trial => {
                b.push_str("COURSE // SIGNAL RUN");
                HAZARD_YELLOW
            }
            HudText::Sector => {
                let _ = write!(b, "SECTOR {:02} // {}", run.floor, FloorTheme::for_floor(run.floor).name());
                HAZARD_YELLOW
            }
            HudText::Stats(i) => {
                let i = (i as usize).min(1);
                let _ = match (is_trial, i) {
                    (true, 0) => write!(b, "CP {}/{}", trial.next_gate.min(crate::trials::TRIAL_CHECKPOINTS), crate::trials::TRIAL_CHECKPOINTS),
                    (true, _) => write!(b, "RESCUES {}", trial.rescues),
                    (false, 0) => write!(b, "KILLS {}", run.kills),
                    (false, _) => write!(b, "JAMMERS {}", run.jammers_destroyed),
                };
                // Only the counter that went up pops: white-hot, red for a pit rescue.
                let e = pulse.stats[i];
                pose = pop_transform(Vec2::splat(pop_scale(e, 0.2)), Vec2::new(-1.0, 0.0));
                COLOR_TEXT.mix(if is_trial && i == 1 { &COLOR_SIGNAL_RED } else { &Color::WHITE }, e)
            }
            HudText::LoadoutChip => {
                b.push_str(chip_tag);
                if loadout.chip == crate::meta::StarterChip::StockUac { HUD_SUB } else { COLOR_CYAN }
            }
            HudText::LoadoutContracts => {
                b.push_str(contract_tags.as_deref().unwrap_or_default());
                contract_color
            }
            HudText::Status => {
                let (_, line, c) = status_line(&run, &trial, &ext, &signal);
                b.push_str(line);
                // A new line lands with a small pop and a white-hot flash.
                pose = pop_transform(Vec2::splat(pop_scale(pulse.status, 0.12)), Vec2::ZERO);
                flash(c, pulse.status)
            }
            HudText::Prompt => match *ext {
                _ if is_trial => Color::NONE,
                ExtractionState::Searching { hold } if signal.near_tower => {
                    let a = 0.6 + 0.4 * (t * 5.0).sin();
                    b.push_str(sos_prompt(mode));
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
                ExtractionState::Uploading { boss_hold: true, .. } => {
                    b.push_str(crate::boss::BOSS_HOLD_TEXT);
                    COLOR_SIGNAL_AMBER.with_alpha(if (t * 6.0).sin() > 0.0 { 1.0 } else { 0.55 })
                }
                ExtractionState::Uploading { progress, jammed: true, .. } => {
                    let _ = write!(b, "UPLINK JAMMED! KILL THE JAMMER FIEND  {:.0}%", progress * 100.0);
                    COLOR_SIGNAL_RED.with_alpha(if (t * 10.0).sin() > 0.0 { 1.0 } else { 0.4 })
                }
                ExtractionState::Uploading { progress, rate, .. } => {
                    let tier = write_uplink_speed(b, progress, rate);
                    let c = uplink_tier_color(tier);
                    // OVERDRIVE shimmers; the other tiers hold steady.
                    if tier == UplinkTier::Overdrive { c.with_alpha(0.85 + 0.15 * (t * 8.0).sin()) } else { c }
                }
                _ => COLOR_CYAN,
            },
            HudText::HpValue => {
                let _ = write!(b, "{}/{}", p.hp.ceil().max(0.0) as i32, p.max_hp as i32);
                // A hit kicks the readout red; critical HP keeps it throbbing.
                let hit = hp_hit_envelope(p.since_damage);
                let critical = hp_band(fraction(p.hp, p.max_hp)) == HpBand::Critical;
                let throb = if critical { 1.0 + 0.07 * danger_pulse(t) } else { 1.0 };
                pose = pop_transform(Vec2::splat(pop_scale(hit, 0.2) * throb), Vec2::new(1.0, 1.0));
                if critical { hp_color(0.0, t) } else { COLOR_TEXT.mix(&COLOR_SIGNAL_RED, hit) }
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
                // A swap slots the new title in toward the gutter.
                let e = pulse.swap;
                if e > 0.0 {
                    pose = UiTransform { translation: Val2::px(-14.0 * e * e, 0.0), ..UiTransform::IDENTITY };
                }
                flash(accent, e)
            }
            HudText::AmmoCount => {
                let _ = write!(b, "{cur}");
                // Every round kicks the count; a swap lands it harder; a dry
                // magazine throbs ember red.
                let kick = pop_scale(pulse.ammo, 0.1) * pop_scale(pulse.swap, 0.18);
                let dry = if cur == 0 { danger_pulse(t) } else { 0.0 };
                pose = pop_transform(Vec2::splat(kick * (1.0 + 0.06 * dry)), Vec2::ONE);
                if cur == 0 { COLOR_EMBER.mix(&COLOR_SIGNAL_RED, dry) } else { COLOR_TEXT.mix(&accent, 0.7 * pulse.ammo) }
            }
            HudText::AmmoMax => {
                let _ = write!(b, "/ {max}");
                HUD_SUB
            }
            HudText::Slot(i) => {
                let w = Weapon::ALL[i as usize];
                b.push_str(weapon_slot_name(w));
                if ws.current == w { weapon_accent(w) } else { HUD_SUB }
            }
            HudText::SlotKey(i) => {
                let cur = Weapon::ALL.iter().position(|w| *w == ws.current).unwrap_or(0);
                b.push(slot_key_label(mode, i as usize, cur, Weapon::ALL.len()));
                if ws.current == Weapon::ALL[i as usize] { INK_BLACK } else { HUD_SUB }
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
                    toast.color.with_alpha(toast_pose(TOAST_SECONDS - toast.timer, toast.timer).alpha)
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
        set_ui(&mut ui, pose);
        // The ink shadow fades with its text; ink lettering sits on a solid
        // stamp (keycap, maxed VEL badge) and casts none.
        if let Some(mut shadow) = shadow {
            let want = if c == INK_BLACK { Color::NONE } else { shadow_color(c.alpha()) };
            if shadow.color != want {
                shadow.color = want;
            }
        }
    }
}

/// `1 -> 0` envelope of the HP readout kick after taking damage.
pub fn hp_hit_envelope(since_damage: f32) -> f32 {
    if since_damage.is_finite() { (1.0 - since_damage / 0.3).clamp(0.0, 1.0) } else { 0.0 }
}

/// HUD contract-tag accent: amber, red at 3+ contracts.
fn loadout_contract_color(contracts: u32) -> Color {
    if contracts >= 3 { COLOR_SIGNAL_RED } else { COLOR_SIGNAL_AMBER }
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

/// Tint an accent rail and its glow together.
fn set_rail(bg: &mut Mut<BackgroundColor>, shadow: &mut Option<Mut<BoxShadow>>, c: Color) {
    set_bg(bg, c);
    if let Some(s) = shadow {
        set_glow(s, c);
    }
}

fn set_ui(ui: &mut Mut<UiTransform>, want: UiTransform) {
    if **ui != want {
        **ui = want;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
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
    (run, trial, pulse): (Res<RunStats>, Res<TimeTrialState>, Res<HudPulse>),
    players: Query<&Player>,
    mut boxes: Query<(&HudBox, &mut Node, &mut BackgroundColor, &mut UiTransform, Option<&mut BorderColor>, Option<&mut BoxShadow>)>,
) {
    let t = time.elapsed_secs();
    let Ok(p) = players.single() else { return };
    let (status, _, status_color) = status_line(&run, &trial, &ext, &signal);
    let uploading = matches!(*ext, ExtractionState::Uploading { .. });
    let critical = hp_band(fraction(p.hp, p.max_hp)) == HpBand::Critical;
    let shotgun = ws.current == Weapon::Shotgun;
    let (cur, max) = ammo(p, ws.current);
    let lit = lit_pips(p.ammo_shells, p.max_shells, SHELL_PIPS);
    let rocket_lit = lit_pips(p.ammo_rockets, p.max_rockets, ROCKET_PIPS);
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    let stamina = stamina_fill(p.dash_cooldown, DASH_COOLDOWN * mods.dash_cooldown_mult);
    let vel_lit = speed_segments(gauge.speed);
    let tier_color = gauge.color();
    let toast_look = |i: u8| -> Option<(ToastPose, Color)> {
        feed.toasts.iter().rev().nth(i as usize).map(|toast| (toast_pose(TOAST_SECONDS - toast.timer, toast.timer), toast.color))
    };
    for (b, mut node, mut bg, mut ui, border, mut shadow) in &mut boxes {
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
            HudBox::FloorRail => {
                // The rail thickens and flashes with whichever counter just went up.
                let e = pulse.stats[0].max(pulse.stats[1]);
                set_rail(&mut bg, &mut shadow, flash(floor_rail_color(&run, status), e));
                set_ui(&mut ui, pop_transform(Vec2::new(1.0 + 1.2 * e, 1.0), Vec2::new(-1.0, 0.0)));
            }
            HudBox::StatusRule => {
                let look = status_rule_look(status, t, pulse.status);
                set_rail(&mut bg, &mut shadow, flash(status_color.with_alpha(look.alpha), pulse.status));
                set_ui(&mut ui, pop_transform(Vec2::new(look.scale_x, 1.0), Vec2::ZERO));
            }
            HudBox::RushRail => {
                set_display(&mut node, uploading);
                if uploading {
                    set_rail(&mut bg, &mut shadow, PUNK_RED.with_alpha(status_rule_look(status, t, 0.0).alpha));
                }
            }
            HudBox::UplinkPanel => set_display(&mut node, uploading),
            HudBox::UplinkFill => {
                if let ExtractionState::Uploading { progress, jammed, rate, .. } = *ext {
                    set_width_pct(&mut node, progress * 100.0);
                    let c = if jammed {
                        COLOR_SIGNAL_RED.with_alpha(if (t * 10.0).sin() > 0.0 { 1.0 } else { 0.4 })
                    } else {
                        uplink_tier_color(uplink_tier(rate))
                    };
                    set_bg(&mut bg, c);
                }
            }
            HudBox::AlertPanel => set_display(&mut node, feed.alert.is_some()),
            HudBox::BossPlate => set_display(&mut node, boss.as_deref().is_some_and(|h| h.active)),
            HudBox::BossTrack => {
                if let Some(h) = boss.as_deref().filter(|h| h.active) {
                    set_border(track_border(boss_tag_color(h)));
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
                // Snap in from the gutter, hold, drift back out.
                let look = toast_look(i);
                set_display(&mut node, look.is_some());
                if let Some((pose, _)) = look {
                    set_ui(&mut ui, UiTransform { translation: Val2::px(pose.slide, 0.0), scale: Vec2::splat(pose.scale), rotation: Rot2::IDENTITY });
                }
            }
            HudBox::ToastPip(i) => {
                let c = toast_look(i).map_or(Color::NONE, |(pose, c)| c.with_alpha(pose.alpha));
                set_rail(&mut bg, &mut shadow, c);
            }
            HudBox::VelTrack => set_border(track_border(tier_color)),
            HudBox::VelValueBadge => set_bg(&mut bg, gauge.badge_color()),
            HudBox::VelChainTag => {
                set_display(&mut node, bhop_chain_label(p.bhop_chain).is_some());
                set_bg(&mut bg, tier_color);
            }
            HudBox::VelSeg(i) => set_bg(&mut bg, segment_color(i, vel_lit)),
            HudBox::HpTrack => {
                // Critical HP: the gauge throbs in an alert-red halo.
                if let Some(s) = &mut shadow {
                    set_glow(s, if critical { PUNK_RED.with_alpha(0.3 + 0.7 * danger_pulse(t)) } else { Color::NONE });
                }
                set_border(track_border(hp_color(fraction(p.hp, p.max_hp), t)));
            }
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
                set_border(shell_pip_border(on));
            }
            HudBox::PlasmaTrack => set_display(&mut node, ws.current == Weapon::Plasma),
            HudBox::RocketRow => set_display(&mut node, ws.current == Weapon::Rocket),
            HudBox::RocketPip(i) => {
                let on = i < rocket_lit;
                set_bg(&mut bg, if on { COLOR_WARHEAD } else { COLOR_PIP_OFF });
                set_border(rocket_pip_border(on));
            }
            HudBox::PlasmaFill => {
                set_width_pct(&mut node, fraction(cur as f32, max as f32) * 100.0);
                set_bg(&mut bg, COLOR_CYAN.with_alpha(0.8 + 0.2 * (t * 4.0).sin()));
            }
            HudBox::SlotChip(i) => {
                // The freshly equipped tab pops up off the bottom edge.
                let e = if ws.current == Weapon::ALL[i as usize] { pulse.swap } else { 0.0 };
                set_ui(&mut ui, pop_transform(Vec2::splat(pop_scale(e, 0.14)), Vec2::new(0.0, 1.0)));
            }
            HudBox::SlotKey(i) => {
                let w = Weapon::ALL[i as usize];
                set_bg(&mut bg, if ws.current == w { weapon_accent(w) } else { COLOR_PIP_OFF });
            }
            HudBox::SlotFill(i) => {
                let w = Weapon::ALL[i as usize];
                let (n, m) = ammo(p, w);
                set_width_pct(&mut node, fraction(n as f32, m as f32) * 100.0);
                set_bg(&mut bg, weapon_accent(w).with_alpha(if ws.current == w { 1.0 } else { 0.5 }));
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
/// Dev-only ([`DevTools`]): a release build without `SEGNALE_DEV` ignores F9,
/// so the floor and leaderboard / reward eligibility stay intact.
fn debug_skip_floor(keys: Res<ButtonInput<KeyCode>>, dev: Option<Res<DevTools>>, mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>) {
    if !dev.is_some_and(|d| d.0) {
        return;
    }
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
        assert!(ANDROID_INPUT_HINT.contains("TOUCH CONTROLS ON") && !ANDROID_INPUT_HINT.contains("NO TOUCH"));
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
        assert_eq!(seed_button_label(&seed, InputMode::KeyboardMouse), "SEED: RANDOM  [S]");
        seed.code = "MARS7".into();
        assert_eq!(seed_button_label(&seed, InputMode::KeyboardMouse), "SEED: MARS7  [S]");
        let mut run = RunStats::default();
        run.start_new_run(0x791E_0A5A);
        assert_eq!(with_seed("SURVEY OS".into(), &run), "SURVEY OS  //  SEED 791E-0A5A");
        assert_eq!(share_button_label(&run, &seed, InputMode::KeyboardMouse), "SHARE SEED  [S]");
        seed.arm_from_run(&run);
        assert_eq!(share_button_label(&run, &seed, InputMode::KeyboardMouse), "SEED 791E-0A5A ARMED  [S]");
        run.mode = RunMode::TimeTrial;
        assert_eq!(with_seed("X".into(), &run), "X");
    }

    #[test]
    fn ui_scale_fits_embeds_and_never_upscales() {
        assert_eq!(ui_scale_for(1280.0, 720.0), 1.0);
        assert_eq!(ui_scale_for(1920.0, 1080.0), 1.0);
        assert_eq!(ui_scale_for(960.0, 540.0), 0.75);
        assert_eq!(ui_scale_for(1024.0, 640.0), 0.8);
        assert!((ui_scale_for(1280.0, 600.0) - 600.0 / 720.0).abs() < 1e-6, "height-bound");
        assert_eq!(ui_scale_for(200.0, 100.0), UI_SCALE_MIN);
        assert_eq!(ui_scale_for(f32::NAN, 540.0), 1.0);
        assert_eq!(ui_scale_for(0.0, 0.0), 1.0);
    }

    #[test]
    fn f9_debug_skip_is_dev_only_and_off_the_field_manual() {
        assert!(dev_tools_allowed(true, false), "debug builds (and cargo test) keep F9");
        assert!(dev_tools_allowed(false, true), "SEGNALE_DEV opts a release build in");
        assert!(!dev_tools_allowed(false, false), "release builds ignore F9");
        assert_eq!(DevTools::from_env().0, cfg!(debug_assertions) || std::env::var_os("SEGNALE_DEV").is_some());
        for (_, binds) in CONTROLS.iter().chain(PAD_CONTROLS.iter()) {
            for (keys, what) in binds.iter() {
                assert!(!keys.contains(&"F9") && !what.contains("debug"), "{keys:?} {what}");
            }
        }
        assert_eq!(OS_STAMP, "UAC-OS v1.0");
    }

    #[test]
    fn controller_prompts_swap_keycaps_for_pad_buttons() {
        use MenuAction::*;
        assert_eq!(pad_prompt(AppState::MainMenu, "PLAY  [ENTER]", StartRun), "PLAY  [START]");
        assert_eq!(pad_prompt(AppState::MainMenu, "LEADERBOARD  [K]", OpenLeaderboard), "LEADERBOARD");
        assert_eq!(pad_prompt(AppState::Paused, "RESUME  [ESC]", Continue), "RESUME  [START]");
        assert_eq!(pad_prompt(AppState::Paused, "MAIN MENU  [M]", BackToMenu), "MAIN MENU", "B resumes in pause, never quits");
        assert_eq!(pad_prompt(AppState::GameOver, "MAIN MENU  [ENTER]", BackToMenu), "MAIN MENU  [B]");
        assert_eq!(pad_prompt(AppState::Victory, "RETRY  [R]", RetryTimeTrial), "RETRY  [X]");
        assert_eq!(pad_prompt(AppState::Leaderboard, "BACK  [ESC]", BackToMenu), "BACK  [B]");
        assert_eq!(pad_prompt(AppState::MainMenu, "RND", RollSeed), "RND", "no keycap stays as is");
        assert_eq!(pad_retry_action(AppState::Victory, RunMode::TimeTrial, true), Some(RetryTimeTrial));
        assert_eq!(pad_retry_action(AppState::Victory, RunMode::Standard, true), None);
        assert_eq!(pad_retry_action(AppState::Victory, RunMode::TimeTrial, false), None);
        // HUD prompts.
        assert_eq!(sos_prompt(InputMode::KeyboardMouse), "PRESS [E] TO TRIGGER SOS UPLINK");
        assert_eq!(sos_prompt(InputMode::Gamepad), "PRESS [X] TO TRIGGER SOS UPLINK");
        assert_eq!(sos_prompt(InputMode::Touch), "TAP [SOS] TO TRIGGER SOS UPLINK");
        assert_eq!(pad_bar_label(STAMINA_LABEL_KBM), STAMINA_LABEL_PAD);
        assert_eq!(pad_bar_label("HP"), "HP");
        let keys = |mode, cur| (0..3).map(|i| slot_key_label(mode, i, cur, 3)).collect::<String>();
        assert_eq!(keys(InputMode::KeyboardMouse, 1), "123");
        assert_eq!(keys(InputMode::Gamepad, 0), "*Y<");
        assert_eq!(keys(InputMode::Gamepad, 2), "Y<*");
        // Seed editor + labels.
        let mut seed = CustomSeedState::default();
        assert_eq!(seed_button_label(&seed, InputMode::Gamepad), "SEED: RANDOM");
        seed.toggle_edit();
        assert_eq!(seed_status_line(&seed, InputMode::Gamepad), PAD_TEXT_HINT);
        assert!(seed_status_line(&seed, InputMode::KeyboardMouse).contains("ENTER"));
        // The pad field manual covers every documented binding.
        let pad: Vec<&str> = PAD_CONTROLS.iter().flat_map(|(_, b)| b.iter().flat_map(|(k, _)| k.iter().copied())).collect();
        for k in ["LS", "RS", "A", "L3", "LB", "RT", "LT", "RB", "X", "Y", "DPAD", "START", "B", "D-UP"] {
            assert!(pad.contains(&k), "{k} missing from the controller manual");
        }
    }

    #[test]
    fn controls_overlay_hotkeys() {
        assert_eq!(controls_hotkey_action(AppState::MainMenu, true), Some(MenuAction::ToggleControls));
        assert_eq!(controls_hotkey_action(AppState::Paused, true), Some(MenuAction::ToggleControls));
        for st in [AppState::Playing, AppState::Loadout, AppState::Settings, AppState::GameOver, AppState::Victory, AppState::Leaderboard] {
            assert_eq!(controls_hotkey_action(st, true), None, "{st:?}");
        }
        assert_eq!(controls_hotkey_action(AppState::MainMenu, false), None);
        // Esc / B close an open overlay (Pause never resumes from under it).
        assert!(controls_back_closes(AppState::Paused, true, true, false));
        assert!(controls_back_closes(AppState::MainMenu, true, false, true));
        assert!(!controls_back_closes(AppState::Paused, false, true, false), "closed overlay: Esc resumes as before");
        assert!(!controls_back_closes(AppState::Loadout, true, true, false));
        // CLOSE shows controller B; CONTROLS keeps no pad tag.
        assert_eq!(pad_prompt(AppState::MainMenu, CONTROLS_CLOSE, MenuAction::CloseControls), "CLOSE  [B]");
        assert_eq!(pad_prompt(AppState::Paused, CONTROLS_BUTTON, MenuAction::ToggleControls), "CONTROLS");
    }

    #[test]
    fn seed_status_only_while_active() {
        let mut seed = CustomSeedState::default();
        assert!(!seed_status_visible(&seed), "idle: the mode telemetry owns the slot");
        seed.toggle_edit();
        assert!(seed_status_visible(&seed));
        assert_eq!(seed_status_line(&seed, InputMode::Gamepad), PAD_TEXT_HINT);
        seed.lock();
        seed.status_msg = Some("SEED EDIT CANCELLED".into());
        assert!(seed_status_visible(&seed));
    }

    #[test]
    fn pause_readout_is_two_compact_lines() {
        let mut run = RunStats { floor: 2, kills: 41, jammers_destroyed: 2, time_elapsed: 192.0, ..default() };
        assert_eq!(pause_summary(&run), "FLOOR 2/5  |  03:12  |  41 KILLS  |  2 JAMMERS");
        let loadout = RunLoadout::default();
        assert!(pause_loadout_line(&loadout, &run).contains("|  0 APPS"), "{}", pause_loadout_line(&loadout, &run));
        let survey = RunLoadout { chip: crate::meta::StarterChip::SurveyOs, ..default() };
        assert!(pause_loadout_line(&survey, &run).starts_with("CHIP SURVEY OS  |"), "no doubled OS: {}", pause_loadout_line(&survey, &run));
        run.upgrades.push(crate::state::UpgradeId::EmpOvercharge);
        assert!(pause_loadout_line(&loadout, &run).contains("|  1 APP"));
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
        let held = ExtractionState::Uploading { progress: 0.99, duration: 44.0, in_ring: true, rate: 1.25, jammed: false, wave_timer: 1.0, baron_spawned: true, boss_hold: true };
        assert_eq!(uplink_status_text(&held), BOSS_HOLD_STATUS);
        assert_eq!(crate::boss::BOSS_HOLD_TEXT, "UPLINK HELD AT 99% // KILL THE CARRIER DAEMON TO EXTRACT");
        assert_eq!(uplink_status_text(&ExtractionState::start_upload(1)), "DEMON RUSH - SOS UPLINK LIVE");
    }

    #[test]
    fn free_roam_uplink_label_shows_live_speed_and_never_scolds() {
        use crate::extraction::{proximity_upload_rate, UPLINK_MAX_RATE, UPLINK_MIN_RATE};
        let label = |progress: f32, rate: f32| {
            let mut b = String::new();
            let tier = write_uplink_speed(&mut b, progress, rate);
            (b, tier)
        };
        assert_eq!(label(0.42, UPLINK_MAX_RATE), ("SOS OVERDRIVE  1.25x  42%".to_string(), UplinkTier::Overdrive));
        assert_eq!(label(0.42, 0.93), ("UPLINKING  0.93x  42%".to_string(), UplinkTier::Near));
        assert_eq!(label(0.42, 0.80).1, UplinkTier::Near);
        assert_eq!(label(0.42, 0.61), ("UPLINKING  0.61x  (CLOSER = FASTER)  42%".to_string(), UplinkTier::Roaming));
        assert_eq!(label(0.0, UPLINK_MIN_RATE).0, "UPLINKING  0.45x  (CLOSER = FASTER)  0%");
        // Distances map to the expected tiers: ring, just outside ring, outer atrium + adjacent halls, mid-bunker.
        assert_eq!(uplink_tier(proximity_upload_rate(5.0, false)), UplinkTier::Overdrive);
        assert_eq!(uplink_tier(proximity_upload_rate(9.5, false)), UplinkTier::Near);
        assert_eq!(uplink_tier(proximity_upload_rate(14.0, false)), UplinkTier::Near);
        assert_eq!(uplink_tier(proximity_upload_rate(20.0, false)), UplinkTier::Near);
        assert_eq!(uplink_tier(proximity_upload_rate(28.0, false)), UplinkTier::Roaming);
        // Tones: boost green, cyan, warm amber; never the jam / alarm red.
        assert_eq!(uplink_tier_color(UplinkTier::Overdrive), ACID_GREEN);
        assert_eq!(uplink_tier_color(UplinkTier::Near), COLOR_CYAN);
        assert_eq!(uplink_tier_color(UplinkTier::Roaming), COLOR_SIGNAL_AMBER);
        for tier in [UplinkTier::Overdrive, UplinkTier::Near, UplinkTier::Roaming] {
            assert_ne!(uplink_tier_color(tier), COLOR_SIGNAL_RED);
        }
        for r in [0.45f32, 0.6, 0.8, 1.0, 1.25] {
            assert!(!label(0.5, r).0.contains("RETURN"), "no scolding copy at {r}x");
        }
    }

    #[test]
    fn slot_labels_hero_font_and_rush_frame() {
        assert_eq!([slot_label(0), slot_label(1), slot_label(2)], ["SLOT 01", "SLOT 02", "SLOT 03"]);
        // The hero switch is set larger than the secondary switches of its row.
        assert_eq!(button_font(ButtonStyle::HazardAmber, 20.0), 20.0);
        assert_eq!(button_font(ButtonStyle::HeroAcid, 20.0), 30.0);
        // DEMON RUSH: the status line, its underline and the floor rail go alert red.
        let (run, trial) = (RunStats::default(), TimeTrialState::default());
        let signal = SignalState { in_dead_zone: false, near_tower: false, ..default() };
        let calm = status_line(&run, &trial, &ExtractionState::Searching { hold: 0.0 }, &signal);
        let uploading = ExtractionState::Uploading { progress: 0.4, duration: 28.0, in_ring: true, rate: 1.25, jammed: false, wave_timer: 0.5, baron_spawned: false, boss_hold: false };
        let rush = status_line(&run, &trial, &uploading, &signal);
        assert_eq!(calm, (StatusKind::Searching, "SEARCH FOR 5-BAR SIGNAL TOWER", COLOR_SIGNAL_AMBER));
        assert_eq!(rush, (StatusKind::Rush, "DEMON RUSH - SOS UPLINK LIVE", COLOR_SIGNAL_RED));
        assert_eq!((floor_rail_color(&run, calm.0), floor_rail_color(&run, rush.0)), (HAZARD_YELLOW, PUNK_RED));
        // Signal states: lock, dead zone; the Time Trial keeps its own cyan lines.
        let near = SignalState { near_tower: true, in_dead_zone: true, ..signal.clone() };
        assert_eq!(status_line(&run, &trial, &ExtractionState::Searching { hold: 0.0 }, &near).0, StatusKind::Locked);
        let dead = SignalState { in_dead_zone: true, ..signal.clone() };
        let dz = status_line(&run, &trial, &ExtractionState::Searching { hold: 0.0 }, &dead);
        assert_eq!((dz.0, dz.1, floor_rail_color(&run, dz.0)), (StatusKind::DeadZone, "NO SERVICE - DEAD ZONE", PUNK_RED));
        let tt = RunStats { mode: RunMode::TimeTrial, ..default() };
        let line = status_line(&tt, &trial, &uploading, &dead);
        assert_eq!((line.0, line.2, floor_rail_color(&tt, line.0)), (StatusKind::Trial, ARC_CYAN, ARC_CYAN));
        let last = TimeTrialState { next_gate: crate::trials::TRIAL_CHECKPOINTS, ..default() };
        assert_eq!(status_line(&tt, &last, &uploading, &dead).0, StatusKind::TrialFinal);
        assert_eq!(floor_rail_color(&RunStats { endless: true, ..default() }, StatusKind::Searching), COLOR_ENDLESS);
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
        let card_inner = DECK_WIDTH;
        let pills: f32 = [OVERCLOCK_PILL, DOPPLER_PILL].iter().map(|l| mono_width(l.len(), PILL_FONT) + 2.0 * PILL_PAD_X).sum::<f32>() + PILL_GAP;
        for w in Weapon::ALL {
            let title = mono_width(weapon_title(w).len(), WEAPON_TITLE_FONT);
            assert!(title + 8.0 + pills <= card_inner, "{} header overflows", weapon_title(w));
        }
        // Reserve readout: red when dry, bright only on the equipped weapon.
        assert_eq!(slot_ammo_color(0, true), COLOR_SIGNAL_RED);
        assert_eq!(slot_ammo_color(0, false), COLOR_SIGNAL_RED);
        assert_eq!(slot_ammo_color(12, true), COLOR_TEXT);
        assert_eq!(slot_ammo_color(12, false), HUD_SUB);
        // Pips and the plasma bar share one right-aligned row inside the deck.
        assert_eq!(PIP_ROW_WIDTH, 207.0);
        assert_eq!(ROCKET_PIPS, SHELL_PIPS);
    }

    #[test]
    fn style_meter_hud_rules_and_layout_budget() {
        use crate::style::StyleRank;
        assert!(!style_hud_visible(0.0, RunMode::Standard), "hidden at zero points");
        assert!(style_hud_visible(12.0, RunMode::Standard));
        assert!(style_hud_visible(12.0, RunMode::Daily(crate::trials::DailyDate::EPOCH)));
        assert!(!style_hud_visible(500.0, RunMode::TimeTrial), "speedrun HUD stays clean");
        assert_eq!(peak_style_label(StyleRank::SS), "PEAK STYLE  SS // 6G");
        assert_eq!(peak_style_label(StyleRank::SSS), "PEAK STYLE  SSS // 6G+");
        // Every caption (`TAG // SUBTITLE`, two 5 px gaps) fits the width budget.
        for r in StyleRank::ALL {
            let w = mono_width(r.tag().len() + 2 + r.subtitle().len(), STYLE_CAPTION_FONT) + 10.0;
            assert!(w <= STYLE_HUD_WIDTH, "{r:?}: {w}");
        }
        // Right column at the 720p reference (960x540 and 1280x720 scale to it,
        // 1024x640 and 1920x1080 are taller): the split timer sits in the
        // corner and a full callout stack under it ends by ~215 px; the widget
        // must end before the weapon deck (bottom 14 px, ~104 px tall) starts
        // at ~602 px.
        let split_bottom = RIGHT_COLUMN_BOTTOM;
        let deck_top = UI_REFERENCE.1 - HUD_BOTTOM - DECK_HEIGHT;
        assert!(STYLE_HUD_TOP + STYLE_HUD_HEIGHT <= deck_top - 8.0, "{} vs {deck_top}", STYLE_HUD_TOP + STYLE_HUD_HEIGHT);
        for (w, h) in [(960.0, 540.0), (1024.0, 640.0), (1280.0, 720.0), (1920.0, 1080.0)] {
            let s = ui_scale_for(w, h);
            assert!((STYLE_HUD_TOP + STYLE_HUD_HEIGHT) * s <= h - (HUD_BOTTOM + DECK_HEIGHT) * s, "{w}x{h}");
        }
        // Even the biggest frame of a rank-up slam keeps clear of the split
        // plate and never crosses the right gutter.
        let half = STYLE_EMBLEM_SIZE / 2.0;
        for i in 0..=200 {
            let pop = i as f32 / 200.0;
            for rank in [StyleRank::D, StyleRank::A, StyleRank::SSS] {
                let p = style_emblem_pose(Some(rank), pop, 0.0, 0.0, i as f32 * 0.37);
                let top = STYLE_HUD_TOP + half + p.offset.y - half * p.scale;
                assert!(top >= split_bottom + 6.0, "{rank:?} pop {pop}: top {top}");
                let right_overhang = p.offset.x + half * p.scale - half;
                assert!(right_overhang <= 1.2, "{rank:?} pop {pop}: {right_overhang}");
                assert!(STYLE_HUD_TOP + half + p.offset.y + half * p.scale <= deck_top, "{rank:?} pop {pop}");
            }
        }
    }

    #[test]
    fn frameless_hud_has_no_plates_and_every_label_casts_a_shadow() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(RunStats::default());
        world.run_system_once(spawn_hud).unwrap();

        // No slab anywhere: the only dark backgrounds left are gauge wells,
        // ammo cells, keycaps and tick marks, all of them thin.
        let px = |v: Val| if let Val::Px(p) = v { Some(p) } else { None };
        let mut nodes = world.query::<(&Node, &BackgroundColor)>();
        let mut wells = 0;
        for (node, bg) in nodes.iter(&world) {
            let c = bg.0.to_srgba();
            if c.alpha <= 0.3 || c.red.max(c.green).max(c.blue) >= 0.25 {
                continue;
            }
            // Fills and cuts span their own (thin) track.
            if node.height == Val::Percent(100.0) {
                continue;
            }
            wells += 1;
            let thin = px(node.height).is_some_and(|h| h <= 16.0) || px(node.width).is_some_and(|w| w <= 20.0);
            assert!(thin, "dark slab: {:?} x {:?}", node.width, node.height);
        }
        assert!(wells >= 8, "gauge tracks keep their dark wells: {wells}");

        // Cluster roots and rows carry no background, border or padding.
        let bare = |node: &Node, bg: &BackgroundColor, what: &str| {
            assert_eq!(bg.0.alpha(), 0.0, "{what}: background");
            assert_eq!((node.border, node.padding), (UiRect::default(), UiRect::default()), "{what}: frame");
        };
        let mut split = world.query_filtered::<(&Node, &BackgroundColor), With<SplitPlate>>();
        let (node, bg) = split.single(&world).unwrap();
        bare(node, bg, "split timer");
        assert_eq!((node.display, node.align_items), (Display::None, AlignItems::FlexEnd));
        let mut boxes = world.query::<(&HudBox, &Node, &BackgroundColor, Option<&BoxShadow>)>();
        let (mut rails, mut toasts) = (0, 0);
        for (b, node, bg, shadow) in boxes.iter(&world) {
            match b {
                HudBox::ToastChip(_) | HudBox::BossPlate | HudBox::AlertPanel | HudBox::UplinkPanel => {
                    toasts += usize::from(matches!(b, HudBox::ToastChip(_)));
                    bare(node, bg, "row");
                }
                HudBox::SlotChip(_) => {
                    assert_eq!(bg.0.alpha(), 0.0, "slot tab");
                    assert_eq!(node.border, UiRect::default(), "slot tab");
                }
                // Every accent rail glows: a halo over an ink edge.
                HudBox::FloorRail | HudBox::StatusRule | HudBox::RushRail | HudBox::ToastPip(_) | HudBox::HpTrack => {
                    rails += 1;
                    assert_eq!(shadow.map(|s| s.0.len()), Some(2), "{b:?}");
                }
                _ => {}
            }
        }
        assert_eq!((rails, toasts), (5 + TOAST_SLOTS as usize, TOAST_SLOTS as usize));
        let mut rule = world.query_filtered::<(&Node, &BoxShadow), With<SplitRule>>();
        assert_eq!(rule.single(&world).unwrap().0.height, Val::Px(2.0));

        // Every floating label sits on a hard ink shadow and never wraps; only
        // ink lettering on a solid stamp and the reticle hitmarker go without.
        let mut labels = world.query::<(&HudText, Option<&TextShadow>, &TextLayout)>();
        let mut seen = 0;
        for (label, shadow, layout) in labels.iter(&world) {
            seen += 1;
            match label {
                HudText::Hitmarker | HudText::VelChain => assert!(shadow.is_none(), "{label:?}"),
                _ => {
                    let s = shadow.unwrap_or_else(|| panic!("{label:?} has no shadow"));
                    assert!(s.color.alpha() > 0.9 && s.offset.x >= 1.4 && s.offset.x <= 2.6, "{label:?}");
                    assert_eq!(layout.linebreak, bevy::text::LineBreak::NoWrap, "{label:?}");
                }
            }
        }
        assert!(seen >= 30, "{seen} HUD labels");
        let mut split_text = world.query::<(&SplitText, &TextShadow)>();
        assert_eq!(split_text.iter(&world).count(), 5);
        // Both counters of the floor strip exist, and they pop independently.
        let mut stats: Vec<u8> = labels.iter(&world).filter_map(|(l, ..)| if let HudText::Stats(i) = l { Some(*i) } else { None }).collect();
        stats.sort_unstable();
        assert_eq!(stats, [0, 1]);
    }

    #[test]
    fn frameless_layout_budget_holds_at_every_resolution() {
        // Longest lines of each top-row cluster (monospace).
        let status = [
            "TIME TRIAL - 3 CHECKPOINTS, THEN THE TOWER",
            "5-BAR SIGNAL - EXTRACTION TOWER IN RANGE",
            "FINAL STRETCH - LAND ON THE TOWER PAD",
            BOSS_HOLD_STATUS,
        ];
        let status_half = status.iter().map(|s| mono_width(s.len(), 20.0)).fold(0.0, f32::max) / 2.0 + 10.0;
        let strip_right = HUD_GUTTER + RAIL_WIDTH + 9.0 + (1..=5).map(|f| mono_width(sector_label(f).len(), 11.0)).fold(0.0, f32::max);
        for line in ["TIME TRIAL  [R] RESET", "FLOOR 5/5  PB 00:00.0", "PB GHOST  +12.34s", "DAILY 2026-11-26"] {
            let font = if line.starts_with("FLOOR") { 11.0 } else if line.starts_with("PB") { 13.0 } else { 10.0 };
            assert!(mono_width(line.len(), font) <= SPLIT_PLATE_WIDTH, "{line}");
        }
        assert!(mono_width(7, SPLIT_TIME_FONT) <= SPLIT_PLATE_WIDTH);
        for (w, h) in [(960.0, 540.0), (1024.0, 640.0), (1280.0, 720.0), (1920.0, 1080.0)] {
            // Logical canvas: every size keeps at least the 1280 x 720 reference.
            let s = ui_scale_for(w, h);
            let (lw, lh) = (w / s, h / s);
            assert!(lw >= UI_REFERENCE.0 - 0.5 && lh >= UI_REFERENCE.1 - 0.5, "{w}x{h}");
            // Top row: floor strip | status line | split timer never touch.
            assert!(strip_right + 24.0 <= lw / 2.0 - status_half, "{w}x{h}: strip vs status");
            assert!(lw / 2.0 + status_half + 24.0 <= lw - HUD_GUTTER - SPLIT_PLATE_WIDTH, "{w}x{h}: status vs timer");
            // Right edge, top to bottom: timer + callouts (checked at compile
            // time against the emblem), emblem, weapon deck.
            assert!(STYLE_HUD_TOP + STYLE_HUD_HEIGHT + 24.0 <= lh - HUD_BOTTOM - DECK_HEIGHT, "{w}x{h}");
            // Bottom row: the centered vitals never reach the deck.
            assert!(lw / 2.0 + VITALS_WIDTH / 2.0 + 24.0 <= lw - HUD_GUTTER - DECK_WIDTH, "{w}x{h}: vitals vs deck");
            // The prompt floats above the vitals (even with the BHOP stamp up,
            // by construction of `PROMPT_BOTTOM`) and stays well below the reticle.
            assert!(lh - PROMPT_BOTTOM - 28.0 >= lh / 2.0 + CROSSHAIR_BOX, "{w}x{h}: prompt vs reticle");
        }
        // HP readout kick: full on the hit, gone 0.3 s later.
        assert_eq!((hp_hit_envelope(0.0), hp_hit_envelope(0.3), hp_hit_envelope(99.0), hp_hit_envelope(f32::NAN)), (1.0, 0.0, 0.0, 0.0));
        assert!((hp_hit_envelope(0.15) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn style_emblem_pose_slams_bounces_shakes_and_trembles() {
        use crate::style::StyleRank;
        // Rank-up slam: 1.55x -> 0.93x -> 1.0x with a -14 deg -> +3 deg -> 0 whip.
        assert_eq!(style_pop_scale(0.0), 1.0);
        assert!((style_pop_scale(1.0) - 1.55).abs() < 1e-4 && style_pop_scale(5.0) == style_pop_scale(1.0));
        assert!((style_pop_scale(0.58) - 0.93).abs() < 1e-3, "{}", style_pop_scale(0.58));
        assert!((style_pop_tilt(1.0) + 14.0).abs() < 1e-3 && (style_pop_tilt(0.58) - 3.0).abs() < 1e-2 && style_pop_tilt(0.0) == 0.0);
        let scales: Vec<f32> = (0..=100).map(|i| style_pop_scale(1.0 - i as f32 / 100.0)).collect();
        let low = scales.iter().copied().fold(f32::MAX, f32::min);
        assert!((0.925..0.94).contains(&low), "overshoot dips under 1x: {low}");
        assert!(scales.windows(2).all(|w| (w[0] - w[1]).abs() < 0.06), "no pops in the curve");
        assert_eq!((style_pop_scale(f32::NAN), style_pop_tilt(f32::NAN), style_pop_scale(-1.0)), (1.0, 0.0, 1.0));
        // Resting pose: unscaled, a barely-there living sway.
        let span = |rank, pop, act, hit| {
            let poses: Vec<StyleEmblemPose> = (0..240).map(|i| style_emblem_pose(Some(rank), pop, act, hit, i as f32 / 60.0)).collect();
            let max = |f: fn(&StyleEmblemPose) -> f32| poses.iter().map(f).fold(0.0f32, f32::max);
            (max(|p| (p.scale - 1.0).abs()), max(|p| p.tilt.abs()), max(|p| p.offset.x.abs()), max(|p| p.offset.y.abs()))
        };
        let rest = span(StyleRank::A, 0.0, 0.0, 0.0);
        assert!(rest.0 == 0.0 && rest.1 > 0.3 && rest.1 <= 0.8 && rest.2 == 0.0 && rest.3 > 0.2 && rest.3 <= 0.6, "{rest:?}");
        // SS / 6G and SSS / 6G+ carry an electric micro-tremor.
        let (ss, sss) = (span(StyleRank::SS, 0.0, 0.0, 0.0), span(StyleRank::SSS, 0.0, 0.0, 0.0));
        assert!(ss.2 > 0.3 && sss.2 > ss.2 && sss.2 <= 1.2, "{ss:?} {sss:?}");
        // Action pulse: a crisp ~10% kick that is gone when the envelope ends.
        let kick = style_emblem_pose(Some(StyleRank::B), 0.0, 1.0, 0.0, 0.0);
        assert!((kick.scale - 1.10).abs() < 1e-4, "{}", kick.scale);
        assert!(style_emblem_pose(Some(StyleRank::B), 0.0, 0.5, 0.0, 0.0).scale < kick.scale);
        // Take-hit: a horizontal tremor and a tilt kick, far beyond the idle sway.
        let hit = span(StyleRank::S, 0.0, 0.0, 1.0);
        assert!(hit.2 > 5.0 && hit.2 <= 7.0 && hit.1 > 5.0, "{hit:?}");
        // Garbage in, a sane pose out.
        let nan = style_emblem_pose(None, f32::NAN, f32::INFINITY, f32::NAN, f32::NAN);
        assert!(nan.scale.is_finite() && nan.tilt.is_finite() && nan.offset.is_finite());
        // Callouts fade with age, the older slot dimmer.
        assert_eq!(style_feed_alpha(0, 0.0), 1.0);
        assert!(style_feed_alpha(1, 0.0) < 1.0 && style_feed_alpha(0, crate::style::FEED_SECS) == 0.0);
        assert!(style_feed_alpha(0, 2.0) < style_feed_alpha(0, 1.0));
    }

    #[test]
    fn touch_menus_drop_keycaps_and_route_back_and_seed_taps() {
        let seed = CustomSeedState::default();
        assert_eq!(seed_button_label(&seed, InputMode::Touch), "SEED: RANDOM");
        assert_eq!(seed_button_label(&seed, InputMode::Gamepad), "SEED: RANDOM");
        assert_eq!(seed_status_line(&seed, InputMode::Touch), TOUCH_SEED_HINT);
        let run = RunStats::default();
        assert!(!share_button_label(&run, &seed, InputMode::Touch).contains('['));
        // A SEED tap in touch mode shows the hint instead of opening the
        // keyboard-only editor; RND / CLR and other modes are untouched.
        let mut s = CustomSeedState::default();
        assert!(touch_seed_tap(&mut s, InputMode::Touch, MenuAction::ToggleSeedEdit));
        assert!(!s.editing && s.status_msg.as_deref() == Some(TOUCH_SEED_HINT) && seed_status_visible(&s));
        assert!(!touch_seed_tap(&mut s, InputMode::Touch, MenuAction::RollSeed));
        assert!(!touch_seed_tap(&mut s, InputMode::KeyboardMouse, MenuAction::ToggleSeedEdit));
        let mut editing = CustomSeedState { editing: true, ..default() };
        assert!(!touch_seed_tap(&mut editing, InputMode::Touch, MenuAction::ToggleSeedEdit), "an open editor can still be closed");
        // Upgrade card hints.
        let hint = DualPrompt::new(UPGRADE_HINT_KBM, UPGRADE_HINT_PAD).with_touch(UPGRADE_HINT_TOUCH);
        assert_eq!(hint.get(InputMode::Touch), "TAP A CARD TO INSTALL");
        assert_eq!(DualPrompt::new("KEY [1]", "").with_touch("").get(InputMode::Touch), "");
        assert_eq!(DualPrompt::new("INSTALL  [1]", "INSTALL  [A]").get(InputMode::Touch), "INSTALL");
        // Android Back (Esc) on the end cards = controller B; Enter still works.
        for st in [AppState::GameOver, AppState::Victory] {
            assert_eq!(menu_key_action(st, false, true, false, false, false, false), Some(MenuAction::BackToMenu), "{st:?}");
            assert_eq!(menu_key_action(st, false, true, false, false, false, false), pad_menu_action(st, false, true), "{st:?}");
        }
        assert_eq!(menu_key_action(AppState::MainMenu, false, true, false, false, false, false), None, "Back never leaves the main menu");
        // Finger-sized menu switches in touch mode, restored otherwise.
        assert_eq!(touch_target_min(true, Val::Auto, Val::Auto), (Val::Px(TOUCH_MENU_TARGET), Val::Px(TOUCH_MENU_TARGET)));
        assert_eq!(touch_target_min(true, Val::Px(176.0), Val::Auto), (Val::Px(176.0), Val::Px(TOUCH_MENU_TARGET)), "a hero keeps its larger minimum");
        assert_eq!(touch_target_min(false, Val::Px(TOUCH_MENU_TARGET), Val::Px(TOUCH_MENU_TARGET)), (Val::Auto, Val::Auto));
        assert_eq!(touch_target_min(false, Val::Px(176.0), Val::Auto), (Val::Px(176.0), Val::Auto));
        // Touch pause: a wrapping 460 px grid of full-width and half cells.
        let stack = pause_stack_node(true);
        assert!(stack.flex_direction == FlexDirection::Row && stack.flex_wrap == FlexWrap::Wrap && stack.width == Val::Px(PAUSE_TOUCH_STACK));
        assert_eq!(pause_stack_node(false).flex_direction, FlexDirection::Column);
        assert_eq!(pause_cell_node(true).width, Val::Percent(100.0));
        let half = pause_cell_node(false);
        assert!(half.flex_grow == 1.0 && matches!(half.flex_basis, Val::Px(b) if b * 2.0 + 8.0 <= PAUSE_TOUCH_STACK));
    }

    #[test]
    fn phone_viewports_fit_the_720p_composition() {
        // 20:9 phones in landscape (logical dp): the whole 1280x720 layout
        // scales uniformly and still fits both axes above the floor.
        for (w, h) in [(915.0, 412.0), (844.0, 390.0), (800.0, 360.0)] {
            let s = ui_scale_for(w, h);
            assert!((UI_SCALE_MIN..1.0).contains(&s), "{w}x{h}: {s}");
            assert!(UI_REFERENCE.1 * s <= h + 0.5 && UI_REFERENCE.0 * s <= w + 0.5, "{w}x{h} overflows at {s}");
        }
        // Touch mode docks the compact deck left of the vitals, clear of the
        // right half; a portrait phone (390x844 at the 0.5 UI floor) raises
        // it to the left gutter instead of pushing it off-screen.
        for (vp, raise) in [(Vec2::new(915.0, 412.0), 300.0), (Vec2::new(844.0, 390.0), 300.0), (Vec2::new(390.0, 844.0), 420.0), (Vec2::new(320.0, 640.0), 380.0)] {
            let k = ui_scale_for(vp.x, vp.y);
            let ui = vp / k;
            let (x, bottom) = touch_deck_place(ui, raise);
            assert!(x >= HUD_GUTTER && x + TOUCH_DECK_WIDTH <= ui.x - HUD_GUTTER, "{vp}: deck x {x} in {ui}");
            let vitals = Rect::new(ui.x * 0.5 - VITALS_WIDTH * 0.5, ui.y - HUD_BOTTOM - VITALS_HEIGHT - BHOP_TAG_HEIGHT, ui.x * 0.5 + VITALS_WIDTH * 0.5, ui.y - HUD_BOTTOM);
            let deck = Rect::new(x, ui.y - bottom - 60.0, x + TOUCH_DECK_WIDTH, ui.y - bottom);
            assert!(deck.intersect(vitals).is_empty(), "{vp}: deck over the vitals");
            assert!(x + TOUCH_DECK_WIDTH <= ui.x * 0.5, "{vp}: deck stays in the left half");
            if vp.x > vp.y {
                assert_eq!(bottom, HUD_BOTTOM, "{vp}: landscape docks on the bottom row");
            } else {
                assert!(bottom >= raise, "{vp}: raised above the stick column");
            }
        }
        let (fill, frame) = touch_slot_plate(Weapon::Plasma, true);
        assert!(fill.alpha() > 0.4 && frame.left == weapon_accent(Weapon::Plasma) && frame.bottom == INK_BLACK);
        assert_eq!(touch_slot_plate(Weapon::Plasma, false).1.left, STEEL_BORDER);
    }
}