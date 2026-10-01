mod gpu;
mod reference;
mod skip;
mod text;
mod tiles;

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Fullscreen, Window, WindowId};

use gpu::{Globals, Gpu, Instance, Kernel, MAX_INSTANCES, STATE_SLOTS, TILE_PIXELS, TileParams};
use reference::{Computed, LEVEL_REFRESH_MARGIN, LEVEL_SPAN, Located, MAX_DISTANCE_PX, Reference};
use tiles::{Cache, MAX_COMPUTE_LEVEL, TILE, TileKey, View};

const LAYERS: u32 = 1536;
const FALLBACK_LEVELS: u32 = 4;
const AUTOPILOT_PERIOD: u64 = 15;
const DEFAULT_PERTURB_FROM_LEVEL: u32 = 0;
// NOTE: pixel spacing at level 100 is 2^-106, far above f32's smallest normal (2^-126), so the 32-bit deltas keep their precision up to here.
const DEFAULT_F32_UNTIL_LEVEL: u32 = 100;
// NOTE: the bulb test sees the pixel as the reference centre rounded to f64 plus its delta, off by about 2^-53; at level 40 that is 1/100 of a pixel, deeper it would misclassify a visible strip along the cardioid.
const BULB_TEST_UNTIL_LEVEL: u32 = 40;
const FALLBACK_WALK: u32 = 12;
const MAX_FALLBACK_UP: u32 = 60;
const LEAD_SECONDS: f64 = 1.5;
const LEAVE_SECONDS: f64 = 0.5;
const SLOW_FRAME: f64 = 0.025;
const MIN_STEPS: u32 = 16;
// NOTE: the cost of a step varies tenfold between tiles (pixels that escape early leave their threads idle), so the adaptive budget can grow on cheap tiles and then overload a frame on dense ones. This cap bounds the worst dispatch to 12 × 65536 × 1024 thread-steps, about 30 ms on an RX 6700 XT and far below the GPU driver's hang timeout on an integrated Radeon 780M.
const MAX_STEPS: u32 = 1 << 10;
const MIN_WORK: f64 = (MIN_STEPS * TILE_PIXELS) as f64;

struct Pending {
    key: TileKey,
    slot: u32,
    since: u64,
    started: bool,
    params: TileParams,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    cache: Cache,
    view: View,
    start: Option<View>,
    app_id: String,
    vsync: bool,
    home_log2_upp: f64,
    frame: u64,
    last: Instant,
    work: f64,
    steps: u32,
    pending: Vec<Pending>,
    zoom_speed: f64,
    start_zoom_speed: Option<f64>,
    zoom_stop_log2_upp: Option<f64>,
    captures: Vec<(When, String)>,
    started: Instant,
    zoom_stopped: bool,
    slow_frames: u32,
    trace: bool,
    last_work: (usize, u32),
    worst_frame: f64,
    cursor: Option<(f64, f64)>,
    dragging: bool,
    autopilot: bool,
    target: Option<(TileKey, f64, f64)>,
    iter_mult: f64,
    samples: u32,
    text: String,
    perturb_from: u32,
    f32_until: u32,
    reference: Option<Reference>,
    incoming: Option<Receiver<Computed>>,
    free_slots: Vec<u32>,
}

impl App {
    fn new(launch: Launch) -> Self {
        App {
            window: None,
            gpu: None,
            cache: Cache::new(LAYERS),
            view: View::home(),
            start: launch.view,
            app_id: launch.app_id,
            vsync: launch.vsync,
            home_log2_upp: 0.0,
            frame: 0,
            last: Instant::now(),
            work: MIN_WORK,
            steps: MIN_STEPS,
            pending: Vec::new(),
            zoom_speed: 0.0,
            start_zoom_speed: launch.zoom_speed,
            zoom_stop_log2_upp: launch.zoom_to_log2_upp,
            captures: launch.captures,
            started: Instant::now(),
            zoom_stopped: false,
            slow_frames: 0,
            trace: launch.trace,
            last_work: (0, 0),
            worst_frame: 0.0,
            cursor: None,
            dragging: false,
            autopilot: false,
            target: None,
            iter_mult: 1.0,
            samples: 2,
            text: String::new(),
            perturb_from: launch.perturb_from,
            f32_until: launch.f32_until,
            reference: None,
            incoming: None,
            free_slots: (0..STATE_SLOTS).collect(),
        }
    }

