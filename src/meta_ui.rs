//! Pre-run meta screens: the LOADOUT editor (starter OS chip, handset skin,
//! 12-card App Store catalog, jam-demo switch) and CARRIER CONTRACTS & FEATS
//! (contract toggles, lifetime stats, the feats list), plus the pure text
//! helpers the main menu, end screens and in-game HUD share.
//!
//! Every edit goes through [`commit_meta_edit`]: it mutates the profile and
//! saves it immediately (no-op for in-memory stores). Cards are tagged with
//! [`MetaCard`] / [`MetaStatus`] / [`MetaName`] and restyled in place every
//! frame from the live profile, so nothing is respawned on click.
//!
//! Buttons use the same manual cursor hit test as `hud.rs`
//! (`ComputedNode::contains_point` + `Window::physical_cursor_position`).
//! Footer buttons are ordinary `hud` menu buttons; keyboard shortcuts live in
//! `hud::menu_key_action`.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;

use crate::audio::{SfxEvent, SfxQueue};
use crate::hud::{spawn_crosshair, text, MenuAction, CROSSHAIR_BOX, COLOR_BG, COLOR_CYAN, COLOR_DIM, COLOR_GOLD, COLOR_SIGNAL_AMBER, COLOR_SIGNAL_GREEN, COLOR_SIGNAL_RED};
use crate::ui_kit::{
    hard_shadow, inset_border, plate_border, plate_rect, punk_button, punk_screen_root, rivet_card, section_crate, set_strip_tape, stamped_tag,
    stencil_title, tape_image, terminal_chassis, ButtonStyle, ChassisStrip, Tape, TerminalChassis, UiKitTextures, ACID_DARK, ACID_GREEN,
    ARC_CYAN, ASH_DIM, BLOOD_DARK, BONE_TEXT, CARD_BORDER, HAZARD_DARK, HAZARD_YELLOW, INK_BLACK, IRON_MID, LOCKED_IRON, PUNK_RED,
    STEEL_BORDER,
};
use crate::meta::{
    default_catalog, CarrierContract, CrosshairColor, CrosshairStyle, FeatId, HandsetSkin, LifetimeStats, PlayerProfile, ProfileStore,
    RunLoadout, RunUnlockSummary, StarterChip, UserSettings, ACTIVE_CATALOG_SIZE, BASE_FOV_RANGE, EFFECT_SCALE_RANGE, MOUSE_SENS_RANGE,
    VOLUME_RANGE,
};
use crate::player::PlayerCamera;
use crate::state::{AppState, SettingsReturn, UpgradeId, FINAL_FLOOR};

const PHONE_WIDTH: f32 = 1240.0;
/// Height of the danger-tape trim on contract cards (an authored tape height).
const CONTRACT_TRIM_HEIGHT: f32 = 6.0;
const COLOR_CRIMSON: Color = Color::srgb(0.95, 0.2, 0.12);

// ---------------------------------------------------------------------------
// Pure text helpers (shared with hud.rs)
// ---------------------------------------------------------------------------

/// Compact upper-case chip label for HUD / end screens.
pub fn chip_label(chip: StarterChip) -> &'static str {
    match chip {
        StarterChip::StockUac => "STOCK OS",
        StarterChip::SurveyOs => "SURVEY OS",
        StarterChip::OverclockRadio => "OVERCLOCK RADIO",
        StarterChip::FaradayCase => "FARADAY CASE",
    }
}

/// Short app names that fit a catalog tile.
pub fn app_short_name(id: UpgradeId) -> &'static str {
    match id {
        UpgradeId::DualSimBarrels => "Dual-SIM Barrels",
        UpgradeId::FiveGOverclock => "5G Overclock",
        UpgradeId::CopperAntenna => "Copper Antenna",
        UpgradeId::SignalSiphon => "Signal Siphon",
        UpgradeId::FirewallDaemon => "Firewall Daemon",
        UpgradeId::PacketSniffer => "Packet Sniffer",
        UpgradeId::EmpOvercharge => "EMP Overcharge",
        UpgradeId::AirplaneModePhase => "Airplane Mode",
        UpgradeId::HighCapacityCell => "Hi-Cap Cell",
        UpgradeId::SubGhzPenetration => "Sub-GHz Penetrate",
        UpgradeId::DopplerOverdrive => "Doppler Overdrive",
        UpgradeId::BlastThrusters => "Blast Thrusters",
        UpgradeId::StoreAndForward => "Store & Forward",
        UpgradeId::EmergencyCallback => "Emergency Call",
        UpgradeId::MissedCall => "Missed Call",
        UpgradeId::SignalSpoofer => "Signal Spoofer",
        UpgradeId::JammerHijack => "Jammer Hijack",
    }
}

/// `"STANDARD"`, `"RISK x1"`..`"RISK x3"`, `"MAX RISK x4"`.
pub fn risk_badge_text(contracts: usize) -> String {
    match contracts {
        0 => "STANDARD".to_string(),
        n if n >= CarrierContract::ALL.len() => format!("MAX RISK x{n}"),
        n => format!("RISK x{n}"),
    }
}

/// Risk badge accent: dim when standard, amber, crimson at 3+.
pub fn risk_color(contracts: usize) -> Color {
    match contracts {
        0 => COLOR_DIM,
        1..=2 => COLOR_SIGNAL_AMBER,
        _ => COLOR_CRIMSON,
    }
}

/// Main-menu strip: `OS: <chip> | SKIN: <skin> | CATALOG | CONTRACTS | FEATS`.
pub fn loadout_summary_line(p: &PlayerProfile) -> String {
    let l = p.build_loadout();
    format!(
        "OS: {}  |  SKIN: {}  |  CATALOG: {}/{}  |  CONTRACTS: {}/{}  |  FEATS: {}/{}",
        chip_label(l.chip),
        l.skin.name().to_uppercase(),
        p.normalized_catalog().len(),
        ACTIVE_CATALOG_SIZE,
        p.active_contracts.len(),
        CarrierContract::ALL.len(),
        p.unlocked_feats.len(),
        FeatId::ALL.len()
    )
}

/// Contract tags of a loadout (`"ROAM PRIO"`), or `None` without contracts.
pub fn contract_tags(l: &RunLoadout) -> Option<String> {
    let mut cs: Vec<CarrierContract> = l.contracts.clone();
    cs.sort();
    cs.dedup();
    (!cs.is_empty()).then(|| cs.iter().map(|c| c.short_tag()).collect::<Vec<_>>().join(" "))
}

/// End-screen loadout line: `OS: <chip> | SKIN: <skin> | CONTRACTS: <tags or NONE>`.
pub fn end_loadout_line(l: &RunLoadout) -> String {
    format!(
        "OS: {}  |  SKIN: {}  |  CONTRACTS: {}",
        chip_label(l.chip),
        l.skin.name().to_uppercase(),
        contract_tags(l).unwrap_or_else(|| "NONE".to_string())
    )
}

/// In-game top-left tag: chip label + optional contract tags (the HUD draws
/// a small square bullet between them; the default font is ASCII-only).
pub fn loadout_tag_parts(l: &RunLoadout) -> (String, Option<String>) {
    (chip_label(l.chip).to_string(), contract_tags(l))
}

/// Line used when nothing was unlocked by the last run.
pub const NO_UNLOCKS_LINE: &str = "No new unlocks this run - view Contracts & Feats for targets";

/// Reward lines (apps / chips / skins) of a run summary; empty when none.
pub fn unlock_reward_lines(s: &RunUnlockSummary) -> Vec<String> {
    let mut out = Vec::new();
    let join = |v: Vec<&str>| v.join(", ");
    if !s.new_apps.is_empty() {
        out.push(format!("APPS LICENSED: {}", join(s.new_apps.iter().map(|a| a.name()).collect())));
    }
    if !s.new_chips.is_empty() {
        out.push(format!("OS CHIPS: {}", join(s.new_chips.iter().map(|c| c.name()).collect())));
    }
    if !s.new_skins.is_empty() {
        out.push(format!("HANDSET SKINS: {}", join(s.new_skins.iter().map(|k| k.name()).collect())));
    }
    out
}

/// Every unlock line of a run summary (exhibition note, or feats first then
/// rewards, or the single [`NO_UNLOCKS_LINE`]).
pub fn unlock_summary_lines(s: &RunUnlockSummary) -> Vec<String> {
    if let Some(reason) = &s.skipped_reason {
        return vec![reason.clone()];
    }
    if s.is_empty() {
        return vec![NO_UNLOCKS_LINE.to_string()];
    }
    let mut out: Vec<String> = s.new_feats.iter().map(|f| format!("FEAT: {}", f.name())).collect();
    out.extend(unlock_reward_lines(s));
    out
}

/// Catalog counter pill text and whether the pool is full.
pub fn catalog_counter_text(selected: usize) -> (String, bool) {
    if selected >= ACTIVE_CATALOG_SIZE {
        (format!("{ACTIVE_CATALOG_SIZE}/{ACTIVE_CATALOG_SIZE} READY"), true)
    } else {
        (format!("{selected}/{ACTIVE_CATALOG_SIZE} - AUTO-FILLS TO {ACTIVE_CATALOG_SIZE} ON DEPLOY"), false)
    }
}

/// A lifetime stat nobody has earned yet (`0`, `0/4`, `--:--`).
pub fn stat_is_blank(value: &str) -> bool {
    value == "0" || value.starts_with("0/") || value.starts_with("--")
}

/// `mm:ss`.
pub fn fmt_mmss(secs: f32) -> String {
    let t = secs.max(0.0) as u32;
    format!("{:02}:{:02}", t / 60, t % 60)
}

/// The lifetime stats strip: `(label, value)` cells.
pub fn lifetime_stat_cells(s: &LifetimeStats) -> Vec<(&'static str, String)> {
    vec![
        ("RUNS", s.runs_played.to_string()),
        ("WINS", s.victories.to_string()),
        ("BEST FLOOR", if s.best_floor > FINAL_FLOOR { format!("FL {}", s.best_floor) } else { format!("{}/{}", s.best_floor, FINAL_FLOOR) }),
        ("KILLS", s.total_kills.to_string()),
        ("JAMMERS", s.total_jammers.to_string()),
        ("0-BAR KILLS", s.total_blackout_kills.to_string()),
        ("BEST TIME", s.fastest_victory_secs.map_or_else(|| "--:--".to_string(), fmt_mmss)),
        ("MAX CONTRACTS", format!("{}/{}", s.max_contracts_cleared, CarrierContract::ALL.len())),
    ]
}

/// The movement / Endless records strip: `(label, value)` cells.
pub fn record_stat_cells(s: &LifetimeStats) -> Vec<(&'static str, String)> {
    vec![
        ("TOP SPEED", if s.best_speed > 0.0 { format!("{:.1} M/S", s.best_speed) } else { "--.- M/S".to_string() }),
        ("BEST BHOP CHAIN", if s.best_bhop_chain > 0 { format!("x{}", s.best_bhop_chain) } else { "0".to_string() }),
        ("ENDLESS BEST", if s.best_endless_floor > 0 { format!("FL {}", s.best_endless_floor) } else { "--".to_string() }),
    ]
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// Something clickable on a meta screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaTarget {
    Chip(StarterChip),
    Skin(HandsetSkin),
    App(UpgradeId),
    ResetCatalog,
    JamDemo,
    Contract(CarrierContract),
}

/// Result of a click on a [`MetaTarget`] (the string is header feedback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    /// The profile changed (saved + `UpgradeSelect` sfx).
    Changed(String),
    /// Refused: locked item / full catalog (`Denied` sfx).
    Denied(String),
    /// Nothing to do (already equipped, ...).
    Unchanged(String),
}

fn locked_msg(name: &str, feat: Option<FeatId>) -> String {
    match feat {
        Some(f) => format!("{name} is LOCKED - earn feat \"{}\"", f.name()),
        None => format!("{name} is LOCKED"),
    }
}

