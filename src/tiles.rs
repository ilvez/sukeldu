use std::collections::HashMap;

use rug::{Float, Integer};

pub const TILE: u32 = 256;
pub const MAX_COMPUTE_LEVEL: u32 = 900;

const MAX_LOG2_UPP: f64 = -4.321928094887362;
const PREC_STEP: u32 = 64;
const PREC_GUARD: u32 = 72;
const LOG10_2: f64 = std::f64::consts::LOG10_2;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub level: u32,
    pub ix: Integer,
    pub iy: Integer,
}

impl TileKey {
    pub fn root() -> Self {
        TileKey {
            level: 0,
            ix: Integer::new(),
            iy: Integer::new(),
        }
    }

    pub fn world_size(level: u32) -> f64 {
        4.0 * 2f64.powi(-(level as i32))
    }

    // NOTE: the f64 origin is exact only while the index fits 53 bits (level <= 52); deeper tiles are placed relative to a reference point instead.
    pub fn origin(&self) -> (f64, f64) {
        let s = Self::world_size(self.level);
        (-2.0 + self.ix.to_f64() * s, -2.0 + self.iy.to_f64() * s)
    }

    pub fn step(&self) -> f64 {
        Self::world_size(self.level) / TILE as f64
    }

    pub fn ancestor(&self, up: u32) -> TileKey {
        TileKey {
            level: self.level - up,
            ix: self.ix.clone() >> up,
            iy: self.iy.clone() >> up,
        }
    }

    // NOTE: uv sub-rectangle of this tile inside its ancestor `up` levels above.
    pub fn uv_in_ancestor(&self, up: u32) -> [f32; 4] {
        let n = 2f64.powi(up as i32);
        let fx = (self.ix.clone().keep_bits(up).to_f64() / n) as f32;
        let fy = (self.iy.clone().keep_bits(up).to_f64() / n) as f32;
        let w = (1.0 / n) as f32;
        [fx, fy, fx + w, fy + w]
    }
}

#[derive(Clone)]
struct Slot {
    key: TileKey,
    last_used: u64,
    ready: bool,
}

pub struct Cache {
    map: HashMap<TileKey, u32>,
    slots: Vec<Option<Slot>>,
    free: Vec<u32>,
    max_ready_level: u32,
}

impl Cache {
    pub fn new(layers: u32) -> Self {
        Cache {
            map: HashMap::new(),
            slots: vec![None; layers as usize],
            free: (0..layers).rev().collect(),
            max_ready_level: 0,
        }
    }

    // NOTE: an upper bound, not lowered on eviction; it only lets the ancestor search skip levels that cannot hold a computed tile.
    pub fn max_ready_level(&self) -> u32 {
        self.max_ready_level
    }

    // NOTE: only fully computed tiles are returned; a tile still being sliced in must not be drawn.
    pub fn get(&mut self, key: &TileKey, frame: u64) -> Option<u32> {
        let layer = *self.map.get(key)?;
        let slot = self.slots[layer as usize].as_mut()?;
        if !slot.ready {
            return None;
        }
        slot.last_used = frame;
        Some(layer)
    }

    pub fn contains(&self, key: &TileKey) -> bool {
        self.map.contains_key(key)
    }

    pub fn alloc(&mut self, key: TileKey, frame: u64) -> Option<u32> {
        let layer = match self.free.pop() {
            Some(l) => l,
            None => self.evict(frame)?,
        };
        self.slots[layer as usize] = Some(Slot {
            key: key.clone(),
            last_used: frame,
            ready: false,
        });
        self.map.insert(key, layer);
        Some(layer)
    }

    pub fn mark_ready(&mut self, layer: u32, frame: u64) {
        if let Some(slot) = self.slots[layer as usize].as_mut() {
            slot.ready = true;
            slot.last_used = frame;
            self.max_ready_level = self.max_ready_level.max(slot.key.level);
        }
    }

    // NOTE: only for tiles still being computed; their layer is freed without ever having been drawn.
    pub fn cancel(&mut self, key: &TileKey) {
        if let Some(layer) = self.map.remove(key) {
            self.slots[layer as usize] = None;
            self.free.push(layer);
        }
    }

