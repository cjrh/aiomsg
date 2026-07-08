#!/usr/bin/env python3
"""Convert a Go coverage profile to a minimal LCOV tracefile.

Go's coverprofile format records statement blocks. LCOV records line hits, so
this converter marks every source line touched by a covered block as covered.
That keeps the exported report useful for dashboards without introducing a Go
specific uploader into each CI job.
"""

from __future__ import annotations

import argparse
from pathlib import Path


def repo_relative(path: Path, repo: Path) -> str:
    try:
        return path.resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        return path.as_posix()


def resolve_source(raw: str, cwd: Path, repo: Path) -> Path:
    path = Path(raw)
    if path.is_absolute():
        return path
    direct = cwd / path
    if direct.exists():
        return direct
    parts = path.parts
    for index in range(len(parts)):
        suffix = Path(*parts[index:])
        for base in (cwd, repo):
            candidate = base / suffix
            if candidate.exists():
                return candidate
    return direct


def add_block(lines: dict[int, int], start: int, end: int, count: int) -> None:
    for line in range(start, end + 1):
        lines[line] = max(lines.get(line, 0), count)


def convert(profile: Path, repo: Path) -> dict[str, dict[int, int]]:
    files: dict[str, dict[int, int]] = {}
    cwd = Path.cwd().resolve()
    for raw in profile.read_text(encoding="utf-8").splitlines():
        if not raw or raw.startswith("mode:"):
            continue
        location, _statements, count_text = raw.split()
        file_name, span = location.split(":", 1)
        start, end = span.split(",", 1)
        start_line = int(start.split(".", 1)[0])
        end_line = int(end.split(".", 1)[0])
        count = int(count_text)
        source = resolve_source(file_name, cwd, repo)
        add_block(files.setdefault(repo_relative(source, repo), {}), start_line, end_line, count)
    return files


def write_lcov(files: dict[str, dict[int, int]], output: Path) -> None:
    with output.open("w", encoding="utf-8") as out:
        for source in sorted(files):
            lines = files[source]
            covered = sum(1 for hits in lines.values() if hits > 0)
            out.write(f"SF:{source}\n")
            for line in sorted(lines):
                out.write(f"DA:{line},{lines[line]}\n")
            out.write(f"LH:{covered}\n")
            out.write(f"LF:{len(lines)}\n")
            out.write("end_of_record\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("profile", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd().parent)
    args = parser.parse_args()
    write_lcov(convert(args.profile, args.repo_root.resolve()), args.output)


if __name__ == "__main__":
    main()
