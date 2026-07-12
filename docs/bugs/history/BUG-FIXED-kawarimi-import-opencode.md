# Kawarimi import worker-flag parity

> **Status: FIXED for the named issue (verified 2026-07-12).** The former
> `BUG-kawarimi-import-no-opencode.md` is archived here. The separate
> `--with-wasm` parser mismatch is tracked in
> [`../BUG-kawarimi-import-flag-parity.md`](../BUG-kawarimi-import-flag-parity.md).

## Historical failure

On v1.1.8 migration testing, `import-tenant.sh <bundle> --with-opencode`
returned `unknown arg: --with-opencode`, forcing a separate worker build after
import. The old report's parser/build line ranges are historical.

## Current fix

- The parser accepts `--with-opencode` at
  `ic/scripts/import-tenant.sh:58-63` (line 60).
- The option is forwarded to `add-tenant` at `:238-246` (line 245).
- It is forwarded to `build-tenant` at `:254-261` (line 259).
- The shell regression harness asserts both forwarding paths at
  `ic/scripts/tests/test-kawarimi-import-flags.sh:149-176`; a fresh run ended
  `ALL TESTS PASSED`.

The feature fix is present in the v2 snapshot and the later worker-selection
commit `fef7bbb` is an ancestor of `HEAD`. Runtime cross-machine import was not
run in this documentation pass.

The root cause was a parser/build-argument mismatch introduced when OpenCode
was added: the import parser only recognized Nanocode and Pebble while the
build path could already accept worker selections. Before the fix, the
workaround was to run `build-opencode-worker` separately and then restart the
tenant; that workaround is no longer needed for `--with-opencode`.

## Verification record

Verification used current shell source, the existing shell harness result, and
Git ancestry. No Cargo/build command or live tenant migration was run.
