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

struct Skip {
    a: vec2<f32>,
    b: vec2<f32>,
    radius: f32,
    unused: f32,
}

struct State {
    dx: f32,
    dy: f32,
    sum: f32,
    inside: u32,
    m: u32,
    n: u32,
    sample: u32,
    unused: array<u32, 9>,
}

@group(0) @binding(0) var<storage, read> tiles: array<Tile>;
@group(0) @binding(1) var out_tex: texture_storage_2d_array<r32float, write>;
@group(0) @binding(4) var<storage, read_write> state: array<State>;
@group(0) @binding(5) var<storage, read_write> done: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read> orbit: array<vec2<f32>>;
@group(0) @binding(7) var<storage, read> skip: array<Skip>;

var<private> p: Tile;

const KERNEL: u32 = 3u;
const TILE: u32 = 256u;
const UNFINISHED: f32 = -2.0;

// NOTE: perturb.wgsl in 32-bit floats. Deltas only need relative precision, so f32 holds them while they stay above its normal range (about 1e-38); the CPU picks this shader only for levels where the pixel spacing is far above that.
fn advance(dcx: f32, dcy: f32, st: ptr<function, State>, spent: ptr<function, u32>) -> f32 {
    let bail = 65536.0;
    let last = p.ref_len - 1u;
    let top = 31u - countLeadingZeros(p.skip_p);
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
        if (*spent >= p.steps) {
            break;
        }
        var ndx = 0.0;
        var ndy = 0.0;
        var len: u32 = 0u;
        if (p.use_skip != 0u) {
            let dmag = dx * dx + dy * dy;
            var k = min(countTrailingZeros(m), top);
            loop {
                if (k == 0u) {
                    break;
                }
                let l = 1u << k;
                if (m + l <= last && n + l <= p.max_iter) {
                    let e = skip[2u * p.skip_p - ((2u * p.skip_p) >> k) + (m >> k)];
                    if (dmag < e.radius * e.radius) {
                        ndx = e.a.x * dx - e.a.y * dy + e.b.x * dcx - e.b.y * dcy;
                        ndy = e.a.x * dy + e.a.y * dx + e.b.x * dcy + e.b.y * dcx;
                        len = l;
                        break;
                    }
                }
                k = k - 1u;
            }
        }
        if (len == 0u) {
            let z = orbit[m];
            ndx = 2.0 * (z.x * dx - z.y * dy) + dx * dx - dy * dy + dcx;
            ndy = 2.0 * (z.x * dy + z.y * dx) + 2.0 * dx * dy + dcy;
            len = 1u;
        }
        m = m + len;
        n = n + len;
        *spent = *spent + len;
        let r = orbit[m];
        let fx = r.x + ndx;
        let fy = r.y + ndy;
        let mag = fx * fx + fy * fy;
        if (mag > bail) {
            result = f32(n) + 3.0 - log2(0.5 * log2(mag));
            break;
        }
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
    var st = State(0.0, 0.0, 0.0, 0u, 0u, 0u, 0u, array<u32, 9>());
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
        let dcx = f32(p.origin.x + f64(px) * p.step + (f64(i) + f64(0.5)) * sub);
        let dcy = f32(p.origin.y + f64(py) * p.step + (f64(j) + f64(0.5)) * sub);
        let v = advance(dcx, dcy, &st, &spent);
        if (v == UNFINISHED) {
            break;
        }
        if (v < 0.0) {
            st.inside = st.inside + 1u;
        } else {
            st.sum = st.sum + v;
        }
        st.dx = 0.0;
        st.dy = 0.0;
        st.m = 0u;
        st.n = 0u;
        st.sample = st.sample + 1u;
    }
    state[idx] = st;
    if (st.sample >= total) {
        var v: f32 = -1.0;
        let escaped = total - st.inside;
        if (escaped > 0u) {
            v = st.sum / f32(escaped);
        }
        textureStore(out_tex, vec2<i32>(i32(px), i32(py)), i32(p.layer), vec4<f32>(v, 0.0, 0.0, 0.0));
        atomicAdd(&done[slot], 1u);
    }
}
