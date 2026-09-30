# sukeldu

A Mandelbrot explorer for a six-year-old: continuous, smooth, deep zoom on an
AMD GPU under Linux. Rust + wgpu (Vulkan/RADV) + winit.

## What it must do

- **Smooth, continuous motion.** The picture never stutters or reflows while
  zooming. Computation may lag behind; the display must not. Already computed
  detail stays on screen and only gets sharper, never blurrier or replaced by
  a different-looking approximation.
- **Stable textures.** A region that has been computed at a given zoom level
  always looks the same when revisited. No shimmering, no per-frame
  re-rendering of the same area.
- **Very fast rendering.** All per-pixel iteration runs on the GPU
  (compute shaders, fp64 where the depth requires it). The CPU only schedules
  work and prepares reference data.
- **Deep.** Plain double precision runs out near 1e-15 units per pixel; beyond
  that the renderer must switch to perturbation (one arbitrary-precision
  reference orbit on the CPU, per-pixel deltas on the GPU) to reach 1e-100 and
  further. Depth is a target, not a nice-to-have.
- **Simple controls, no settings screens.**
  - Mouse scroll sets the zoom speed: scroll up = zoom in faster, scroll down =
    slower, past zero = zoom out. The zoom keeps running by itself.
  - Right click stops the zoom (speed to zero).
  - The zoom is centred on the mouse position; moving the mouse steers.
  - Left-drag pans.
  - `A` toggles the autopilot: the zoom centre drifts toward the most detailed
    boundary structure near the centre (later: toward the next minibrot).
  - `F` toggles fullscreen, `H` goes home, `Esc` quits.
  - `S` toggles 2×2 supersampling (tiles one level finer than the screen
    needs; four times the compute). `[` / `]` lower / raise the iteration
    budget.
- **Depth readout.** Top-left corner, small text: current magnification
  (as 10^N), tile level and iteration count, with the centre coordinates and
  units-per-pixel on a second line. Nothing else on screen.
- **Reproducible locations.** `C` prints the current location as
  `--at CX CY UPP` to stdout; `sukeldu --at CX CY UPP` starts there.
  `--perturb-from N` switches tiles at level N and deeper to perturbation
  (default 36; 0 renders everything but the root tile that way, for
  comparing the two paths at one location).

## Architecture

- **Tile cache.** The plane is cut into a quadtree of 256×256 tiles. Level L
  tiles cover 4/2^L world units. Each computed tile lives in one layer of a
  2D texture array (R32Float, smooth iteration count, negative = inside).
  Tiles are only evicted when they were not drawn recently; level 0 is never
  evicted.
- **Drawing.** Every frame draws the visible tiles of the target level. A tile
  that is not computed yet is drawn from its nearest computed ancestor (a
  sub-rectangle of the coarser tile), so the screen is always fully covered
  and refinement only ever adds sharpness.
- **Scheduling.** Missing tiles are computed coarse-to-fine, nearest to the
  zoom focus first. Each tile is computed in 32-row slices spread over
  frames, under a per-frame slice budget that adapts to the frame time so the
  frame rate stays at the display refresh even when one tile costs more than a
  frame. A tile is drawn only once all its slices are done.
- **Shaders.** `src/shaders/compute.wgsl` iterates one tile directly in fp64
  (shallow levels). `src/shaders/perturb.wgsl` iterates one tile as deltas
  against a reference orbit, with rebasing, so one reference is valid for
  every pixel. `src/shaders/render.wgsl` maps iteration counts to colours and
  draws the tile quads plus the text overlay.
- **Reference orbits.** `src/reference.rs` computes one arbitrary-precision
  orbit (MPFR) on a worker thread, stored as f64 pairs and uploaded to a GPU
  buffer. A reference serves tiles within 2^30 tile-pixels of it and up to 32
  levels deeper than the level it was made for; a replacement is requested
  before either limit is reached. Tiles that no reference serves are not
  scheduled, so their ancestors stay on screen. Installing a new reference
  cancels perturbation tiles still in flight; finished tiles are kept.

## Toolchain

Rust is installed through mise, scoped to this directory (`.mise.toml`,
`rust = "stable"`); nothing Rust-related is installed system-wide. Build and
run with `mise exec -- cargo run --release` (or plain `cargo run --release`
inside the directory when mise is activated in the shell). Requires a Vulkan
driver (`vulkan-radeon`), already present on this machine.

## Status

Initial version: fp64 on the GPU, tile cache, continuous zoom, autopilot
toward boundary detail, depth readout, 2×2 supersampling in the compute
shader, fixed palette (hue from log2 of the smooth count, 20% brightness
banding on the raw count). The view centre is fixed-point arbitrary precision
(`rug`) and tile keys are big integers, so the zoom itself has no depth limit.
Tiles from level 36 on are computed by perturbation with f64 deltas, up to
level 900 (about 1e-273 units per pixel); deeper views magnify the level-900
tiles. Perturbation has no interior shortcut yet, so interior pixels run to
the iteration limit, and a single dispatch can still exceed a frame at very
high iteration counts.

## Open work, in order

1. **Restrict wgpu to the Vulkan backend.** Every recorded crash
   (`coredumpctl list sukeldu`) is in wgpu's OpenGL/EGL backend on Wayland,
   at `create_surface` or at exit in `dlclose`; rendering itself runs on
   Vulkan. Pass `backends: wgpu::Backends::VULKAN` in the `InstanceDescriptor`.
2. **Quality standard before further tuning.** Fixed reference locations,
   a frame-rate target while zooming, and a reference screenshot per location,
   so every change is compared instead of judged by feel. Known references:
   - minibrot in dust: `--at -0.7394310682635371 -0.1268495465745192 4.263e-13`
   - spiral valley: `--at -0.7436438870371587 0.1318259042053119 2.0e-12`
   - a flat, bland region (pink screenshot at depth 10^12.7, level 45) and a
     jagged black-filament region (depth 10^5.4, level 21) still need their
     `--at` lines recorded (press `C` there).
3. **Perturbation speed and range.** Exponent-extended deltas past level 900
   (removes the last depth limit), bilinear approximation to skip iterations
   (makes deep and interior tiles cheap, which is what pays for supersampling
   and higher iteration counts), per-pixel state carried across dispatches so
   one slice never exceeds a frame, and an f32 inner loop where it matches the
   f64 images.
4. **Iteration budget.** The per-level formula
   (`150 + 100 × 1.12^level`) is a guess; near minibrots it leaves black
   areas that should have detail. Needs an adaptive rule (e.g. raise while a
   significant share of a tile's samples hit the cap).
5. **Supersampling cost.** 2×2 quadrupled tile cost and made refinement lag
   visibly before perturbation existed. Revisit the default (`S` cycles
   1/2/3) once tiles are cheap; consider averaging colours instead of counts.
6. **Palette.** Content-adaptive normalisation was tried and rejected: colours
   drifted while zooming, which breaks texture stability. Any palette must be
   a fixed function of the count. Dust regions (counts varying by thousands
   between neighbours) stay noisy under every fixed map; only more samples or
   distance-estimate shading smooths them. Distance estimation (store
   |z|·log|z|/|dz| as a second channel, shade filaments) is the candidate.
7. **Autopilot toward minibrots.** Currently steers to the highest escape
   count near the centre. Newton's method for the nearest minibrot nucleus
   (period detection via the reference orbit) is the intended replacement.
