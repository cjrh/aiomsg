#!/usr/bin/env python3
"""Rewrite LCOV SF entries to paths relative to the repository root."""

from __future__ import annotations

import argparse
from pathlib import Path


def normalize(source: str, cwd: Path, repo: Path) -> str:
    path = Path(source)
    if not path.is_absolute():
        path = cwd / path
    try:
        return path.resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        return source


def rewrite(input_path: Path, output_path: Path, repo: Path) -> None:
    cwd = Path.cwd().resolve()
    lines = []
    for raw in input_path.read_text(encoding="utf-8").splitlines():
        if raw.startswith("SF:"):
            lines.append("SF:" + normalize(raw[3:], cwd, repo))
        else:
            lines.append(raw)
    output_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd().parent)
    args = parser.parse_args()
    rewrite(args.input, args.output, args.repo_root)


if __name__ == "__main__":
    main()
