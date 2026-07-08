#!/usr/bin/env python3
"""Run Lua tests under a line hook and emit a minimal LCOV tracefile."""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


RUNNER = r'''
local output = arg[1]
local covered = {}

local function normalize(source)
  if source:sub(1, 1) ~= "@" then return nil end
  local path = source:sub(2):gsub("^%./", "")
  if path:match("^aiomsg/.*%.lua$") then return path end
  return nil
end

debug.sethook(function(_, line)
  local info = debug.getinfo(2, "S")
  local path = info and normalize(info.source)
  if path then
    covered[path] = covered[path] or {}
    covered[path][line] = true
  end
end, "l")

for i = 2, #arg do
  dofile(arg[i])
end

debug.sethook()
local handle = assert(io.open(output, "w"))
for path, lines in pairs(covered) do
  for line in pairs(lines) do
    handle:write(path, ":", tostring(line), "\n")
  end
end
handle:close()
'''


def find_luac() -> str:
    for name in ("luac", "luac5.4", "luac5.3", "luac5.2", "luac5.1"):
        path = shutil.which(name)
        if path:
            return path
    raise SystemExit("luac is required to identify executable Lua lines")


def executable_lines(source_dir: Path) -> dict[str, set[int]]:
    luac = find_luac()
    files: dict[str, set[int]] = {}
    cwd = Path.cwd().resolve()
    for source in sorted(source_dir.glob("*.lua")):
        source = source.resolve()
        rel = source.relative_to(cwd).as_posix()
        output = subprocess.check_output([luac, "-l", "-p", str(source)], text=True)
        lines = {int(match.group(1)) for match in re.finditer(r"^\s*\d+\s+\[(\d+)\]", output, re.M) if int(match.group(1)) > 0}
        files[rel] = lines
    return files


def run_tests(lua: str, tests: list[Path]) -> dict[str, set[int]]:
    with tempfile.TemporaryDirectory() as tmp:
        runner = Path(tmp) / "coverage_runner.lua"
        hits = Path(tmp) / "hits.txt"
        runner.write_text(RUNNER, encoding="utf-8")
        subprocess.run([lua, str(runner), str(hits), *map(str, tests)], check=True)
        covered: dict[str, set[int]] = {}
        for raw in hits.read_text(encoding="utf-8").splitlines():
            source, line = raw.rsplit(":", 1)
            covered.setdefault(source, set()).add(int(line))
        return covered


def repo_relative(path: Path, repo: Path) -> str:
    try:
        return path.resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        return path.resolve().as_posix()


def write_lcov(executable: dict[str, set[int]], covered: dict[str, set[int]], output: Path, repo: Path) -> None:
    with output.open("w", encoding="utf-8") as out:
        total_covered = 0
        total_lines = 0
        for rel in sorted(executable):
            lines = executable[rel]
            hits = covered.get(rel, set())
            source = repo_relative(Path.cwd() / rel, repo)
            out.write(f"SF:{source}\n")
            file_covered = 0
            for line in sorted(lines):
                count = 1 if line in hits else 0
                file_covered += count
                out.write(f"DA:{line},{count}\n")
            out.write(f"LH:{file_covered}\n")
            out.write(f"LF:{len(lines)}\n")
            out.write("end_of_record\n")
            total_covered += file_covered
            total_lines += len(lines)
    percent = total_covered / total_lines * 100 if total_lines else 0
    print(f"coverage: {total_covered}/{total_lines} lines ({percent:.1f}%)")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("tests", nargs="+", type=Path)
    parser.add_argument("--lua", default="lua")
    parser.add_argument("--source-dir", type=Path, default=Path("aiomsg"))
    parser.add_argument("--repo-root", type=Path, default=Path.cwd().parent)
    args = parser.parse_args()
    executable = executable_lines(args.source_dir)
    covered = run_tests(args.lua, args.tests)
    write_lcov(executable, covered, args.output, args.repo_root)


if __name__ == "__main__":
    main()
