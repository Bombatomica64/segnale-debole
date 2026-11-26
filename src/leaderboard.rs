//! Global leaderboards, Authentik account & cloud profile sync (Milestone 26).
//!
//! The game talks to the `server/` backend at `signal.bombatomica64.dev`
//! (`/api/v1/...`):
//! - `GET /auth/me` once per session (entering `MainMenu`): when signed in the
//!   cloud profile is merged monotonically into the local [`ProfileStore`]
//!   ([`PlayerProfile::merge_from`]) and pushed back if this device had newer
//!   progress.
//! - Finished Daily runs (Victory / Game Over, never exhibitions) and Time
//!   Trial finishes POST to `/leaderboard/daily` / `/leaderboard/trial`; the
//!   end card shows a live [`SubmitStatus`] badge.
//! - Signed in, every finished run / callsign change PUTs `/profile`.
//! - `AppState::Leaderboard` lists the top 25 of today's Daily and of the
//!   Time Trial, with an editable callsign plate and SIGN IN / SIGN OUT.
//!
//! Transport ([`LeaderboardClient`]): browser `fetch` on wasm32 (cookies only
//! same-origin), a background thread + `ureq` (rustls) on desktop / Android.
//! Replies land in a shared inbox drained every frame, so nothing ever blocks
//! a frame. Networking is **off** under `cfg(test)`, in the headless test app
//! ([`NetworkDisabled`]), with a [`crate::bot_input::BotControl`], for bot /
//! screenshot runs (`SEGNALE_BOT*`, `SEGNALE_*SHOTS`) and with
//! `SEGNALE_LEADERBOARD_OFFLINE=1`: requests then resolve instantly as offline.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::gamepad::{pad_edit_text, DualPrompt, GamepadInput, InputMode, PadFocusLock, PAD_TEXT_HINT};
use crate::hud::{pad_prompt, text, MenuAction};
use crate::meta::{
    is_callsign_char, record_game_over, record_victory, sanitize_callsign, PlayerProfile, ProfileStore, RunLoadout, CALLSIGN_MAX, PROFILE_SCHEMA_VERSION,
};
use crate::state::{AppState, RunMode, RunStats};
use crate::trials::{daily_record, format_split_time, DailyDate, TimeTrialState};
use crate::ui_kit::{
    hard_shadow, plate_rect, punk_button, punk_screen_root, section_crate, stencil_title, terminal_chassis, ButtonStyle, UiKitTextures,
    ACID_GREEN, ARC_CYAN, ASH_DIM, BONE_TEXT, HAZARD_YELLOW, INK_BLACK, PUNK_RED, STEEL_BORDER,
};

/// Production host of the web build and the API.
pub const DEFAULT_BASE_URL: &str = "https://signal.bombatomica64.dev";
/// Native override of the API base URL.
pub const URL_ENV: &str = "SEGNALE_LEADERBOARD_URL";
/// Native bearer session token (desktop / Android have no browser cookie).
pub const TOKEN_ENV: &str = "SEGNALE_AUTH_TOKEN";
/// `1` disables all networking.
pub const OFFLINE_ENV: &str = "SEGNALE_LEADERBOARD_OFFLINE";
/// Rows of a board.
pub const TOP_N: usize = 25;
/// Rows per leaderboard page (the Top 25 spans 3 pages): keeps every row,
/// the page indicator, the status lines and BACK inside a 960x540 embed.
pub const LB_PAGE_SIZE: usize = 10;
/// Daily score cap accepted by the server.
pub const MAX_SCORE: u64 = 10_000_000;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One row of the Daily board.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyEntry {
    pub rank: u32,
    pub callsign: String,
    pub verified: bool,
    pub victory: bool,
    pub floor: u32,
    pub time_secs: f64,
    pub peak_speed: f64,
    pub max_bhop_chain: u32,
    pub score: u64,
}

/// One row of the Time Trial board.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrialEntry {
    pub rank: u32,
    pub callsign: String,
    pub verified: bool,
    pub time_secs: f64,
    pub splits: Vec<f64>,
    pub peak_speed: f64,
    pub max_bhop_chain: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct DailyBoardResp {
    date: String,
    entries: Vec<DailyEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TrialBoardResp {
    entries: Vec<TrialEntry>,
}

/// `POST /leaderboard/*` reply.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SubmitResp {
    pub accepted: bool,
    pub improved: bool,
    pub rank: u32,
    pub verified: bool,
}

/// `GET /auth/me` reply.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MeResp {
    pub authenticated: bool,
    pub auth_enabled: bool,
    pub username: Option<String>,
    pub callsign: Option<String>,
    pub profile: Option<serde_json::Value>,
}

/// `GET|PUT /profile` reply.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ProfileResp {
    callsign: Option<String>,
    profile: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ErrorResp {
    error: String,
}

/// `POST /leaderboard/daily` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DailySubmission {
    pub callsign: String,
    pub date: String,
    pub victory: bool,
    pub floor: u32,
    pub time_secs: f64,
    pub peak_speed: f64,
    pub max_bhop_chain: u32,
    pub score: u64,
}

/// `POST /leaderboard/trial` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrialSubmission {
    pub callsign: String,
    pub time_secs: f64,
    pub splits: Vec<f64>,
    pub peak_speed: f64,
    pub max_bhop_chain: u32,
}

/// Daily ranking score (tiebreak telemetry; the board orders by victory,
/// floor, then time): floors dominate, a victory adds a bonus that grows the
/// faster it was, kills add a little.
pub fn daily_score(victory: bool, floor: u32, kills: u32, time_secs: f32) -> u64 {
    let speed_bonus = if victory && time_secs.is_finite() { (3600.0 - time_secs).max(0.0) as u64 * 10 } else { 0 };
    let raw = floor as u64 * 10_000 + if victory { 50_000 } else { 0 } + kills as u64 * 25 + speed_bonus;
    raw.min(MAX_SCORE)
}

fn finite_clamp(v: f32, lo: f32, hi: f32) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) as f64 } else { lo as f64 }
}

/// The Daily submission of a finished run, or why it is not ranked. Mirrors
/// the server validation so obviously invalid runs never leave the device.
/// `victory` is this end card's outcome; an Endless continuation of a Daily
/// victory still counts as a victory.
pub fn daily_submission(run: &RunStats, victory: bool, callsign: &str) -> Result<DailySubmission, &'static str> {
    let Some(date) = run.mode.daily_date() else { return Err("NOT A DAILY RUN") };
    let victory = victory || run.endless;
    let t = run.time_elapsed;
    if !t.is_finite() || t < 5.0 {
        return Err("RUN TOO SHORT - NOT RANKED");
    }
    if t > 7200.0 {
        return Err("RUN TOO LONG - NOT RANKED");
    }
    let floor = run.floor.clamp(1, 200);
    if victory && (t < 15.0 || floor < 3) {
        return Err("INVALID VICTORY - NOT RANKED");
    }
    Ok(DailySubmission {
        callsign: sanitize_callsign(callsign),
        date: date.format_iso(),
        victory,
        floor,
        time_secs: t as f64,
        peak_speed: finite_clamp(run.peak_speed, 0.0, 65.0),
        max_bhop_chain: run.max_bhop_chain.min(500),
        score: daily_score(victory, floor, run.kills, t),
    })
}

/// The Time Trial submission of a finish, or why it is not ranked: 3
/// checkpoints + finish as strictly increasing cumulative splits ending at
/// the final time.
pub fn trial_submission(time: f32, splits: &[f32], peak_speed: f32, bhop: u32, callsign: &str) -> Result<TrialSubmission, &'static str> {
    if !time.is_finite() || !(5.0..=3600.0).contains(&time) {
        return Err("TIME OUT OF RANGE - NOT RANKED");
    }
    if splits.len() != 4 || splits.iter().any(|s| !s.is_finite() || *s <= 0.0) || splits.windows(2).any(|w| w[1] <= w[0]) {
        return Err("INVALID SPLITS - NOT RANKED");
    }
    if (splits[3] - time).abs() > 0.2 {
        return Err("INVALID SPLITS - NOT RANKED");
    }
    Ok(TrialSubmission {
        callsign: sanitize_callsign(callsign),
        time_secs: time as f64,
        splits: splits.iter().map(|s| *s as f64).collect(),
        peak_speed: finite_clamp(peak_speed, 0.0, 65.0),
        max_bhop_chain: bhop.min(500),
    })
}

/// A cloud profile JSON -> [`PlayerProfile`] (`None` for junk or a newer schema).
pub fn parse_cloud_profile(v: &serde_json::Value) -> Option<PlayerProfile> {
    if v.as_object().is_none_or(|o| o.is_empty()) {
        return None;
    }
    let mut p: PlayerProfile = serde_json::from_value(v.clone()).ok()?;
    if p.schema_version > PROFILE_SCHEMA_VERSION {
        return None;
    }
    p.sanitize();
    Some(p)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Which request a reply answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetKind {
    Me,
    PutProfile,
    DailyBoard,
    TrialBoard,
    SubmitDaily,
    SubmitTrial,
    Logout,
}

/// A finished request: HTTP `status` (`0` = offline / transport failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetReply {
    pub kind: NetKind,
    pub status: u16,
    pub body: String,
}

/// Marker: the app must never touch the network (inserted by the headless
/// test app before `CoreGameplayPlugin`).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct NetworkDisabled;

/// Whether this process may use the network at all (see the module docs).
pub fn network_allowed_by_env(vars: impl Iterator<Item = (String, String)>) -> bool {
    if cfg!(test) {
        return false;
    }
    for (k, v) in vars {
        if k == OFFLINE_ENV && v.trim() == "1" {
            return false;
        }
        if k.starts_with("SEGNALE_BOT") || (k.starts_with("SEGNALE_") && k.ends_with("SHOTS")) {
            return false;
        }
    }
    true
}

/// Process-level check: `std::env::vars()` panics on `wasm32-unknown-unknown`
/// (`library/std/src/sys/env/unsupported.rs`), where no process environment exists.
pub fn network_allowed_in_process() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        !cfg!(test)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        network_allowed_by_env(std::env::vars())
    }
}

/// Base URL + whether to send credentials (cookies) for the current host.
/// Same-origin (production host / local dev) uses relative URLs + cookies;
/// anywhere else (itch.io embeds) talks to production as a guest.
pub fn web_base_for_host(host: &str) -> (String, bool) {
    let host = host.trim().to_ascii_lowercase();
    if host == "signal.bombatomica64.dev" || host == "localhost" || host == "127.0.0.1" {
        (String::new(), true)
    } else {
        (DEFAULT_BASE_URL.to_string(), false)
    }
}

