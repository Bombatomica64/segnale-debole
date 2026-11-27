//! Milestone 37: the on-screen touch HUD (drawing only; `touch.rs` routes).
//!
//! Industrial-punk plates in the `ui_kit` language: translucent ink fills,
//! hard square corners, a thick accent rail on the left / bottom edges, press
//! flashes in the accent color, and live cooldown fills that drain from the
//! top. The floating thumbstick is the one round element (a ring, the 85%
//! auto-bhop rim and a puck) and rests dimmed until a thumb lands. Everything
//! is placed from [`TouchInput::layout`] (logical px) divided by [`UiScale`],
//! scaled by `touch_button_scale` and faded by `touch_opacity`. The layout
//! keeps every plate off the reticle, the vitals / velocity cluster, the boss
//! bar and the style meter (which `hud.rs` moves aside in touch mode).

use bevy::prelude::*;
use bevy::text::FontSize;

use crate::apps::{overload_cooldown, overload_gate_bars, ping_cooldown, PhoneApps, OVERLOAD_MIN_BARS, PING_MIN_BARS};
use crate::gamepad::InputMode;
use crate::meta::{live_settings, ProfileStore};
use crate::phone_combat::{flash_cooldown, FLASH_MIN_BARS, PUNCH_COOLDOWN};
use crate::player::{Player, DASH_COOLDOWN};
use crate::signal::SignalState;
use crate::state::{AppState, GameSet, PlayerModifiers};
use crate::touch::{overlay_visible, TouchButton, TouchInput, AUTO_BHOP_RIM};
use crate::ui_kit::{plate_rect, ACID_GREEN, ARC_CYAN, ASH_DIM, BONE_TEXT, HAZARD_YELLOW, INK_BLACK, PUNK_RED, STEEL_LIGHT};

/// Laid-out parts of the touch HUD.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchHudPart {
    Root,
    StickBase,
    /// The 85% auto-bhop rim guide inside the base.
    StickRim,
    StickPuck,
    Plate(TouchButton),
    /// Cooldown fill of a plate (drains from the top).
    Fill(TouchButton),
}

/// Texts of the touch HUD.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchHudText {
    Label(TouchButton),
    /// Small status line (cooldown seconds, bar requirement, READY).
    Readout(TouchButton),
}

/// Live game numbers the plates show.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlateContext {
    pub bars: u8,
    /// Remaining cooldown fractions `[0, 1]` and seconds.
    pub dash: (f32, f32),
    pub punch: (f32, f32),
    pub ping: (f32, f32),
    pub overload: (f32, f32),
    pub flash: (f32, f32),
    /// The app gates (enough bars, Emergency Callback / Missed Call aware).
    pub ping_gate: bool,
    pub overload_gate: bool,
    pub flash_gate: bool,
    pub auto_fire: bool,
    pub auto_bhop_latched: bool,
    /// The stick sits on its auto-bhop rim.
    pub bhop_rim: bool,
}

/// How one plate draws this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PlateLook {
    pub accent: Color,
    /// Usable right now (full-strength rail and label).
    pub ready: bool,
    /// Glows (5G Overload ready, a latched AUTO-BHOP).
    pub glow: bool,
    /// Remaining cooldown fraction (drawn as the fill).
    pub cooldown: f32,
    pub readout: String,
}

fn cooldown_text(cd: (f32, f32)) -> Option<String> {
    (cd.1 > 0.05).then(|| format!("{:.1}", cd.1))
}

