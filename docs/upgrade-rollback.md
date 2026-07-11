# Upgrade, migration, and rollback

OpenCode2API uses config schema version `1`, checksum-verified release artifacts, and transactional self-update.

## Before upgrading

1. Record the current version:

   ```bash
   opencode2api --version
   ```

2. Back up the configuration and current binary.
3. Run `opencode2api doctor` and ensure readiness is healthy.
4. Download the target release checksum, SBOM, and provenance.
5. Review `CHANGELOG.md` for config or behavior changes.

## Self-update

Check without changing the installation:

```bash
opencode2api update --check
```

Apply:

```bash
opencode2api update
```

Force reinstall the release:

```bash
opencode2api update --force
```

The update transaction performs:

```text
download binary and companion .sha256
→ verify SHA-256
→ run candidate --version
→ preserve previous binary
→ atomically replace
→ run installed binary --version
→ remove backup on success
```

If post-install smoke fails, the previous binary is restored automatically.

## Install-script upgrade

The install script accepts a version and destination:

```bash
OPENCODE2API_VERSION=v0.5.0 \
OPENCODE2API_BINDIR="$HOME/.local/bin" \
sh install.sh
```

For a private mirror or offline fixture, set both:

```bash
OPENCODE2API_DOWNLOAD_URL='https://mirror.example/opencode2api-linux-amd64'
OPENCODE2API_CHECKSUM_URL='https://mirror.example/opencode2api-linux-amd64.sha256'
```

The installer refuses to copy a checksum-mismatched or non-executable candidate.

## Config migration

The loader migrates supported legacy keys to the current schema before deserialization. Migration is idempotent. It rejects:

- a future unsupported `schema_version`;
- simultaneous legacy and current keys with conflicting values;
- unknown or semantically invalid settings during management apply.

Generate a fresh reference without overwriting the active file:

```bash
opencode2api init --output opencode2api.new.toml
```

Preview changes through the management API before applying. The apply path recursively merges known keys, writes atomically, verifies the resulting file, and restores the prior content on failure.

## Manual binary rollback

Stop the managed process:

```bash
opencode2api server stop
```

Replace the binary with the previously verified copy, then confirm:

```bash
opencode2api --version
opencode2api server start
curl -fsS http://127.0.0.1:4000/health/ready
```

Do not replace a running executable through an unverified download.

## Config rollback

Restore the backed-up TOML and start in foreground first:

```bash
opencode2api server start --foreground --config /path/to/restored.toml
```

After liveness and readiness pass, return to the intended service manager or background supervisor.

## Container rollback

Pin the previous known-good WARP image digest in the config. Managed primaries can then be reconciled. Protected warm standbys are outside application lifecycle ownership and must be rolled back by their provisioning system.

Always use dry-run first:

```bash
opencode2api --json proxy purge --dry-run
```

## Release verification

Each release should contain:

- platform binary;
- platform binary `.sha256` companion;
- `SHA256SUMS`;
- SPDX JSON SBOM;
- GitHub build provenance;
- signed container digest with registry SBOM/provenance.

Verify SHA-256 before install and verify GitHub attestation or container signature against the repository identity.

## Test evidence

Transactional updater tests cover checksum mismatch, pre-install smoke failure, successful replacement, and post-install rollback. `tests/install_e2e.sh` covers disposable installation, checksum rejection, dry-run uninstall, and complete uninstall.
