struct Tile {
    origin: vec2<f64>,
    step: f64,
    layer: u32,
    max_iter: u32,
    samples: u32,
    ref_len: u32,
    skip_p: u32,
    use_skip: u32,
    kernel: u32,
    first: u32,
    steps: u32,
    unused: u32,
}

struct State {
    zx: f64,
    zy: f64,
    sx: f64,
    sy: f64,
    sum: f32,
    inside: u32,
    n: u32,
    sample: u32,
    next_save: u32,
    save_gap: u32,
    unused: vec2<u32>,
}

@group(0) @binding(0) var<storage, read> tiles: array<Tile>;
@group(0) @binding(1) var out_tex: texture_storage_2d_array<r32float, write>;
@group(0) @binding(4) var<storage, read_write> state: array<State>;
@group(0) @binding(5) var<storage, read_write> done: array<atomic<u32>>;

var<private> p: Tile;

const KERNEL: u32 = 1u;
const TILE: u32 = 256u;
const UNFINISHED: f32 = -2.0;
const FIRST_SAVE: u32 = 8u;

fn fresh() -> State {
    return State(f64(0.0), f64(0.0), f64(0.0), f64(0.0), 0.0, 0u, 0u, 0u, FIRST_SAVE, FIRST_SAVE, vec2<u32>(0u));
}

// NOTE: main cardioid and period-2 bulb are interior; skipping them avoids max_iter loops on the most common inside pixels.
fn in_main_bulbs(cx: f64, cy: f64) -> bool {
    let q = (cx - f64(0.25)) * (cx - f64(0.25)) + cy * cy;
    return q * (q + (cx - f64(0.25))) <= f64(0.25) * cy * cy || (cx + f64(1.0)) * (cx + f64(1.0)) + cy * cy <= f64(0.0625);
}

// Returns the smooth count once the sample escapes, -1 when it is interior, UNFINISHED when the per-dispatch budget ran out first.
fn advance(cx: f64, cy: f64, st: ptr<function, State>, spent: ptr<function, u32>) -> f32 {
    var zx = (*st).zx;
    var zy = (*st).zy;
    var zx2 = zx * zx;
    var zy2 = zy * zy;
    var n = (*st).n;
    var sx = (*st).sx;
    var sy = (*st).sy;
    var next_save = (*st).next_save;
    var save_gap = (*st).save_gap;
    let bail = f64(65536.0);
    // NOTE: Brent-style periodicity check: interior orbits converge to a cycle, and matching a saved point ends them long before max_iter.
    // The tolerance must stay far below the pixel spacing at any supported depth, or chaotic boundary orbits get flagged as interior.
    let eps = f64(1e-60);
    var result = UNFINISHED;
    loop {
        if (n >= p.max_iter) {
            result = -1.0;
            break;
        }
        if (zx2 + zy2 > bail) {
            // NOTE: smooth iteration count for bailout radius 256: log2(log2(256)) = 3.
            result = f32(n) + 3.0 - log2(0.5 * log2(f32(zx2 + zy2)));
            break;
        }
        if (*spent >= p.steps) {
            break;
        }
        zy = f64(2.0) * zx * zy + cy;
        zx = zx2 - zy2 + cx;
        zx2 = zx * zx;
        zy2 = zy * zy;
        n = n + 1u;
        *spent = *spent + 1u;
        let dx = zx - sx;
        let dy = zy - sy;
        if (dx * dx + dy * dy < eps) {
            result = -1.0;
            break;
        }
        if (n == next_save) {
            sx = zx;
            sy = zy;
            save_gap = min(save_gap * 2u, 4096u);
            next_save = n + save_gap;
        }
    }
    (*st).zx = zx;
    (*st).zy = zy;
    (*st).n = n;
    (*st).sx = sx;
    (*st).sy = sy;
    (*st).next_save = next_save;
    (*st).save_gap = save_gap;
    return result;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let slot = gid.z;
    p = tiles[slot];
    if (p.kernel != KERNEL) {
        return;
    }
    let px = gid.x;
    let py = gid.y;
    let s = max(p.samples, 1u);
    let total = s * s;
    let idx = slot * TILE * TILE + py * TILE + px;
    var st = fresh();
    if (p.first == 0u) {
        st = state[idx];
    }
    if (st.sample >= total) {
        return;
    }
    let sub = p.step / f64(s);
    var spent: u32 = 0u;
    loop {
        if (st.sample >= total) {
            break;
        }
        let i = st.sample % s;
        let j = st.sample / s;
        let cx = p.origin.x + f64(px) * p.step + (f64(i) + f64(0.5)) * sub;
        let cy = p.origin.y + f64(py) * p.step + (f64(j) + f64(0.5)) * sub;
        var v: f32 = -1.0;
        if (st.n != 0u || !in_main_bulbs(cx, cy)) {
            v = advance(cx, cy, &st, &spent);
        }
        if (v == UNFINISHED) {
            break;
        }
        if (v < 0.0) {
            st.inside = st.inside + 1u;
        } else {
            st.sum = st.sum + v;
        }
        let sum = st.sum;
        let inside = st.inside;
        let sample = st.sample;
        st = fresh();
        st.sum = sum;
        st.inside = inside;
        st.sample = sample + 1u;
    }
    state[idx] = st;
    if (st.sample >= total) {
        // NOTE: a texel is stored as interior only when every sample is; otherwise the escaped samples' mean keeps boundary pixels coloured.
        var v: f32 = -1.0;
        let escaped = total - st.inside;
        if (escaped > 0u) {
            v = st.sum / f32(escaped);
        }
        textureStore(out_tex, vec2<i32>(i32(px), i32(py)), i32(p.layer), vec4<f32>(v, 0.0, 0.0, 0.0));
        atomicAdd(&done[slot], 1u);
    }
}
