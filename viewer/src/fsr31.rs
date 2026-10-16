//! Optional FSR 3.1 temporal upscaling through the viewer's wgpu renderer.
//!
//! This integrates the local wgpu 30 adaptation of the MIT-licensed
//! `wgpu-ffx` 0.1.0 implementation (AMD FidelityFX SDK 1.1.4 / FSR 3.1.4).
//! The renderer keeps the world and first-person cameras separate: world
//! depth and motion vectors drive accumulation, while first-person depth is
//! used to mark the weapon and hands reactive.

use bevy::asset::{load_internal_asset, uuid_handle};
use bevy::camera::{CameraOutputMode, MainPassResolutionOverride, Projection};
use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, ViewPrepassTextures};
use bevy::core_pipeline::schedule::Core3d;
use bevy::core_pipeline::upscaling::upscaling;
use bevy::core_pipeline::FullscreenShader;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, texture_depth_2d};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::sync_world::RenderEntity;
use bevy::render::texture::CachedTexture;
use bevy::render::view::{ExtractedCamera, MipBias, ViewTarget};
use bevy::render::{Extract, ExtractSchedule, RenderApp, RenderSystems};
use bevy::window::PrimaryWindow;
use wgpu_ffx::{
    FsrContext, FsrContextFlags, FsrContextInfo, FsrDispatchFlags, FsrDispatchInfo, FsrView,
    get_jitter_offset, get_jitter_phase_count,
};

const SHADER: Handle<Shader> = uuid_handle!("98b21d8f-5067-4fcb-b8a2-4c5a83b13109");
const RENDER_SCALE: f32 = 2.0 / 3.0;

/// The Display-menu switch. It starts off and is copied to the render world.
#[derive(Resource, Default, Clone, ExtractResource)]
#[extract_app(RenderApp)]
pub struct Fsr31Setting {
    pub enabled: bool,
}

/// Marks the camera whose prepass supplies world depth and motion vectors.
#[derive(Component, Clone, Copy, ExtractComponent)]
#[extract_component_filter(With<Camera3d>)]
#[extract_app(RenderApp)]
pub struct FsrWorldCamera;

/// Marks the final camera, which writes the combined world and first-person image.
#[derive(Component, Clone, Copy, ExtractComponent)]
#[extract_component_filter(With<Camera3d>)]
#[extract_app(RenderApp)]
pub struct FsrFinalCamera;

#[derive(Resource, Clone, Copy, Default, ExtractResource)]
#[extract_app(RenderApp)]
struct FsrFrameDelta {
    milliseconds: f32,
}

#[derive(Default)]
struct CameraSyncState {
    enabled: Option<bool>,
    size: Option<(u32, u32)>,
}

/// Enables temporal inputs and the lower render size only while the setting is on.
fn sync_cameras(
    setting: Res<Fsr31Setting>,
    windows: Query<&Window, With<PrimaryWindow>>,
    worlds: Query<Entity, With<FsrWorldCamera>>,
    finals: Query<Entity, With<FsrFinalCamera>>,
    mut commands: Commands,
    mut state: Local<CameraSyncState>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let size = (window.physical_width(), window.physical_height());
    if size.0 == 0 || size.1 == 0 {
        return;
    }
    if state.enabled == Some(setting.enabled) && state.size == Some(size) {
        return;
    }
    state.enabled = Some(setting.enabled);
    state.size = Some(size);

    for entity in &worlds {
        let mut entity = commands.entity(entity);
        if setting.enabled {
            let (width, height) = scaled_size(size.0, size.1);
            entity.insert((
                MainPassResolutionOverride(UVec2::new(width, height)),
                MipBias(RENDER_SCALE.log2()),
                DepthPrepass,
                MotionVectorPrepass,
                TemporalJitter::default(),
            ));
        } else {
            entity.remove::<(
                MainPassResolutionOverride,
                MipBias,
                DepthPrepass,
                MotionVectorPrepass,
                TemporalJitter,
            )>();
        }
    }
    for entity in &finals {
        let mut entity = commands.entity(entity);
        if setting.enabled {
            let (width, height) = scaled_size(size.0, size.1);
            entity.insert((
                MainPassResolutionOverride(UVec2::new(width, height)),
                MipBias(RENDER_SCALE.log2()),
                DepthPrepass,
                TemporalJitter::default(),
            ));
        } else {
            entity.remove::<(
                MainPassResolutionOverride,
                MipBias,
                DepthPrepass,
                TemporalJitter,
            )>();
        }
    }
}

