mod gpu;
mod text;
mod tiles;

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use gpu::{Globals, Gpu, Instance, Job, MAX_DISPATCH, MAX_INSTANCES, SLICE_ROWS};
use tiles::{Cache, TILE, TileKey, View};

const LAYERS: u32 = 1536;
const FALLBACK_LEVELS: u32 = 4;
const AUTOPILOT_PERIOD: u64 = 15;

struct Pending {
    layer: u32,
    key: TileKey,
    max_iter: u32,
    next_row: u32,
    samples: u32,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    cache: Cache,
    view: View,
    start: Option<View>,
    home_upp: f64,
    frame: u64,
    last: Instant,
    budget: usize,
    pending: Vec<Pending>,
    zoom_speed: f64,
    cursor: (f64, f64),
    dragging: bool,
    autopilot: bool,
    target: Option<(f64, f64)>,
    iter_mult: f64,
    samples: u32,
    text: String,
}

impl App {
    fn new(start: Option<View>) -> Self {
        App {
            window: None,
            gpu: None,
            cache: Cache::new(LAYERS),
            view: View::home(),
            start,
            home_upp: 1.0,
            frame: 0,
            last: Instant::now(),
            budget: 16,
            pending: Vec::new(),
            zoom_speed: 0.0,
            cursor: (0.0, 0.0),
            dragging: false,
            autopilot: false,
            target: None,
            iter_mult: 1.0,
            samples: 2,
            text: String::new(),
        }
    }

