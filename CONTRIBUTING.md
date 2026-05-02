# Contributing to Claude Self-Reflect

## Development Setup

```bash
git clone https://github.com/ramakay/claude-self-reflect.git
cd claude-self-reflect/csr-engine
cargo build --release
cargo test
```

## Code Quality

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

## Pre-commit Hook

A pre-commit hook runs fmt, clippy, and tests automatically. Install:

```bash
cp .githooks/pre-commit .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit
```

## Pull Requests

- Branch from `main`
- Include tests for new features
- All CI checks must pass
- Keep PRs focused — one feature/fix per PR

## Architecture

See [CLAUDE.md](CLAUDE.md) for architecture details and key patterns.