/// Supplies the same low-discrepancy jitter to both camera passes.
fn update_jitter(
    setting: Res<Fsr31Setting>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<&mut TemporalJitter, Or<(With<FsrWorldCamera>, With<FsrFinalCamera>)>>,
    mut frame: Local<i32>,
) {
    if !setting.enabled {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (render_width, _) = scaled_size(window.physical_width(), window.physical_height());
    let width = window.physical_width();
    if render_width < 2 || width == 0 {
        return;
    }
    let phases = get_jitter_phase_count(render_width as i32, width as i32).max(1);
    let offset = get_jitter_offset(*frame, phases);
    *frame = frame.wrapping_add(1);
    for mut jitter in &mut cameras {
        jitter.offset = Vec2::new(offset[0], offset[1]);
    }
}

fn update_frame_delta(time: Res<Time<Real>>, mut delta: ResMut<FsrFrameDelta>) {
    delta.milliseconds = (time.delta_secs() * 1000.0).clamp(1.0, 200.0);
}

fn scaled_size(width: u32, height: u32) -> (u32, u32) {
    (
        (width as f32 * RENDER_SCALE).round().max(2.0) as u32,
        (height as f32 * RENDER_SCALE).round().max(2.0) as u32,
    )
}

/// Adds Bevy's non-Clone render-world-only resolution override to marked cameras.
fn extract_resolution_override(
    mut commands: Commands,
    cameras: Extract<
        Query<
            (
                &RenderEntity,
                Option<&MainPassResolutionOverride>,
            ),
            Or<(With<FsrWorldCamera>, With<FsrFinalCamera>)>,
        >,
    >,
) {
    for (render_entity, resolution) in &cameras {
        let Ok(mut entity) = commands.get_entity(render_entity.id()) else {
            continue;
        };
        if let Some(resolution) = resolution {
            entity.insert(MainPassResolutionOverride(**resolution));
        } else {
            entity.remove::<MainPassResolutionOverride>();
        }
    }
}

struct ExternalFsrTextures {
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    reactive_mask: wgpu::Texture,
    reactive_view: wgpu::TextureView,
    dilated_depth: wgpu::Texture,
    dilated_motion_vectors: wgpu::Texture,
    reconstructed_previous_depth: wgpu::Buffer,
}

impl ExternalFsrTextures {
    fn new(device: &wgpu::Device, render: [u32; 2], upscale: [u32; 2]) -> Self {
        let texture = |label, size, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let storage_sampled = TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING;
        let output = texture(
            "fsr31_output",
            upscale,
            TextureFormat::Rgba16Float,
            storage_sampled,
        );
        let output_view = output.create_view(&TextureViewDescriptor::default());
        let reactive_mask = texture(
            "fsr31_reactive_mask",
            render,
            TextureFormat::R8Unorm,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        );
        let reactive_view = reactive_mask.create_view(&TextureViewDescriptor::default());
        let dilated_depth = texture(
            "fsr31_dilated_depth",
            render,
            TextureFormat::R32Float,
            storage_sampled,
        );
        let dilated_motion_vectors = texture(
            "fsr31_dilated_motion_vectors",
            render,
            TextureFormat::Rg16Float,
            storage_sampled,
        );
        let reconstructed_previous_depth = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fsr31_reconstructed_previous_depth"),
            size: render[0] as u64 * render[1] as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            output,
            output_view,
            reactive_mask,
            reactive_view,
            dilated_depth,
            dilated_motion_vectors,
            reconstructed_previous_depth,
        }
    }
}

