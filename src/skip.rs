const EPS: f64 = 1.0 / 16_777_216.0;

// NOTE: entry layout is [a.x, a.y, b.x, b.y, radius, unused], matching `Skip` in perturb.wgsl. An entry at level k covers 2^k orbit steps: delta' = a * delta + b * dc, valid while |delta| < radius.
pub struct Table {
    pub entries: Vec<[f64; 6]>,
    pub p: u32,
}

impl Table {
    pub fn build(orbit: &[[f64; 2]], dc_bound: f64) -> Self {
        let steps = orbit.len() - 1;
        let p = steps.next_power_of_two();
        let mut entries = vec![[0.0; 6]; 2 * p];
        for (i, z) in orbit[..steps].iter().enumerate() {
            entries[i] = [
                2.0 * z[0],
                2.0 * z[1],
                1.0,
                0.0,
                EPS * z[0].hypot(z[1]),
                0.0,
            ];
        }
        let mut level = 1;
        while p >> level > 0 {
            let (below, here) = (level_start(p, level - 1), level_start(p, level));
            for j in 0..p >> level {
                let (x, y) = (entries[below + 2 * j], entries[below + 2 * j + 1]);
                entries[here + j] = merge(x, y, dc_bound);
            }
            level += 1;
        }
        Table {
            entries,
            p: p as u32,
        }
    }

    #[cfg(test)]
    pub fn start(&self, level: u32) -> usize {
        level_start(self.p as usize, level as usize)
    }
}

fn level_start(p: usize, level: usize) -> usize {
    2 * p - ((2 * p) >> level)
}

fn merge(x: [f64; 6], y: [f64; 6], dc_bound: f64) -> [f64; 6] {
    let a = mul([y[0], y[1]], [x[0], x[1]]);
    let yb = mul([y[0], y[1]], [x[2], x[3]]);
    let b = [yb[0] + y[2], yb[1] + y[3]];
    let ax = x[0].hypot(x[1]);
    let spare = y[4] - x[2].hypot(x[3]) * dc_bound;
    let radius = if spare <= 0.0 {
        0.0
    } else if ax == 0.0 {
        x[4]
    } else {
        x[4].min(spare / ax)
    };
    if radius.is_finite() && a.iter().chain(&b).all(|v| v.is_finite()) {
        [a[0], a[1], b[0], b[1], radius, 0.0]
    } else {
        [0.0; 6]
    }
}

fn mul(u: [f64; 2], v: [f64; 2]) -> [f64; 2] {
    [u[0] * v[0] - u[1] * v[1], u[0] * v[1] + u[1] * v[0]]
}
