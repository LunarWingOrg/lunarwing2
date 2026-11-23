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
  egress allowlists, not by this envelope. The generic shell's Git extension
  does not grant access to a worker's credentials.
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

## Git remotes (WI#816, WI#860)

```json
{
  "manifest_version": 1,
  "network": {"mode": "proxy-enforced"},
  "extensions": ["git-remotes"],
  "git": {"remotes": ["origin"]}
}
```

`extensions: ["git-remotes"]` requests a Git grant. The tenant operator
configures `SANDBOX_GIT_REMOTES` as `name=url[,name=url...]`;
`git.remotes` can select a subset by name but cannot add a remote.

Every configured remote grants `read`. The operator promotes remotes to
`write` with `SANDBOX_GIT_WRITE_REMOTES` (`name[,name...]`); a name there must
already exist in `SANDBOX_GIT_REMOTES`, so the write list can never add a
destination. A read-only grant keeps refusing `git push`.

- HTTP(S) remotes require `proxy-enforced`; `none`, `tunnel`, and advisory
  `proxy` do not carry them. All proxy traffic for that call is restricted to
  the exact selected hosts. Each must already match the tenant sandbox egress
  allowlist (`SANDBOX_EXTRA_DOMAINS` plus defaults); the grant adds no domain.
- `file:///workspace/...` paths must initially resolve inside the call's
  workspace. They work in `none`, `proxy-enforced`, and `tunnel`. A file-only
  grant in `proxy-enforced` has no upstream hosts. A file-only grant in
  `tunnel` does not change the existing tunnel's policy.
- Configured URLs must not contain userinfo, a query, or a fragment. Invalid
  configuration is reported without printing the supplied value.

The manifest preflight checks recognizable Git commands before starting a
runtime. Use a literal granted URL for cloning and name an explicit granted
remote for fetch or pull with networking enabled. Unsupported read options
are denied instead of guessing which argument is the remote. With network
disabled, ordinary local reads such as `git clone . copy`, `git fetch` and
`git pull` can run without a grant. Operations that write to the local
workspace still need a writable mount. Calls that omit the manifest retain
legacy behavior and do not use this preflight or the scoped proxy.

The shell scan is not a security boundary: Git configuration and scripts can
change what actually runs, and HTTPS CONNECT does not expose repository paths
or Git operations to the proxy. Manifest preflight authorizes a recognized `git
push` only to a write-granted remote and refuses force push and ref deletion in
every spelling; it likewise rejects the remote transport
plumbing — `send-pack`, `fetch-pack`, `http-push`, `http-fetch` and the
`remote-*` helpers, whether named as a `git` subcommand or invoked by path.
It also refuses in-command configuration that would change what a later word
means: `-c alias.*`, `-c include.*` and `-c includeIf.*` (which can define an
alias or rewrite a remote), the redirecting `remote.*`, `url.*`,
`credential.*` and `http.extraheader` keys, `git config` writes to those same
keys, and shell assignments to `GIT_CONFIG_*`. These refusals are preflight
only: a script can still write `.git/config` directly. This is not a
repository write filter for arbitrary shell processes. Use server-side
permissions and branch protection to enforce repository write policy.

### Push (WI#860)

A write grant authorizes `git push` to that remote. The preflight names the
remote (or its exact granted URL) and refuses, in every spelling:

- force push: `+src` refspecs, `--force`, `-f`, `-qf`-style short clusters,
  `--force-with-lease`, `--force-if-includes`, `--mirror`;
- ref deletion: `--delete`, `-d`, `--prune`, and a `:dst` or empty-source
  refspec.

`--all`, `--branches` and `--tags` are refused because they rewrite refs the
call did not name, and `--repo`/`--receive-pack`/`-o` are refused because they
retarget the push or run an arbitrary receive-pack. A push must name an
explicit remote; a default destination is refused.

### Credentials (WI#860)

The tenant operator sets `SANDBOX_GIT_CREDENTIALS` as
`host=SECRET_NAME[,host=SECRET_NAME...]`. The secret value stays in the
daemon. For a call whose granted remotes include that host, the daemon starts
a credential broker on a Unix socket and bind-mounts its directory read-only
at `/run/lunarwing-git-credential`. The container's git runs a generated
helper (`/run/lunarwing-git-credential/helper.sh`) that speaks the
git-credential protocol to that socket; the broker answers only for the exact
bound hosts and only with `username`/`password` lines. The secret never
appears in the container's environment, files, arguments, process listings or
command output.

A binding for a host no selected remote uses is a `capability_denied`, so a
typo fails closed instead of silently granting nothing. Credentials bind to
`http`/`https` remotes; a `file` remote needs none.

The container otherwise receives the granted remote URLs as
`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` entries, a reset `credential.helper`,
the broker helper when credentials apply, `GIT_TERMINAL_PROMPT=0`, and
`GIT_CONFIG_GLOBAL=/dev/null`. The separate host-side `ssh_git` tool is
unchanged.

`capability_evidence.git` records the grant identifier, supported egress modes,
and per-remote name, transport, host and its operation classes (`read`, and
`write` when granted). It reports the grant, not the operation a process
actually performed. It never records a credential, token, key, URL userinfo, or
helper command.

### Egress modes

- `proxy-enforced`: supported. The per-call proxy allows only the selected
  hosts, and the broker socket is the only credential path.
- `none`: supported for `file` remotes. Network remotes are refused.
- `tunnel`: supported for `file` remotes. Network remotes and credentials are
  refused, because the tunnel does not carry the per-call host restriction.
- advisory `proxy`: refused. It cannot enforce either the host set or the
  credential boundary.

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
a transient runtime failure. A network-disabled decision neither starts nor
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
absence of a disk cap, empty shell secret grants, granted extensions, and
origin/policy ID. It contains no secret values, credentials, runtime env, or
raw job token.
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
