//! Performance controls and telemetry:
//! - `F4` toggles VSync live (`AutoNoVsync` <-> `AutoVsync`) on the primary
//!   window, with a HUD toast while playing. In the browser the compositor
//!   owns frame pacing, so `F4` only shows "VSYNC MANAGED BY BROWSER".
//! - `F3` toggles a compact telemetry overlay (off by default): smoothed FPS,
//!   avg / 1%-worst / max frame time, entity count, visible / total point
//!   lights, material and mesh asset counts, VSync mode. Its text refreshes at
//!   4 Hz and nothing is sampled or formatted while it is hidden.

use std::fmt::Write as _;

use bevy::core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT;
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::renderer::RenderAdapter;
use bevy::render::view::Msaa;
use bevy::window::{PresentMode, PrimaryWindow};

use crate::fx::HudFeed;
use crate::state::AppState;
use crate::ui_kit::{plate_border, ARC_CYAN, BONE_TEXT, INK_BLACK, STEEL_BORDER};

/// Frame-time samples kept for the rolling statistics.
pub const FRAME_WINDOW: usize = 120;
/// Overlay text refresh period (4 Hz).
pub const OVERLAY_REFRESH_SECS: f32 = 0.25;

/// Adapter-validated MSAA level shared by every 3D camera (menu, world, and
/// viewmodel overlay). Defaults to [`Msaa::Sample2`] when 2× multisampling is
/// supported for both color and depth render targets, and falls back to
/// [`Msaa::Off`] otherwise so adapters without 2× MSAA never fail validation.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderMsaa(pub Msaa);

impl Default for RenderMsaa {
    fn default() -> Self {
        RenderMsaa(if IS_WEB { Msaa::Off } else { Msaa::Sample2 })
    }
}

/// True for the WebGL2 browser build.
pub const IS_WEB: bool = cfg!(target_arch = "wasm32");

/// Startup MSAA policy: ray tracing (Solari's single-sample G-buffer) and the
/// WebGL2 build (multi-camera MSAA resolve hazards, fill-rate on integrated
/// GPUs) force [`Msaa::Off`]; native otherwise uses [`select_msaa`].
pub fn platform_msaa(is_web: bool, raytracing: bool, supports_2x: bool, env_override: Option<&str>) -> Msaa {
    if is_web || raytracing { Msaa::Off } else { select_msaa(supports_2x, env_override) }
}

/// Toast shown when `F4` is pressed in the browser, where the compositor owns
/// frame pacing (`requestAnimationFrame`) and `AutoNoVsync` is meaningless.
pub const WEB_VSYNC_TOAST: &str = "VSYNC MANAGED BY BROWSER";

/// `F4` outcome: the new VSync flag and the toast to show while playing.
pub fn f4_vsync_toggle(is_web: bool, vsync: bool) -> (bool, &'static str) {
    if is_web {
        (vsync, WEB_VSYNC_TOAST)
    } else if vsync {
        (false, "VSYNC: OFF")
    } else {
        (true, "VSYNC: ON")
    }
}

/// Pure selection helper for unit testing and startup probing.
pub fn select_msaa(supports_2x: bool, env_override: Option<&str>) -> Msaa {
    match env_override.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("off") || v == "1" => return Msaa::Off,
        Some("4") => return Msaa::Sample4,
        _ => {}
    }
    if supports_2x { Msaa::Sample2 } else { Msaa::Off }
}

/// True when `adapter` supports 2× multisampling as a render attachment on the
/// SDR surface formats, HDR target format, and 3D depth format.
pub fn adapter_supports_msaa_2x(adapter: &RenderAdapter) -> bool {
    const FORMATS: [TextureFormat; 4] = [
        TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Rgba16Float,
        CORE_3D_DEPTH_FORMAT,
    ];
    FORMATS.into_iter().all(|fmt| {
        let f = adapter.get_texture_format_features(fmt);
        f.allowed_usages.contains(TextureUsages::RENDER_ATTACHMENT) && f.flags.sample_count_supported(2)
    })
}

/// Live performance toggles.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerfSettings {
    /// `F3` telemetry overlay visible.
    pub overlay: bool,
    /// `F4` VSync on (`AutoVsync`); off = `AutoNoVsync` (the default).
    pub vsync: bool,
}

impl Default for PerfSettings {
    /// Native starts uncapped; the browser always presents vsynced (`AutoVsync`).
    fn default() -> Self {
        PerfSettings { overlay: false, vsync: IS_WEB }
    }
}

impl PerfSettings {
    pub fn present_mode(self) -> PresentMode {
        present_mode_for(self.vsync)
    }
}

pub fn present_mode_for(vsync: bool) -> PresentMode {
    if vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync }
}

pub fn vsync_label(vsync: bool) -> &'static str {
    if vsync { "VSYNC" } else { "NO-VSYNC" }
}

