#!/usr/bin/env python3
"""Public-binary regressions for captured outputs, called by probatum.toml."""
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time
from urllib.request import urlopen

SAMPLE = " spaces ' \" \\ 雪\n$(printf injected); echo bad "
WITHHELD = "[output withheld: scenario captures or consumes values]\n"
BINARY = "./target/debug/probatum"


def invoke(args, code=0, config=None):
    result = subprocess.run([BINARY, "run", *args, "--json"], input=config,
                            text=True, capture_output=True, timeout=15)
    assert result.returncode == code, (result.returncode, code, result.stdout, result.stderr)
    report = json.loads(result.stdout)
    assert report["schema"] == 4, report
    if "run_dir" in report:
        directory = Path(report["run_dir"])
        assert json.loads((directory / "run.json").read_text()) == report
        for check in report["checks"]:
            if check["status"] in ("Skipped", "Excluded"):
                assert check["log_file"] is None and check["duration_ms"] == 0, check
                assert not check["captures"], check
            if check["output_withheld"]:
                assert check["cause"] is None, check
                assert Path(check["log_file"]).read_text() == WITHHELD, check
        assert len(list(directory.glob("check-*.log"))) == report["executed"], report
    return report


def http_checks():
    logfile = Path("demo-app/data/app.log")
    def once(args):
        before = logfile.read_text().count("auth login ok")
        report = invoke(args)
        assert logfile.read_text().count("auth login ok") == before + 1
        assert sum(c["scenario"] == "login" for c in report["checks"]) == 1
        assert all(c["output_withheld"] for c in report["checks"] if c["status"] == "Passed")
        login = next(c for c in report["checks"] if c["scenario"] == "login")
        assert login["captures"] == ["token"] and login["step"] is None
        return report

    fixture = ".probatum/capture-http-scenarios.toml"
    report = once([fixture])
    assert [c["scenario"] for c in report["checks"]] == ["login", "profile", "permissions"]
    assert report["executed"] == 3 and report["excluded"] == 0
    for scenario in ("profile", "permissions"):
        selected = once([fixture, "--scenario", scenario])
        assert selected["executed"] == 2 and selected["excluded"] == 1
        assert selected["prerequisites"] == ["login"]
        assert selected["selected_scenario"] == scenario
        replay = shlex.split(selected["replay"])
        again = once(replay[2:])
        assert again["seed"] == selected["seed"]
        assert again["prerequisites"] == ["login"]

    # JSON substitution preserves a number's type and quotes string contents safely.
    report = invoke(["-"], config='''
[post]
post = "http://127.0.0.1:8087/api/events"
headers = { Authorization = "Bearer ${login.token}" }
body = '{"key":"${text.value}","value":"${version.count}"}'
expect = 200
[text]
run = "python3 demo-app/tests/captures.py emit"
capture = { value = "stdout" }
[version]
get = "http://127.0.0.1:8087/api/version"
capture = { count = "json.keys" }
[login]
post = "http://127.0.0.1:8087/auth/login"
body = '{"username":"editor","password":"hunter2"}'
capture = { token = "json.access_token" }
''')
    assert report["executed"] == 4
    print("capture HTTP checks verified")


def command_checks():
    report = invoke([".probatum/capture-stdout.toml", "--scenario", "consumer"])
    assert report["prerequisites"] == ["quoted producer"]
    assert [(c["scenario"], c["step"]) for c in report["checks"]] == [
        ("quoted producer", 2), ("consumer", 1), ("consumer", 10)]
    assert [c["captures"] for c in report["checks"]] == [["value"], ["copy"], []]
    assert report["executed"] == 3
    assert all(c["output_withheld"] for c in report["checks"])
    print("capture command checks verified")


