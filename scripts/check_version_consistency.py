#!/usr/bin/env python3
"""Fail when package, lockfile, changelog, binary, or release tag versions drift."""
from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")


def package_version() -> str:
    data = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    value = str(data["package"]["version"])
    if not SEMVER.fullmatch(value):
        raise ValueError(f"Cargo.toml package version is not semver: {value!r}")
    return value


def lock_version() -> str:
    data = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    matches = [pkg["version"] for pkg in data.get("package", []) if pkg.get("name") == "opencode2api"]
    if len(matches) != 1:
        raise ValueError(f"expected one opencode2api package in Cargo.lock, found {len(matches)}")
    return str(matches[0])


def changelog_version() -> str:
    text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    match = re.search(r"^## \[([^]]+)]", text, flags=re.MULTILINE)
    if not match:
        raise ValueError("CHANGELOG.md has no release heading")
    return match.group(1)


def binary_version(path: pathlib.Path) -> str:
    output = subprocess.check_output([str(path), "--version"], text=True).strip()
    match = re.search(r"(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?)", output)
    if not match:
        raise ValueError(f"cannot parse version from {path}: {output!r}")
    return match.group(1)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path)
    parser.add_argument("--tag", help="release tag, with or without v prefix")
    args = parser.parse_args()

    try:
        expected = package_version()
        observed = {
            "Cargo.lock": lock_version(),
            "CHANGELOG.md": changelog_version(),
        }
        if args.binary:
            observed[str(args.binary)] = binary_version(args.binary)
        if args.tag:
            observed["release tag"] = args.tag.removeprefix("v")
    except (OSError, KeyError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"version-consistency: ERROR: {exc}", file=sys.stderr)
        return 1

    mismatches = {name: value for name, value in observed.items() if value != expected}
    if mismatches:
        print(f"version-consistency: FAILED expected={expected}", file=sys.stderr)
        for name, value in mismatches.items():
            print(f"- {name}: {value}", file=sys.stderr)
        return 1

    print(f"version-consistency: PASS version={expected} sources={len(observed) + 1}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