/// A plate's look from the game state (pure; unit-tested).
pub fn plate_look(b: TouchButton, c: &PlateContext) -> PlateLook {
    let gated = |gate: bool, need: u8, cd: (f32, f32)| -> (bool, f32, String) {
        if !gate {
            (false, 0.0, format!("{need} BAR{}", if need == 1 { "" } else { "S" }))
        } else if let Some(t) = cooldown_text(cd) {
            (false, cd.0, t)
        } else {
            (true, 0.0, String::new())
        }
    };
    let (accent, ready, glow, cooldown, readout) = match b {
        TouchButton::Fire => (PUNK_RED, true, false, 0.0, if c.auto_fire { "AUTO".to_string() } else { String::new() }),
        TouchButton::Jump => (ARC_CYAN, true, c.bhop_rim, 0.0, if c.bhop_rim { "BHOP".to_string() } else { String::new() }),
        TouchButton::Dash => {
            let t = cooldown_text(c.dash);
            (HAZARD_YELLOW, t.is_none(), false, if t.is_some() { c.dash.0 } else { 0.0 }, t.unwrap_or_default())
        }
        TouchButton::Punch => {
            let t = cooldown_text(c.punch);
            (BONE_TEXT, t.is_none(), false, if t.is_some() { c.punch.0 } else { 0.0 }, t.unwrap_or_default())
        }
        TouchButton::Ping => {
            let (r, cd, t) = gated(c.ping_gate, PING_MIN_BARS, c.ping);
            (ARC_CYAN, r, false, cd, t)
        }
        TouchButton::Overload => {
            let (r, cd, t) = gated(c.overload_gate, OVERLOAD_MIN_BARS, c.overload);
            (ACID_GREEN, r, r, cd, if r { "READY".to_string() } else { t })
        }
        TouchButton::Flash => {
            let (r, cd, t) = gated(c.flash_gate, FLASH_MIN_BARS, c.flash);
            (BONE_TEXT, r, false, cd, t)
        }
        TouchButton::Swap => (STEEL_LIGHT, true, false, 0.0, String::new()),
        TouchButton::Sos => (ACID_GREEN, true, true, 0.0, "UPLINK".to_string()),
        TouchButton::Pause | TouchButton::Reset => (BONE_TEXT, true, false, 0.0, String::new()),
        TouchButton::AutoBhop => (ACID_GREEN, c.auto_bhop_latched, c.auto_bhop_latched, 0.0, String::new()),
        TouchButton::Slot(_) => (STEEL_LIGHT, true, false, 0.0, String::new()),
    };
    PlateLook { accent, ready, glow, cooldown: cooldown.clamp(0.0, 1.0), readout }
}

/// `(fill, border, label)` colors of a plate at `opacity`.
pub fn plate_colors(look: &PlateLook, pressed: bool, opacity: f32) -> (Color, Color, Color) {
    let op = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 0.7 };
    if pressed {
        return (look.accent.with_alpha(0.78), BONE_TEXT, INK_BLACK);
    }
    let fill = if look.glow { look.accent.mix(&INK_BLACK, 0.7).with_alpha(0.7 * op) } else { INK_BLACK.with_alpha(0.55 * op) };
    let border = if look.ready { look.accent.with_alpha(0.95 * op) } else { ASH_DIM.with_alpha(0.7 * op) };
    let label = if look.glow { look.accent.with_alpha(op.max(0.6)) } else if look.ready { BONE_TEXT.with_alpha(op.max(0.5)) } else { ASH_DIM.with_alpha(op.max(0.45)) };
    (fill, border, label)
}

fn abs_node(width: f32, height: f32) -> Node {
    Node { position_type: PositionType::Absolute, width: Val::Px(width), height: Val::Px(height), ..default() }
}

