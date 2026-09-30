struct Params {
    origin: vec2<f64>,
    step: f64,
    layer: u32,
    max_iter: u32,
    row0: u32,
    samples: u32,
    ref_len: u32,
    bla_p: u32,
    use_bla: u32,
    slot: u32,
    pass_index: u32,
}

struct Bla {
    a: vec2<f64>,
    b: vec2<f64>,
    radius: f64,
    unused: f64,
}

struct State {
    dx: f64,
    dy: f64,
    sum: f32,
    inside: u32,
    m: u32,
    n: u32,
    sample: u32,
    unused: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var out_tex: texture_storage_2d_array<r32float, write>;
@group(0) @binding(2) var<storage, read> orbit: array<vec2<f64>>;
@group(0) @binding(3) var<storage, read> bla: array<Bla>;
@group(0) @binding(4) var<storage, read_write> state: array<State>;

override STEP_CAP: u32 = 512u;

const TILE: u32 = 256u;
const UNFINISHED: f32 = -2.0;

// Returns the smooth count once the sample escapes, -1 when it reaches max_iter, UNFINISHED when the per-dispatch budget ran out first.
fn advance(dcx: f64, dcy: f64, st: ptr<function, State>, spent: ptr<function, u32>) -> f32 {
    let two = f64(2.0);
    let bail = f64(65536.0);
    let last = p.ref_len - 1u;
    let top = 31u - countLeadingZeros(p.bla_p);
    var dx = (*st).dx;
    var dy = (*st).dy;
    var m = (*st).m;
    var n = (*st).n;
    var result = UNFINISHED;
    loop {
        if (n >= p.max_iter) {
            result = -1.0;
            break;
        }
        if (*spent >= STEP_CAP) {
            break;
        }
        var ndx = f64(0.0);
        var ndy = f64(0.0);
        var skip: u32 = 0u;
        // NOTE: bilinear skip. An entry at level k replaces 2^k steps by delta' = a * delta + b * dc, valid while |delta| is below its radius; the longest valid one starting at orbit index m is taken (only indices divisible by 2^k start a level-k entry).
        if (p.use_bla != 0u) {
            let dmag = dx * dx + dy * dy;
            var k = min(countTrailingZeros(m), top);
            loop {
                if (k == 0u) {
                    break;
                }
                let l = 1u << k;
                if (m + l <= last && n + l <= p.max_iter) {
                    let e = bla[2u * p.bla_p - ((2u * p.bla_p) >> k) + (m >> k)];
                    if (dmag < e.radius * e.radius) {
                        ndx = e.a.x * dx - e.a.y * dy + e.b.x * dcx - e.b.y * dcy;
                        ndy = e.a.x * dy + e.a.y * dx + e.b.x * dcy + e.b.y * dcx;
                        skip = l;
                        break;
                    }
                }
                k = k - 1u;
            }
        }
        if (skip == 0u) {
            let z = orbit[m];
            ndx = two * (z.x * dx - z.y * dy) + dx * dx - dy * dy + dcx;
            ndy = two * (z.x * dy + z.y * dx) + two * dx * dy + dcy;
            skip = 1u;
        }
        m = m + skip;
        n = n + skip;
        *spent = *spent + skip;
        let r = orbit[m];
        let fx = r.x + ndx;
        let fy = r.y + ndy;
        let mag = fx * fx + fy * fy;
        if (mag > bail) {
            // NOTE: smooth iteration count for bailout radius 256, same formula as compute.wgsl.
            result = f32(n) + 3.0 - log2(0.5 * log2(f32(mag)));
            break;
        }
        // NOTE: rebasing. When the full orbit value is smaller than the delta, or the reference orbit ends, continue with the full value against the start of the reference; this is what makes one reference valid for every pixel.
        if (mag < ndx * ndx + ndy * ndy || m == last) {
            dx = fx;
            dy = fy;
            m = 0u;
        } else {
            dx = ndx;
            dy = ndy;
        }
    }
    (*st).dx = dx;
    (*st).dy = dy;
    (*st).m = m;
    (*st).n = n;
    return result;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = gid.x;
    let py = gid.y + p.row0;
    if (px >= TILE || py >= TILE) {
        return;
    }
    let s = max(p.samples, 1u);
    let total = s * s;
    let idx = p.slot * TILE * TILE + py * TILE + px;
    var st = State(f64(0.0), f64(0.0), 0.0, 0u, 0u, 0u, 0u, 0u);
    if (p.pass_index != 0u) {
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
        let dcx = p.origin.x + f64(px) * p.step + (f64(i) + f64(0.5)) * sub;
        let dcy = p.origin.y + f64(py) * p.step + (f64(j) + f64(0.5)) * sub;
        let v = advance(dcx, dcy, &st, &spent);
        if (v == UNFINISHED) {
            break;
        }
        if (v < 0.0) {
            st.inside = st.inside + 1u;
        } else {
            st.sum = st.sum + v;
        }
        st.dx = f64(0.0);
        st.dy = f64(0.0);
        st.m = 0u;
        st.n = 0u;
        st.sample = st.sample + 1u;
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
    }
}
