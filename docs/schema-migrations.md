# Flux Schema Migrations

Each entry: trigger, motivation, impact, automatic vs manual.

## v1 (current - 2026-05-01)

Initial declared schema version. Older configs without a `version:` field
default to 1. No automatic migration; everything is identity.
