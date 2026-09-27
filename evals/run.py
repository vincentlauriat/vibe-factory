#!/usr/bin/env python3
"""Run one isolated evaluation; standard library only, no API keys in reports."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


def execute(args, cwd, env, log):
    with log.open("w") as output:
        try:
            result = subprocess.run(args, cwd=cwd, env=env, stdout=output,
                                    stderr=subprocess.STDOUT, check=False, timeout=1800)
        except subprocess.TimeoutExpired:
            return 124
    return result.returncode


CASES = Path(__file__).resolve().parent / "cases"
FIXTURE_MANIFEST = '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2024"\n\n[workspace]\n'
ORACLE_MANIFEST = ('[package]\nname = "oracle"\nversion = "0.1.0"\nedition = "2024"\n'
                   '\n[workspace]\n\n[dependencies]\nfixture = { path = "../project" }\n')


def case_names():
    return sorted(path.stem for path in CASES.glob("*.json"))


def load_case(name):
    return json.loads((CASES / f"{name}.json").read_text())


def write_project(case, project):
    """Write only the fixture files: nothing else from the case (or evals/) reaches the agent."""
    for name, content in case["files"].items():
        path = project / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    (project / "Cargo.toml").write_text(FIXTURE_MANIFEST)
    (project / ".gitignore").write_text("/target\n/.vibe\n")


def write_oracle(case, oracle):
    """Acceptance crate beside project/, depending on the fixture by path."""
    (oracle / "tests").mkdir(parents=True)
    (oracle / "Cargo.toml").write_text(ORACLE_MANIFEST)
    (oracle / "tests" / "acceptance.rs").write_text(case["oracle"])


def framework_info(vibe):
    """Version of the vibe binary and commit of the framework checkout, when known."""
    info = {"vibe_version": None, "commit": None}
    try:
        out = subprocess.run([vibe, "--version"], capture_output=True, text=True, timeout=60)
        info["vibe_version"] = out.stdout.strip() or None
        out = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True,
                             timeout=60, cwd=Path(__file__).resolve().parent)
        if out.returncode == 0:
            info["commit"] = out.stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        pass
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", choices=case_names())
    parser.add_argument("destination", type=Path, help="New directory; never overwritten")
    parser.add_argument("--vibe", default="vibe")
    parser.add_argument("--provider", required=True, help="mock for an offline smoke test")
    parser.add_argument("--model")
    parser.add_argument("--max-tokens", type=int,
                        help="Pause the run beyond this many tokens (vibe run --max-tokens)")
    args = parser.parse_args()
    vibe = shutil.which(args.vibe)
    if not vibe:
        parser.error("vibe executable not found; build it or pass --vibe")
    vibe = str(Path(vibe).resolve())
    case = load_case(args.case)
    root = args.destination.resolve()
    root.mkdir(parents=True, exist_ok=False)
    project = root / "project"
    project.mkdir()
    write_project(case, project)
    env = os.environ.copy()
    env.update(GIT_AUTHOR_NAME="Vibe evaluation", GIT_COMMITTER_NAME="Vibe evaluation",
               GIT_AUTHOR_EMAIL="eval@example.invalid", GIT_COMMITTER_EMAIL="eval@example.invalid")
    setup = [
        ["git", "init", "-q", "-b", "main"],
        ["git", "add", "."],
        ["git", "-c", "core.hooksPath=", "-c", "commit.gpgsign=false", "commit", "-qm", "Evaluation baseline"],
        [vibe, "init"],
        [vibe, "config", "set", "pipeline.max_parallel_subtasks", "1"],
        [vibe, "config", "set", "pipeline.validation_commands", '["cargo test --offline"]'],
        [vibe, "task", "add", case["title"], "-d", case["task"]],
    ]
    for i, command in enumerate(setup):
        if execute(command, project, env, root / f"setup-{i}.log"):
            raise SystemExit(f"Setup failed; see {root / f'setup-{i}.log'}")

    # Kept outside the agent workspace; the oracle is compiled against its final source.
    oracle = root / "oracle"
    write_oracle(case, oracle)
    if execute(["cargo", "test", "--offline"], project, env, root / "fixture.log"):
        raise SystemExit("Fixture does not build; see fixture.log")
    baseline = execute(["cargo", "test", "--offline"], oracle, env, root / "baseline.log")
    if baseline == 0:
        raise SystemExit("Invalid fixture: baseline already passes the oracle")
    command = [vibe, "--json", "run", "1", "--workspace", "in_place",
               "--provider", args.provider]
    if args.model:
        command.extend(["--model", args.model])
    if args.max_tokens:
        command.extend(["--max-tokens", str(args.max_tokens)])
    start = time.monotonic()
    exit_code = execute(command, project, env, root / "run.jsonl")
    duration_ms = round((time.monotonic() - start) * 1000)
    acceptance = execute(["cargo", "test", "--offline"], oracle, env, root / "acceptance.log")
    summary = None
    for line in (root / "run.jsonl").read_text().splitlines():
        try:
            item = json.loads(line)
        except ValueError:
            continue
        if item.get("type") == "summary":
            summary = item
    states = list((project / ".vibe" / "tasks").glob("*/run.json"))
    state = json.loads(states[0].read_text()) if states else {}
    report = {
        "schema_version": 1, "case": args.case, "provider": args.provider,
        "model_override": args.model, "duration_ms": duration_ms,
        "pipeline_exit_code": exit_code, "oracle_exit_code": acceptance,
        "baseline_oracle_exit_code": baseline,
        "success": exit_code == 0 and acceptance == 0,
        "human_interventions": 0, "estimated_cost": None,
        "usage": summary.get("usage") if summary else None,
        "error": summary.get("last_error") if summary else None,
        "pipeline_summary": summary, "validations": state.get("validations", []),
        "validation_attempts": len(state.get("validations", [])),
        "framework": framework_info(vibe),
    }
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
