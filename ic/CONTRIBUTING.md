# Contributing

## Getting Started

```bash
git clone https://git.lunarwing.org/lunarwing/lunarwing2.git
cd lunarwing2
./scripts/dev-setup.sh
```

This installs the Rust toolchain, WASM targets, git hooks, and runs initial checks.

## Development Workflow

```bash
cargo fmt                                                    # format
cargo clippy --all --benches --tests --examples --all-features  # lint (zero warnings)
cargo test                                                   # unit tests
cargo test --features integration                            # + PostgreSQL tests
```

## Code Style

- Zero clippy warnings policy (except for clearly defined exemptions)
- No `.unwrap()` or `.expect()` in production code (tests are fine)
- Use `thiserror` for error types, map errors with context
- Prefer `crate::` for cross-module imports
- Comments for non-obvious logic only

## Review Tracks

All PRs follow a risk-based review process:

| Track | Scope | Requirements |
|-------|-------|-------------|
| **A** | Docs, tests, chore, dependency bumps | 1 approval + CI green |
| **B** | Features, refactors, new tools/channels | 1 approval + CI green + test evidence |
| **C** | Security (`src/safety/`, `src/secrets/`), runtime (`src/agent/`, `src/worker/`), database schema, CI workflows | 2 approvals + rollback plan documented |

Select the appropriate track in the PR template based on what your changes touch.

## Adding Dependencies

Run `cargo deny check` before adding new dependencies to verify license compatibility and check for known advisories.

## AI code guidelines

* See: `docs/guides/AI-CODE-CONTRIBUTION-POLICY.md` for details
