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

### Write grant (WI#860)

Read access is the default. `SANDBOX_GIT_WRITE_REMOTES=name[,name...]` grants
the `write` operation class to named remotes in the same `SANDBOX_GIT_REMOTES`
list; a name outside that list is a configuration load error, so the write
grant can never broaden the read grant. Without a write grant, `git push` is
refused with `capability_denied` before any credential or network work.

- **Force push is refused in every spelling** the preflight can see: `-f`,
  `--force`, `--force-with-lease[=ref]`, `--force-if-includes`, `--mirror`,
  short clusters containing `f` (`-qf`), and a `+` refspec.
- **Ref deletion is refused.** There is no host switch: a `:ref` or `+:ref`
  refspec, a bare `:` and `--delete`/`--del` are refused by the preflight, and
  the per-call proxy refuses any deleting push (below).
- **The preflight is an early, friendly refusal, not the boundary.** A script
  can still write `.git/config` (`remote.origin.mirror`, `remote.origin.push`,
  `alias.p`) or reach a remote with a client other than git.

**Where the push happens (plain `http://` remotes).** The per-call git proxy
reads every smart-HTTP request, so for `http://` remotes it, not the command
text, decides write and deletion:

- `git-receive-pack` (the advertisement
  `GET <repo>/info/refs?service=git-receive-pack` and
  `POST <repo>/git-receive-pack`) is forwarded only to a write-granted remote's
  own repository, host and port. On a read-only remote, on another repository
  of the same host, or in any other spelling or encoding, it is refused with
  `403`, with or without a bound credential and whatever client sent it.
- A push whose command list deletes a ref (an all-zero new object id) is
  refused, as are signed pushes (`push-cert`) and compressed, truncated or
  malformed push bodies.
- The per-call git proxy forwards only `GET`, `HEAD`, `POST` and `OPTIONS`, on
  any path: smart HTTP needs nothing else, so WebDAV writes (`git http-push`)
  are refused however the path is spelled.
- The upstream `Host` is the request's own host and port: a request whose
  `Host` header names another virtual host is refused, so a granted request
  and its credential cannot be steered elsewhere on the same address.
- `CONNECT` is opened only to the host and port of a granted `https://`
  remote; every other tunnel, a plain-HTTP remote's host included, is
  refused, so a client cannot tunnel around these checks.
- **A non-fast-forward update cannot be seen from the request**, so it is left
  to the forge: protect the target branches (no force push). For `https://`
  remotes, which travel in opaque `CONNECT` tunnels, the forge is the only
  write boundary: give read grants a read-only key and protect branches.

### Non-public remotes (WI#875)

A plain `http://` remote on the LAN or on an overlay (NetBird and other
`100.64.0.0/10` networks) is a normal deployment. In `proxy-enforced` mode
the proxy keeps its public-only destination check by default, so such a
remote must be allowed explicitly:

```
SANDBOX_GIT_PRIVATE_REMOTES=origin[,name...]
```

- **Named granted remotes only.** Each name must be a remote in the same
  `SANDBOX_GIT_REMOTES` list and must be `http://` or `https://`; a `file://`
  remote, an unknown name or an empty value is a configuration load error.
  The allowance opens exactly the named remote's own host and port. Another
  host on the same subnet, another port on the granted host, and every
  unnamed remote stay unreachable.
- **IP-literal LAN and overlay remotes only.** A named remote's URL host must
  be an RFC 1918 address, an address in the CGNAT range `100.64.0.0/10`, or
  an IPv6 unique-local address (`fc00::/7`), written as the literal address
  (`http://192.168.1.20:3000/…`, `http://100.64.0.7/…`). A host name is a
  configuration load error, and the proxy also refuses a non-public answer
  for any name, so no DNS answer can move the allowance to another private
  host. Loopback, link-local, the cloud metadata addresses
  (`169.254.169.254`, and `100.100.100.200` inside the CGNAT range) and every
  other non-public address stay refused.
- **The address is pinned.** The proxy connects to the literal address; a
  redirect cannot move the connection.
  If the allowance cannot be applied exactly, the call is refused as before.
- **The shared tenant proxy is unchanged.** Only the per-call git proxy
  carries the allowance; the shared proxy still refuses every non-public
  destination.
- **Evidence records it.** `capability_evidence.git.remotes[]` carries
  `"private_destination": true` for each allowed remote.

