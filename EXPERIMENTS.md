# Experiments

The zoom must flow: frames at the display rate while zooming, and the first
picture of a region (coarser tiles magnified, or tiles computed ahead of the
zoom) already showing the shapes of the final one. Every idea is tried in
turn and its score recorded here, whether it is kept or rejected.

## How a run is scored

`tools/preview_diff.py --zoom 2 --levels 8` starts the app 8 levels
shallower than each of four fixed locations and zooms in at 2 levels per
second, stopping on the location. The app writes its own frame (`--capture`)
on the frame the zoom stops (the preview) and on the first frame with no
tile work left (settled), and logs every frame slower than 25 ms. One row
per location: black fraction of both frames, mean difference / share of
pixels differing by more than 32 of 255, slow frames up to the preview
capture, worst frame.

Locations: minibrot in dust and spiral valley from the README (levels 36 and
33 on arrival), and each 8 levels deeper (minibrot+8 at level 44, spiral+8
at level 41).

The app runs on the Hyprland workspace `test:a` with its own app id, so it
never takes focus, and without vsync, because the compositor throttles a
hidden window. The window is 1920x1168 there. Black fractions are below
0.001 everywhere unless stated. Single runs on a shared desktop: differences
of ten points in the mean difference are noise.

Earlier runs used other conditions, so each score table compares only
within itself: the first two used a visible window with vsync, 1920x1140,
screenshots, and a fixed 15 s "late" frame, which turned out not to be
settled at the minibrot locations; the third still used the 15 s late
frame. Static comparisons (`tools/preview_diff.py` without `--zoom`) start
at the location and capture its settled picture, which is what the
bit-identity checks between builds use.

## Scores

### Visible window, static start (preview at 2 s, late at 15 s)

| Build | minibrot | spiral | minibrot+8 | spiral+8 |
|---|---|---|---|---|
| baseline | 19.6 / 0.24 | 0.0 / 0.00 | 40.6 / 0.29 | 10.9 / 0.03 |
| slow cap growth + view cap | 35.2 / 0.44 | 0.0 / 0.00 | 65.2 / 0.44 | 12.2 / 0.04 |
| + root fallback + 4096-step dispatch | 18.7 / 0.23 | 0.0 / 0.00 | 48.5 / 0.38 | 10.4 / 0.03 |

### Visible window, zoom (late at 15 s)

| Build | minibrot | spiral | minibrot+8 | spiral+8 |
|---|---|---|---|---|
| no look-ahead | 44.7 / 0.54 | 5.4 / 0.01 | 32.8 / 0.47 | 12.4 / 0.05 |
| + look-ahead | 40.7 / 0.50 | 2.8 / 0.00 | 22.3 / 0.21 | 1.0 / 0.00 |
| same, 2048-step dispatch | 29.7 / 0.34 | no capture | 31.3 / 0.43 | 15.8 / 0.09 |
| same, 4096-step, frame stats added | 40.7 / 0.50, 48 of 87 slow | 2.9 / 0.00, 40 of 148 | 31.6 / 0.44, 48 of 146 | 12.4 / 0.05, 30 of 193 |

Every one of these runs had a worst frame of 100 ms (the clamp) and drew
far fewer than 60 frames per second while zooming.

### Hidden window, zoom (late at 15 s), slow frames / frames up to the preview, worst

| Build | minibrot | spiral | minibrot+8 | spiral+8 |
|---|---|---|---|---|
| 4096-step, without done-skipping | 41.0 / 0.51, 45 of 88, 100 ms | 1.7 / 0.00, 52 of 166, 100 ms | 34.6 / 0.48, 59 of 168, 100 ms | 11.3 / 0.04, 41 of 287, 100 ms |
| 4096-step, with done-skipping | 41.4 / 0.51, 46 of 89, 100 ms | 2.2 / 0.00, 54 of 171, 100 ms | 34.6 / 0.48, 61 of 174, 100 ms | 11.3 / 0.04, 45 of 296, 100 ms |
| 256-step | 39.5 / 0.49, 48 of 92, 100 ms | 2.1 / 0.00, 54 of 178, 100 ms | 37.8 / 0.53, 1 of 500, 100 ms | 12.8 / 0.05, 25 of 552, 100 ms |
| 256-step, all levels by perturbation | 67.0 / 0.72, 9 of 500, 60 ms | 16.6 / 0.07, 6 of 379, 37 ms | 39.1 / 0.56, 3 of 525, 63 ms | 11.3 / 0.04, 12 of 610, 43 ms |
| 256-step, resumable direct tiles | 49.8 / 0.58, 0 of 483, 21 ms | 6.2 / 0.01, 5 of 581, 26 ms | 37.7 / 0.52, 2 of 528, 41 ms | 12.4 / 0.05, 15 of 691, 53 ms |

