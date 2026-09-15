# Provenance — vendored sherpa-onnx native libraries

## Source artifact

- Upstream release asset (v1.12.15, linux-x64, shared):
  `sherpa-onnx-v1.12.15-linux-x64-shared.tar.bz2` from
  https://github.com/k2-fsa/sherpa-onnx/releases/tag/v1.12.15
- Tarball sha256: `be6a1f660622f98169ce8667773d1cc3e34eec16a0ae63aecf126f571b67e500`
  (26,847,553 bytes)
- Cross-checked byte-identical to the `checksum.txt` entry in sherpa-rs at
  pinned rev `857bedd35ec5215c65b4911fdadca31f403b6f6b` — i.e. the exact
  artifact the former `download-binaries` sha256 gate would have accepted.
  Equivalence is the core safety argument of the vendoring change.

Only `lib/` is committed. Excluded on purpose:

- `bin/` (~36 CLI tools, ~100 MB) — the sidecar never invokes them.
- Headers — bindgen compiles against the `sherpa-onnx/` git submodule pin
  `b68e69dd1a41dff6bcb8d31bf91dd3ba6b0dc568` (recorded at sherpa-rs rev
  857bedd3); `SHERPA_LIB_PATH` steers libraries only, never headers.

## Verification

- Downloaded once (2026-09-13), hashed independently of upstream metadata,
  extracted, per-file hashes recorded in `SHA256SUMS` (this directory).
- `docker build` runs `sha256sum -c SHA256SUMS` before `cargo build`, so a
  corrupted or missing library fails the build, not the runtime.
- Runtime contract unchanged: build.rs copies the `.so`s next to the binary;
  the image stages them into `/app/lib` with `LD_LIBRARY_PATH=/app/lib`.

## Rotation

Rotation = new upstream release: download, hash, compare against the new
sherpa-rs pin's `checksum.txt`, replace `linux-x64/lib/*`, update
`SHA256SUMS` and the pins above in the same commit.
