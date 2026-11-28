# LunarWing patch: Monty takes postcard without its default features

Base: `pydantic/monty` crate `monty 0.0.17`, git revision
`5c7cf2b62be8e4421a4828072d41ebe69328bbf2` (crate path `crates/monty`), the
revision `lunarwing_engine` pins. Copied from `ic/vendor/monty` (the
`cargo vendor` output for that git dependency) and now used as a path-only
`[patch]` of the same git source, the way `patches/wasmtime-internal-cache`
is used.

Change, part 1 — one manifest line. The `postcard` dependency keeps
`features = ["alloc"]` and adds `default-features = false`:

```toml
[dependencies.postcard]
version = "1.1"
default-features = false
features = ["alloc"]
```

Change, part 2 — package-layout lines only, no code. The crate archive
addresses files outside the crate root, which a path dependency does not
have. `readme = "../../README.md"` becomes `readme = "README.md"`, and
`src/lib.rs` line 1 becomes `#![doc = include_str!("../README.md")]`; the
upstream `README.md` and `LICENSE` from the same revision are copied in
beside the manifest. Those two lines are the only non-manifest edits, and
`src/` is otherwise byte-identical to the revision.

Why: postcard 1.2.0's default `heapless-cas` feature pulls `heapless 0.7.17`,
which matches nine unsound advisories (RUSTSEC-2026-0574 through
RUSTSEC-2026-0582) and drags in unmaintained `atomic-polyfill 1.0.3`
(RUSTSEC-2023-0089). Monty only reaches postcard through its snapshot path:
`postcard::to_allocvec` (an `alloc::vec::Vec<u8>`) and
`postcard::from_bytes` (slice deserialization of Monty types) in
`src/{run,run_progress,repl}.rs`. Neither API is behind `heapless-cas`;
`alloc` is the only feature that path needs. LunarWing itself never calls
Monty's `dump`/`load`.

Upstream status: no 0.0.x release disables postcard's defaults (checked
`v0.0.18` through `v0.0.23`). Upstream removed the postcard dependency
entirely in `60e20e305b88eb2dd328d85cf1e25e056f0bae68` ("remove postcard, make
`id` calculations explicit and stable", 2026-11-19), which is part of the 1.x
rewrite; a revision bump is a large API change, not a feature fix.

Wiring: `ic/Cargo.toml` and `ic/bridges/xmpp-bridge/Cargo.toml` patch the git
source so both lock roots resolve this directory:

```toml
[patch."https://github.com/pydantic/monty.git"]
monty = { path = "patches/monty" }
```

Because the crate is a path dependency, `cargo vendor` leaves it alone and
`ic/scripts/refresh-vendor.sh` needs no special case for it. The lockfile
records `monty 0.0.17` without a `source` line.

Refreshing the base: if the `lunarwing_engine` pin moves, copy the crate from
the new revision over this directory (drop the vendor `.cargo-checksum.json`,
which a path dependency does not use), re-apply the `default-features = false`
line above, update the revision in this file, and confirm both lock roots:

```sh
taskset -c 0-17 cargo tree --locked --all-features --target all -e features -i postcard
```

The output must show `postcard feature "alloc"` (and `use-std` where Wasmtime
enables it) with no `default`, `heapless` or `heapless-cas` feature.