def negative_checks():
    missing = invoke(["-"], 1, '''
[consumer]
run = "exit 91"
env = { TOKEN = "${login.token}" }
[login]
post = "http://127.0.0.1:8087/auth/login"
body = '{"username":"editor","password":"hunter2"}'
capture = { token = "json.missing" }
''')
    assert missing["failed"] == 1 and missing["skipped"] == 1
    assert "missing JSON field" in missing["checks"][0]["detail"]
    assert missing["checks"][0]["captures"] == []

    for command, code in [("true", 1), ("printf value; exit 9", 1),
                          ("python3 -c 'print(\"x\" * 1048577)'", 2),
                          ("python3 -c 'import sys; sys.stdout.buffer.write(bytes([255]))'", 2)]:
        config = f'''[consumer]
run = "exit 91"
env = {{ VALUE = "${{producer.value}}" }}
[producer]
run = {json.dumps(command)}
capture = {{ value = "stdout" }}
'''
        report = invoke(["-"], code, config)
        assert report["executed"] == 1 and report["skipped"] == 1
        assert not report["checks"][0]["captures"]

    for config in [
        "[x]\nrun='exit 91'\nenv={X='${unknown.token}'}",
        "[x]\nrun='exit 91'\ncapture={v='stdout'}\nenv={X='${y.v}'}\n[y]\nrun='exit 92'\ncapture={v='stdout'}\nenv={Y='${x.v}'}",
        "[ok]\nrun='exit 91'\n[outside]\nos='windows'\nrun='true'\nenv={X='${unknown.token}'}",
    ]:
        report = invoke(["-"], 2, config)
        assert report["error"]["kind"] == "invalid_config" and "run_dir" not in report

    # No response body or echoed secret survives a timeout in persisted evidence.
    private_failure = invoke(["-"], 1, '''
[x.1]
run = "python3 demo-app/tests/captures.py emit"
capture = { value = "stdout" }
[x.2]
run = 'printf "%s" "$VALUE"; sleep 5'
env = { VALUE = "${x.1.value}" }
timeout = 1
''')
    assert private_failure["failed"] == 1

    # Withheld means payloads, not diagnostics: probatum's own detail survives,
    # with every captured value redacted.
    leak = invoke(["-"], 1, '''
[x.1]
run = "python3 demo-app/tests/captures.py emit"
capture = { value = "stdout" }
[x.2]
run = "true"
contains = ["${x.1.value} and more"]
''')
    assert leak["checks"][1]["detail"] == 'output missing "[redacted] and more"', leak
    assert SAMPLE.strip() not in json.dumps(leak, ensure_ascii=False)
    unreachable = invoke(["-"], 2, '''
[x.1]
run = "python3 demo-app/tests/captures.py emit"
capture = { value = "stdout" }
[x.2]
get = "http://127.0.0.1:9/${x.1.value}"
''')
    assert "nothing answered" in unreachable["checks"][1]["detail"], unreachable
    print("capture negative checks verified")


def scope_checks():
    config = '''
[consumer]
run = "exit 91"
env = { X = "${foreign.x}" }
[foreign]
os = "windows"
run = "exit 92"
capture = { x = "stdout" }
[independent]
run = "true"
'''
    for args in (["-"], ["-", "--scenario", "consumer"]):
        report = invoke(args, 2, config)
        consumer = next(c for c in report["checks"] if c["scenario"] == "consumer")
        assert consumer["status"] == "Skipped" and consumer["reason"] == "dependency_unavailable"
        assert report["reason"] == "dependency_unavailable" and report["failed"] == 0
    excluded = invoke(["-", "--scenario", "consumer"], 2, '''
[consumer]
os = "windows"
run = "exit 91"
env = { X = "${producer.x}" }
[producer]
run = "exit 92"
capture = { x = "stdout" }
''')
    assert excluded["executed"] == 0 and excluded["prerequisites"] == []
    assert excluded["reason"] == "no_applicable_checks"
    print("capture scope checks verified")


def ownership_checks():
    config = '''
[private.1]
run = "python3 demo-app/tests/captures.py emit"
capture = { value = "stdout" }
[private.2]
run = 'printf "%s" "$VALUE"; exec python3 demo-app/app.py'
env = { VALUE = "${private.1.value}" }
ready = "http://127.0.0.1:8087/healthz"
timeout = 5
'''
    for interruption in ("panic", signal.SIGINT, signal.SIGTERM):
        before = set(Path(".probatum/runs").iterdir())
        env = dict(os.environ)
        text = config
        if interruption == "panic":
            env["PROBATUM_TEST_PANIC"] = "1"
        else:
            text += '\n[private.3]\nrun = "sleep 30"\ntimeout = 35\n'
        process = subprocess.Popen([BINARY, "run", "-", "--json"], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, env=env)
        try:
            process.stdin.write(text)
            process.stdin.close()
            process.stdin = None
            if interruption != "panic":
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    assert process.poll() is None, "runner exited before signal"
                    try:
                        with urlopen("http://127.0.0.1:8087/healthz", timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise AssertionError("service never became ready")
                process.send_signal(interruption)
            stdout, stderr = process.communicate(timeout=10)
            expected = 101 if interruption == "panic" else 128 + interruption
            assert process.returncode == expected, (process.returncode, stdout, stderr)
            assert SAMPLE not in stdout + stderr
            if interruption == "panic":
                assert json.loads(stdout)["error"]["kind"] == "internal_error"
            else:
                assert not stdout
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
        created = set(Path(".probatum/runs").iterdir()) - before
        assert len(created) == 1, created
        logs = list(created.pop().glob("check-*.log"))
        assert len(logs) >= 2 and all(path.read_text() == WITHHELD for path in logs)
        probe = subprocess.run([sys.executable, "demo-app/tests/portfree.py"],
                               capture_output=True, text=True)
        assert probe.returncode == 0, probe.stdout + probe.stderr
    print("capture ownership checks verified")


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "emit":
        print("stderr must not be captured", file=sys.stderr)
        print(SAMPLE)
    elif mode == "verify":
        assert os.environ["VALUE"] == SAMPLE
        print(SAMPLE)  # Deliberate secret echo; evidence must withhold it.
        print("ok.")
    else:
        {"http": http_checks, "command": command_checks,
         "negative": negative_checks, "scope": scope_checks,
         "ownership": ownership_checks}[mode]()
