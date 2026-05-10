#!/usr/bin/env python3
"""Compare a fresh criterion run against the committed baseline.

Reads the per-benchmark median from
`target/criterion/<group>/<id>/new/estimates.json` (or the bare
`<bench>/new/estimates.json` for ungrouped benches) produced by the most
recent `cargo bench -p portunus-client --bench data_plane` run, and exits
non-zero if any benchmark's median regressed beyond `--max-regression-pct`
versus `crates/portunus-client/benches/baselines/v0.1.0.json`.

Usage:
    cargo bench -p portunus-client --bench data_plane -- --save-baseline new
    python3 scripts/bench_regression_gate.py [--max-regression-pct 25]

The default 25% tolerance is intentionally loose — CI runners are noisy
and small variation is expected. A real regression typically shows up as
2-3x, not 10%. Tighten it once you have data on actual run-to-run noise
on the CI host.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASELINE = ROOT / "crates/portunus-client/benches/baselines/v0.1.0.json"
CRITERION = ROOT / "target/criterion"

# Map our baseline keys → criterion's filesystem layout. Throughput
# benches live under a group dir; the latency bench is ungrouped.
LAYOUT = {
    "data_plane.throughput.64KiB_echo": "data_plane.throughput/64KiB_echo",
    "data_plane.throughput.1024KiB_echo": "data_plane.throughput/1024KiB_echo",
    "data_plane.rtt_1byte_through_proxy": "data_plane.rtt_1byte_through_proxy",
}


def load_baseline() -> dict[str, float]:
    data = json.loads(BASELINE.read_text())
    return {name: spec["median"] for name, spec in data["benchmarks"].items()}


def load_fresh(name: str) -> float | None:
    rel = LAYOUT.get(name)
    if rel is None:
        return None
    path = CRITERION / rel / "new" / "estimates.json"
    if not path.exists():
        return None
    return json.loads(path.read_text())["median"]["point_estimate"]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--max-regression-pct",
        type=float,
        default=25.0,
        help="fail if any benchmark's median is >this percent slower than baseline",
    )
    args = parser.parse_args()

    baseline = load_baseline()
    rows: list[tuple[str, float, float | None, float | None]] = []
    failed = False
    missing = False
    for name, base_median in baseline.items():
        fresh_median = load_fresh(name)
        if fresh_median is None:
            rows.append((name, base_median, None, None))
            missing = True
            continue
        delta_pct = (fresh_median - base_median) / base_median * 100.0
        rows.append((name, base_median, fresh_median, delta_pct))
        if delta_pct > args.max_regression_pct:
            failed = True

    width = max(len(name) for name, *_ in rows)
    print(f"{'benchmark'.ljust(width)}  baseline_ns   fresh_ns      delta")
    for name, base, fresh, delta in rows:
        if fresh is None:
            print(f"{name.ljust(width)}  {base:>11.1f}   <missing>     —")
        else:
            marker = "" if delta <= args.max_regression_pct else "  ← REGRESSION"
            print(
                f"{name.ljust(width)}  {base:>11.1f}   {fresh:>10.1f}  "
                f"{delta:+6.2f}%{marker}"
            )

    if missing:
        print(
            "\nERROR: one or more benchmarks have no fresh measurement. "
            "Did you run `cargo bench` first?",
            file=sys.stderr,
        )
        return 2
    if failed:
        print(
            f"\nFAIL: regression exceeded {args.max_regression_pct:.0f}% threshold.",
            file=sys.stderr,
        )
        return 1
    print(f"\nOK: all benchmarks within {args.max_regression_pct:.0f}% of baseline.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