### Credentials (WI#860)

`SANDBOX_GIT_CREDENTIALS=remote=ENV_NAME[,remote=ENV_NAME...]` binds a granted
remote to a daemon environment variable. The value is the literal
`Authorization` header value (for example `Basic <base64>` or `Bearer <token>`).

- **The variable name is dedicated (WI#876).** It must be
  `SANDBOX_GIT_TOKEN_<NAME>`; any other daemon variable — `LLM_API_KEY`,
  `DATABASE_URL`, or a plain `GIT_ORIGIN_TOKEN` — is a load error, so a
  binding cannot ship an unrelated secret to that remote.

- **The credential stays host-side.** The per-call egress proxy resolves
  `ENV_NAME` and adds the header to the outgoing request. The container never
  receives it: not in its environment, files, command arguments, process
  listings or output. There is no credential helper and no socket a process in
  the run can query.
- **The credential reaches only its repository's endpoints.** The header is
  added only to the bound remote's own smart-HTTP requests (`info/refs`,
  `git-upload-pack`, and `git-receive-pack` for a write grant) on its exact
  host and port, and it is that remote's own credential even when two
  remotes share a host. Another repository on the same host, the forge's API, a
  dumb-HTTP read and any other path get no credential. Configure the remote's
  canonical smart-HTTP URL (with `.git` where the forge redirects to it).
- **Only plain `http://` remotes can be bound.** HTTPS travels in an opaque
  `CONNECT` tunnel the proxy cannot modify, so binding an `https://` remote is a
  configuration load error rather than a silent no-op. Use `http://` for
  credential-bound remotes, or configure the forge to accept an unauthenticated
  read. The hop from the host to the remote is plain HTTP, so it belongs on a
  trusted network or an encrypted overlay. The per-call proxy keeps
  `proxy-enforced`'s public-only destination check, so today a remote whose
  name resolves to a private, CGNAT (including NetBird's `100.64.0.0/10`) or
  loopback address is refused.
- **A binding must name a granted remote**, and is applied only when that
  remote is selected for the call.
- **Egress modes.** Credential injection works in `proxy-enforced`, the only
  mode that carries HTTP(S) remotes. `none` and `tunnel` carry `file://`
  remotes only; a network remote in either mode is already denied. Credentials
  are therefore never silently absent: the mode either supports the binding or
  the call fails closed at authorization.

The manifest preflight checks recognizable Git commands before starting a
runtime. Use a literal granted URL for cloning and name an explicit granted
remote for fetch, pull or push with networking enabled. Unsupported read
options are denied instead of guessing which argument is the remote. With
network disabled, ordinary local reads such as `git clone . copy`, `git fetch`
and `git pull` can run without a grant. Operations that write to the local
workspace still need a writable mount. Calls that omit the manifest retain
legacy behavior and do not use this preflight or the scoped proxy.

The shell scan is not a security boundary: Git configuration and scripts can
change what actually runs, and HTTPS CONNECT does not expose repository paths
or Git operations to the proxy. Manifest preflight rejects the remote
transport plumbing — `send-pack`, `fetch-pack`, `http-push`, `http-fetch` and
the `remote-*` helpers, whether named as a `git` subcommand or invoked by
path.
Git's dispatcher runs `git <verb>` as the program `git-<verb>` in its exec
path (`/usr/lib/git-core/git-push`, or a bare `git-push` on `PATH`), so a
dashed program is read as the subcommand it stands for: `git-push -f` is
refused exactly as `git push -f` is, and `git-fetch`, `git-clone`,
`git-config` and the rest get the subcommand's own checks. A dashed program
takes the subcommand's arguments, not `git`'s global options, so none are
skipped ahead of it. `git submodule--helper` — the program behind
`git submodule` — is read like `git submodule`: its `foreach` body is
scanned with the helper's own option rules (unambiguous long-option
abbreviations included), and its other subcommands are refused.

For `git submodule`, preflight permits the default listing, `status` and
`summary`, and scans `foreach` bodies. For `add`, it skips supported options
and validates the repository operand. With a Git grant, that operand must be
an explicit granted URL or its equivalent absolute `/workspace` path.

