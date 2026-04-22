// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

// Animated gradient background shader.
// Renders a smooth, slowly shifting color gradient.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Uniforms {
    time: f32,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Full-screen triangle
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0,  3.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0)
    );

    var output: VertexOutput;
    let pos = positions[vertex_index];
    output.position = vec4<f32>(pos.x, pos.y, 0.0, 1.0);
    output.uv = pos * 0.5 + 0.5;
    return output;
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let t = uniforms.time * 0.4;

    // Warm, wooden/leather brown tones for a TV case feel
    let base_r = 0.25 + 0.05 * sin(uv.x * 2.0 + t);
    let base_g = 0.15 + 0.03 * cos(uv.y * 2.0 - t * 0.5);
    let base_b = 0.08 + 0.02 * sin((uv.x + uv.y) * 1.5 + t * 0.7);

    // Subtle grain or wood-like streaks
    let grain = 0.02 * sin(uv.x * 100.0) * cos(uv.y * 5.0);
    
    // Vignette for the "case" itself
    let dist_from_center = length(uv - vec2<f32>(0.5, 0.5));
    let case_vignette = smoothstep(0.8, 0.3, dist_from_center);

    return vec4<f32>(
        (base_r + grain) * case_vignette,
        (base_g + grain) * case_vignette,
        (base_b + grain) * case_vignette,
        1.0
    );
}
