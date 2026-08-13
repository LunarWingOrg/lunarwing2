# Kawarimi import `--with-wasm` flag parity

> **Status: STILL-OPEN (source-verified against current source on 2026-08-13).**
> `import-tenant.sh` always adds `--with-wasm` internally but rejects an
> operator-supplied `--with-wasm`, unlike `build-tenant`.

## Current behavior

- The import parser rejects unknown flags at
  `ic/scripts/import-tenant.sh:85-105` (reject arm at `:102`); `--with-wasm` is
  not a case arm.
- The build argument array unconditionally includes `--with-wasm`
  (`import-tenant.sh:1043`).
- The related `--with-opencode` omission is fixed and documented in
  [`history/BUG-FIXED-kawarimi-import-opencode.md`](history/BUG-FIXED-kawarimi-import-opencode.md).

Thus an operator who mirrors the `build-tenant` flag surface receives
`unknown arg: --with-wasm`, even though WASM is always built by the import flow.
This is a CLI/documentation parity defect, not a worker-build failure.

## Reproduction

```bash
sudo ic/scripts/import-tenant.sh /path/to/bundle.tar --with-wasm
# unknown arg: --with-wasm
```

The normal import command without the flag still builds WASM because the script
adds it internally. The import flag surface has since grown: `--with-kimi` was
added with full parity (`import-tenant.sh:94,1046`), `--with-vision` is also
accepted (`import-tenant.sh:96`), and the retired
`--with-nanocode` flag demonstrates a third resolution option — recognize the
flag and reject it with an explicit explanation (`import-tenant.sh:91`, covered
by the flag harness). A future fix should accept and ignore/record
`--with-wasm`, reject it with a clear "WASM is always built" message, or remove
it from the documented import surface. No code change is made in this
documentation task.

## Verification record

Verified by current parser/build-argument inspection. A fresh run of the
Kawarimi flag harness (`ic/scripts/tests/test-kawarimi-import-flags.sh`) passed
all cases, including the related worker-flag forwarding paths, but it does not
exercise an explicit `--with-wasm` argument. No tenant import or Cargo command
was run. Re-verified on 2026-08-13 (revision `7e813db4`): the parser
(`import-tenant.sh:85-105`, reject arm `:102`), unconditional
`--with-wasm` build argument (`:1043`), and `--with-kimi` parity (`:94,1046`)
are all unchanged since the previous pass.
