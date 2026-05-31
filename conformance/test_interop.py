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
RUST_SYNC_DIR = REPO_ROOT / "rust-lib-sync"
GOLANG_DIR = REPO_ROOT / "golang-lib"
PY_AGENT = REPO_ROOT / "conformance" / "agents" / "python_agent.py"
# Shared self-signed cert (regenerate with rust-lib-async's gen_test_certs
# example). It is its own trust anchor, so the same file is both cert and CA.
CERTS = REPO_ROOT / "conformance" / "certs"
TLS_CERT = CERTS / "cert.pem"
TLS_KEY = CERTS / "key.pem"

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
    # Go interop with both Python and Rust.
    ("go", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "go", "connect", "roundrobin", "at-most-once"),
    ("go", "connect", "rust", "bind", "roundrobin", "at-most-once"),
    ("rust", "bind", "go", "connect", "publish", "at-most-once"),
    ("go", "connect", "python", "bind", "roundrobin", "at-least-once"),
    ("python", "connect", "go", "bind", "roundrobin", "at-least-once"),
    ("go", "connect", "rust", "bind", "roundrobin", "at-least-once"),
    # Synchronous Rust interop with every other implementation.
    ("rust-sync", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "rust-sync", "connect", "roundrobin", "at-most-once"),
    ("rust-sync", "connect", "rust", "bind", "roundrobin", "at-most-once"),
    ("go", "bind", "rust-sync", "connect", "publish", "at-most-once"),
    ("rust-sync", "connect", "python", "bind", "roundrobin", "at-least-once"),
]

# The same matrix, but over TLS — proving every implementation speaks the
# protocol identically whether the transport is plain TCP or rustls/crypto-tls/
# OpenSSL. Covers each language on both the bind (TLS server) and connect (TLS
# client) side, both Rust crates against each other, and one at-least-once path.
TLS_SCENARIOS = [
    ("python", "bind", "rust", "connect", "publish", "at-most-once"),
    ("rust", "bind", "python", "connect", "roundrobin", "at-most-once"),
    ("go", "bind", "rust-sync", "connect", "publish", "at-most-once"),
    ("rust-sync", "bind", "go", "connect", "roundrobin", "at-most-once"),
    ("rust", "bind", "rust-sync", "connect", "roundrobin", "at-most-once"),
    ("python", "connect", "go", "bind", "roundrobin", "at-least-once"),
]

# Each parametrized case is (scenario tuple, tls flag).
ALL_SCENARIOS = [(s, False) for s in SCENARIOS] + [(s, True) for s in TLS_SCENARIOS]


def _scenario_id(case):
    s, tls = case
    src_lang, src_role, sink_lang, sink_role, mode, delivery = s
    transport = "tls" if tls else "tcp"
    return (
        f"{src_lang}-{src_role}-source__{sink_lang}-{sink_role}-sink"
        f"__{mode}__{delivery}__{transport}"
    )


def _build_cargo_example(crate_dir):
    """Build a crate's conformance_agent example and return its binary path."""
    if not _have("cargo"):
        pytest.skip("cargo not available")
    proc = subprocess.run(
        ["cargo", "build", "--example", "conformance_agent", "--message-format=json"],
        cwd=crate_dir,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        pytest.fail(f"failed to build rust agent in {crate_dir}:\n{proc.stderr}")

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
    assert exe, f"could not locate built conformance_agent binary in {crate_dir}"
    return exe


@pytest.fixture(scope="session")
def rust_agent_exe():
    return _build_cargo_example(RUST_ASYNC_DIR)


@pytest.fixture(scope="session")
def rust_sync_agent_exe():
    return _build_cargo_example(RUST_SYNC_DIR)


@pytest.fixture(scope="session")
def go_agent_exe(tmp_path_factory):
    """Build the Go conformance agent once and return its binary path."""
    if not _have("go"):
        pytest.skip("go not available")
    out = tmp_path_factory.mktemp("go-agent") / "conformance_agent"
    proc = subprocess.run(
        ["go", "build", "-o", str(out), "./cmd/conformance_agent"],
        cwd=GOLANG_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        pytest.fail(f"failed to build go agent:\n{proc.stderr}")
    return str(out)


@pytest.fixture(scope="session")
def agents(rust_agent_exe, rust_sync_agent_exe, go_agent_exe):
    """Built native-agent binaries, keyed by language."""
    return {
        "rust": rust_agent_exe,
        "rust-sync": rust_sync_agent_exe,
        "go": go_agent_exe,
    }


def _have(name):
    from shutil import which

    return which(name) is not None


def _free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _agent_cmd(lang, exes, *, role, behavior, port, send_mode, delivery, tls=False):
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
    if tls:
        # The cert is its own trust anchor: it is both the server cert (bind
        # side) and the trusted CA (connect side). Each agent uses the flags
        # relevant to its role; the server name defaults to the host (the cert
        # has an IP SAN for 127.0.0.1).
        flags += [
            "--tls", "true",
            "--tls-cert", str(TLS_CERT),
            "--tls-key", str(TLS_KEY),
            "--tls-ca", str(TLS_CERT),
        ]
    if lang == "python":
        return [sys.executable, str(PY_AGENT), *flags], _python_env()
    return [exes[lang], *flags], None


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


@pytest.mark.parametrize(
    "case", ALL_SCENARIOS, ids=[_scenario_id(c) for c in ALL_SCENARIOS]
)
def test_interop(agents, case):
    scenario, tls = case
    src_lang, src_role, sink_lang, sink_role, send_mode, delivery = scenario
    if tls and not TLS_CERT.exists():
        pytest.skip("shared test cert missing; run the gen_test_certs example")
    port = _free_port()

    source_cmd = _agent_cmd(
        src_lang, agents,
        role=src_role, behavior="source", port=port,
        send_mode=send_mode, delivery=delivery, tls=tls,
    )
    sink_cmd = _agent_cmd(
        sink_lang, agents,
        role=sink_role, behavior="sink", port=port,
        send_mode=send_mode, delivery=delivery, tls=tls,
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
