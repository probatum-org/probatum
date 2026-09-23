#!/usr/bin/env python3
"""Assert the schema-4 scenario report emitted by the binary in probatum.toml.

Expected fields come from each acceptance check; this also checks the evidence
copy and the distinction between excluded, skipped, and evaluated checks.
"""
import json
from pathlib import Path
import shlex
import subprocess
import sys

expected = json.loads(sys.argv[1])
report = json.load(sys.stdin)  # rejects stray stdout or more than one document
assert report["schema"] == 4, report
assert report["host_os"] == {"darwin": "macos"}.get(sys.platform, sys.platform)
order = list(dict.fromkeys(c["scenario"] for c in report["checks"]))
if "scenario_order" in expected:
    assert order == expected.pop("scenario_order"), order
for key, value in expected.items():
    assert report[key] == value, (key, report[key], value)

checks = report["checks"]
assert report["failed"] == sum(c["status"] == "Failed" for c in checks)
assert report["errored"] == sum(c["status"] == "Errored" for c in checks)
assert report["excluded"] == sum(c["status"] == "Excluded" for c in checks)
assert report["skipped"] == sum(c["status"] == "Skipped" for c in checks)
assert report["executed"] == len(checks) - report["excluded"] - report["skipped"]
for check in checks:
    if check["status"] in ("Excluded", "Skipped"):
        assert check["duration_ms"] == 0 and check["log_file"] is None, check
        assert check["detail"], check
        if check["status"] == "Skipped":
            assert check["reason"] in ("previous_failure", "dependency_unavailable"), check
        elif (report["selected_scenario"] not in (None, check["scenario"])
              and check["scenario"] not in report["prerequisites"]):
            assert check["reason"] == "not_selected", check
        else:
            assert check["reason"] == "os_mismatch", check
    else:
        assert check["reason"] is None and Path(check["log_file"]).is_file(), check

run_dir = Path(report["run_dir"])
assert json.loads((run_dir / "run.json").read_text()) == report
assert (run_dir / "config.toml").is_file()
# No empty evidence files for checks which did not execute.
assert len(list(run_dir.glob("check-*.log"))) == report["executed"]
replay = shlex.split(report["replay"])
expected_replay = ["probatum", "run", str(run_dir / "config.toml"), "--seed", str(report["seed"])]
if report["selected_scenario"] is not None:
    expected_replay += ["--scenario", report["selected_scenario"]]
assert replay == expected_replay, replay
if "--replay" in sys.argv[2:]:
    replay[0] = "./target/debug/probatum"
    result = subprocess.run(replay + ["--json"], capture_output=True, text=True, timeout=15)
    assert result.returncode == 0, result.stderr
    again = json.loads(result.stdout)
    for key in ("schema", "selected_scenario", "verdict", "executed", "excluded", "seed"):
        assert again[key] == report[key], (key, again, report)
    assert [(c["scenario"], c["status"]) for c in again["checks"]] == [
        (c["scenario"], c["status"]) for c in checks
    ]
print("scenario report verified")