    fn centre_tile_layer(&mut self, level: u32) -> Option<(TileKey, u32)> {
        let (w, h) = self.size();
        let (cx, cy) = self
            .view
            .screen_to_world(w as f64 / 2.0, h as f64 / 2.0, w, h);
        let s = TileKey::world_size(level);
        let key = TileKey {
            level,
            ix: ((cx + 2.0) / s).floor() as i64,
            iy: ((cy + 2.0) / s).floor() as i64,
        };
        let mut up = 0;
        loop {
            let k = key.ancestor(up);
            if let Some(layer) = self.cache.get(k, self.frame) {
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
        ((150.0 + 100.0 * 1.12f64.powi(level as i32)) * self.iter_mult).clamp(64.0, 200_000.0)
            as u32
    }

    fn go_home(&mut self) {
        let (w, h) = self.size();
        self.view = View::home();
        self.view.fit(w, h);
        self.home_upp = self.view.upp;
        if let Some(v) = self.start.take() {
            self.view = v;
        }
        self.zoom_speed = 0.0;
        self.target = None;
    }

    fn step_motion(&mut self, dt: f64) {
        let (w, h) = self.size();
        let (mut fx, mut fy) = self.cursor;
        if self.autopilot {
            if let Some((tx, ty)) = self.target {
                let k = (3.0 * dt).min(1.0);
                self.view.cx += (tx - self.view.cx) * k;
                self.view.cy += (ty - self.view.cy) * k;
            }
            fx = w as f64 / 2.0;
            fy = h as f64 / 2.0;
        }
        if self.zoom_speed != 0.0 {
            let factor = 2f64.powf(-self.zoom_speed * dt);
            self.view.zoom_about(factor, fx, fy, w, h);
        }
    }

    fn update_autopilot_target(&mut self, found: TileKey, data: &[f32]) {
        let (w, h) = self.size();
        let (cx, cy) = self
            .view
            .screen_to_world(w as f64 / 2.0, h as f64 / 2.0, w, h);
        let (ox, oy) = found.origin();
        let step = found.step();
        let (px, py) = (((cx - ox) / step) as i64, ((cy - oy) / step) as i64);
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
            self.target = Some((ox + (x + 0.5) * step, oy + (y + 0.5) * step));
        }
    }

    fn schedule(&mut self, level: u32, focus: (f64, f64), w: u32, h: u32) -> Vec<Job> {
        let (focus_x, focus_y) = self.view.screen_to_world(focus.0, focus.1, w, h);
        let root = TileKey {
            level: 0,
            ix: 0,
            iy: 0,
        };
        if !self.cache.contains(root) {
            let layer = self.cache.alloc(root, self.frame).unwrap();
            self.pending.push(Pending {
                layer,
                key: root,
                max_iter: self.max_iter(0),
                next_row: 0,
                samples: self.samples,
            });
        }
        self.cache.get(root, self.frame);

        // NOTE: tiles are computed in row slices across frames; keep a few tiles in flight so the slice budget is always usable.
        if self.pending.len() < 4 {
            let mut wanted: Vec<(f64, TileKey)> = Vec::new();
            let first = level.saturating_sub(FALLBACK_LEVELS);
            for l in first..=level {
                for key in self.view.visible_tiles(l, w, h) {
                    if self.cache.contains(key) {
                        self.cache.get(key, self.frame);
                        continue;
                    }
                    let (ox, oy) = key.origin();
                    let half = TileKey::world_size(l) / 2.0;
                    let d = ((ox + half - focus_x).powi(2) + (oy + half - focus_y).powi(2)).sqrt()
                        / self.view.upp;
                    wanted.push((d + (level - l) as f64 * -1e9, key));
                }
            }
            wanted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for (_, key) in wanted.into_iter().take(8) {
                if let Some(layer) = self.cache.alloc(key, self.frame) {
                    self.pending.push(Pending {
                        layer,
                        key,
                        max_iter: self.max_iter(key.level),
                        next_row: 0,
                        samples: self.samples,
                    });
                }
            }
        }

        let mut jobs = Vec::new();
        let mut slices = self.budget.min(MAX_DISPATCH);
        for p in self.pending.iter_mut() {
            while slices > 0 && p.next_row < TILE {
                jobs.push(Job {
                    layer: p.layer,
                    key: p.key,
                    max_iter: p.max_iter,
                    row0: p.next_row,
                    samples: p.samples,
                });
                p.next_row += SLICE_ROWS;
                slices -= 1;
            }
            if slices == 0 {
                break;
            }
        }
        let frame = self.frame;
        let cache = &mut self.cache;
        self.pending.retain(|p| {
            if p.next_row >= TILE {
                cache.mark_ready(p.layer, frame);
                false
            } else {
                true
            }
        });
        jobs
    }

    fn render_frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f64().min(0.1);
        self.last = now;
        self.frame += 1;
        let (w, h) = self.size();

        self.step_motion(dt);

        let level = self.view.target_level();
        if self.autopilot && self.frame % AUTOPILOT_PERIOD == 0 {
            if let Some((key, layer)) = self.centre_tile_layer(level) {
                let data = self.gpu.as_ref().unwrap().read_layer(layer);
                self.update_autopilot_target(key, &data);
            }
        }

        let focus = if self.autopilot {
            (w as f64 / 2.0, h as f64 / 2.0)
        } else {
            self.cursor
        };
        let jobs = self.schedule(level, focus, w, h);

        let mut instances: Vec<Instance> = Vec::new();
        for key in self.view.visible_tiles(level, w, h) {
            let mut up = 0;
            let layer = loop {
                let k = key.ancestor(up);
                if let Some(layer) = self.cache.get(k, self.frame) {
                    break layer;
                }
                if k.level == 0 {
                    break 0;
                }
                up += 1;
            };
            let (ox, oy) = key.origin();
            let s = TileKey::world_size(level);
            let (x0, y0) = self.view.world_to_screen(ox, oy, w, h);
            let (x1, y1) = self.view.world_to_screen(ox + s, oy + s, w, h);
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

        let mag = (self.home_upp / self.view.upp).log10();
        let text = format!(
            "depth 10^{:.1}  level {}  iter {}  ss {}\nx {:.17}  y {:.17}  upp {:.3e}",
            mag,
            level,
            self.max_iter(level),
            self.samples,
            self.view.cx,
            self.view.cy,
            self.view.upp
        );
        let text_changed = text != self.text;
        self.text = text;

        // NOTE: the frame time includes the vsync wait, so it only signals overload once the budget pushes past one refresh interval.
        if dt > 0.018 {
            self.budget = (self.budget / 2).max(1);
        } else if dt < 0.0169 {
            self.budget = (self.budget + 2).min(MAX_DISPATCH);
        }

        let globals = Globals {
            screen: [w as f32, h as f32],
            color_scale: 1.5,
            color_offset: 0.0,
            text_rect: [8.0, 8.0, 8.0 + gpu::TEXT_W as f32, 8.0 + gpu::TEXT_H as f32],
            color_lin: 0.0,
            stripe: 0.2,
            _pad: [0.0; 2],
        };
        let gpu = self.gpu.as_mut().unwrap();
        if text_changed {
            gpu.write_text(&text::rasterize(
                &self.text,
                gpu::TEXT_W as usize,
                gpu::TEXT_H as usize,
            ));
        }
        gpu.frame(&jobs, &instances, globals);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("sukeldu")
            .with_inner_size(LogicalSize::new(1280, 800));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        self.gpu = Some(Gpu::new(window.clone(), LAYERS));
        self.window = Some(window);
        self.go_home();
        self.last = Instant::now();
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
                if self.dragging {
                    let dx = position.x - self.cursor.0;
                    let dy = position.y - self.cursor.1;
                    self.view.cx -= dx * self.view.upp;
                    self.view.cy -= dy * self.view.upp;
                }
                self.cursor = (position.x, position.y);
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
                    PhysicalKey::Code(KeyCode::KeyC) => println!(
                        "--at {:.17} {:.17} {:.6e}",
                        self.view.cx, self.view.cy, self.view.upp
                    ),
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

fn parse_args() -> Option<View> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--at")?;
    let cx = args.get(i + 1)?.parse().ok()?;
    let cy = args.get(i + 2)?.parse().ok()?;
    let upp = args.get(i + 3)?.parse().ok()?;
    Some(View { cx, cy, upp })
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(parse_args());
    event_loop.run_app(&mut app).expect("run");
}