/// Rolling window of real frame times (seconds).
#[derive(Resource, Debug, Clone)]
pub struct FrameStats {
    samples: [f32; FRAME_WINDOW],
    len: usize,
    head: usize,
}

impl Default for FrameStats {
    fn default() -> Self {
        FrameStats { samples: [0.0; FRAME_WINDOW], len: 0, head: 0 }
    }
}

impl FrameStats {
    pub fn push(&mut self, dt: f32) {
        self.samples[self.head] = dt;
        self.head = (self.head + 1) % FRAME_WINDOW;
        self.len = (self.len + 1).min(FRAME_WINDOW);
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn window(&self) -> &[f32] {
        &self.samples[..self.len]
    }

    pub fn avg_ms(&self) -> f32 {
        if self.len == 0 { 0.0 } else { self.window().iter().sum::<f32>() / self.len as f32 * 1000.0 }
    }

    pub fn fps(&self) -> f32 {
        let avg = self.avg_ms();
        if avg > 0.0 { 1000.0 / avg } else { 0.0 }
    }

    pub fn max_ms(&self) -> f32 {
        self.window().iter().copied().fold(0.0, f32::max) * 1000.0
    }

    /// Mean of the worst 1% frames (at least one frame).
    pub fn worst_1pct_ms(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let mut sorted = [0.0f32; FRAME_WINDOW];
        let w = &mut sorted[..self.len];
        w.copy_from_slice(self.window());
        w.sort_unstable_by(|a, b| b.total_cmp(a));
        let n = (self.len / 100).max(1);
        w[..n].iter().sum::<f32>() / n as f32 * 1000.0
    }
}

/// Core (simulation-side) perf controls: hotkeys + present mode sync.
pub struct PerfPlugin;

impl Plugin for PerfPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PerfSettings>()
            .init_resource::<RenderMsaa>()
            .init_resource::<FrameStats>()
            .add_systems(PreUpdate, perf_hotkeys.after(bevy::input::InputSystems))
            .add_systems(PostUpdate, apply_present_mode);
    }

    fn finish(&self, app: &mut App) {
        let env_override = std::env::var("SEGNALE_MSAA").ok();
        let supports_2x = app
            .world()
            .get_resource::<RenderAdapter>()
            .map(adapter_supports_msaa_2x)
            .unwrap_or(true);
        let rt = app.world().get_resource::<crate::raytracing::RaytracingActive>().is_some_and(|r| r.0);
        // Solari's deferred G-buffer needs single-sample targets on every
        // camera; WebGL2 always runs single-sample (see `platform_msaa`).
        let chosen = platform_msaa(IS_WEB, rt, supports_2x, env_override.as_deref());
        if app.world().contains_resource::<RenderAdapter>() {
            info!("perf: MSAA selected {:?} (adapter 2x support: {})", chosen, supports_2x);
        }
        app.insert_resource(RenderMsaa(chosen));
    }
}

fn perf_hotkeys(keys: Res<ButtonInput<KeyCode>>, state: Option<Res<State<AppState>>>, mut settings: ResMut<PerfSettings>, mut stats: ResMut<FrameStats>, mut feed: ResMut<HudFeed>) {
    if keys.just_pressed(KeyCode::F3) {
        settings.overlay = !settings.overlay;
        stats.clear();
    }
    if keys.just_pressed(KeyCode::F4) {
        let (vsync, toast) = f4_vsync_toggle(IS_WEB, settings.vsync);
        settings.vsync = vsync;
        if state.is_some_and(|s| *s.get() == AppState::Playing) {
            feed.toast(toast, Color::srgb(0.3, 1.0, 0.85));
        }
    }
}

/// Push the chosen present mode to the primary window (only on change).
fn apply_present_mode(settings: Res<PerfSettings>, mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    let want = settings.present_mode();
    for mut w in &mut windows {
        if w.present_mode != want {
            w.present_mode = want;
        }
    }
}

// ---------------------------------------------------------------------------
// Overlay (UI side; added by the HUD plugin)
// ---------------------------------------------------------------------------

/// Root panel of the `F3` telemetry overlay.
#[derive(Component)]
pub struct PerfOverlay;

/// Text of the `F3` telemetry overlay.
#[derive(Component)]
pub struct PerfOverlayText;

pub struct PerfOverlayPlugin;

impl Plugin for PerfOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_perf_overlay).add_systems(Last, (record_frame_time, update_perf_overlay).chain());
    }
}