### Hidden window, zoom, settled capture; also the time to settle

From here on the late frame is the first frame with no tile work left, and
the zoom stops exactly on the target, so runs end on identical views.

| Build | minibrot | spiral | minibrot+8 | spiral+8 |
|---|---|---|---|---|
| 256-step, edge-tile fix | 55.6 / 0.62, 0 of 582, 22 ms, 93 s | 7.2 / 0.01, 2 of 533, 28 ms, 7 s | 41.6 / 0.59, 4 of 526, 50 ms, 165 s | 13.1 / 0.05, 17 of 704, 55 ms, 32 s |
| same, 32-bit perturbation | 58.9 / 0.65, 0 of 533, 21 ms, 71 s | 7.2 / 0.01, 1 of 537, 27 ms, 7 s | 41.0 / 0.59, 1 of 514, 28 ms, 117 s | 11.1 / 0.04, 11 of 759, 41 ms, 26 s |
| batched dispatch, adaptive steps | 44.7 / 0.53, 30 of 315, 100 ms, 31 s | 1.8 / 0.00, 33 of 440, 61 ms, 4 s | 38.7 / 0.53, 36 of 353, 100 ms, 56 s | 8.8 / 0.02, 28 of 386, 72 ms, 10 s |
| batched, budget in pixel-steps | 43.2 / 0.52, 16 of 667, 31 ms, 29 s | 1.6 / 0.00, 14 of 585, 41 ms, 4 s | 38.7 / 0.53, 10 of 643, 34 ms, 54 s | 8.5 / 0.02, 15 of 625, 43 ms, 10 s |
| same, 32-bit perturbation | 43.2 / 0.52, 8 of 632, 35 ms, 15 s | 1.6 / 0.00, 7 of 612, 37 ms, 4 s | 26.8 / 0.34, 9 of 652, 35 ms, 20 s | 3.8 / 0.01, 7 of 627, 29 ms, 5 s |

### Static views, settled pictures compared between builds

| Comparison | minibrot | spiral | minibrot+8 | spiral+8 |
|---|---|---|---|---|
| one-pass vs 256-step direct (bit-identical expected) | identical | not run | identical | not run |
| 256-step vs batched dispatch (bit-identical expected) | identical | identical | identical | identical |
| 64-bit vs 32-bit perturbation, share of pixels differing by more than 32 | 0.32 | 0.00 | 0.16 | 0.0003 |
| settle time, 256-step 64-bit / 32-bit | 88 / 67 s | 4 / 4 s | 151 / 107 s | 27 / 20 s |
| settle time, batched 64-bit | 27 s | 1.3 s | 50 s | 6.3 s |

The 32-bit differences sit in chaotic dust, where neighbouring pixels'
counts differ by thousands; shapes and smooth regions match.

## Findings

- **The two render paths agree.** Direct fp64 and perturbation produce the
  same picture at the spiral locations (under 0.1% of pixels differ when
  settled). The switch at level 36 is not a visual seam.
- **The zoom stalled on single heavy dispatches.** `--trace` showed that one
  dispatch of a 4096-step perturbation slice, or one direct slice at levels
  26 to 35, took 35 to 100 ms, so even one slice per frame stalled.
- **Interior pixels decide how fast tiles finish.** Settling the minibrot
  locations took 34 and 54 s with the direct path and 107 and 141 s with
  perturbation, which has no interior test and runs interior pixels to the
  cap (both on per-slice dispatch, before the edge-tile fix).
- **Resumable direct tiles are exact.** A one-pass build and the 256-step
  build give bit-identical settled pictures.
- **The GPU was busy but badly occupied.** The driver reported the GPU 99%
  busy, yet 32-bit math only gave 1.3 times the speed: each dispatch held
  8192 pixels and dispatches writing the same buffers run one after
  another. One dispatch per shader per frame over all 12 in-flight tiles
  (786k pixels) settled every location 3 to 4 times faster, with
  bit-identical pictures.
