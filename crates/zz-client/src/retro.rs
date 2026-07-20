//! Retro render target: the 3D scene renders at a fixed low resolution and is
//! nearest-neighbor upscaled to the window, PS1/PS2 style. HUD (bevy_ui) and
//! egui draw at native resolution on top.
//!
//! Wiring:
//! - [`RetroTarget`] (PreStartup) holds the offscreen image every 3D camera
//!   must render to (`RenderTarget::Image(...)` component on the camera entity);
//! - a 2D "present" camera targets the window and hosts all UI: it is the
//!   [`IsDefaultUiCamera`] and carries the [`PrimaryEguiContext`] (bevy_egui
//!   auto-attachment is disabled so the context can't land on the 3D camera);
//! - the retro frame reaches the window as a stretched [`ImageNode`] behind
//!   every HUD layer (negative [`GlobalZIndex`]), letterboxed to 16:9.

use bevy::{
    image::{Image, ImageSampler},
    prelude::*,
    render::render_resource::TextureFormat,
    ui::FocusPolicy,
};
use bevy_egui::{EguiGlobalSettings, PrimaryEguiContext};

/// Internal 3D resolution. 480x270 reads as deliberate chunky pixels and is
/// the mobile perf lever; 640x360 is the tuning alternative if it turns out
/// too coarse on large displays.
pub const RETRO_W: u32 = 480;
pub const RETRO_H: u32 = 270;

/// Letterbox/pillarbox bar color — dark slate, deliberately not pure black.
const BAR_COLOR: Color = Color::srgb(0.05, 0.06, 0.08);

/// The offscreen image the 3D camera renders into.
#[derive(Resource)]
pub struct RetroTarget {
    pub image: Handle<Image>,
}

/// Marker for the full-window UI node that presents the retro frame.
#[derive(Component)]
struct RetroBlit;

pub struct RetroRenderPlugin;

impl Plugin for RetroRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, setup_target)
            .add_systems(Startup, setup_present)
            .add_systems(Update, fit_blit_node);
    }
}

fn setup_target(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut egui_settings: ResMut<EguiGlobalSettings>,
) {
    // The primary egui context is placed explicitly on the present camera in
    // `setup_present`; auto-attach would grab the first camera it sees, which
    // may be the 480x270 3D camera.
    egui_settings.auto_create_primary_context = false;

    // Storage format is linear; sRGB view format matches PBR camera output
    // (same pair as Bevy's `render_to_texture` example).
    let mut image = Image::new_target_texture(
        RETRO_W,
        RETRO_H,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    );
    // Nearest sampling is what makes the upscale chunky instead of blurry.
    image.sampler = ImageSampler::nearest();
    commands.insert_resource(RetroTarget {
        image: images.add(image),
    });
}

fn setup_present(mut commands: Commands, target: Res<RetroTarget>) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::Custom(BAR_COLOR),
            ..default()
        },
        IsDefaultUiCamera,
        PrimaryEguiContext,
        Name::new("PresentCamera"),
    ));

    commands.spawn((
        RetroBlit,
        ImageNode {
            image: target.image.clone(),
            image_mode: bevy::ui::widget::NodeImageMode::Stretch,
            ..default()
        },
        Node {
            position_type: PositionType::Absolute,
            ..default()
        },
        FocusPolicy::Pass,
        GlobalZIndex(-100),
        Name::new("RetroBlit"),
    ));
}

/// Keep the blit node letterboxed to the window (runs cheaply: writes only
/// when the window size actually changed).
fn fit_blit_node(
    window: Single<&Window>,
    mut last: Local<Vec2>,
    mut node: Single<&mut Node, With<RetroBlit>>,
) {
    let size = Vec2::new(window.width(), window.height());
    if size == *last || size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    *last = size;
    let (x, y, w, h) = letterbox_rect(size.x, size.y);
    node.left = px(x);
    node.top = px(y);
    node.width = px(w);
    node.height = px(h);
}

/// Largest 16:9 (RETRO_W:RETRO_H) rect that fits `win_w` × `win_h`, centred:
/// returns (x, y, w, h) in logical pixels.
pub fn letterbox_rect(win_w: f32, win_h: f32) -> (f32, f32, f32, f32) {
    let scale = (win_w / RETRO_W as f32).min(win_h / RETRO_H as f32);
    let w = RETRO_W as f32 * scale;
    let h = RETRO_H as f32 * scale;
    ((win_w - w) * 0.5, (win_h - h) * 0.5, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_fits_and_keeps_aspect(win_w: f32, win_h: f32) {
        let (x, y, w, h) = letterbox_rect(win_w, win_h);
        assert!(w <= win_w + 0.001 && h <= win_h + 0.001, "{win_w}x{win_h}");
        // Centred with symmetric bars.
        assert!((x * 2.0 + w - win_w).abs() < 0.01);
        assert!((y * 2.0 + h - win_h).abs() < 0.01);
        // Aspect preserved.
        let want = RETRO_W as f32 / RETRO_H as f32;
        assert!((w / h - want).abs() < 0.001, "aspect {} != {want}", w / h);
        // One axis fills the window exactly.
        assert!((w - win_w).abs() < 0.01 || (h - win_h).abs() < 0.01);
    }

    #[test]
    fn letterbox_preserves_aspect_across_window_shapes() {
        assert_fits_and_keeps_aspect(1920.0, 1080.0); // exact 16:9 → no bars
        assert_fits_and_keeps_aspect(1280.0, 800.0); // wider than tall → top/bottom bars
        assert_fits_and_keeps_aspect(800.0, 1280.0); // portrait phone → pillarbox
    }

    #[test]
    fn letterbox_exact_16_9_has_no_bars() {
        let (x, y, w, h) = letterbox_rect(1920.0, 1080.0);
        assert_eq!((x, y), (0.0, 0.0));
        assert_eq!((w, h), (1920.0, 1080.0));
    }
}
