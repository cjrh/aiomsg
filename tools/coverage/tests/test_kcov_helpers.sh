#!/usr/bin/env bash
# Fixture tests for kcov's supported Cobertura layouts and LCOV sanity guard.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
# shellcheck source=../kcov_helpers.sh
source "$repo_root/tools/coverage/kcov_helpers.sh"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

assert_eq() {
  [[ "$1" == "$2" ]] || { echo "expected $2, got $1" >&2; exit 1; }
}

mkdir -p "$work/direct"
touch "$work/direct/cobertura.xml"
assert_eq "$(kcov_cobertura_xml "$work/direct")" "$work/direct/cobertura.xml"

mkdir -p "$work/hashed/2c1f"
touch "$work/hashed/2c1f/cov.xml"
ln -s "2c1f" "$work/hashed/latest"
assert_eq "$(kcov_cobertura_xml "$work/hashed")" "$work/hashed/2c1f/cov.xml"

mkdir -p "$work/merged/kcov-merged"
touch "$work/merged/kcov-merged/coverage.xml"
assert_eq "$(kcov_cobertura_xml "$work/merged")" "$work/merged/kcov-merged/coverage.xml"

printf 'TN:\nSF:src/example.zig\nDA:1,1\nLH:1\nLF:1\nend_of_record\n' > "$work/nonempty.lcov"
[[ -z "$(warn_empty_lcov "$work/nonempty.lcov" 2>&1)" ]]
printf 'TN:\nLH:0\nLF:0\nend_of_record\n' > "$work/empty.lcov"
warning=$(warn_empty_lcov "$work/empty.lcov" 2>&1)
[[ "$warning" == *"LCOV report has no source lines"* ]] || {
  echo "empty LCOV did not produce the expected warning" >&2
  exit 1
}

echo "kcov helper fixtures passed"