- **Once occupied, 64-bit arithmetic was the cost.** On the batched build,
  32-bit perturbation settles the deep views 2 to 2.6 times faster and
  sharpens the deep previews. Precision is verified at levels 36 to 44
  only.
- **A step budget overloads frames that start tiles.** With a fixed step
  count per frame, the count grew while most pixels were finished, and
  fresh tiles then cost a 100 ms frame. Budgeting pixel-steps (steps =
  budget / pixels still iterating) removed the spikes.
- **64-bit floats are not needed for most of the range.** Perturbation
  deltas need relative, not absolute, precision; 32-bit holds them down to
  about 1e-38 (level ~110). 64-bit is needed by the direct path between
  levels ~12 and 36, and by perturbation past level 100.

## Where to pick up

1. Verify 32-bit perturbation deeper than level 44: record a location near
   level 80 to 100 (press `C` there) and compare settled 64-bit and 32-bit
   pictures, as in the static comparison table.
2. The minibrot preview (level 36) is still the weakest score: interior and
   near-interior pixels run to the cap in perturbation. An interior test in
   the perturbation shaders, then perturbation from a shallower level so the
   fast 32-bit path also covers levels 12 to 36.
3. More slots in flight (12 now, 4 MB of state each) once the scheduler can
   keep them filled with useful tiles.
4. The remaining slow frames (about 1 in 80) are the budget probing upward
   and halving; a gentler increase or a smoothed frame time would remove
   most of them.

Nothing is committed yet.

## Ideas

| Idea | Status | Observed |
|---|---|---|
| Content-adaptive palette normalisation | rejected | Colours drifted while zooming, breaking texture stability. |
| Iteration cap grows slowly per level (`2000 + 400 × level`, was `150 + 100 × 1.12^level`) and fallback tiles use the cap of the view's level | kept | Black blobs at minibrot sites vanish from the preview (they were cap hits, not interior). |
| Ancestor walk falls back to the root when no ancestor within 12 levels is ready | kept | Removes unrendered black strips in deep previews. |
| Look-ahead scheduling: while zooming in, compute the tiles the zoom is heading into near the focus, skip tiles that leave the screen within half a second | kept | Arrival frame much closer to the settled picture at every location. Its first version also dropped half-visible edge tiles when not zooming (fixed). |
| Zoom focus defaults to the window centre until the mouse moves | kept | A `--zoom-speed` run drifted off its target before this. |
| `STEP_CAP` 4096 (from 512) | superseded | Fewer dispatches, but each one took 35 to 100 ms. |
| `STEP_CAP` 2048 | rejected | No better than 4096. |
| `STEP_CAP` 256 | superseded | Perturbation zoom (minibrot+8) went from 61 slow frames to 1; replaced by the per-frame budget of batched dispatch. |
| Per-slice finished-pixel counter on the GPU, read back asynchronously | kept, now per tile | Neutral at 4096-step slices; with batched dispatch it is how the CPU learns that a tile is done. |
| All levels by perturbation (`--perturb-from 0`) | rejected | Smooth frames, but previews much blurrier and settling 3 times slower: no interior test. |
| Resumable direct tiles: `compute.wgsl` iterates in chunks like the perturbation path | kept | Remaining slow frames gone at the shallow locations (worst 21 to 26 ms, from 100 ms). Bit-identical to one-pass. |
| Exact zoom stop for test runs | kept | Runs end on identical views, so settled pictures compare between builds. |
| 32-bit perturbation deltas, orbit and skip table (`perturb32.wgsl`) | kept, default up to level 100 | 1.3 times faster on per-slice dispatch, 2 to 2.6 times on batched dispatch; pictures match except colour noise in dust. Verified at levels 36 to 44. |
| One dispatch per shader per frame over all in-flight tiles | kept | 3 to 4 times faster settling, bit-identical pictures. |
| Per-frame step count adapted to frame time | superseded | 100 ms spikes when fresh tiles started. |
| Per-frame budget in pixel-steps | kept | Slow frames 10 to 16 per 600, worst 31 to 43 ms. |
| Interior test in the perturbation shader | not tried | Needed before perturbation can replace the direct path. |
| More tiles in flight (more state slots) | not tried | |
| Paint cap-hit pixels with the cap count's colour instead of black; black only for proven interior | not tried | Needs a decision on how minibrots look in the perturbation regime. |
