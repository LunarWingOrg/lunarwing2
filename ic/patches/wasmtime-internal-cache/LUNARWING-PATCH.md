# LunarWing patch: advisory-fixed zstd for Wasmtime's compilation cache

Base: crates.io `wasmtime-internal-cache` 36.0.19, copied without source changes.

Change: the normalized `Cargo.toml` requires `zstd = "0.14.3"` instead of
`zstd = "0.13.0"`. This selects `zstd-safe` 8.2.0 and addresses the advisories
reported in master pipeline 6502 (RUSTSEC-2026-0391 and RUSTSEC-2026-0395
through RUSTSEC-2026-0406). Wasmtime 36, current newer releases, and upstream
`main` still require `zstd = "0.13.0"` as of 2026-10-26, so a normal lockfile
update cannot select the fixed implementation.

The cache crate uses only `zstd::encode_all`, `zstd::decode_all`, and
`zstd::DEFAULT_COMPRESSION_LEVEL`; all three APIs are present with the same
signatures in zstd 0.14.3. `Cargo.toml.orig` is retained unchanged as upstream
provenance and is not the manifest Cargo uses for this path dependency.

Remove this patch when the selected Wasmtime release natively accepts a
zstd/zstd-safe version clear of these advisories.
