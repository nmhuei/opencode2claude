#!/usr/bin/env python3
"""Keep direct process/container execution inside infrastructure adapters."""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
COMMAND_PATTERNS = [
    re.compile(r"(?:tokio::process::|std::process::)?Command::new\s*\("),
    re.compile(r"tokio::process::Command::new\s*\("),
]
ALLOWED_PREFIX = pathlib.Path("src/infrastructure")


def main() -> int:
    violations: list[str] = []
    for path in sorted(SRC.rglob("*.rs")):
        relative = path.relative_to(ROOT)
        if relative.parts[:2] == ALLOWED_PREFIX.parts:
            continue
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if any(pattern.search(line) for pattern in COMMAND_PATTERNS):
                violations.append(f"{relative}:{number}: {line.strip()}")

    if violations:
        print("infrastructure-boundary: FAILED", file=sys.stderr)
        print("Direct process execution must stay under src/infrastructure/.", file=sys.stderr)
        for violation in violations:
            print(f"- {violation}", file=sys.stderr)
        return 1

    print("infrastructure-boundary: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
