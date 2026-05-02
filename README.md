# flux

SSH remote configuration sync tool — keep dotfiles, AI tool configs and
install scripts in one declarative `.flux/<name>.yml`, sync to a remote
host with idempotent `apply / skip` semantics.

## Quick start

```bash
cargo build --release

# inspect what would change without touching the remote
./target/release/flux sync <name> --dry-run --diff

# real run
./target/release/flux sync <name>

# undo the previous sync (restores .flux-<ts>.bak files)
./target/release/flux undo <name>
```

`<name>` resolves to `.flux/<name>.yml` (or `~/.flux/<name>.yml`).

## Phase 4 features at a glance

- `--dry-run --diff` — unified diff per file / before-after per zsh block
- `--only-stage file|script|block` / `--skip-stage` / `--only-item a,b` / `--tag dotfiles`
- `flux undo <name>` — rolls back to the most recent `.flux-<ts>.bak`
- `--hosts a,b,c` — fan out one config to multiple hosts in parallel
- `--retries N` — automatic retry on transient SSH/IO errors
- `--script-timeout SECS` — kill remote scripts that hang
- `--log-format json` — structured tracing output for CI consumption
- `~/.flux/audit.jsonl` — append-only audit log of every run
- `~/.flux/state/<host>.json` — cache for RTT-free skips on repeat sync
- `--resume` — continue from the last failed item

## Schema notes

- `.flux/.env` is auto-loaded; reference variables in YAML as `${VAR}` or `${VAR:-default}`.
- Secrets can use `password: "keychain:service.account"` (macOS Keychain / Linux secret-tool).
- `imports: [base.yml, work.yml]` lets you compose configs (deep-merge, later overrides earlier).
- File items support `kind: file|dir|glob|link` (Auto detected; only `link` must be explicit).

See `docs/schema-migrations.md` for the v1 → v2 schema changelog and
`docs/superpowers/specs/2026-05-01-flux-refactor-design.md` for the
full architecture.
