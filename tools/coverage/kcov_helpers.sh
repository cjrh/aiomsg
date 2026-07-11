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

# Warn before a coverage uploader turns an empty report into an unhelpful
# remote diagnostic. Kcov can produce no source lines for a supported Zig build;
# the caller remains responsible for deciding whether that is release-blocking.
warn_empty_lcov() {
  local report=$1
  local total
  total=$(awk -F: '$1 == "LF" { sum += $2 } END { print sum + 0 }' "$report")
  if (( total == 0 )); then
    echo "warning: LCOV report has no source lines: $report" >&2
  fi
}
