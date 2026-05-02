# Flux Schema Migrations

Each entry: trigger, motivation, impact, automatic vs manual.

## v1 (current - 2026-05-01)

Initial declared schema version. Older configs without a `version:` field
default to 1. No automatic migration; everything is identity.

## v1 additive note (2026-05-02)

`file` / `script` / `block` items accept optional `tags: []` for pipeline
filtering. This is additive and backward compatible within schema v1.

## v2 (2026-05-02)

Schema v2 adds:
- `imports` at the config root
- `password` as `SecretValue` (`"plain"` or `"keychain:service.account"`)
- `file.kind` and `file.target`
- additive `tags` support across items

Validation tightened alongside v2 usage:
- `register_key: true` now requires a non-empty `key`
- `file.chmod` values must be valid octal strings
- `proxy.local_port` must be greater than zero

Env interpolation remains `${VAR}` / `${VAR:-default}` and now supports `$$`
to emit a literal dollar sign, which is useful for YAML comments and examples.

This remains backward compatible for v1 YAML: existing configs without these
fields still load unchanged, and configs without an explicit `version:` field
still probe as schema v1.
