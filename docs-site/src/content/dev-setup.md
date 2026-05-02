---
title: Development Setup
---

## Prerequisites

- Rust 1.73+
- macOS (Apple Silicon) or Linux
- Git

## Build

```bash
git clone https://github.com/ramakay/claude-self-reflect.git
cd claude-self-reflect/csr-engine
cargo build --release
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

## Tests

```bash
cargo test               # All tests
cargo test --lib          # Unit only
cargo test --test hooks_integration  # Hooks
cargo test test_reflect_on_past      # Specific
```

## Workflow

1. Create feature branch from `main`
2. Make changes
3. `cargo test` — all must pass
4. `cargo clippy -- -D warnings` — zero warnings
5. `cargo fmt` — clean formatting
6. Submit PR

## License

MIT. See [LICENSE](https://github.com/ramakay/claude-self-reflect/blob/main/LICENSE).
