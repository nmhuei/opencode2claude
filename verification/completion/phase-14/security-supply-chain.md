# Phase 14 — Security and supply-chain evidence

Implementation commit: `b9e0ea1`
Branch: `completion/full-repository-20260711`
Date: 2026-07-11

## Dependency graph

Direct dependency versions were normalized to current compatible majors, including `indicatif 0.18`, `thiserror 2`, `unicode-width 0.2`, and `sha2 0.11`. The obsolete `number_prefix` path and direct UUID dependency were removed. Security-sensitive random identifiers use `getrandom` through `src/infrastructure/random.rs`.

Executed:

```bash
cargo audit
cargo deny check
```

Result:

```text
cargo audit: no unresolved advisory
advisories ok, bans ok, licenses ok, sources ok
```

The deny policy documents the MPL-2.0 transitive exception and unavoidable duplicate dependency trees.

## Secret scanning

Executed:

```bash
python3 scripts/check_secrets.py --self-test
python3 scripts/check_secrets.py
```

Result:

```text
secret-scan self-test: PASS
secret-scan: PASS files=218
```

The scanner includes tracked files and untracked non-ignored files. It checks deployable token signatures, private-key material, `.env`, and key/certificate containers.

## Parser fuzz smoke

Executed in debug and release profiles:

```bash
cargo test --test parser_fuzz_smoke
cargo test --release --test parser_fuzz_smoke
```

Result:

```text
2 tests passed
0 failed
```

The deterministic corpus covers 2,000 DSML/config/search/text inputs and 256 malformed HTTP JSON requests through the production router.

## Shell and workflow lint

Executed:

```bash
docker run --rm -v "$PWD:/mnt" -w /mnt koalaman/shellcheck:stable --shell=bash ...
docker run --rm -v "$PWD:/mnt" -w /mnt koalaman/shellcheck:stable --shell=sh install.sh
docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest
```

Result: PASS with no findings.