    fn centre_tile_layer(&mut self, level: u32) -> Option<(TileKey, u32)> {
        let key = self.view.centre_key(level);
        let mut up = 0;
        loop {
            let k = key.ancestor(up);
            if let Some(layer) = self.cache.get(&k, self.frame) {
                return Some((k, layer));
            }
            if k.level == 0 {
                return None;
            }
            up += 1;
        }
    }

    fn size(&self) -> (u32, u32) {
        let g = self.gpu.as_ref().unwrap();
        (g.config.width, g.config.height)
    }

    fn max_iter(&self, level: u32) -> u32 {
        ((2000.0 + 400.0 * level as f64) * self.iter_mult).clamp(64.0, 200_000.0) as u32
    }

    fn go_home(&mut self) {
        let (w, h) = self.size();
        self.view = View::home();
        self.view.fit(w, h);
        self.home_log2_upp = self.view.log2_upp;
        if let Some(v) = self.start.take() {
            self.view = v;
        }
        self.zoom_speed = self.start_zoom_speed.take().unwrap_or(0.0);
        self.target = None;
    }

    fn step_motion(&mut self, dt: f64) {
        let (w, h) = self.size();
        let (mut fx, mut fy) = self.focus(w, h);
        if self.autopilot {
            if let Some((key, u, v)) = &self.target {
                let k = (3.0 * dt).min(1.0);
                let (dx, dy) = self.view.pixels_to(key, *u, *v);
                self.view.add_pixels(dx * k, dy * k);
            }
            (fx, fy) = (w as f64 / 2.0, h as f64 / 2.0);
        }
        if self.zoom_speed != 0.0 {
            let mut step = -self.zoom_speed * dt;
            let stop = self.zoom_stop_log2_upp.filter(|_| self.zoom_speed > 0.0);
            if let Some(stop) = stop {
                step = step.max(stop - self.view.log2_upp);
            }
            self.view.zoom_about(step.exp2(), fx, fy, w, h);
            if stop.is_some_and(|stop| self.view.log2_upp <= stop) {
                self.zoom_speed = 0.0;
                self.zoom_stop_log2_upp = None;
                self.zoom_stopped = true;
            }
        }
    }

    fn focus(&self, w: u32, h: u32) -> (f64, f64) {
        self.cursor.unwrap_or((w as f64 / 2.0, h as f64 / 2.0))
    }

    fn lead_levels(&self) -> u32 {
        if self.zoom_speed > 0.0 {
            (self.zoom_speed * LEAD_SECONDS).ceil() as u32
        } else {
            0
        }
    }

    fn update_autopilot_target(&mut self, found: TileKey, data: &[f32]) {
        let (fx, fy) = self.view.tile_frac(&found);
        let (px, py) = ((fx * TILE as f64) as i64, (fy * TILE as f64) as i64);
        let mut best = (f32::MIN, 0usize);
        for (i, v) in data.iter().enumerate() {
            let (x, y) = ((i % TILE as usize) as i64, (i / TILE as usize) as i64);
            let d2 = ((x - px) * (x - px) + (y - py) * (y - py)) as f32;
            // NOTE: prefer high escape counts (boundary detail) but discount points far from the current centre so the path stays continuous.
            let score = *v - d2.sqrt() * 0.02;
            if *v >= 0.0 && score > best.0 {
                best = (score, i);
            }
        }
        if best.0 > f32::MIN {
            let (x, y) = (
                (best.1 % TILE as usize) as f64,
                (best.1 / TILE as usize) as f64,
            );
            self.target = Some((found, (x + 0.5) / TILE as f64, (y + 0.5) / TILE as f64));
        }
    }

