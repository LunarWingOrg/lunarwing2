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
  `WorkspaceWrite`; `read_only` may then be omitted. The virtual path must be
  exactly `/workspace`, not `/`, `/etc`, `../`, relative paths, or other
  paths; both RO and RW are a conflict. Multiple mounts and toolchain mounts
  are not implemented in version 1.
- The manifest names only the virtual destination. The host source of
  `/workspace` is the call's working directory (`workdir`), as for a call
  without a manifest. It is canonicalized and checked by the existing
  `mount_guard`, which refuses the protected paths and `/`, `/run` and
  `/var/run`, not every system directory; the runner continues to mask
  discovered protected paths and Unix sockets. Repository mount masking is *existing*
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
  by default. The single exception is `git-remotes` (WI#816), and only when
  the tenant operator sets `SANDBOX_GIT_REMOTES`. Browser, GUI, devices and
  other hardware receive no access.
- Unsupported or advisory-only behavior fails closed. The string `"proxy"` is
  accepted by the serde-shaped enum for an actionable policy denial, not exposed
  in the published request schema as a supported grant.

## Git remotes (WI#816)

```json
{
  "manifest_version": 1,
  "network": {"mode": "proxy-enforced"},
  "extensions": ["git-remotes"],
  "git": {"remotes": ["origin"]}
}
```

`extensions: ["git-remotes"]` asks for the host's per-tenant git grant. The host
grants it with `SANDBOX_GIT_REMOTES` as `name=url[,name=url...]`:

- `https://host/path` and `http://host/path` remotes need `proxy-enforced` or
  the legacy advisory `proxy`; they fail closed under `none` and `tunnel`
  because the grant cannot prove the remote is reachable only through the
  allowlist. The host must also allow the remote's host in the sandbox egress
  allowlist (`SANDBOX_EXTRA_DOMAINS`); the grant never adds a domain.
- `file:///workspace/...` remotes are virtual paths into the call's own
  workspace. They must canonicalize inside that workspace and work in every
  egress mode.
- URLs must not embed credentials or a userinfo component; such configuration
  is a load error.

Without the extension, git network operations inside a manifest-backed call are
refused with `capability_denied` before a runtime, proxy or credential is
started. `git.remotes` narrows the host list and can never add a remote; an
unknown name is a denial. Inside the call, a granted remote is used by alias
(`git push origin main`) or by its exact URL. Relative and scp-style remotes are
refused. `git push --force`, `--force-with-lease`, `-f`, `--mirror`, `--delete`,
`--prune`, a leading `+` refspec, and a leading `:` (ref deletion) refspec are
denied, so a force push and a remote ref deletion are both refused.

The container holds no credential. It receives the granted remote URLs as
`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` entries, a reset `credential.helper`,
`GIT_TERMINAL_PROMPT=0`, and `GIT_CONFIG_GLOBAL=/dev/null`. Private repositories
that need a credential are out of scope: use the host-side `ssh_git` tool,
which keeps keys in the harness agent and never enters the shell container.

`capability_evidence.git` records the grant identifier, supported egress modes,
and per-remote name, transport, host and read/write operation classes. It never
records a credential, token, key, URL userinfo, or helper command.

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
a transient runtime failure. A reduced network decision neither starts nor
stops the host's proxy or tunnel: its container gets `--network none` and
attaches neither, so the shared transports stay up for the other calls.

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
the existing host filesystem. WI#797 local measurement on the development host: about 20 µs per decision
including canonical mount checks, and about 15 µs per JSON audit serialization
(mean of 20 decisions and 1000 serializations, warmed test build). These are
samples for local documentation, not a portable performance guarantee or an
unattended benchmark.
