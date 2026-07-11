#!/usr/bin/env python3
"""Prevent runtime policy from bypassing the resolved configuration tree."""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
CALL = re.compile(r"std::env::var(?:_os)?\s*\(")
ALLOWED_FILES = {
    pathlib.Path("src/config/loader.rs"),
    pathlib.Path("src/runtime.rs"),  # HOME/RUNTIME_DIR bootstrap fallback only.
    pathlib.Path("src/output.rs"),   # NO_COLOR presentation convention only.
}


def main() -> int:
    violations: list[str] = []
    for path in sorted(SRC.rglob("*.rs")):
        relative = path.relative_to(ROOT)
        if relative in ALLOWED_FILES or "tests" in path.parts:
            continue
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if CALL.search(line):
                violations.append(f"{relative}:{number}: {line.strip()}")

    if violations:
        print("config-boundary: FAILED", file=sys.stderr)
        print("Runtime environment reads must be resolved in src/config/loader.rs.", file=sys.stderr)
        for violation in violations:
            print(f"- {violation}", file=sys.stderr)
        return 1

    print("config-boundary: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
