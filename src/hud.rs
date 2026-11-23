//! Menus (main / App Store upgrade select / game over / victory) and the
//! in-game HUD: status + SOS uplink bar, crosshair hitmarker, killfeed toasts,
//! HP / armor / stamina vitals, weapon & ammo deck and a subtle Dead-Zone edge
//! frame. Signal bars, radar and phone apps live on the 3D phone screen
//! (`phone_screen.rs`), not here.
//!
//! Buttons use a small manual hit test (`ComputedNode::contains_point` with the
//! window's physical cursor position) because Bevy 0.20 deprecated `Interaction`
//! in favour of picking, which this build does not enable.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;

use crate::audio::{SfxEvent, SfxQueue};
use crate::extraction::{ExtractionState, UPLINK_HOLD_SECS};
use crate::fx::HudFeed;
use crate::player::{Player, DASH_COOLDOWN};
use crate::signal::SignalState;
use crate::state::{complete_floor, start_run, AppState, GameSet, PlayerModifiers, RunStats, FINAL_FLOOR};
use crate::upgrades::{draft_upgrades, next_rank_preview, pick_upgrade, UpgradeChoices};
use crate::weapons::{Weapon, WeaponState};

pub const COLOR_BG: Color = Color::srgb(0.07, 0.01, 0.01);
pub const COLOR_TITLE: Color = Color::srgb(1.0, 0.22, 0.12);
pub const COLOR_TEXT: Color = Color::srgb(0.92, 0.82, 0.75);
pub const COLOR_DIM: Color = Color::srgb(0.55, 0.45, 0.42);
pub const COLOR_SIGNAL_RED: Color = Color::srgb(1.0, 0.15, 0.1);
pub const COLOR_SIGNAL_AMBER: Color = Color::srgb(1.0, 0.7, 0.1);
pub const COLOR_SIGNAL_GREEN: Color = Color::srgb(0.2, 1.0, 0.75);
pub const COLOR_CYAN: Color = Color::srgb(0.3, 0.95, 1.0);
pub const COLOR_STORE_BG: Color = Color::srgba(0.0, 0.04, 0.05, 0.94);
pub const COLOR_CARD: Color = Color::srgb(0.02, 0.1, 0.11);
pub const COLOR_CARD_HOVER: Color = Color::srgb(0.04, 0.22, 0.22);

/// Color for a 0..=5 bar reading.
pub fn signal_color(bars: u8) -> Color {
    match bars {
        0..=1 => COLOR_SIGNAL_RED,
        2..=3 => COLOR_SIGNAL_AMBER,
        _ => COLOR_SIGNAL_GREEN,
    }
}

/// What a clickable menu button does.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    StartRun,
    Continue,
    BackToMenu,
}

#[derive(Component)]
struct MenuButton;

/// App Store card for offer `n` (0..3).
#[derive(Component)]
struct UpgradeCard(usize);

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::MainMenu), spawn_main_menu)
            .add_systems(OnEnter(AppState::Playing), spawn_hud)
            .add_systems(OnEnter(AppState::UpgradeSelect), spawn_app_store.after(draft_upgrades))
            .add_systems(OnEnter(AppState::GameOver), spawn_game_over)
            .add_systems(OnEnter(AppState::Victory), spawn_victory)
            .add_systems(Update, (menu_buttons, menu_keys).run_if(not(in_state(AppState::Playing))))
            .add_systems(Update, upgrade_cards.run_if(in_state(AppState::UpgradeSelect)))
            .add_systems(Update, (update_hud, update_hud_boxes, debug_skip_floor).in_set(GameSet::Ui));
    }
}

fn text(s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (Text::new(s), TextFont::from_font_size(size), TextColor(color))
}

fn button(label: &str, action: MenuAction) -> impl Bundle {
    (
        MenuButton,
        action,
        Node {
            padding: UiRect::axes(Val::Px(36.0), Val::Px(14.0)),
            margin: UiRect::top(Val::Px(28.0)),
            border: UiRect::all(Val::Px(3.0)),
            justify_content: JustifyContent::Center,
            ..default()
        },
        BorderColor::all(COLOR_TITLE),
        BackgroundColor(Color::srgb(0.25, 0.03, 0.02)),
        children![text(label, 28.0, COLOR_TEXT)],
    )
}

