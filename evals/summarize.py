#!/usr/bin/env python3
"""Summarise evaluation reports: success rate, wall time and tokens.

Standard library only. Pass report.json files or directories (searched
recursively). Groups by provider, model and case, plus one total row per
provider/model. Missing usage (the CLI failed before its summary) is counted
separately and excluded from token statistics.
"""

import argparse
import json
from pathlib import Path
import statistics
import sys


def load_reports(paths):
    reports = []
    for path in paths:
        path = Path(path)
        files = sorted(path.rglob("report.json")) if path.is_dir() else [path]
        for file in files:
            try:
                report = json.loads(file.read_text())
            except (OSError, ValueError) as error:
                print(f"skipping {file}: {error}", file=sys.stderr)
                continue
            if not isinstance(report, dict) or "case" not in report:
                print(f"skipping {file}: not an evaluation report", file=sys.stderr)
                continue
            report["_path"] = str(file)
            reports.append(report)
    return reports


def tokens(report):
    usage = report.get("usage")
    if not isinstance(usage, dict):
        return None
    return int(usage.get("input_tokens", 0)) + int(usage.get("output_tokens", 0))


def row(label, reports):
    runs = len(reports)
    successes = sum(1 for r in reports if r.get("success"))
    times = [r["duration_ms"] / 1000 for r in reports if isinstance(r.get("duration_ms"), (int, float))]
    counted = [t for t in (tokens(r) for r in reports) if t is not None]
    attempts = [len(r.get("validations") or []) for r in reports]
    return {
        **label,
        "runs": runs,
        "successes": successes,
        "success_rate": round(successes / runs, 3) if runs else None,
        "mean_seconds": round(statistics.mean(times), 1) if times else None,
        "median_seconds": round(statistics.median(times), 1) if times else None,
        "total_tokens": sum(counted) if counted else None,
        "mean_tokens": round(statistics.mean(counted)) if counted else None,
        "runs_without_usage": runs - len(counted),
        "mean_validation_attempts": round(statistics.mean(attempts), 2) if attempts else None,
    }


def summarize(reports):
    groups = {}
    for r in reports:
        key = (r.get("provider"), r.get("model_override"), r.get("case"))
        groups.setdefault(key, []).append(r)
    rows = [row({"provider": p, "model": m, "case": c}, rs)
            for (p, m, c), rs in sorted(groups.items(), key=lambda kv: tuple(str(x) for x in kv[0]))]
    totals = {}
    for r in reports:
        totals.setdefault((r.get("provider"), r.get("model_override")), []).append(r)
    rows += [row({"provider": p, "model": m, "case": "ALL"}, rs)
             for (p, m), rs in sorted(totals.items(), key=lambda kv: tuple(str(x) for x in kv[0]))]
    commits = sorted({(r.get("framework") or {}).get("commit") or "unknown" for r in reports})
    return {"schema_version": 1, "reports": len(reports), "framework_commits": commits,
            "rows": rows}


def fmt(value):
    return "-" if value is None else str(value)


def to_markdown(summary):
    header = ["provider", "model", "case", "runs", "success", "mean s", "median s",
              "mean tokens", "no usage", "validations"]
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    for r in summary["rows"]:
        rate = "-" if r["success_rate"] is None else f"{r['successes']}/{r['runs']}"
        cells = [r["provider"], r["model"], r["case"], r["runs"], rate, r["mean_seconds"],
                 r["median_seconds"], r["mean_tokens"], r["runs_without_usage"],
                 r["mean_validation_attempts"]]
        lines.append("| " + " | ".join(fmt(c) for c in cells) + " |")
    lines.append("")
    lines.append(f"{summary['reports']} report(s); framework commit(s): "
                 + ", ".join(summary["framework_commits"]))
    return "\n".join(lines) + "\n"


def to_json(summary):
    return json.dumps(summary, indent=2) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="+", type=Path, help="report.json files or directories")
    parser.add_argument("--json", action="store_true", help="print JSON instead of Markdown")
    args = parser.parse_args()
    reports = load_reports(args.paths)
    if not reports:
        parser.error("no report.json found")
    summary = summarize(reports)
    print(to_json(summary) if args.json else to_markdown(summary), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