/// Build the (hidden) touch HUD with every plate; [`update_touch_hud`] places
/// and shows what applies.
fn spawn_touch_hud(mut commands: Commands) {
    let root = commands
        .spawn((
            DespawnOnExit(AppState::Playing),
            TouchHudPart::Root,
            Node { display: Display::None, position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
            GlobalZIndex(3),
        ))
        .id();
    let base = commands
        .spawn((
            ChildOf(root),
            TouchHudPart::StickBase,
            Node { border: UiRect::all(Val::Px(2.0)), border_radius: BorderRadius::MAX, ..abs_node(100.0, 100.0) },
            BorderColor::all(ARC_CYAN.with_alpha(0.3)),
            BackgroundColor(INK_BLACK.with_alpha(0.25)),
        ))
        .id();
    commands.spawn((
        ChildOf(base),
        TouchHudPart::StickRim,
        Node { border: UiRect::all(Val::Px(1.0)), border_radius: BorderRadius::MAX, ..abs_node(85.0, 85.0) },
        BorderColor::all(STEEL_LIGHT.with_alpha(0.25)),
        BackgroundColor(Color::NONE),
    ));
    commands.spawn((
        ChildOf(root),
        TouchHudPart::StickPuck,
        Node { border: UiRect::all(Val::Px(2.0)), border_radius: BorderRadius::MAX, ..abs_node(40.0, 40.0) },
        BorderColor::all(BONE_TEXT.with_alpha(0.4)),
        BackgroundColor(ASH_DIM.with_alpha(0.3)),
    ));
    for b in TouchButton::HUD {
        let plate = commands
            .spawn((
                ChildOf(root),
                TouchHudPart::Plate(b),
                Node {
                    display: Display::None,
                    border: plate_rect((3.0, 1.0, 1.0, 3.0)),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    overflow: Overflow::clip(),
                    ..abs_node(60.0, 60.0)
                },
                BorderColor::all(ASH_DIM),
                BackgroundColor(INK_BLACK.with_alpha(0.5)),
            ))
            .id();
        commands.spawn((
            ChildOf(plate),
            TouchHudPart::Fill(b),
            Node { left: Val::Px(0.0), top: Val::Px(0.0), width: Val::Percent(100.0), height: Val::Percent(0.0), ..abs_node(0.0, 0.0) },
            BackgroundColor(INK_BLACK.with_alpha(0.6)),
        ));
        commands.spawn((
            ChildOf(plate),
            TouchHudText::Label(b),
            Text::new(b.label()),
            TextFont::from_font_size(14.0),
            TextColor(BONE_TEXT),
            TextLayout::no_wrap(),
            TextShadow { offset: Vec2::splat(1.5), color: Color::BLACK.with_alpha(0.8) },
        ));
        commands.spawn((
            ChildOf(plate),
            TouchHudText::Readout(b),
            Text::new(""),
            TextFont::from_font_size(9.0),
            TextColor(ASH_DIM),
            TextLayout::no_wrap(),
        ));
    }
}

fn set_px(v: &mut Val, px: f32) {
    let want = Val::Px(px.round());
    if *v != want {
        *v = want;
    }
}

fn set_display(n: &mut Node, show: bool) {
    let want = if show { Display::Flex } else { Display::None };
    if n.display != want {
        n.display = want;
    }
}

fn place(n: &mut Node, center: Vec2, half: Vec2, k: f32) {
    set_px(&mut n.left, (center.x - half.x) / k);
    set_px(&mut n.top, (center.y - half.y) / k);
    set_px(&mut n.width, half.x * 2.0 / k);
    set_px(&mut n.height, half.y * 2.0 / k);
}

/// The live plate numbers from the game resources.
fn plate_context(player: Option<&Player>, apps: Option<&PhoneApps>, mods: Option<&PlayerModifiers>, signal: Option<&SignalState>, touch: &TouchInput) -> PlateContext {
    let frac = |cd: f32, max: f32| if max > 0.0 { (cd / max).clamp(0.0, 1.0) } else { 0.0 };
    let default_mods = PlayerModifiers::default();
    let mods = mods.unwrap_or(&default_mods);
    let bars = signal.map_or(0, |s| s.bars);
    let mut c = PlateContext {
        bars,
        auto_fire: touch.auto_fire,
        auto_bhop_latched: touch.frame.auto_bhop_latched,
        bhop_rim: touch.frame.stick.is_some() && touch.frame.stick_raw.length() >= AUTO_BHOP_RIM && touch.frame.jump,
        ..default()
    };
    if let Some(p) = player {
        c.dash = (frac(p.dash_cooldown, DASH_COOLDOWN * mods.dash_cooldown_mult), p.dash_cooldown);
    }
    if let Some(a) = apps {
        c.punch = (frac(a.punch_cooldown, PUNCH_COOLDOWN), a.punch_cooldown);
        c.ping = (frac(a.ping_cooldown, ping_cooldown(mods)), a.ping_cooldown);
        c.overload = (frac(a.overload_cooldown, overload_cooldown(mods)), a.overload_cooldown);
        c.flash = (frac(a.flash_cooldown, flash_cooldown(mods)), a.flash_cooldown);
        c.overload_gate = overload_gate_bars(bars, a.callback_timer) >= OVERLOAD_MIN_BARS;
    }
    c.ping_gate = bars >= PING_MIN_BARS || (bars == 0 && mods.missed_call);
    c.flash_gate = bars >= FLASH_MIN_BARS;
    c
}

/// Place, show and restyle the touch HUD every frame.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_touch_hud(
    touch: Res<TouchInput>,
    (mode, store, state): (Option<Res<InputMode>>, Option<Res<ProfileStore>>, Res<State<AppState>>),
    ui_scale: Option<Res<UiScale>>,
    (apps, mods, signal): (Option<Res<PhoneApps>>, Option<Res<PlayerModifiers>>, Option<Res<SignalState>>),
    players: Query<&Player>,
    mut parts: Query<(&TouchHudPart, &mut Node, &mut BackgroundColor, &mut BorderColor)>,
    mut texts: Query<(&TouchHudText, &mut Text, &mut TextColor, &mut TextFont)>,
) {
    let settings = live_settings(store.as_deref());
    let on = overlay_visible(*state.get(), settings.touch_mode, mode.map_or(InputMode::KeyboardMouse, |m| *m));
    let k = ui_scale.map_or(1.0, |s| s.0).max(0.01);
    let op = settings.touch_opacity;
    let layout = &touch.layout;
    let frame = &touch.frame;
    let ctx = plate_context(players.single().ok(), apps.as_deref(), mods.as_deref(), signal.as_deref(), &touch);
    let stick_active = frame.stick.is_some();
    let (base_c, puck_c) = frame.stick.unwrap_or((layout.stick_rest, layout.stick_rest));
    let r = layout.stick_radius;
    for (part, mut node, mut bg, mut border) in &mut parts {
        match *part {
            TouchHudPart::Root => set_display(&mut node, on),
            TouchHudPart::StickBase => {
                place(&mut node, base_c, Vec2::splat(r), k);
                let (b, f) = if stick_active { (ARC_CYAN.with_alpha(0.9 * op.max(0.6)), INK_BLACK.with_alpha(0.35 * op)) } else { (ARC_CYAN.with_alpha(0.35 * op), INK_BLACK.with_alpha(0.2 * op)) };
                if bg.0 != f {
                    bg.0 = f;
                }
                if border.top != b {
                    *border = BorderColor::all(b);
                }
            }
            TouchHudPart::StickRim => {
                // Relative to the base (its child): centered, 85% of its size.
                let side = r * 2.0 * AUTO_BHOP_RIM / k;
                let off = (r * 2.0 / k - side) * 0.5 - 2.0;
                set_px(&mut node.left, off);
                set_px(&mut node.top, off);
                set_px(&mut node.width, side);
                set_px(&mut node.height, side);
                let b = if ctx.bhop_rim { ACID_GREEN.with_alpha(0.9) } else { STEEL_LIGHT.with_alpha(0.25 * op) };
                if border.top != b {
                    *border = BorderColor::all(b);
                }
            }
            TouchHudPart::StickPuck => {
                place(&mut node, puck_c, Vec2::splat(r * 0.42), k);
                let (f, b) = if stick_active { (BONE_TEXT.with_alpha(0.55), BONE_TEXT.with_alpha(0.95)) } else { (ASH_DIM.with_alpha(0.25 * op), BONE_TEXT.with_alpha(0.35 * op)) };
                if bg.0 != f {
                    bg.0 = f;
                }
                if border.top != b {
                    *border = BorderColor::all(b);
                }
            }
            TouchHudPart::Plate(b) => {
                let Some(rect) = layout.button(b) else {
                    set_display(&mut node, false);
                    continue;
                };
                set_display(&mut node, true);
                place(&mut node, rect.center, rect.half, k);
                let look = plate_look(b, &ctx);
                let (fill, edge, _) = plate_colors(&look, frame.is_pressed(b), op);
                if bg.0 != fill {
                    bg.0 = fill;
                }
                if border.top != edge {
                    *border = BorderColor::all(edge);
                }
            }
            TouchHudPart::Fill(b) => {
                let pct = plate_look(b, &ctx).cooldown * 100.0;
                let want = Val::Percent(pct.round());
                if node.height != want {
                    node.height = want;
                }
            }
        }
    }
    for (t, mut text, mut color, mut font) in &mut texts {
        match *t {
            TouchHudText::Label(b) => {
                let look = plate_look(b, &ctx);
                let (_, _, c) = plate_colors(&look, frame.is_pressed(b), op);
                if color.0 != c {
                    color.0 = c;
                }
                let half = layout.button(b).map_or(20.0, |r| r.half.y.min(r.half.x * 0.6)) / k;
                let size = (half * 0.5).clamp(9.0, 26.0).round();
                if font.font_size != FontSize::Px(size) {
                    font.font_size = FontSize::Px(size);
                }
            }
            TouchHudText::Readout(b) => {
                let look = plate_look(b, &ctx);
                if text.0 != look.readout {
                    text.0.clone_from(&look.readout);
                }
                let c = if look.glow { look.accent } else { BONE_TEXT.with_alpha(0.8) };
                if color.0 != c {
                    color.0 = c;
                }
                let half = layout.button(b).map_or(20.0, |r| r.half.y) / k;
                let size = (half * 0.3).clamp(8.0, 15.0).round();
                if font.font_size != FontSize::Px(size) {
                    font.font_size = FontSize::Px(size);
                }
            }
        }
    }
}

