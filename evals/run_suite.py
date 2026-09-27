#!/usr/bin/env python3
"""Run every evaluation case (or a selection) several times, then summarise.

Standard library only. Each case/repetition gets a fresh destination under
DEST/<case>/rep-<n>; DEST must not exist. The summary is printed and written to
DEST/summary.json and DEST/summary.md. `--jobs N` runs up to N evaluations at
once. The suite stops after the first run whose error no other run can avoid:
rejected credentials, an unknown model or an invalid configuration; runs already
started finish, the others are skipped.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import subprocess
import sys
import threading

from run import case_names
import summarize

HERE = Path(__file__).resolve().parent

# `last_error` prefixes (error kinds) that fail every run the same way.
FATAL_ERRORS = ("AuthFailed", "InvalidRequest", "Config")


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
    parser.add_argument("--jobs", type=int, default=1,
                        help="Evaluations run at the same time (default: 1)")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("--repetitions must be at least 1")
    if args.jobs < 1:
        parser.error("--jobs must be at least 1")
    root = args.destination.resolve()
    root.mkdir(parents=True, exist_ok=False)
    cases = args.cases or case_names()
    runs = [(case, rep) for case in cases for rep in range(1, args.repetitions + 1)]
    state = {"failures": 0, "aborted": None}
    lock = threading.Lock()

    def run_one(case, rep):
        with lock:
            if state["aborted"]:
                return
            print(f"== {case} #{rep}", flush=True)
        dest = root / case / f"rep-{rep}"
        dest.parent.mkdir(parents=True, exist_ok=True)
        command = [sys.executable, str(HERE / "run.py"), case, str(dest),
                   "--vibe", args.vibe, "--provider", args.provider]
        if args.model:
            command += ["--model", args.model]
        if args.max_tokens:
            command += ["--max-tokens", str(args.max_tokens)]
        log_path = root / case / f"rep-{rep}.log"
        with log_path.open("w") as log:
            code = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                  check=False).returncode
        with lock:
            if not (dest / "report.json").exists():
                state["failures"] += 1
                print(f"   {case} #{rep}: harness error (exit {code}); see {log_path}", flush=True)
                return
            print(f"   {case} #{rep}: {'success' if code == 0 else 'failure'}", flush=True)
            error = fatal_error(dest / "report.json")
            if error and not state["aborted"]:
                state["aborted"] = error
                print(f"   stopping the suite: {error}", flush=True)

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for future in [pool.submit(run_one, case, rep) for case, rep in runs]:
            future.result()
    failures, aborted = state["failures"], state["aborted"]
    reports = summarize.load_reports([root])
    summary = summarize.summarize(reports)
    (root / "summary.json").write_text(summarize.to_json(summary))
    (root / "summary.md").write_text(summarize.to_markdown(summary))
    print(summarize.to_markdown(summary))
    if aborted:
        print("The suite stopped early: every run would fail the same way. Check the "
              "provider's API key and the model name, then start a new destination.",
              file=sys.stderr)
    return 1 if failures or aborted else 0


def fatal_error(report_path):
    """The error of a run that no other run can avoid (credentials, model name,
    configuration), or None."""
    try:
        error = json.loads(report_path.read_text()).get("error") or ""
    except (OSError, ValueError):
        return None
    return error if error.startswith(FATAL_ERRORS) else None


if __name__ == "__main__":
    raise SystemExit(main())