    fn evict(&mut self, frame: u64) -> Option<u32> {
        let (layer, key) = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|slot| (i as u32, slot)))
            .filter(|(_, s)| s.ready && s.last_used + 1 < frame && s.key.level > 0)
            .min_by_key(|(_, s)| s.last_used)
            .map(|(i, s)| (i, s.key.clone()))?;
        self.map.remove(&key);
        self.slots[layer as usize] = None;
        Some(layer)
    }
}

// NOTE: the centre is fixed-point, (world + 2) / 4 scaled by 2^prec. prec follows the depth so a
// pixel offset always stays far above the last bit, which is what removes the depth limit.
#[derive(Clone)]
pub struct View {
    cx: Integer,
    cy: Integer,
    prec: u32,
    pub log2_upp: f64,
}

impl View {
    pub fn home() -> Self {
        Self::from_world(&Float::with_val(64, -0.6), &Float::with_val(64, 0.0), 0.0)
    }

    pub fn at(cx: &str, cy: &str, upp: &str) -> Option<Self> {
        let upp = Float::with_val(64, Float::parse(upp).ok()?);
        if !(upp.is_finite() && upp > 0) {
            return None;
        }
        let log2_upp = upp.log2().to_f64();
        let bits = prec_for(log2_upp) + PREC_STEP;
        let x = Float::with_val(bits, Float::parse(cx).ok()?);
        let y = Float::with_val(bits, Float::parse(cy).ok()?);
        if !(x.is_finite() && y.is_finite()) {
            return None;
        }
        Some(Self::from_world(&x, &y, log2_upp))
    }

    pub fn fit(&mut self, w: u32, h: u32) {
        self.log2_upp = (3.0 / w.min(h) as f64).log2();
        self.grow_prec();
    }

    pub fn target_level(&self) -> u32 {
        level_for(self.log2_upp)
    }

    pub fn depth_log10(&self, home_log2_upp: f64) -> f64 {
        (home_log2_upp - self.log2_upp) * LOG10_2
    }

    pub fn upp_string(&self, digits: usize) -> String {
        let e10 = self.log2_upp * LOG10_2;
        let mut exp = e10.floor();
        let mut mantissa = 10f64.powf(e10 - exp);
        let scale = 10f64.powi(digits as i32);
        if (mantissa * scale).round() >= 10.0 * scale {
            mantissa = 1.0;
            exp += 1.0;
        }
        format!("{:.*}e{}", digits, mantissa, exp)
    }

    pub fn world(&self) -> (f64, f64) {
        (
            self.world_axis(&self.cx).to_f64(),
            self.world_axis(&self.cy).to_f64(),
        )
    }

    pub fn world_strings(&self) -> (String, String) {
        let decimals = (((-self.log2_upp + 8.0) * LOG10_2).ceil() as usize + 3).max(17);
        (
            format!("{:.*}", decimals, self.world_axis(&self.cx)),
            format!("{:.*}", decimals, self.world_axis(&self.cy)),
        )
    }

    pub fn centre_world(&self, bits: u32) -> (Float, Float) {
        (
            Float::with_val(bits, self.world_axis(&self.cx)),
            Float::with_val(bits, self.world_axis(&self.cy)),
        )
    }

    pub fn offset_px(&self, cx: &Float, cy: &Float) -> (f64, f64) {
        let scale = Float::with_val(64, -self.log2_upp).exp2();
        let axis = |fixed: &Integer, c: &Float| {
            let mut d = self.world_axis(fixed);
            d -= c;
            d *= &scale;
            d.to_f64()
        };
        (axis(&self.cx, cx), axis(&self.cy, cy))
    }

    pub fn add_pixels(&mut self, dx: f64, dy: f64) {
        let (fx, fy) = (self.pixels_to_fixed(dx), self.pixels_to_fixed(dy));
        self.cx += fx;
        self.cy += fy;
    }

    pub fn zoom_about(&mut self, factor: f64, sx: f64, sy: f64, w: u32, h: u32) {
        let new_log2_upp = (self.log2_upp + factor.log2()).min(MAX_LOG2_UPP);
        let factor = (new_log2_upp - self.log2_upp).exp2();
        let (ox, oy) = (sx - w as f64 / 2.0, sy - h as f64 / 2.0);
        self.add_pixels(ox * (1.0 - factor), oy * (1.0 - factor));
        self.log2_upp = new_log2_upp;
        self.grow_prec();
    }

