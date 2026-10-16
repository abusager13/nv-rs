// Reads the FSR output and writes it to Bevy's presentation target.
import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;

@group(0) @binding(0) var fsr_output: texture_2d<f32>;
@group(0) @binding(1) var fsr_sampler: sampler;

@fragment
fn blit(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return textureSample(fsr_output, fsr_sampler, in.uv);
}
