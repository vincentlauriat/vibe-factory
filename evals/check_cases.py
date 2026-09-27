#!/usr/bin/env python3
"""Check evaluation cases offline; standard library only, no model calls.

For each case: the fixture passes its own `cargo test --offline`, and the oracle fails
against the untouched fixture. When evals/references/<case>/ exists, its files are laid
over a second copy of the fixture, which must then pass both its own tests and the oracle.
References are maintainer-only: run.py never reads them.
"""

import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from run import case_names, load_case, write_oracle, write_project

REFERENCES = Path(__file__).resolve().parent / "references"


def cargo_test(cwd, log):
    with log.open("w") as output:
        try:
            result = subprocess.run(["cargo", "test", "--offline"], cwd=cwd, stdout=output,
                                    stderr=subprocess.STDOUT, check=False, timeout=600)
        except subprocess.TimeoutExpired:
            return 124
    return result.returncode


def materialise(case, root, reference=None):
    project = root / "project"
    project.mkdir(parents=True)
    write_project(case, project)
    if reference is not None:
        for source in sorted(reference.rglob("*")):
            if source.is_file():
                target = project / source.relative_to(reference)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
    write_oracle(case, root / "oracle")
    return project, root / "oracle"


def check(name, work):
    """Returns (problems, row) for one case."""
    case = load_case(name)
    problems = []
    if case.get("id") != name:
        problems.append(f"id {case.get('id')!r} does not match file name")
    extra = set(case) - {"id", "title", "task", "files", "oracle"}
    if extra:
        problems.append(f"unexpected keys {sorted(extra)}")
    root = work / name / "fixture"
    project, oracle = materialise(case, root)
    fixture = cargo_test(project, root / "fixture.log")
    baseline = cargo_test(oracle, root / "baseline.log")
    if fixture:
        problems.append(f"fixture tests fail (exit {fixture}); see {root / 'fixture.log'}")
    if baseline == 0:
        problems.append("oracle already passes on the untouched fixture")
    row = {"case": name, "fixture": fixture, "oracle_before": baseline,
           "reference_tests": None, "oracle_after": None}
    reference = REFERENCES / name
    if reference.is_dir():
        root = work / name / "reference"
        project, oracle = materialise(case, root, reference)
        row["reference_tests"] = cargo_test(project, root / "fixture.log")
        row["oracle_after"] = cargo_test(oracle, root / "acceptance.log")
        if row["reference_tests"]:
            problems.append(f"reference fails its own tests; see {root / 'fixture.log'}")
        if row["oracle_after"]:
            problems.append(f"reference fails the oracle; see {root / 'acceptance.log'}")
    else:
        problems.append("no reference solution in evals/references/")
    return problems, row


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cases", nargs="*", metavar="case",
                        help="Cases to check (default: all of %s)" % ", ".join(case_names()))
    parser.add_argument("--keep", type=Path, help="Keep work files in this new directory")
    parser.add_argument("--allow-missing-reference", action="store_true")
    args = parser.parse_args()
    unknown = sorted(set(args.cases) - set(case_names()))
    if unknown:
        parser.error(f"unknown case(s): {', '.join(unknown)}")
    names = args.cases or case_names()
    if shutil.which("cargo") is None:
        parser.error("cargo not found")
    temp = None
    if args.keep:
        work = args.keep.resolve()
        work.mkdir(parents=True, exist_ok=False)
    else:
        temp = tempfile.TemporaryDirectory(prefix="vibe-cases-")
        work = Path(temp.name)
    failed = 0
    print(f"{'case':<10} {'fixture':>7} {'oracle':>6} {'ref tests':>9} {'ref oracle':>10}  result")
    for name in names:
        problems, row = check(name, work)
        if args.allow_missing_reference:
            problems = [p for p in problems if not p.startswith("no reference")]
        cells = [row["fixture"], row["oracle_before"], row["reference_tests"], row["oracle_after"]]
        cells = ["-" if cell is None else str(cell) for cell in cells]
        print(f"{name:<10} {cells[0]:>7} {cells[1]:>6} {cells[2]:>9} {cells[3]:>10}  "
              f"{'ok' if not problems else 'FAIL'}")
        for problem in problems:
            print(f"  - {problem}")
        failed += bool(problems)
    print("exit codes: fixture and reference must be 0; oracle must fail before and pass after")
    if temp is not None and failed:
        print("rerun with --keep DIR to inspect the logs", file=sys.stderr)
    if temp is not None:
        temp.cleanup()
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
