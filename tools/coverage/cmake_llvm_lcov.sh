#!/usr/bin/env bash
# Build a CMake project with Clang source coverage and export an LCOV tracefile.
# Usage:
#   cmake_llvm_lcov.sh <project-dir> <c|cxx> <output-lcov> <source>...
set -euo pipefail

if [[ $# -lt 4 ]]; then
  echo "usage: $0 <project-dir> <c|cxx> <output-lcov> <source>..." >&2
  exit 2
fi

project_dir=$1
language=$2
output=$3
shift 3

project_dir=$(cd "$project_dir" && pwd)
output=$(python3 - <<'PY' "$output"
import pathlib, sys
print(pathlib.Path(sys.argv[1]).resolve())
PY
)
build_dir="${AIOMSG_COVERAGE_BUILD_DIR:-$project_dir/build-coverage}"
profile_dir="$build_dir/profraw"
profdata="$build_dir/coverage.profdata"

cmake_args=(-S "$project_dir" -B "$build_dir" -DCMAKE_BUILD_TYPE=Debug)
case "$language" in
  c)
    cmake_args+=(
      -DCMAKE_C_COMPILER=clang
      "-DCMAKE_C_FLAGS=-fprofile-instr-generate -fcoverage-mapping -O0 -g"
      "-DCMAKE_EXE_LINKER_FLAGS=-fprofile-instr-generate -fcoverage-mapping"
    )
    ;;
  cxx)
    cmake_args+=(
      -DCMAKE_CXX_COMPILER=clang++
      "-DCMAKE_CXX_FLAGS=-fprofile-instr-generate -fcoverage-mapping -O0 -g"
      "-DCMAKE_EXE_LINKER_FLAGS=-fprofile-instr-generate -fcoverage-mapping"
    )
    ;;
  *)
    echo "unknown language '$language' (expected c or cxx)" >&2
    exit 2
    ;;
esac

rm -rf "$build_dir"
mkdir -p "$profile_dir"
cmake "${cmake_args[@]}"
cmake --build "$build_dir"
LLVM_PROFILE_FILE="$profile_dir/%p.profraw" ctest --test-dir "$build_dir" --output-on-failure

shopt -s nullglob
profiles=("$profile_dir"/*.profraw)
if (( ${#profiles[@]} == 0 )); then
  echo "no LLVM profile data was produced" >&2
  exit 1
fi
llvm-profdata merge -sparse "${profiles[@]}" -o "$profdata"

objects=()
for name in test_protocol test_integration test_ws; do
  if [[ -x "$build_dir/$name" ]]; then
    objects+=("$build_dir/$name")
  fi
done
if (( ${#objects[@]} == 0 )); then
  echo "no test executables found in $build_dir" >&2
  exit 1
fi

object_args=()
for ((i = 1; i < ${#objects[@]}; i++)); do
  object_args+=(-object "${objects[$i]}")
done

sources=()
for source in "$@"; do
  if [[ "$source" = /* ]]; then
    sources+=("$source")
  else
    sources+=("$project_dir/$source")
  fi
done

llvm-cov export -format=lcov "${objects[0]}" "${object_args[@]}" \
  -instr-profile="$profdata" "${sources[@]}" > "$output"

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
