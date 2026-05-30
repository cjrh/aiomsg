"""Cross-language conformance suite.

Pairs two language "agents" (see ``agents/`` and each implementation's
conformance agent) over real TCP and asserts they interoperate on the wire.
Each scenario picks a source language+role and a sink language+role; exactly one
side binds. The sink prints what it received; we assert it matches what the
source sent.

Run with:  just test-conformance   (from the repo root)
"""

import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
PYTHON_LIB = REPO_ROOT / "python-lib"
RUST_ASYNC_DIR = REPO_ROOT / "rust-lib-async"
PY_AGENT = REPO_ROOT / "conformance" / "agents" / "python_agent.py"

COUNT = 10
PREFIX = "m"
SOURCE_LINGER = "2.0"

# (source_lang, source_role, sink_lang, sink_role, send_mode, delivery)
SCENARIOS = [
    ("python", "bind", "rust", "connect", "publish", "at-most-once"),
    ("rust", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "connect", "rust", "bind", "roundrobin", "at-most-once"),
    ("rust", "connect", "python", "bind", "roundrobin", "at-most-once"),
    # bind-side source exercises cross-language send buffering (sends before the
    # connecting sink exists).
    ("python", "bind", "rust", "connect", "roundrobin", "at-most-once"),
    ("rust", "bind", "python", "connect", "roundrobin", "at-most-once"),
    # AT_LEAST_ONCE exercises DATA_REQ/ACK interop in both directions.
    ("rust", "connect", "python", "bind", "roundrobin", "at-least-once"),
    ("python", "connect", "rust", "bind", "roundrobin", "at-least-once"),
]


def _scenario_id(s):
    src_lang, src_role, sink_lang, sink_role, mode, delivery = s
    return f"{src_lang}-{src_role}-source__{sink_lang}-{sink_role}-sink__{mode}__{delivery}"


@pytest.fixture(scope="session")
def rust_agent_exe():
    """Build the Rust conformance agent once and return its binary path."""
    if not _have_cargo():
        pytest.skip("cargo not available")
    proc = subprocess.run(
        ["cargo", "build", "--example", "conformance_agent", "--message-format=json"],
        cwd=RUST_ASYNC_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        pytest.fail(f"failed to build rust agent:\n{proc.stderr}")

    exe = None
    for line in proc.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("executable")
            and msg.get("target", {}).get("name") == "conformance_agent"
        ):
            exe = msg["executable"]
    assert exe, "could not locate built conformance_agent binary"
    return exe


def _have_cargo():
    from shutil import which

    return which("cargo") is not None


def _free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _agent_cmd(lang, rust_exe, *, role, behavior, port, send_mode, delivery):
    flags = [
        "--role", role,
        "--host", "127.0.0.1",
        "--port", str(port),
        "--send-mode", send_mode,
        "--behavior", behavior,
        "--count", str(COUNT),
        "--prefix", PREFIX,
        "--delivery", delivery,
        "--linger", SOURCE_LINGER,
    ]
    if lang == "python":
        return [sys.executable, str(PY_AGENT), *flags], _python_env()
    return [rust_exe, *flags], None


def _python_env():
    env = dict(os.environ)
    # aiomsg is stdlib-only, so PYTHONPATH is enough — no install required.
    existing = env.get("PYTHONPATH")
    env["PYTHONPATH"] = str(PYTHON_LIB) + (os.pathsep + existing if existing else "")
    return env


def _launch(cmd_env):
    cmd, env = cmd_env
    return subprocess.Popen(
        cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env
    )


@pytest.mark.parametrize("scenario", SCENARIOS, ids=[_scenario_id(s) for s in SCENARIOS])
def test_interop(rust_agent_exe, scenario):
    src_lang, src_role, sink_lang, sink_role, send_mode, delivery = scenario
    port = _free_port()

    source_cmd = _agent_cmd(
        src_lang, rust_agent_exe,
        role=src_role, behavior="source", port=port,
        send_mode=send_mode, delivery=delivery,
    )
    sink_cmd = _agent_cmd(
        sink_lang, rust_agent_exe,
        role=sink_role, behavior="sink", port=port,
        send_mode=send_mode, delivery=delivery,
    )

    # The binding side must come up first.
    if src_role == "bind":
        first, second = source_cmd, sink_cmd
    else:
        first, second = sink_cmd, source_cmd

    first_proc = _launch(first)
    time.sleep(0.6)
    second_proc = _launch(second)

    source_proc = first_proc if src_role == "bind" else second_proc
    sink_proc = second_proc if src_role == "bind" else first_proc

    try:
        out, err = sink_proc.communicate(timeout=25)
    except subprocess.TimeoutExpired:
        sink_proc.kill()
        out, err = sink_proc.communicate()
        pytest.fail(f"sink timed out; stderr:\n{err}")
    finally:
        for p in (first_proc, second_proc):
            if p.poll() is None:
                p.terminate()
        for p in (first_proc, second_proc):
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()

    received = [line for line in out.splitlines() if line != ""]
    expected = [f"{PREFIX}{i}" for i in range(COUNT)]
    assert received == expected, (
        f"sink received {received!r}, expected {expected!r}; sink stderr:\n{err}"
    )