#[derive(Resource, Default)]
struct FsrGpuState {
    context: Option<FsrContext>,
    view: Option<FsrView>,
    external: Option<ExternalFsrTextures>,
    sizes: Option<([u32; 2], [u32; 2])>,
    was_enabled: bool,
    failed: bool,
}

#[derive(Resource)]
struct FsrPipelines {
    blit_layout: BindGroupLayout,
    mask_layout: BindGroupLayout,
    sampler: Sampler,
    mask: CachedRenderPipelineId,
    blit: std::collections::HashMap<TextureFormat, CachedRenderPipelineId>,
    fullscreen: FullscreenShader,
}

impl FromWorld for FsrPipelines {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        let fullscreen = world.resource::<FullscreenShader>().clone();
        let blit_layout_descriptor = BindGroupLayoutDescriptor::new(
            "fsr31_blit_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    texture_2d(TextureSampleType::Float { filterable: true }),
                    sampler(SamplerBindingType::Filtering),
                ),
            ),
        );
        let blit_layout = device.create_bind_group_layout(
            blit_layout_descriptor.label.as_ref(),
            &blit_layout_descriptor.entries,
        );
        let mask_layout_descriptor = BindGroupLayoutDescriptor::new(
            "fsr31_reactive_mask_layout",
            &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (texture_depth_2d(),)),
        );
        let mask_layout = device.create_bind_group_layout(
            mask_layout_descriptor.label.as_ref(),
            &mask_layout_descriptor.entries,
        );
        let sampler = device.create_sampler(&SamplerDescriptor {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..default()
        });
        let mask_descriptor = RenderPipelineDescriptor {
            label: Some("fsr31_reactive_mask_pipeline".into()),
            layout: vec![mask_layout_descriptor],
            vertex: fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: SHADER,
                shader_defs: vec![],
                entry_point: Some("reactive_mask".into()),
                constants: vec![],
                targets: vec![Some(ColorTargetState {
                    format: TextureFormat::R8Unorm,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            immediate_size: 0,
            zero_initialize_workgroup_memory: false,
        };
        let mask = world
            .resource_mut::<PipelineCache>()
            .queue_render_pipeline(mask_descriptor);
        Self {
            blit_layout,
            mask_layout,
            sampler,
            mask,
            blit: std::collections::HashMap::new(),
            fullscreen,
        }
    }
}

impl FsrPipelines {
    fn blit_pipeline(
        &mut self,
        target: TextureFormat,
        cache: &mut PipelineCache,
    ) -> CachedRenderPipelineId {
        *self.blit.entry(target).or_insert_with(|| {
            cache.queue_render_pipeline(RenderPipelineDescriptor {
                label: Some("fsr31_blit_pipeline".into()),
                layout: vec![BindGroupLayoutDescriptor::new(
                    "fsr31_blit_layout",
                    &BindGroupLayoutEntries::sequential(
                        ShaderStages::FRAGMENT,
                        (
                            texture_2d(TextureSampleType::Float { filterable: true }),
                            sampler(SamplerBindingType::Filtering),
                        ),
                    ),
                )],
                vertex: self.fullscreen.to_vertex_state(),
                fragment: Some(FragmentState {
                    shader: SHADER,
                    shader_defs: vec![],
                    entry_point: Some("blit".into()),
                    constants: vec![],
                    targets: vec![Some(ColorTargetState {
                        format: target,
                        blend: None,
                        write_mask: ColorWrites::ALL,
                    })],
                }),
                primitive: PrimitiveState::default(),
                depth_stencil: None,
                multisample: MultisampleState::default(),
                immediate_size: 0,
                zero_initialize_workgroup_memory: false,
            })
        })
    }
}

/// Runs FSR after Bevy's bilinear upscale and replaces that output before present.
#[allow(clippy::too_many_arguments)]
fn fsr31_upscale(
    final_view: ViewQuery<(
        &'static ViewTarget,
        &'static ExtractedCamera,
        Option<&'static ViewPrepassTextures>,
    )>,
    world_views: Query<
        (
            &ViewPrepassTextures,
            &Projection,
            &TemporalJitter,
        ),
        With<FsrWorldCamera>,
    >,
    setting: Res<Fsr31Setting>,
    delta: Res<FsrFrameDelta>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut state: ResMut<FsrGpuState>,
    mut pipelines: ResMut<FsrPipelines>,
    mut pipeline_cache: ResMut<PipelineCache>,
    mut render_context: RenderContext,
) {
    if !setting.enabled {
        state.context = None;
        state.view = None;
        state.external = None;
        state.sizes = None;
        state.was_enabled = false;
        state.failed = false;
        return;
    }
    if state.failed {
        return;
    }
    let (target, camera, final_prepass) = final_view.into_inner();
    if !matches!(camera.output_mode, CameraOutputMode::Write { .. })
        || target.out_texture_view_format().is_none()
    {
        return;
    }
    let Ok((world_prepass, projection, jitter)) = world_views.single() else {
        return;
    };
    let Some(depth) = world_prepass.depth.as_ref() else {
        return;
    };
    let Some(motion) = world_prepass.motion_vectors.as_ref() else {
        return;
    };
    let Some(final_depth) = final_prepass.and_then(|p| p.depth.as_ref()) else {
        return;
    };

    let source = target.main_texture();
    if source.format() != TextureFormat::Rgba16Float {
        log::warn_once!("FSR 3.1 needs the viewer's HDR Rgba16Float camera target");
        return;
    }
    let display = [source.size().width, source.size().height];
    let render = [scaled_size(display[0], display[1]).0, scaled_size(display[0], display[1]).1];
    if render[0] < 2 || render[1] < 2 {
        return;
    }

    if state.context.is_none() {
        let device_wgpu = device.wgpu_device();
        let scope = device_wgpu.push_error_scope(wgpu::ErrorFilter::Validation);
        state.context = Some(FsrContext::new(FsrContextInfo {
            device: device_wgpu.clone(),
            flags: FsrContextFlags::HIGH_DYNAMIC_RANGE
                | FsrContextFlags::DEPTH_INVERTED
                | FsrContextFlags::DEPTH_INFINITE,
        }));
        if let Some(error) = pollster::block_on(scope.pop()) {
            log::error!("FSR 3.1 could not initialize on this adapter: {error}");
            state.context = None;
            state.failed = true;
            return;
        }
    }

    let sizes = (render, display);
    let mut reset_history = !state.was_enabled || state.sizes != Some(sizes);
    if state.sizes != Some(sizes) {
        let Some(context) = state.context.as_ref() else {
            return;
        };
        state.view = Some(context.create_view(&queue, render, display));
        state.external = Some(ExternalFsrTextures::new(device.wgpu_device(), render, display));
        state.sizes = Some(sizes);
    }
    state.was_enabled = true;
    let (Some(context), Some(view), Some(external)) =
        (state.context.as_ref(), state.view.as_mut(), state.external.as_ref())
    else {
        return;
    };

    // Mark first-person geometry highly reactive: its camera has a cleared
    // depth buffer and its velocity cannot represent the world camera's view.
    let mask_bind_group = device.create_bind_group(
        "fsr31_reactive_mask_bind_group",
        &pipelines.mask_layout,
        &BindGroupEntries::sequential((&final_depth.texture.default_view,)),
    );
    let Some(mask_pipeline) = pipeline_cache.get_render_pipeline(pipelines.mask) else {
        return;
    };
    let mask_pass = RenderPassDescriptor {
        label: Some("fsr31_reactive_mask"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: &external.reactive_view,
            depth_slice: None,
            resolve_target: None,
            ops: Operations {
                load: LoadOp::Clear(LinearRgba::BLACK.into()),
                store: StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };
    {
        let mut pass = render_context.command_encoder().begin_render_pass(&mask_pass);
        pass.set_pipeline(mask_pipeline);
        pass.set_bind_group(0, &mask_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    let (near, far, fov_y) = match projection {
        Projection::Perspective(p) => (p.near, p.far, p.fov),
        _ => {
            log::warn_once!("FSR 3.1 currently requires the viewer's perspective camera");
            return;
        }
    };
    let dispatch = FsrDispatchInfo {
        color: source.texture.clone(),
        depth: depth.texture.texture.clone(),
        motion_vectors: motion.texture.texture.clone(),
        exposure: None,
        reactive_mask: Some(external.reactive_mask.clone()),
        transparency_and_composition: None,
        dilated_depth: external.dilated_depth.clone(),
        dilated_motion_vectors: external.dilated_motion_vectors.clone(),
        reconstructed_previous_depth: external.reconstructed_previous_depth.clone(),
        output: external.output.clone(),
        jitter_offset: jitter.offset.to_array(),
        // Bevy writes current-minus-previous UV displacement; FSR expects the
        // opposite direction in pixel units (current pixel to previous one).
        motion_vector_scale: [-(render[0] as f32), -(render[1] as f32)],
        render_size: render,
        upscale_size: display,
        enable_sharpening: true,
        sharpness: 0.2,
        frame_time_delta: delta.milliseconds,
        pre_exposure: 1.0,
        reset_history,
        camera_near: near,
        camera_far: far,
        camera_fov_y: fov_y,
        view_space_to_meters_factor: 1.0,
        flags: FsrDispatchFlags::empty(),
    };
    if let Err(error) = context.dispatch(view, render_context.command_encoder(), &dispatch) {
        log::error!("FSR 3.1 skipped a frame: {error}");
        state.failed = true;
        return;
    }
    reset_history = false;
    let _ = reset_history;

    let Some(target_format) = target.out_texture_view_format() else {
        return;
    };
    let blit_id = pipelines.blit_pipeline(target_format, &mut pipeline_cache);
    let Some(blit_pipeline) = pipeline_cache.get_render_pipeline(blit_id) else {
        return;
    };
    let bind_group = device.create_bind_group(
        "fsr31_blit_bind_group",
        &pipelines.blit_layout,
        &BindGroupEntries::sequential((&external.output_view, &pipelines.sampler)),
    );
    let Some(attachment) = target.out_texture_color_attachment(None) else {
        return;
    };
    let descriptor = RenderPassDescriptor {
        label: Some("fsr31_blit"),
        color_attachments: &[Some(attachment)],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };
    let mut pass = render_context.command_encoder().begin_render_pass(&descriptor);
    pass.set_pipeline(blit_pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

/// Installs FSR and its per-camera inputs. The setting is available anywhere
/// the desktop wgpu renderer is built, including macOS/Metal.
pub struct ViewerFsr31Plugin;

impl Plugin for ViewerFsr31Plugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, SHADER, "fsr31.wesl", Shader::from_wesl);
        app.init_resource::<Fsr31Setting>()
            .init_resource::<FsrFrameDelta>()
            .add_plugins((
                ExtractResourcePlugin::<Fsr31Setting>::default(),
                ExtractResourcePlugin::<FsrFrameDelta>::default(),
                ExtractComponentPlugin::<FsrWorldCamera>::default(),
                ExtractComponentPlugin::<FsrFinalCamera>::default(),
            ))
            .add_systems(Update, (sync_cameras, update_jitter).chain())
            .add_systems(Update, update_frame_delta);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<FsrGpuState>()
            .init_resource::<FsrPipelines>()
            .add_systems(
                ExtractSchedule,
                extract_resolution_override.after(bevy::render::camera::extract_cameras),
            )
            .add_systems(Core3d, fsr31_upscale.after(upscaling).in_set(RenderSystems::Render));
    }
}

pub fn is_available() -> bool {
    !cfg!(target_family = "wasm")
}