    pub fn centre_key(&self, level: u32) -> TileKey {
        let shift = self.prec - level;
        TileKey {
            level,
            ix: Integer::from(&self.cx >> shift),
            iy: Integer::from(&self.cy >> shift),
        }
    }

    pub fn tile_frac(&self, key: &TileKey) -> (f64, f64) {
        let shift = self.prec - key.level;
        (
            frac_in_tile(&self.cx, &key.ix, shift),
            frac_in_tile(&self.cy, &key.iy, shift),
        )
    }

    pub fn tile_rect(&self, key: &TileKey, w: u32, h: u32) -> (f64, f64, f64, f64) {
        let (fx, fy) = self.tile_frac(key);
        let s = self.tile_px(key.level);
        let x0 = w as f64 / 2.0 - fx * s;
        let y0 = h as f64 / 2.0 - fy * s;
        (x0, y0, x0 + s, y0 + s)
    }

    pub fn pixels_to(&self, key: &TileKey, u: f64, v: f64) -> (f64, f64) {
        let (fx, fy) = self.tile_frac(key);
        let s = self.tile_px(key.level);
        ((u - fx) * s, (v - fy) * s)
    }

    pub fn visible_tiles(&self, level: u32, w: u32, h: u32) -> Vec<TileKey> {
        let centre = self.centre_key(level);
        let (fx, fy) = self.tile_frac(&centre);
        let s = self.tile_px(level);
        let (hw, hh) = (w as f64 / 2.0 / s, h as f64 / 2.0 / s);
        let n = Integer::from(1) << level;
        let mut out = Vec::new();
        for dy in (fy - hh).floor() as i64..=(fy + hh).floor() as i64 {
            for dx in (fx - hw).floor() as i64..=(fx + hw).floor() as i64 {
                let ix = Integer::from(&centre.ix + dx);
                let iy = Integer::from(&centre.iy + dy);
                if ix >= 0 && ix < n && iy >= 0 && iy < n {
                    out.push(TileKey { level, ix, iy });
                }
            }
        }
        out
    }

    fn from_world(x: &Float, y: &Float, log2_upp: f64) -> Self {
        let prec = prec_for(log2_upp);
        View {
            cx: to_fixed(x, prec),
            cy: to_fixed(y, prec),
            prec,
            log2_upp,
        }
    }

    fn grow_prec(&mut self) {
        let want = prec_for(self.log2_upp);
        if want > self.prec {
            let extra = want - self.prec;
            self.cx <<= extra;
            self.cy <<= extra;
            self.prec = want;
        }
    }

    fn tile_px(&self, level: u32) -> f64 {
        (2.0 - level as f64 - self.log2_upp).exp2()
    }

    fn pixels_to_fixed(&self, px: f64) -> Integer {
        let scale = Float::with_val(64, self.log2_upp - 2.0 + self.prec as f64).exp2();
        (Float::with_val(64, px) * scale)
            .to_integer()
            .unwrap_or_default()
    }

    fn world_axis(&self, fixed: &Integer) -> Float {
        let mut w = Float::with_val(self.prec + PREC_STEP, fixed);
        w >>= self.prec;
        w <<= 2u32;
        w -= 2;
        w
    }
}

fn level_for(log2_upp: f64) -> u32 {
    (-log2_upp - 6.0).ceil().max(0.0) as u32
}

fn prec_for(log2_upp: f64) -> u32 {
    (level_for(log2_upp) + PREC_GUARD).div_ceil(PREC_STEP) * PREC_STEP
}

fn to_fixed(world: &Float, prec: u32) -> Integer {
    let mut u = Float::with_val(prec + PREC_STEP, world);
    u += 2;
    u >>= 2u32;
    u <<= prec;
    u.to_integer().unwrap_or_default()
}

