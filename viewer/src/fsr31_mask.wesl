// First-person geometry has no world-camera motion vectors. Mark those pixels
// reactive so FSR favors the current weapon and hand sample over old history.
import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;

@group(0) @binding(0) var first_person_depth: texture_depth_2d;

@fragment
fn reactive_mask(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(in.position.xy);
    let depth = textureLoad(first_person_depth, pixel, 0);
    let reactive = select(0.0, 1.0, depth > 0.0);
    return vec4<f32>(reactive, 0.0, 0.0, 1.0);
}