fn screen_root(state: AppState, bg: Color) -> impl Bundle {
    (
        DespawnOnExit(state),
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: Val::Px(10.0),
            ..default()
        },
        BackgroundColor(bg),
    )
}

fn spawn_main_menu(mut commands: Commands) {
    // Menu needs its own camera: the player camera only exists in a run.
    commands.spawn((DespawnOnExit(AppState::MainMenu), Camera3d::default(), Camera { clear_color: ClearColorConfig::Custom(COLOR_BG), ..default() }));
    commands.spawn((
        screen_root(AppState::MainMenu, COLOR_BG),
        children![
            text("SEGNALE DEBOLE", 84.0, COLOR_TITLE),
            text("// WEAK SIGNAL //", 22.0, COLOR_SIGNAL_AMBER),
            (
                Node { max_width: Val::Px(780.0), margin: UiRect::vertical(Val::Px(14.0)), ..default() },
                children![text(
                    "Mars Research Bunker 7 has gone dark. Hell came through the server racks.\n\
                     Your left hand grips a cracked phone hunting for a single bar.\n\
                     Your right hand grips a pump shotgun.\n\
                     Find the 5-bar Signal Tower, destroy the Demon Jammers, hold the SOS uplink. Get out.",
                    20.0,
                    COLOR_TEXT
                )],
            ),
            text(
                "WASD move   MOUSE look   SPACE jump   SHIFT airplane-mode dash\n\
                 LMB fire   1/2/TAB/WHEEL switch shotgun / plasma\n\
                 RMB PING SCAN (1+ bar)   Q 5G OVERLOAD EMP (3+ bars)   E trigger SOS uplink at the tower\n\
                 CLICK capture mouse   ESC release   (F9 debug: skip floor)",
                17.0,
                COLOR_DIM
            ),
            button("START RUN  [ENTER]", MenuAction::StartRun),
        ],
    ));
}

// ---------------------------------------------------------------------------
// App Store (upgrade select)
// ---------------------------------------------------------------------------

