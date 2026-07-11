#!/usr/bin/env python3
"""Validate the repository feature matrix.

Default mode validates structure and ownership. Set REQUIRE_VERIFIED=1 to make
any mandatory non-verified row fail, which is used by release-candidate gates.
"""
from __future__ import annotations

import os
import pathlib
import sys

MATRIX = pathlib.Path(__file__).resolve().parents[1] / "verification" / "FEATURE_MATRIX.md"
REQUIRED_COLUMNS = [
    "id",
    "feature",
    "public contract",
    "implementation module",
    "unit test",
    "integration/system test",
    "documentation",
    "mandatory",
    "status",
]
ALLOWED_STATUS = {"implemented", "partial", "blocked", "verified"}
ALLOWED_MANDATORY = {"yes", "no"}


def parse_rows(text: str) -> tuple[list[str], list[dict[str, str]]]:
    table_lines = [line.strip() for line in text.splitlines() if line.strip().startswith("|")]
    if len(table_lines) < 2:
        raise ValueError("matrix does not contain a Markdown table")
    header = [cell.strip() for cell in table_lines[0].strip("|").split("|")]
    if header != REQUIRED_COLUMNS:
        raise ValueError(f"unexpected columns: {header!r}; expected {REQUIRED_COLUMNS!r}")

    rows: list[dict[str, str]] = []
    for line in table_lines[2:]:
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != len(header):
            raise ValueError(f"row has {len(cells)} cells instead of {len(header)}: {line}")
        rows.append(dict(zip(header, cells, strict=True)))
    return header, rows


def main() -> int:
    try:
        _, rows = parse_rows(MATRIX.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        print(f"feature-matrix: ERROR: {exc}", file=sys.stderr)
        return 1

    errors: list[str] = []
    ids: set[str] = set()
    for index, row in enumerate(rows, start=1):
        row_id = row["id"]
        if not row_id:
            errors.append(f"row {index}: missing id")
        elif row_id in ids:
            errors.append(f"row {index}: duplicate id {row_id}")
        ids.add(row_id)

        for column in REQUIRED_COLUMNS:
            if not row[column]:
                errors.append(f"{row_id or index}: empty mandatory column {column!r}")
        if row["status"] not in ALLOWED_STATUS:
            errors.append(f"{row_id}: invalid status {row['status']!r}")
        if row["mandatory"] not in ALLOWED_MANDATORY:
            errors.append(f"{row_id}: invalid mandatory value {row['mandatory']!r}")

    if os.getenv("REQUIRE_VERIFIED") == "1":
        for row in rows:
            if row["mandatory"] == "yes" and row["status"] != "verified":
                errors.append(f"{row['id']}: mandatory feature is {row['status']}, not verified")

    if errors:
        print("feature-matrix: FAILED", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    counts: dict[str, int] = {status: 0 for status in ALLOWED_STATUS}
    for row in rows:
        counts[row["status"]] += 1
    print(
        "feature-matrix: PASS "
        f"rows={len(rows)} verified={counts['verified']} implemented={counts['implemented']} "
        f"partial={counts['partial']} blocked={counts['blocked']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