Preflight refuses `update` regardless of flags because Git can clone or
fetch using a URL from `.gitmodules` or local configuration. It also refuses
`init`, `sync`, and `set-url`, which can change submodule remote URLs.
Local actions `deinit`, `absorbgitdirs`, and `set-branch` are intentionally
unsupported. To clone or fetch submodule content, use a literal granted URL
for `git clone` or name a granted remote in `git fetch`. Git can still fetch
within already-populated submodules according to its recursion settings.
Those implicit fetches are outside the command preflight's visibility.

Preflight also refuses in-command configuration that would change what a later
word means: `-c alias.*`, `-c include.*` and `-c includeIf.*` (which can define an
alias or rewrite a remote), the redirecting `remote.*`, `url.*`,
`credential.*` keys, HTTP `extraHeader` (including URL-scoped keys),
`help.autocorrect`, and the Git editor and filesystem monitor keys. It
refuses `git config` writes and renames involving those keys and refuses
`git config edit` outright. Literal reads are allowed. It also refuses
recognized writes to `GIT_CONFIG_*`, `GIT_EDITOR` and `GIT_SEQUENCE_EDITOR`.
These refusals are preflight only: a script can still write `.git/config`
directly. The scan is not a repository write filter for arbitrary shell
processes: for `http://` remotes the per-call proxy enforces write and deletion
(see the write grant above), and for everything else, and for non-fast-forward
updates, use server-side permissions and branch protection.

The scan recognizes GNU `env`'s split-string option in its separate, attached,
and clustered spellings (`-S body`, `-Sbody`, `-iS body`, `--split-string body`,
`--split-string=body` and the long-option abbreviations). Preflight uses GNU
`env`'s literal splitting rules, not shell command separators. It appends the
remaining arguments, rechecks `env`'s options, and inspects the resulting
single command. Dynamic expansions and invalid split strings are refused.

The scan recognizes common wrappers, grouping prefixes, separate `-c` script
operands (including `bash -cex 'git status'`), and literal `rebase --exec`,
`submodule foreach` and `bisect run` bodies. `submodule foreach` is read as
the whole operand tail, because the later operands are part of what runs:
git starts the words directly (`foreach sh -c '<body>'`), or, when the first
operand has a shell metacharacter, runs it as a script with the later
operands as positional parameters, `"$@"` joined to its end. So a tail is
scanned too, not just a single quoted body. When later operands are present,
a first operand that contains `$` or a backtick is refused as dynamic,
because it could run those operands where the scan does not read them
(`foreach '"$@" #' git push -f …`); with one operand, `foreach 'echo $name'`
stays an ordinary body. `rebase` exec is read from the
short-option cluster the way git reads it: a value-taking letter (`-s`, `-X`,
`-C`, `-S`, `-r`) consumes the rest of the cluster, so `-ix` and `-vix` run
their body, `-ix<body>` attaches it, and `-sx`/`-Xx`/`-C3x`/`-rx` are that
option's value rather than an exec. It does not fully model shell
expansion (including globs, braces and command substitutions), dynamic
command names, alias or command lookup, text evaluated by commands such as
`eval` or `trap`, or shell scripts supplied on stdin. Unsupported
forms, including indirect writes to shell variables, are not guaranteed
to be refused.
Ordinary commands such as `grep git README`, `echo git push`, and
`env FOO=1 git status`, `timeout 30 git fetch origin`, `bash -c 'git status'`
keep passing. `git config` is read in the classic form and in the 2.46
`list`/`get`/`set`/`unset`/`rename-section`/`remove-section`/`edit`
subcommands, with git's long-option prefixes (`--fil` is `--file`) and the
rename target checked; any edit is refused, since its editor can write any
key. `git remote` reads options before the action word, so `git remote -v
set-url origin <url>` is checked. `git_subcommand` steps over every
value-taking global option, `--attr-source` included.

The container receives the granted remote URLs as
`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` entries, a reset `credential.helper`,
`GIT_TERMINAL_PROMPT=0`, and `GIT_CONFIG_GLOBAL=/dev/null`. Each remote is
pinned with an empty `remote.<name>.url` first, which clears the URL list, so
a repository-local URL does not outrank the granted one while the process
retains the host's Git configuration environment entries.
The separate host-side `ssh_git` tool is unchanged.

`capability_evidence.git` records the grant identifier, supported egress modes,
and per-remote name, transport, host and operation classes (`read`, and
`write` where granted). It also records each credential binding as the remote
name plus the daemon variable *name*. It reports the grant, not the operation
a process actually performed. It never records a credential value, token, key,
URL userinfo, or helper command.

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
