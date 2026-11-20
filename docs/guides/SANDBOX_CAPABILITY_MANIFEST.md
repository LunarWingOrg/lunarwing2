# Sandbox capability manifest (WI#797)

The generic shell sandbox accepts an optional JSON `capability_manifest` on the
`shell` tool. `ic/src/sandbox/capability.rs` owns the version-1 schema,
request/default handling, host authorization, effective-policy decisions, and
audit serialization. The version-1 JSON Schema is also embedded in the shell
tool's parameter schema.

## Ownership and precedence

The state inspection for this WI found two separate execution systems. They
remain separate:

- **Generic shell**, `ic/src/sandbox/` and `ic/src/tools/builtin/shell.rs`:
  `SandboxConfig` selects the manager's compatibility policy and host limits;
  the manifest may reduce them. `mount_guard` canonicalizes the host working
  directory and guards protected paths. `CliRunner` enforces network, workspace,
  cgroup memory/CPU, the 256-process cap, timeout, and cancellation cleanup.
  `SandboxProxy` owns the public-IP/domain egress allowlist; `TunnelSidecar`
  owns provider identity, DNS, and its verified firewall.
- **Persistent worker containers**, `ic/src/orchestrator/job_manager.rs`: job
  token-scoped credential grants, MCP grants, mounts, and job iteration limits.
  This WI does not wire those containers to the shell manifest. External worker
  lanes remain governed by mt-admin's per-tenant container configuration and
  egress allowlists, not by this envelope. Do not infer that a shell manifest
  grants access to tools such as git or to a worker's credential grants.
- **FullAccess/local execution** remains a separately opted-in legacy path.
  A supplied manifest refuses disabled or absent sandbox configuration rather
  than executing on the host. A request cannot select FullAccess.

A request describes a minimum boundary. The host's `SANDBOX_NETWORK`,
`SANDBOX_POLICY`, resource ceilings, exported protected paths, and
`tunnel` settings are authoritative. Requests can select network `disabled`
(or `none`) or the exact configured `proxy-enforced` / `tunnel` mode. They must
not broaden networking, produce a fallback, select another tunnel provider or
identity, or pick the legacy advisory `proxy` mode. There are no arbitrary
runtime arguments, hosts, devices, or mount flags.

## Version-1 request and omitted fields

All top-level objects and nested structs reject unknown keys. Unknown versions
are rejected with an error naming the field and version-1 contract. The wire
size is bounded at 16 KiB.

```json
{
  "manifest_version": 1,
  "network": {
    "mode": "disabled"
  },
  "filesystem": {
    "read_only": ["/workspace"],
    "read_write": []
  },
  "resources": {
    "cpu_shares": 128,
    "memory_bytes": 536870912,
    "timeout_seconds": 60
  },
  "secrets": {"names": []},
  "extensions": []
}
```

- `manifest_version` is required and must be exactly `1`.
- Omitted `network` means `disabled`; it behaves as `none`. Overlay must be
  absent, and DNS cannot be selected. `proxy-enforced` permits only the existing
  daemon-chosen default DNS/public-proxy behavior. `tunnel` uses the existing
  host-configured provider; omitting its DNS preserves the host behavior,
  `provider` selects it, and `disabled` is refused. Incompatible network/DNS or
  overlay combinations fail rather than silently reconfiguring a sidecar.
- Omitted `filesystem` requests the existing virtual `/workspace` read-only.
  `read_write: ["/workspace"]` is allowed only when host `SandboxPolicy` is
  `WorkspaceWrite`. Mount paths must be exactly `/workspace`, not `/`, `/etc`,
  `../`, relative paths, or other host paths; both RO and RW are a conflict.
  Multiple mounts and toolchain mounts are not implemented in version 1. The
  host working directory is canonicalized and checked by the existing
  symlink/protected-path guard, and the runner continues to mask discovered
  protected paths and Unix sockets. Repository mount masking is *existing*
  enforcement, not a new grant of cross-tenant host access.
