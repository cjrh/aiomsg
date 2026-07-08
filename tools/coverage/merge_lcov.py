#!/usr/bin/env python3
"""Merge line records from multiple LCOV tracefiles.

This intentionally preserves only source-file and line-hit data. That is enough
for coverage dashboards and avoids fragile function/branch merging semantics
across tools that do not agree on those records.
"""

from __future__ import annotations

import argparse
from pathlib import Path


def read_lcov(path: Path) -> dict[str, dict[int, int]]:
    files: dict[str, dict[int, int]] = {}
    current: str | None = None
    for raw in path.read_text(encoding="utf-8").splitlines():
        if raw.startswith("SF:"):
            current = raw[3:]
            files.setdefault(current, {})
        elif raw.startswith("DA:") and current is not None:
            number_text, hits_text, *_ = raw[3:].split(",")
            number = int(number_text)
            hits = int(float(hits_text))
            lines = files[current]
            lines[number] = max(lines.get(number, 0), hits)
        elif raw == "end_of_record":
            current = None
    return files


def merge(inputs: list[Path]) -> dict[str, dict[int, int]]:
    merged: dict[str, dict[int, int]] = {}
    for path in inputs:
        for source, lines in read_lcov(path).items():
            target = merged.setdefault(source, {})
            for number, hits in lines.items():
                target[number] = max(target.get(number, 0), hits)
    return merged


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
    parser.add_argument("output", type=Path)
    parser.add_argument("inputs", nargs="+", type=Path)
    args = parser.parse_args()
    write_lcov(merge(args.inputs), args.output)


if __name__ == "__main__":
    main()
