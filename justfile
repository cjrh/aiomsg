# Top-level dispatch for the multi-language aiomsg repo.
# Each language implementation lives in its own subdirectory with its own
# justfile exposing a uniform `test` recipe. Run `just` to list recipes.

# Show available recipes.
default:
    @just --list

# Run the Python implementation's test suite.
test-python:
    cd python-lib && just test

# Run the async Rust implementation's test suite.
test-rust-async:
    cd rust-lib-async && just test

# Run the sync Rust implementation's test suite.
test-rust-sync:
    cd rust-lib-sync && just test

# Run the Go implementation's test suite.
test-golang:
    cd golang-lib && just test

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

# Run every implementation's test suite that currently exists.
test-all:
    #!/usr/bin/env bash
    set -uo pipefail
    rc=0
    for d in python-lib rust-lib-async rust-lib-sync golang-lib javascript-lib csharp-lib lua-lib; do
        if [[ -f "$d/justfile" ]]; then
            echo "=== $d ==="
            (cd "$d" && just test) || rc=1
        fi
    done
    exit $rc
