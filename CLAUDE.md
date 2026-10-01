# sukeldu

What the app is and how it works: `README.md`. What has been tried, kept and
rejected, with scores: `EXPERIMENTS.md`. Every rendering or scheduling change
is measured with the setup below and gets a row in `EXPERIMENTS.md`, whether
it is kept or reverted.

## Testing

### Prerequisites

- Hyprland (`hyprctl`), Python with numpy and Pillow.
- `tools/preview_diff.py` runs `target/release/sukeldu` and does not build
  it: run `mise exec -- cargo build --release` first.
- `mise exec -- cargo test --release` covers the CPU mirror of the
  perturbation algorithm and the WGSL layouts (shaders parse and validate,
  `Tile`/`State`/`Skip` structs match the Rust side).

### Running

Zoom score (the main one):

    python tools/preview_diff.py --tag NAME --zoom 2 --levels 8 [-- APP_FLAGS...]

Static settled pictures, for checking that a change leaves the final image
alone:

    python tools/preview_diff.py --tag NAME [--compare OTHER_TAG] [-- APP_FLAGS...]

- `--only PREFIX` limits the run to locations whose name starts with it
  (`spiral` matches `spiral` and `spiral+8`).
- `--timeout S` (default 120) bounds the wait for the settled picture; deep
  minibrot views take 15 to 60 s.
- `--compare OTHER_TAG` prints, per location, how this run's settled
  picture differs from run OTHER_TAG's. A change that should not alter the
  image must show `identical 1.0000`.
- App flags go after `--`, written literally. A shell variable holding
  several flags reaches the app as one argument, which it ignores silently.
- Useful app flags for A/B runs: `--perturb-from N`, `--f32-until N`,
  `--trace`.

### What a run does

- Each location launches the app with `--app-id sukeldu-test --no-vsync`.
  The tool registers a runtime Hyprland window rule that puts that class on
  workspace `test:a` without focus, so the desktop stays usable during
  runs. The rule disappears on a Hyprland config reload; the tool re-adds
  it on every run.
- Vsync is off because the compositor throttles windows on hidden
  workspaces; frame times then measure the app's own work.
- The app writes its own frames (`--capture`), as binary PPM, to
  `~/temp/sukeldu/TAG_LOCATION_{stop,2,settled}.ppm`, with a log per
  location in `~/temp/sukeldu/TAG_LOCATION.log`. `settled` is the first
  frame with no tile work left; `stop` is the frame on which a `--zoom` run
  arrives.

### Reading the output

- Columns: black fraction of preview and settled frame, mean difference
  between them (0 to 255) and the share of pixels differing by more than 32,
  slow frames (over 25 ms) up to the preview capture, worst frame, time to
  settle.
- Single runs on a shared desktop: differences of about ten points in the
  mean difference are noise. Settle times repeat within a few percent.
- The frame that writes a capture is itself slow (about 100 ms, it waits
  for the GPU and writes the file); slow-frame counts stop before it.
- With `--trace`, each slow frame logs the work dispatched before it:
  `slow frame 39ms at 4.04s: previous frame advanced 12 tiles by 1024 steps,
  level 41`.

### Rules

- One run at a time: runs share the GPU, so parallel runs distort each
  other's frame times.
- Do not rebuild while a run is going: the next location would launch the
  new binary.
- Compare runs only under the same conditions (window size, vsync, zoom
  parameters); `EXPERIMENTS.md` notes which score tables are comparable.
- A GPU hang ("context is guilty of a hard recovery") means a dispatch ran
  past the driver timeout; the per-frame step cap in `src/main.rs`
  (`MAX_STEPS`) is what prevents it, so changes to the work budget need a
  run on a slow GPU too (Radeon 780M laptop).