/// Apply a click to the profile (no saving; see [`commit_meta_edit`]).
pub fn apply_meta_edit(p: &mut PlayerProfile, t: MetaTarget) -> EditOutcome {
    match t {
        MetaTarget::Chip(c) => {
            if !p.is_chip_unlocked(c) {
                EditOutcome::Denied(locked_msg(c.name(), c.unlock_feat()))
            } else if p.equipped_chip == c {
                EditOutcome::Unchanged(format!("{} already equipped", c.name()))
            } else {
                p.equip_chip(c);
                EditOutcome::Changed(format!("Equipped {}", c.name()))
            }
        }
        MetaTarget::Skin(s) => {
            if !p.is_skin_unlocked(s) {
                EditOutcome::Denied(locked_msg(s.name(), s.unlock_feat()))
            } else if p.equipped_skin == s {
                EditOutcome::Unchanged(format!("{} already equipped", s.name()))
            } else {
                p.equip_skin(s);
                EditOutcome::Changed(format!("Equipped {} skin", s.name()))
            }
        }
        MetaTarget::App(id) => {
            let in_pool = p.normalized_catalog().contains(&id);
            if !in_pool && !p.is_upgrade_unlocked(id) {
                return EditOutcome::Denied(locked_msg(id.name(), id.unlock_feat()));
            }
            let before = p.normalized_catalog().len();
            if p.toggle_catalog_app(id) {
                let n = p.normalized_catalog().len();
                if in_pool {
                    EditOutcome::Changed(format!("Benched {} ({n}/{ACTIVE_CATALOG_SIZE})", id.name()))
                } else {
                    EditOutcome::Changed(format!("Added {} to the pool ({n}/{ACTIVE_CATALOG_SIZE})", id.name()))
                }
            } else {
                EditOutcome::Denied(format!("Catalog full ({before}/{ACTIVE_CATALOG_SIZE}) - deselect an app first"))
            }
        }
        MetaTarget::ResetCatalog => {
            if p.normalized_catalog() == default_catalog() {
                EditOutcome::Unchanged("Catalog already at the default 12 core apps".to_string())
            } else {
                p.reset_default_catalog();
                EditOutcome::Changed("Catalog reset to the default 12 core apps".to_string())
            }
        }
        MetaTarget::JamDemo => {
            let on = !p.jam_demo_unlock_all;
            p.set_jam_demo_unlock_all(on);
            EditOutcome::Changed(if on {
                "JAM DEMO: everything unlocked (earned feats untouched)".to_string()
            } else {
                "JAM DEMO off - locked picks reverted to earned unlocks".to_string()
            })
        }
        MetaTarget::Contract(c) => {
            let on = p.toggle_contract(c);
            EditOutcome::Changed(format!("{} {}", c.name(), if on { "ACTIVE" } else { "OFF" }))
        }
    }
}

/// Apply a click and persist immediately when the profile changed. If a prior
/// save failed (`store.dirty`), an `Unchanged` click retries flushing without
/// mutating the profile again.
pub fn commit_meta_edit(store: &mut ProfileStore, t: MetaTarget) -> EditOutcome {
    let out = apply_meta_edit(&mut store.profile, t);
    match &out {
        EditOutcome::Changed(_) => {
            if let Err(e) = store.try_save() {
                warn!("profile save failed (marked dirty): {e}");
                return EditOutcome::Denied(format!("SAVE FAILED ({e}) - unsaved changes kept, retry anytime"));
            }
        }
        EditOutcome::Unchanged(_) if store.dirty => match store.try_save() {
            Ok(()) => return EditOutcome::Changed("Saved pending profile changes".to_string()),
            Err(e) => {
                warn!("profile retry save failed: {e}");
                return EditOutcome::Denied(format!("SAVE FAILED ({e}) - unsaved changes kept, retry anytime"));
            }
        },
        _ => {}
    }
    out
}

/// How a target currently reads on its card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    Equipped,
    Ready,
    Locked,
    InPool,
    /// Not manually selected, but will auto-fill the 12-card draft pool on deploy.
    AutoFill,
    Benched,
    Active,
    Off,
    Action,
}

pub fn target_state(p: &PlayerProfile, t: MetaTarget) -> TargetState {
    match t {
        MetaTarget::Chip(c) if p.equipped_chip == c && p.is_chip_unlocked(c) => TargetState::Equipped,
        MetaTarget::Chip(c) if p.is_chip_unlocked(c) => TargetState::Ready,
        MetaTarget::Skin(s) if p.equipped_skin == s && p.is_skin_unlocked(s) => TargetState::Equipped,
        MetaTarget::Skin(s) if p.is_skin_unlocked(s) => TargetState::Ready,
        MetaTarget::Chip(_) | MetaTarget::Skin(_) => TargetState::Locked,
        MetaTarget::App(id) if p.normalized_catalog().contains(&id) => TargetState::InPool,
        MetaTarget::App(id) if p.is_upgrade_unlocked(id) && p.effective_catalog().contains(&id) => TargetState::AutoFill,
        MetaTarget::App(id) if p.is_upgrade_unlocked(id) => TargetState::Benched,
        MetaTarget::App(_) => TargetState::Locked,
        MetaTarget::Contract(c) if p.active_contracts.contains(&c) => TargetState::Active,
        MetaTarget::Contract(_) => TargetState::Off,
        MetaTarget::JamDemo if p.jam_demo_unlock_all => TargetState::Active,
        MetaTarget::JamDemo => TargetState::Off,
        MetaTarget::ResetCatalog => TargetState::Action,
    }
}

fn target_feat(t: MetaTarget) -> Option<FeatId> {
    match t {
        MetaTarget::Chip(c) => c.unlock_feat(),
        MetaTarget::Skin(s) => s.unlock_feat(),
        MetaTarget::App(id) => id.unlock_feat(),
        _ => None,
    }
}

/// Status tag text for a card.
pub fn status_text(p: &PlayerProfile, t: MetaTarget) -> String {
    match (t, target_state(p, t)) {
        (MetaTarget::JamDemo, s) => format!("JAM DEMO UNLOCK ALL: {}", if s == TargetState::Active { "ON" } else { "OFF" }),
        (MetaTarget::ResetCatalog, _) => format!("RESET DEFAULT {ACTIVE_CATALOG_SIZE}"),
        (MetaTarget::Contract(_), TargetState::Active) => "[ACTIVE]".to_string(),
        (MetaTarget::Contract(_), _) => "[OFF]".to_string(),
        (MetaTarget::App(_), TargetState::Locked) => "LOCKED".to_string(),
        (_, TargetState::Locked) => match target_feat(t) {
            Some(f) => format!("LOCKED: {}", f.name()),
            None => "LOCKED".to_string(),
        },
        (_, TargetState::Equipped) => "EQUIPPED".to_string(),
        (_, TargetState::Ready) => "READY".to_string(),
        (_, TargetState::InPool) => "IN POOL".to_string(),
        (_, TargetState::AutoFill) => "AUTO-FILL".to_string(),
        (_, TargetState::Benched) => "BENCHED".to_string(),
        (_, s) => format!("{s:?}").to_uppercase(),
    }
}

/// Live card colors: name text, heavy left status rail, thin edge, fill and
/// the stamped status tag.
struct CardLook {
    /// Status text when it sits straight on the card (toggle switches).
    status_color: Color,
    name_color: Color,
    rail: Color,
    edge: Color,
    bg: Color,
    /// Status tag `(fill, text, outline)`. Committed states are solid tape
    /// with ink text; idle states are hollow outlines; locked is a dead slot.
    tag: (Color, Color, Color),
}

/// Rusted crimson-soot rail of locked cards.
const LOCKED_RAIL: Color = Color::srgb(0.42, 0.08, 0.05);

/// Text of the hollow `READY` / `OFF` / `BENCHED` tags: ash lifted toward
/// bone so it stays legible (>= 4.5:1) on a raw steel card.
const IDLE_TAG_TEXT: Color = Color::srgb(0.67, 0.6, 0.54);

/// Solid stamped tag in `c` with ink text.
fn solid_tag(c: Color) -> (Color, Color, Color) {
    (c, INK_BLACK, c)
}

/// Hollow tag: ink well, `c` text, thin `c` outline.
fn hollow_tag(c: Color) -> (Color, Color, Color) {
    (INK_BLACK.with_alpha(0.55), c, c.with_alpha(0.55))
}

fn card_look(state: TargetState, hovered: bool, pulse: f32) -> CardLook {
    // Hover: razor-bright edge + the fill lifts toward raw steel.
    let edge = |c: Color| if hovered { BONE_TEXT } else { c };
    let lift = |c: Color| if hovered { c.mix(&STEEL_BORDER, 0.35) } else { c };
    match state {
        TargetState::Equipped | TargetState::InPool => CardLook {
            status_color: ACID_GREEN,
            name_color: BONE_TEXT,
            rail: ACID_GREEN,
            edge: edge(ACID_GREEN.with_alpha(0.5)),
            bg: lift(ACID_DARK),
            tag: solid_tag(ACID_GREEN),
        },
        TargetState::AutoFill => CardLook {
            status_color: HAZARD_YELLOW,
            name_color: BONE_TEXT,
            rail: HAZARD_YELLOW,
            edge: edge(HAZARD_YELLOW.with_alpha(0.4)),
            bg: lift(HAZARD_DARK),
            tag: solid_tag(HAZARD_YELLOW),
        },
        // Available but not committed: the name stays readable, the card and
        // its hollow tag recede.
        TargetState::Ready | TargetState::Off => CardLook {
            status_color: ASH_DIM,
            name_color: BONE_TEXT,
            rail: STEEL_BORDER,
            edge: edge(STEEL_BORDER.with_alpha(0.55)),
            bg: lift(IRON_MID),
            tag: hollow_tag(IDLE_TAG_TEXT),
        },
        TargetState::Benched => CardLook {
            status_color: ASH_DIM,
            name_color: if hovered { BONE_TEXT } else { ASH_DIM.mix(&BONE_TEXT, 0.35) },
            rail: STEEL_BORDER,
            edge: edge(STEEL_BORDER.with_alpha(0.45)),
            bg: lift(IRON_MID.mix(&INK_BLACK, 0.3)),
            tag: hollow_tag(IDLE_TAG_TEXT),
        },
        TargetState::Locked => CardLook {
            status_color: COLOR_CRIMSON,
            name_color: ASH_DIM.with_alpha(0.75),
            rail: if hovered { PUNK_RED } else { LOCKED_RAIL },
            edge: if hovered { PUNK_RED.with_alpha(0.7) } else { LOCKED_RAIL.with_alpha(0.5) },
            bg: LOCKED_IRON,
            tag: (BLOOD_DARK, COLOR_CRIMSON, LOCKED_RAIL),
        },
        TargetState::Active => CardLook {
            status_color: HAZARD_YELLOW,
            name_color: BONE_TEXT,
            rail: PUNK_RED.mix(&HAZARD_YELLOW, pulse),
            edge: edge(PUNK_RED.with_alpha(0.6)),
            bg: lift(BLOOD_DARK),
            // Bone stamp: the one tag that must not sink into the blood fill.
            tag: solid_tag(BONE_TEXT),
        },
        TargetState::Action => CardLook {
            status_color: ARC_CYAN,
            name_color: ARC_CYAN,
            rail: ARC_CYAN,
            edge: edge(STEEL_BORDER.with_alpha(0.55)),
            bg: lift(IRON_MID),
            tag: hollow_tag(ARC_CYAN),
        },
    }
}

// ---------------------------------------------------------------------------
// ECS
// ---------------------------------------------------------------------------

/// Clickable card / toggle on a meta screen.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaCard(pub MetaTarget);

/// Status tag text inside a card (refreshed live).
#[derive(Component, Debug, Clone, Copy)]
pub struct MetaStatus(pub MetaTarget);

/// Name text inside a card (dimmed while locked).
#[derive(Component, Debug, Clone, Copy)]
pub struct MetaName(pub MetaTarget);

/// Stamped status tag box of a card: its fill and outline follow the card
/// state, and the [`MetaStatus`] text inside it takes the tag's text color.
#[derive(Component, Debug, Clone, Copy)]
pub struct MetaStatusTag(pub MetaTarget);

/// Danger-tape trim along the top edge of a contract card: live tape while
/// the contract is signed, a dead iron slot while it is off.
#[derive(Component, Debug, Clone, Copy)]
pub struct ContractTrim(pub CarrierContract);

/// Live text labels.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaLabel {
    Feedback,
    CatalogCounter,
    RiskBadge,
}

/// Live boxes (pills / glowing panels).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaBox {
    CatalogCounterPill,
    RiskBadgePill,
    /// Main-menu `[JAM DEMO: ALL UNLOCKED]` pill.
    JamPill,
    /// End-screen RUN UNLOCKS panel (glows when something was unlocked).
    UnlockPanel,
}

