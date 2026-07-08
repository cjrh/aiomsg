#!/usr/bin/env python3
"""Convert Cobertura XML to a minimal LCOV tracefile.

The converter intentionally filters by source roots supplied on the command
line. That keeps auxiliary executables, examples, and test assemblies out of the
implementation coverage number while still allowing tools such as
`dotnet-coverage` and `kcov` to emit their native Cobertura XML.
"""

from __future__ import annotations

import argparse
import xml.etree.ElementTree as ET
from pathlib import Path


def resolve_path(raw: str, cwd: Path) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        path = cwd / path
    return path.resolve()


def is_under(path: Path, roots: list[Path]) -> bool:
    if not roots:
        return True
    return any(path == root or root in path.parents for root in roots)


def repo_relative(path: Path, repo: Path) -> str:
    try:
        return path.relative_to(repo).as_posix()
    except ValueError:
        return path.as_posix()


def convert(xml_path: Path, roots: list[Path], repo: Path) -> dict[str, dict[int, int]]:
    tree = ET.parse(xml_path)
    files: dict[str, dict[int, int]] = {}
    cwd = Path.cwd().resolve()
    for klass in tree.findall(".//class"):
        raw_filename = klass.get("filename")
        if not raw_filename:
            continue
        source = resolve_path(raw_filename, cwd)
        if not is_under(source, roots):
            continue
        lines = files.setdefault(repo_relative(source, repo), {})
        for line in klass.findall("./lines/line"):
            number = int(line.get("number", "0"))
            hits = int(float(line.get("hits", "0")))
            lines[number] = max(lines.get(number, 0), hits)
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
    parser.add_argument("roots", nargs="*", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd().parent)
    args = parser.parse_args()
    roots = [resolve_path(str(root), Path.cwd().resolve()) for root in args.roots]
    write_lcov(convert(args.xml, roots, args.repo_root.resolve()), args.output)


if __name__ == "__main__":
    main()
