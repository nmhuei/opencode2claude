#!/usr/bin/env python3
"""Fail when tracked or untracked non-ignored source contains deployable secrets.

This complements hosted scanners. It intentionally targets strong provider/token
signatures and private-key material to keep local/CI results deterministic.
"""
from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SKIP_SUFFIXES = {
    ".lock", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf",
    ".zip", ".gz", ".tar", ".woff", ".woff2", ".ttf",
}
PATTERNS = [
    ("private-key", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----")),
    ("aws-access-key", re.compile(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b")),
    ("github-token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{30,}\b")),
    ("slack-token", re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{24,}\b")),
    ("openai-style-token", re.compile(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{32,}\b")),
    ("tavily-token", re.compile(r"\btvly-[A-Za-z0-9_-]{24,}\b")),
    ("jwt", re.compile(r"\beyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{16,}\b")),
]
ALLOW_MARKER = "EXAMPLE_SECRET_SCAN_ALLOW"


def repository_files() -> list[pathlib.Path]:
    output = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT,
    )
    return [ROOT / item.decode() for item in output.split(b"\0") if item]


def scan_text(path: pathlib.Path, text: str) -> list[str]:
    findings: list[str] = []
    for number, line in enumerate(text.splitlines(), start=1):
        if ALLOW_MARKER in line:
            continue
        for name, pattern in PATTERNS:
            if pattern.search(line):
                findings.append(f"{path.relative_to(ROOT)}:{number}: {name}")
    return findings


def scan_repository() -> list[str]:
    findings: list[str] = []
    for path in repository_files():
        relative = path.relative_to(ROOT)
        if relative.name == ".env":
            findings.append(".env: tracked runtime secret file is forbidden")
            continue
        if path.suffix.lower() in {".pem", ".key", ".p12", ".pfx"}:
            findings.append(f"{relative}: tracked key/certificate container is forbidden")
            continue
        if path.suffix.lower() in SKIP_SUFFIXES or not path.is_file():
            continue
        if path.stat().st_size > 2_000_000:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        findings.extend(scan_text(path, text))
    return findings


def self_test() -> int:
    fake = ROOT / "self-test.txt"
    positive = [
        "-----BEGIN PRIVATE KEY-----",  # EXAMPLE_SECRET_SCAN_ALLOW
        "token=ghp_" + "A" * 36,
        "AWS=AKIA" + "B" * 16,
        "OPENAI=sk-" + "c" * 40,
    ]
    for sample in positive:
        if not scan_text(fake, sample):
            print(f"secret-scan self-test missed: {sample[:20]}", file=sys.stderr)
            return 1
    negative = [
        "TAVILY_API_KEY=tvly-...",
        "REST_API_TOKEN=change-me",
        f"ghp_{'A' * 36} {ALLOW_MARKER}",
    ]
    for sample in negative:
        if scan_text(fake, sample):
            print(f"secret-scan self-test false positive: {sample}", file=sys.stderr)
            return 1
    print("secret-scan self-test: PASS")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    findings = scan_repository()
    if findings:
        print("secret-scan: FAILED", file=sys.stderr)
        for finding in findings:
            print(f"- {finding}", file=sys.stderr)
        return 1
    print(f"secret-scan: PASS files={len(repository_files())}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