/// HTTP client handle + the shared reply inbox.
#[derive(Resource, Clone)]
pub struct LeaderboardClient {
    pub enabled: bool,
    /// `""` = same-origin (web).
    pub base: String,
    /// Native `Authorization: Bearer` session token.
    pub bearer: Option<String>,
    /// Web: send cookies (`RequestCredentials::Include`).
    pub credentials: bool,
    inbox: Arc<Mutex<Vec<NetReply>>>,
    /// Requests sent so far (tests / diagnostics).
    pub sent: u32,
}

impl LeaderboardClient {
    /// A client that never touches the network: every request resolves as
    /// offline on the next frame.
    pub fn offline() -> Self {
        LeaderboardClient { enabled: false, base: DEFAULT_BASE_URL.into(), bearer: None, credentials: false, inbox: default(), sent: 0 }
    }

    /// The platform client (or [`LeaderboardClient::offline`] when `enabled` is false).
    pub fn for_platform(enabled: bool) -> Self {
        let mut c = LeaderboardClient::offline();
        c.enabled = enabled;
        #[cfg(target_arch = "wasm32")]
        {
            let host = web_sys::window().and_then(|w| w.location().hostname().ok()).unwrap_or_default();
            let (base, credentials) = web_base_for_host(&host);
            c.base = base;
            c.credentials = credentials;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Ok(url) = std::env::var(URL_ENV)
                && !url.trim().is_empty()
            {
                c.base = url.trim().trim_end_matches('/').to_string();
            }
            c.bearer = std::env::var(TOKEN_ENV).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        }
        c
    }

    /// Absolute (or same-origin relative) URL of an API path.
    pub fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base, path)
    }

    /// Fire a request; its [`NetReply`] shows up in [`Self::drain`] later.
    pub fn send(&mut self, kind: NetKind, method: &'static str, path: &str, body: Option<String>) {
        self.sent += 1;
        if !self.enabled {
            self.push(NetReply { kind, status: 0, body: String::new() });
            return;
        }
        let url = self.url(path);
        let inbox = self.inbox.clone();
        #[cfg(target_arch = "wasm32")]
        {
            let credentials = self.credentials;
            wasm_bindgen_futures::spawn_local(async move {
                let (status, body) = web_fetch(method, &url, body, credentials).await.unwrap_or((0, String::new()));
                if let Ok(mut q) = inbox.lock() {
                    q.push(NetReply { kind, status, body });
                }
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let bearer = self.bearer.clone();
            let spawned = std::thread::Builder::new().name("leaderboard-net".into()).spawn({
                let inbox = inbox.clone();
                let kind = kind.clone();
                move || {
                    let (status, body) = native_request(method, &url, body, bearer.as_deref());
                    if let Ok(mut q) = inbox.lock() {
                        q.push(NetReply { kind, status, body });
                    }
                }
            });
            if spawned.is_err()
                && let Ok(mut q) = inbox.lock()
            {
                q.push(NetReply { kind, status: 0, body: String::new() });
            }
        }
    }

    fn push(&self, r: NetReply) {
        if let Ok(mut q) = self.inbox.lock() {
            q.push(r);
        }
    }

    /// Replies that arrived since the last call.
    pub fn drain(&self) -> Vec<NetReply> {
        self.inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
    }
}

/// Blocking native request (background thread only). Never panics.
#[cfg(not(target_arch = "wasm32"))]
fn native_request(method: &str, url: &str, body: Option<String>, bearer: Option<&str>) -> (u16, String) {
    use std::time::Duration;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .user_agent(concat!("segnale-debole/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let auth = bearer.map(|t| format!("Bearer {t}"));
    let result = match method {
        "GET" => {
            let mut req = agent.get(url);
            if let Some(a) = &auth {
                req = req.header("Authorization", a);
            }
            req.call()
        }
        _ => {
            let mut req = match method {
                "PUT" => agent.put(url),
                _ => agent.post(url),
            };
            if let Some(a) = &auth {
                req = req.header("Authorization", a);
            }
            match body {
                Some(b) => req.header("Content-Type", "application/json").send(b),
                None => req.send_empty(),
            }
        }
    };
    match result {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            (status, text)
        }
        Err(e) => {
            warn!("leaderboard: {method} {url} failed: {e}");
            (0, String::new())
        }
    }
}

/// Browser `fetch` (same-origin cookies when `credentials`).
#[cfg(target_arch = "wasm32")]
async fn web_fetch(method: &str, url: &str, body: Option<String>, credentials: bool) -> Option<(u16, String)> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_mode(web_sys::RequestMode::Cors);
    init.set_credentials(if credentials { web_sys::RequestCredentials::Include } else { web_sys::RequestCredentials::Omit });
    let has_body = body.is_some();
    if let Some(b) = body {
        init.set_body(&wasm_bindgen::JsValue::from_str(&b));
    }
    let req = web_sys::Request::new_with_str_and_init(url, &init).ok()?;
    if has_body {
        req.headers().set("Content-Type", "application/json").ok()?;
    }
    let window = web_sys::window()?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&req)).await.ok()?.dyn_into().ok()?;
    let status = resp.status();
    let text = JsFuture::from(resp.text().ok()?).await.ok()?.as_string().unwrap_or_default();
    Some((status, text))
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Account status (Authentik session).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Account {
    /// `/auth/me` not asked yet.
    #[default]
    Unknown,
    Checking,
    /// Not signed in (`auth_enabled`: the server offers SIGN IN).
    Guest { auth_enabled: bool },
    SignedIn { username: String },
    /// Server unreachable.
    Offline,
}

impl Account {
    pub fn signed_in(&self) -> bool {
        matches!(self, Account::SignedIn { .. })
    }
}

/// A board's load state.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Board<T> {
    #[default]
    Idle,
    Loading,
    Loaded(Vec<T>),
    Offline,
}

/// End-card submission state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SubmitStatus {
    /// Nothing to submit (Standard runs).
    #[default]
    Idle,
    Submitting,
    Ranked { rank: u32, improved: bool, verified: bool },
    /// Not sent / refused (exhibition, invalid, rate limited...).
    NotRanked(String),
    Offline,
}

/// End-card badge text and color of a [`SubmitStatus`] (`None` = no badge).
pub fn submit_badge(s: &SubmitStatus) -> Option<(String, Color)> {
    Some(match s {
        SubmitStatus::Idle => return None,
        SubmitStatus::Submitting => ("SUBMITTING...".into(), ARC_CYAN),
        SubmitStatus::Ranked { rank, improved: true, .. } => (format!("GLOBAL RANK #{rank} (NEW PB!)"), crate::hud::COLOR_GOLD),
        SubmitStatus::Ranked { rank, .. } => (format!("GLOBAL RANK #{rank}"), ACID_GREEN),
        SubmitStatus::NotRanked(why) => (why.clone(), HAZARD_YELLOW),
        SubmitStatus::Offline => ("OFFLINE - SAVED LOCALLY".into(), ASH_DIM),
    })
}

/// Leaderboard screen tab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LbTab {
    #[default]
    Daily,
    TimeTrial,
}

impl LbTab {
    pub fn other(self) -> Self {
        match self {
            LbTab::Daily => LbTab::TimeTrial,
            LbTab::TimeTrial => LbTab::Daily,
        }
    }
}

/// Everything the leaderboard UI shows.
#[derive(Resource, Debug, Clone, Default)]
pub struct LeaderboardState {
    pub account: Account,
    /// Cloud callsign/profile already folded in this session.
    pub me_requested: bool,
    pub daily: Board<DailyEntry>,
    /// ISO date of the Daily board.
    pub daily_date: String,
    pub trial: Board<TrialEntry>,
    pub submit: SubmitStatus,
    pub tab: LbTab,
    /// Callsign editor buffer (`Some` while typing).
    pub editing: Option<String>,
    /// One-line feedback under the header (sync errors, saved callsign...).
    pub notice: Option<String>,
    /// Leaderboard switches pressed this frame (routed from `hud::perform`).
    pub pending: Vec<MenuAction>,
    /// 0-based page of the active board ([`LB_PAGE_SIZE`] rows each).
    pub page: usize,
}

/// Pages a board of `rows` entries spans (at least 1).
pub fn page_count(rows: usize) -> usize {
    rows.min(TOP_N).div_ceil(LB_PAGE_SIZE).max(1)
}

impl LeaderboardState {
    /// Entries on the active tab's board (0 unless loaded).
    pub fn active_rows(&self) -> usize {
        match self.tab {
            LbTab::Daily => if let Board::Loaded(v) = &self.daily { v.len() } else { 0 },
            LbTab::TimeTrial => if let Board::Loaded(v) = &self.trial { v.len() } else { 0 },
        }
    }

    /// The shown page, clamped to the active board.
    pub fn current_page(&self) -> usize {
        self.page.min(page_count(self.active_rows()) - 1)
    }

    /// Step the page by `delta` (clamped; never wraps).
    pub fn turn_page(&mut self, delta: i32) {
        let last = page_count(self.active_rows()) as i32 - 1;
        let want = (self.current_page() as i32 + delta).clamp(0, last) as usize;
        if self.page != want {
            self.page = want;
        }
    }

    /// Switch DAILY <-> TIME TRIAL (back to page 1).
    pub fn switch_tab(&mut self) {
        self.tab = self.tab.other();
        self.page = 0;
    }
}

/// `PAGE 2/3` indicator of the active board.
pub fn page_label(lb: &LeaderboardState) -> String {
    format!("PAGE {}/{}", lb.current_page() + 1, page_count(lb.active_rows()))
}

/// Native / Android guests post scores anonymously (no browser sign-in):
/// a one-line explanation for the leaderboard header (`None` elsewhere).
pub fn guest_note(account: &Account) -> Option<&'static str> {
    guest_note_for(cfg!(target_arch = "wasm32"), account)
}

/// [`guest_note`] for an explicit platform (testable on every target).
pub fn guest_note_for(web: bool, account: &Account) -> Option<&'static str> {
    (!web && matches!(account, Account::Guest { .. })).then_some(GUEST_NOTE)
}

/// See [`guest_note`].
pub const GUEST_NOTE: &str = "GUEST MODE // SCORES POST AS GUEST (WEB SIGN-IN FOR CLOUD SYNC)";