    fn schedule(&mut self, level: u32, focus: (f64, f64), w: u32, h: u32) -> Vec<TileParams> {
        self.finish_tiles();
        let root = TileKey::root();
        if !self.cache.contains(&root) && !self.free_slots.is_empty() {
            if let Some(located) = self.located(&root) {
                self.start(root.clone(), located, self.max_iter(0));
            }
        }
        self.cache.get(&root, self.frame);

        if !self.free_slots.is_empty() {
            let mut wanted: Vec<(f64, TileKey, Option<Located>)> = Vec::new();
            let first = level.saturating_sub(FALLBACK_LEVELS);
            let last = (level + self.lead_levels()).min(MAX_COMPUTE_LEVEL);
            // NOTE: zooming in scales every offset from the focus by 2^(speed * t); a tile whose nearest point is off screen after LEAVE_SECONDS is gone before it could land, so it is not worth computing.
            let spread = (self.zoom_speed.max(0.0) * LEAVE_SECONDS).exp2();
            let stays = |(x0, y0, x1, y1): (f64, f64, f64, f64)| {
                let (nx, ny) = (focus.0.clamp(x0, x1), focus.1.clamp(y0, y1));
                let (sx, sy) = (
                    focus.0 + (nx - focus.0) * spread,
                    focus.1 + (ny - focus.1) * spread,
                );
                (0.0..=w as f64).contains(&sx) && (0.0..=h as f64).contains(&sy)
            };
            for l in first..=last {
                // NOTE: look-ahead levels are wanted where the zoom will be once it reaches them, not where the screen is now.
                let mut view = self.view.clone();
                if l > level {
                    let factor = (-((l - level) as f64)).exp2();
                    view.zoom_about(factor, focus.0, focus.1, w, h);
                }
                for key in view.visible_tiles(l, w, h) {
                    if self.cache.contains(&key) {
                        self.cache.get(&key, self.frame);
                        continue;
                    }
                    let (x0, y0, x1, y1) = view.tile_rect(&key, w, h);
                    if !stays((x0, y0, x1, y1)) {
                        continue;
                    }
                    let (dx, dy) = ((x0 + x1) / 2.0 - focus.0, (y0 + y1) / 2.0 - focus.1);
                    let d = (dx * dx + dy * dy).sqrt();
                    let Some(located) = self.located(&key) else {
                        continue;
                    };
                    wanted.push((d + (l as f64 - level as f64) * 1e9, key, located));
                }
            }
            wanted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let free = self.free_slots.len();
            for (_, key, located) in wanted.into_iter().take(free) {
                let max_iter = self.max_iter(key.level.max(level));
                self.start(key, located, max_iter);
            }
        }

        // NOTE: the per-frame budget is kept in pixel-steps and every in-flight tile is costed at its full size: GPU threads run in groups that last as long as their slowest pixel, so a tile with a few scattered unfinished pixels costs nearly as much as a fresh one. Fresh tiles then lower the step count at once instead of overloading the frame that starts them.
        let running = self.pending.len() as f64 * TILE_PIXELS as f64;
        self.steps =
            (self.work / running.max(1.0)).clamp(MIN_STEPS as f64, MAX_STEPS as f64) as u32;
        let mut tiles = vec![TileParams::default(); STATE_SLOTS as usize];
        for p in &self.pending {
            tiles[p.slot as usize] = TileParams {
                first: !p.started as u32,
                steps: self.steps,
                ..p.params
            };
        }
        tiles
    }

    // NOTE: None means the tile needs perturbation but no reference covers it yet; Some(None) routes it to the direct kernel.
    fn located(&self, key: &TileKey) -> Option<Option<Located>> {
        if key.level < self.perturb_from {
            return Some(None);
        }
        self.reference.as_ref()?.locate(key).map(Some)
    }

    fn start(&mut self, key: TileKey, located: Option<Located>, max_iter: u32) {
        let Some(layer) = self.cache.alloc(key.clone(), self.frame) else {
            return;
        };
        let (origin, kernel, use_skip) = match located {
            Some(located) if key.level <= self.f32_until => {
                (located.offset, Kernel::Perturb32, located.skip)
            }
            Some(located) => (located.offset, Kernel::Perturb, located.skip),
            None => {
                let (ox, oy) = key.origin();
                ([ox, oy], Kernel::Direct, false)
            }
        };
        let params = TileParams {
            origin,
            centre: self.reference.as_ref().map_or([0.0; 2], |r| r.centre()),
            bulbs: (key.level <= BULB_TEST_UNTIL_LEVEL) as u32,
            step: key.step(),
            layer,
            max_iter,
            samples: self.samples,
            ref_len: self.reference.as_ref().map_or(0, |r| r.len),
            skip_p: self.reference.as_ref().map_or(0, |r| r.skip_p),
            use_skip: use_skip as u32,
            kernel: kernel as u32,
            ..Default::default()
        };
        self.start_tile(key, params);
    }

