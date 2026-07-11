#!/usr/bin/env python3
"""Validate required documentation files and reject known unsupported claims."""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
REQUIRED = [
    "README.md",
    "docs/architecture/README.md",
    "docs/configuration.md",
    "docs/cli.md",
    "docs/compatibility.md",
    "docs/management-api.md",
    "docs/proxy-pool.md",
    "docs/security.md",
    "docs/deployment.md",
    "docs/observability.md",
    "docs/troubleshooting.md",
    "docs/upgrade-rollback.md",
    "docs/testing.md",
    "docs/release-checklist.md",
]
FORBIDDEN_README_PATTERNS = {
    r"\bopencode2api estimate\b": "no estimate CLI command exists",
    r"--warp-pool\b": "no --warp-pool option exists",
    r"--pool-size\b": "no --pool-size option exists",
    r"--standby-size\b": "no --standby-size option exists",
    r"Prometheus metrics \(`/metrics`\)": "metrics are authenticated at /api/v1/metrics",
    r"prefix executes locally": "shell delegation emits tool_use; the bridge does not execute it",
    r"BRIDGE_WARP_POOL": "unsupported legacy setting",
}
REQUIRED_README_LINKS = [
    "docs/configuration.md",
    "docs/compatibility.md",
    "docs/security.md",
    "docs/testing.md",
    "docs/upgrade-rollback.md",
]


def main() -> int:
    errors: list[str] = []
    for relative in REQUIRED:
        path = ROOT / relative
        if not path.is_file() or path.stat().st_size < 100:
            errors.append(f"missing or empty required document: {relative}")

    readme_path = ROOT / "README.md"
    readme = readme_path.read_text(encoding="utf-8") if readme_path.exists() else ""
    for pattern, reason in FORBIDDEN_README_PATTERNS.items():
        if re.search(pattern, readme, flags=re.IGNORECASE):
            errors.append(f"README unsupported claim ({reason}): /{pattern}/")
    for link in REQUIRED_README_LINKS:
        if link not in readme:
            errors.append(f"README does not link required guide: {link}")

    config_doc = (ROOT / "docs/configuration.md")
    env_example = (ROOT / ".env.example")
    if config_doc.exists() and env_example.exists():
        config_text = config_doc.read_text(encoding="utf-8")
        env_names = set(re.findall(r"^#?\s*([A-Z][A-Z0-9_]+)=", env_example.read_text(encoding="utf-8"), re.MULTILINE))
        undocumented = sorted(name for name in env_names if name not in config_text)
        if undocumented:
            errors.append("configuration guide misses env vars: " + ", ".join(undocumented))

    if errors:
        print("docs-check: FAILED", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(f"docs-check: PASS required={len(REQUIRED)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