- Omitted `resources` preserves host limits. Present values must reduce or
  equal the host ceiling. `cpu_shares` is a scheduler weight, not a dedicated
  CPU count; `memory_bytes` must be an integer number of MiB; timeouts must
  be nonzero seconds. The shell's outer timeout is also reduced to the
  effective value. Disk remains `null` (unimplemented). Process/iteration caps
  stay host-controlled: 256 processes for this shell, and worker iteration
  settings on the separate job lane.
- Omitted `secrets.names` is empty. Every nonempty secret-list request is
  refused in this initial slice: this generic shell has no approved named
  secret resolver. Wildcards are refused by the capability-name grammar, as
  are empty or non-lowercase names. Existing worker-scoped job tokens and
  credential grants are unchanged. Requesting `git_credentials` here is a
  denial, not a secret lookup.
- Omitted `extensions` is empty; every named extension is reserved and denied
  by default. A future grant requires reviewed host policy and an enforcement
  backend. Browser, GUI, devices and other hardware receive no access.
- Unsupported or advisory-only behavior fails closed. The string `"proxy"` is
  accepted by the serde-shaped enum for an actionable policy denial, not exposed
  in the published request schema as a supported grant.

## Compatibility and retries

Omitting `capability_manifest` keeps the previous behavior only through the
explicit `legacy:configured-worker-policy` compatibility decision. A single
legacy invocation is **not** reinterpreted as `manifest_version: 1`; its network
mode, legacy proxy credential behavior, and FullAccess exception remain what
they were. Omitting individual fields inside an explicitly present manifest
does not select that compatibility policy.

Every declared execution validates and authorizes before runtime detection,
image lookup, proxy/tunnel startup, mounts, secret work, or a runtime launch.
It is validated again on each internal retry or shell resume/retry call,
including host working-directory canonicalization; no previous grant is
reused. Policy denials map to `[sandbox:capability_denied]` and are never
a transient runtime failure. A reduced network decision stops a previously
initialized wider transport before creating the reduced run.

The generated container argument is still constrained by `CliRunner`; it adds
`--network none` for an explicit network-disabled manifest and reduced cgroup
limits for reduced resource requests. Timeout, process-tree cleanup, in-flight
container cleanup, and the reaper remain those of the existing runner.
Shutdown releases initialized proxy/tunnel transport. A reduced network mode
must not leave a different mode as an implicit fallback.

## Evidence and boundaries

The effective decision is serialized into a `capability_audit` tracing field.
Successful manifest-backed shell results include `capability_evidence`. This
typed data is safe to persist in job metadata or an audit snapshot: canonical
host source, virtual destination, writable flag, network/provider/DNS mode,
`enforced` / `advisory` / `not-requested` status, effective resource limits,
absence of a disk cap, empty shell secret/extension grants, and origin/policy
ID. It contains no secret values, credentials, runtime env, or raw job token.
JSON serialization handles any diagnostic strings as data.

The evidence describes what this shell granted; it does not claim additional
host authorization or external-worker enforcement. Secret-store names are not
even accepted here, so values can never travel through this record.

Useful deny tests include unknown keys/versions, `read_write` under a read-only
host policy, unauthorized overlay/DNS mixes, `*` or other secret references,
over-ceiling resources, and cross-tenant or traversal paths. Container argument
inspection checks the accepted network and resource behavior without requiring
Podman or Docker. Existing runner tests cover actual timeout cleanup; existing
mount tests cover canonicalization, symlinks, and protected-path masks.

## Migration, overhead, and rollback

The shipped schema-v1 piece is intentionally a narrow vertical slice, not the
whole future capability surface. Existing callers need no migration: remove
`capability_manifest` to re-enter explicit legacy compatibility. New callers
should start with network disabled and read-only workspace, then request only
the existing narrowed network/write/access they require. Delete or ignore the
optional parameter to roll a caller back; restore the previous `ic` tree to
remove the new runtime contract. No database migration, manifest file, secret,
worker-image expansion, or shell-tool package installation is involved.

The focused `sandbox::capability` unit suite includes wall-clock samples of the
pure decision/audit serialization work; canonical mount validation also walks
the existing host filesystem. Samples are for local documentation only, not a
portable performance guarantee or an unattended benchmark.
