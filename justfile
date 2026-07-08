# Top-level dispatch for the multi-language aiomsg repo.
# Each language implementation lives in its own subdirectory with its own
# justfile exposing a uniform `test` recipe. Run `just` to list recipes.

# Show available recipes.
default:
    @just --list

# Run the Python implementation's test suite.
test-python:
    cd python-lib && just test

# Run the blocking Python implementation's test suite.
test-python-blocking:
    cd python-lib-blocking && just test

# Run the async Rust implementation's test suite.
test-rust-async:
    cd rust-lib-async && just test

# Run the sync Rust implementation's test suite.
test-rust-sync:
    cd rust-lib-sync && just test

# Run the Go implementation's test suite.
test-golang:
    cd golang-lib && just test

# Run the C implementation's test suite.
test-c:
    cd c-lib && just test

# Run the sync C++ implementation's test suite.
test-cpp-sync:
    cd cpp-lib-sync && just test

# Run the async C++ implementation's test suite.
test-cpp-async:
    cd cpp-lib-async && just test

# Run the Zig implementation's test suite.
test-zig:
    cd zig-lib && just test

# Run the Java implementation's test suite.
test-java:
    cd java-lib && just test

# Run the JavaScript (Node) implementation's test suite.
test-javascript:
    cd javascript-lib && just test

# Run the C# implementation's test suite.
test-csharp:
    cd csharp-lib && just test

# Run the Lua implementation's test suite.
test-lua:
    cd lua-lib && just test

# Run the cross-language conformance (interop) suite.
test-conformance:
    uv run --project python-lib --group test pytest conformance/ -v

# Syntax-check GitHub Actions workflows without adding PyYAML to any project env.
lint-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    uv run --with pyyaml --no-project python - <<'PY'
    from pathlib import Path
    import yaml

    for path in sorted(Path('.github/workflows').glob('*.yml')):
        with path.open('r', encoding='utf-8') as f:
            yaml.safe_load(f)
        print(f'ok {path}')
    PY

# Build all compiled conformance agents to their conventional local paths.
build-agents:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --manifest-path rust-lib-async/Cargo.toml --example conformance_agent
    cargo build --manifest-path rust-lib-sync/Cargo.toml --example conformance_agent
    (cd golang-lib && mkdir -p build && go build -o build/conformance_agent ./cmd/conformance_agent)
    cmake -B c-lib/build -S c-lib && cmake --build c-lib/build --target conformance_agent
    cmake -B cpp-lib-sync/build -S cpp-lib-sync && cmake --build cpp-lib-sync/build --target conformance_agent
    cmake -B cpp-lib-async/build -S cpp-lib-async && cmake --build cpp-lib-async/build --target conformance_agent
    (cd zig-lib && zig build)
    mkdir -p java-lib/build/classes
    javac -d java-lib/build/classes java-lib/src/main/java/aiomsg/*.java
    dotnet build csharp-lib/ConformanceAgent/ConformanceAgent.csproj -c Release --nologo
    echo "Python, JavaScript, and Lua agents run directly from source."

# Report which conformance-agent entrypoints are ready after `just build-agents`.
agents:
    #!/usr/bin/env bash
    set -euo pipefail
    rc=0
    while IFS='|' read -r lang path; do
        [[ -z "$lang" ]] && continue
        if [[ -e "$path" ]]; then
            printf 'READY   %-11s %s\n' "$lang" "$path"
        else
            printf 'MISSING %-11s %s\n' "$lang" "$path"
            rc=1
        fi
    done <<'EOF'
    python|conformance/agents/python_agent.py
    python-blocking|conformance/agents/python_blocking_agent.py
    rust|rust-lib-async/target/debug/examples/conformance_agent
    rust-sync|rust-lib-sync/target/debug/examples/conformance_agent
    go|golang-lib/build/conformance_agent
    c|c-lib/build/conformance_agent
    cpp-sync|cpp-lib-sync/build/conformance_agent
    cpp-async|cpp-lib-async/build/conformance_agent
    zig|zig-lib/zig-out/bin/conformance_agent
    java|java-lib/build/classes/aiomsg/ConformanceAgent.class
    javascript|javascript-lib/conformance_agent.js
    csharp|csharp-lib/ConformanceAgent/bin/Release/net9.0/conformance_agent
    lua|lua-lib/conformance_agent.lua
    ws|conformance/agents/ws_agent.py
    browser|browser-lib/conformance_agent.js
    EOF
    exit "$rc"

# Run every implementation's test suite that currently exists.
test-all:
    #!/usr/bin/env bash
    set -uo pipefail
    rc=0
    for d in python-lib python-lib-blocking rust-lib-async rust-lib-sync golang-lib c-lib cpp-lib-sync cpp-lib-async zig-lib java-lib javascript-lib csharp-lib lua-lib; do
        if [[ -f "$d/justfile" ]]; then
            echo "=== $d ==="
            (cd "$d" && just test) || rc=1
        fi
    done
    exit $rc
