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


def resolve_path(raw: str, base: Path) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        path = base / path
    return path.resolve()


def existing_candidate(candidates: list[Path]) -> Path | None:
    for candidate in candidates:
        if candidate.exists():
            return candidate.resolve()
    return None


def resolve_source(raw: str, cwd: Path, repo: Path, xml_sources: list[Path]) -> Path:
    """Resolve Cobertura class filenames across producer conventions.

    Cobertura producers disagree about whether class `filename` is absolute,
    relative to the current directory, relative to the repository, or relative
    to one of the XML `<source>` entries. Kcov on Ubuntu 22.04 uses the
    `<sources>` section for Zig paths, so try those before falling back.
    """

    path = Path(raw)
    candidates: list[Path] = []
    if path.is_absolute():
        candidates.append(path)
    else:
        candidates.extend(source / path for source in xml_sources)
        candidates.extend([repo / path, cwd / path])

    found = existing_candidate(candidates)
    if found is not None:
        return found

    for source in xml_sources:
        # Kcov can emit a single-file `<source>` entry. In that case the class
        # filename may be just the basename, not a path relative to a directory.
        if source.name == path.name and source.exists():
            return source.resolve()

    return candidates[0].resolve() if candidates else path.resolve()


def xml_source_roots(tree: ET.ElementTree, cwd: Path, repo: Path) -> list[Path]:
    roots: list[Path] = []
    for node in tree.findall(".//sources/source"):
        text = (node.text or "").strip()
        if not text:
            continue
        source = resolve_path(text, cwd)
        if source.exists():
            roots.append(source)
            continue
        source = resolve_path(text, repo)
        roots.append(source)
    return roots


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
    xml_sources = xml_source_roots(tree, cwd, repo)
    for klass in tree.findall(".//class"):
        raw_filename = klass.get("filename")
        if not raw_filename:
            continue
        source = resolve_source(raw_filename, cwd, repo, xml_sources)
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
