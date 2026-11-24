#!/usr/bin/env python3
"""Gate a pqr-guard bench report for the public 1M contract."""

import argparse
import json
import math
import sys
from pathlib import Path


def fail(errors, message):
    errors.append(message)


def positive_number(report, key, errors):
    value = report.get(key)
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        fail(errors, f"{key}: missing or non-numeric")
        return None
    value = float(value)
    if not math.isfinite(value) or value <= 0:
        fail(errors, f"{key}: must be finite and positive, got {value}")
        return None
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path, help="bench --report JSON")
    parser.add_argument("--p50-max", type=float, default=4.0)
    parser.add_argument("--p95-max", type=float, default=5.0)
    parser.add_argument("--recall-min", type=float, default=0.99)
    parser.add_argument("--ef-search", type=int, default=128)
    args = parser.parse_args()

    try:
        report = json.loads(args.report.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        print(f"FAIL: cannot read report: {exc}", file=sys.stderr)
        return 2
    if not isinstance(report, dict):
        print("FAIL: report must be a JSON object", file=sys.stderr)
        return 2

    errors = []
    for key, want in (
        ("baseline", "usearch-hnsw"),
        ("queries", 100),
        ("k", 10),
        ("repeat", 5),
    ):
        if report.get(key) != want:
            fail(errors, f"{key}: expected {want!r}, got {report.get(key)!r}")

    if report.get("hnsw_ef_search") != args.ef_search:
        fail(
            errors,
            f"hnsw_ef_search: expected {args.ef_search}, got {report.get('hnsw_ef_search')!r}",
        )
    recall = report.get("hnsw_recall")
    if not isinstance(recall, (int, float)) or isinstance(recall, bool) or not math.isfinite(recall):
        fail(errors, f"hnsw_recall: missing or invalid value {recall!r}")
    elif recall < args.recall_min:
        fail(errors, f"hnsw_recall: {recall:.6f} < {args.recall_min}")

    times = {
        key: positive_number(report, key, errors)
        for key in ("pqr_p50_ms", "pqr_p95_ms", "hnsw_p50_ms", "hnsw_p95_ms")
    }

    p50_ratio = None
    p95_ratio = None
    if all(times.values()):
        p50_ratio = times["pqr_p50_ms"] / times["hnsw_p50_ms"]
        p95_ratio = times["pqr_p95_ms"] / times["hnsw_p95_ms"]
        if p50_ratio > args.p50_max:
            fail(errors, f"p50 ratio {p50_ratio:.3f}x > {args.p50_max:.3f}x")
        if p95_ratio > args.p95_max:
            fail(errors, f"p95 ratio {p95_ratio:.3f}x > {args.p95_max:.3f}x")
        stated = report.get("pqr_over_hnsw")
        if not isinstance(stated, (int, float)) or abs(stated - p50_ratio) > 1e-9:
            fail(errors, f"pqr_over_hnsw: stale or invalid value {stated!r}")

    if errors:
        print("FAIL: 1M benchmark contract breached", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print(
        json.dumps(
            {
                "status": "pass",
                "pqr_p50_ms": times["pqr_p50_ms"],
                "pqr_p95_ms": times["pqr_p95_ms"],
                "hnsw_p50_ms": times["hnsw_p50_ms"],
                "hnsw_p95_ms": times["hnsw_p95_ms"],
                "p50_over_hnsw": p50_ratio,
                "p95_over_hnsw": p95_ratio,
                "hnsw_ef_search": report["hnsw_ef_search"],
                "hnsw_recall": recall,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