/// Header feedback line of the meta screens.
#[derive(Resource, Debug, Clone)]
pub struct MetaFeedback {
    pub text: String,
    pub color: Color,
}

impl Default for MetaFeedback {
    fn default() -> Self {
        MetaFeedback { text: "Click a card to select / toggle - changes save instantly".to_string(), color: COLOR_DIM }
    }
}

/// The card under the cursor this frame.
#[derive(Resource, Debug, Clone, Copy, Default)]
struct MetaHover(Option<MetaTarget>);

pub struct MetaUiPlugin;

impl Plugin for MetaUiPlugin {
    fn build(&self, app: &mut App) {
        let meta_screen = in_state(AppState::Loadout).or_else(in_state(AppState::ContractsFeats));
        add_settings_systems(app);
        app.init_resource::<MetaFeedback>()
            .init_resource::<MetaHover>()
            .add_systems(OnEnter(AppState::Loadout), (reset_feedback, spawn_loadout_screen).chain())
            .add_systems(OnEnter(AppState::ContractsFeats), (reset_feedback, spawn_contracts_screen).chain())
            .add_systems(Update, meta_card_clicks.run_if(meta_screen))
            .add_systems(
                Update,
                (refresh_contract_chassis, refresh_contract_trims).after(meta_card_clicks).run_if(in_state(AppState::ContractsFeats)),
            )
            .add_systems(
                Update,
                refresh_meta_ui.after(meta_card_clicks).after(settings_clicks).run_if(not(in_state(AppState::Playing)).and_then(not(in_state(AppState::UpgradeSelect)))),
            );
    }
}

fn reset_feedback(store: Res<ProfileStore>, mut fb: ResMut<MetaFeedback>, mut hover: ResMut<MetaHover>) {
    *fb = if let Some(err) = &store.last_save_error {
        MetaFeedback { text: format!("UNSAVED PROFILE ({err}) - click any card to retry save"), color: COLOR_SIGNAL_RED }
    } else if let Some(notice) = &store.recovery_notice {
        MetaFeedback { text: notice.clone(), color: COLOR_SIGNAL_AMBER }
    } else {
        MetaFeedback::default()
    };
    hover.0 = None;
}

fn meta_card_clicks(
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    cards: Query<(&ComputedNode, &UiGlobalTransform, &MetaCard)>,
    mut store: ResMut<ProfileStore>,
    mut fb: ResMut<MetaFeedback>,
    mut hover: ResMut<MetaHover>,
    mut sfx: ResMut<SfxQueue>,
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    hover.0 = cursor.and_then(|c| cards.iter().find(|(n, tf, _)| n.contains_point(**tf, c)).map(|(_, _, card)| card.0));
    if let Some(t) = hover.0
        && mouse.just_released(MouseButton::Left)
    {
        let (text, color) = match commit_meta_edit(&mut store, t) {
            EditOutcome::Changed(s) => {
                sfx.play(SfxEvent::UpgradeSelect);
                (s, COLOR_SIGNAL_GREEN)
            }
            EditOutcome::Denied(s) => {
                sfx.play(SfxEvent::Denied);
                (s, COLOR_SIGNAL_RED)
            }
            EditOutcome::Unchanged(s) => (s, COLOR_CYAN),
        };
        *fb = MetaFeedback { text, color };
    }
}

/// Change-guarded text write (no `DerefMut`, hence no relayout, when equal).
fn set_text(txt: &mut Mut<Text>, s: &str) {
    if txt.0 != s {
        txt.0.clear();
        txt.0.push_str(s);
    }
}

/// Restyle every tagged card / label / pill from the live profile.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn refresh_meta_ui(
    store: Res<ProfileStore>,
    fb: Res<MetaFeedback>,
    hover: Res<MetaHover>,
    time: Res<Time>,
    mut boxes: Query<
        (&mut BackgroundColor, &mut BorderColor, Option<&MetaCard>, Option<&MetaBox>, Option<&MetaStatusTag>),
        Or<(With<MetaCard>, With<MetaBox>, With<MetaStatusTag>)>,
    >,
    mut texts: Query<
        (&mut Text, &mut TextColor, Option<&MetaStatus>, Option<&MetaName>, Option<&MetaLabel>, Option<&ChildOf>),
        Or<(With<MetaStatus>, With<MetaName>, With<MetaLabel>)>,
    >,
    tags: Query<(), With<MetaStatusTag>>,
    fresh: Query<(), Or<(Added<MetaStatus>, Added<MetaLabel>)>>,
) {
    let p = &store.profile;
    let pulse = 0.5 + 0.5 * (time.elapsed_secs() * 3.5).sin();
    // Label strings only depend on the profile / feedback: rebuild them only
    // when those change or new labels appear (colors still pulse per frame).
    let retext = store.is_changed() || fb.is_changed() || !fresh.is_empty();
    let selected = p.normalized_catalog().len();
    let (counter, full) = catalog_counter_text(selected);
    let counter_color = if full { ACID_GREEN } else { HAZARD_YELLOW };
    let risk = p.active_contracts.len();
    let look = |t: MetaTarget| card_look(target_state(p, t), hover.0 == Some(t), pulse);
    for (mut bg, mut border, card, b, tag) in &mut boxes {
        // Stamped tapes: solid accent fill (INK label), dark bottom bevel.
        let tape = |c: Color| (c, BorderColor::all(INK_BLACK));
        let (new_bg, nb) = match (card, b) {
            (Some(card), _) => {
                let l = look(card.0);
                (l.bg, plate_border(l.rail, l.edge))
            }
            (None, None) if tag.is_some() => {
                let (fill, _, outline) = look(tag.map_or(MetaTarget::JamDemo, |t| t.0)).tag;
                (fill, BorderColor { bottom: if fill.alpha() > 0.9 { INK_BLACK } else { outline }, ..BorderColor::all(outline) })
            }
            (None, Some(MetaBox::CatalogCounterPill)) => tape(counter_color),
            (None, Some(MetaBox::RiskBadgePill)) => {
                let c = risk_color(risk);
                tape(if risk > 0 { c.mix(&BONE_TEXT, 0.25 * pulse) } else { c })
            }
            (None, Some(MetaBox::JamPill)) => tape(HAZARD_YELLOW.mix(&HAZARD_DARK, 0.35 * pulse)),
            (None, Some(MetaBox::UnlockPanel)) => {
                let (fill, rail) = unlock_panel_look(store.last_run_summary.as_ref(), pulse);
                (fill, plate_border(rail, STEEL_BORDER.with_alpha(0.7)))
            }
            (None, None) => continue,
        };
        if bg.0 != new_bg {
            bg.0 = new_bg;
        }
        if *border != nb {
            *border = nb;
        }
    }
    for (mut txt, mut col, status, name, label, parent) in &mut texts {
        let c = if let Some(s) = status {
            if retext {
                set_text(&mut txt, &status_text(p, s.0));
            }
            // Inside a stamped tag the text takes the tag's own text color.
            let l = look(s.0);
            if parent.is_some_and(|p| tags.contains(p.parent())) { l.tag.1 } else { l.status_color }
        } else if let Some(n) = name {
            look(n.0).name_color
        } else {
            match label {
                Some(MetaLabel::Feedback) => {
                    if retext {
                        set_text(&mut txt, &fb.text);
                    }
                    fb.color
                }
                Some(MetaLabel::CatalogCounter) => {
                    if retext {
                        set_text(&mut txt, &counter);
                    }
                    INK_BLACK
                }
                Some(MetaLabel::RiskBadge) => {
                    if retext {
                        set_text(&mut txt, &risk_badge_text(risk));
                    }
                    INK_BLACK
                }
                None => continue,
            }
        };
        if col.0 != c {
            col.0 = c;
        }
    }
}

/// Contracts screen: each contract card's top trim shows live danger tape
/// while that contract is signed.
fn refresh_contract_trims(store: Res<ProfileStore>, mut trims: Query<(&ContractTrim, &mut ImageNode, &mut BackgroundColor)>) {
    for (trim, mut image, mut bg) in &mut trims {
        let (tint, fill) = contract_trim_look(store.profile.active_contracts.contains(&trim.0));
        if image.color != tint {
            image.color = tint;
        }
        if bg.0 != fill {
            bg.0 = fill;
        }
    }
}

/// `(tape tint, slot fill)` of a contract card's top trim: full-strength
/// danger tape while signed, an empty dark slot (tape hidden) while off.
pub fn contract_trim_look(signed: bool) -> (Color, Color) {
    if signed { (Color::WHITE, INK_BLACK) } else { (Color::NONE, INK_BLACK.with_alpha(0.55)) }
}

/// `(fill, rail)` of the end-screen RUN UNLOCKS crate: phosphor on a winning
/// run, salvage amber when a lost run still unlocked something, and a dead
/// ink well when there is nothing to show.
pub fn unlock_panel_look(summary: Option<&RunUnlockSummary>, pulse: f32) -> (Color, Color) {
    match summary {
        Some(s) if !s.is_empty() && s.victory => (ACID_DARK.with_alpha(0.9), ACID_GREEN.mix(&COLOR_GOLD, pulse)),
        Some(s) if !s.is_empty() => (HAZARD_DARK.with_alpha(0.9), COLOR_GOLD.mix(&HAZARD_YELLOW, pulse)),
        _ => (INK_BLACK.with_alpha(0.7), STEEL_BORDER),
    }
}

// ---------------------------------------------------------------------------
// Layout builders
// ---------------------------------------------------------------------------

/// Full-screen grunge root + the screen's own camera (entered from menus or
/// end screens, where no player camera exists).
fn meta_root(commands: &mut Commands, state: AppState, tex: Option<&UiKitTextures>, msaa: bevy::render::view::Msaa) -> Entity {
    commands.spawn((
        DespawnOnExit(state),
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::Custom(COLOR_BG), ..default() },
        msaa,
    ));
    punk_screen_root(commands, state, tex, INK_BLACK, 1.0)
}

fn row(justify: JustifyContent, gap: f32) -> Node {
    Node { width: Val::Percent(100.0), justify_content: justify, align_items: AlignItems::Center, column_gap: Val::Px(gap), ..default() }
}

/// CSS grid of `cols` equal columns; rows share any spare panel height, so
/// side-by-side panels line their cards up.
fn grid(commands: &mut Commands, parent: Entity, cols: u16, gap: f32) -> Entity {
    commands
        .spawn((
            ChildOf(parent),
            Node {
                display: Display::Grid,
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                grid_auto_rows: vec![GridTrack::flex(1.0)],
                grid_template_columns: RepeatedGridTrack::flex(cols, 1.0),
                column_gap: Val::Px(gap),
                row_gap: Val::Px(gap),
                ..default()
            },
        ))
        .id()
}

/// Stamped-tape node (live pills recolor its fill; its label is INK text).
fn tape_node(pad_x: f32, pad_y: f32) -> Node {
    Node {
        padding: UiRect::axes(Val::Px(pad_x), Val::Px(pad_y)),
        border: UiRect::bottom(Val::Px(2.0)),
        align_items: AlignItems::Center,
        justify_content: JustifyContent::Center,
        flex_shrink: 0.0,
        ..default()
    }
}

/// Live stamped status tag of a card (`IN POOL`, `BENCHED`, `[ACTIVE]`...):
/// restyled every frame from the card state by [`refresh_meta_ui`].
fn status_tag(p: &PlayerProfile, t: MetaTarget, font: f32) -> impl Bundle {
    (
        MetaStatusTag(t),
        Node {
            padding: UiRect::axes(Val::Px(5.0), Val::Px(0.0)),
            border: plate_rect((1.0, 1.0, 1.0, 2.0)),
            align_items: AlignItems::Center,
            flex_shrink: 0.0,
            ..default()
        },
        BorderColor::all(STEEL_BORDER),
        BackgroundColor(INK_BLACK.with_alpha(0.55)),
        children![(MetaStatus(t), text(status_text(p, t), font, ASH_DIM))],
    )
}