fn frac_in_tile(centre: &Integer, tile: &Integer, shift: u32) -> f64 {
    let rel = centre - Integer::from(tile << shift);
    (Float::with_val(64, &rel) >> shift).to_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1280;
    const H: u32 = 800;

    fn deep_view() -> View {
        View::at("-0.7436438870371587", "0.1318259042053119", "1e-60").unwrap()
    }

    fn rect_centre(view: &View, key: &TileKey) -> (f64, f64) {
        let (x0, y0, x1, y1) = view.tile_rect(key, W, H);
        ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
    }

    #[test]
    fn zoom_about_leaves_the_tile_under_the_cursor_in_place() {
        let mut view = deep_view();
        let key = view.centre_key(view.target_level());
        view.add_pixels(120.0, 45.0);
        let (fx, fy) = rect_centre(&view, &key);

        view.zoom_about(0.9, fx, fy, W, H);

        let (ax, ay) = rect_centre(&view, &key);
        assert!((ax - fx).abs() < 1e-6, "{ax} vs {fx}");
        assert!((ay - fy).abs() < 1e-6, "{ay} vs {fy}");
    }

    #[test]
    fn repeated_zoom_grows_precision_with_depth() {
        let mut view = deep_view();
        let start = view.target_level();

        for _ in 0..1000 {
            view.zoom_about(0.5, 300.0, 200.0, W, H);
        }

        assert_eq!(view.target_level(), start + 1000);
        let key = view.centre_key(view.target_level());
        let (fx, fy) = view.tile_frac(&key);
        assert!((0.0..1.0).contains(&fx) && (0.0..1.0).contains(&fy));
    }

    #[test]
    fn panning_there_and_back_returns_to_the_same_place() {
        let mut view = deep_view();
        let key = view.centre_key(view.target_level());
        let before = view.tile_frac(&key);

        view.add_pixels(1234.5, -987.25);
        view.add_pixels(-1234.5, 987.25);

        let after = view.tile_frac(&key);
        assert!((before.0 - after.0).abs() < 1e-9);
        assert!((before.1 - after.1).abs() < 1e-9);
    }

    #[test]
    fn parent_rect_is_the_union_of_its_children() {
        let view = deep_view();
        let level = view.target_level() - 1;
        let parent = view.centre_key(level);
        let child = |dx: u32, dy: u32| TileKey {
            level: level + 1,
            ix: parent.ix.clone() * 2u32 + dx,
            iy: parent.iy.clone() * 2u32 + dy,
        };

        let (px0, py0, px1, py1) = view.tile_rect(&parent, W, H);
        let (cx0, cy0, _, _) = view.tile_rect(&child(0, 0), W, H);
        let (_, _, cx1, cy1) = view.tile_rect(&child(1, 1), W, H);

        assert!((px0 - cx0).abs() < 1e-6 && (py0 - cy0).abs() < 1e-6);
        assert!((px1 - cx1).abs() < 1e-6 && (py1 - cy1).abs() < 1e-6);
    }

    #[test]
    fn location_strings_round_trip_beyond_f64_depth() {
        let view = deep_view();
        let (x, y) = view.world_strings();

        let again = View::at(&x, &y, &view.upp_string(15)).unwrap();

        let key = view.centre_key(view.target_level());
        let (a, b) = (view.tile_frac(&key), again.tile_frac(&key));
        assert!(
            (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3,
            "{a:?} vs {b:?}"
        );
    }

    #[test]
    fn upp_below_f64_range_is_accepted() {
        let view = View::at("0", "1", "1e-400").unwrap();
        assert!(view.target_level() > 1300);
        assert!(view.upp_string(3).ends_with("e-400"));
    }

    #[test]
    fn invalid_location_is_rejected() {
        assert!(View::at("abc", "0", "1e-3").is_none());
        assert!(View::at("0", "0", "0").is_none());
        assert!(View::at("0", "0", "-1").is_none());
    }

    #[test]
    fn visible_tiles_cover_the_window() {
        let view = deep_view();
        let tiles = view.visible_tiles(view.target_level(), W, H);

        let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
        let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
        for key in &tiles {
            let (x0, y0, x1, y1) = view.tile_rect(key, W, H);
            min_x = min_x.min(x0);
            min_y = min_y.min(y0);
            max_x = max_x.max(x1);
            max_y = max_y.max(y1);
        }
        assert!(min_x <= 0.0 && min_y <= 0.0 && max_x >= W as f64 && max_y >= H as f64);
    }
}
