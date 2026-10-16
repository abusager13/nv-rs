//! Runtime MetalFX spatial-upscaling option for Apple Silicon macOS.

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::RenderApp;

/// Whether the player's Display setting has MetalFX enabled.
#[derive(Resource, Default, Clone, ExtractResource)]
#[extract_app(RenderApp)]
pub struct MetalFxSetting {
    pub enabled: bool,
}

/// Installs the local Bevy 0.20 MetalFX adaptation where the framework exists.
pub struct ViewerMetalFxPlugin;

impl Plugin for ViewerMetalFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MetalFxSetting>();
        app.add_plugins(bevy::render::extract_resource::ExtractResourcePlugin::<
            MetalFxSetting,
        >::default());

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            app.add_plugins(bevy_metalfx::MetalFxPlugin {
                render_scale: 0.75,
                mode: bevy_metalfx::MetalFxMode::Disabled,
                adaptive: false,
                gpu_timing_sink: None,
            })
            .add_systems(
                Update,
                apply_setting.before(bevy_metalfx::MetalFxConfigSync),
            );
        }
    }
}

/// The setting is offered only on Apple Silicon macOS systems that expose
/// MetalFX's spatial scaler at runtime.
pub fn is_available() -> bool {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        bevy_metalfx::is_available()
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        false
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn apply_setting(
    setting: Res<MetalFxSetting>,
    mode: Option<ResMut<bevy_metalfx::MetalFxModeResource>>,
    scale: Option<ResMut<bevy_metalfx::MetalFxRenderScale>>,
) {
    if !setting.is_changed() || setting.is_added() || !bevy_metalfx::is_available() {
        return;
    }
    let Some(mut mode) = mode else { return };
    let Some(mut scale) = scale else { return };

    if setting.enabled {
        mode.set(bevy_metalfx::MetalFxMode::Spatial);
        scale.0 = 0.75;
    } else {
        mode.set(bevy_metalfx::MetalFxMode::Disabled);
        scale.0 = 1.0;
    }
}
