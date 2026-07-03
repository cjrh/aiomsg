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
PYTHON_BLOCKING_LIB = REPO_ROOT / "python-lib-blocking"
RUST_ASYNC_DIR = REPO_ROOT / "rust-lib-async"
RUST_SYNC_DIR = REPO_ROOT / "rust-lib-sync"
GOLANG_DIR = REPO_ROOT / "golang-lib"
C_DIR = REPO_ROOT / "c-lib"
CPP_SYNC_DIR = REPO_ROOT / "cpp-lib-sync"
CPP_ASYNC_DIR = REPO_ROOT / "cpp-lib-async"
ZIG_DIR = REPO_ROOT / "zig-lib"
JAVA_DIR = REPO_ROOT / "java-lib"
JS_DIR = REPO_ROOT / "javascript-lib"
CSHARP_DIR = REPO_ROOT / "csharp-lib"
LUA_DIR = REPO_ROOT / "lua-lib"
PY_AGENT = REPO_ROOT / "conformance" / "agents" / "python_agent.py"
PY_BLOCKING_AGENT = REPO_ROOT / "conformance" / "agents" / "python_blocking_agent.py"
JS_AGENT = JS_DIR / "conformance_agent.js"
LUA_AGENT = LUA_DIR / "conformance_agent.lua"
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
    # C interop with Python (every role / mode / delivery).
    ("python", "bind", "c", "connect", "roundrobin", "at-most-once"),
    ("c", "bind", "python", "connect", "publish", "at-most-once"),
    ("c", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # C++ (sync) interop with Python.
    ("python", "bind", "cpp-sync", "connect", "roundrobin", "at-most-once"),
    ("cpp-sync", "bind", "python", "connect", "publish", "at-most-once"),
    ("cpp-sync", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # C++ (async) interop with Python.
    ("python", "bind", "cpp-async", "connect", "roundrobin", "at-most-once"),
    ("cpp-async", "bind", "python", "connect", "publish", "at-most-once"),
    ("cpp-async", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # Zig interop with Python.
    ("python", "bind", "zig", "connect", "roundrobin", "at-most-once"),
    ("zig", "bind", "python", "connect", "publish", "at-most-once"),
    ("zig", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # Java interop with Python.
    ("python", "bind", "java", "connect", "roundrobin", "at-most-once"),
    ("java", "bind", "python", "connect", "publish", "at-most-once"),
    ("java", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # JavaScript (Node) interop with Python.
    ("python", "bind", "javascript", "connect", "roundrobin", "at-most-once"),
    ("javascript", "bind", "python", "connect", "publish", "at-most-once"),
    ("javascript", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # C# interop with Python.
    ("python", "bind", "csharp", "connect", "roundrobin", "at-most-once"),
    ("csharp", "bind", "python", "connect", "publish", "at-most-once"),
    ("csharp", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # Lua interop with Python.
    ("python", "bind", "lua", "connect", "roundrobin", "at-most-once"),
    ("lua", "bind", "python", "connect", "publish", "at-most-once"),
    ("lua", "connect", "python", "bind", "roundrobin", "at-least-once"),
    # Cross-native interop (no Python in the loop) across the whole family.
    ("c", "bind", "cpp-async", "connect", "roundrobin", "at-most-once"),
    ("zig", "bind", "java", "connect", "publish", "at-most-once"),
    ("cpp-sync", "connect", "go", "bind", "roundrobin", "at-least-once"),
    ("rust", "bind", "zig", "connect", "roundrobin", "at-most-once"),
    ("java", "connect", "rust-sync", "bind", "roundrobin", "at-least-once"),
    ("javascript", "bind", "go", "connect", "publish", "at-most-once"),
    ("rust", "connect", "javascript", "bind", "roundrobin", "at-least-once"),
    ("csharp", "bind", "javascript", "connect", "roundrobin", "at-most-once"),
    ("go", "connect", "csharp", "bind", "roundrobin", "at-least-once"),
    ("lua", "bind", "rust", "connect", "publish", "at-most-once"),
    ("javascript", "connect", "lua", "bind", "roundrobin", "at-least-once"),
    # Blocking Python interop with async Python and Rust (every role / mode /
    # delivery combination that the other sync ports also cover).
    ("python-blocking", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "python-blocking", "connect", "roundrobin", "at-most-once"),
    ("python-blocking", "connect", "rust", "bind", "roundrobin", "at-most-once"),
    ("rust", "bind", "python-blocking", "connect", "publish", "at-most-once"),
    ("python-blocking", "connect", "python", "bind", "roundrobin", "at-least-once"),
    ("python", "connect", "python-blocking", "bind", "roundrobin", "at-least-once"),
]

# The same matrix, but over TLS — proving every implementation speaks the
# protocol identically whether the transport is plain TCP or rustls/crypto-tls/
# OpenSSL. Covers each language on both the bind (TLS server) and connect (TLS
# client) side, both Rust crates against each other, and one at-least-once path.
LANGUAGES = [
    "python",
    "python-blocking",
    "rust",
    "rust-sync",
    "go",
    "c",
    "cpp-sync",
    "cpp-async",
    "zig",
    "java",
    "javascript",
    "csharp",
    "lua",
]

TLS_SCENARIOS = [
    ("python", "bind", "rust", "connect", "publish", "at-most-once"),
    ("rust", "bind", "python", "connect", "roundrobin", "at-most-once"),
    ("go", "bind", "rust-sync", "connect", "publish", "at-most-once"),
    ("rust-sync", "bind", "go", "connect", "roundrobin", "at-most-once"),
    ("rust", "bind", "rust-sync", "connect", "roundrobin", "at-most-once"),
    ("python", "connect", "go", "bind", "roundrobin", "at-least-once"),
    # The C-family ports + Zig + Java all speak TLS over OpenSSL / JSSE; prove
    # each interoperates with Python over TLS, on both the bind and connect side.
    ("python", "bind", "c", "connect", "publish", "at-most-once"),
    ("cpp-sync", "bind", "python", "connect", "roundrobin", "at-most-once"),
    ("python", "bind", "cpp-async", "connect", "roundrobin", "at-most-once"),
    ("zig", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "java", "connect", "roundrobin", "at-least-once"),
    ("javascript", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "javascript", "connect", "roundrobin", "at-least-once"),
    ("csharp", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "csharp", "connect", "roundrobin", "at-least-once"),
    ("lua", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "lua", "connect", "roundrobin", "at-least-once"),
    # Cross-native TLS interop.
    ("c", "connect", "cpp-async", "bind", "roundrobin", "at-most-once"),
    ("java", "bind", "zig", "connect", "roundrobin", "at-least-once"),
    # Blocking Python speaks TLS via ssl.SSLSocket (not asyncio's MemoryBIO);
    # prove both its bind (TLS server) and connect (TLS client) sides.
    ("python-blocking", "bind", "python", "connect", "publish", "at-most-once"),
    ("python", "bind", "python-blocking", "connect", "roundrobin", "at-least-once"),
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
def go_agent_exe():
    """Build the Go conformance agent once and return its stable local path."""
    if not _have("go"):
        pytest.skip("go not available")
    out = GOLANG_DIR / "build" / "conformance_agent"
    out.parent.mkdir(parents=True, exist_ok=True)
    proc = subprocess.run(
        ["go", "build", "-o", str(out), "./cmd/conformance_agent"],
        cwd=GOLANG_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        pytest.fail(f"failed to build go agent:\n{proc.stderr}")
    return str(out)


def _build_cmake_agent(src_dir):
    """Configure + build a CMake project's conformance_agent; return its path."""
    if not _have("cmake"):
        pytest.skip("cmake not available")
    build = src_dir / "build"
    for cmd in (
        ["cmake", "-B", str(build), "-S", str(src_dir)],
        ["cmake", "--build", str(build), "--target", "conformance_agent"],
    ):
        proc = subprocess.run(cmd, capture_output=True, text=True)
        if proc.returncode != 0:
            pytest.fail(f"{' '.join(cmd)} failed:\n{proc.stdout}\n{proc.stderr}")
    exe = build / "conformance_agent"
    assert exe.exists(), f"agent not found at {exe}"
    return str(exe)


@pytest.fixture(scope="session")
def c_agent_exe():
    return _build_cmake_agent(C_DIR)


@pytest.fixture(scope="session")
def cpp_sync_agent_exe():
    return _build_cmake_agent(CPP_SYNC_DIR)


@pytest.fixture(scope="session")
def cpp_async_agent_exe():
    return _build_cmake_agent(CPP_ASYNC_DIR)


@pytest.fixture(scope="session")
def zig_agent_exe():
    """Build the Zig conformance agent once and return its binary path."""
    if not _have("zig"):
        pytest.skip("zig not available")
    proc = subprocess.run(["zig", "build"], cwd=ZIG_DIR, capture_output=True, text=True)
    if proc.returncode != 0:
        pytest.fail(f"failed to build zig agent:\n{proc.stderr}")
    exe = ZIG_DIR / "zig-out" / "bin" / "conformance_agent"
    assert exe.exists(), f"agent not found at {exe}"
    return str(exe)


@pytest.fixture(scope="session")
def java_agent_cmd():
    """Compile the Java agent with javac; return its `java -cp ...` command."""
    if not _have("javac") or not _have("java"):
        pytest.skip("java not available")
    classes = JAVA_DIR / "build" / "classes"
    classes.mkdir(parents=True, exist_ok=True)
    sources = [str(p) for p in (JAVA_DIR / "src/main/java/aiomsg").glob("*.java")]
    proc = subprocess.run(
        ["javac", "-d", str(classes), *sources], capture_output=True, text=True
    )
    if proc.returncode != 0:
        pytest.fail(f"failed to compile java agent:\n{proc.stderr}")
    return ["java", "-cp", str(classes), "aiomsg.ConformanceAgent"]


@pytest.fixture(scope="session")
def node_agent_cmd():
    """The JavaScript agent runs straight from source under Node — no build or
    install step (the implementation has zero runtime dependencies)."""
    if not _have("node"):
        pytest.skip("node not available")
    return ["node", str(JS_AGENT)]


@pytest.fixture(scope="session")
def csharp_agent_exe():
    """Build the C# conformance agent once with `dotnet build` and return the
    produced native launcher binary."""
    if not _have("dotnet"):
        pytest.skip("dotnet not available")
    project = CSHARP_DIR / "ConformanceAgent" / "ConformanceAgent.csproj"
    proc = subprocess.run(
        ["dotnet", "build", str(project), "-c", "Release", "--nologo"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        pytest.fail(f"failed to build c# agent:\n{proc.stdout}\n{proc.stderr}")
    exe = CSHARP_DIR / "ConformanceAgent" / "bin" / "Release" / "net9.0" / "conformance_agent"
    assert exe.exists(), f"agent not found at {exe}"
    return str(exe)


@pytest.fixture(scope="session")
def lua_agent_cmd():
    """The Lua agent runs straight from source (no build step); it needs the
    `lua` interpreter plus the LuaSocket and LuaSec rocks."""
    if not _have("lua"):
        pytest.skip("lua not available")
    return ["lua", str(LUA_AGENT)]


@pytest.fixture(scope="session")
def agents(
    rust_agent_exe,
    rust_sync_agent_exe,
    go_agent_exe,
    c_agent_exe,
    cpp_sync_agent_exe,
    cpp_async_agent_exe,
    zig_agent_exe,
    java_agent_cmd,
    node_agent_cmd,
    csharp_agent_exe,
    lua_agent_cmd,
):
    """Built native-agent invocations, keyed by SCENARIOS language strings.

    A value is either a binary path (string) or a full command prefix (list,
    e.g. for Java). Keep these keys in lockstep with SCENARIOS; `_agent_cmd`
    handles only `python` and `python-blocking` specially and looks up every
    other language here.
    """
    return {
        "rust": rust_agent_exe,
        "rust-sync": rust_sync_agent_exe,
        "go": go_agent_exe,
        "c": c_agent_exe,
        "cpp-sync": cpp_sync_agent_exe,
        "cpp-async": cpp_async_agent_exe,
        "zig": zig_agent_exe,
        "java": java_agent_cmd,
        "javascript": node_agent_cmd,
        "csharp": csharp_agent_exe,
        "lua": lua_agent_cmd,
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
        return [sys.executable, str(PY_AGENT), *flags], _python_env(PYTHON_LIB)
    if lang == "python-blocking":
        return (
            [sys.executable, str(PY_BLOCKING_AGENT), *flags],
            _python_env(PYTHON_BLOCKING_LIB),
        )
    # A native agent is either a binary path (str) or a command prefix (list).
    invocation = exes[lang]
    prefix = invocation if isinstance(invocation, list) else [invocation]
    return [*prefix, *flags], None


def _python_env(lib_dir):
    env = dict(os.environ)
    # Both Python packages are stdlib-only, so PYTHONPATH is enough — no
    # install required.
    existing = env.get("PYTHONPATH")
    env["PYTHONPATH"] = str(lib_dir) + (os.pathsep + existing if existing else "")
    return env


def _launch(cmd_env):
    cmd, env = cmd_env
    return subprocess.Popen(
        cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env
    )


RAW_IDENTITY = b"raw-conformance!"[:16]
T_HELLO = 0x01
T_HEARTBEAT = 0x02
T_DATA = 0x03
T_DATA_REQ = 0x04
T_ACK = 0x05


def _raw_frame(envelope):
    return len(envelope).to_bytes(4, "big") + envelope


def _raw_hello():
    return _raw_frame(bytes((T_HELLO, 0x01)) + RAW_IDENTITY)


def _raw_data(payload):
    return _raw_frame(bytes((T_DATA,)) + payload)


def _raw_ack(msg_id):
    return _raw_frame(bytes((T_ACK,)) + msg_id)


def _read_exact(sock, n):
    chunks = []
    remaining = n
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _raw_read_frame(sock):
    header = _read_exact(sock, 4)
    if header is None:
        return None
    return _read_exact(sock, int.from_bytes(header, "big"))


def _expect_raw_hello(sock):
    envelope = _raw_read_frame(sock)
    assert envelope is not None, "peer closed before HELLO"
    assert len(envelope) == 18 and envelope[0] == T_HELLO and envelope[1] == 0x01


def _send_pipelined_hello_and_data(sock):
    frames = [_raw_hello()]
    frames.extend(_raw_data(f"{PREFIX}{i}".encode()) for i in range(COUNT))
    # One send intentionally leaves DATA frames buffered behind HELLO in peers
    # whose handshake reader consumes more than one frame from the socket.
    sock.sendall(b"".join(frames))
    _expect_raw_hello(sock)
    time.sleep(0.2)


def _collect_raw_payloads(sock, count):
    received = []
    while len(received) < count:
        envelope = _raw_read_frame(sock)
        assert envelope is not None, f"peer closed after {len(received)} messages"
        if not envelope:
            continue
        typ = envelope[0]
        if typ == T_DATA:
            received.append(envelope[1:].decode())
        elif typ == T_DATA_REQ and len(envelope) >= 17:
            msg_id = envelope[1:17]
            received.append(envelope[17:].decode())
            sock.sendall(_raw_ack(msg_id))
        elif typ in (T_HELLO, T_HEARTBEAT, T_ACK):
            continue
    return received


def _terminate(proc):
    if proc is None:
        return ""
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    return proc.stderr.read() if proc.stderr is not None else ""


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


@pytest.mark.parametrize("sink_lang", LANGUAGES)
@pytest.mark.parametrize("sink_role", ["bind", "connect"])
def test_pipelined_hello_data_reaches_sink(agents, sink_lang, sink_role):
    """Every implementation must drain decoder-buffered post-HELLO frames.

    The raw peer sends HELLO and all DATA frames in one TCP write. Implementations
    whose handshake reader buffers beyond HELLO must forward those complete DATA
    frames before blocking for fresh socket bytes.
    """
    port = _free_port()
    sink_cmd = _agent_cmd(
        sink_lang, agents,
        role=sink_role, behavior="sink", port=port,
        send_mode="roundrobin", delivery="at-most-once",
    )
    sink_proc = None
    server = None
    try:
        if sink_role == "bind":
            sink_proc = _launch(sink_cmd)
            time.sleep(0.6)
            with socket.create_connection(("127.0.0.1", port), timeout=10) as raw:
                raw.settimeout(10)
                _send_pipelined_hello_and_data(raw)
        else:
            server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            server.bind(("127.0.0.1", port))
            server.listen(1)
            server.settimeout(10)
            sink_proc = _launch(sink_cmd)
            raw, _ = server.accept()
            with raw:
                raw.settimeout(10)
                _send_pipelined_hello_and_data(raw)

        try:
            out, err = sink_proc.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            # Some agents keep background runtime work alive even after the sink
            # printed all expected lines. This test is about delivery, so kill
            # after a short grace period and evaluate captured stdout.
            sink_proc.kill()
            out, err = sink_proc.communicate()
        received = [line for line in out.splitlines() if line]
    finally:
        if server is not None:
            server.close()
        if sink_proc is not None and sink_proc.poll() is None:
            err = _terminate(sink_proc)

    expected = [f"{PREFIX}{i}" for i in range(COUNT)]
    assert received == expected, (
        f"{sink_lang} {sink_role} sink received {received!r}, expected {expected!r}; "
        f"stderr:\n{err}"
    )


@pytest.mark.parametrize("source_lang", LANGUAGES)
def test_connect_source_buffers_until_raw_peer_appears(agents, source_lang):
    """A connect-end source must queue sends made before a peer exists."""
    port = _free_port()
    source_cmd = _agent_cmd(
        source_lang, agents,
        role="connect", behavior="source", port=port,
        send_mode="roundrobin", delivery="at-most-once",
    )
    source_proc = _launch(source_cmd)
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        # Give the source a window to call send() while no listener exists.
        time.sleep(0.6)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("127.0.0.1", port))
        server.listen(1)
        server.settimeout(10)
        raw, _ = server.accept()
        with raw:
            raw.settimeout(10)
            raw.sendall(_raw_hello())
            _expect_raw_hello(raw)
            received = _collect_raw_payloads(raw, COUNT)

    finally:
        server.close()
        err = _terminate(source_proc)

    expected = [f"{PREFIX}{i}" for i in range(COUNT)]
    assert received == expected, (
        f"{source_lang} connect source sent {received!r}, expected {expected!r}; "
        f"stderr:\n{err}"
    )