/// Hollow stamp (thin outline, no fill): the "not yet" counterpart of a solid
/// [`stamped_tag`].
fn hollow_stamp(label: &str, color: Color, font: f32) -> impl Bundle {
    (
        Node {
            padding: UiRect::axes(Val::Px((font * 0.5).round()), Val::Px(0.0)),
            border: plate_rect((1.0, 1.0, 1.0, 2.0)),
            align_items: AlignItems::Center,
            flex_shrink: 0.0,
            ..default()
        },
        BorderColor::all(color.with_alpha(0.55)),
        BackgroundColor(INK_BLACK.with_alpha(0.55)),
        children![text(label, font, color)],
    )
}

/// A clickable switch-plate toggle / action whose label is its status.
fn toggle_button(commands: &mut Commands, parent: Entity, t: MetaTarget, p: &PlayerProfile) {
    commands.spawn((
        ChildOf(parent),
        MetaCard(t),
        Node { border: plate_rect((4.0, 1.0, 1.0, 3.0)), flex_shrink: 0.0, ..tape_node(11.0, 4.0) },
        plate_border(STEEL_BORDER, STEEL_BORDER),
        BackgroundColor(IRON_MID),
        hard_shadow(3.0),
        children![(MetaStatus(t), text(status_text(p, t), 13.0, BONE_TEXT))],
    ));
}

/// Header row: stencil screen title | live feedback (center) | caller extras.
fn screen_header(commands: &mut Commands, phone: Entity, title: &str, fb: &MetaFeedback) -> Entity {
    let header = commands.spawn((ChildOf(phone), row(JustifyContent::SpaceBetween, 14.0))).id();
    stencil_title(commands, header, title, 23.0, BONE_TEXT, PUNK_RED);
    commands.spawn((
        ChildOf(header),
        Node { flex_grow: 1.0, flex_shrink: 1.0, justify_content: JustifyContent::Center, ..default() },
        children![(MetaLabel::Feedback, text(fb.text.clone(), 14.0, fb.color))],
    ));
    header
}

fn footer(commands: &mut Commands, phone: Entity, buttons: [(&str, MenuAction, ButtonStyle); 3]) {
    let f = commands.spawn((ChildOf(phone), Node { margin: UiRect { top: Val::Px(2.0), bottom: Val::Px(4.0), ..default() }, ..row(JustifyContent::Center, 24.0) })).id();
    for (label, action, style) in buttons {
        commands.spawn((ChildOf(f), punk_button(label, action, 19.0, style)));
    }
}

// ---------------------------------------------------------------------------
// LOADOUT screen
// ---------------------------------------------------------------------------

fn spawn_loadout_screen(
    mut commands: Commands,
    store: Res<ProfileStore>,
    fb: Res<MetaFeedback>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
) {
    let tex = tex.as_deref();
    let msaa = msaa.map(|m| m.0).unwrap_or(bevy::render::view::Msaa::Sample2);
    let p = &store.profile;
    let root = meta_root(&mut commands, AppState::Loadout, tex, msaa);
    let phone = terminal_chassis(&mut commands, root, tex, PHONE_WIDTH, false);

    // Header: title | live feedback | jam-demo toggle.
    let header = screen_header(&mut commands, phone, "UAC-PHONE // PRE-RUN LOADOUT", &fb);
    toggle_button(&mut commands, header, MetaTarget::JamDemo, p);

    // Upper split: starter chips (left) | handset skins (right).
    let split = commands
        .spawn((ChildOf(phone), Node { width: Val::Percent(100.0), column_gap: Val::Px(10.0), align_items: AlignItems::Stretch, ..default() }))
        .id();
    let (chips, chip_head) = section_crate(&mut commands, split, "STARTER OS CHIP", HAZARD_YELLOW, Node { width: Val::Percent(55.0), ..default() });
    commands.spawn((ChildOf(chip_head), text("PICK 1 OF 4", 11.0, ASH_DIM)));
    let chip_grid = grid(&mut commands, chips, 2, 8.0);
    for chip in StarterChip::ALL {
        let t = MetaTarget::Chip(chip);
        let c = rivet_card(&mut commands, chip_grid, t, UiRect { left: Val::Px(10.0), right: Val::Px(8.0), top: Val::Px(6.0), bottom: Val::Px(7.0) }, 4.0);
        commands.spawn((
            ChildOf(c),
            Node { flex_wrap: FlexWrap::Wrap, row_gap: Val::Px(3.0), margin: UiRect::bottom(Val::Px(1.0)), ..row(JustifyContent::SpaceBetween, 8.0) },
            children![(MetaName(t), text(chip.name().to_uppercase(), 15.0, BONE_TEXT)), status_tag(p, t, 10.0)],
        ));
        // PRO / CON ledger: a fixed tag column keeps wrapped lines aligned.
        for (tag, line, color) in [("PRO", chip.upside(), ACID_GREEN.mix(&BONE_TEXT, 0.25)), ("CON", chip.tradeoff(), HAZARD_YELLOW)] {
            commands.spawn((
                ChildOf(c),
                Node { width: Val::Percent(100.0), column_gap: Val::Px(8.0), align_items: AlignItems::FlexStart, ..default() },
                children![
                    (Node { width: Val::Px(24.0), flex_shrink: 0.0, ..default() }, children![text(tag, 12.0, color.with_alpha(0.7))]),
                    (Node { flex_grow: 1.0, flex_shrink: 1.0, flex_basis: Val::Px(0.0), ..default() }, children![text(line, 12.0, color)]),
                ],
            ));
        }
    }

    let (skins, skin_head) =
        section_crate(&mut commands, split, "HANDSET SKIN", HAZARD_YELLOW, Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() });
    commands.spawn((ChildOf(skin_head), text("PICK 1 OF 4", 11.0, ASH_DIM)));
    let skin_grid = grid(&mut commands, skins, 2, 8.0);
    for skin in HandsetSkin::ALL {
        let t = MetaTarget::Skin(skin);
        let c = rivet_card(&mut commands, skin_grid, t, UiRect { left: Val::Px(10.0), right: Val::Px(8.0), top: Val::Px(7.0), bottom: Val::Px(7.0) }, 5.0);
        let pal = skin.palette();
        let bar = commands
            .spawn((
                ChildOf(c),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(20.0),
                    column_gap: Val::Px(2.0),
                    padding: UiRect::all(Val::Px(2.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                // Swatches sit in a recessed slot, like paint chips in a tray.
                inset_border(),
                BackgroundColor(INK_BLACK),
            ))
            .id();
        for m in [pal.bezel, pal.frame, pal.bumper, pal.ptt] {
            commands.spawn((ChildOf(bar), Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() }, BackgroundColor(m.color)));
        }
        commands.spawn((ChildOf(c), MetaName(t), text(skin.name().to_uppercase(), 13.0, BONE_TEXT)));
        commands.spawn((ChildOf(c), Node { flex_wrap: FlexWrap::Wrap, ..default() }, children![status_tag(p, t, 10.0)]));
    }

    // App Store catalog.
    let (cat, title_row) = section_crate(&mut commands, phone, "APP STORE CATALOG", HAZARD_YELLOW, Node { width: Val::Percent(100.0), ..default() });
    commands.spawn((ChildOf(title_row), text(format!("{ACTIVE_CATALOG_SIZE}-CARD DRAFT POOL"), 11.0, ASH_DIM)));
    let (counter, _) = catalog_counter_text(p.normalized_catalog().len());
    commands.spawn((
        ChildOf(title_row),
        MetaBox::CatalogCounterPill,
        tape_node(9.0, 2.0),
        BorderColor::all(INK_BLACK),
        BackgroundColor(ACID_GREEN),
        children![(MetaLabel::CatalogCounter, text(counter, 12.0, INK_BLACK))],
    ));
    commands.spawn((ChildOf(title_row), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(title_row), text("click an app to add / bench it", 12.0, ASH_DIM)));
    toggle_button(&mut commands, title_row, MetaTarget::ResetCatalog, p);
    let app_grid = grid(&mut commands, cat, 5, 8.0);
    for id in UpgradeId::ALL {
        let t = MetaTarget::App(id);
        let c = rivet_card(&mut commands, app_grid, t, UiRect { left: Val::Px(8.0), right: Val::Px(7.0), top: Val::Px(4.0), bottom: Val::Px(5.0) }, 4.0);
        commands.spawn((
            ChildOf(c),
            row(JustifyContent::FlexStart, 6.0),
            children![text(id.icon(), 12.0, HAZARD_YELLOW), (MetaName(t), text(app_short_name(id), 13.0, BONE_TEXT))],
        ));
        let bottom = commands.spawn((ChildOf(c), row(JustifyContent::SpaceBetween, 6.0))).id();
        commands.spawn((ChildOf(bottom), status_tag(p, t, 10.0)));
        if !id.is_core() {
            // Signature apps wear a hollow gold badge, so it never reads as
            // a (solid) pool state.
            commands.spawn((ChildOf(bottom), hollow_stamp("SIG", COLOR_GOLD, 10.0)));
        }
    }

    footer(
        &mut commands,
        phone,
        [
            ("BACK  [ESC]", MenuAction::BackToMenu, ButtonStyle::ScrapSteel),
            ("CONTRACTS & FEATS  [C]", MenuAction::OpenContractsFeats, ButtonStyle::DangerRed),
            ("DEPLOY RUN  [ENTER]", MenuAction::StartRun, ButtonStyle::HeroAcid),
        ],
    );
}

// ---------------------------------------------------------------------------
// CONTRACTS & FEATS screen
// ---------------------------------------------------------------------------

