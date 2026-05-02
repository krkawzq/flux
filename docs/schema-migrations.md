# Flux Schema Migrations

Each entry: trigger, motivation, impact, automatic vs manual.

## v1 (current - 2026-05-01)

Initial declared schema version. Older configs without a `version:` field
default to 1. No automatic migration; everything is identity.

## v1 additive note (2026-05-02)

`file` / `script` / `block` items accept optional `tags: []` for pipeline
filtering. This is additive and backward compatible within schema v1.
