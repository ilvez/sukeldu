use std::collections::HashMap;

pub const TILE: u32 = 256;
pub const MAX_LEVEL: u32 = 48;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub level: u32,
    pub ix: i64,
    pub iy: i64,
}

impl TileKey {
    pub fn world_size(level: u32) -> f64 {
        4.0 * 2f64.powi(-(level as i32))
    }

    pub fn origin(&self) -> (f64, f64) {
        let s = Self::world_size(self.level);
        (-2.0 + self.ix as f64 * s, -2.0 + self.iy as f64 * s)
    }

    pub fn step(&self) -> f64 {
        Self::world_size(self.level) / TILE as f64
    }

    pub fn ancestor(&self, up: u32) -> TileKey {
        TileKey { level: self.level - up, ix: self.ix >> up, iy: self.iy >> up }
    }

    // NOTE: uv sub-rectangle of this tile inside its ancestor `up` levels above.
    pub fn uv_in_ancestor(&self, up: u32) -> [f32; 4] {
        let n = 1i64 << up;
        let fx = (self.ix & (n - 1)) as f32 / n as f32;
        let fy = (self.iy & (n - 1)) as f32 / n as f32;
        let w = 1.0 / n as f32;
        [fx, fy, fx + w, fy + w]
    }
}

#[derive(Clone, Copy)]
struct Slot {
    key: TileKey,
    last_used: u64,
    ready: bool,
}

pub struct Cache {
    map: HashMap<TileKey, u32>,
    slots: Vec<Option<Slot>>,
    free: Vec<u32>,
}

impl Cache {
    pub fn new(layers: u32) -> Self {
        Cache {
            map: HashMap::new(),
            slots: vec![None; layers as usize],
            free: (0..layers).rev().collect(),
        }
    }

    // NOTE: only fully computed tiles are returned; a tile still being sliced in must not be drawn.
    pub fn get(&mut self, key: TileKey, frame: u64) -> Option<u32> {
        let layer = *self.map.get(&key)?;
        let slot = self.slots[layer as usize].as_mut()?;
        if !slot.ready {
            return None;
        }
        slot.last_used = frame;
        Some(layer)
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.map.contains_key(&key)
    }

    pub fn alloc(&mut self, key: TileKey, frame: u64) -> Option<u32> {
        let layer = match self.free.pop() {
            Some(l) => l,
            None => self.evict(frame)?,
        };
        self.slots[layer as usize] = Some(Slot { key, last_used: frame, ready: false });
        self.map.insert(key, layer);
        Some(layer)
    }

    pub fn mark_ready(&mut self, layer: u32, frame: u64) {
        if let Some(slot) = self.slots[layer as usize].as_mut() {
            slot.ready = true;
            slot.last_used = frame;
        }
    }

    fn evict(&mut self, frame: u64) -> Option<u32> {
        let (layer, key) = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|slot| (i as u32, slot)))
            .filter(|(_, s)| s.ready && s.last_used + 1 < frame && s.key.level > 0)
            .min_by_key(|(_, s)| s.last_used)
            .map(|(i, s)| (i, s.key))?;
        self.map.remove(&key);
        self.slots[layer as usize] = None;
        Some(layer)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    pub cx: f64,
    pub cy: f64,
    pub upp: f64,
}

impl View {
    pub fn home() -> Self {
        View { cx: -0.6, cy: 0.0, upp: 1.0 }
    }

    pub fn fit(&mut self, w: u32, h: u32) {
        self.upp = 3.0 / h.min(w) as f64;
    }

    pub fn target_level(&self) -> u32 {
        let l = (4.0 / (TILE as f64 * self.upp)).log2().ceil();
        l.clamp(0.0, MAX_LEVEL as f64) as u32
    }

    pub fn screen_to_world(&self, sx: f64, sy: f64, w: u32, h: u32) -> (f64, f64) {
        (
            self.cx + (sx - w as f64 / 2.0) * self.upp,
            self.cy + (sy - h as f64 / 2.0) * self.upp,
        )
    }

    pub fn world_to_screen(&self, wx: f64, wy: f64, w: u32, h: u32) -> (f64, f64) {
        (
            (wx - self.cx) / self.upp + w as f64 / 2.0,
            (wy - self.cy) / self.upp + h as f64 / 2.0,
        )
    }

    pub fn zoom_about(&mut self, factor: f64, sx: f64, sy: f64, w: u32, h: u32) {
        let (fx, fy) = self.screen_to_world(sx, sy, w, h);
        let new_upp = (self.upp * factor).clamp(1e-16, 0.05);
        let factor = new_upp / self.upp;
        self.cx = fx + (self.cx - fx) * factor;
        self.cy = fy + (self.cy - fy) * factor;
        self.upp = new_upp;
    }

    pub fn visible_tiles(&self, level: u32, w: u32, h: u32) -> Vec<TileKey> {
        let s = TileKey::world_size(level);
        let n = 1i64 << level;
        let (x0, y0) = self.screen_to_world(0.0, 0.0, w, h);
        let (x1, y1) = self.screen_to_world(w as f64, h as f64, w, h);
        let ix0 = (((x0 + 2.0) / s).floor() as i64).clamp(0, n - 1);
        let ix1 = (((x1 + 2.0) / s).floor() as i64).clamp(0, n - 1);
        let iy0 = (((y0 + 2.0) / s).floor() as i64).clamp(0, n - 1);
        let iy1 = (((y1 + 2.0) / s).floor() as i64).clamp(0, n - 1);
        let mut out = Vec::new();
        for iy in iy0..=iy1 {
            for ix in ix0..=ix1 {
                out.push(TileKey { level, ix, iy });
            }
        }
        out
    }
}