fn spawn_contracts_screen(
    mut commands: Commands,
    store: Res<ProfileStore>,
    fb: Res<MetaFeedback>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
) {
    let tex = tex.as_deref();
    let msaa = msaa.map(|m| m.0).unwrap_or(bevy::render::view::Msaa::Sample2);
    let p = &store.profile;
    let risk = p.active_contracts.len();
    let root = meta_root(&mut commands, AppState::ContractsFeats, tex, msaa);
    let phone = terminal_chassis(&mut commands, root, tex, PHONE_WIDTH, risk > 0);

    let header = screen_header(&mut commands, phone, "UAC-PHONE // CARRIER CONTRACTS & FEATS", &fb);
    commands.spawn((
        ChildOf(header),
        MetaBox::RiskBadgePill,
        Node { border: plate_rect((4.0, 0.0, 0.0, 3.0)), ..tape_node(13.0, 4.0) },
        BorderColor::all(INK_BLACK),
        BackgroundColor(risk_color(risk)),
        children![(MetaLabel::RiskBadge, text(risk_badge_text(risk), 15.0, INK_BLACK))],
    ));

    // Contracts.
    let (sec, title_row) = section_crate(&mut commands, phone, "CARRIER CONTRACTS", PUNK_RED, Node { width: Val::Percent(100.0), ..default() });
    commands.spawn((ChildOf(title_row), text("STACKABLE 0-4  //  opt-in risk: click to sign / cancel - needed for prestige feats", 12.0, ASH_DIM)));
    let cgrid = grid(&mut commands, sec, 4, 8.0);
    for c in CarrierContract::ALL {
        let t = MetaTarget::Contract(c);
        let cardx = rivet_card(
            &mut commands,
            cgrid,
            t,
            UiRect { left: Val::Px(10.0), right: Val::Px(9.0), top: Val::Px(CONTRACT_TRIM_HEIGHT + 4.0), bottom: Val::Px(5.0) },
            3.0,
        );
        // Danger-stripe top trim: live tape while signed, a dead slot while off.
        let signed = p.active_contracts.contains(&c);
        let (tint, fill) = contract_trim_look(signed);
        let trim = commands
            .spawn((
                ChildOf(cardx),
                ContractTrim(c),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(0.0),
                    height: Val::Px(CONTRACT_TRIM_HEIGHT),
                    ..default()
                },
                BackgroundColor(fill),
            ))
            .id();
        if let Some(tex) = tex {
            commands.entity(trim).insert(ImageNode { color: tint, ..tape_image(tex, CONTRACT_TRIM_HEIGHT, Tape::Danger) });
        }
        commands.spawn((
            ChildOf(cardx),
            row(JustifyContent::SpaceBetween, 6.0),
            children![stamped_tag(&format!("[{}]", c.short_tag()), HAZARD_YELLOW, INK_BLACK, 12.0), status_tag(p, t, 11.0)],
        ));
        commands.spawn((ChildOf(cardx), MetaName(t), text(c.name().to_uppercase(), 15.0, BONE_TEXT)));
        commands.spawn((ChildOf(cardx), text(c.description(), 12.0, BONE_TEXT.with_alpha(0.78))));
    }

    // Lifetime-stats readout: a stamped tape block + eight recessed cells,
    // then the movement / Endless records strip.
    // Untouched records (0 / --:--) stay dim so real numbers carry the eye.
    stat_strip(&mut commands, phone, ("LIFETIME", "SERVICE LOG"), HAZARD_YELLOW, lifetime_stat_cells(&p.stats), 19.0);
    stat_strip(&mut commands, phone, ("RECORDS", "MOVEMENT // ENDLESS"), ARC_CYAN, record_stat_cells(&p.stats), 16.0);

    // Feats manifest.
    let done = FeatId::ALL.iter().filter(|f| p.has_feat(**f)).count();
    let (fsec, ftitle) = section_crate(&mut commands, phone, "FEATS & UNLOCKS", ACID_GREEN, Node { width: Val::Percent(100.0), ..default() });
    commands.spawn((ChildOf(ftitle), text(format!("{done} / {} EARNED", FeatId::ALL.len()), 12.0, ACID_GREEN)));
    if p.jam_demo_unlock_all {
        commands.spawn((ChildOf(ftitle), text("JAM DEMO is unlocking everything - feats below are what you have really earned", 12.0, HAZARD_YELLOW)));
    }
    let fgrid = grid(&mut commands, fsec, 2, 5.0);
    for f in FeatId::ALL {
        let earned = p.has_feat(f);
        let (rail, bg) = if earned { (ACID_GREEN, ACID_DARK.with_alpha(0.9)) } else { (STEEL_BORDER, IRON_MID.mix(&INK_BLACK, 0.35).with_alpha(0.85)) };
        // Earned feats wear a solid phosphor stamp; open ones a hollow outline.
        let stamp = commands.spawn(Node { flex_shrink: 0.0, ..default() }).id();
        if earned {
            commands.spawn((ChildOf(stamp), stamped_tag("[DONE]", ACID_GREEN, INK_BLACK, 11.0)));
        } else {
            commands.spawn((ChildOf(stamp), hollow_stamp("[LOCKED]", ASH_DIM, 11.0)));
        }
        let feat = commands.spawn((
            ChildOf(fgrid),
            Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(Val::Px(9.0), Val::Px(2.0)),
                row_gap: Val::Px(1.0),
                border: plate_rect(CARD_BORDER),
                ..default()
            },
            plate_border(rail, rail.with_alpha(0.3)),
            BackgroundColor(bg),
        ));
        let feat = feat.id();
        let top = commands.spawn((ChildOf(feat), Node { flex_wrap: FlexWrap::Wrap, ..row(JustifyContent::SpaceBetween, 8.0) })).id();
        let lead = commands.spawn((ChildOf(top), Node { column_gap: Val::Px(8.0), align_items: AlignItems::Center, ..default() })).id();
        commands.entity(stamp).insert(ChildOf(lead));
        commands.spawn((ChildOf(lead), text(f.name().to_uppercase(), 14.0, if earned { BONE_TEXT } else { ASH_DIM.mix(&BONE_TEXT, 0.45) })));
        commands.spawn((ChildOf(top), text(format!("REWARD: {}", f.reward_summary()), 12.0, if earned { COLOR_GOLD } else { ARC_CYAN.mix(&ASH_DIM, 0.4) })));
        commands.spawn((ChildOf(feat), text(f.requirement(), 12.0, if earned { ASH_DIM.mix(&BONE_TEXT, 0.25) } else { ASH_DIM })));
    }

    footer(
        &mut commands,
        phone,
        [
            ("BACK  [ESC]", MenuAction::BackToMenu, ButtonStyle::ScrapSteel),
            ("LOADOUT  [L]", MenuAction::OpenLoadout, ButtonStyle::HazardAmber),
            ("DEPLOY RUN  [ENTER]", MenuAction::StartRun, ButtonStyle::HeroAcid),
        ],
    );
}

/// Contracts screen: the terminal's tape strips + left rail flip to crimson
/// danger stripes while any contract is signed.
/// Recessed stat readout: a stamped tape block followed by equal cells.
fn stat_strip(commands: &mut Commands, parent: Entity, (title, sub): (&str, &str), tape: Color, cells: Vec<(&'static str, String)>, font: f32) {
    let strip = commands
        .spawn((
            ChildOf(parent),
            Node { width: Val::Percent(100.0), align_items: AlignItems::Stretch, border: plate_rect((0.0, 2.0, 2.0, 2.0)), ..default() },
            BorderColor { left: Color::NONE, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.74)),
        ))
        .id();
    commands.spawn((
        ChildOf(strip),
        Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::Center,
            padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
            width: Val::Px(150.0),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(tape),
        children![text(title, 13.0, INK_BLACK), text(sub, 10.0, INK_BLACK.with_alpha(0.8))],
    ));
    for (label, value) in cells {
        let blank = stat_is_blank(&value);
        commands.spawn((
            ChildOf(strip),
            Node {
                flex_grow: 1.0,
                flex_basis: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(1.0),
                padding: UiRect::vertical(Val::Px(if font > 17.0 { 5.0 } else { 3.0 })),
                border: UiRect::left(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(STEEL_BORDER.with_alpha(0.5)),
            children![text(label, 10.0, ASH_DIM), text(value, font, if blank { ASH_DIM.with_alpha(0.7) } else { BONE_TEXT })],
        ));
    }
}

fn refresh_contract_chassis(
    store: Res<ProfileStore>,
    tex: Option<Res<UiKitTextures>>,
    mut strips: Query<(&mut ImageNode, &ChassisStrip)>,
    mut chassis: Query<&mut BorderColor, With<TerminalChassis>>,
) {
    let danger = !store.profile.active_contracts.is_empty();
    if let Some(tex) = tex.as_deref() {
        for (mut img, strip) in &mut strips {
            set_strip_tape(&mut img, tex, strip.0, danger);
        }
    }
    let rail = Tape::from(danger).accent();
    for mut b in &mut chassis {
        if b.left != rail {
            b.left = rail;
        }
    }
}

// ---------------------------------------------------------------------------
// SETTINGS screen
// ---------------------------------------------------------------------------

/// A numeric setting adjusted with `-` / `+`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingKnob {
    Sensitivity,
    BaseFov,
    SpeedFov,
    StrafeRoll,
    Volume,
}

impl SettingKnob {
    pub const ALL: [SettingKnob; 5] = [SettingKnob::Sensitivity, SettingKnob::BaseFov, SettingKnob::SpeedFov, SettingKnob::StrafeRoll, SettingKnob::Volume];

    pub fn label(self) -> &'static str {
        match self {
            SettingKnob::Sensitivity => "MOUSE SENSITIVITY",
            SettingKnob::BaseFov => "BASE FOV",
            SettingKnob::SpeedFov => "SPEED FOV INTENSITY",
            SettingKnob::StrafeRoll => "STRAFE ROLL INTENSITY",
            SettingKnob::Volume => "MASTER VOLUME",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            SettingKnob::Sensitivity => "multiplier on mouse look",
            SettingKnob::BaseFov => "resting vertical field of view",
            SettingKnob::SpeedFov => "view widening at speed / dash",
            SettingKnob::StrafeRoll => "camera lean into strafes",
            SettingKnob::Volume => "every sound effect",
        }
    }

    /// `(min, max, step)`.
    pub fn range(self) -> (f32, f32, f32) {
        match self {
            SettingKnob::Sensitivity => (MOUSE_SENS_RANGE.0, MOUSE_SENS_RANGE.1, 0.05),
            SettingKnob::BaseFov => (BASE_FOV_RANGE.0, BASE_FOV_RANGE.1, 5.0),
            SettingKnob::SpeedFov | SettingKnob::StrafeRoll => (EFFECT_SCALE_RANGE.0, EFFECT_SCALE_RANGE.1, 0.25),
            SettingKnob::Volume => (VOLUME_RANGE.0, VOLUME_RANGE.1, 0.1),
        }
    }

    pub fn get(self, s: &UserSettings) -> f32 {
        match self {
            SettingKnob::Sensitivity => s.mouse_sensitivity,
            SettingKnob::BaseFov => s.base_fov_deg,
            SettingKnob::SpeedFov => s.speed_fov_scale,
            SettingKnob::StrafeRoll => s.strafe_roll_scale,
            SettingKnob::Volume => s.audio_master_volume,
        }
    }

    fn slot(self, s: &mut UserSettings) -> &mut f32 {
        match self {
            SettingKnob::Sensitivity => &mut s.mouse_sensitivity,
            SettingKnob::BaseFov => &mut s.base_fov_deg,
            SettingKnob::SpeedFov => &mut s.speed_fov_scale,
            SettingKnob::StrafeRoll => &mut s.strafe_roll_scale,
            SettingKnob::Volume => &mut s.audio_master_volume,
        }
    }

    /// The toggle gating this knob (its meter dims while the toggle is off).
    pub fn gate(self) -> Option<SettingToggle> {
        match self {
            SettingKnob::SpeedFov => Some(SettingToggle::SpeedFov),
            SettingKnob::StrafeRoll => Some(SettingToggle::StrafeRoll),
            SettingKnob::Volume => Some(SettingToggle::Mute),
            SettingKnob::Sensitivity | SettingKnob::BaseFov => None,
        }
    }

    /// Readout of the current value.
    pub fn value_text(self, s: &UserSettings) -> String {
        let v = self.get(s);
        match self {
            SettingKnob::Sensitivity => format!("{v:.2}x"),
            SettingKnob::BaseFov => format!("{v:.0} DEG"),
            SettingKnob::SpeedFov | SettingKnob::StrafeRoll | SettingKnob::Volume => format!("{:.0}%", v * 100.0),
        }
    }

    /// Position of the value in its range (0..=1).
    pub fn fraction(self, s: &UserSettings) -> f32 {
        let (lo, hi, _) = self.range();
        ((self.get(s) - lo) / (hi - lo)).clamp(0.0, 1.0)
    }
}

/// An on/off setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingToggle {
    SpeedFov,
    StrafeRoll,
    Mute,
    /// Speedrun split timer plate on Standard runs.
    SpeedrunSplits,
}

impl SettingToggle {
    pub fn label(self) -> &'static str {
        match self {
            SettingToggle::SpeedFov => "DYNAMIC SPEED FOV",
            SettingToggle::StrafeRoll => "STRAFE ROLL",
            SettingToggle::Mute => "MUTE ALL AUDIO",
            SettingToggle::SpeedrunSplits => "SPEEDRUN SPLITS",
        }
    }

    fn slot(self, s: &mut UserSettings) -> &mut bool {
        match self {
            SettingToggle::SpeedFov => &mut s.speed_fov_enabled,
            SettingToggle::StrafeRoll => &mut s.strafe_roll_enabled,
            SettingToggle::Mute => &mut s.audio_muted,
            SettingToggle::SpeedrunSplits => &mut s.speedrun_splits,
        }
    }

    pub fn get(self, s: &UserSettings) -> bool {
        match self {
            SettingToggle::SpeedFov => s.speed_fov_enabled,
            SettingToggle::StrafeRoll => s.strafe_roll_enabled,
            SettingToggle::Mute => s.audio_muted,
            SettingToggle::SpeedrunSplits => s.speedrun_splits,
        }
    }

    /// Whether the effect this toggle gates is live (mute inverts).
    pub fn effect_live(self, s: &UserSettings) -> bool {
        if self == SettingToggle::Mute { !s.audio_muted } else { self.get(s) }
    }

    pub fn state_text(self, s: &UserSettings) -> &'static str {
        match (self, self.get(s)) {
            (SettingToggle::Mute, true) => "MUTED",
            (SettingToggle::Mute, false) => "LIVE",
            (_, true) => "ON",
            (_, false) => "OFF",
        }
    }
}

/// Something clickable on the Settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsTarget {
    Down(SettingKnob),
    Up(SettingKnob),
    Toggle(SettingToggle),
    Style(CrosshairStyle),
    Color(CrosshairColor),
    ResetDefaults,
}

