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
    let t = uniforms.time * 0.8;

    // Animated gradient with bold, sweeping color waves
    let r = 0.3 + 0.3 * sin(uv.x * 4.0 + t * 1.2);
    let g = 0.2 + 0.25 * sin(uv.y * 3.0 - t * 0.9 + 2.0);
    let b = 0.4 + 0.35 * sin((uv.x + uv.y) * 2.5 + t * 1.5 + 1.0);

    // Moving radial glow that orbits the center
    let center = vec2<f32>(0.5 + 0.25 * sin(t * 0.7), 0.5 + 0.25 * cos(t * 0.5));
    let dist = length(uv - center);
    let glow = 0.4 * exp(-dist * dist * 3.0);

    // Second glow on the opposite side
    let center2 = vec2<f32>(0.5 - 0.2 * sin(t * 0.6 + 1.0), 0.5 - 0.2 * cos(t * 0.8));
    let dist2 = length(uv - center2);
    let glow2 = 0.25 * exp(-dist2 * dist2 * 5.0);

    return vec4<f32>(
        r + glow + glow2 * 0.3,
        g + glow * 0.6 + glow2,
        b + glow * 0.3 + glow2 * 0.5,
        1.0
    );
}
