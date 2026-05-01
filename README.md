# flux

Rust project with local machine bootstrap and config templates under `.flux.example/`.

Initialize local config:

```bash
cp -R .flux.example .flux
```

Then fill real values into `.flux/` before using any machine-specific config.

Build and run:

```bash
cargo build
cargo run
```