/// Apply a Settings click (no saving; see [`commit_settings_edit`]).
pub fn apply_settings_edit(s: &mut UserSettings, t: SettingsTarget) -> EditOutcome {
    s.sanitize();
    match t {
        SettingsTarget::Down(k) | SettingsTarget::Up(k) => {
            let (lo, hi, step) = k.range();
            let dir = if matches!(t, SettingsTarget::Up(_)) { 1.0 } else { -1.0 };
            let slot = k.slot(s);
            // Snap to the step grid so repeated clicks never drift.
            let want = (((*slot + dir * step) / step).round() * step).clamp(lo, hi);
            if (want - *slot).abs() < 1e-4 {
                let edge = if dir > 0.0 { "MAX" } else { "MIN" };
                EditOutcome::Unchanged(format!("{} already at {edge}", k.label()))
            } else {
                *slot = want;
                EditOutcome::Changed(format!("{} -> {}", k.label(), k.value_text(s)))
            }
        }
        SettingsTarget::Toggle(g) => {
            let slot = g.slot(s);
            *slot = !*slot;
            EditOutcome::Changed(format!("{}: {}", g.label(), g.state_text(s)))
        }
        SettingsTarget::Style(st) => {
            if s.crosshair_style == st {
                EditOutcome::Unchanged(format!("Reticle already {}", st.name()))
            } else {
                s.crosshair_style = st;
                EditOutcome::Changed(format!("Reticle style -> {}", st.name()))
            }
        }
        SettingsTarget::Color(c) => {
            if s.crosshair_color == c {
                EditOutcome::Unchanged(format!("Reticle already {}", c.name()))
            } else {
                s.crosshair_color = c;
                EditOutcome::Changed(format!("Reticle color -> {}", c.name()))
            }
        }
        SettingsTarget::ResetDefaults => {
            if *s == UserSettings::default() {
                EditOutcome::Unchanged("Settings already at factory defaults".to_string())
            } else {
                *s = UserSettings::default();
                EditOutcome::Changed("Settings reset to factory defaults".to_string())
            }
        }
    }
}

/// Apply a Settings click to the profile and persist it immediately (same
/// save / retry semantics as [`commit_meta_edit`]).
pub fn commit_settings_edit(store: &mut ProfileStore, t: SettingsTarget) -> EditOutcome {
    let out = apply_settings_edit(&mut store.profile.settings, t);
    let changed = matches!(out, EditOutcome::Changed(_));
    if changed || store.dirty {
        if let Err(e) = store.try_save() {
            warn!("profile save failed (marked dirty): {e}");
            return EditOutcome::Denied(format!("SAVE FAILED ({e}) - unsaved changes kept, retry anytime"));
        }
        if !changed {
            return EditOutcome::Changed("Saved pending profile changes".to_string());
        }
    }
    out
}

/// Clickable control on the Settings screen.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsControl(pub SettingsTarget);

/// Live Settings readouts.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsLabel {
    Knob(SettingKnob),
    Toggle(SettingToggle),
    /// Label inside a clickable control (follows the control's text color).
    Control(SettingsTarget),
    PreviewCaption,
}

/// Pip `i` of a knob's meter.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPip(pub SettingKnob, pub u8);

/// Container of the live reticle preview (rebuilt on reticle changes).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReticlePreview(pub CrosshairStyle, pub CrosshairColor);

/// Pips per knob meter.
pub const SETTINGS_PIPS: u8 = 12;

/// Lit pips of a knob meter.
pub fn lit_settings_pips(k: SettingKnob, s: &UserSettings) -> u8 {
    (k.fraction(s) * SETTINGS_PIPS as f32).round() as u8
}

/// The control under the cursor this frame.
#[derive(Resource, Debug, Clone, Copy, Default)]
struct SettingsHover(Option<SettingsTarget>);

/// Whether a control currently reads "selected / on".
fn control_active(s: &UserSettings, t: SettingsTarget) -> bool {
    match t {
        SettingsTarget::Toggle(g) => g.get(s),
        SettingsTarget::Style(st) => s.crosshair_style == st,
        SettingsTarget::Color(c) => s.crosshair_color == c,
        SettingsTarget::Down(_) | SettingsTarget::Up(_) | SettingsTarget::ResetDefaults => false,
    }
}

/// `(fill, rail, edge, text)` of a control.
fn control_look(t: SettingsTarget, active: bool, hovered: bool) -> (Color, Color, Color, Color) {
    let accent = match t {
        SettingsTarget::ResetDefaults => PUNK_RED,
        SettingsTarget::Toggle(SettingToggle::Mute) => PUNK_RED,
        SettingsTarget::Toggle(_) => ACID_GREEN,
        SettingsTarget::Style(_) => ARC_CYAN,
        SettingsTarget::Color(c) => c.color().with_alpha(1.0),
        SettingsTarget::Down(_) | SettingsTarget::Up(_) => HAZARD_YELLOW,
    };
    if let SettingsTarget::Color(_) = t {
        // The equipped (or hovered) swatch shows full paint in a bone frame;
        // the others sit dimmed, so the pick reads even on the white swatch.
        let lit = active || hovered;
        let fill = if lit { accent } else { accent.mix(&INK_BLACK, 0.4) };
        let edge = if active { BONE_TEXT } else if hovered { accent } else { STEEL_BORDER };
        return (fill, if active { BONE_TEXT } else { accent.mix(&INK_BLACK, 0.55) }, edge, INK_BLACK);
    }
    match (active, hovered) {
        (_, true) => (accent, accent, BONE_TEXT, INK_BLACK),
        (true, false) => (accent.mix(&INK_BLACK, 0.72), accent, accent.with_alpha(0.8), accent.mix(&BONE_TEXT, 0.35)),
        (false, false) => (IRON_MID, STEEL_BORDER, STEEL_BORDER, BONE_TEXT),
    }
}

/// Settings screen plugin pieces (registered by [`MetaUiPlugin`]).
fn add_settings_systems(app: &mut App) {
    app.init_resource::<SettingsHover>()
        .add_systems(OnEnter(AppState::Settings), (reset_feedback, spawn_settings_screen).chain())
        .add_systems(Update, (settings_clicks, refresh_settings_ui).chain().run_if(in_state(AppState::Settings)));
}

fn settings_clicks(
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    controls: Query<(&ComputedNode, &UiGlobalTransform, &SettingsControl)>,
    mut store: ResMut<ProfileStore>,
    mut fb: ResMut<MetaFeedback>,
    mut hover: ResMut<SettingsHover>,
    mut sfx: ResMut<SfxQueue>,
) {
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    let now = cursor.and_then(|c| controls.iter().find(|(n, tf, _)| n.contains_point(**tf, c)).map(|(_, _, ctl)| ctl.0));
    if hover.0 != now {
        hover.0 = now;
    }
    if let Some(t) = hover.0
        && mouse.just_released(MouseButton::Left)
    {
        let (text, color) = match commit_settings_edit(&mut store, t) {
            EditOutcome::Changed(s) => {
                sfx.play(SfxEvent::UpgradeSelect);
                (s, COLOR_SIGNAL_GREEN)
            }
            EditOutcome::Denied(s) => {
                sfx.play(SfxEvent::Denied);
                (s, COLOR_SIGNAL_RED)
            }
            EditOutcome::Unchanged(s) => (s, COLOR_CYAN),
        };
        *fb = MetaFeedback { text, color };
    }
}

/// Restyle controls, meters and readouts from the live settings; rebuild the
/// reticle preview when its style / color changed.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn refresh_settings_ui(
    mut commands: Commands,
    store: Res<ProfileStore>,
    hover: Res<SettingsHover>,
    mut controls: Query<(&SettingsControl, &mut BackgroundColor, &mut BorderColor), Without<SettingsPip>>,
    mut pips: Query<(&SettingsPip, &mut BackgroundColor), Without<SettingsControl>>,
    mut labels: Query<(&SettingsLabel, &mut Text, &mut TextColor)>,
    mut preview: Query<(Entity, &mut ReticlePreview)>,
    fresh: Query<(), Added<SettingsLabel>>,
) {
    let s = store.profile.settings.sanitized();
    for (ctl, mut bg, mut border) in &mut controls {
        let (fill, rail, edge, _) = control_look(ctl.0, control_active(&s, ctl.0), hover.0 == Some(ctl.0));
        if bg.0 != fill {
            bg.0 = fill;
        }
        let nb = plate_border(rail, edge);
        if *border != nb {
            *border = nb;
        }
    }
    for (pip, mut bg) in &mut pips {
        let live = pip.0.gate().is_none_or(|g| g.effect_live(&s));
        let lit = pip.1 < lit_settings_pips(pip.0, &s);
        let accent = if pip.0 == SettingKnob::Volume { ACID_GREEN } else { ARC_CYAN };
        let c = match (lit, live) {
            (true, true) => accent,
            (true, false) => ASH_DIM.with_alpha(0.55),
            (false, _) => Color::srgba(1.0, 1.0, 1.0, 0.07),
        };
        if bg.0 != c {
            bg.0 = c;
        }
    }
    let retext = store.is_changed() || !fresh.is_empty();
    for (label, mut txt, mut col) in &mut labels {
        let (value, color): (Option<String>, Color) = match *label {
            SettingsLabel::Knob(k) => {
                let live = k.gate().is_none_or(|g| g.effect_live(&s));
                (retext.then(|| k.value_text(&s)), if live { BONE_TEXT } else { ASH_DIM })
            }
            SettingsLabel::Toggle(g) => {
                let on = g.effect_live(&s);
                (retext.then(|| g.state_text(&s).to_string()), if on { ACID_GREEN } else { PUNK_RED })
            }
            SettingsLabel::Control(t) => (None, control_look(t, control_active(&s, t), hover.0 == Some(t)).3),
            SettingsLabel::PreviewCaption => {
                (retext.then(|| format!("{} // {}", s.crosshair_style.name(), s.crosshair_color.name())), s.crosshair_color.color().with_alpha(1.0))
            }
        };
        if let Some(v) = value {
            set_text(&mut txt, &v);
        }
        if col.0 != color {
            col.0 = color;
        }
    }
    for (e, mut cur) in &mut preview {
        let want = ReticlePreview(s.crosshair_style, s.crosshair_color);
        if *cur != want {
            *cur = want;
            commands.entity(e).despawn_children();
            spawn_crosshair(&mut commands, e, want.0, want.1.color(), PREVIEW_SCALE);
        }
    }
}

/// Reticle preview magnification.
const PREVIEW_SCALE: f32 = 2.5;
/// Pip meter width: 12 whole-pixel pips (10 px) + 11 gaps (2 px) + frame and
/// padding (4 px), so every pip and gap lands on the pixel grid.
const METER_WIDTH: f32 = 146.0;
const SETTINGS_WIDTH: f32 = 1120.0;

/// A clickable Settings plate holding one label.
fn settings_button(commands: &mut Commands, parent: Entity, t: SettingsTarget, label: &str, font: f32, pad: (f32, f32)) -> Entity {
    commands
        .spawn((
            ChildOf(parent),
            SettingsControl(t),
            Node {
                border: plate_rect((3.0, 1.0, 1.0, 3.0)),
                padding: UiRect::axes(Val::Px(pad.0), Val::Px(pad.1)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_shrink: 0.0,
                ..default()
            },
            plate_border(STEEL_BORDER, STEEL_BORDER),
            BackgroundColor(IRON_MID),
            hard_shadow(3.0),
            children![(SettingsLabel::Control(t), text(label, font, BONE_TEXT))],
        ))
        .id()
}

/// Label column of a settings row: name + dim hint.
fn setting_name(commands: &mut Commands, row_e: Entity, name: &str, hint: &str) {
    commands.spawn((
        ChildOf(row_e),
        Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, flex_basis: Val::Px(0.0), row_gap: Val::Px(1.0), ..default() },
        children![text(name, 14.0, BONE_TEXT), text(hint, 11.0, ASH_DIM)],
    ));
}

/// Recessed settings row (rail on the left).
fn setting_row(commands: &mut Commands, parent: Entity, rail: Color) -> Entity {
    commands
        .spawn((
            ChildOf(parent),
            Node {
                width: Val::Percent(100.0),
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                border: plate_rect((3.0, 1.0, 1.0, 1.0)),
                ..default()
            },
            BorderColor { left: rail, ..inset_border() },
            BackgroundColor(INK_BLACK.with_alpha(0.5)),
        ))
        .id()
}