    fn start_tile(&mut self, key: TileKey, params: TileParams) {
        let slot = self.free_slots.pop().unwrap();
        let gpu = self.gpu.as_ref().unwrap();
        gpu.reset_done(slot);
        self.pending.push(Pending {
            key,
            slot,
            since: gpu.submitted() + 1,
            started: false,
            params,
        });
    }

    // NOTE: a tile is done when the GPU counted every one of its pixels finished; the counts are read back asynchronously, so a tile is seen done a few frames after its last pixel, and counts read before the slot was claimed (since) are ignored.
    fn finish_tiles(&mut self) {
        let Some((at, counts)) = self.gpu.as_mut().unwrap().poll_done() else {
            return;
        };
        let frame = self.frame;
        let cache = &mut self.cache;
        let free_slots = &mut self.free_slots;
        self.pending.retain(|p| {
            let done = p.since <= at && counts[p.slot as usize] == TILE_PIXELS;
            if done {
                cache.mark_ready(p.params.layer, frame);
                free_slots.push(p.slot);
            }
            !done
        });
    }

    fn render_frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f64().min(0.1);
        self.last = now;
        self.frame += 1;
        let (w, h) = self.size();

        self.step_motion(dt);
        if dt > SLOW_FRAME {
            self.slow_frames += 1;
            if self.trace {
                let (tiles, steps) = self.last_work;
                println!(
                    "slow frame {:.0}ms at {:.2}s: previous frame advanced {tiles} tiles by {steps} steps, level {}",
                    dt * 1000.0,
                    self.started.elapsed().as_secs_f64(),
                    self.view.target_level()
                );
            }
        }
        self.worst_frame = self.worst_frame.max(dt);
        // NOTE: dt measures the previous frame. Only a frame that dispatched tile work says anything about the budget; idle frames are always fast and would otherwise grow it without bound until the next tiles start with a dispatch long enough to trip the driver's GPU timeout. The frame time includes the vsync wait, so overload shows only past one refresh interval.
        if self.last_work.0 > 0 {
            if dt > 0.018 {
                self.work = (self.work / 2.0).max(MIN_WORK);
            } else if dt < 0.0169 {
                self.work *= 1.0625;
            }
        }

        let level = self.view.target_level().min(MAX_COMPUTE_LEVEL);
        self.update_reference((level + self.lead_levels()).min(MAX_COMPUTE_LEVEL));
        if self.autopilot && self.frame % AUTOPILOT_PERIOD == 0 {
            if let Some((key, layer)) = self.centre_tile_layer(level) {
                let data = self.gpu.as_ref().unwrap().read_layer(layer);
                self.update_autopilot_target(key, &data);
            }
        }

        let focus = if self.autopilot {
            (w as f64 / 2.0, h as f64 / 2.0)
        } else {
            self.focus(w, h)
        };
        let tiles = self.schedule(level, focus, w, h);
        self.last_work = (self.pending.len(), self.steps);

