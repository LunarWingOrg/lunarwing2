# Multi-tenant Nanocode image distribution is too large

> **Status: STILL-OPEN (verified against HEAD 2026-07-12).** The worker image
> build and healthcheck issues from the 1.1.4 systemd pass are fixed, but the
> Nanocode image remains large and is copied into each rootless tenant store.

## Finding

The historical systemd pass recorded a roughly 6 GB Nanocode image. Per-tenant
`save | load` distribution is slow and disk-heavy and can fail under disk
pressure; a retry succeeded in the original validation. This is an operational
capacity issue, not a correctness failure in the worker protocol.

## Current evidence

- `_ensure_tenant_image` still distributes images with `save | load` and warns
  that large images can take minutes (`ic/scripts/lunarwing-mt-admin.sh:585-629`).
- `start_tenant_nanocode` calls that helper before starting the worker
  (`ic/scripts/lunarwing-mt-admin.sh:3428-3433`).
- The image Dockerfile installs a broad runtime/toolchain package set
  (`lunarcode4lunarwing/Dockerfile:45-120`), and no slim/shared-image or
  additional-image-store mechanism is implemented in this tree.

The exact 6 GB measurement is host/image-version dependent and is retained as
historical evidence rather than a guaranteed current size. The unresolved
engineering options are to slim the runtime image or configure a shared
additional image store; neither is implemented here.

## Related fixes, now closed

The same 1.1.4 report's F1-F10 and F12 findings are resolved in the current
scripts: fully-qualified image defaults, non-fatal readiness handling, init
gating, resumable tenant setup, and `--format docker`/Quadlet fixes. They are
historical context, not duplicate open bugs.

## Verification record

Status is based on current script/Dockerfile inspection and the retained
systemd validation record. No image build, container run, or Cargo command was
performed in this pass.