/// Short player-facing badge of a refused / failed score submission.
pub fn submit_failure(status: u16, body: &str) -> SubmitStatus {
    match status {
        0 => SubmitStatus::Offline,
        429 => SubmitStatus::NotRanked("RATE LIMITED - TRY AGAIN SOON".into()),
        400..=499 => {
            let why = serde_json::from_str::<ErrorResp>(body).map(|e| e.error).unwrap_or_default();
            warn!("leaderboard: submission rejected ({status}): {why}");
            SubmitStatus::NotRanked(rejection_reason(&why))
        }
        _ => SubmitStatus::NotRanked("SERVER BUSY - SAVED LOCALLY".into()),
    }
}

/// Longest server reason shown on an end card.
const REASON_MAX: usize = 40;

/// `REJECTED: <SERVER REASON>` in uppercase ASCII, cut at a word boundary
/// (generic when the server gave no reason).
pub fn rejection_reason(error: &str) -> String {
    let clean: String = error
        .chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c.to_ascii_uppercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() {
        return "REJECTED BY SERVER - NOT RANKED".into();
    }
    let mut out = String::new();
    for word in clean.split(' ') {
        let extra = if out.is_empty() { word.len() } else { word.len() + 1 };
        if out.len() + extra > REASON_MAX {
            if out.is_empty() {
                out.push_str(&word[..REASON_MAX.min(word.len())]);
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    format!("REJECTED: {out}")
}

/// Account pill text for the menus.
pub fn account_label(account: &Account, callsign: &str) -> String {
    match account {
        Account::SignedIn { .. } => format!("AUTHENTIK: {callsign}"),
        Account::Unknown | Account::Checking => format!("GUEST: {callsign}  //  CHECKING..."),
        Account::Guest { .. } => format!("GUEST: {callsign}"),
        Account::Offline => format!("OFFLINE: {callsign}"),
    }
}

/// Whether the SIGN IN switch is offered (web only: native signs in with a
/// `SEGNALE_AUTH_TOKEN` bearer token).
pub fn sign_in_offered(account: &Account) -> bool {
    cfg!(target_arch = "wasm32") && matches!(account, Account::Guest { auth_enabled: true })
}

/// JSON body of `PUT /profile`.
pub fn profile_put_body(p: &PlayerProfile) -> String {
    serde_json::json!({ "callsign": sanitize_callsign(&p.callsign), "profile": p }).to_string()
}

fn put_profile(client: &mut LeaderboardClient, p: &PlayerProfile) {
    client.send(NetKind::PutProfile, "PUT", "/profile", Some(profile_put_body(p)));
}

/// Refresh both boards (Daily of today's UTC date).
pub fn request_boards(client: &mut LeaderboardClient, lb: &mut LeaderboardState) {
    let date = DailyDate::today_utc().format_iso();
    client.send(NetKind::DailyBoard, "GET", &format!("/leaderboard/daily?date={date}"), None);
    client.send(NetKind::TrialBoard, "GET", "/leaderboard/trial", None);
    lb.daily_date = date;
    lb.daily = Board::Loading;
    lb.trial = Board::Loading;
}

/// Fold a cloud profile into the local store; `true` when the cloud copy is
/// behind the merged result (push it back).
pub fn adopt_cloud_profile(store: &mut ProfileStore, cloud: Option<&serde_json::Value>, cloud_callsign: Option<&str>) -> bool {
    let before = store.profile.clone();
    let remote = cloud.and_then(parse_cloud_profile);
    if let Some(r) = &remote {
        store.profile.merge_from(r);
    }
    // The account's callsign is authoritative for verified entries.
    if let Some(cs) = cloud_callsign.map(sanitize_callsign) {
        store.profile.callsign = cs;
    }
    if store.profile != before
        && let Err(e) = store.try_save()
    {
        warn!("profile save after cloud merge failed: {e}");
    }
    match remote {
        Some(mut r) => {
            r.callsign = store.profile.callsign.clone();
            r != store.profile
        }
        None => true,
    }
}

/// Notice after a failed `POST /auth/logout` (still signed in).
pub const SIGN_OUT_FAILED: &str = "SIGN OUT FAILED - RETRY";
/// Notice after `PUT /profile` answered 401.
pub const SESSION_EXPIRED: &str = "SESSION EXPIRED - SIGN IN AGAIN";

/// Apply one reply to the state (and the local profile).
pub fn apply_reply(r: NetReply, lb: &mut LeaderboardState, store: &mut ProfileStore, client: &mut LeaderboardClient) {
    let ok = (200..300).contains(&r.status);
    match r.kind {
        NetKind::Me => {
            let me = ok.then(|| serde_json::from_str::<MeResp>(&r.body).ok()).flatten();
            match me {
                Some(me) if me.authenticated => {
                    lb.account = Account::SignedIn { username: me.username.clone().unwrap_or_default() };
                    if adopt_cloud_profile(store, me.profile.as_ref(), me.callsign.as_deref()) {
                        put_profile(client, &store.profile);
                    }
                }
                Some(me) => {
                    lb.account = Account::Guest { auth_enabled: me.auth_enabled };
                    if client.bearer.take().is_some() {
                        lb.notice = Some(SESSION_EXPIRED.into());
                    }
                }
                None => lb.account = Account::Offline,
            }
        }
        NetKind::PutProfile => match r.status {
            200..=299 => {
                if let Ok(p) = serde_json::from_str::<ProfileResp>(&r.body) {
                    // The server merges too (other devices): fold it back in.
                    adopt_cloud_profile(store, p.profile.as_ref(), p.callsign.as_deref());
                }
            }
            401 => {
                // The session expired server-side: drop the stale sign-in
                // (and a native bearer token) and say so.
                lb.account = Account::Guest { auth_enabled: true };
                client.bearer = None;
                lb.notice = Some(SESSION_EXPIRED.into());
            }
            _ => lb.notice = Some("CLOUD SYNC FAILED - PROGRESS SAVED LOCALLY".into()),
        },
        NetKind::DailyBoard => {
            lb.daily = match ok.then(|| serde_json::from_str::<DailyBoardResp>(&r.body).ok()).flatten() {
                Some(b) => {
                    if !b.date.is_empty() {
                        lb.daily_date = b.date;
                    }
                    Board::Loaded(b.entries.into_iter().take(TOP_N).collect())
                }
                None => Board::Offline,
            }
        }
        NetKind::TrialBoard => {
            lb.trial = match ok.then(|| serde_json::from_str::<TrialBoardResp>(&r.body).ok()).flatten() {
                Some(b) => Board::Loaded(b.entries.into_iter().take(TOP_N).collect()),
                None => Board::Offline,
            }
        }
        NetKind::SubmitDaily | NetKind::SubmitTrial => {
            lb.submit = match r.status {
                200..=299 => match serde_json::from_str::<SubmitResp>(&r.body) {
                    Ok(s) if s.accepted => SubmitStatus::Ranked { rank: s.rank, improved: s.improved, verified: s.verified },
                    _ => SubmitStatus::NotRanked("NOT RANKED".into()),
                },
                status => submit_failure(status, &r.body),
            };
        }
        // Only a confirmed sign-out clears the account: a failed request
        // keeps the session (and says so) so the player can retry.
        NetKind::Logout if ok => {
            lb.account = Account::Guest { auth_enabled: true };
            client.bearer = None;
            lb.notice = Some("SIGNED OUT".into());
        }
        NetKind::Logout => {
            warn!("leaderboard: sign out failed ({})", r.status);
            lb.notice = Some(SIGN_OUT_FAILED.into());
        }
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// A bot driver means a test / playtest: never touch the network.
fn disable_for_bots(bot: Option<Res<crate::bot_input::BotControl>>, mut client: ResMut<LeaderboardClient>) {
    if bot.is_some() && client.enabled {
        client.enabled = false;
    }
}

fn poll_replies(mut client: ResMut<LeaderboardClient>, mut lb: ResMut<LeaderboardState>, mut store: ResMut<ProfileStore>) {
    let replies = client.drain();
    for r in replies {
        apply_reply(r, &mut lb, &mut store, &mut client);
    }
}

/// `OnEnter(MainMenu)`: ask `/auth/me` once per session; clear the end-card badge.
fn on_main_menu(mut client: ResMut<LeaderboardClient>, mut lb: ResMut<LeaderboardState>) {
    lb.submit = SubmitStatus::Idle;
    if !lb.me_requested {
        lb.me_requested = true;
        lb.account = Account::Checking;
        client.send(NetKind::Me, "GET", "/auth/me", None);
    }
}

/// `OnEnter(GameOver | Victory)` after the profile recorder: rank Daily /
/// Time Trial runs and sync the profile when signed in.
pub fn auto_submit(
    state: Res<State<AppState>>,
    run: Res<RunStats>,
    loadout: Res<RunLoadout>,
    trial: Res<TimeTrialState>,
    store: Res<ProfileStore>,
    mut client: ResMut<LeaderboardClient>,
    mut lb: ResMut<LeaderboardState>,
) {
    let victory = *state.get() == AppState::Victory;
    let callsign = store.profile.callsign.clone();
    lb.submit = match run.mode {
        RunMode::Standard => SubmitStatus::Idle,
        _ if run.debug_skipped || loadout.jam_demo => SubmitStatus::NotRanked("EXHIBITION - NOT RANKED".into()),
        RunMode::Daily(_) => match daily_submission(&run, victory, &callsign) {
            Ok(sub) => {
                client.send(NetKind::SubmitDaily, "POST", "/leaderboard/daily", serde_json::to_string(&sub).ok());
                SubmitStatus::Submitting
            }
            Err(why) => SubmitStatus::NotRanked(why.into()),
        },
        RunMode::TimeTrial if !victory => SubmitStatus::Idle,
        RunMode::TimeTrial => {
            let time = trial.final_time().unwrap_or(trial.elapsed);
            match trial_submission(time, &trial.splits, run.peak_speed, run.max_bhop_chain, &callsign) {
                Ok(sub) => {
                    client.send(NetKind::SubmitTrial, "POST", "/leaderboard/trial", serde_json::to_string(&sub).ok());
                    SubmitStatus::Submitting
                }
                Err(why) => SubmitStatus::NotRanked(why.into()),
            }
        }
    };
    if lb.account.signed_in() {
        put_profile(&mut client, &store.profile);
    }
}

fn on_enter_leaderboard(mut client: ResMut<LeaderboardClient>, mut lb: ResMut<LeaderboardState>) {
    lb.editing = None;
    lb.notice = None;
    lb.page = 0;
    request_boards(&mut client, &mut lb);
}

fn on_exit_leaderboard(mut lb: ResMut<LeaderboardState>, mut lock: ResMut<PadFocusLock>) {
    lb.editing = None;
    lock.0 = false;
}

/// Commit the callsign editor: save locally, sync when signed in.
pub fn commit_callsign(lb: &mut LeaderboardState, store: &mut ProfileStore, client: &mut LeaderboardClient) {
    let Some(buf) = lb.editing.take() else { return };
    let cs = sanitize_callsign(&buf);
    if cs != buf {
        lb.notice = Some(format!("CALLSIGN NEEDS 2-12 OF A-Z 0-9 _ -  //  SAVED AS {cs}"));
    } else {
        lb.notice = Some(format!("CALLSIGN SAVED: {cs}"));
    }
    if store.profile.callsign != cs {
        store.profile.callsign = cs;
        if let Err(e) = store.try_save() {
            warn!("profile save failed (marked dirty for retry): {e}");
        }
        if lb.account.signed_in() {
            put_profile(client, &store.profile);
        }
    }
}

/// Start / toggle the callsign editor.
fn toggle_callsign_edit(lb: &mut LeaderboardState, store: &mut ProfileStore, client: &mut LeaderboardClient) {
    if lb.editing.is_some() {
        commit_callsign(lb, store, client);
    } else {
        lb.editing = Some(store.profile.callsign.clone());
        // The footer shows the editor controls for the active input mode.
        lb.notice = None;
    }
}

/// Keyboard callsign editor controls (see [`PAD_TEXT_HINT`] for the pad).
pub const CALLSIGN_KBM_HINT: &str = "TYPE A-Z 0-9 _ -  //  ENTER SAVE  //  ESC CANCEL";

/// Character set cycled by the controller in the callsign editor.
const PAD_CHARSET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";

/// Cycle a callsign character by `step` through [`PAD_CHARSET`].
pub fn cycle_callsign_char(c: char, step: i32) -> char {
    crate::gamepad::cycle_char(PAD_CHARSET, c, step)
}

/// SIGN IN (web: navigate to the Authentik login) / SIGN OUT.
fn account_action(lb: &mut LeaderboardState, client: &mut LeaderboardClient) {
    if lb.account.signed_in() {
        client.send(NetKind::Logout, "POST", "/auth/logout", None);
    } else if !sign_in_offered(&lb.account) {
        lb.notice = Some(format!("SIGN IN ON {DEFAULT_BASE_URL} (DESKTOP: SET {TOKEN_ENV})"));
    } else {
        #[cfg(target_arch = "wasm32")]
        if let Some(w) = web_sys::window() {
            let url = client.url("/auth/login");
            if client.credentials {
                let _ = w.location().assign(&url);
            } else {
                let _ = w.open_with_url_and_target(&url, "_blank");
            }
        }
    }
}

/// Leaderboard switches routed from `hud::perform` + the screen's keys.
#[allow(clippy::too_many_arguments)]
fn leaderboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    pad: Res<GamepadInput>,
    mut lb: ResMut<LeaderboardState>,
    mut store: ResMut<ProfileStore>,
    mut client: ResMut<LeaderboardClient>,
    mut lock: ResMut<PadFocusLock>,
    mut next: ResMut<NextState<AppState>>,
) {
    // Input of the frame that opened the editor never also commits it.
    let was_editing = lb.editing.is_some();
    let pending = std::mem::take(&mut lb.pending);
    for action in pending {
        match action {
            MenuAction::LbTab => lb.switch_tab(),
            MenuAction::LbPrevPage => lb.turn_page(-1),
            MenuAction::LbNextPage => lb.turn_page(1),
            MenuAction::LbRefresh => request_boards(&mut client, &mut lb),
            MenuAction::LbCallsign => toggle_callsign_edit(&mut lb, &mut store, &mut client),
            MenuAction::LbAccount => account_action(&mut lb, &mut client),
            _ => {}
        }
    }
    if was_editing && lb.editing.is_some() {
        let mut buf = lb.editing.clone().unwrap_or_default();
        let enter = keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]) || pad.confirm;
        if keys.just_pressed(KeyCode::Escape) || pad.back {
            lb.editing = None;
            lb.notice = Some("CALLSIGN EDIT CANCELLED".into());
        } else if enter {
            commit_callsign(&mut lb, &mut store, &mut client);
        } else {
            if keys.just_pressed(KeyCode::Backspace) {
                buf.pop();
            }
            for k in keys.get_just_pressed() {
                if let Some(c) = callsign_key_char(*k)
                    && buf.chars().count() < CALLSIGN_MAX
                {
                    buf.push(c);
                }
            }
            pad_edit_text(&mut buf, &pad, PAD_CHARSET, CALLSIGN_MAX);
            if lb.editing.as_deref() != Some(buf.as_str()) {
                lb.editing = Some(buf);
            }
        }
    } else if !was_editing && lb.editing.is_none() {
        let tab = keys.any_just_pressed([KeyCode::Tab, KeyCode::KeyQ, KeyCode::KeyE]) || pad.tab_prev || pad.tab_next;
        if tab {
            lb.switch_tab();
        }
        // Pages: [ / ] , Left / Right, PageUp / PageDown, or LT / RT.
        if keys.any_just_pressed([KeyCode::BracketLeft, KeyCode::ArrowLeft, KeyCode::PageUp]) || pad.page_prev {
            lb.turn_page(-1);
        }
        if keys.any_just_pressed([KeyCode::BracketRight, KeyCode::ArrowRight, KeyCode::PageDown]) || pad.page_next {
            lb.turn_page(1);
        }
        if keys.just_pressed(KeyCode::KeyR) || pad.alt_action {
            request_boards(&mut client, &mut lb);
        }
        if keys.just_pressed(KeyCode::KeyC) {
            toggle_callsign_edit(&mut lb, &mut store, &mut client);
        }
        if keys.just_pressed(KeyCode::Escape) || pad.back {
            next.set(AppState::MainMenu);
        }
    }
    let want = lb.editing.is_some();
    if lock.0 != want {
        lock.0 = want;
    }
}

/// `[A-Z0-9_-]` of a key press (letters uppercase).
pub fn callsign_key_char(k: KeyCode) -> Option<char> {
    let c = match k {
        KeyCode::Minus | KeyCode::NumpadSubtract => '-',
        KeyCode::Digit0 | KeyCode::Numpad0 => '0',
        KeyCode::Digit1 | KeyCode::Numpad1 => '1',
        KeyCode::Digit2 | KeyCode::Numpad2 => '2',
        KeyCode::Digit3 | KeyCode::Numpad3 => '3',
        KeyCode::Digit4 | KeyCode::Numpad4 => '4',
        KeyCode::Digit5 | KeyCode::Numpad5 => '5',
        KeyCode::Digit6 | KeyCode::Numpad6 => '6',
        KeyCode::Digit7 | KeyCode::Numpad7 => '7',
        KeyCode::Digit8 | KeyCode::Numpad8 => '8',
        KeyCode::Digit9 | KeyCode::Numpad9 => '9',
        _ => {
            // KeyA..KeyZ: Debug name "KeyX".
            let name = format!("{k:?}");
            let rest = name.strip_prefix("Key")?;
            let mut it = rest.chars();
            let c = it.next()?;
            if it.next().is_some() || !c.is_ascii_uppercase() {
                return None;
            }
            c
        }
    };
    is_callsign_char(c).then_some(c)
}

/// Core leaderboard / account / cloud-sync plugin (no UI).
pub struct LeaderboardPlugin;

impl Plugin for LeaderboardPlugin {
    fn build(&self, app: &mut App) {
        let enabled = !app.world().contains_resource::<NetworkDisabled>() && network_allowed_in_process();
        if !app.world().contains_resource::<LeaderboardClient>() {
            app.insert_resource(LeaderboardClient::for_platform(enabled));
        }
        app.init_resource::<LeaderboardState>()
            .add_systems(PreUpdate, (disable_for_bots, poll_replies).chain())
            .add_systems(OnEnter(AppState::MainMenu), on_main_menu)
            .add_systems(OnEnter(AppState::GameOver), auto_submit.after(record_game_over))
            .add_systems(OnEnter(AppState::Victory), auto_submit.after(record_victory))
            .add_systems(OnEnter(AppState::Leaderboard), on_enter_leaderboard)
            .add_systems(OnExit(AppState::Leaderboard), on_exit_leaderboard)
            .add_systems(PostUpdate, leaderboard_input.run_if(in_state(AppState::Leaderboard)));
    }
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

/// Live account pill (main menu header / leaderboard header).
#[derive(Component)]
pub struct AccountPillText;

/// End-card submission badge.
#[derive(Component)]
pub struct SubmitBadge;

/// The main menu's compact SIGN IN switch (never shows SIGN OUT).
#[derive(Component)]
pub struct MenuSignIn;

/// Leaderboard screen labels refreshed from [`LeaderboardState`].
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum LbLabel {
    Status,
    Notice,
    Message,
    LocalSummary,
    Page,
    Guest,
}

/// Table column header `i`.
#[derive(Component)]
struct LbHeader(usize);

/// Table row `i` (0-based) and its cell `(row, col)`.
#[derive(Component)]
struct LbRow(usize);

#[derive(Component)]
struct LbCell(usize, usize);

/// The table body / the empty-state message box.
#[derive(Component)]
struct LbTable;

#[derive(Component)]
struct LbMessageBox;

/// Column flex bases (px) of the 9-column table: the columns grow in
/// proportion to fill a wide chassis and shrink together (each cell clips)
/// down to a 960x540 embed. Sum + gaps ~ 830 px.
const COLS: [f32; 9] = [36.0, 150.0, 40.0, 104.0, 62.0, 96.0, 104.0, 96.0, 64.0];
const ROW_FONT: f32 = 13.0;
const ROW_H: f32 = 18.0;

/// A fluid table column (header or cell).
fn col_node(w: f32) -> Node {
    Node { flex_basis: Val::Px(w), flex_grow: w, flex_shrink: 1.0, min_width: Val::Px(0.0), overflow: Overflow::clip(), ..default() }
}
const VERIFIED_TAG: &str = "[ID]";

/// Column headers of a tab.
pub fn column_headers(tab: LbTab) -> [&'static str; 9] {
    match tab {
        LbTab::Daily => ["#", "CALLSIGN", "", "RESULT", "FLOOR", "SCORE", "TIME", "PEAK", "BHOP"],
        LbTab::TimeTrial => ["#", "CALLSIGN", "", "CP 1", "CP 2", "CP 3", "FINISH", "PEAK", "BHOP"],
    }
}

/// Cells of a Daily row.
pub fn daily_cells(e: &DailyEntry) -> [String; 9] {
    [
        format!("{}", e.rank),
        e.callsign.clone(),
        if e.verified { VERIFIED_TAG.into() } else { String::new() },
        if e.victory { "EXTRACTED".into() } else { "NO SERVICE".into() },
        format!("F{}", e.floor),
        format!("{}", e.score),
        format_split_time(e.time_secs as f32),
        format!("{:.1} m/s", e.peak_speed),
        format!("x{}", e.max_bhop_chain),
    ]
}

/// Cells of a Time Trial row.
pub fn trial_cells(e: &TrialEntry) -> [String; 9] {
    let split = |i: usize| e.splits.get(i).map(|s| format_split_time(*s as f32)).unwrap_or_else(|| "--".into());
    [
        format!("{}", e.rank),
        e.callsign.clone(),
        if e.verified { VERIFIED_TAG.into() } else { String::new() },
        split(0),
        split(1),
        split(2),
        format_split_time(e.time_secs as f32),
        format!("{:.1} m/s", e.peak_speed),
        format!("x{}", e.max_bhop_chain),
    ]
}

/// Local records shown when the boards are unreachable.
pub fn local_summary(p: &PlayerProfile, today: DailyDate) -> String {
    let daily = match daily_record(&p.daily_records, today) {
        Some(r) => format!(
            "TODAY'S DAILY: BEST F{}{}{}  ({} ATTEMPT{})",
            r.best_floor,
            if r.victory { "  //  EXTRACTED" } else { "" },
            r.best_time_secs.map(|t| format!(" IN {}", format_split_time(t))).unwrap_or_default(),
            r.attempts,
            if r.attempts == 1 { "" } else { "S" }
        ),
        None => "TODAY'S DAILY: NOT ATTEMPTED".into(),
    };
    let trial = match p.time_trial_best_secs {
        Some(t) => format!(
            "TIME TRIAL PB: {}  ({})",
            format_split_time(t),
            p.time_trial_best_splits.iter().map(|s| format_split_time(*s)).collect::<Vec<_>>().join(" / ")
        ),
        None => "TIME TRIAL PB: NONE ON RECORD".into(),
    };
    format!("{daily}\n{trial}")
}

/// Label of the callsign plate.
pub fn callsign_plate_label(lb: &LeaderboardState, callsign: &str, mode: InputMode) -> String {
    match &lb.editing {
        Some(buf) => format!("CALLSIGN: {buf}_"),
        None => format!("CALLSIGN: {callsign}{}", mode.pick("  [C]", "")),
    }
}

/// Label of the tab switch (names the board on screen).
pub fn tab_label(lb: &LeaderboardState, mode: InputMode) -> String {
    let date = if lb.daily_date.is_empty() { DailyDate::today_utc().format_iso() } else { lb.daily_date.clone() };
    let key = mode.pick("[TAB]", "[LB/RB]");
    match lb.tab {
        LbTab::Daily => format!("BOARD: DAILY {date}  {key}"),
        LbTab::TimeTrial => format!("BOARD: TIME TRIAL  {key}"),
    }
}

/// Account switch label (`None` = hidden).
pub fn account_switch_label(account: &Account) -> Option<&'static str> {
    if account.signed_in() {
        Some("SIGN OUT")
    } else if sign_in_offered(account) {
        Some("SIGN IN")
    } else {
        None
    }
}

/// Key hints of the leaderboard (mouse & keyboard).
pub const LB_HINTS: &str = "TAB: SWITCH BOARD  //  [ ] OR ARROWS: PAGE  //  C: EDIT CALLSIGN  //  R: REFRESH";
/// Key hints of the leaderboard (controller).
pub const LB_PAD_HINTS: &str = "LB / RB: SWITCH BOARD  //  LT / RT: PAGE  //  Y: REFRESH  //  A: SELECT  //  B: BACK";

/// Notice line: the callsign editor's controls while typing (per input
/// mode), else the live notice, else the key hints.
pub fn footer_line(lb: &LeaderboardState, mode: InputMode) -> (&str, Color) {
    match (&lb.editing, &lb.notice) {
        (Some(_), _) => (mode.pick(CALLSIGN_KBM_HINT, PAD_TEXT_HINT), HAZARD_YELLOW),
        (None, Some(n)) => (n.as_str(), HAZARD_YELLOW),
        (None, None) => (mode.pick(LB_HINTS, LB_PAD_HINTS), ASH_DIM),
    }
}

/// Status line of the active board.
fn board_status(lb: &LeaderboardState) -> (String, Color) {
    let (loading, offline, n) = match lb.tab {
        LbTab::Daily => (lb.daily == Board::Loading, lb.daily == Board::Offline, if let Board::Loaded(v) = &lb.daily { v.len() } else { 0 }),
        LbTab::TimeTrial => (lb.trial == Board::Loading, lb.trial == Board::Offline, if let Board::Loaded(v) = &lb.trial { v.len() } else { 0 }),
    };
    if loading {
        ("CONNECTING...".into(), ARC_CYAN)
    } else if offline {
        ("OFFLINE - LOCAL RECORDS ONLY".into(), PUNK_RED)
    } else {
        (format!("TOP {TOP_N}  //  {n} ON BOARD"), ACID_GREEN)
    }
}

/// Main menu header: account pill + (web) SIGN IN switch.
pub fn spawn_account_pill(commands: &mut Commands, parent: Entity, lb: &LeaderboardState, callsign: &str) {
    let row = commands.spawn((ChildOf(parent), Node { column_gap: Val::Px(6.0), align_items: AlignItems::Center, ..default() })).id();
    commands.spawn((
        ChildOf(row),
        Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(1.0)), border: plate_rect((3.0, 1.0, 1.0, 1.0)), ..default() },
        BorderColor { left: ARC_CYAN, top: STEEL_BORDER, right: STEEL_BORDER, bottom: STEEL_BORDER },
        BackgroundColor(INK_BLACK.with_alpha(0.7)),
        children![(AccountPillText, text(account_label(&lb.account, callsign), 11.0, ARC_CYAN))],
    ));
    let sign = commands.spawn((ChildOf(row), MenuSignIn, punk_button("SIGN IN", MenuAction::LbAccount, 11.0, ButtonStyle::PrimaryAcid))).id();
    if account_switch_label(&lb.account) != Some("SIGN IN") {
        commands.entity(sign).insert(Node { display: Display::None, ..default() });
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_leaderboard_screen(
    mut commands: Commands,
    store: Res<ProfileStore>,
    lb: Res<LeaderboardState>,
    mode: Option<Res<InputMode>>,
    tex: Option<Res<UiKitTextures>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
) {
    let tex = tex.as_deref();
    let mode = mode.map(|m| *m).unwrap_or_default();
    let msaa = msaa.map(|m| m.0).unwrap_or(bevy::render::view::Msaa::Sample2);
    commands.spawn((
        DespawnOnExit(AppState::Leaderboard),
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::Custom(INK_BLACK), ..default() },
        msaa,
    ));
    let root = punk_screen_root(&mut commands, AppState::Leaderboard, tex, INK_BLACK, 1.0);
    let body = terminal_chassis(&mut commands, root, tex, 1180.0, false);
    let p = &store.profile;
    let full = || Node { width: Val::Percent(100.0), align_items: AlignItems::Center, column_gap: Val::Px(10.0), ..default() };

    // Header: title | account pill | SIGN IN / SIGN OUT.
    let header = commands.spawn((ChildOf(body), full())).id();
    stencil_title(&mut commands, header, "GLOBAL LEADERBOARD", 24.0, BONE_TEXT, PUNK_RED);
    commands.spawn((ChildOf(header), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((
        ChildOf(header),
        Node { padding: UiRect::axes(Val::Px(8.0), Val::Px(2.0)), border: plate_rect((3.0, 1.0, 1.0, 1.0)), flex_shrink: 0.0, ..default() },
        BorderColor { left: ARC_CYAN, top: STEEL_BORDER, right: STEEL_BORDER, bottom: STEEL_BORDER },
        BackgroundColor(INK_BLACK.with_alpha(0.7)),
        children![(AccountPillText, text(account_label(&lb.account, &p.callsign), 13.0, ARC_CYAN))],
    ));
    let acct = commands.spawn((ChildOf(header), punk_button(account_switch_label(&lb.account).unwrap_or("SIGN IN"), MenuAction::LbAccount, 14.0, ButtonStyle::PrimaryAcid))).id();
    if account_switch_label(&lb.account).is_none() {
        commands.entity(acct).insert(Node { display: Display::None, ..default() });
    }
    // Native / Android guests: why scores post as GUEST.
    let note = guest_note(&lb.account);
    commands.spawn((
        ChildOf(body),
        LbLabel::Guest,
        text(note.unwrap_or_default(), 11.0, ARC_CYAN.with_alpha(0.85)),
        Node { display: if note.is_some() { Display::Flex } else { Display::None }, ..default() },
    ));

    // Controls: callsign plate | tab switch | REFRESH.
    let controls = commands.spawn((ChildOf(body), full())).id();
    let plate = commands.spawn((ChildOf(controls), punk_button(&callsign_plate_label(&lb, &p.callsign, mode), MenuAction::LbCallsign, 14.0, ButtonStyle::HazardAmber))).id();
    // Roomy enough that typing a callsign barely shifts the row, narrow
    // enough for a 960 px embed.
    commands.entity(plate).entry::<Node>().and_modify(|mut n| n.min_width = Val::Px(220.0));
    commands.spawn((ChildOf(controls), punk_button(&tab_label(&lb, mode), MenuAction::LbTab, 14.0, ButtonStyle::PrimaryAcid)));
    commands.spawn((ChildOf(controls), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((
        ChildOf(controls),
        punk_button("REFRESH  [R]", MenuAction::LbRefresh, 14.0, ButtonStyle::ScrapSteel),
        DualPrompt::new("REFRESH  [R]", "REFRESH  [Y]"),
    ));

    // Table: one page of LB_PAGE_SIZE rows.
    let (table, table_head) = section_crate(&mut commands, body, "TOP 25", HAZARD_YELLOW, Node { width: Val::Percent(100.0), row_gap: Val::Px(1.0), ..default() });
    commands.entity(table).insert(hard_shadow(5.0));
    let (status, color) = board_status(&lb);
    commands.spawn((ChildOf(table_head), LbLabel::Status, text(status, 12.0, color)));
    commands.spawn((ChildOf(table_head), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(table_head), text("[ID] = AUTHENTIK-VERIFIED  //  YOU ARE HIGHLIGHTED", 11.0, ASH_DIM)));
    let head_row = commands.spawn((ChildOf(table), Node { column_gap: Val::Px(8.0), padding: UiRect::horizontal(Val::Px(6.0)), ..default() })).id();
    for (i, w) in COLS.iter().enumerate() {
        commands.spawn((ChildOf(head_row), col_node(*w), children![(LbHeader(i), text(column_headers(lb.tab)[i], 11.0, HAZARD_YELLOW), TextLayout::no_wrap())]));
    }
    // Fixed height: the chassis never jumps between loaded / empty / offline.
    let rows = commands
        .spawn((
            ChildOf(table),
            LbTable,
            Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(1.0), min_height: Val::Px(LB_PAGE_SIZE as f32 * (ROW_H + 1.0)), ..default() },
        ))
        .id();
    for r in 0..LB_PAGE_SIZE {
        let row = commands
            .spawn((
                ChildOf(rows),
                LbRow(r),
                Node { height: Val::Px(ROW_H), column_gap: Val::Px(8.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(6.0)), display: Display::None, ..default() },
                BackgroundColor(Color::NONE),
            ))
            .id();
        for (c, w) in COLS.iter().enumerate() {
            commands.spawn((ChildOf(row), col_node(*w), children![(LbCell(r, c), text("", ROW_FONT, BONE_TEXT), TextLayout::no_wrap())]));
        }
    }
    commands.spawn((
        ChildOf(rows),
        LbMessageBox,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(36.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(10.0),
            ..default()
        },
        children![(LbLabel::Message, text("", 20.0, PUNK_RED)), (LbLabel::LocalSummary, text("", 14.0, BONE_TEXT))],
    ));

    // Feedback (sync errors, sign-out, callsign editor controls) or key hints.
    let notice = commands.spawn((ChildOf(body), Node { width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() })).id();
    let (line, color) = footer_line(&lb, mode);
    commands.spawn((ChildOf(notice), LbLabel::Notice, text(line, 12.0, color)));

    // Footer: BACK | PREV  PAGE n/3  NEXT.
    let footer = commands.spawn((ChildOf(body), full())).id();
    commands.spawn((ChildOf(footer), punk_button("BACK  [ESC]", MenuAction::BackToMenu, 16.0, ButtonStyle::ScrapSteel), DualPrompt::new("BACK  [ESC]", pad_prompt(AppState::Leaderboard, "BACK  [ESC]", MenuAction::BackToMenu))));
    commands.spawn((ChildOf(footer), Node { flex_grow: 1.0, ..default() }));
    commands.spawn((ChildOf(footer), punk_button(PREV_PAGE_KBM, MenuAction::LbPrevPage, 14.0, ButtonStyle::ScrapSteel), DualPrompt::new(PREV_PAGE_KBM, "PREV  [LT]")));
    commands.spawn((ChildOf(footer), Node { min_width: Val::Px(84.0), justify_content: JustifyContent::Center, ..default() }, children![(LbLabel::Page, text(page_label(&lb), 14.0, HAZARD_YELLOW))]));
    commands.spawn((ChildOf(footer), punk_button(NEXT_PAGE_KBM, MenuAction::LbNextPage, 14.0, ButtonStyle::PrimaryAcid), DualPrompt::new(NEXT_PAGE_KBM, "NEXT  [RT]")));
}

/// Page switches (mouse & keyboard keycaps: `[` / `]` or the arrows).
const PREV_PAGE_KBM: &str = "PREV  [ [ ]";
const NEXT_PAGE_KBM: &str = "NEXT  [ ] ]";

fn set_text(t: &mut Mut<Text>, s: &str) {
    if t.0 != s {
        t.0.clear();
        t.0.push_str(s);
    }
}

fn set_color(c: &mut Mut<TextColor>, want: Color) {
    if c.0 != want {
        c.0 = want;
    }
}

fn set_display(n: &mut Mut<Node>, show: bool) {
    let want = if show { Display::Flex } else { Display::None };
    if n.display != want {
        n.display = want;
    }
}

/// Refresh the leaderboard screen from [`LeaderboardState`].
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn refresh_leaderboard_ui(
    lb: Res<LeaderboardState>,
    store: Res<ProfileStore>,
    mode: Option<Res<InputMode>>,
    mut rows: Query<(&LbRow, &mut Node, &mut BackgroundColor), (Without<LbMessageBox>, Without<LbLabel>)>,
    mut msg_box: Query<&mut Node, (With<LbMessageBox>, Without<LbRow>, Without<LbLabel>)>,
    mut label_nodes: Query<(&LbLabel, &mut Node), (Without<LbRow>, Without<LbMessageBox>)>,
    mut cells: Query<(&LbCell, &mut Text, &mut TextColor), (Without<LbHeader>, Without<LbLabel>)>,
    mut headers: Query<(&LbHeader, &mut Text), (Without<LbCell>, Without<LbLabel>)>,
    mut labels: Query<(&LbLabel, &mut Text, &mut TextColor), (Without<LbCell>, Without<LbHeader>)>,
    buttons: Query<(&MenuAction, &Children)>,
    mut button_texts: Query<&mut Text, (Without<LbCell>, Without<LbHeader>, Without<LbLabel>)>,
) {
    let mode_changed = mode.as_ref().is_some_and(|m| m.is_changed());
    let mode = mode.map(|m| *m).unwrap_or_default();
    if !lb.is_changed() && !store.is_changed() && !mode_changed {
        return;
    }
    let me = store.profile.callsign.as_str();
    let offset = lb.current_page() * LB_PAGE_SIZE;
    let lines: Vec<([String; 9], bool, bool)> = match (lb.tab, &lb.daily, &lb.trial) {
        (LbTab::Daily, Board::Loaded(v), _) => v.iter().map(|e| (daily_cells(e), e.callsign == me, e.verified)).collect(),
        (LbTab::TimeTrial, _, Board::Loaded(v)) => v.iter().map(|e| (trial_cells(e), e.callsign == me, e.verified)).collect(),
        _ => Vec::new(),
    };
    for (row, mut node, mut bg) in &mut rows {
        let line = lines.get(offset + row.0);
        set_display(&mut node, line.is_some());
        let want = match line {
            Some((_, true, _)) => HAZARD_YELLOW.with_alpha(0.22),
            Some(_) if (offset + row.0).is_multiple_of(2) => Color::WHITE.with_alpha(0.035),
            _ => Color::NONE,
        };
        if bg.0 != want {
            bg.0 = want;
        }
    }
    for (cell, mut t, mut c) in &mut cells {
        let Some((vals, mine, _)) = lines.get(offset + cell.0) else { continue };
        set_text(&mut t, &vals[cell.1]);
        let color = match cell.1 {
            0 if offset + cell.0 < 3 => crate::hud::COLOR_GOLD,
            1 if *mine => HAZARD_YELLOW,
            2 => ARC_CYAN,
            3 if lb.tab == LbTab::Daily && vals[3] == "EXTRACTED" => ACID_GREEN,
            3 if lb.tab == LbTab::Daily => PUNK_RED,
            6 => BONE_TEXT,
            _ => BONE_TEXT.with_alpha(0.82),
        };
        set_color(&mut c, color);
    }
    for (h, mut t) in &mut headers {
        set_text(&mut t, column_headers(lb.tab)[h.0]);
    }
    let board = match lb.tab {
        LbTab::Daily => (lb.daily == Board::Offline, matches!(lb.daily, Board::Loaded(ref v) if v.is_empty()), lb.daily == Board::Loading || lb.daily == Board::Idle),
        LbTab::TimeTrial => (lb.trial == Board::Offline, matches!(lb.trial, Board::Loaded(ref v) if v.is_empty()), lb.trial == Board::Loading || lb.trial == Board::Idle),
    };
    let (message, msg_color) = match board {
        (true, _, _) => ("OFFLINE - LOCAL RECORDS ONLY", PUNK_RED),
        (_, true, _) => ("NO ENTRIES YET - BE THE FIRST ON THE BOARD", HAZARD_YELLOW),
        (_, _, true) => ("CONNECTING TO SIGNAL.BOMBATOMICA64.DEV...", ARC_CYAN),
        _ => ("", BONE_TEXT),
    };
    for mut n in &mut msg_box {
        set_display(&mut n, !message.is_empty());
    }
    let (status, status_color) = board_status(&lb);
    for (label, mut t, mut c) in &mut labels {
        match label {
            LbLabel::Status => {
                set_text(&mut t, &status);
                set_color(&mut c, status_color);
            }
            LbLabel::Notice => {
                let (line, color) = footer_line(&lb, mode);
                set_text(&mut t, line);
                set_color(&mut c, color);
            }
            LbLabel::Page => set_text(&mut t, &page_label(&lb)),
            LbLabel::Guest => set_text(&mut t, guest_note(&lb.account).unwrap_or_default()),
            LbLabel::Message => {
                set_text(&mut t, message);
                set_color(&mut c, msg_color);
            }
            LbLabel::LocalSummary => set_text(&mut t, &if message.is_empty() { String::new() } else { local_summary(&store.profile, DailyDate::today_utc()) }),
        }
    }
    for (label, mut n) in &mut label_nodes {
        if *label == LbLabel::Guest {
            set_display(&mut n, guest_note(&lb.account).is_some());
        }
    }
    for (action, children) in &buttons {
        let want = match action {
            MenuAction::LbCallsign => callsign_plate_label(&lb, me, mode),
            MenuAction::LbTab => tab_label(&lb, mode),
            _ => continue,
        };
        for c in children {
            if let Ok(mut t) = button_texts.get_mut(*c) {
                set_text(&mut t, &want);
            }
        }
    }
}

/// Account pills + SIGN IN / OUT switches on every screen that shows them.
fn refresh_account_ui(
    lb: Res<LeaderboardState>,
    store: Res<ProfileStore>,
    mut pills: Query<&mut Text, With<AccountPillText>>,
    mut switches: Query<(&MenuAction, &mut Node, &Children, Has<MenuSignIn>)>,
    mut labels: Query<&mut Text, Without<AccountPillText>>,
) {
    if !lb.is_changed() && !store.is_changed() {
        return;
    }
    let label = account_label(&lb.account, &store.profile.callsign);
    for mut t in &mut pills {
        set_text(&mut t, &label);
    }
    let switch = account_switch_label(&lb.account);
    for (action, mut node, children, menu) in &mut switches {
        if *action != MenuAction::LbAccount {
            continue;
        }
        let switch = if menu { switch.filter(|s| *s == "SIGN IN") } else { switch };
        set_display(&mut node, switch.is_some());
        if let Some(s) = switch {
            for c in children {
                if let Ok(mut t) = labels.get_mut(*c) {
                    set_text(&mut t, s);
                }
            }
        }
    }
}

/// End-card badge from [`LeaderboardState::submit`].
fn refresh_submit_badge(lb: Res<LeaderboardState>, mut badges: Query<(&mut Text, &mut TextColor, &mut Node), With<SubmitBadge>>) {
    if !lb.is_changed() {
        return;
    }
    let badge = submit_badge(&lb.submit);
    for (mut t, mut c, mut n) in &mut badges {
        set_display(&mut n, badge.is_some());
        if let Some((s, color)) = &badge {
            set_text(&mut t, s);
            set_color(&mut c, *color);
        }
    }
}

/// End-card badge bundle (filled live by [`refresh_submit_badge`]).
pub fn submit_badge_bundle(lb: &LeaderboardState, font: f32) -> impl Bundle {
    let (s, color) = submit_badge(&lb.submit).unwrap_or_default();
    (SubmitBadge, text(s, font, color))
}

/// Leaderboard screen + account pills + end-card badges (windowed / HUD apps).
pub struct LeaderboardUiPlugin;

impl Plugin for LeaderboardUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Leaderboard), spawn_leaderboard_screen.after(on_enter_leaderboard))
            .add_systems(Update, (refresh_leaderboard_ui.run_if(in_state(AppState::Leaderboard)), refresh_account_ui, refresh_submit_badge));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily_run(floor: u32, t: f32) -> RunStats {
        RunStats { floor, time_elapsed: t, peak_speed: 31.5, max_bhop_chain: 7, kills: 40, mode: RunMode::Daily(DailyDate::new(2026, 11, 25)), ..default() }
    }

    #[test]
    fn daily_submissions_mirror_server_validation() {
        let s = daily_submission(&daily_run(5, 640.0), true, "ace pilot").unwrap();
        assert_eq!((s.callsign.as_str(), s.date.as_str(), s.victory, s.floor), ("ACEPILOT", "2026-11-25", true, 5));
        assert!(s.score > 0 && s.score <= MAX_SCORE);
        assert!(daily_submission(&daily_run(1, 3.0), false, "X1").is_err(), "too short");
        assert!(daily_submission(&daily_run(1, 8000.0), false, "X1").is_err(), "too long");
        assert!(daily_submission(&daily_run(2, 400.0), true, "X1").is_err(), "victory below floor 3");
        assert!(daily_submission(&RunStats { time_elapsed: 60.0, ..default() }, false, "X1").is_err(), "standard runs are not daily");
        // Endless continuation of a Daily victory stays a victory at the deeper floor.
        let endless = RunStats { endless: true, ..daily_run(8, 1500.0) };
        let s = daily_submission(&endless, false, "AB").unwrap();
        assert!(s.victory && s.floor == 8);
        // Junk telemetry is clamped, not sent raw.
        let junk = RunStats { peak_speed: f32::NAN, max_bhop_chain: 9000, ..daily_run(2, 100.0) };
        let s = daily_submission(&junk, false, "AB").unwrap();
        assert_eq!((s.peak_speed, s.max_bhop_chain), (0.0, 500));
        assert!(daily_score(true, 5, 100, 300.0) > daily_score(false, 5, 100, 300.0));
        assert!(daily_score(false, 6, 0, 300.0) > daily_score(false, 5, 100, 300.0));
        assert_eq!(daily_score(true, 200, u32::MAX, 0.0), MAX_SCORE);
    }

    #[test]
    fn trial_submissions_need_four_increasing_splits() {
        assert!(trial_submission(48.6, &[9.8, 21.5, 36.0, 48.6], 40.0, 5, "ZERO").is_ok());
        assert!(trial_submission(48.6, &[9.8, 21.5, 48.6], 40.0, 5, "ZERO").is_err());
        assert!(trial_submission(48.6, &[9.8, 9.8, 36.0, 48.6], 40.0, 5, "ZERO").is_err());
        assert!(trial_submission(48.6, &[9.8, 21.5, 36.0, 47.0], 40.0, 5, "ZERO").is_err());
        assert!(trial_submission(3.0, &[1.0, 2.0, 2.5, 3.0], 40.0, 5, "ZERO").is_err());
        assert!(trial_submission(f32::INFINITY, &[1.0, 2.0, 2.5, 3.0], 40.0, 5, "ZERO").is_err());
        let s = trial_submission(48.6, &[9.8, 21.5, 36.0, 48.7], 99.0, 5, "zero").unwrap();
        assert_eq!((s.callsign.as_str(), s.peak_speed), ("ZERO", 65.0));
    }

    #[test]
    fn networking_is_off_in_tests_bots_and_shots() {
        let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>().into_iter();
        // cfg(test) always wins.
        assert!(!network_allowed_by_env(env(&[])));
        assert!(!network_allowed_by_env(env(&[("HOME", "/x")])));
        let mut c = LeaderboardClient::offline();
        c.send(NetKind::Me, "GET", "/auth/me", None);
        let replies = c.drain();
        assert_eq!(replies, vec![NetReply { kind: NetKind::Me, status: 0, body: String::new() }]);
        assert!(c.drain().is_empty());
    }

    #[test]
    fn web_hosts_pick_same_origin_or_production() {
        assert_eq!(web_base_for_host("signal.bombatomica64.dev"), (String::new(), true));
        assert_eq!(web_base_for_host("LOCALHOST"), (String::new(), true));
        assert_eq!(web_base_for_host("127.0.0.1"), (String::new(), true));
        assert_eq!(web_base_for_host("html.itch.zone"), (DEFAULT_BASE_URL.to_string(), false));
        let c = LeaderboardClient::offline();
        assert_eq!(c.url("/leaderboard/trial"), "https://signal.bombatomica64.dev/api/v1/leaderboard/trial");
    }

    #[test]
    fn replies_drive_account_boards_and_badges() {
        let mut lb = LeaderboardState::default();
        let mut store = ProfileStore::in_memory(PlayerProfile { time_trial_best_secs: Some(60.0), time_trial_best_splits: vec![10.0, 20.0, 40.0, 60.0], ..default() });
        let mut client = LeaderboardClient::offline();
        let reply = |kind: NetKind, status: u16, body: &str| NetReply { kind, status, body: body.into() };

        apply_reply(reply(NetKind::Me, 0, ""), &mut lb, &mut store, &mut client);
        assert_eq!(lb.account, Account::Offline);
        apply_reply(reply(NetKind::Me, 200, r#"{"authenticated":false,"auth_enabled":true}"#), &mut lb, &mut store, &mut client);
        assert_eq!(lb.account, Account::Guest { auth_enabled: true });

        // Signed in: the cloud profile merges in, the account callsign wins, and
        // the faster local PB is pushed back.
        let cloud = PlayerProfile { time_trial_best_secs: Some(70.0), stats: crate::meta::LifetimeStats { runs_played: 9, ..default() }, ..default() };
        let body = serde_json::json!({"authenticated":true,"auth_enabled":true,"sub":"abc","username":"lorenzo","callsign":"BOMBA","profile":cloud}).to_string();
        let sent = client.sent;
        apply_reply(reply(NetKind::Me, 200, &body), &mut lb, &mut store, &mut client);
        assert!(lb.account.signed_in());
        assert_eq!((store.profile.callsign.as_str(), store.profile.stats.runs_played, store.profile.time_trial_best_secs), ("BOMBA", 9, Some(60.0)));
        assert_eq!(client.sent, sent + 1, "local progress was newer: PUT /profile");
        assert_eq!(client.drain()[0].kind, NetKind::PutProfile);

        // Boards.
        let daily = r#"{"date":"2026-11-25","entries":[{"rank":1,"callsign":"BOMBA","verified":true,"victory":true,"floor":5,"time_secs":512.3,"peak_speed":33.1,"max_bhop_chain":9,"score":123}]}"#;
        apply_reply(reply(NetKind::DailyBoard, 200, daily), &mut lb, &mut store, &mut client);
        let Board::Loaded(v) = &lb.daily else { panic!("daily loaded") };
        assert_eq!((v.len(), v[0].verified, lb.daily_date.as_str()), (1, true, "2026-11-25"));
        apply_reply(reply(NetKind::TrialBoard, 503, ""), &mut lb, &mut store, &mut client);
        assert_eq!(lb.trial, Board::Offline);

        // Submissions -> badge.
        apply_reply(reply(NetKind::SubmitTrial, 200, r#"{"accepted":true,"improved":true,"rank":3,"verified":true}"#), &mut lb, &mut store, &mut client);
        assert_eq!(submit_badge(&lb.submit).unwrap().0, "GLOBAL RANK #3 (NEW PB!)");
        apply_reply(reply(NetKind::SubmitDaily, 200, r#"{"accepted":true,"improved":false,"rank":12,"verified":false}"#), &mut lb, &mut store, &mut client);
        assert_eq!(submit_badge(&lb.submit).unwrap().0, "GLOBAL RANK #12");
        apply_reply(reply(NetKind::SubmitDaily, 0, ""), &mut lb, &mut store, &mut client);
        assert_eq!(submit_badge(&lb.submit).unwrap().0, "OFFLINE - SAVED LOCALLY");
        apply_reply(reply(NetKind::SubmitDaily, 429, ""), &mut lb, &mut store, &mut client);
        assert!(matches!(lb.submit, SubmitStatus::NotRanked(_)));
        assert!(submit_badge(&SubmitStatus::Idle).is_none());

        // Logout.
        apply_reply(reply(NetKind::Logout, 200, "{}"), &mut lb, &mut store, &mut client);
        assert_eq!(lb.account, Account::Guest { auth_enabled: true });
    }

    #[test]
    fn sign_out_only_clears_the_account_on_success() {
        let reply = |kind: NetKind, status: u16, body: &str| NetReply { kind, status, body: body.into() };
        let signed_in = || LeaderboardState { account: Account::SignedIn { username: "u".into() }, ..default() };
        let mut store = ProfileStore::in_memory(PlayerProfile::default());
        for status in [0, 401, 500, 503] {
            let mut lb = signed_in();
            let mut client = LeaderboardClient::offline();
            client.bearer = Some("tok".into());
            apply_reply(reply(NetKind::Logout, status, ""), &mut lb, &mut store, &mut client);
            assert!(lb.account.signed_in(), "status {status} keeps the session");
            assert_eq!(lb.notice.as_deref(), Some(SIGN_OUT_FAILED));
            assert_eq!(client.bearer.as_deref(), Some("tok"));
            assert_eq!(account_switch_label(&lb.account), Some("SIGN OUT"), "retry stays offered");
        }
        let mut lb = signed_in();
        let mut client = LeaderboardClient::offline();
        client.bearer = Some("tok".into());
        apply_reply(reply(NetKind::Logout, 204, ""), &mut lb, &mut store, &mut client);
        assert_eq!(lb.account, Account::Guest { auth_enabled: true });
        assert_eq!(lb.notice.as_deref(), Some("SIGNED OUT"));
        assert!(client.bearer.is_none());
    }

    #[test]
    fn expired_session_on_profile_put_signs_out_visibly() {
        let mut lb = LeaderboardState { account: Account::SignedIn { username: "u".into() }, ..default() };
        let mut store = ProfileStore::in_memory(PlayerProfile::default());
        let mut client = LeaderboardClient::offline();
        client.bearer = Some("stale".into());
        apply_reply(NetReply { kind: NetKind::PutProfile, status: 401, body: r#"{"error":"not authenticated"}"#.into() }, &mut lb, &mut store, &mut client);
        assert!(!lb.account.signed_in());
        assert_eq!(lb.notice.as_deref(), Some(SESSION_EXPIRED));
        assert!(client.bearer.is_none());
        // Other failures keep the account and say the progress is local.
        let mut lb = LeaderboardState { account: Account::SignedIn { username: "u".into() }, ..default() };
        apply_reply(NetReply { kind: NetKind::PutProfile, status: 500, body: String::new() }, &mut lb, &mut store, &mut client);
        assert!(lb.account.signed_in());
        assert!(lb.notice.as_deref().is_some_and(|n| n.contains("CLOUD SYNC FAILED")));
    }

    #[test]
    fn submission_errors_map_to_short_player_messages() {
        let badge = |status: u16, body: &str| submit_badge(&submit_failure(status, body)).unwrap().0;
        assert_eq!(badge(429, r#"{"error":"too many submissions, slow down"}"#), "RATE LIMITED - TRY AGAIN SOON");
        assert_eq!(badge(400, r#"{"error":"date must be a real YYYY-MM-DD date"}"#), "REJECTED: DATE MUST BE A REAL YYYY-MM-DD DATE");
        assert_eq!(badge(400, "<html>"), "REJECTED BY SERVER - NOT RANKED");
        assert_eq!(badge(0, ""), "OFFLINE - SAVED LOCALLY");
        assert_eq!(badge(502, ""), "SERVER BUSY - SAVED LOCALLY");
        let long = rejection_reason("time_secs is way out of the accepted range for this course and date");
        assert!(long.starts_with("REJECTED: TIME_SECS IS WAY") && long.len() <= "REJECTED: ".len() + 40, "{long}");
        assert_eq!(rejection_reason("bad\ncallsign \u{1F600}"), "REJECTED: BAD CALLSIGN");
        assert_eq!(rejection_reason(&"X".repeat(90)), format!("REJECTED: {}", "X".repeat(40)));
    }

    #[test]
    fn top_25_pages_ten_rows_at_a_time() {
        assert_eq!((page_count(0), page_count(1), page_count(10), page_count(11), page_count(25), page_count(99)), (1, 1, 1, 2, 3, 3));
        let mut lb = LeaderboardState { daily: Board::Loaded(vec![DailyEntry::default(); 25]), trial: Board::Loaded(vec![TrialEntry::default(); 4]), ..default() };
        assert_eq!(page_label(&lb), "PAGE 1/3");
        lb.turn_page(-1);
        assert_eq!(lb.current_page(), 0, "never wraps below page 1");
        lb.turn_page(1);
        lb.turn_page(1);
        lb.turn_page(1);
        assert_eq!((lb.current_page(), page_label(&lb)), (2, "PAGE 3/3".to_string()));
        lb.switch_tab();
        assert_eq!((lb.tab, page_label(&lb)), (LbTab::TimeTrial, "PAGE 1/1".to_string()), "switching boards resets the page");
        // A shorter reload clamps the shown page.
        lb.switch_tab();
        lb.page = 2;
        lb.daily = Board::Loaded(vec![DailyEntry::default(); 12]);
        assert_eq!(lb.current_page(), 1);
        lb.daily = Board::Offline;
        assert_eq!(page_label(&lb), "PAGE 1/1");
    }

    #[test]
    fn guest_note_footer_and_labels_follow_platform_and_input() {
        let guest = Account::Guest { auth_enabled: true };
        assert_eq!(guest_note_for(false, &guest), Some(GUEST_NOTE));
        assert_eq!(guest_note_for(false, &Account::Guest { auth_enabled: false }), Some(GUEST_NOTE));
        assert_eq!(guest_note_for(true, &guest), None, "the web build offers SIGN IN instead");
        assert_eq!(guest_note_for(false, &Account::SignedIn { username: "u".into() }), None);
        assert_eq!(guest_note_for(false, &Account::Offline), None);
        assert!(GUEST_NOTE.starts_with("GUEST MODE"));
        let mut lb = LeaderboardState::default();
        assert_eq!(footer_line(&lb, InputMode::KeyboardMouse).0, LB_HINTS);
        assert_eq!(footer_line(&lb, InputMode::Gamepad).0, LB_PAD_HINTS);
        lb.notice = Some(SIGN_OUT_FAILED.into());
        assert_eq!(footer_line(&lb, InputMode::Gamepad).0, SIGN_OUT_FAILED);
        lb.editing = Some("AB".into());
        assert_eq!(footer_line(&lb, InputMode::Gamepad).0, PAD_TEXT_HINT, "controller editing controls");
        assert_eq!(footer_line(&lb, InputMode::KeyboardMouse).0, CALLSIGN_KBM_HINT);
        lb.editing = None;
        assert_eq!(callsign_plate_label(&lb, "ACE", InputMode::KeyboardMouse), "CALLSIGN: ACE  [C]");
        assert_eq!(callsign_plate_label(&lb, "ACE", InputMode::Gamepad), "CALLSIGN: ACE");
        assert!(tab_label(&lb, InputMode::Gamepad).ends_with("[LB/RB]") && tab_label(&lb, InputMode::KeyboardMouse).ends_with("[TAB]"));
    }

    #[test]
    fn callsign_editor_keys_and_pad_cycle() {
        assert_eq!(callsign_key_char(KeyCode::KeyQ), Some('Q'));
        assert_eq!(callsign_key_char(KeyCode::Digit7), Some('7'));
        assert_eq!(callsign_key_char(KeyCode::Minus), Some('-'));
        assert_eq!(callsign_key_char(KeyCode::Space), None);
        assert_eq!(callsign_key_char(KeyCode::Enter), None);
        assert_eq!(callsign_key_char(KeyCode::KeyA).map(is_callsign_char), Some(true));
        assert_eq!(cycle_callsign_char('A', 1), 'B');
        assert_eq!(cycle_callsign_char('A', -1), '-');
        assert_eq!(cycle_callsign_char('-', 1), 'A');

        let mut lb = LeaderboardState { editing: Some("ne w".into()), ..default() };
        let mut store = ProfileStore::in_memory(PlayerProfile::default());
        let mut client = LeaderboardClient::offline();
        commit_callsign(&mut lb, &mut store, &mut client);
        assert_eq!(store.profile.callsign, "NEW");
        assert!(lb.editing.is_none() && client.sent == 0, "guests never PUT");
        lb.account = Account::SignedIn { username: "x".into() };
        lb.editing = Some("ACE_01".into());
        commit_callsign(&mut lb, &mut store, &mut client);
        assert_eq!((store.profile.callsign.as_str(), client.sent), ("ACE_01", 1));
    }

    #[test]
    fn table_cells_labels_and_local_fallback() {
        let d = DailyEntry { rank: 2, callsign: "ACE".into(), verified: true, victory: true, floor: 5, time_secs: 512.25, peak_speed: 33.14, max_bhop_chain: 9, score: 99 };
        let c = daily_cells(&d);
        assert_eq!((c[0].as_str(), c[2].as_str(), c[3].as_str(), c[4].as_str(), c[8].as_str()), ("2", "[ID]", "EXTRACTED", "F5", "x9"));
        let t = TrialEntry { rank: 1, callsign: "ZZ".into(), verified: false, time_secs: 48.6, splits: vec![9.8, 21.5, 36.0, 48.6], peak_speed: 40.0, max_bhop_chain: 3 };
        let c = trial_cells(&t);
        assert_eq!((c[2].as_str(), c[3].clone(), c[6].clone()), ("", format_split_time(9.8), format_split_time(48.6)));
        assert_eq!(column_headers(LbTab::TimeTrial)[6], "FINISH");
        assert_eq!(account_label(&Account::SignedIn { username: "u".into() }, "ACE"), "AUTHENTIK: ACE");
        assert_eq!(account_switch_label(&Account::SignedIn { username: "u".into() }), Some("SIGN OUT"));
        // Native never offers SIGN IN (bearer token instead).
        assert_eq!(account_switch_label(&Account::Guest { auth_enabled: true }), None);
        let p = PlayerProfile { time_trial_best_secs: Some(48.6), time_trial_best_splits: vec![9.8, 21.5, 36.0, 48.6], ..default() };
        let s = local_summary(&p, DailyDate::new(2026, 11, 25));
        assert!(s.contains("NOT ATTEMPTED") && s.contains("TIME TRIAL PB"));
        let lb = LeaderboardState { editing: Some("AB".into()), ..default() };
        assert_eq!(callsign_plate_label(&lb, "X", InputMode::KeyboardMouse), "CALLSIGN: AB_");
        assert_eq!(LbTab::Daily.other(), LbTab::TimeTrial);
    }
}