fn spawn_perf_overlay(mut commands: Commands) {
    commands.spawn((
        PerfOverlay,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            right: Val::Px(8.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
            border: UiRect { left: Val::Px(3.0), right: Val::Px(1.0), top: Val::Px(1.0), bottom: Val::Px(2.0) },
            display: Display::None,
            ..default()
        },
        plate_border(ARC_CYAN, STEEL_BORDER),
        BackgroundColor(INK_BLACK.with_alpha(0.86)),
        GlobalZIndex(1000),
        children![(PerfOverlayText, Text::new(""), TextFont::from_font_size(12.0), TextColor(BONE_TEXT))],
    ));
}

fn record_frame_time(time: Res<Time<Real>>, settings: Res<PerfSettings>, mut stats: ResMut<FrameStats>) {
    if settings.overlay {
        stats.push(time.delta_secs());
    }
}

#[allow(clippy::too_many_arguments)]
fn update_perf_overlay(
    time: Res<Time<Real>>,
    settings: Res<PerfSettings>,
    stats: Res<FrameStats>,
    entities: Query<Entity>,
    lights: Query<&Visibility, With<PointLight>>,
    materials: Res<Assets<StandardMaterial>>,
    meshes: Res<Assets<Mesh>>,
    mut roots: Query<&mut Node, With<PerfOverlay>>,
    mut texts: Query<&mut Text, With<PerfOverlayText>>,
    mut timer: Local<f32>,
) {
    let want = if settings.overlay { Display::Flex } else { Display::None };
    for mut node in &mut roots {
        if node.display != want {
            node.display = want;
        }
    }
    if !settings.overlay {
        *timer = 0.0;
        return;
    }
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = OVERLAY_REFRESH_SECS;
    let total_lights = lights.iter().count();
    let visible_lights = lights.iter().filter(|v| **v != Visibility::Hidden).count();
    let mut s = String::with_capacity(192);
    let _ = write!(
        s,
        "FPS {:>6.1}\nAVG {:>5.2} ms\n1%  {:>5.2} ms\nMAX {:>5.2} ms\nENT {:>6}\nLGT {:>3}/{:<3}\nMAT {:>6}\nMSH {:>6}\n{} [F4]",
        stats.fps(),
        stats.avg_ms(),
        stats.worst_1pct_ms(),
        stats.max_ms(),
        entities.iter().count(),
        visible_lights,
        total_lights,
        materials.len(),
        meshes.len(),
        vsync_label(settings.vsync),
    );
    for mut t in &mut texts {
        if t.0 != s {
            t.0.clone_from(&s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_stats_roll_and_rank() {
        let mut f = FrameStats::default();
        assert_eq!(f.fps(), 0.0);
        for _ in 0..200 {
            f.push(0.01);
        }
        assert_eq!(f.len(), FRAME_WINDOW);
        assert!((f.avg_ms() - 10.0).abs() < 1e-3);
        assert!((f.fps() - 100.0).abs() < 0.1);
        f.push(0.05);
        assert!((f.max_ms() - 50.0).abs() < 1e-3);
        assert!((f.worst_1pct_ms() - 50.0).abs() < 1e-3);
        f.clear();
        assert!(f.is_empty());
    }

    #[test]
    fn vsync_modes() {
        assert_eq!(PerfSettings::default().present_mode(), PresentMode::AutoNoVsync);
        assert_eq!(present_mode_for(true), PresentMode::AutoVsync);
        assert_eq!(vsync_label(false), "NO-VSYNC");
        // F4: native flips VSync; the browser keeps it and explains why.
        assert_eq!(f4_vsync_toggle(false, false), (true, "VSYNC: ON"));
        assert_eq!(f4_vsync_toggle(false, true), (false, "VSYNC: OFF"));
        assert_eq!(f4_vsync_toggle(true, true), (true, WEB_VSYNC_TOAST));
        assert_eq!(f4_vsync_toggle(true, false), (false, WEB_VSYNC_TOAST));
    }

    #[test]
    fn msaa_selection_falls_back_when_2x_unsupported() {
        assert_eq!(select_msaa(true, None), Msaa::Sample2);
        assert_eq!(select_msaa(false, None), Msaa::Off);
        assert_eq!(select_msaa(true, Some("off")), Msaa::Off);
        assert_eq!(select_msaa(true, Some("1")), Msaa::Off);
        assert_eq!(select_msaa(false, Some("4")), Msaa::Sample4);
        // Web build and ray tracing always run single-sample.
        assert_eq!(platform_msaa(true, false, true, None), Msaa::Off);
        assert_eq!(platform_msaa(true, false, true, Some("4")), Msaa::Off);
        assert_eq!(platform_msaa(false, true, true, None), Msaa::Off);
        assert_eq!(platform_msaa(false, false, true, None), Msaa::Sample2);
        assert_eq!(platform_msaa(false, false, true, Some("4")), Msaa::Sample4);
        assert_eq!(RenderMsaa::default().0, if IS_WEB { Msaa::Off } else { Msaa::Sample2 });
    }
}
