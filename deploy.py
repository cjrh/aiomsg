#!/usr/bin/env python3
import sys
import shlex
import argparse
from pathlib import Path
import subprocess as sp


def main(args):
    args.debug and print(args)
    args.dry_run and print("Dry run active!")

    folder = Path(__file__).parent
    version_filename = folder / "VERSION"
    version = open(version_filename, encoding="utf-8").readline().strip()
    if args.show:
        print(version)
        return

    major, minor, patch, *_ = version.split(".")
    if "major" in args.field:
        major = int(major) + 1
        minor = 0
        patch = 0
    if "minor" in args.field:
        minor = int(minor) + 1
        patch = 0
    if "patch" in args.field or not args.field:
        patch = int(patch) + 1
    new_version = f"{major}.{minor}.{patch}"

    if args.dry_run:
        print(f"The new version that would be written: {new_version}")
        return

    git_status_output = sp.run("git status".split(), capture_output=True).stdout
    if b"Changes not staged for commit:" in git_status_output:
        print(f"Repo has uncommitted changes. Cannot continue")
        sys.exit(1)

    if b"Untracked files:" in git_status_output:
        print(f"Repo has untracked files. Cannot continue")
        sys.exit(1)

    with open(version_filename, "w", encoding="utf-8") as f:
        f.write(new_version)

    sp.run(f"git add {version_filename}".split(), cwd=folder)
    sp.run(shlex.split(f"git commit -m 'Bump version to {new_version}'"), cwd=folder)
    sp.run(f"git tag v{new_version}".split(), cwd=folder)
    if args.push_git:
        sp.run(f"git push --follow-tags".split(), cwd=folder)

    sp.run(f"python setup.py bdist_wheel sdist".split(), cwd=folder)
    if args.push_pypi:
        sp.run(f"twine upload dist/*{new_version}*", cwd=folder)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--debug", action="store_true")
    parser.add_argument("--show", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--push-git", action="store_true")
    parser.add_argument("--push-pypi", action="store_true")
    parser.add_argument(
        "field",
        choices=["major", "minor", "patch"],
        default="patch",
        const="patch",
        nargs="?",
    )
    args = parser.parse_args()
    main(args)
