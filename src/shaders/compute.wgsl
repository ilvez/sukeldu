struct Params {
    origin: vec2<f64>,
    step: f64,
    layer: u32,
    max_iter: u32,
    row0: u32,
    samples: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var out_tex: texture_storage_2d_array<r32float, write>;

const TILE: u32 = 256u;

fn iterate(cx: f64, cy: f64) -> f32 {
    // NOTE: main cardioid and period-2 bulb are interior; skipping them avoids max_iter loops on the most common inside pixels.
    let q = (cx - f64(0.25)) * (cx - f64(0.25)) + cy * cy;
    if (q * (q + (cx - f64(0.25))) <= f64(0.25) * cy * cy || (cx + f64(1.0)) * (cx + f64(1.0)) + cy * cy <= f64(0.0625)) {
        return -1.0;
    }
    var zx = f64(0.0);
    var zy = f64(0.0);
    var zx2 = f64(0.0);
    var zy2 = f64(0.0);
    var n: u32 = 0u;
    let bail = f64(65536.0);
    // NOTE: Brent-style periodicity check: interior orbits converge to a cycle, and matching a saved point ends them long before max_iter.
    // The tolerance must stay far below the pixel spacing at any supported depth, or chaotic boundary orbits get flagged as interior.
    var sx = f64(0.0);
    var sy = f64(0.0);
    var next_save: u32 = 8u;
    var save_gap: u32 = 8u;
    let eps = f64(1e-60);
    loop {
        if (n >= p.max_iter || zx2 + zy2 > bail) {
            break;
        }
        zy = f64(2.0) * zx * zy + cy;
        zx = zx2 - zy2 + cx;
        zx2 = zx * zx;
        zy2 = zy * zy;
        n = n + 1u;
        let dx = zx - sx;
        let dy = zy - sy;
        if (dx * dx + dy * dy < eps) {
            return -1.0;
        }
        if (n == next_save) {
            sx = zx;
            sy = zy;
            save_gap = min(save_gap * 2u, 4096u);
            next_save = n + save_gap;
        }
    }
    if (n >= p.max_iter) {
        return -1.0;
    }
    let mag2 = f32(zx2 + zy2);
    // NOTE: smooth iteration count for bailout radius 256: log2(log2(256)) = 3.
    return f32(n) + 3.0 - log2(0.5 * log2(mag2));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = gid.x;
    let py = gid.y + p.row0;
    if (px >= TILE || py >= TILE) {
        return;
    }
    let s = max(p.samples, 1u);
    let sub = p.step / f64(s);
    var sum: f32 = 0.0;
    var inside: u32 = 0u;
    for (var j: u32 = 0u; j < s; j = j + 1u) {
        for (var i: u32 = 0u; i < s; i = i + 1u) {
            let cx = p.origin.x + f64(px) * p.step + (f64(i) + f64(0.5)) * sub;
            let cy = p.origin.y + f64(py) * p.step + (f64(j) + f64(0.5)) * sub;
            let v = iterate(cx, cy);
            if (v < 0.0) {
                inside = inside + 1u;
            } else {
                sum = sum + v;
            }
        }
    }
    // NOTE: a texel is stored as interior only when every sample is; otherwise the escaped samples' mean keeps boundary pixels coloured.
    var v: f32 = -1.0;
    let escaped = s * s - inside;
    if (escaped > 0u) {
        v = sum / f32(escaped);
    }
    textureStore(out_tex, vec2<i32>(i32(px), i32(py)), i32(p.layer), vec4<f32>(v, 0.0, 0.0, 0.0));
}
