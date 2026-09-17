# LunarWing Code Contribution Policy

This document is the single canonical reference for how code is contributed to LunarWing. It consolidates conventions that were previously scattered across `AGENTS.md`, the GitLab CI, subsystem specs, and guide documents. When this policy disagrees with an older document, this one wins.

**Revalidated 2026-09-17 (v2.1.2.0).** Policy text and referenced paths were
re-checked against the current tree; no changes were needed (the
`ic/src/*/CLAUDE.md` subsystem-spec glob currently resolves for `db`; other
subsystem specs live in `docs/` as flagged in `AGENTS.md`).

## 1. Project values

LunarWing is privacy-first, security-first, and self-hosting-first. We prefer open-weight models and local inference, we support user choice, and we do not expose secrets to any model or third-party service. See `MANIFESTO.md` and `README.md` for the full philosophy.

## 2. What this policy covers

This policy applies to all code merged into the `lunarwing2` repository. It does not replace subsystem specs (`ic/src/*/CLAUDE.md`, `docs/architecture/`, `docs/specs/`); it sits above them. Read the relevant subsystem spec before changing a complex area.

## 3. Code generation with AI

See [`AI-CODE-CONTRIBUTION-POLICY.md`](./AI-CODE-CONTRIBUTION-POLICY.md) for the complete rules. In short:

- Closed-weight/proprietary models may **not** be used to generate source code merged into this repository (effective 2026-09-15).
- Open-weight models are preferred and acceptable for code generation.
- Proprietary models are permitted only for narrow, non-code-generation purposes such as documentation refinement and security analysis.
- All other human contribution rules still apply regardless of how the code was produced.

## 4. Language, toolchain, and style

- **Rust edition:** 2024, MSRV pinned to **1.96.1**.
- **Formatting:** standard `rustfmt`. Run `cargo fmt --all` before committing.
- **Linting:** zero warnings policy. `cargo clippy --all --benches --tests --examples -- -D warnings` must pass; same with `--all-features`.
- **Dependency audit:** `cargo deny check` must pass.
- **Python:** follow the existing scripts; CI enforces syntax and selected test coverage.
- **Shell:** CI runs shellcheck on harnesses and scripts.

## 5. Code conventions

- **Imports:** prefer `crate::` for cross-module references; group `std`, external crates, then internal crates.
- **Error handling:** use `thiserror` for structured errors and `anyhow` for propagation. Avoid `unwrap()` and `expect()` in production code. They are allowed only in tests or for literally infallible invariants (e.g. literal regexes) with a safety comment.
- **Types:** prefer strong types and enums over stringly-typed control flow when the shape is known.
- **Naming:** standard Rust conventions (`snake_case` functions/variables, `PascalCase` types/traits, `SCREAMING_SNAKE_CASE` constants).
- **Complexity:** keep functions under 100 lines, cognitive complexity under 15, and arguments under 7 (enforced via `clippy.toml`).
- **Secrets:** use the `secrecy` crate; never log or expose secrets.
- **Logging:** use `tracing` macros (`info!`, `warn!`, `error!`), not `println!`.

## 6. Architecture and module discipline

- Keep `src/main.rs` and `src/app.rs` orchestration-focused. Do not move module-owned logic into entrypoints.
- Module-specific initialization lives behind a public factory/helper in the owning module.
- Keep feature-flag branching inside the module that owns the abstraction whenever possible.
- Prefer extending existing traits and registries over hardcoding one-off integration paths.
- Treat Docker, podman containers, and external services as untrusted.
- Preserve extension lifecycle expectations: install, authenticate/configure, activate, remove.

## 7. Database and persistence

- **PostgreSQL** is the primary backend and is far more heavily supported. `libSQL` support is aspirational; do not regress existing `libSQL` coverage when possible.
- Add new DB operations to the shared DB trait first, then implement both backends.
- Treat bootstrap config, DB-backed settings, and encrypted secrets as distinct layers.
- If onboarding or setup behavior changes, update `src/setup/README.md` in the same branch.
- Do not break config precedence, bootstrap env loading, DB-backed config reload, or post-secrets LLM re-resolution.

## 8. Multi-tenancy and service operations

- Tenant lifecycle, status, and health checks go through `ic/scripts/lunarwing-mt-admin.sh` (override path with `LUNARWING_MT_ADMIN` if needed).
- Onboard/web/verify wrappers must be **thin drivers** of `mt-admin`; they must not reimplement tenant lifecycle or call `systemctl`/`rc-service` directly.
- **Both systemd and OpenRC are first-class.** launchd is supported at a lower priority.
- systemd tenant units live on the **user** bus (`systemctl --user`), not the system bus.
- For install-style harness tests, prefer rendered service units over transient shell environments.

## 9. Build and test discipline

- **Use `cargo check` for compile verification, not `cargo build`.** Full debug builds are wasteful and should be avoided.
- Use release builds (`cargo build --release`) only when deploying to a tenant or upgrading an existing tenant's release binary.
- On the Gentoo dev VM, prefix every cargo command with `taskset -c 0-11` and use `-j12`.
- Long-running commands must use `tmux`.
- Add the narrowest tests that cover the change: unit tests for local logic, integration tests for runtime/DB/routing behavior, E2E or trace coverage for gateway/approvals/extensions/user-visible flows.
- WASM extension builds are required for some integration tests: `./scripts/build-wasm-extensions.sh`.

## 10. Security and runtime invariants

- Review any change touching listeners, routes, auth, secrets, sandboxing, approvals, or outbound HTTP with a security mindset.
- Do not weaken bearer-token auth, webhook auth, CORS/origin checks, body limits, rate limits, allowlists, or secret-handling guarantees.
- Session/thread/turn state matters. Submission parsing happens before normal chat handling.
- Skills are selected deterministically. Tool approval and auth flows are special paths and must not be mixed into normal chat history carelessly.
- Persistent memory is the workspace system, not just transcript storage; preserve file-like semantics, chunking/search behavior, and identity/system-prompt loading.

## 11. Change discipline

- Keep changes scoped. Avoid broad refactors unless the task truly requires them.
- Security, database schema, runtime, worker, CI, and secrets changes are high-risk. Call out rollback risks, compatibility concerns, and hidden side effects.
- Preserve existing defaults unless the task explicitly changes them.
- Avoid unrelated file churn and generated-file edits unless required.
- Respect a dirty worktree and never revert user changes you did not make.
- If behavior changes, update the relevant docs/specs in the same branch.

## 12. Before finishing a contribution

- Run the most targeted tests/checks that cover the change.
- Run `cargo fmt --all -- --check`.
- Run the relevant `cargo clippy` and `cargo deny check` commands.
- Re-check security-sensitive paths when touching auth, secrets, network listeners, sandboxing, or approvals.
- Keep the final diff scoped to the task.

## 13. Where to get help

- Architecture specs: `docs/architecture/`
- Subsystem specs: `ic/src/*/CLAUDE.md`
- Testing guide: `docs/guides/TESTING_GUIDE.md`
- Debug logging: `docs/guides/DEBUG_LOG.md`
- Multi-tenant quickstart: `docs/guides/MT-ADMIN-QUICKSTART.md`
- Community IRC: `#lunarwing` on Libera Chat (`irc.libera.chat:6697`, TLS)

## 14. Policy maintenance

This file is intended to be the single up-to-date source for code contribution conventions. If you find contradictions between this policy and another document, update the other document or file an issue. Do not add duplicate convention docs; extend this one instead.
