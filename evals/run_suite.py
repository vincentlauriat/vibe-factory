#!/usr/bin/env python3
"""Run every evaluation case (or a selection) several times, then summarise.

Standard library only. Each case/repetition gets a fresh destination under
DEST/<case>/rep-<n>; DEST must not exist. The summary is printed and written to
DEST/summary.json and DEST/summary.md.
"""

import argparse
from pathlib import Path
import subprocess
import sys

from run import case_names
import summarize

HERE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("destination", type=Path, help="New directory; never overwritten")
    parser.add_argument("--provider", required=True, help="mock for an offline smoke test")
    parser.add_argument("--model", help="provider/model used for every phase")
    parser.add_argument("--vibe", default="vibe")
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--max-tokens", type=int, help="token budget per run")
    parser.add_argument("--cases", nargs="+", choices=case_names(), metavar="CASE",
                        help="Cases to run (default: all of %s)" % ", ".join(case_names()))
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("--repetitions must be at least 1")
    root = args.destination.resolve()
    root.mkdir(parents=True, exist_ok=False)
    cases = args.cases or case_names()
    failures = 0
    for case in cases:
        for rep in range(1, args.repetitions + 1):
            dest = root / case / f"rep-{rep}"
            dest.parent.mkdir(parents=True, exist_ok=True)
            command = [sys.executable, str(HERE / "run.py"), case, str(dest),
                       "--vibe", args.vibe, "--provider", args.provider]
            if args.model:
                command += ["--model", args.model]
            if args.max_tokens:
                command += ["--max-tokens", str(args.max_tokens)]
            print(f"== {case} #{rep}", flush=True)
            with (root / case / f"rep-{rep}.log").open("w") as log:
                code = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                      check=False).returncode
            if not (dest / "report.json").exists():
                failures += 1
                print(f"   harness error (exit {code}); see {root / case / f'rep-{rep}.log'}")
            else:
                print(f"   {'success' if code == 0 else 'failure'}")
    reports = summarize.load_reports([root])
    summary = summarize.summarize(reports)
    (root / "summary.json").write_text(summarize.to_json(summary))
    (root / "summary.md").write_text(summarize.to_markdown(summary))
    print(summarize.to_markdown(summary))
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