/// "APP STORE // FIRMWARE DOWNLOAD": three upgrade cards (click or 1/2/3).
pub fn spawn_app_store(mut commands: Commands, run: Res<RunStats>, mods: Res<PlayerModifiers>, choices: Res<UpgradeChoices>) {
    let root = commands.spawn(screen_root(AppState::UpgradeSelect, COLOR_STORE_BG)).id();
    let phone = commands
        .spawn((
            ChildOf(root),
            Node {
                width: Val::Px(1080.0),
                max_width: Val::Percent(96.0),
                padding: UiRect::all(Val::Px(22.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(8.0),
                border: UiRect::all(Val::Px(4.0)),
                border_radius: BorderRadius::all(Val::Px(26.0)),
                ..default()
            },
            BorderColor::all(Color::srgb(0.2, 0.22, 0.24)),
            BackgroundColor(Color::srgba(0.01, 0.03, 0.035, 0.97)),
        ))
        .id();
    commands.spawn((
        ChildOf(phone),
        Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, ..default() },
        children![
            text("UAC-PHONE  5G", 15.0, COLOR_SIGNAL_GREEN),
            text("||||| -55 dBm   SOS SENT", 15.0, COLOR_SIGNAL_GREEN),
        ],
    ));
    commands.spawn((ChildOf(phone), text("APP STORE // FIRMWARE DOWNLOAD", 44.0, COLOR_CYAN)));
    commands.spawn((
        ChildOf(phone),
        text(
            format!("Full 5-bar uplink. One download slot unlocked.  Next: SECTOR {} / {}", run.floor, FINAL_FLOOR),
            19.0,
            COLOR_TEXT,
        ),
    ));
    let row = commands
        .spawn((
            ChildOf(phone),
            Node { margin: UiRect::vertical(Val::Px(16.0)), column_gap: Val::Px(18.0), justify_content: JustifyContent::Center, ..default() },
        ))
        .id();
    for (i, id) in choices.0.iter().enumerate() {
        let owned = run.upgrades.iter().filter(|u| *u == id).count();
        let tag = if owned > 0 { format!("INSTALLED x{owned} - UPDATE") } else { "NEW".to_string() };
        commands.spawn((
            ChildOf(row),
            UpgradeCard(i),
            Node {
                width: Val::Px(330.0),
                min_height: Val::Px(270.0),
                padding: UiRect::all(Val::Px(16.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(10.0),
                border: UiRect::all(Val::Px(3.0)),
                border_radius: BorderRadius::all(Val::Px(16.0)),
                ..default()
            },
            BorderColor::all(Color::srgb(0.1, 0.6, 0.6)),
            BackgroundColor(COLOR_CARD),
            children![
                text(format!("[{}]", i + 1), 18.0, COLOR_DIM),
                (
                    Node {
                        width: Val::Px(84.0),
                        height: Val::Px(84.0),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border_radius: BorderRadius::all(Val::Px(18.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.05, 0.3, 0.32)),
                    children![text(id.icon(), 24.0, COLOR_CYAN)],
                ),
                text(id.name(), 24.0, COLOR_TEXT),
                (Node { max_width: Val::Px(296.0), ..default() }, children![text(id.description(), 19.0, COLOR_DIM)]),
                text(tag, 14.0, COLOR_SIGNAL_AMBER),
                (
                    Node {
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        max_width: Val::Px(298.0),
                        ..default()
                    },
                    BorderColor::all(Color::srgb(0.15, 0.55, 0.6)),
                    BackgroundColor(Color::srgb(0.01, 0.06, 0.07)),
                    children![text("NEXT RANK", 11.0, COLOR_SIGNAL_AMBER), text(next_rank_preview(*id, &mods), 14.0, COLOR_CYAN)],
                ),
                (
                    Node { padding: UiRect::axes(Val::Px(22.0), Val::Px(6.0)), border_radius: BorderRadius::all(Val::Px(10.0)), ..default() },
                    BackgroundColor(Color::srgb(0.08, 0.55, 0.5)),
                    children![text("INSTALL", 18.0, Color::WHITE)],
                ),
            ],
        ));
    }
    commands.spawn((ChildOf(phone), text(installed_list(&run), 16.0, COLOR_DIM)));
    commands.spawn((ChildOf(phone), text("CLICK A CARD OR PRESS 1 / 2 / 3", 16.0, COLOR_SIGNAL_AMBER)));
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
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    let mut chosen = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3].iter().position(|k| keys.just_pressed(*k));
    for (node, tf, card, mut bg, mut border) in &mut cards {
        let hovered = cursor.is_some_and(|c| node.contains_point(*tf, c));
        bg.0 = if hovered { COLOR_CARD_HOVER } else { COLOR_CARD };
        *border = BorderColor::all(if hovered { COLOR_CYAN } else { Color::srgb(0.1, 0.6, 0.6) });
        if hovered && mouse.just_released(MouseButton::Left) {
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
        "Floor {}  |  Kills {}  |  Jammers destroyed {}  |  Time {:02}:{:02}",
        run.floor,
        run.kills,
        run.jammers_destroyed,
        t / 60,
        t % 60
    )
}

fn spawn_game_over(mut commands: Commands, run: Res<RunStats>) {
    commands.spawn((
        screen_root(AppState::GameOver, Color::srgba(0.12, 0.0, 0.0, 0.9)),
        children![
            text("NO SERVICE", 80.0, COLOR_SIGNAL_RED),
            text("Connection lost. The bunker keeps your body.", 22.0, COLOR_TEXT),
            text(summary(&run), 20.0, COLOR_DIM),
            (Node { max_width: Val::Px(900.0), ..default() }, children![text(installed_list(&run), 17.0, COLOR_SIGNAL_AMBER)]),
            button("MAIN MENU  [ENTER]", MenuAction::BackToMenu),
        ],
    ));
}

fn spawn_victory(mut commands: Commands, run: Res<RunStats>) {
    commands.spawn((
        screen_root(AppState::Victory, Color::srgba(0.0, 0.06, 0.05, 0.92)),
        children![
            text("FULL SIGNAL", 80.0, COLOR_SIGNAL_GREEN),
            text("SOS received. Orbital extraction confirmed. Bunker 7 sealed.", 22.0, COLOR_TEXT),
            text(summary(&run), 20.0, COLOR_DIM),
            (Node { max_width: Val::Px(900.0), ..default() }, children![text(installed_list(&run), 17.0, COLOR_CYAN)]),
            button("MAIN MENU  [ENTER]", MenuAction::BackToMenu),
        ],
    ));
}

fn perform(action: MenuAction, run: &mut RunStats, mods: &mut PlayerModifiers, time: &Time, next: &mut NextState<AppState>) {
    match action {
        MenuAction::StartRun => start_run(run, mods, next, time.elapsed().as_nanos() as u64 ^ 0xA5A5_5A5A_1234_5678),
        MenuAction::Continue => next.set(AppState::Playing),
        MenuAction::BackToMenu => next.set(AppState::MainMenu),
    }
}

#[allow(clippy::too_many_arguments)]
fn menu_buttons(
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut buttons: Query<(&ComputedNode, &UiGlobalTransform, &MenuAction, &mut BackgroundColor), With<MenuButton>>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    time: Res<Time>,
    mut next: ResMut<NextState<AppState>>,
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    for (node, tf, action, mut bg) in &mut buttons {
        let hovered = cursor.is_some_and(|c| node.contains_point(*tf, c));
        bg.0 = if hovered { Color::srgb(0.55, 0.06, 0.03) } else { Color::srgb(0.25, 0.03, 0.02) };
        if hovered && mouse.just_released(MouseButton::Left) {
            perform(*action, &mut run, &mut mods, &time, &mut next);
            return;
        }
    }
}

fn menu_keys(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<&MenuAction, With<MenuButton>>,
    mut run: ResMut<RunStats>,
    mut mods: ResMut<PlayerModifiers>,
    time: Res<Time>,
    mut next: ResMut<NextState<AppState>>,
) {
    if (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter))
        && let Some(action) = buttons.iter().next()
    {
        perform(*action, &mut run, &mut mods, &time, &mut next);
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
// - bottom-center: vitals panel (HP / ARMOR / STAMINA bars)
// - bottom-right: weapon & ammo deck (active card, shell pips or plasma bar,
//   slot chips, 5G overclock pill)
// - edges: subtle dark-red Dead-Zone frame (no scanlines across the view)

const PANEL_BG: Color = Color::srgba(0.02, 0.03, 0.04, 0.72);
const PANEL_BORDER: Color = Color::srgba(0.55, 0.6, 0.65, 0.28);
const TRACK_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.65);
const TRACK_BORDER: Color = Color::srgba(1.0, 1.0, 1.0, 0.18);
const COLOR_HP_GREEN: Color = Color::srgb(0.16, 0.9, 0.46);
const COLOR_ARMOR: Color = Color::srgb(0.46, 0.74, 0.88);
const COLOR_GOLD: Color = Color::srgb(1.0, 0.8, 0.28);
const COLOR_SHOTGUN: Color = Color::srgb(1.0, 0.56, 0.14);
const COLOR_SHELL: Color = Color::srgb(0.82, 0.13, 0.09);
const COLOR_BRASS: Color = Color::srgb(0.95, 0.72, 0.3);
const COLOR_PIP_OFF: Color = Color::srgba(1.0, 1.0, 1.0, 0.07);
const COLOR_PIP_OFF_BORDER: Color = Color::srgba(1.0, 1.0, 1.0, 0.16);

const TOAST_SLOTS: u8 = 5;
/// Shell pips shown for the shotgun magazine.
pub const SHELL_PIPS: u8 = 10;
const DECK_WIDTH: f32 = 272.0;
const VITALS_WIDTH: f32 = 300.0;

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

/// The 5G overclock damage bonus is live.
pub fn overclock_active(bars: u8, bonus: f32) -> bool {
    bars >= 4 && bonus > 0.0
}

fn weapon_accent(w: Weapon) -> Color {
    match w {
        Weapon::Shotgun => COLOR_SHOTGUN,
        Weapon::Plasma => COLOR_CYAN,
    }
}

fn weapon_title(w: Weapon) -> &'static str {
    match w {
        Weapon::Shotgun => "SHOTGUN",
        Weapon::Plasma => "PLASMA REPEATER",
    }
}

fn ammo(p: &Player, w: Weapon) -> (u32, u32) {
    match w {
        Weapon::Shotgun => (p.ammo_shells, p.max_shells),
        Weapon::Plasma => (p.ammo_plasma, p.max_plasma),
    }
}

/// Every HUD text element, updated by one query.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum HudText {
    Floor,
    Stats,
    Status,
    Prompt,
    Alert,
    Toast(u8),
    Hitmarker,
    UplinkLabel,
    HpValue,
    ArmorValue,
    StaminaValue,
    WeaponName,
    AmmoCount,
    AmmoMax,
    Slot(u8),
}

/// Every HUD box (background/size/border driven), updated by one query.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum HudBox {
    DamageFlash,
    DeadZoneFrame,
    UplinkPanel,
    UplinkFill,
    AlertPanel,
    ToastChip(u8),
    HpFill,
    ArmorFill,
    StaminaFill,
    ShellRow,
    ShellPip(u8),
    PlasmaTrack,
    PlasmaFill,
    WeaponCard,
    SlotChip(u8),
    OverclockPill,
}

fn abs_full() -> Node {
    Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }
}