/// `[-] [pip meter + value] [+]` row of a knob.
fn knob_row(commands: &mut Commands, parent: Entity, k: SettingKnob, s: &UserSettings) {
    let r = setting_row(commands, parent, HAZARD_YELLOW.with_alpha(0.8));
    setting_name(commands, r, k.label(), k.hint());
    settings_button(commands, r, SettingsTarget::Down(k), "-", 18.0, (11.0, 0.0));
    let meter = commands
        .spawn((
            ChildOf(r),
            Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(2.0), width: Val::Px(METER_WIDTH), flex_shrink: 0.0, ..default() },
        ))
        .id();
    commands.spawn((ChildOf(meter), SettingsLabel::Knob(k), text(k.value_text(s), 15.0, BONE_TEXT)));
    let track = commands
        .spawn((
            ChildOf(meter),
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(9.0),
                column_gap: Val::Px(2.0),
                padding: UiRect::all(Val::Px(1.0)),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            inset_border(),
            BackgroundColor(INK_BLACK),
        ))
        .id();
    for i in 0..SETTINGS_PIPS {
        commands.spawn((ChildOf(track), SettingsPip(k, i), Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() }, BackgroundColor(Color::NONE)));
    }
    settings_button(commands, r, SettingsTarget::Up(k), "+", 18.0, (11.0, 0.0));
}

/// Toggle row: name | live state | switch.
fn toggle_row(commands: &mut Commands, parent: Entity, g: SettingToggle, hint: &str, s: &UserSettings) {
    let r = setting_row(commands, parent, ACID_GREEN.with_alpha(0.8));
    setting_name(commands, r, g.label(), hint);
    commands.spawn((
        ChildOf(r),
        Node { width: Val::Px(70.0), justify_content: JustifyContent::Center, flex_shrink: 0.0, ..default() },
        children![(SettingsLabel::Toggle(g), text(g.state_text(s), 15.0, ACID_GREEN))],
    ));
    settings_button(commands, r, SettingsTarget::Toggle(g), "TOGGLE", 13.0, (10.0, 3.0));
}

