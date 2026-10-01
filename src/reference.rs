use std::sync::mpsc::{self, Receiver};
use std::thread;

use rug::{Assign, Float};

use crate::skip::Table;
use crate::tiles::{TileKey, View};

pub const LEVEL_SPAN: u32 = 32;
pub const LEVEL_REFRESH_MARGIN: u32 = 8;
pub const MAX_DISTANCE_PX: f64 = 1_073_741_824.0;

const ORBIT_GUARD_BITS: u32 = 96;
const BAILOUT: f64 = 65536.0;
const SKIP_DISTANCE_PX: f64 = 65536.0;

pub struct Reference {
    cx: Float,
    cy: Float,
    skip_dc: f64,
    pub level: u32,
    pub len: u32,
    pub skip_p: u32,
}

pub type Orbit = Vec<[f64; 2]>;

pub struct Computed {
    pub reference: Reference,
    pub orbit: Orbit,
    pub skip: Table,
}

pub struct Located {
    pub offset: [f64; 2],
    pub skip: bool,
}

pub fn bits_for(level: u32) -> u32 {
    level + LEVEL_SPAN + ORBIT_GUARD_BITS
}

pub fn spawn(cx: Float, cy: Float, level: u32, max_iter: u32) -> Receiver<Computed> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let orbit = compute_orbit(&cx, &cy, max_iter);
        let skip_dc = SKIP_DISTANCE_PX * (TileKey::world_size(level) / crate::tiles::TILE as f64);
        let skip = Table::build(&orbit, skip_dc);
        let reference = Reference {
            cx,
            cy,
            skip_dc,
            level,
            len: orbit.len() as u32,
            skip_p: skip.p,
        };
        let _ = tx.send(Computed {
            reference,
            orbit,
            skip,
        });
    });
    rx
}

impl Reference {
    pub fn centre(&self) -> [f64; 2] {
        [self.cx.to_f64(), self.cy.to_f64()]
    }

    pub fn distance_px(&self, view: &View) -> f64 {
        let (dx, dy) = view.offset_px(&self.cx, &self.cy);
        dx.hypot(dy)
    }

    // NOTE: the skip table is only valid for deltas up to skip_dc, so a tile whose farthest pixel is beyond that must iterate step by step.
    pub fn locate(&self, key: &TileKey) -> Option<Located> {
        if key.level > self.level + LEVEL_SPAN {
            return None;
        }
        let bits = self.cx.prec().max(key.level + 64);
        let offset = |index: &rug::Integer, reference: &Float| {
            let mut w = Float::with_val(bits, index);
            w >>= key.level;
            w <<= 2u32;
            w -= 2;
            w -= reference;
            w.to_f64()
        };
        let (ox, oy) = (offset(&key.ix, &self.cx), offset(&key.iy, &self.cy));
        let size = TileKey::world_size(key.level);
        let distance = (ox + size / 2.0).hypot(oy + size / 2.0) / key.step();
        let farthest = ox
            .abs()
            .max((ox + size).abs())
            .hypot(oy.abs().max((oy + size).abs()));
        (distance <= MAX_DISTANCE_PX).then_some(Located {
            offset: [ox, oy],
            skip: farthest <= self.skip_dc,
        })
    }
}