/// Dark translucent backing panel with a thin border and rounded corners.
fn panel(node: Node) -> impl Bundle {
    (
        Node {
            padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
            ..node
        },
        BorderColor::all(PANEL_BORDER),
        BackgroundColor(PANEL_BG),
    )
}

#[allow(clippy::too_many_arguments)]
/// Labelled horizontal bar: `LABEL ......... value` over a framed track.
fn spawn_bar(commands: &mut Commands, parent: Entity, label: &str, label_color: Color, value: HudText, fill: HudBox, height: f32, color: Color) {
    let block = commands
        .spawn((ChildOf(parent), Node { width: Val::Percent(100.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }))
        .id();
    commands.spawn((
        ChildOf(block),
        Node { width: Val::Percent(100.0), justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::FlexEnd, ..default() },
        children![text(label, 13.0, label_color), (value, text("", 15.0, COLOR_TEXT))],
    ));
    let track = commands
        .spawn((
            ChildOf(block),
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(height),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(3.0)),
                padding: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(TRACK_BORDER),
            BackgroundColor(TRACK_BG),
        ))
        .id();
    commands.spawn((
        ChildOf(track),
        fill,
        Node { width: Val::Percent(100.0), height: Val::Percent(100.0), border_radius: BorderRadius::all(Val::Px(2.0)), ..default() },
        BackgroundColor(color),
    ));
}

