#!/usr/bin/env python3
"""Authoritative release inventory and non-publishing metadata contract check.

Package-specific ``just set-release-version`` recipes own mutations. This module
owns the repository-wide knowledge of which packages participate in a coherent
release, where each package stores its version, and which packages a ``v*`` tag
publishes. Keeping that knowledge here prevents the root release recipe,
documentation, and publication policy from drifting independently.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
VersionReader = Callable[[Path], str]


@dataclass(frozen=True)
class Package:
    """One implementation participating in the repository release."""

    directory: str
    version_source: str
    read_version: VersionReader | None
    registry: str = "—"
    published: bool = False
    note: str = ""


def json_version(path: Path) -> str:
    return json.loads(path.read_text(encoding="utf-8"))["version"]


def toml_project_version(path: Path) -> str:
    return tomllib.loads(path.read_text(encoding="utf-8"))["project"]["version"]


def toml_package_version(path: Path) -> str:
    return tomllib.loads(path.read_text(encoding="utf-8"))["package"]["version"]


def cargo_lock_version(path: Path, package_name: str) -> str:
    packages = tomllib.loads(path.read_text(encoding="utf-8")).get("package", [])
    versions = [item["version"] for item in packages if item.get("name") == package_name]
    if len(versions) != 1:
        raise ValueError(f"expected one {package_name!r} entry, found {len(versions)}")
    return versions[0]


def regex_version(path: Path, pattern: str) -> str:
    match = re.search(pattern, path.read_text(encoding="utf-8"), re.MULTILINE)
    if not match:
        raise ValueError(f"could not find version in {path}")
    return match.group(1)


def cmake_version(path: Path) -> str:
    return regex_version(path, r"^\s*VERSION\s+([0-9]+\.[0-9]+\.[0-9]+)\s*$")


def zig_version(path: Path) -> str:
    return regex_version(path, r'^\s*\.version\s*=\s*"([^"]+)"')


def gradle_version(path: Path) -> str:
    return regex_version(path, r'^\s*version\s*=\s*"([^"]+)"')


def csproj_version(path: Path) -> str:
    return ET.parse(path).findtext(".//Version") or ""


def lua_rockspec_version(path: Path) -> str:
    return regex_version(path, r'^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)-1"\s*$')


def lua_rockspec_filename_version(path: Path) -> str:
    match = re.fullmatch(r"aiomsg-([0-9]+\.[0-9]+\.[0-9]+)-1\.rockspec", path.name)
    if not match:
        raise ValueError(f"unexpected LuaRocks filename {path.name}")
    return match.group(1)


def lua_version(root: Path) -> str:
    rockspecs = list((root / "lua-lib").glob("aiomsg-*.rockspec"))
    if len(rockspecs) != 1:
        raise ValueError(f"expected one LuaRocks spec, found {len(rockspecs)}")
    rockspec = rockspecs[0]
    version = lua_rockspec_version(rockspec)
    filename_version = lua_rockspec_filename_version(rockspec)
    if version != filename_version:
        raise ValueError(
            f"LuaRocks filename version {filename_version} does not match spec {version}"
        )
    return version


# This is the single machine-readable inventory. The root justfile obtains its
# package loops from it; the release contract checker validates its sources.
INVENTORY = (
    Package("browser-lib", "browser-lib/package.json", lambda root: json_version(root / "browser-lib/package.json"), "npm", True),
    Package("javascript-lib", "javascript-lib/package.json", lambda root: json_version(root / "javascript-lib/package.json")),
    Package("python-lib", "python-lib/pyproject.toml", lambda root: toml_project_version(root / "python-lib/pyproject.toml"), "PyPI", True),
    Package("python-lib-blocking", "python-lib-blocking/pyproject.toml", lambda root: toml_project_version(root / "python-lib-blocking/pyproject.toml")),
    Package("rust-lib-async", "Cargo.toml + Cargo.lock", lambda root: checked_pair(toml_package_version(root / "rust-lib-async/Cargo.toml"), cargo_lock_version(root / "rust-lib-async/Cargo.lock", "aiomsg"))),
    Package("rust-lib-sync", "Cargo.toml + Cargo.lock", lambda root: checked_pair(toml_package_version(root / "rust-lib-sync/Cargo.toml"), cargo_lock_version(root / "rust-lib-sync/Cargo.lock", "aiomsg-sync"))),
    Package("c-lib", "c-lib/CMakeLists.txt", lambda root: cmake_version(root / "c-lib/CMakeLists.txt")),
    Package("cpp-lib-sync", "cpp-lib-sync/CMakeLists.txt", lambda root: cmake_version(root / "cpp-lib-sync/CMakeLists.txt")),
    Package("cpp-lib-async", "cpp-lib-async/CMakeLists.txt", lambda root: cmake_version(root / "cpp-lib-async/CMakeLists.txt")),
    Package("zig-lib", "zig-lib/build.zig.zon", lambda root: zig_version(root / "zig-lib/build.zig.zon")),
    Package("lua-lib", "lua-lib/aiomsg-X.Y.Z-1.rockspec", lua_version, note="LuaRocks revision is always -1"),
    Package("java-lib", "java-lib/build.gradle.kts", lambda root: gradle_version(root / "java-lib/build.gradle.kts")),
    Package("csharp-lib", "csharp-lib/Aiomsg/Aiomsg.csproj", lambda root: csproj_version(root / "csharp-lib/Aiomsg/Aiomsg.csproj")),
    Package("golang-lib", "golang-lib/vX.Y.Z Git tag", None, note="Go module version comes from its directory-prefixed tag"),
)


def checked_pair(first: str, second: str) -> str:
    if first != second:
        raise ValueError(f"manifest version {first} does not match lockfile version {second}")
    return first


def validate(root: Path, expected: str | None) -> list[str]:
    """Return contract failures without mutating the checkout or using network."""

    failures: list[str] = []
    observed: list[str] = []
    for package in INVENTORY:
        if package.read_version is None:
            continue
        try:
            actual = package.read_version(root)
        except (OSError, KeyError, ValueError, tomllib.TOMLDecodeError, ET.ParseError) as exc:
            failures.append(f"{package.directory}: cannot read {package.version_source}: {exc}")
            continue
        observed.append(actual)
        if expected is not None and actual != expected:
            failures.append(
                f"{package.directory}: {package.version_source} is {actual}, expected {expected}"
            )
    if expected is None and observed and len(set(observed)) != 1:
        failures.append("version-bearing packages do not all agree: " + ", ".join(sorted(set(observed))))
    return failures


def print_inventory() -> None:
    print("directory\tversion source\tregistry\tpublished\tnote")
    for package in INVENTORY:
        print(
            f"{package.directory}\t{package.version_source}\t{package.registry}\t"
            f"{'yes' if package.published else 'no'}\t{package.note}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    subcommands.add_parser("paths", help="print release package directories, one per line")
    subcommands.add_parser("list", help="print the release inventory")
    check = subcommands.add_parser("check", help="validate release metadata without publishing")
    check.add_argument("version", nargs="?", help="expected X.Y.Z version; omit to require agreement")
    args = parser.parse_args()

    if args.command == "paths":
        print("\n".join(package.directory for package in INVENTORY))
        return 0
    if args.command == "list":
        print_inventory()
        return 0

    if args.version and not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("version must be X.Y.Z")
    failures = validate(ROOT, args.version)
    if failures:
        print("release metadata check failed:", file=sys.stderr)
        print(*[f"- {failure}" for failure in failures], sep="\n", file=sys.stderr)
        return 1
    suffix = f" for {args.version}" if args.version else ""
    print(f"release metadata is consistent{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
