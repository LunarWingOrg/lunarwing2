# Kawarimi import `--with-wasm` flag parity

> **Status: STILL-OPEN (source-verified 2026-07-12).** `import-tenant.sh`
> always adds `--with-wasm` internally but rejects an operator-supplied
> `--with-wasm`, unlike `build-tenant`.

## Current behavior

- The import parser rejects unknown flags at
  `ic/scripts/import-tenant.sh:51-69`; `--with-wasm` is not a case arm.
- The build argument array unconditionally includes `--with-wasm`
  (`import-tenant.sh:254-260`).
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
adds it internally. A future fix should either accept and ignore/record the
flag, or remove it from the documented import surface and explain that WASM is
unconditional. No code change is made in this documentation task.

## Verification record

Verified by current parser/build-argument inspection. The existing Kawarimi
flag harness (`ic/scripts/tests/test-kawarimi-import-flags.sh`) also passed for
the related worker-flag forwarding paths, but it does not exercise an explicit
`--with-wasm` argument. No tenant import or Cargo command was run.
