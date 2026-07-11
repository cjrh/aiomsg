#!/usr/bin/env bash
# Shared kcov output checks. Kept separate from the Zig runner so fixtures can
# exercise distro-specific output layouts without installing Zig or kcov.

# Print the first Cobertura XML produced below a kcov output directory.
# Kcov versions have used direct files, hashed child directories (sometimes
# reached through a friendly symlink), and kcov-merged as their output layout.
kcov_cobertura_xml() {
  local dir=$1
  local name candidate

  for name in cobertura.xml cov.xml coverage.xml; do
    if [[ -f "$dir/$name" ]]; then
      printf '%s\n' "$dir/$name"
      return 0
    fi
    while IFS= read -r candidate; do
      printf '%s\n' "$candidate"
      return 0
    done < <(find "$dir" -type f -name "$name" ! -path '*/kcov-merged/*' -print)
  done

  for name in cobertura.xml cov.xml coverage.xml; do
    while IFS= read -r candidate; do
      printf '%s\n' "$candidate"
      return 0
    done < <(find "$dir" -type f -name "$name" -print)
  done

  echo "error: kcov did not write a Cobertura XML file in $dir" >&2
  find "$dir" -maxdepth 3 \( -type f -o -type l \) -print >&2 || true
  return 1
}

# Reject an LCOV report with no measurable source lines before an uploader turns
# it into an unhelpful remote "Nothing to report" diagnostic.
require_lcov_lines() {
  local report=$1
  local total
  total=$(awk -F: '$1 == "LF" { sum += $2 } END { print sum + 0 }' "$report")
  if (( total == 0 )); then
    echo "error: LCOV report has no source lines: $report" >&2
    return 1
  fi
}
