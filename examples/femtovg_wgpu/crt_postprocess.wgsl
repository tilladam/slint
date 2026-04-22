// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

// CRT / retro monitor post-processing shader.
// Applies scanlines, barrel distortion, chromatic aberration, and vignette.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Params {
    resolution: vec2<f32>,
    time: f32,
    scanline_intensity: f32,
    curvature: f32,
    chromatic_aberration: f32,
    vignette_intensity: f32,
    _pad: f32,
};

@group(0) @binding(0)
var tex: texture_2d<f32>;
@group(0) @binding(1)
var tex_sampler: sampler;
@group(0) @binding(2)
var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0,  3.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0)
    );

    var output: VertexOutput;
    let pos = positions[vertex_index];
    output.position = vec4<f32>(pos.x, pos.y, 0.0, 1.0);
    output.uv = vec2<f32>(pos.x * 0.5 + 0.5, 1.0 - (pos.y * 0.5 + 0.5));
    return output;
}

// Barrel distortion: curves UV coordinates to simulate a CRT screen curvature
fn barrel_distort(uv: vec2<f32>, amount: f32) -> vec2<f32> {
    let centered = uv * 2.0 - 1.0;
    let r2 = dot(centered, centered);
    let distorted = centered * (1.0 + amount * r2);
    return distorted * 0.5 + 0.5;
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let curvature_amount = params.curvature * 0.3;

    // Apply barrel distortion
    let distorted_uv = barrel_distort(uv, curvature_amount);

    // Transparent outside the curved screen area to show the "case" background
    if distorted_uv.x < 0.0 || distorted_uv.x > 1.0 || distorted_uv.y < 0.0 || distorted_uv.y > 1.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // Inner shadow at the edge of the "tube"
    let edge_dist = min(
        min(distorted_uv.x, 1.0 - distorted_uv.x),
        min(distorted_uv.y, 1.0 - distorted_uv.y)
    );
    let inner_shadow = smoothstep(0.0, 0.05, edge_dist);

    // Chromatic aberration: offset R and B channels slightly
    let aberration = params.chromatic_aberration * 0.005;
    let r = textureSample(tex, tex_sampler, distorted_uv + vec2<f32>(aberration, 0.0)).r;
    let g = textureSample(tex, tex_sampler, distorted_uv).g;
    let b = textureSample(tex, tex_sampler, distorted_uv - vec2<f32>(aberration, 0.0)).b;
    let a = textureSample(tex, tex_sampler, distorted_uv).a;
    var color = vec4<f32>(r, g, b, a);

    // Scanlines: darken every other row with a sine pattern
    let scanline = sin(distorted_uv.y * params.resolution.y * 3.14159) * 0.5 + 0.5;
    let scanline_factor = 1.0 - params.scanline_intensity * (1.0 - scanline);
    // Subtle flicker
    let flicker = 1.0 - 0.02 * params.scanline_intensity * sin(params.time * 8.0);
    color = vec4<f32>(color.rgb * scanline_factor * flicker, color.a);

    // Vignette: darken the edges
    let centered = uv * 2.0 - 1.0;
    let vignette = 1.0 - params.vignette_intensity * dot(centered, centered);
    color = vec4<f32>(color.rgb * max(vignette, 0.0) * inner_shadow, color.a);

    // Subtle phosphor glow (slightly boost brightness near the center)
    let glow = 1.0 + 0.05 * (1.0 - dot(centered, centered));
    color = vec4<f32>(color.rgb * glow, color.a);

    return color;
}