/// Added by `HudPlugin` (rendering side; the router lives in `touch.rs`).
pub struct TouchHudPlugin;

impl Plugin for TouchHudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), spawn_touch_hud).add_systems(Update, update_touch_hud.in_set(GameSet::Ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plates_show_cooldowns_bar_gates_and_ready_glows() {
        let ready = PlateContext { bars: 5, ping_gate: true, overload_gate: true, flash_gate: true, ..default() };
        let o = plate_look(TouchButton::Overload, &ready);
        assert!(o.ready && o.glow && o.readout == "READY" && o.accent == ACID_GREEN, "5G glows acid when usable");
        let weak = PlateContext { bars: 1, ping_gate: true, ..default() };
        let o = plate_look(TouchButton::Overload, &weak);
        assert!(!o.ready && !o.glow && o.readout == "3 BARS");
        assert_eq!(plate_look(TouchButton::Flash, &weak).readout, "2 BARS");
        assert!(plate_look(TouchButton::Ping, &weak).ready);
        // Cooldowns: fill + seconds.
        let cooling = PlateContext { dash: (0.5, 1.0), punch: (0.2, 0.13), ping: (0.75, 3.0), ping_gate: true, ..default() };
        let d = plate_look(TouchButton::Dash, &cooling);
        assert!(!d.ready && (d.cooldown - 0.5).abs() < 1e-6 && d.readout == "1.0");
        let p = plate_look(TouchButton::Ping, &cooling);
        assert!(!p.ready && p.readout == "3.0" && (p.cooldown - 0.75).abs() < 1e-6);
        assert!(plate_look(TouchButton::Punch, &cooling).readout == "0.1");
        assert!(plate_look(TouchButton::Dash, &PlateContext::default()).ready);
        // Fire / jump / latch status.
        assert_eq!(plate_look(TouchButton::Fire, &PlateContext { auto_fire: true, ..default() }).readout, "AUTO");
        assert!(plate_look(TouchButton::Jump, &PlateContext { bhop_rim: true, ..default() }).glow);
        assert!(plate_look(TouchButton::AutoBhop, &PlateContext { auto_bhop_latched: true, ..default() }).glow);
        assert!(!plate_look(TouchButton::AutoBhop, &PlateContext::default()).ready);
    }

    #[test]
    fn plate_colors_follow_opacity_and_presses() {
        let look = plate_look(TouchButton::Fire, &PlateContext::default());
        let (fill_lo, edge_lo, _) = plate_colors(&look, false, 0.25);
        let (fill_hi, edge_hi, _) = plate_colors(&look, false, 1.0);
        assert!(fill_lo.alpha() < fill_hi.alpha() && edge_lo.alpha() < edge_hi.alpha());
        let (fill_p, edge_p, label_p) = plate_colors(&look, true, 0.25);
        assert!(fill_p.alpha() > fill_hi.alpha() && edge_p == BONE_TEXT && label_p == INK_BLACK, "a press flashes at full strength");
        assert!(plate_colors(&look, false, f32::NAN).0.alpha() > 0.0);
    }
}
