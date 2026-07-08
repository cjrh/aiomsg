#!/usr/bin/env bash
# Build Zig test binaries, run them under kcov, and merge Cobertura output to LCOV.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <project-dir> <output-lcov>" >&2
  exit 2
fi

project_dir=$(cd "$1" && pwd)
output=$(python3 - <<'PY' "$2"
import pathlib, sys
print(pathlib.Path(sys.argv[1]).resolve())
PY
)
repo_root=$(cd "$project_dir/.." && pwd)
build_dir="$project_dir/build-coverage"
kcov_dir="$build_dir/kcov"

if ! command -v kcov >/dev/null 2>&1; then
  echo "error: kcov is required for Zig coverage" >&2
  echo "install it with your package manager (CI uses: apt-get install kcov)" >&2
  exit 127
fi

rm -rf "$build_dir"
mkdir -p "$build_dir" "$kcov_dir"

cat > "$build_dir/build_options.zig" <<EOF
pub const cert_dir: []const u8 = "$repo_root/conformance/certs";
EOF

(
  cd "$project_dir"
  zig test src/protocol.zig --test-no-exec -femit-bin="$build_dir/protocol_test"
  zig test --test-no-exec -femit-bin="$build_dir/aiomsg_test" -lc -lssl -lcrypto \
    --dep build_options -Mroot=src/aiomsg.zig -Mbuild_options="$build_dir/build_options.zig"
)

kcov --include-path="$project_dir/src" "$kcov_dir/protocol" "$build_dir/protocol_test"
kcov --include-path="$project_dir/src" "$kcov_dir/aiomsg" "$build_dir/aiomsg_test"

python3 "$repo_root/tools/coverage/cobertura_to_lcov.py" \
  "$kcov_dir/protocol/coverage.xml" "$build_dir/protocol.lcov" src --repo-root "$repo_root"
python3 "$repo_root/tools/coverage/cobertura_to_lcov.py" \
  "$kcov_dir/aiomsg/coverage.xml" "$build_dir/aiomsg.lcov" src --repo-root "$repo_root"
python3 "$repo_root/tools/coverage/merge_lcov.py" "$output" \
  "$build_dir/protocol.lcov" "$build_dir/aiomsg.lcov"

python3 - <<'PY' "$output"
from pathlib import Path
import sys
covered = total = 0
for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    if line.startswith("LH:"):
        covered += int(line[3:])
    elif line.startswith("LF:"):
        total += int(line[3:])
percent = covered / total * 100 if total else 0
print(f"coverage: {covered}/{total} lines ({percent:.1f}%)")
PY
