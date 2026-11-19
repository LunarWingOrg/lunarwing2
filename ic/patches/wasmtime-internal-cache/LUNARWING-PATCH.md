# LunarWing patch: advisory-fixed zstd for Wasmtime's compilation cache

Base: crates.io `wasmtime-internal-cache` 36.0.21, copied without source changes.
The upstream crate archive SHA-256 is
`1ff2377df26271c42d9649ac4ee13656ef89a51f02f0daa7917e67c635d9bc1d`.

Change: the normalized `Cargo.toml` requires `zstd = "0.14.3"` instead of
`zstd = "0.13.0"`. The v2.2.1.0 sweep resolved this to `zstd` 0.14.4 with
`zstd-safe` 8.3.0 (the requirement is a caret range, so later 0.14.x clears
the floor), and it addresses the advisories
reported in master pipeline 6502 (RUSTSEC-2026-0391 and RUSTSEC-2026-0395
through RUSTSEC-2026-0406). Wasmtime 36, current newer releases, and upstream
`main` still required `zstd = "0.13.0"` when this patch was introduced on
2026-10-26; the 36.0.21 crate still has that requirement, so a normal lockfile
update cannot select the fixed implementation. The 2026-10-30 rebase kept
this patch selected alongside the Wasmtime 36.0.20 fix for
RUSTSEC-2026-0424 (WI#524); it does not restore the old zstd acceptance.

The 2026-11-19 rebase (WI#767, v2.2.1.0 crate sweep) moves the base to
36.0.21 after `cargo update` took Wasmtime from 36.0.20 to 36.0.21. The patch
is pinned to the exact crate version, so leaving it at 36.0.20 made Cargo
silently drop the override and the locks fell back to `zstd` 0.13.5 /
`zstd-safe` 7.4.1. `src/` is byte-identical between 36.0.20 and 36.0.21, so
only the version and the `LICENSE` copy change; the `zstd` requirement stays
at 0.14.3 (resolving to 0.14.4 / zstd-safe 8.3.0, which remains advisory-clear).
Wasmtime 36.0.21 sits outside the WI#524 `deny.toml` ban
`wasmtime >=36.0.0, <36.0.20`, which exists to block downgrades below the
36.0.20 fix for RUSTSEC-2026-0424.

The cache crate uses only `zstd::encode_all`, `zstd::decode_all`, and
`zstd::DEFAULT_COMPRESSION_LEVEL`; all three APIs are present with the same
signatures in zstd 0.14.3. `Cargo.toml.orig` is retained unchanged as upstream
provenance and is not the manifest Cargo uses for this path dependency.

Remove this patch when the selected Wasmtime release natively accepts a
zstd/zstd-safe version clear of these advisories.
