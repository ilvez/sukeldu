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
  `--perturb-from N` computes tiles shallower than level N with the direct
  fp64 shader (default 0: every tile by perturbation), as an exact reference
  for comparing pictures at one location. `--zoom-speed S` starts zooming
  in at S levels per second, `--zoom-to UPP` stops that zoom at UPP, and
  `--capture T1,T2 PREFIX` writes the frame at those seconds after start
  (or on the frame the zoom stops, for `stop`, or on the first frame with no
  tile work left, for `settled`) to `PREFIX_T.ppm`; together they make a
  zoom run reproducible for `tools/preview_diff.py`. Test runs also use
  `--app-id ID` (Wayland app id, for a compositor rule), `--no-vsync`
  (hidden windows are throttled by the compositor) and `--trace` (one log
  line per frame slower than 25 ms, with the work dispatched before it).

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
  zoom focus first. While zooming in, tiles of the levels the zoom reaches
  within the next 1.5 s are wanted too (where the view will be then), after
  the current ones, and tiles whose nearest point leaves the screen within
  half a second of zoom are skipped. Every tile is computed with the iteration cap of the view's
  level (`2000 + 400 × level`, at most 200k), so magnified fallbacks show
  the same black areas as the final tile. Up to 12 tiles are in flight, one
  state slot each. Every frame, one dispatch per shader advances every
  in-flight tile at once (256×256 pixels × 12 slots) by the same number of
  iterations per pixel, and each pixel saves its state in its slot of a GPU
  buffer, so a tile spreads over as many frames as it needs. The per-frame
  budget is kept in pixel-iterations and adapts to the frame time of frames
  that computed tiles; the step count is that budget divided by the pixels
  of all in-flight tiles, at most 1024, so newly started tiles do not
  overload a frame and no dispatch can run long enough to trip the GPU
  driver's hang timeout. Each finished pixel increments its
  slot's counter on the GPU; the counters are read back asynchronously, and
  a tile is drawn once its counter reaches 65536.
- **Shaders.** `src/shaders/perturb.wgsl` iterates as deltas
  against a reference orbit, with rebasing, so one reference is valid for
  every pixel, and with bilinear skipping: `src/skip.rs` builds, next to each
  orbit, a table where an entry at level k replaces 2^k steps by one linear
  step, valid while the delta is below the entry's radius. A tile may use it
  only if its farthest pixel is within 2^16 pixels of the reference.
  Interior pixels end early once |dz_n/dz_1|, carried along the orbit, falls
  below 1e-6 (the orbit is drawn into an attracting cycle).
  `src/shaders/perturb32.wgsl` is the same in f32 (deltas, orbit and skip
  table narrowed on upload), used for every tile up to level 100
  (`--f32-until N` changes it, 0 turns it off); deeper tiles use the f64
  shader. Up to level 40 it also skips samples inside the main cardioid and
  the period-2 bulb.
  `src/shaders/compute.wgsl` iterates directly in fp64, with the same bulb
  test and a periodicity check; it only runs below `--perturb-from N`.
  `src/shaders/render.wgsl` maps iteration counts to colours and
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
Every tile is computed by perturbation, with f32 deltas up to level 100 and
f64 deltas up to level 900 (about 1e-273 units per pixel); deeper views
magnify the level-900 tiles.

## Experiments

Ideas for keeping the preview during a zoom close to the final picture are
tried one at a time and scored with `tools/preview_diff.py`; the log of what
was tried, kept and rejected is `EXPERIMENTS.md`.

## Open work, in order

1. **Restrict wgpu to the Vulkan backend.** Every recorded crash
   (`coredumpctl list sukeldu`) is in wgpu's OpenGL/EGL backend on Wayland,
   at `create_surface` or at exit in `dlclose`; rendering itself runs on
   Vulkan. Pass `backends: wgpu::Backends::VULKAN` in the `InstanceDescriptor`.
2. **Quality standard before further tuning.** `tools/preview_diff.py` and
   `EXPERIMENTS.md` cover the preview-versus-settled comparison at fixed
   locations; a frame-rate target while zooming is still missing (capture
   logs show 30 to 100 ms frames). Known references:
   - minibrot in dust: `--at -0.7394310682635371 -0.1268495465745192 4.263e-13`
   - spiral valley: `--at -0.7436438870371587 0.1318259042053119 2.0e-12`
   - a flat, bland region (pink screenshot at depth 10^12.7, level 45) and a
     jagged black-filament region (depth 10^5.4, level 21) still need their
     `--at` lines recorded (press `C` there).
3. **Perturbation speed and range.** Exponent-extended deltas past level 900
   (removes the last depth limit), tuning of the dispatch size (`STEP_CAP`) and
   of the skip accuracy (`EPS` in
   `src/skip.rs`) against reference screenshots, and an f32 inner loop where it
   matches the f64 images. Cheap deep and interior tiles are what pays for
   supersampling and higher iteration counts.
4. **Iteration budget.** The per-level formula
   (`2000 + 400 × level`, capped at 200k) is a guess; near minibrots it may
   leave black areas that should have detail. Needs an adaptive rule (e.g.
   raise while a significant share of a tile's samples hit the cap).
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