fn compute_orbit(cx: &Float, cy: &Float, max_iter: u32) -> Orbit {
    let bits = cx.prec();
    let mut zx = Float::new(bits);
    let mut zy = Float::new(bits);
    let mut sq_x = Float::new(bits);
    let mut sq_y = Float::new(bits);
    let mut xy = Float::new(bits);
    let mut orbit = Vec::with_capacity(max_iter as usize + 1);
    orbit.push([0.0, 0.0]);
    for _ in 0..max_iter {
        sq_x.assign(&zx * &zx);
        sq_y.assign(&zy * &zy);
        xy.assign(&zx * &zy);
        zx.assign(&sq_x - &sq_y);
        zx += cx;
        zy.assign(&xy * 2u32);
        zy += cy;
        let (fx, fy) = (zx.to_f64(), zy.to_f64());
        orbit.push([fx, fy]);
        if fx * fx + fy * fy > BAILOUT {
            break;
        }
    }
    orbit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(x: f64, y: f64) -> (Float, Float) {
        (Float::with_val(300, x), Float::with_val(300, y))
    }

    fn orbit_of(x: f64, y: f64, max_iter: u32) -> Orbit {
        let (cx, cy) = world(x, y);
        spawn(cx, cy, 10, max_iter).recv().unwrap().orbit
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Step {
        Escaped(u32),
        Interior,
        Unfinished,
    }

    #[derive(Default)]
    struct Walk {
        dx: f64,
        dy: f64,
        m: usize,
        n: u32,
    }

    // NOTE: mirrors `advance` in shaders/perturb.wgsl step for step, so the algorithm, including rebasing, skipping and the per-dispatch budget, is checked without a GPU.
    fn advance(
        orbit: &Orbit,
        table: Option<&Table>,
        walk: &mut Walk,
        (dcx, dcy): (f64, f64),
        max_iter: u32,
        spent: &mut u32,
        cap: u32,
    ) -> Step {
        let last = orbit.len() - 1;
        loop {
            if walk.n >= max_iter {
                return Step::Interior;
            }
            if *spent >= cap {
                return Step::Unfinished;
            }
            let (dx, dy, m, n) = (walk.dx, walk.dy, walk.m, walk.n);
            let (mut ndx, mut ndy, mut len) = (0.0, 0.0, 0usize);
            if let Some(table) = table {
                let dmag = dx * dx + dy * dy;
                let mut k = (m.trailing_zeros()).min(table.p.ilog2());
                while k >= 1 {
                    let l = 1usize << k;
                    if m + l <= last && n as usize + l <= max_iter as usize {
                        let e = table.entries[table.start(k) + (m >> k)];
                        if dmag < e[4] * e[4] {
                            ndx = e[0] * dx - e[1] * dy + e[2] * dcx - e[3] * dcy;
                            ndy = e[0] * dy + e[1] * dx + e[2] * dcy + e[3] * dcx;
                            len = l;
                            break;
                        }
                    }
                    k -= 1;
                }
            }
            if len == 0 {
                let z = orbit[m];
                ndx = 2.0 * (z[0] * dx - z[1] * dy) + dx * dx - dy * dy + dcx;
                ndy = 2.0 * (z[0] * dy + z[1] * dx) + 2.0 * dx * dy + dcy;
                len = 1;
            }
            walk.m += len;
            walk.n += len as u32;
            *spent += len as u32;
            let (fx, fy) = (orbit[walk.m][0] + ndx, orbit[walk.m][1] + ndy);
            let mag = fx * fx + fy * fy;
            if mag > BAILOUT {
                return Step::Escaped(walk.n);
            }
            if mag < ndx * ndx + ndy * ndy || walk.m == last {
                (walk.dx, walk.dy, walk.m) = (fx, fy, 0);
            } else {
                (walk.dx, walk.dy) = (ndx, ndy);
            }
        }
    }

    fn perturbed_escape(
        orbit: &Orbit,
        table: Option<&Table>,
        dcx: f64,
        dcy: f64,
        max_iter: u32,
    ) -> Option<u32> {
        let mut spent = 0;
        match advance(
            orbit,
            table,
            &mut Walk::default(),
            (dcx, dcy),
            max_iter,
            &mut spent,
            u32::MAX,
        ) {
            Step::Escaped(n) => Some(n),
            _ => None,
        }
    }

    fn dispatches_until_done(
        orbit: &Orbit,
        table: Option<&Table>,
        samples: &[(f64, f64)],
        max_iter: u32,
        cap: u32,
    ) -> (Vec<Step>, u32) {
        let (mut results, mut dispatches) = (Vec::new(), 0);
        let mut walk = Walk::default();
        while results.len() < samples.len() {
            dispatches += 1;
            let mut spent = 0;
            while results.len() < samples.len() {
                let dc = samples[results.len()];
                match advance(orbit, table, &mut walk, dc, max_iter, &mut spent, cap) {
                    Step::Unfinished => break,
                    done => {
                        results.push(done);
                        walk = Walk::default();
                    }
                }
            }
        }
        (results, dispatches)
    }

    fn exact_escape(cx: &Float, cy: &Float, max_iter: u32) -> Option<u32> {
        let orbit = compute_orbit(cx, cy, max_iter);
        let last = orbit.last().unwrap();
        (last[0] * last[0] + last[1] * last[1] > BAILOUT).then(|| orbit.len() as u32 - 1)
    }

    #[test]
    fn orbit_follows_the_quadratic_map() {
        let orbit = orbit_of(-0.75, 0.1, 50);

        let (mut zx, mut zy) = (0.0f64, 0.0f64);
        for z in &orbit[1..30] {
            (zx, zy) = (zx * zx - zy * zy - 0.75, 2.0 * zx * zy + 0.1);
            assert!((z[0] - zx).abs() < 1e-9 && (z[1] - zy).abs() < 1e-9);
        }
    }

    #[test]
    fn orbit_stops_once_escaped() {
        let orbit = orbit_of(1.0, 0.0, 1000);

        let last = orbit.last().unwrap();
        assert!(orbit.len() < 20);
        assert!(last[0] * last[0] + last[1] * last[1] > BAILOUT);
    }

    #[test]
    fn orbit_of_an_interior_point_runs_to_the_iteration_limit() {
        assert_eq!(orbit_of(-0.1, 0.0, 500).len(), 501);
    }

    fn distinct_counts_matching_exact(x0: f64, y0: f64, spacing: f64, max_iter: u32) -> usize {
        let orbit = orbit_of(x0, y0, max_iter);
        let table = Table::build(&orbit, spacing * 8.0);
        let (ref_x, ref_y) = world(x0, y0);
        let mut counts = std::collections::BTreeSet::new();
        for i in -4..=4 {
            for j in -4..=4 {
                let (dx, dy) = (i as f64 * spacing, j as f64 * spacing);
                let exact = exact_escape(&(ref_x.clone() + dx), &(ref_y.clone() + dy), max_iter);
                let stepped = perturbed_escape(&orbit, None, dx, dy, max_iter);
                let skipped = perturbed_escape(&orbit, Some(&table), dx, dy, max_iter);
                assert_eq!(stepped, exact, "stepping, offset ({dx}, {dy})");
                assert_eq!(skipped, exact, "skipping, offset ({dx}, {dy})");
                counts.insert(exact);
            }
        }
        counts.len()
    }

    #[test]
    fn splitting_the_work_across_dispatches_does_not_change_the_result() {
        let orbit = orbit_of(0.0, 1.0, 500);
        let table = Table::build(&orbit, 1e-5);
        let samples: Vec<(f64, f64)> = (-3..=3)
            .flat_map(|i| (-3..=3).map(move |j| (i as f64 * 1e-6, j as f64 * 1e-6)))
            .collect();

        for table in [None, Some(&table)] {
            let (whole, one) = dispatches_until_done(&orbit, table, &samples, 500, u32::MAX);
            let (split, many) = dispatches_until_done(&orbit, table, &samples, 500, 7);
            assert_eq!(one, 1);
            assert!(many > 1);
            assert_eq!(split, whole);
        }
    }

    #[test]
    fn skip_table_allows_long_skips_near_a_boundary_point() {
        let orbit = orbit_of(0.0, 1.0, 500);

        let table = Table::build(&orbit, 1e-29);

        let longest = (1..table.p.ilog2())
            .filter(|&k| {
                let start = table.start(k);
                (0..(table.p >> k) as usize).any(|j| table.entries[start + j][4] > 0.0)
            })
            .max();
        assert!(longest.unwrap_or(0) >= 3, "longest skip level {longest:?}");
    }

    #[test]
    fn perturbed_escape_counts_match_exact_arithmetic_near_a_spiral() {
        distinct_counts_matching_exact(-0.7436438870371587, 0.1318259042053119, 3e-8, 400);
    }

    #[test]
    fn perturbed_escape_counts_match_exact_arithmetic_around_a_boundary_point() {
        assert!(distinct_counts_matching_exact(0.0, 1.0, 1e-6, 500) > 5);
    }

    #[test]
    fn perturbed_escape_counts_match_exact_arithmetic_beyond_f64_pixel_spacing() {
        assert!(distinct_counts_matching_exact(0.0, 1.0, 1e-30, 500) > 5);
    }

    #[test]
    fn locate_accepts_the_tile_under_the_reference_and_rejects_far_or_too_deep_tiles() {
        let view = View::at("-0.7436438870371587", "0.1318259042053119", "1e-20").unwrap();
        let level = view.target_level();
        let (cx, cy) = view.centre_world(bits_for(level));
        let reference = spawn(cx, cy, level, 64).recv().unwrap().reference;
        let key = view.centre_key(level);

        let located = reference.locate(&key).unwrap();
        let size = TileKey::world_size(level);
        assert!(located.offset[0].abs() <= size && located.offset[1].abs() <= size);
        assert!(located.skip);

        let deeper = TileKey {
            level: level + LEVEL_SPAN + 1,
            ix: key.ix.clone() << (LEVEL_SPAN + 1),
            iy: key.iy.clone() << (LEVEL_SPAN + 1),
        };
        assert!(reference.locate(&deeper).is_none());

        let far = TileKey {
            level,
            ix: key.ix.clone() + (1u64 << 40),
            iy: key.iy.clone(),
        };
        assert!(reference.locate(&far).is_none());
    }
}
