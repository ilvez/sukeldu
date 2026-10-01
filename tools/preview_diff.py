#!/usr/bin/env python
"""Preview score: how far the first picture of a location is from the settled one.

Runs the release binary at each location with `--capture`, which makes the app
write its own frame at an early time (the preview a zooming user sees) and on
the first frame with no tile work left (the settled picture), and prints one
table row per location with the black fraction of both captures, their
difference, and how many frames took longer than 25 ms up to the preview
capture.

The app runs with its own app id on the Hyprland workspace test:a, without
taking focus (a runtime window rule, registered here, gone on config reload),
and without vsync, because the compositor throttles windows that are not
visible; frame times therefore measure the app's own work.

With --zoom SPEED the app starts LEVELS levels shallower and zooms into the
location at SPEED levels per second, stopping there; the early capture is
the frame on which it arrives, so it shows what the look-ahead had ready.

    python tools/preview_diff.py [--early S] [--timeout S] [--tag NAME] [--only PREFIX]
                                 [--zoom SPEED --levels N] [--compare OTHER_TAG]
                                 [-- APP_ARGS...]
"""

import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target" / "release" / "sukeldu"
OUT = Path.home() / "temp" / "sukeldu"
TEXT_MASK = (60, 800)
DIFF_THRESHOLD = 32
APP_ID = "sukeldu-test"
WINDOW_RULE = (
    f'hl.window_rule({{ match = {{ class = "^{APP_ID}$" }}, workspace = "name:test:a silent" }})'
)

LOCATIONS = [
    ("minibrot", "-0.7394310682635371", "-0.1268495465745192", "4.263e-13"),
    ("spiral", "-0.7436438870371587", "0.1318259042053119", "2.0e-12"),
    ("minibrot+8", "-0.7394310682635371", "-0.1268495465745192", "1.665e-15"),
    ("spiral+8", "-0.7436438870371587", "0.1318259042053119", "7.8125e-15"),
    ("minibrot-16", "-0.7394310682635371", "-0.1268495465745192", "2.79379968e-8"),
    ("spiral-16", "-0.7436438870371587", "0.1318259042053119", "1.31072e-7"),
    ("bulbs", "-0.125", "0.6495", "2.5e-5"),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--early", type=float, default=2.0)
    parser.add_argument("--timeout", type=float, default=120.0, help="seconds to wait for settling")
    parser.add_argument("--tag", default="run")
    parser.add_argument("--only", help="location name prefix")
    parser.add_argument("--zoom", type=float, help="zoom speed in levels per second")
    parser.add_argument("--levels", type=int, default=8, help="levels to zoom through")
    parser.add_argument("--compare", metavar="TAG", help="compare settled pictures with run TAG")
    parser.add_argument("app_args", nargs="*")
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)

    subprocess.run(["hyprctl", "eval", WINDOW_RULE], check=True, capture_output=True)
    print(
        "| location | black early | black late | mean diff | pixels > 32 "
        "| slow frames | worst | settled after |"
    )
    print("|---|---|---|---|---|---|---|---|")
    names = [loc[0] for loc in LOCATIONS if not args.only or loc[0].startswith(args.only)]
    for name, cx, cy, upp in LOCATIONS:
        if name not in names:
            continue
        early, late, frames = capture(name, cx, cy, upp, args)
        print(score_row(name, early, late) + frames + settle_time(OUT / f"{args.tag}_{name}"))
    if args.compare:
        compare(args.tag, args.compare, names)


def compare(tag, other, names):
    print(f"\n| location | settled {tag} vs {other}: identical | mean diff | pixels > 32 |")
    print("|---|---|---|---|")
    for name in names:
        ours = load(OUT / f"{tag}_{name}_settled.ppm")
        theirs = load(OUT / f"{other}_{name}_settled.ppm")
        if ours is None or theirs is None or ours.shape != theirs.shape:
            print(f"| {name} | missing or different size | - | - |")
            continue
        diff = np.abs(ours - theirs).max(axis=2)
        print(
            f"| {name} | {(diff == 0).mean():.4f} | {diff.mean():.2f} "
            f"| {(diff > DIFF_THRESHOLD).mean():.4f} |"
        )


def settle_time(prefix):
    match = re.search(r"_settled\.ppm: ([\d.]+)s", Path(f"{prefix}.log").read_text())
    return f" {match.group(1)} s |" if match else " - |"


def capture(name, cx, cy, upp, args):
    prefix = OUT / f"{args.tag}_{name}"
    early_at = f"{args.early:g}"
    app_args = list(args.app_args)
    if args.zoom:
        start_upp = float(upp) * 2 ** args.levels
        app_args += ["--zoom-speed", str(args.zoom), "--zoom-to", upp]
        early_at = "stop"
        upp = repr(start_upp)
    cmd = [str(BINARY), "--app-id", APP_ID, "--no-vsync", "--at", cx, cy, upp]
    cmd += ["--capture", f"{early_at},settled", str(prefix)]
    early_path = f"{prefix}_{early_at}.ppm"
    late_path = Path(f"{prefix}_settled.ppm")
    for stale in (Path(early_path), late_path):
        stale.unlink(missing_ok=True)
    with open(f"{prefix}.log", "w") as log:
        app = subprocess.Popen(cmd + app_args, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline and not written(prefix, [early_path, late_path]):
                time.sleep(0.5)
        finally:
            app.terminate()
            app.wait()
    return load(early_path), load(late_path), frame_stats(prefix, early_path)


def written(prefix, paths):
    log = Path(f"{prefix}.log").read_text()
    return all(str(path) in log for path in paths)


def frame_stats(prefix, early_path):
    log = Path(f"{prefix}.log").read_text()
    match = re.search(
        re.escape(early_path) + r":.*slow frames (\d+) of (\d+), worst (\d+)ms", log
    )
    if match is None:
        return " - | - |"
    slow, total, worst = match.groups()
    return f" {slow}/{total} | {worst} ms |"


def load(path):
    if not Path(path).exists():
        return None
    pixels = np.asarray(Image.open(path).convert("RGB")).astype(int)
    pixels[: TEXT_MASK[0], : TEXT_MASK[1]] = 255
    return pixels


def score_row(name, early, late):
    if early is None or late is None:
        return f"| {name} | no capture | - | - | - |"
    if early.shape != late.shape:
        return f"| {name} | window resized | - | - | - |"
    diff = np.abs(early - late).max(axis=2)
    return (
        f"| {name} | {black_fraction(early):.4f} | {black_fraction(late):.4f} "
        f"| {diff.mean():.1f} | {(diff > DIFF_THRESHOLD).mean():.3f} |"
    )


def black_fraction(pixels):
    return (pixels.max(axis=2) == 0).mean()


if __name__ == "__main__":
    sys.exit(main())
