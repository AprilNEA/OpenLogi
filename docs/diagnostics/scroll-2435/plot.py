"""Render the issue #2435 evidence graphs from the adjacent sanitized CSVs.

Requires matplotlib. Run this script from any directory; outputs stay beside it.
"""
from pathlib import Path
import csv

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parent
BLUE = "#0369a1"
ORANGE = "#c2410c"
GREEN = "#047857"
GRAY = "#64748b"
plt.rcParams.update({"font.size": 10, "axes.spines.top": False,
                     "axes.spines.right": False, "savefig.facecolor": "white"})


def load(name):
    with (ROOT / name).open(newline="") as stream:
        return [{key: float(value) if value else float("nan")
                 for key, value in row.items()} for row in csv.DictReader(stream)]


def column(rows, key):
    return [row[key] for row in rows]


def canvas(title, subtitle):
    fig, axes = plt.subplots(2, 1, figsize=(12, 6.8), sharex=True,
                             gridspec_kw={"height_ratios": [1, 2]})
    fig.suptitle(title, x=0.08, y=0.97, ha="left", fontsize=15, fontweight="bold")
    fig.text(0.08, 0.92, subtitle, ha="left", fontsize=10, color=GRAY)
    for axis in axes:
        axis.grid(axis="y", alpha=0.22)
        axis.axhline(0, color=GRAY, linewidth=0.6)
        axis.margins(x=0)
    axes[0].set_ylabel("Wheel input\n(notches/s)")
    axes[1].set_ylabel("Scroll speed (px/s)")
    axes[1].set_xlabel("Seconds from the start of the displayed interval")
    fig.subplots_adjust(top=0.86, bottom=0.14, left=0.09, right=0.97, hspace=0.17)
    return fig, axes


def save(fig, name, note):
    fig.text(0.09, 0.035, note, color=GRAY, fontsize=9)
    fig.savefig(ROOT / name, dpi=170, metadata={"Software": "Matplotlib"})
    plt.close(fig)


rows = load("after-2437-live.csv")
time = column(rows, "seconds")
fig, axes = canvas("After #2437: sustained wheel input, two speed collapses",
                   "Live MX Master 4 / macOS / Chrome capture · 100 ms bins · speed magnitudes")
axes[0].plot(time, column(rows, "wheel_notches_per_s"), color=BLUE, label="Physical wheel reports")
axes[0].set_ylim(bottom=0)
axes[0].legend(loc="upper right", frameon=False)
axes[1].plot(time, column(rows, "page_px_per_s"), color=ORANGE, label="Chrome page movement")
axes[1].plot(time, column(rows, "emitted_px_per_s"), color=GRAY, linestyle="--",
             linewidth=1, alpha=0.8, label="OpenLogi emitted scroll")
axes[1].set_ylim(bottom=0)
for lo, hi in [(5.2, 6.5), (7.7, 8.9)]:
    for axis in axes:
        axis.axvspan(lo, hi, color=ORANGE, alpha=0.08)
axes[1].legend(loc="upper left", frameon=False, ncol=2)
save(fig, "after-2437-live.png",
     "Shaded windows: page speed fell by over 90% while the wheel kept reporting rapidly. Final drop is the user stopping.")

rows = load("same-input-replay.csv")
time = column(rows, "seconds")
fig, axes = canvas("Same recorded input: merged #2437 baseline vs proposed tracker",
                   "Deterministic software replay · identical timestamps/settings · 100 ms bins · not a new hardware measurement")
axes[0].plot(time, column(rows, "wheel_notches_per_s"), color=BLUE, label="Identical recorded wheel input")
axes[0].set_ylim(bottom=0)
axes[0].legend(loc="upper right", frameon=False)
axes[1].plot(time, column(rows, "baseline_px_per_s"), color=ORANGE, linestyle="--",
             label="Merged #2437 baseline")
axes[1].plot(time, column(rows, "proposed_px_per_s"), color=GREEN, label="Proposed tracker (final code)")
axes[1].set_ylim(bottom=0)
for lo, hi in [(5.2, 6.5), (7.7, 8.9)]:
    for axis in axes:
        axis.axvspan(lo, hi, color=GRAY, alpha=0.07)
axes[1].legend(loc="upper left", frameon=False, ncol=2)
save(fig, "same-input-replay.png",
     "The final code sustains motion through both former slowdown windows; the finger stop still ends output.")

rows = load("wheel-driven-prototype-live.csv")
time = column(rows, "seconds")
fig, axes = canvas("Live prototype: sustained scrolling in both directions",
                   "MX Master 4 / macOS / Chrome · 42 s main test · before final numerical/lifecycle cleanup · 100 ms bins")
axes[0].plot(time, column(rows, "wheel_notches_per_s"), color=BLUE, linewidth=1)
axes[1].plot(time, column(rows, "page_px_per_s"), color=BLUE, label="Chrome page movement")
axes[1].plot(time, column(rows, "emitted_px_per_s"), color=ORANGE, linestyle="--",
             linewidth=1, alpha=0.75, label="OpenLogi emitted scroll")
axes[1].legend(loc="lower left", frameon=False, ncol=2)
save(fig, "wheel-driven-prototype-live.png",
     "Positive = down, negative = up. Turns/page edges account for sign changes and pauses. User feedback: 'feels good'.")
