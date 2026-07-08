#!/usr/bin/env python3
"""Convert JaCoCo XML to a minimal LCOV tracefile.

JaCoCo reports source files by Java package plus file name. This script maps
those entries back to source roots and emits line records only for files under
explicitly included roots.
"""

from __future__ import annotations

import argparse
import xml.etree.ElementTree as ET
from pathlib import Path


def repo_relative(path: Path, repo: Path) -> str:
    try:
        return path.resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        return path.resolve().as_posix()


def is_under(path: Path, roots: list[Path]) -> bool:
    if not roots:
        return True
    resolved = path.resolve()
    return any(resolved == root or root in resolved.parents for root in roots)


def source_path(source_root: Path, package_name: str, file_name: str) -> Path:
    package_dir = Path(*package_name.split("/")) if package_name else Path()
    return (source_root / package_dir / file_name).resolve()


def convert(xml_path: Path, source_root: Path, include_roots: list[Path], repo: Path) -> dict[str, dict[int, int]]:
    tree = ET.parse(xml_path)
    files: dict[str, dict[int, int]] = {}
    for package in tree.findall("package"):
        package_name = package.get("name", "")
        for sourcefile in package.findall("sourcefile"):
            path = source_path(source_root, package_name, sourcefile.get("name", ""))
            if not is_under(path, include_roots):
                continue
            lines = files.setdefault(repo_relative(path, repo), {})
            for line in sourcefile.findall("line"):
                number = int(line.get("nr", "0"))
                covered_instructions = int(line.get("ci", "0"))
                missed_instructions = int(line.get("mi", "0"))
                if covered_instructions == 0 and missed_instructions == 0:
                    continue
                lines[number] = max(lines.get(number, 0), covered_instructions)
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
    parser.add_argument("xml", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("source_root", type=Path)
    parser.add_argument("include_roots", nargs="*", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd().parent)
    args = parser.parse_args()
    source_root = args.source_root.resolve()
    include_roots = [root.resolve() for root in args.include_roots]
    write_lcov(convert(args.xml, source_root, include_roots, args.repo_root.resolve()), args.output)


if __name__ == "__main__":
    main()