fn spawn_settings_screen(
    mut commands: Commands,
    store: Res<ProfileStore>,
    fb: Res<MetaFeedback>,
    ret: Res<SettingsReturn>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
    player_cams: Query<(), With<PlayerCamera>>,
) {
    let tex = tex.as_deref();
    let s = store.profile.settings.sanitized();
    let mid_run = ret.0 == AppState::Paused;
    // Mid-run the frozen player camera keeps rendering the bunker behind a
    // translucent overlay; from the main menu the screen brings its own camera.
    let root = if player_cams.is_empty() {
        let msaa = msaa.map(|m| m.0).unwrap_or(bevy::render::view::Msaa::Sample2);
        meta_root(&mut commands, AppState::Settings, tex, msaa)
    } else {
        punk_screen_root(&mut commands, AppState::Settings, tex, Color::srgba(0.02, 0.015, 0.012, 0.8), 0.3)
    };
    let phone = terminal_chassis(&mut commands, root, tex, SETTINGS_WIDTH, false);

    let header = screen_header(&mut commands, phone, "UAC-PHONE // SYSTEM SETTINGS", &fb);
    if mid_run {
        commands.spawn((ChildOf(header), stamped_tag("MID-RUN // SIM PAUSED", HAZARD_YELLOW, INK_BLACK, 11.0)));
    } else {
        commands.spawn((ChildOf(header), hollow_stamp("SAVED TO PROFILE", ASH_DIM, 11.0)));
    }

    let split = commands
        .spawn((ChildOf(phone), Node { width: Val::Percent(100.0), column_gap: Val::Px(10.0), align_items: AlignItems::Stretch, ..default() }))
        .id();

    // Left: input & camera.
    let (input, input_head) =
        section_crate(&mut commands, split, "INPUT & CAMERA", ARC_CYAN, Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() });
    commands.spawn((ChildOf(input_head), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(input_head), text("APPLIES INSTANTLY", 11.0, ASH_DIM)));
    knob_row(&mut commands, input, SettingKnob::Sensitivity, &s);
    knob_row(&mut commands, input, SettingKnob::BaseFov, &s);
    toggle_row(&mut commands, input, SettingToggle::SpeedFov, "widen the view at speed & on dash", &s);
    knob_row(&mut commands, input, SettingKnob::SpeedFov, &s);
    toggle_row(&mut commands, input, SettingToggle::StrafeRoll, "lean the camera into strafes", &s);
    knob_row(&mut commands, input, SettingKnob::StrafeRoll, &s);

    // Right: audio & reticle.
    let (audio, audio_head) =
        section_crate(&mut commands, split, "AUDIO & RETICLE", HAZARD_YELLOW, Node { flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() });
    commands.spawn((ChildOf(audio_head), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(audio_head), text("SFX BUS // HUD", 11.0, ASH_DIM)));
    knob_row(&mut commands, audio, SettingKnob::Volume, &s);
    toggle_row(&mut commands, audio, SettingToggle::Mute, "silence every sound effect", &s);
    toggle_row(&mut commands, audio, SettingToggle::SpeedrunSplits, "HUD timer + PB deltas on standard runs", &s);

    // Reticle block: style pills + color swatches beside the live preview.
    let reticle = commands
        .spawn((
            ChildOf(audio),
            // Grows to the crate's stretched height so the right column ends flush with the left.
            Node { width: Val::Percent(100.0), flex_grow: 1.0, column_gap: Val::Px(10.0), align_items: AlignItems::Stretch, ..default() },
        ))
        .id();
    let pickers = commands
        .spawn((ChildOf(reticle), Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, flex_basis: Val::Px(0.0), row_gap: Val::Px(6.0), ..default() }))
        .id();
    let style_row = setting_row(&mut commands, pickers, ARC_CYAN.with_alpha(0.8));
    commands.entity(style_row).insert(Node {
        width: Val::Percent(100.0),
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Stretch,
        row_gap: Val::Px(7.0),
        padding: UiRect::axes(Val::Px(8.0), Val::Px(6.0)),
        border: plate_rect((3.0, 1.0, 1.0, 1.0)),
        ..default()
    });
    commands.spawn((ChildOf(style_row), text("CROSSHAIR STYLE", 14.0, BONE_TEXT)));
    let pills = commands.spawn((ChildOf(style_row), Node { column_gap: Val::Px(6.0), flex_wrap: FlexWrap::Wrap, row_gap: Val::Px(6.0), ..default() })).id();
    for st in CrosshairStyle::ALL {
        settings_button(&mut commands, pills, SettingsTarget::Style(st), st.name(), 12.0, (8.0, 3.0));
    }
    let color_row = setting_row(&mut commands, pickers, HAZARD_YELLOW.with_alpha(0.8));
    commands.entity(color_row).insert(Node {
        width: Val::Percent(100.0),
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Stretch,
        row_gap: Val::Px(7.0),
        padding: UiRect::axes(Val::Px(8.0), Val::Px(6.0)),
        border: plate_rect((3.0, 1.0, 1.0, 1.0)),
        ..default()
    });
    commands.spawn((ChildOf(color_row), text("CROSSHAIR COLOR", 14.0, BONE_TEXT)));
    let swatches = commands.spawn((ChildOf(color_row), Node { column_gap: Val::Px(7.0), ..default() })).id();
    for c in CrosshairColor::ALL {
        commands.spawn((
            ChildOf(swatches),
            SettingsControl(SettingsTarget::Color(c)),
            Node { width: Val::Px(38.0), height: Val::Px(24.0), border: plate_rect((3.0, 1.0, 1.0, 3.0)), flex_shrink: 0.0, ..default() },
            plate_border(STEEL_BORDER, STEEL_BORDER),
            BackgroundColor(c.color()),
            hard_shadow(2.0),
        ));
    }

    // Live preview: a recessed scope well with the real HUD reticle builder.
    let well = commands
        .spawn((
            ChildOf(reticle),
            Node {
                width: Val::Px(150.0),
                flex_shrink: 0.0,
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(6.0)),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            inset_border(),
            BackgroundColor(Color::srgb(0.16, 0.035, 0.03)),
        ))
        .id();
    commands.spawn((ChildOf(well), text("LIVE PREVIEW", 10.0, ASH_DIM)));
    let side = CROSSHAIR_BOX * PREVIEW_SCALE;
    let box_e = commands
        .spawn((
            ChildOf(well),
            ReticlePreview(s.crosshair_style, s.crosshair_color),
            Node { width: Val::Px(side), height: Val::Px(side), flex_shrink: 0.0, ..default() },
        ))
        .id();
    spawn_crosshair(&mut commands, box_e, s.crosshair_style, s.crosshair_color.color(), PREVIEW_SCALE);
    commands.spawn((ChildOf(well), SettingsLabel::PreviewCaption, text("", 11.0, BONE_TEXT)));

    // Footer.
    let f = commands
        .spawn((ChildOf(phone), Node { margin: UiRect { top: Val::Px(2.0), bottom: Val::Px(4.0), ..default() }, ..row(JustifyContent::Center, 24.0) }))
        .id();
    settings_button(&mut commands, f, SettingsTarget::ResetDefaults, "RESET DEFAULTS", 17.0, (16.0, 7.0));
    let back = if mid_run { "BACK TO PAUSE  [ESC]" } else { "BACK  [ESC]" };
    commands.spawn((ChildOf(f), punk_button(back, MenuAction::CloseSettings, 19.0, ButtonStyle::HeroAcid)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::RunUnlockSummary;
    use std::path::Path;

    #[test]
    fn risk_badge_scales_with_contracts() {
        assert_eq!(risk_badge_text(0), "STANDARD");
        assert_eq!(risk_badge_text(1), "RISK x1");
        assert_eq!(risk_badge_text(3), "RISK x3");
        assert_eq!(risk_badge_text(4), "MAX RISK x4");
        assert_eq!(risk_color(0), COLOR_DIM);
        assert_ne!(risk_color(1), risk_color(4));
    }

    #[test]
    fn loadout_summary_line_reflects_profile() {
        let mut p = PlayerProfile::default();
        assert_eq!(
            loadout_summary_line(&p),
            "OS: STOCK OS  |  SKIN: UAC STANDARD ORANGE  |  CATALOG: 12/12  |  CONTRACTS: 0/4  |  FEATS: 0/10"
        );
        p.set_jam_demo_unlock_all(true);
        p.equip_chip(StarterChip::SurveyOs);
        p.equip_skin(HandsetSkin::ChromeGold);
        p.toggle_contract(CarrierContract::RoamingCharges);
        p.toggle_catalog_app(UpgradeId::CopperAntenna);
        p.unlocked_feats.insert(FeatId::FirstExtraction);
        let s = loadout_summary_line(&p);
        assert!(s.starts_with("OS: SURVEY OS  |  SKIN: 5G CHROME GOLD"), "{s}");
        assert!(s.contains("CATALOG: 11/12") && s.contains("CONTRACTS: 1/4") && s.contains("FEATS: 1/10"), "{s}");
    }

    #[test]
    fn end_loadout_and_hud_tag_lines() {
        let mut l = RunLoadout::default();
        assert_eq!(end_loadout_line(&l), "OS: STOCK OS  |  SKIN: UAC STANDARD ORANGE  |  CONTRACTS: NONE");
        assert_eq!(loadout_tag_parts(&l), ("STOCK OS".to_string(), None));
        l.chip = StarterChip::SurveyOs;
        l.skin = HandsetSkin::ChromeGold;
        // Order-insensitive and deduplicated.
        l.contracts = vec![CarrierContract::PriorityJamming, CarrierContract::RoamingCharges, CarrierContract::PriorityJamming];
        assert_eq!(loadout_tag_parts(&l), ("SURVEY OS".to_string(), Some("ROAM PRIO".to_string())));
        assert_eq!(end_loadout_line(&l), "OS: SURVEY OS  |  SKIN: 5G CHROME GOLD  |  CONTRACTS: ROAM PRIO");
    }

    #[test]
    fn unlock_summary_lines_list_feats_then_rewards() {
        let empty = RunUnlockSummary::default();
        assert_eq!(unlock_summary_lines(&empty), vec![NO_UNLOCKS_LINE.to_string()]);
        assert!(unlock_reward_lines(&empty).is_empty());
        let s = RunUnlockSummary {
            new_feats: vec![FeatId::SignalScout, FeatId::BunkerCleared],
            new_apps: FeatId::SignalScout.reward_apps(),
            new_chips: vec![StarterChip::SurveyOs, StarterChip::OverclockRadio],
            new_skins: FeatId::BunkerCleared.reward_skins(),
            victory: true,
            skipped_reason: None,
        };
        assert_eq!(
            unlock_summary_lines(&s),
            vec![
                "FEAT: Signal Scout".to_string(),
                "FEAT: Full Signal".to_string(),
                "APPS LICENSED: Signal Spoofer".to_string(),
                "OS CHIPS: Survey OS, Overclock Radio".to_string(),
                "HANDSET SKINS: Stealth Carbon".to_string(),
            ]
        );
    }

    #[test]
    fn catalog_counter_and_stats_cells() {
        assert_eq!(catalog_counter_text(12), ("12/12 READY".to_string(), true));
        assert_eq!(catalog_counter_text(8), ("8/12 - AUTO-FILLS TO 12 ON DEPLOY".to_string(), false));
        assert_eq!(fmt_mmss(512.7), "08:32");
        let mut st = LifetimeStats { runs_played: 3, best_floor: 4, ..default() };
        let cells = lifetime_stat_cells(&st);
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[2], ("BEST FLOOR", "4/5".to_string()));
        assert_eq!(cells[6], ("BEST TIME", "--:--".to_string()));
        st.fastest_victory_secs = Some(61.0);
        assert_eq!(lifetime_stat_cells(&st)[6].1, "01:01");
    }

    #[test]
    fn settings_edits_step_clamp_toggle_and_reset() {
        let mut s = UserSettings::default();
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::Up(SettingKnob::Sensitivity)), EditOutcome::Changed(m) if m.contains("1.05x")));
        for _ in 0..100 {
            apply_settings_edit(&mut s, SettingsTarget::Up(SettingKnob::Sensitivity));
        }
        assert_eq!(s.mouse_sensitivity, 3.0);
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::Up(SettingKnob::Sensitivity)), EditOutcome::Unchanged(m) if m.contains("MAX")));
        for _ in 0..3 {
            apply_settings_edit(&mut s, SettingsTarget::Down(SettingKnob::BaseFov));
        }
        assert_eq!(s.base_fov_deg, 70.0);
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::Down(SettingKnob::BaseFov)), EditOutcome::Unchanged(m) if m.contains("MIN")));
        // Repeated volume steps never drift off the 10% grid.
        for _ in 0..7 {
            apply_settings_edit(&mut s, SettingsTarget::Down(SettingKnob::Volume));
        }
        assert!((s.audio_master_volume - 0.3).abs() < 1e-5);
        assert_eq!(SettingKnob::Volume.value_text(&s), "30%");
        assert_eq!(lit_settings_pips(SettingKnob::Volume, &s), 4);
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::Toggle(SettingToggle::Mute)), EditOutcome::Changed(m) if m.contains("MUTED")));
        assert!(s.audio_muted && !SettingToggle::Mute.effect_live(&s));
        apply_settings_edit(&mut s, SettingsTarget::Toggle(SettingToggle::SpeedFov));
        assert!(!s.speed_fov_enabled);
        assert_eq!(SettingToggle::SpeedFov.state_text(&s), "OFF");
        apply_settings_edit(&mut s, SettingsTarget::Style(CrosshairStyle::Circle));
        apply_settings_edit(&mut s, SettingsTarget::Color(CrosshairColor::Crimson));
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::Color(CrosshairColor::Crimson)), EditOutcome::Unchanged(_)));
        assert_eq!((s.crosshair_style, s.crosshair_color), (CrosshairStyle::Circle, CrosshairColor::Crimson));
        assert_eq!(SettingKnob::BaseFov.value_text(&s), "70 DEG");
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::ResetDefaults), EditOutcome::Changed(_)));
        assert_eq!(s, UserSettings::default());
        assert!(matches!(apply_settings_edit(&mut s, SettingsTarget::ResetDefaults), EditOutcome::Unchanged(_)));
        for k in SettingKnob::ALL {
            assert!(!k.label().is_empty() && !k.hint().is_empty());
            let (lo, hi, step) = k.range();
            assert!(lo < hi && step > 0.0 && k.fraction(&s) > 0.0);
        }

        // Commit saves straight to disk.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-profiles").join("settings_commit");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("profile.json");
        let mut store = ProfileStore::open(Some(path.clone()));
        assert!(matches!(commit_settings_edit(&mut store, SettingsTarget::Style(CrosshairStyle::Dot)), EditOutcome::Changed(_)));
        assert_eq!(crate::meta::load_profile(&path).settings.crosshair_style, CrosshairStyle::Dot);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_cells_and_endless_best_floor() {
        let mut st = LifetimeStats::default();
        let cells = record_stat_cells(&st);
        assert_eq!(cells.len(), 3);
        assert!(cells.iter().all(|(_, v)| stat_is_blank(v)), "{cells:?}");
        st.best_speed = 31.26;
        st.best_bhop_chain = 9;
        st.best_endless_floor = 12;
        st.best_floor = 12;
        let cells = record_stat_cells(&st);
        assert_eq!(cells[0].1, "31.3 M/S");
        assert_eq!(cells[1].1, "x9");
        assert_eq!(cells[2].1, "FL 12");
        assert_eq!(lifetime_stat_cells(&st)[2].1, "FL 12");
    }

    #[test]
    fn edits_respect_locks_and_the_twelve_card_cap() {
        let mut p = PlayerProfile::default();
        // Locked chip / skin / signature app are refused.
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::Chip(StarterChip::SurveyOs)), EditOutcome::Denied(m) if m.contains("Signal Scout")));
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::Skin(HandsetSkin::ChromeGold)), EditOutcome::Denied(_)));
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::App(UpgradeId::SignalSpoofer)), EditOutcome::Denied(_)));
        assert_eq!(status_text(&p, MetaTarget::Chip(StarterChip::SurveyOs)), "LOCKED: Signal Scout");
        assert_eq!(status_text(&p, MetaTarget::Chip(StarterChip::StockUac)), "EQUIPPED");
        assert_eq!(status_text(&p, MetaTarget::App(UpgradeId::SignalSpoofer)), "LOCKED");
        assert_eq!(status_text(&p, MetaTarget::App(UpgradeId::DualSimBarrels)), "IN POOL");
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::Chip(StarterChip::StockUac)), EditOutcome::Unchanged(_)));

        // Jam demo: everything unlocked; a full catalog refuses additions.
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::JamDemo), EditOutcome::Changed(_)));
        assert_eq!(status_text(&p, MetaTarget::JamDemo), "JAM DEMO UNLOCK ALL: ON");
        assert_eq!(status_text(&p, MetaTarget::App(UpgradeId::SignalSpoofer)), "BENCHED");
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::App(UpgradeId::SignalSpoofer)), EditOutcome::Denied(m) if m.contains("Catalog full (12/12)")));
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::App(UpgradeId::CopperAntenna)), EditOutcome::Changed(m) if m.contains("11/12")));
        assert_eq!(status_text(&p, MetaTarget::App(UpgradeId::CopperAntenna)), "AUTO-FILL");
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::App(UpgradeId::SignalSpoofer)), EditOutcome::Changed(_)));
        assert_eq!(status_text(&p, MetaTarget::App(UpgradeId::SignalSpoofer)), "IN POOL");
        assert_eq!(status_text(&p, MetaTarget::App(UpgradeId::CopperAntenna)), "BENCHED");
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::Chip(StarterChip::FaradayCase)), EditOutcome::Changed(_)));
        assert_eq!(p.equipped_chip, StarterChip::FaradayCase);

        // Reset + jam off revert to earned unlocks.
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::ResetCatalog), EditOutcome::Changed(_)));
        assert_eq!(p.normalized_catalog(), default_catalog());
        assert!(matches!(apply_meta_edit(&mut p, MetaTarget::ResetCatalog), EditOutcome::Unchanged(_)));
        apply_meta_edit(&mut p, MetaTarget::JamDemo);
        assert_eq!(p.equipped_chip, StarterChip::StockUac);

        // Contracts toggle.
        let t = MetaTarget::Contract(CarrierContract::OvertimeUplink);
        assert_eq!(status_text(&p, t), "[OFF]");
        assert!(matches!(apply_meta_edit(&mut p, t), EditOutcome::Changed(m) if m.ends_with("ACTIVE")));
        assert_eq!(status_text(&p, t), "[ACTIVE]");
        assert!(matches!(apply_meta_edit(&mut p, t), EditOutcome::Changed(m) if m.ends_with("OFF")));
    }

    #[test]
    fn catalog_states_read_apart_at_a_glance() {
        let look = |s| card_look(s, false, 0.0);
        let states = [TargetState::InPool, TargetState::AutoFill, TargetState::Benched, TargetState::Locked];
        // Every state has its own tag fill and its own card fill + rail.
        for (i, a) in states.iter().enumerate() {
            for b in &states[i + 1..] {
                let (la, lb) = (look(*a), look(*b));
                assert_ne!(la.tag.0, lb.tag.0, "{a:?} vs {b:?} tag");
                assert_ne!((la.bg, la.rail), (lb.bg, lb.rail), "{a:?} vs {b:?} card");
            }
        }
        // Committed states are solid tape with ink text; idle ones are hollow
        // (translucent well, colored text); every tag text stays legible.
        for s in [TargetState::InPool, TargetState::AutoFill, TargetState::Equipped, TargetState::Active] {
            let (fill, fg, _) = look(s).tag;
            assert_eq!((fill.alpha(), fg), (1.0, INK_BLACK), "{s:?}");
            assert!(crate::ui_kit::contrast_ratio(fg, fill) >= 4.5, "{s:?}");
        }
        for s in [TargetState::Benched, TargetState::Ready, TargetState::Off] {
            let (fill, fg, _) = look(s).tag;
            assert!(fill.alpha() < 1.0 && fg != INK_BLACK, "{s:?}");
            assert!(crate::ui_kit::contrast_ratio(fg, look(s).bg) >= 4.5, "{s:?}");
        }
        // A benched app recedes behind a pooled one; a locked one is dimmer still.
        let lum = |s| crate::ui_kit::luminance(look(s).name_color.with_alpha(1.0)) * look(s).name_color.alpha();
        assert!(lum(TargetState::InPool) > lum(TargetState::Benched) && lum(TargetState::Benched) > lum(TargetState::Locked));
        // The signed-contract stamp never sinks into the blood-red card.
        let active = look(TargetState::Active);
        assert!(crate::ui_kit::contrast_ratio(active.tag.0, active.bg) >= 7.0);
    }

    #[test]
    fn contract_trim_unlock_panel_and_blank_stats() {
        // Signed: full-strength tape. Off: the tape is hidden, the slot stays.
        assert_eq!(contract_trim_look(true).0, Color::WHITE);
        assert_eq!(contract_trim_look(false).0.alpha(), 0.0);
        assert!(contract_trim_look(false).1.alpha() > 0.0);
        // RUN UNLOCKS crate: phosphor for a win, salvage amber for a loss
        // that still unlocked something, a dead well otherwise.
        let feats = |victory| RunUnlockSummary { new_feats: vec![FeatId::FirstExtraction], victory, ..Default::default() };
        let (win, _) = unlock_panel_look(Some(&feats(true)), 0.0);
        let (loss, _) = unlock_panel_look(Some(&feats(false)), 0.0);
        let (none, rail) = unlock_panel_look(Some(&RunUnlockSummary::default()), 0.0);
        let (w, l) = (win.to_srgba(), loss.to_srgba());
        assert!(w.green > w.red && l.red > l.green, "win is green, salvage is amber");
        assert_eq!((none, rail), unlock_panel_look(None, 0.7));
        assert_ne!(none, win);
        // Unearned records are dimmed in the lifetime strip.
        assert!(stat_is_blank("0") && stat_is_blank("0/4") && stat_is_blank("--:--"));
        assert!(!stat_is_blank("11") && !stat_is_blank("4/5") && !stat_is_blank("08:32") && !stat_is_blank("10"));
    }

    #[test]
    fn short_names_fit_catalog_tiles() {
        for id in UpgradeId::ALL {
            // icon + space + short name at 13px mono must fit a ~210px tile.
            assert!(id.icon().len() + 1 + app_short_name(id).len() <= 26, "{id:?}");
        }
    }
}
