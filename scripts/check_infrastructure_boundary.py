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
        lines = path.read_text(encoding="utf-8").splitlines()
        in_test_module = False
        pending_test_cfg = False
        test_depth = 0
        depth = 0
        for number, line in enumerate(lines, start=1):
            stripped = line.strip()
            if stripped.startswith("#[cfg(test)]"):
                pending_test_cfg = True
            if pending_test_cfg and (stripped.startswith("mod tests") or stripped.startswith("pub mod tests")) and "{" in line:
                in_test_module = True
                test_depth = depth + line.count("{") - line.count("}")
                pending_test_cfg = False
            if any(pattern.search(line) for pattern in COMMAND_PATTERNS) and not in_test_module:
                violations.append(f"{relative}:{number}: {stripped}")
            depth += line.count("{") - line.count("}")
            if in_test_module and depth < test_depth:
                in_test_module = False

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