        let mut instances: Vec<Instance> = Vec::new();
        let first_up = level.saturating_sub(self.cache.max_ready_level());
        let last_up = (first_up + FALLBACK_WALK).min(level).min(MAX_FALLBACK_UP);
        for key in self.view.visible_tiles(level, w, h) {
            let frame = self.frame;
            let cache = &mut self.cache;
            let found = (first_up..=last_up)
                .find_map(|up| cache.get(&key.ancestor(up), frame).map(|layer| (layer, up)))
                .or_else(|| {
                    cache
                        .get(&key.ancestor(key.level), frame)
                        .map(|layer| (layer, key.level))
                });
            let Some((layer, up)) = found else {
                continue;
            };
            let (x0, y0, x1, y1) = self.view.tile_rect(&key, w, h);
            instances.push(Instance {
                rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32],
                uv: key.uv_in_ancestor(up),
                layer,
                _pad: [0; 3],
            });
            if instances.len() >= MAX_INSTANCES {
                break;
            }
        }

        let mag = self.view.depth_log10(self.home_log2_upp);
        let (wx, wy) = self.view.world();
        let text = format!(
            "depth 10^{:.1}  level {}  iter {}  ss {}\nx {:.17}  y {:.17}  upp {}",
            mag,
            level,
            self.max_iter(level),
            self.samples,
            wx,
            wy,
            self.view.upp_string(3)
        );
        let text_changed = text != self.text;
        self.text = text;

        let globals = Globals {
            screen: [w as f32, h as f32],
            color_scale: 1.5,
            color_offset: 0.0,
            text_rect: [8.0, 8.0, 8.0 + gpu::TEXT_W as f32, 8.0 + gpu::TEXT_H as f32],
            color_lin: 0.0,
            stripe: 0.2,
            _pad: [0.0; 2],
        };
        let elapsed = self.started.elapsed().as_secs_f64();
        let settled = self.is_settled();
        let due = self.captures.iter().position(|(when, _)| match when {
            When::At(at) => elapsed >= *at,
            When::ZoomStop => self.zoom_stopped,
            When::Settled => settled,
        });
        let capture = due.map(|i| self.captures.remove(i).1);
        let gpu = self.gpu.as_mut().unwrap();
        if text_changed {
            gpu.write_text(&text::rasterize(
                &self.text,
                gpu::TEXT_W as usize,
                gpu::TEXT_H as usize,
            ));
        }
        if gpu.frame(&tiles, &instances, globals, capture.as_deref()) {
            for p in &mut self.pending {
                p.started = true;
            }
        }
        if let Some(path) = capture {
            println!(
                "capture {path}: {elapsed:.2}s, level {level}, slow frames {} of {}, worst {:.0}ms",
                self.slow_frames,
                self.frame,
                self.worst_frame * 1000.0
            );
        }
    }

    fn update_reference(&mut self, level: u32) {
        let arrived = self.incoming.as_ref().map(|rx| rx.try_recv());
        match arrived {
            Some(Ok(computed)) => {
                self.incoming = None;
                self.install_reference(computed);
            }
            Some(Err(TryRecvError::Disconnected)) => self.incoming = None,
            Some(Err(TryRecvError::Empty)) | None => {}
        }
        if self.incoming.is_none() && level >= self.perturb_from && self.reference_is_stale(level) {
            let (cx, cy) = self.view.centre_world(reference::bits_for(level));
            let max_iter = self.max_iter(level + LEVEL_SPAN);
            self.incoming = Some(reference::spawn(cx, cy, level, max_iter));
        }
    }

    fn install_reference(&mut self, computed: Computed) {
        let Computed {
            reference,
            orbit,
            skip,
        } = computed;
        self.gpu
            .as_mut()
            .unwrap()
            .upload_reference(&orbit, &skip.entries);
        let cache = &mut self.cache;
        let free_slots = &mut self.free_slots;
        self.pending.retain(|p| {
            let direct = p.params.kernel == Kernel::Direct as u32;
            if !direct {
                cache.cancel(&p.key);
                free_slots.push(p.slot);
            }
            direct
        });
        self.reference = Some(reference);
    }

    // NOTE: schedule() refills the pending list whenever a visible tile is missing, so an empty list after it ran means nothing is left to compute, unless a reference orbit is still on its way.
    fn is_settled(&self) -> bool {
        self.zoom_speed == 0.0
            && self.start_zoom_speed.is_none()
            && self.pending.is_empty()
            && self.incoming.is_none()
    }

    fn reference_is_stale(&self, level: u32) -> bool {
        match &self.reference {
            None => true,
            Some(r) => {
                level + LEVEL_REFRESH_MARGIN > r.level + LEVEL_SPAN
                    || r.distance_px(&self.view) > MAX_DISTANCE_PX / 2.0
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("sukeldu")
            .with_name(self.app_id.as_str(), "")
            .with_inner_size(LogicalSize::new(1280, 800));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        self.gpu = Some(Gpu::new(window.clone(), LAYERS, self.vsync));
        self.window = Some(window);
        self.go_home();
        self.last = Instant::now();
        self.started = self.last;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render_frame(),
            WindowEvent::CursorMoved { position, .. } => {
                if let (true, Some((ox, oy))) = (self.dragging, self.cursor) {
                    self.view.add_pixels(ox - position.x, oy - position.y);
                }
                self.cursor = Some((position.x, position.y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                self.zoom_speed = (self.zoom_speed + lines * 0.25).clamp(-8.0, 8.0);
            }
            WindowEvent::MouseInput { state, button, .. } => match button {
                MouseButton::Right if state == ElementState::Pressed => self.zoom_speed = 0.0,
                MouseButton::Left => self.dragging = state == ElementState::Pressed,
                _ => {}
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if !event.state.is_pressed() || event.repeat {
                    return;
                }
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => event_loop.exit(),
                    PhysicalKey::Code(KeyCode::KeyA) => {
                        self.autopilot = !self.autopilot;
                        self.target = None;
                    }
                    PhysicalKey::Code(KeyCode::KeyH) => self.go_home(),
                    PhysicalKey::Code(KeyCode::KeyS) => self.samples = self.samples % 3 + 1,
                    PhysicalKey::Code(KeyCode::KeyC) => {
                        let (x, y) = self.view.world_strings();
                        println!("--at {} {} {}", x, y, self.view.upp_string(12));
                    }
                    PhysicalKey::Code(KeyCode::KeyF) => {
                        let win = self.window.as_ref().unwrap();
                        let fs = if win.fullscreen().is_some() {
                            None
                        } else {
                            Some(Fullscreen::Borderless(None))
                        };
                        win.set_fullscreen(fs);
                    }
                    PhysicalKey::Code(KeyCode::BracketRight) => {
                        self.iter_mult = (self.iter_mult * 1.5).min(64.0)
                    }
                    PhysicalKey::Code(KeyCode::BracketLeft) => {
                        self.iter_mult = (self.iter_mult / 1.5).max(0.25)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.gpu = None;
        self.window = None;
    }
}

enum When {
    At(f64),
    ZoomStop,
    Settled,
}

struct Launch {
    view: Option<View>,
    perturb_from: u32,
    f32_until: u32,
    zoom_speed: Option<f64>,
    zoom_to_log2_upp: Option<f64>,
    captures: Vec<(When, String)>,
    app_id: String,
    vsync: bool,
    trace: bool,
}

fn parse_launch() -> Launch {
    let args: Vec<String> = std::env::args().collect();
    let after = |flag: &str| {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1)
    };
    let parse = |flag: &str| after(flag)?.parse::<f64>().ok();
    let view = args
        .iter()
        .position(|a| a == "--at")
        .and_then(|i| View::at(args.get(i + 1)?, args.get(i + 2)?, args.get(i + 3)?));
    let captures = args
        .iter()
        .position(|a| a == "--capture")
        .and_then(|i| {
            let prefix = args.get(i + 2)?;
            let time = |t: &str| match t {
                "stop" => Some(When::ZoomStop),
                "settled" => Some(When::Settled),
                _ => t.parse().ok().map(When::At),
            };
            Some(
                args.get(i + 1)?
                    .split(',')
                    .filter_map(|t| Some((time(t)?, format!("{prefix}_{t}.ppm"))))
                    .collect(),
            )
        })
        .unwrap_or_default();
    Launch {
        view,
        perturb_from: after("--perturb-from")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_PERTURB_FROM_LEVEL),
        f32_until: after("--f32-until")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_F32_UNTIL_LEVEL),
        zoom_speed: parse("--zoom-speed"),
        zoom_to_log2_upp: parse("--zoom-to").map(f64::log2),
        captures,
        app_id: after("--app-id").map_or("sukeldu".to_string(), String::clone),
        vsync: !args.iter().any(|a| a == "--no-vsync"),
        trace: args.iter().any(|a| a == "--trace"),
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(parse_launch());
    event_loop.run_app(&mut app).expect("run");
}