fn spawn_hud(mut commands: Commands) {
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

    // Crosshair + hitmarker.
    commands.spawn((
        ChildOf(root),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            margin: UiRect { left: Val::Px(-7.0), top: Val::Px(-12.0), ..default() },
            ..default()
        },
        children![text("+", 24.0, Color::srgba(1.0, 0.9, 0.8, 0.8))],
    ));
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

    // Top-left: floor + kills / jammers.
    commands.spawn((
        ChildOf(root),
        panel(Node {
            position_type: PositionType::Absolute,
            left: Val::Px(14.0),
            top: Val::Px(12.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            ..default()
        }),
        children![
            (HudText::Floor, text("FLOOR 1/5", 20.0, COLOR_TEXT)),
            (HudText::Stats, text("KILLS 0  |  JAMMERS 0", 14.0, COLOR_DIM)),
        ],
    ));

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
    let status = commands
        .spawn((
            ChildOf(top),
            panel(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(6.0), ..default() }),
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
    commands.spawn((ChildOf(uplink), HudText::UplinkLabel, text("UPLOADING SOS 0%", 20.0, COLOR_CYAN)));
    let track = commands
        .spawn((
            ChildOf(uplink),
            Node {
                width: Val::Px(440.0),
                height: Val::Px(16.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                padding: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(Color::srgb(0.3, 0.8, 0.85)),
            BackgroundColor(TRACK_BG),
        ))
        .id();
    commands.spawn((
        ChildOf(track),
        HudBox::UplinkFill,
        Node { width: Val::Percent(0.0), height: Val::Percent(100.0), border_radius: BorderRadius::all(Val::Px(2.0)), ..default() },
        BackgroundColor(COLOR_CYAN),
    ));
    commands.spawn((
        ChildOf(top),
        HudBox::AlertPanel,
        panel(Node { display: Display::None, ..default() }),
        children![(HudText::Alert, text("", 30.0, COLOR_SIGNAL_AMBER))],
    ));

    // Bottom-center prompt (above the vitals panel).
    commands.spawn((
        ChildOf(root),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            bottom: Val::Px(150.0),
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
            panel(Node { display: Display::None, padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)), ..default() }),
            children![(HudText::Toast(i), text("", 15.0, COLOR_TEXT))],
        ));
    }

    // Bottom-center: vitals panel.
    let vitals = commands
        .spawn((
            ChildOf(root),
            panel(Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                bottom: Val::Px(14.0),
                width: Val::Px(VITALS_WIDTH),
                margin: UiRect::left(Val::Px(-VITALS_WIDTH / 2.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(5.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                ..default()
            }),
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
                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BorderColor::all(COLOR_SHOTGUN),
            BackgroundColor(Color::srgba(0.02, 0.03, 0.04, 0.8)),
        ))
        .id();
    commands.spawn((
        ChildOf(card),
        Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() },
        children![
            (HudText::WeaponName, text("SHOTGUN", 15.0, COLOR_SHOTGUN)),
            (
                HudBox::OverclockPill,
                Node {
                    display: Display::None,
                    padding: UiRect::axes(Val::Px(6.0), Val::Px(1.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BorderColor::all(COLOR_SIGNAL_GREEN),
                BackgroundColor(Color::srgba(0.05, 0.4, 0.3, 0.55)),
                children![text("5G OVERCLOCK", 11.0, COLOR_SIGNAL_GREEN)],
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
                border_radius: BorderRadius::all(Val::Px(3.0)),
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
            Node {
                display: Display::None,
                width: Val::Percent(100.0),
                height: Val::Px(14.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(3.0)),
                padding: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(COLOR_CYAN.with_alpha(0.7)),
            BackgroundColor(TRACK_BG),
        ))
        .id();
    commands.spawn((
        ChildOf(ptrack),
        HudBox::PlasmaFill,
        Node { width: Val::Percent(100.0), height: Val::Percent(100.0), border_radius: BorderRadius::all(Val::Px(2.0)), ..default() },
        BackgroundColor(COLOR_CYAN),
    ));
    for n in 1..10 {
        commands.spawn((
            ChildOf(ptrack),
            Node { position_type: PositionType::Absolute, left: Val::Percent(n as f32 * 10.0), top: Val::Px(0.0), width: Val::Px(1.0), height: Val::Percent(100.0), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        ));
    }
    let slots = commands.spawn((ChildOf(deck), Node { column_gap: Val::Px(6.0), ..default() })).id();
    for i in 0..2u8 {
        commands.spawn((
            ChildOf(slots),
            HudBox::SlotChip(i),
            Node {
                flex_grow: 1.0,
                flex_basis: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BorderColor::all(PANEL_BORDER),
            BackgroundColor(PANEL_BG),
            children![(HudText::Slot(i), text("", 13.0, COLOR_DIM))],
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn update_hud(
    signal: Res<SignalState>,
    run: Res<RunStats>,
    time: Res<Time>,
    feed: Res<HudFeed>,
    ws: Res<WeaponState>,
    ext: Res<ExtractionState>,
    players: Query<&Player>,
    mut texts: Query<(&HudText, &mut Text, &mut TextColor)>,
) {
    let t = time.elapsed_secs();
    let Ok(p) = players.single() else { return };
    let accent = weapon_accent(ws.current);
    let (cur, max) = ammo(p, ws.current);
    for (slot, mut txt, mut col) in &mut texts {
        let (s, c): (String, Color) = match *slot {
            HudText::Floor => (format!("FLOOR {}/{}", run.floor, FINAL_FLOOR), COLOR_TEXT),
            HudText::Stats => (format!("KILLS {}  |  JAMMERS {}", run.kills, run.jammers_destroyed), COLOR_DIM),
            HudText::Status => match *ext {
                ExtractionState::Uploading { .. } => ("DEMON RUSH - HOLD THE SIGNAL".into(), COLOR_SIGNAL_RED),
                ExtractionState::Extracting { .. } | ExtractionState::Complete => ("ORBITAL EXTRACTION IN PROGRESS".into(), COLOR_CYAN),
                _ if signal.near_tower => ("5-BAR SIGNAL - EXTRACTION TOWER IN RANGE".into(), COLOR_SIGNAL_GREEN),
                _ if signal.in_dead_zone => ("NO SERVICE - DEAD ZONE".into(), COLOR_SIGNAL_RED),
                _ => ("SEARCH FOR 5-BAR SIGNAL TOWER".into(), COLOR_SIGNAL_AMBER),
            },
            HudText::Prompt => match *ext {
                ExtractionState::Searching { hold } if signal.near_tower => {
                    let a = 0.6 + 0.4 * (t * 5.0).sin();
                    let extra = if hold > 0.0 { format!("  ({:.0}%)", hold / UPLINK_HOLD_SECS * 100.0) } else { String::new() };
                    (format!("PRESS [E] TO TRIGGER SOS UPLINK{extra}"), COLOR_SIGNAL_GREEN.with_alpha(a))
                }
                _ => (String::new(), Color::NONE),
            },
            HudText::Alert => match &feed.alert {
                Some(a) => (a.text.clone(), a.color.with_alpha(if (t * 8.0).sin() > -0.3 { 1.0 } else { 0.35 })),
                None => (String::new(), Color::NONE),
            },
            HudText::UplinkLabel => match *ext {
                ExtractionState::Uploading { progress, jammed: true, .. } => {
                    (format!("UPLINK JAMMED! KILL THE JAMMER FIEND  {:.0}%", progress * 100.0), COLOR_SIGNAL_RED.with_alpha(if (t * 10.0).sin() > 0.0 { 1.0 } else { 0.4 }))
                }
                ExtractionState::Uploading { progress, in_ring: false, .. } => {
                    (format!("RETURN TO SIGNAL RING!  {:.0}%", progress * 100.0), COLOR_SIGNAL_AMBER)
                }
                ExtractionState::Uploading { progress, .. } => (format!("UPLOADING SOS {:.0}%", progress * 100.0), COLOR_CYAN),
                _ => (String::new(), COLOR_CYAN),
            },
            HudText::HpValue => {
                let c = if hp_band(fraction(p.hp, p.max_hp)) == HpBand::Critical { hp_color(0.0, t) } else { COLOR_TEXT };
                (format!("{}/{}", p.hp.ceil().max(0.0) as i32, p.max_hp as i32), c)
            }
            HudText::ArmorValue => (format!("{}/{}", p.armor.ceil().max(0.0) as i32, p.max_armor as i32), COLOR_ARMOR),
            HudText::StaminaValue => {
                if p.dash_ready() {
                    ("READY".into(), COLOR_GOLD)
                } else {
                    (format!("{:.1}s", p.dash_cooldown), COLOR_CYAN)
                }
            }
            HudText::WeaponName => (weapon_title(ws.current).into(), accent),
            HudText::AmmoCount => (cur.to_string(), if cur == 0 { COLOR_SIGNAL_RED } else { COLOR_TEXT }),
            HudText::AmmoMax => (format!("/ {max}"), COLOR_DIM),
            HudText::Slot(i) => {
                let w = if i == 0 { Weapon::Shotgun } else { Weapon::Plasma };
                let name = if i == 0 { "SHOTGUN" } else { "PLASMA" };
                let (n, _) = ammo(p, w);
                (format!("[{}] {name} {n}", i + 1), if ws.current == w { weapon_accent(w) } else { COLOR_DIM })
            }
            HudText::Toast(i) => match feed.toasts.iter().rev().nth(i as usize) {
                Some(toast) => (toast.text.clone(), toast.color.with_alpha((toast.timer / 0.6).min(1.0))),
                None => (String::new(), Color::NONE),
            },
            HudText::Hitmarker => {
                let a = (feed.hitmarker / 0.18).clamp(0.0, 1.0);
                ("X".into(), if feed.hit_kill { COLOR_SIGNAL_RED.with_alpha(a) } else { Color::srgba(1.0, 1.0, 1.0, a) })
            }
        };
        if txt.0 != s {
            txt.0 = s;
        }
        col.0 = c;
    }
}

fn set_display(node: &mut Node, show: bool) {
    let want = if show { Display::Flex } else { Display::None };
    if node.display != want {
        node.display = want;
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
    players: Query<&Player>,
    mut boxes: Query<(&HudBox, &mut Node, &mut BackgroundColor, Option<&mut BorderColor>)>,
) {
    let t = time.elapsed_secs();
    let Ok(p) = players.single() else { return };
    let accent = weapon_accent(ws.current);
    let shotgun = ws.current == Weapon::Shotgun;
    let (cur, max) = ammo(p, ws.current);
    let lit = lit_pips(p.ammo_shells, p.max_shells, SHELL_PIPS);
    let stamina = stamina_fill(p.dash_cooldown, DASH_COOLDOWN * mods.dash_cooldown_mult);
    let toast_alpha = |i: u8| feed.toasts.iter().rev().nth(i as usize).map(|toast| (toast.timer / 0.6).min(1.0));
    for (b, mut node, mut bg, border) in &mut boxes {
        let set_border = |c: Color| {
            if let Some(mut bc) = border {
                *bc = BorderColor::all(c);
            }
        };
        match *b {
            HudBox::DamageFlash => bg.0 = Color::srgba(0.8, 0.0, 0.0, (0.45 - p.since_damage).max(0.0)),
            HudBox::DeadZoneFrame => {
                if signal.in_dead_zone {
                    let strength = if signal.bars == 0 { 1.0 } else { 0.6 };
                    set_border(Color::srgba(0.55, 0.03, 0.03, strength * (0.4 + 0.2 * (t * 3.0).sin())));
                    bg.0 = Color::srgba(0.4, 0.0, 0.0, 0.03 * strength);
                } else {
                    set_border(Color::NONE);
                    bg.0 = Color::NONE;
                }
            }
            HudBox::UplinkPanel => set_display(&mut node, matches!(*ext, ExtractionState::Uploading { .. })),
            HudBox::UplinkFill => {
                if let ExtractionState::Uploading { progress, jammed, in_ring, .. } = *ext {
                    node.width = Val::Percent(progress * 100.0);
                    bg.0 = if jammed {
                        COLOR_SIGNAL_RED.with_alpha(if (t * 10.0).sin() > 0.0 { 1.0 } else { 0.4 })
                    } else if !in_ring {
                        COLOR_SIGNAL_AMBER
                    } else {
                        COLOR_CYAN
                    };
                }
            }
            HudBox::AlertPanel => set_display(&mut node, feed.alert.is_some()),
            HudBox::ToastChip(i) => {
                let a = toast_alpha(i);
                set_display(&mut node, a.is_some());
                let a = a.unwrap_or(0.0);
                bg.0 = PANEL_BG.with_alpha(0.72 * a);
                set_border(PANEL_BORDER.with_alpha(0.28 * a));
            }
            HudBox::HpFill => {
                let f = fraction(p.hp, p.max_hp);
                node.width = Val::Percent(f * 100.0);
                bg.0 = hp_color(f, t);
            }
            HudBox::ArmorFill => node.width = Val::Percent(fraction(p.armor, p.max_armor) * 100.0),
            HudBox::StaminaFill => {
                node.width = Val::Percent(stamina * 100.0);
                bg.0 = if stamina >= 1.0 { COLOR_GOLD } else { COLOR_CYAN.with_alpha(0.75) };
            }
            HudBox::ShellRow => set_display(&mut node, shotgun),
            HudBox::ShellPip(i) => {
                let on = i < lit;
                bg.0 = if on { COLOR_SHELL } else { COLOR_PIP_OFF };
                set_border(if on { COLOR_BRASS } else { COLOR_PIP_OFF_BORDER });
            }
            HudBox::PlasmaTrack => set_display(&mut node, !shotgun),
            HudBox::PlasmaFill => {
                node.width = Val::Percent(fraction(cur as f32, max as f32) * 100.0);
                bg.0 = COLOR_CYAN.with_alpha(0.8 + 0.2 * (t * 4.0).sin());
            }
            HudBox::WeaponCard => set_border(accent),
            HudBox::SlotChip(i) => {
                let active = (i == 0) == shotgun;
                let w = if i == 0 { Weapon::Shotgun } else { Weapon::Plasma };
                set_border(if active { weapon_accent(w) } else { PANEL_BORDER });
                bg.0 = if active { weapon_accent(w).with_alpha(0.16) } else { PANEL_BG };
            }
            HudBox::OverclockPill => set_display(&mut node, overclock_active(signal.bars, mods.overclock_bonus)),
        }
    }
}

/// F9: debug-complete the current floor (exercises the floor transition flow).
fn debug_skip_floor(keys: Res<ButtonInput<KeyCode>>, mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>) {
    if keys.just_pressed(KeyCode::F9) {
        complete_floor(&mut run, &mut next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn overclock_needs_bars_and_upgrade() {
        assert!(overclock_active(4, 0.25));
        assert!(!overclock_active(3, 0.25));
        assert!(!overclock_active(5, 0.0));
    }

    #[test]
    fn weapon_titles_and_accents_differ() {
        assert_eq!(weapon_title(Weapon::Plasma), "PLASMA REPEATER");
        assert_ne!(weapon_accent(Weapon::Shotgun), weapon_accent(Weapon::Plasma));
    }
}
