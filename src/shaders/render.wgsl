struct Globals {
    screen: vec2<f32>,
    color_scale: f32,
    color_offset: f32,
    text_rect: vec4<f32>,
    color_lin: f32,
    stripe: f32,
    _pad0: f32,
    _pad1: f32,
}

struct Instance {
    rect: vec4<f32>,
    uv: vec4<f32>,
    layer: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var tiles: texture_2d_array<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> g: Globals;
@group(0) @binding(3) var<storage, read> instances: array<Instance>;
@group(0) @binding(4) var text_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let inst = instances[ii];
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = mix(inst.rect.xy, inst.rect.zw, corner);
    let ndc = vec2<f32>(px.x / g.screen.x * 2.0 - 1.0, 1.0 - px.y / g.screen.y * 2.0);
    var o: VsOut;
    o.pos = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    o.layer = inst.layer;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let v = textureSample(tiles, samp, in.uv, i32(in.layer)).r;
    if (v < 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let t = log2(v + 1.0) * g.color_scale + v * g.color_lin + g.color_offset;
    let r = 0.5 + 0.5 * sin(t);
    let gg = 0.5 + 0.5 * sin(t + 2.094);
    let b = 0.5 + 0.5 * sin(t + 4.188);
    // NOTE: fine brightness stripes on the raw count keep structure visible where the hue barely changes.
    let k = 1.0 - g.stripe * (0.5 + 0.5 * sin(v * 0.15));
    return vec4<f32>(r * k, gg * k, b * k, 1.0);
}

struct TextOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_text(@builtin(vertex_index) vi: u32) -> TextOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = mix(g.text_rect.xy, g.text_rect.zw, corner);
    let ndc = vec2<f32>(px.x / g.screen.x * 2.0 - 1.0, 1.0 - px.y / g.screen.y * 2.0);
    var o: TextOut;
    o.pos = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = corner;
    return o;
}

@fragment
fn fs_text(in: TextOut) -> @location(0) vec4<f32> {
    let a = textureSample(text_tex, samp, in.uv).r;
    // NOTE: dark halo from the neighbouring texels keeps the text readable on bright fractal colours.
    let d = vec2<f32>(1.0) / vec2<f32>(textureDimensions(text_tex));
    let halo = max(max(textureSample(text_tex, samp, in.uv + vec2<f32>(d.x, 0.0)).r, textureSample(text_tex, samp, in.uv - vec2<f32>(d.x, 0.0)).r),
                   max(textureSample(text_tex, samp, in.uv + vec2<f32>(0.0, d.y)).r, textureSample(text_tex, samp, in.uv - vec2<f32>(0.0, d.y)).r));
    let alpha = max(a, halo * 0.8);
    return vec4<f32>(vec3<f32>(a), alpha);
}
