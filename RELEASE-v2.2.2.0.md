# Release Notes for LunarWing v2.2.2.0

## Codename: Kuran (クラン) — Clan

**Release Date:** 2026-11-27

## Highlights

- **The shell sandbox accepts a declarative capability manifest (WI#797).** A
  `shell` call may carry a version-1 `capability_manifest` that can only narrow
  what host policy already allows: network mode, filesystem mounts, CPU shares,
  memory and timeout. A request that carries an unknown key, a secret name, or
  an extension host policy does not grant is refused rather than down-graded —
  the generic shell has no secret injection at all — and a manifest refuses a
  host with no sandbox configuration instead of executing on the host.
- **A shell call can use git without the container holding a credential
  (WI#816).** `extensions: ["git-remotes"]` grants credential-free git access
  to remotes the tenant operator names in `SANDBOX_GIT_REMOTES`; a remote
  outside that list is refused. No credential is injected, which is what keeps
  the grant from writing to an authenticated remote. Host-held authentication
  and push from inside the shell are **not** part of this release.
- **`upgrade-tenant` rebuilds the worker containers it used to leave behind
  (WI#796).** Both upgrade paths refresh each enabled lane's container after
  promotion, so a lane that had soft-skipped no longer keeps serving a
  pre-upgrade image while the upgrade reports green. An enabled lane whose
  image is missing now returns **3** instead of silently succeeding, and a
  bridge Quadlet that fails to start returns **5**.
- **A detached turn's late budget scales with the work it did (WI#807,
  WI#837).** The late watcher keeps a 600-second base, earns 90 seconds for
  every completed tool action, is re-read as it waits, and is capped at 1,800
  seconds. Every turn, foreground and late, leaves one `Turn finished` record
  with `phase`, `tool_calls`, `budget_secs`, `budget_exhausted`, `elapsed_ms`
  and `outcome`, so a bounded task with a known step count becomes a
  measurable data point. The TUI's own sealing windows move with it (WI#837).
- **Tool runtime calibration reports what it has learned (WI#318).** One
  `ToolEstimates` per tool registry observes completed calls and exposes
  read-only `GET /api/estimates` (user-scoped) plus owner-only counters, a
  held-out evaluation trace, LLM call timing and gate-wait diagnostics. Hints
  in approval text stay opt-in (`LUNARWING_TOOL_ESTIMATES=1`); the default is
  observe-only, and no learned factor survives a restart. LLM logical-call
  timing (`LUNARWING_LLM_TIMINGS`) observes by default, daemon-wide; `0`
  disables it.

## Sandbox and shell access

- **The capability manifest is a narrowing envelope, not a grant (WI#797).**
  `ic/src/sandbox/capability.rs` owns the version-1 schema; the JSON Schema is
  also embedded in the `shell` tool's parameter schema. The wire document is
  bounded at 16 KiB, all objects reject unknown keys, and an unknown version is
  refused with an error naming the field. The request may pick network
  `disabled`/`none` or the exact configured `proxy-enforced`/`tunnel` mode, may
  select read-only and read-write workspace paths and lower resource ceilings.
  Secret names are refused outright: the generic shell has no secret injection,
  and worker jobs keep their host-issued credential grants. It cannot add a
  host, a device, a mount flag, or a runtime argument, and it cannot produce a
  fallback; it cannot select FullAccess; and the legacy advisory `proxy`
  spelling is refused with an actionable message rather than mapped onto
  another mode. The `capability_evidence` record names the request, the
  effective policy and the refusals, and never records a secret value. The
  guide is
  [`docs/guides/SANDBOX_CAPABILITY_MANIFEST.md`](docs/guides/SANDBOX_CAPABILITY_MANIFEST.md).
- **Git remotes are the one grantable extension, and the grant carries no
  credential (WI#816).** `SANDBOX_GIT_REMOTES` holds `name=url[,name=url...]`;
  `"extensions": ["git-remotes"]` with `"git": {"remotes": ["origin"]}` selects
  a subset by name and cannot add one. HTTP(S) remotes require
  `proxy-enforced` and every host must already be on the tenant sandbox
  egress allowlist (`SANDBOX_EXTRA_DOMAINS` plus defaults, not the worker
  egress allowlists `mt-admin` keeps); `file:///workspace/...` remotes work in
  `none`, `proxy-enforced`
  and `tunnel` when they resolve inside the call's workspace. The container
  receives the granted URLs as `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`, a reset
  `credential.helper`, `GIT_TERMINAL_PROMPT=0` and `GIT_CONFIG_GLOBAL=/dev/null`
  — no credential. The manifest preflight rejects recognized `git push`
  invocations, including a plain push, because repository-local settings can
  change what runs; it also denies unrecognized read options rather than
  guessing which argument is the remote. Unlisted plumbing verbs such as
  `git send-pack` are not refused by it (WI#844). Calls that omit the manifest keep the
  old behavior and skip both the preflight and the scoped proxy. The guide's
  wording is explicit that the shell scan is not a security boundary.
- **The git preflight refuses more routes around its own check (WI#816,
  WI#844).** It denies git's remote transport plumbing (`send-pack`,
  `fetch-pack`, `http-push`, `http-fetch`, `remote-*`, also invoked by path),
  in-command configuration of `alias.*`, `include.*`/`includeIf.*` and the
  redirecting `remote.*`, `url.*`, `credential.*` and `http.extraheader` keys,
  `git config` writes to those keys and shell assignments to `GIT_CONFIG_*`,
  and it reads the option arity of `remote add`/`set-url` so the checked word
  is the URL. These are preflight refusals: the grant still carries no
  credential, and the per-call proxy still bounds egress.
- **A dashed program and `git submodule--helper` are read as the subcommand
  they are (WI#886).** Git's dispatcher runs `git <verb>` as the program
  `git-<verb>` in its exec path (`/usr/lib/git-core/git-push`, or a bare
  `git-push` on `PATH`), so `git-push -f` is refused exactly as `git push -f`
  is, and `git-fetch`, `git-clone`, `git-config` and the rest get the
  subcommand's own checks. A dashed program takes the subcommand's arguments,
  not `git`'s global options, so none are skipped ahead of it.
  `git submodule--helper` — the program behind `git submodule` — is read like
  `git submodule`: its `foreach` body is scanned with the helper's own option
  rules (unambiguous long-option abbreviations included), and its other
  subcommands are refused
- **`git submodule` is authorized per action (WI#888).** The default listing,
  `status` and `summary` are permitted and a `foreach` body is scanned. `add`
  skips supported options and validates the repository operand, which under a
  grant must be an explicit granted URL or its equivalent absolute
  `/workspace` path. `update` is refused regardless of flags, because Git can
  clone or fetch using a URL from `.gitmodules` or local configuration; so are
  `init`, `sync` and `set-url`, which can change submodule remote URLs.
  `deinit`, `absorbgitdirs` and `set-branch` are intentionally unsupported.
  Before this, the action dispatch fell through to `Ok(())`, so `update
  --init`/`--remote` and `sync` reached `.gitmodules` remotes with no grant
  check. Git can still fetch within already-populated submodules according to
  its recursion settings; those implicit fetches stay outside the preflight's
  visibility.
- **An `argv[0]` override, a glob and `for-each-repo` are read (WI#905).**
  For `env -a`/`--argv0` and `exec -a`, the preflight reads the final
  `argv[0]` by basename when the executable is Git, so a replacement such as
  `git-fetch` uses the fetch checks; an expansion in Git's `argv[0]` is
  refused. Unquoted globs that name Git programs are refused in command
  positions and in the arguments of wrappers the preflight cannot read: it
  looks for a literal character or a single-character class in the final path
  component and checks the pattern against Git program names, including dashed
  names in `git-core`. Generic patterns such as `*`, `dir/*` and `???` pass,
  and a single `case` pattern at a pattern delimiter is exempt. When Bash
  enables `extglob` with `-O extglob` or a literal `shopt -s extglob`, extended
  globs in command words are refused. Literal brace lists and ranges expand
  into separate arguments before the scan reads commands, wrapper arguments,
  or Git options and operands; quoted and escaped braces stay literal, as does
  `{}`, and a brace expansion the preflight cannot resolve is refused. For
  `git for-each-repo`, the arguments after the recognized options are checked
  as a Git command, including global options after the `--` separator.
- **The relay's accept loop no longer dies at 32 connections (WI#834).** The
  host relay (`ic/src/sandbox/proxy/relay.rs`) reaps finished connections, so
  its 32-connection cap bounds live connections only and the lifetime
  accept count keeps rising. A connection over the cap waits on the socket for
  a slot. A failed `accept` (for example `EMFILE`) is logged and retried after
  a delay that doubles from 10 ms to 1 s and resets on the next success. The
  in-container `relay.py` keeps the same 32-connection cap.
- **A failure is attributed to the proxy only with evidence (WI#836).** A run
  is classified `proxy_error` only when the exit code is 125, a line *starts*
  with `lunarwing-proxy-relay:`, and the host relay accepted no container
  connection during the run. A sandboxed command that prints the marker itself
  keeps its own exit code and stderr. `relay.py` binds its listener before the
  startup probe, so a failed startup leaves the count at zero. The rule is
  recorded in `ic/src/NETWORK_SECURITY.md`

## Worker containers and external lanes

- **Kimi Code moves to `2.7.2` (WI#804).** `2.7.0` (2026-11-19 11:24Z) was
  superseded by `2.7.1`, and `2.7.1` by `2.7.2` (2026-11-20 09:15Z), both
  inside the earlier release's 24-hour window, so neither was ever pinned.
  `2.7.2` was taken after its own window closed at 2026-11-21 09:15Z. The
  Dockerfile `ARG KIMI_CODE_VERSION`, `kimi-cli/package.json`, its lockfile,
  `.env.example`, the build script's `source_ref`, the contract tests and the
  bump entry in [`docs/ops/WORKER-CONTAINERS.md`](docs/ops/WORKER-CONTAINERS.md)
  move together. The three 2.7.x releases add one feature flag
  (`KIMI_CODE_EXPERIMENTAL_PROMPT_PREDICTION`, TUI-only and off by default) and
  export the task's own session and workspace identifiers; the lane takes
  neither as input. The pinned 2.7.2 CLI also carries two subcommands that
  start a listener: `kimi rc` (alias `kimi remote`, the local server and web UI
  through Remote Control) and `kimi vis` (the session visualizer, with
  `--port`/`--host`). Since WI#852 the guard denies both (below). The real-CLI contract gate passed 4/4 on this
  host, and the guard was shown blocking a mock-provider `sudo id` end to end.
- **OpenCode 2 and OpenCode v1 were re-checked and did not move (WI#804).**
  `git ls-remote --tags` on `anomalyco/opencode` still resolves the newest
  `v2.*` tag to `v2.3.2` at `c0ad18b40…` and the newest `v1.*` tag to
  `v1.19.6` at `f4e363c82…`, the commits already pinned. No image was rebuilt.
  The opencode2 flag gates (`console_watches`, `mcp_events`, `artifacts`) were
  re-read at that commit, and the compile-out anchor was re-run: it exits 0
  with "flag pin anchor text unchanged". Pebble's upstream HEAD is still
  `2d0f3786…`; LunarDepth's HEAD has moved to `9eb615d8…` from the pinned
  `0472d115…`, and a bump of either is its own item.
- **The guard reads the punctuation bash glues to a word (WI#839, WI#853).**
  A subshell `(…)`, a `$(…)` or backtick substitution (also inside double
  quotes) and a glued `)` or `}` no longer hide a listener, a git discard or a
  recursive `chmod`/`chown` of a system root: `(kimi server)`,
  `$(git clean -fdx)` and `(chmod -R 777 /etc)` take the plain spellings'
  verdicts.
- **Kimi's options and subcommands are read as the pinned CLI reads them
  (WI#845, WI#846, WI#852).** The listener check reads the 2.7.2 option
  table, so an option value (`kimi --model X server`) cannot stand in for the
  subcommand, and an option the table does not name is read both ways.
  `kimi rc`, `kimi remote` and `kimi vis` are denied with `server` and `web`,
  and a `find -exec`/`-execdir`/`-ok`/`-okdir` body is read like an `xargs`
  body.
- **The git and file rules read options as the tools do (WI#841, WI#842,
  WI#843).** Git discard options match by any GNU-unambiguous prefix
  (`git reset --har`), `chmod`/`chown`/`chgrp` match `--rec` and longer, and a
  git alias or include definition is refused before the verb is read: inline
  `-c`/`--config-env` (after git's own value options), the
  `GIT_CONFIG_KEY_*`/`GIT_CONFIG_PARAMETERS` spellings,
  `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`, and `git config` writes, reads,
  renames and edits of `alias.*`, `include.*` and `includeIf.*` keys, matched
  without regard to case. An alias already in a config file stays an accepted
  residual.
- **A rename or `tar --remove-files` cannot launder a protected tree
  (WI#849).** An `mv` whose source or destination names, contains or sits
  inside `/home/kimi/.kimi-code`, `/app` or `$KIMI_CODE_HOME`, or is a system
  root, is denied, as is one holding a variable, a substitution, `~user` or a
  `{}` placeholder; ordinary workspace renames pass. Every member of
  `tar --remove-files` takes the temp-root test.
- **Perl deletion reads its operands exactly (WI#851).** File::Path option
  hashes and `rmtree`'s legacy flags are not targets; `glob()`, `<…>`,
  `qw()` and the implicit-`$_` forms are read as deletions; and an operand
  that is not one string literal (a concatenation, `&&`, `||`, `//`) is
  unverifiable.
- **A task can no longer rewrite the guard or the MCP registration for later
  tasks (WI#847, WI#848).** The executor re-asserts the `PreToolUse` guard in
  `$KIMI_CODE_HOME/config.toml` before every spawn and refuses to spawn when
  it cannot. The trusted `mcp.json` is a root-owned image input, materialized
  at start and restored before every spawn, and the hook denies the plain
  writes to it. These and the guard changes above need the rebuilt Kimi image;
  the guide's rollout sentence names them.
- **The baked guard denies kimi service mode from a task's Bash tool
  (WI#833).** `kimi` is on `PATH`, so a task's Bash tool could start
  `kimi server`/`kimi web` itself. The baked `PreToolUse` hook now denies those
  subcommands, plus a closed list of listener tools (`python -m http.server`,
  `nc`/`ncat`/`netcat -l`, `socat TCP-LISTEN`, `busybox httpd`, `php -S`,
  `ruby -run -e httpd`), with exit 2 before the process starts; a print-mode
  prompt whose text is `server` or `web` is still data. The executor drops
  every task-supplied `KIMI_CODE_*` value that is not on its allowlist, strips
  the remote-control relay URL, keeps `KIMI_CODE_HOME` worker-owned, and leaves
  auto-update and telemetry disabled. The hook does not resolve dynamic command
  names or recognize every program that can listen; the container's network
  policy remains the boundary. The change needs a rebuilt kimi image and a
  recreated worker, and the guide's bump checklist now asks for the new
  `KIMI_CODE_*` names and subcommands to be re-read on every bump.
- **The kimi headless guard closes a series of bypasses (WI#795, WI#810,
  WI#813, WI#824, WI#826, WI#829, WI#830, WI#832).** The `PreToolUse` hook
  (`kimicode4lunarwing/hooks/headless_command_guard.mjs`) joins
  backslash-newline continuations for every deny family rather than one
  (WI#813), keeps the join on when an apostrophe appears in a heredoc body
  (WI#810), matches a multi-line command in the recursive-`rm` rule (WI#795),
  recognises `rm` after word-splicing and without a literal `rm` token
  (WI#824, WI#826), and applies resolved-path containment to every deletion
  rule (WI#830). It covers deletion performed by `find -exec unlink`,
  `rsync --delete` and interpreter one-liners, decoded payloads fed to an
  interpreter, device writes through `tee`/`cp`, and the git commands that
  discard work: a forced `git clean` (a dry run passes), `git reset --hard`, a
  whole-tree `git checkout`/`git restore`, a forced `checkout`/`switch`, and a
  `stash` that takes untracked or ignored files (WI#826, WI#832).
  Abbreviated options, git aliases and wrapped forms are covered by WI#841,
  WI#842 and WI#853 (above). The SSH rule now matches the agent
  directory and socket path the lanes actually mount, and `ssh-add`, instead
  of a literal path no lane uses (WI#829); direct key-file auth stays allowed
  on purpose. Two false denials were removed in the same pass (WI#832). Every
  rule is pinned by `scripts/headless_command_guard_test.ts`.
- **A redirection is read out of the line before any rule sees it (WI#889).**
  The clause, word, segment, listener, git-verb and raw-shape readers all step
  over every form of `man bash` REDIRECTION (`[n]<word`, `[n]>word`,
  `[n]>|word`, `[n]>>word`, `&>word`, `>&word`, `&>>word`, `[n]<<[-]word`,
  `[n]<<<word`, `[n]<&word`, `[n]>&word`, `[n]<>word`, each also with
  `{varname}` and with or without blanks), so `rm 2>&1 -rf /home/kimi/x`,
  `git 2>/dev/null reset --hard`, `kimi 2>/dev/null server` and
  `bash 2>/dev/null -c "kimi server"` deny like the plain spellings, while the
  false refusals the old reading made (`rm -rf /tmp/x 2> /dev/null`,
  `rm -rf /tmp/x </dev/null`, `rm -rf 2>&1 /tmp/x`) pass. `|&` is bash's
  `2>&1 |`, so its bar splits a pipeline (`curl … |& sh` denies); a
  redirection's own `&`/bar are not control operators; a write target stays
  visible to the device rule (`>&/dev/sda1`); and a here-string body is read
  as the command line it spells (`bash <<<"kimi server"` denies).
- **A launcher that runs a quoted string through a shell is one table entry
  (WI#887).** `flock <file> -c`/`--command`, `watch`'s command, `su
  -c`/`--command`/`--session-command` and GNU `parallel`'s template and `:::`
  arguments all hand their body to the full body check, so
  `flock /tmp/l -c "tee /dev/sda1"`, `watch -n1 "nc -l 9000"`, `su -c "kimi
  server"` and `parallel ::: "python3 -m http.server 8080"` deny like the
  unwrapped spellings, while `flock /tmp/build.lock make -j8`,
  `watch -n5 "git status"`, `su kimi -c "cargo check"` and
  `parallel ::: "cargo check"` stay ordinary work. Each reader follows its
  launcher's own option walk: flock reads its options as util-linux flock's
  `getopt_long` does (a short cluster letter by letter, a long option by exact
  name or unique prefix, `--name=value` inline, `--` ending the options) and
  reads `-c` only after the lock file; where it cannot resolve an option word
  (an unknown option, an ambiguous prefix, a `-c` before the file) doubt never
  allows — the text that word carries and every later word are each read as a
  possible body, and a clause with more than 64 words from that word on is
  refused. su permutes its words through `getopt`, watch joins every word from
  its first non-option one and hands the line to `sh -c`, and parallel reads
  its template and each `:::` argument as a command line. A `-c` that belongs
  to another command (`git commit -c`, `grep -c`) is not a launcher body.
- **The decode and fetch rules are distance-free (WI#892).** In one pipeline a
  decode stage, or a `curl`/`wget`/`fetch`/`nc`/`ncat`/`netcat` stage, anywhere
  before an interpreter stage counts, so `echo <b64> | base64 -d | cat | tee
  /tmp/x | sh`, `gunzip -c x.gz | cat | sh` and `curl … | head -c 100000 | bash`
  deny. A shell, `eval` or `source`/`.` fed by a command substitution, a
  process substitution or a here-string that holds a decode stage or a fetch
  denies too (`sh -c "$(curl …)"`, `bash <(gunzip -c x.gz)`). A file counts as
  written when a stage fed by a decode or a fetch names it as a redirect
  target, a `tee` operand, `dd of=`, the destination of `cp`/`install` from
  `/dev/stdin`, curl `-o`/`--output`, wget `-O`/`--output-document`, the URL's
  last component for curl `-O` and for wget without `-O`, an in-place
  `gunzip`/`unxz`/`bunzip2`/`unzstd`, or `xxd -r IN OUT`; a `cp`/`mv`/`ln`/
  `install` or a redirect that copies a written file writes its destination
  too. Written and run files are compared by their last component only, so a
  relative path, an absolute one and the file reached after a `cd` match.
  Anywhere in the same command line a written file denies when it is executed
  — as a shell's script operand, `--rcfile`/`--init-file` or a `-c` string's
  positional parameter, by `source`/`.`, as the command word, as a shell's `<`
  input, piped into a shell or a stdin interpreter, through a substitution in
  a shell's text, or by a shell word behind a launcher the guard does not
  know. Where the guard cannot read a written name, a later shell operand or a
  path run (`sh x`, `./x`) denies; a bare command name (`jq`, `make`) is found
  in `PATH` and matches only a written file of that exact name. Only the
  command position executes, so a written file read by `jq .`, `tar -xzf … -C
  .`, `cp … .`, `wc -l` or `tar -czf` stays ordinary work, as does decoding or
  downloading to a file without running it; an interpreter given the file
  (`python3 x.py`) is outside the rule. A variable filled from such a
  substitution denies when a shell, `eval` or `source` word in the same stage
  expands it, when it is the command word, or when it is piped into a shell.
  A here-document that a shell or a stdin interpreter reads denies when its
  delimiter is unquoted and its body holds such a substitution or such a
  variable, and every here-document body of every line is checked as a command
  line of its own. Ordinary substitutions stay data: `bash -c "$(cat
  build.sh)"`, `bash <(cat build.sh)`, `eval "$(ssh-agent -s)"`, `source
  <(kubectl completion bash)`.
- **The bodies git runs itself are read (WI#895).** `git submodule [opts]
  foreach <tail>` and `git rebase … --exec/-x <body>` pass their operand to
  `sh -c`, so the tail gets the same full rule set, in the separate, attached
  (`--exec=<body>`, `-x<body>`), clustered (`-ix <body>`) and long-prefix
  (`--ex`, `--exe`) spellings, with `--no-exec` carrying none and `foreach`'s
  own leading options (`--quiet`, `--recursive`) stepped over. `git submodule
  foreach 'kimi server'`, `git submodule foreach 'tee /dev/sda1'`, `git rebase
  --exec='nc -l 9000' HEAD` and `git rebase -ix 'kimi server' HEAD` deny like
  the unwrapped spellings while `git submodule foreach 'echo hi'` and `git
  rebase -x 'cargo test' HEAD~1` still pass. The clause is read once per git
  word, so 16 KB of repeated `git rebase` stays inside the deadline.
- **The guard reads the git carriers and settings that run a command
  (WI#898, WI#906).** WI#898 covers `filter-branch` filters, diff and merge
  tools, grep's pager, pack commands on fetch, pull, clone, ls-remote,
  fetch-pack, push and send-pack, and archive's remote command, using the Git
  2.56 command manuals and option parsers. It checks command-valued settings
  in `-c`, `--config-env`, clone configuration, `git config` writes and the
  `GIT_CONFIG_*` environment forms, over keys that include editor, pager, SSH,
  external diff, merge, filter, pack, credential-helper,
  `trailer.<name>.cmd`/`trailer.<name>.command`, fsmonitor and man-viewer, and
  it refuses explicit enabling of Git's external command transport. WI#906
  then separates a real assignment from an argument that only looks like one:
  for the eight names `GIT_EDITOR`, `GIT_SEQUENCE_EDITOR`, `GIT_PAGER`,
  `GIT_SSH_COMMAND`, `GIT_EXTERNAL_DIFF`, `EDITOR`, `VISUAL` and `PAGER`, it
  reads bare and command-prefix assignments, operands of `export`, `declare`,
  `typeset`, `readonly` and `local`, and `env` assignments before its command,
  and no longer refuses an assignment-shaped argument to another program whose
  value holds an expansion (`echo "EDITOR=$EDITOR"`). A Git command value or
  an assignment to one of the eight names that the outer shell can change is
  refused as unverifiable, even without Git on the line — `export
  PAGER=less` passes, `export PAGER="${PAGER:-less}"` does not. Literal values
  are still checked as shell bodies even in arguments.
- **The guard's scope is a documented decision (WI#904, WI#907, WI#909).** By
  the maintainer's decision of 2026-11-26 the guard is a guard against naive
  use, not a security boundary, and its coverage is not extended to further
  spellings of a command it already refuses. `kimicode4lunarwing/AGENTS.md`
  now carries a **Guard Scope** section naming the three limits: literal text
  handed to a shell another way (`echo 'kimi server' | sh`, a file written and
  then run, a variable given to `eval` or `sh -c`, a `trap` action — WI#907);
  the further routes by which decoded or fetched content reaches a shell
  (`x=$(curl …); y=$x; $y` — WI#904); and Git's own listeners (`git daemon`,
  `git instaweb`) plus Git programs named by their dashed path
  (`/usr/lib/git-core/git-rebase -x '<body>'` — WI#909). The container and
  tenant egress policy remain the boundary. Every example in the section was
  run through the hook: the plain spellings are refused and the named limits
  pass.
- **A Perl deletion behind shell quoting is denied (WI#840).** The guard
  removes the shell escapes of a quoted one-liner before it reads the Perl
  deletion calls, so an escaped quote no longer hides an
  `rmtree`/`unlink`/`rmdir`/`remove_tree` operand, and every operand of a comma
  list is read instead of only the first. The change is pinned by the guard's
  test, and the guard's notes were corrected in the same merge request. A
  concatenated operand (`rmtree "/tmp/x/" . "../../workspace"`) and the
  `glob()`, `<…>`, `qw()` and implicit-`$_` forms are covered by WI#851
  (above).
- **The pebble worker-inputs manifest covers the files that ship (WI#806).**
  `entrypoint-split.sh` and `src/task_runner.rs` are build-read inputs, so a
  change to either now moves the lane's `worker-inputs` hash instead of
  leaving it green. `.gitlab/ci` validation and a regression test pin the
  shape.
- **Gotify reads the operator's file, so `mt-admin` reaches the tool
  (WI#817, WI#831).** The host now merges
  `<LUNARWING_BASE_DIR>/workspace/config/gotify.json` into the tool's
  workspace view at execution time, on disk first and the workspace document
  second, so the URL `add-tenant --gotify-url` and `configure-gotify` write is
  the URL the tool uses. The on-disk file takes precedence because `mt-admin`
  rewrites the capability allowlist from the same file, so the effective URL
  and the allowlist always name the same host. Symlinked files and files over
  64 KiB are ignored. `configure-gotify` now writes the workspace config and
  the allowlist together, and the guide's numbering was fixed.

## Agent runtime and turn handling

- **The detached-turn budget is step-aware and re-read while waiting
  (WI#807).** `ic/src/bridge/router/outcome.rs` keeps the 600-second base,
  adds 90 seconds per completed tool action, re-reads the budget whenever a
  wait slice expires (so an action completed during the late wait also earns
  time), and caps the total at 1,800 seconds. Only `ActionExecuted` events earn
  time — a tool that returned an error still counts, a refusal (`ActionFailed`)
  does not — and a turn with no tool calls keeps exactly the old 600 seconds.
  The stop request keeps its 60-second grace and settlement its own.
- **Every turn now leaves one measurement record (WI#807).** `Turn finished`
  is written for the foreground phase and for the late phase, and carries
  `phase`, `tool_calls`, `budget_secs` (the budget of that phase),
  `budget_exhausted`, `elapsed_ms` and `outcome`, on the boundary between
  "fits its budget" and "ran out" (`ic/src/bridge/router/outcome.rs`). The
  detach warning ("Turn wait budget exhausted; transferring delivery to late
  watcher") carries the starting `late_budget_secs`. The probe's field names
  are pinned against the record writer by a test. A daemon older than this
  release logs `Late turn finished` instead, for detached turns only.
- **The bounded-turn probe measures a known-step task (WI#807).**
  `ic/scripts/bounded-turn-probe.py` runs a task with a known step count
  against a live daemon and reads that record's `budget_exhausted`,
  `tool_calls` and `elapsed_ms`, so "it felt long" becomes "eight calls earned
  a 1,320-second allowance, and `budget_exhausted` says whether the model
  finished inside it". No probe run is claimed in this draft. The section is in
  [`docs/ops/PRE-RELEASE-TESTING.md`](docs/ops/PRE-RELEASE-TESTING.md).
- **The TUI's sealing windows follow the longer late budget (WI#837).**
  `YOUNG_FAILED_TURN_MS` moves from 610,000 to 2,530,000 ms, the daemon's
  longest late window (610 s wait budget `OUTCOME_TIMEOUT` + the capped
  1,800 s late budget + both 60 s graces). The legacy `unanswered` seal stays
  at 610,000 ms (`UNANSWERED_AFTER_MS`) on purpose, so a swallowed message does
  not hold its queue slot for the whole late budget.
  `tui/src/state/correlate.ts` holds the window as a literal
  (`LATE_REPLY_WINDOW_MS`) that mirrors the daemon's `LONGEST_LATE_WINDOW`,
  which `outcome/tests.rs` pins at 2,530 s; the reducer test states the
  distinction.
- **A finished message is remembered for the whole late window (WI#820).**
  The channel manager's finished-message memory
  (`ic/src/channels/lifecycle.rs`) derives its TTL from `LONGEST_LATE_WINDOW`
  instead of a shorter literal, so a late second finish is recognised as a
  duplicate for as long as the watcher can deliver it.
- **Skills no longer claim credential-bearing API access (WI#811).**
  `ic/crates/lunarwing_engine/orchestrator/default.py` no longer tells the
  model that every active skill "provides API access with automatic credential
  injection": Engine V2 metadata carries no per-skill credential, host or API
  grant. Credentials for the `http` tool come from host-side tool credential
  mappings, independent of which skills are active. The unconditional claim
  was removed, not reworded.
- **Tool runtime calibration observes, and reports only what it observed
  (WI#318).** `ToolEstimates` records exactly one `Ok` completion per executed
  call, measured around the existing timeout-wrapped execution; failures,
  timeouts, cancellations, preflight denials and approval waits never train a
  duration and are not converted to zero-duration successes. No estimate is
  returned until five valid completed calls exist. Keys are per user and per
  canonical tool name, never logged, and at most 1,024 models are retained.
  Five read-only gateway routes expose the models — `GET /api/estimates`
  (user-scoped), `/api/estimates/counters`, `/api/estimates/evaluation`
  (opt-in trace), `/api/estimates/llm` and `/api/estimates/gate-waits` (all
  four owner-only); they use the existing bearer-token middleware and add no
  listener.
- **LLM call timing observes by default (WI#318).** One daemon-wide timer
  decorates the provider chain built at startup and records each completed
  logical LLM call; cache hits, failures, incomplete streams and calls that
  switch provider or model do not train, and cost is not learned. Unlike the
  tool estimates it is on unless `LUNARWING_LLM_TIMINGS=0`; it is in memory,
  keyed by configured provider and model with no user identity, and resets on
  restart ([`docs/ops/TENANT-CONFIGURATION.md`](docs/ops/TENANT-CONFIGURATION.md)).
- **Approval hints are opt-in and say what they are (WI#318).** With
  `LUNARWING_TOOL_ESTIMATES=1`, an approval description gains one line such as
  "Typical tool runtime: ~2.00s (EMA, 5 completed calls, low confidence).
  Excludes approval wait; not a deadline." The default stays observe-only, so
  nothing new appears in approval text on an untouched host.
  `LUNARWING_TOOL_ESTIMATES_CATEGORIES` gates display per tool and fails closed
  (no hints) on a malformed rule; `LUNARWING_TOOL_ESTIMATES_TRACE=1` enables
  the bounded held-out trace. Authorization and external-wait gates receive no
  hint, and gate IDs, expiry, redaction and resolution semantics are unchanged.
- **The A4 rollout gate is reported, not hand-computed (WI#318).** Each
  category's evaluation cohort now carries `gate_pass`: `true` only when the
  learned prediction beats the model-derived early-run baseline by at least 10%
  on median absolute error, `false` when the improvement falls short of 10%,
  and `null` when it could not be judged. Operators record the verdict and use
  it to set the category allowlist manually; it changes no
  runtime behaviour and the default remains observe-only. The operator
  procedure is in
  [`docs/plans/WI191_PERFORMANCE_EMA_INTEGRATION.md`](docs/plans/WI191_PERFORMANCE_EMA_INTEGRATION.md).

- **Job and mission timing (WI#318 Phase C).** A local job gets an estimate
  frozen at dispatch from a per-user model kept apart by kind (routine runs and
  user-started jobs). In display mode it appears in `job_status` and
  `GET /api/jobs/{id}`, and mission detail shows run timing once five valid
  runs exist, counting active time only (approval and child waits excluded).
  Sandbox and external jobs are excluded, and nothing feeds behaviour.

## Tools and extensions

- **The GitLab tool can read merge-request diffs (WI#279).** `get_mr_changes`
  takes `project`, `iid` and an optional `access_raw_diffs`, and
  `list_merge_requests` gained `source_branch`, `target_branch` and
  `all_pages`. `unapprove_merge_request` is the counterpart of the existing
  approve action. The tool is at `0.1.19`; the plugin wrappers and their tests
  carry the new actions.
- **The GitLab tool's search is bounded and typed (WI#891).** Every
  project-scoped search runs through one per-page-capped path: the page size is
  clamped to 20 at the call site's own boundary rather than at each caller, so a
  `search_issues`/`search_blobs`/`search_projects` page cannot grow with the
  server's own limit. A blob hit's matched snippet (`data`) is cut to 2,000
  characters with a `data_truncated` flag, and a hit keeps its triage fields —
  project identity and blob location — while heavy subtrees are skipped. The
  term is matched rather than trapped on a known matching value, and the
  response is parsed typed instead of by string. The tool is at `0.1.19`.
- **A WASM trap now reports its cause (WI#891).** Wasmtime attaches a
  backtrace as an anyhow context on every trap, and anyhow's plain `Display`
  prints only the outermost context, so every trap reported as a reason-less
  backtrace. `ic/src/tools/wasm/trap.rs` classifies traps structurally: it
  reads the root cause, downcasts to `wasmtime::Trap` in preference to string
  matching, matches Wasmtime 36's fuel wording, and folds a recorded
  `memory.grow` denial into a memory-limit explanation. A refused memory
  growth is recorded as a denial rather than surfacing as a bare trap. The
  tool and channel lanes use the same structural classification.
- **`ssh_git` refuses a ref-deleting push (WI#818).** A push refspec whose
  source is empty (`:branch`, or a bare `:`) is refused with `NotAuthorized`
  before git runs, alongside the existing refusal of a leading `-` and of a
  leading `+`. Before this, only force-push was refused.
  [`docs/ops/SSH-HARNESS-SETUP.md`](docs/ops/SSH-HARNESS-SETUP.md) and
  `docs/guides/SSH-TOOL-TESTING.md` say so.
- **The XMPP channel drains a backlog in pages without replaying it
  (WI#850).** The bridge serves `GET /v1/messages` in pages (`limit`, default
  64, at most 100), the host dispatches an accepted page and commits the
  guest's cursor before it reports an emit overflow, and a poller that lost
  its cursor (a daemon-only restart) resumes after the last acknowledged
  message instead of replaying the bridge's ring. Upgrade the bridge with the
  daemon and rebuild the WASM channel (registry 0.1.25).
- **The channel and tool registries patch-bump with their crates (WI#799).**
  Channels are darkirc `0.1.18`, multica `0.1.22`, weechat `0.2.19` and xmpp
  `0.1.25`. Every tool whose source or dependencies moved in the sweep is
  patch-bumped: cavepony-tool `0.4.13`, gitlab `0.1.19`, gotify `0.1.9`,
  llm-context `0.1.11`, multica-bridge `0.1.12`, ssh `0.1.10`,
  vision-analyze `0.2.4`, voice-transcribe `0.1.10` and web-search `0.2.12`.

## Security and supply chain

- **The audit is a point-in-time record (WI#800).** The report is
  [`docs/ops/v2.2.2.0_CARGO_DENY_AUDIT.md`](docs/ops/v2.2.2.0_CARGO_DENY_AUDIT.md).
  It reads `85d564a82` against RustSec advisory-db `55e1c3df` (2026-11-21) and
  covers all 27 tracked non-vendor lock roots, including the manifest-less
  Pebble override. It re-verified all 23 acceptances, found none stale or
  widen-able, and added, removed, widened or re-worded none of them. The
  database moved under the audit — it began at `f291af9b` and finished at
  `55e1c3df`; the one added record is a Matrix-URI panic against
  `ruma-identifiers-validation`, which is in no lock or manifest. The report is
  true for that database revision and that date only; the CI gates are the
  standing check.
- **The acceptances now name a standing owner (WI#779).** The twelve
  acceptances whose clearing owners WI#367 and WI#418 closed without clearing
  are re-homed to
  [`docs/ops/v2.2.2.0_STANDING_ADVISORY_OWNERS.md`](docs/ops/v2.2.2.0_STANDING_ADVISORY_OWNERS.md),
  a document with a row per advisory and no open/closed state. `ic/deny.toml`
  still holds 23 IDs, and a CI test requires each to appear as a table row in
  the owner document.
- **`cargo-deny` bans the Wasmtime versions the fixes came from.** `[bans]`
  denies `wasmtime >=36.0.0, <36.0.20` and `wasmtime-wasi >=36.0.0, <36.0.22`
  inside the supported 36.x line, with no advisory ignore for either. The locks
  carry `36.0.23`, which is above both floors; RUSTSEC-2026-0587 is cleared by
  the version, not accepted. The Wasmtime acceptance the v2.2.1.0 notes
  described as removed is still absent, and the comment says a re-broken record
  is a new maintainer decision.
- **The secret-history baseline carries the 2.2.2 vendor commit (WI#799,
  WI#798).** Nine fingerprints were added since v2.2.1.0, recorded in
  `.gitlab/gitleaks-history-ignore` with disposition rows in
  `.gitlab/gitleaks-history-dispositions.md`, in two sets: five from the
  sweep's vendor commit `aacffdd47` (WI#799: `cc`, `serde_with`, `toml`,
  `toml_edit` and `toml_parser`) and four from the Wasmtime 36.0.23 refresh at
  `da90adaad` (WI#798: `wasmtime`, `wasmtime-wasi`, `cranelift-assembler-x64`
  and `cranelift-entity`). Each is a cargo checksum manifest whose package
  digest equals the crate's checksum in `ic/Cargo.lock`, not a credential. Without them the blocking `secret-history`
  job would have failed at the release tag, because the scan runs only on
  schedules and protected tags.
- **The WASM tool crates are compiled, tested and linted in CI (WI#809,
  WI#825).** Before this, a tool crate was only resolved: nothing built an
  `ic/tools-src` crate until a tenant build did, so a broken tool reached a
  tenant before any pipeline showed it. `wasm-tools-check` takes its crate list
  from `.gitlab/deliverables.json`, resolves each manifest through
  `ic/registry/tools/*.json` to the same source directory a tenant build
  compiles, and runs `cargo build --locked --release --target wasm32-wasip2`,
  `cargo test --locked` and `cargo clippy --locked --all-targets -- -D warnings`
  through `.gitlab/scripts/cargo-ci.sh` for the eight crates assigned to it;
  `cavepony-tool` is built by `cavepony-wasm`, which runs one host
  compatibility test and no clippy. The job blocks a merge. The coverage
  check is no longer a substring test (WI#825): an `echo`, a comment or a lone
  `cd` that mentions the path does not count, only an actual cargo compile
  subcommand run from that directory or naming its manifest.
- **Artifact retention now states the limit the platform delivers (WI#805).**
  The project deletes pipelines after 30 days, and deleting a pipeline deletes
  its jobs, logs and artifacts. The scan and promotion evidence therefore
  declare 30 days — the ceiling, not a shortening — and
  [`docs/ops/CI_ARTIFACT_RETENTION.md`](docs/ops/CI_ARTIFACT_RETENTION.md) says
  so at the `expire_in` line and in the document. The one-off cleanup the
  earlier text recommended is named as unsafe there: it deleted the scan and
  promotion artifacts of every release pipeline older than two days, and
  promotion never rebuilds, so that evidence cannot be made again. Rootfs tars
  still expire in two days.

- **The pebble locks move to `url` 2.5.9 (RUSTSEC-2026-0592, WI#855).** The
  advisory (a stack overflow in `Url::origin()` on nested `blob:` URLs) failed
  `satellite-advisory-sweep` on the two pebble locks, the only ones still at
  2.5.8. Both now carry `url` 2.5.9 and `form_urlencoded` 1.2.3, the override
  verifier allows both, and `PEBBLE_LOCK_SHA256` pins the new override.

## Operations and tenant lifecycle

- **`upgrade-tenant` refreshes the worker containers (WI#796).** Both upgrade
  paths call `upgrade_refresh_worker_containers` after promotion: it
  distributes each enabled lane's `:latest` image into the tenant's rootless
  store and removes the bridge and task containers still running a different
  image, so the following start rebuilds them from the promoted image. It is
  idempotent, skips a deselected lane and an intentionally stopped OpenRC lane,
  and only warns when an image cannot be distributed; on OpenRC it stops each
  stale container's `-sup` supervisor before removing that container. The
  standalone verb is:

  ```bash
  sudo ic/scripts/lunarwing-mt-admin.sh refresh-worker-containers <tenant>
  ```

  It removes stale containers but does not start their replacements; follow it
  with `start-tenant <tenant>`.
- **A dead lane is no longer reported as a green upgrade (WI#796).** An enabled
  lane whose image is absent returns **3** instead of a silent success, and a
  bridge Quadlet that exists but never starts returns **5**, named by
  `worker_quadlet_start_failed` with the container-log hint. The tenant-wide
  start pass records 3, 4 and 5 as failed lanes, attempts later lanes, and
  returns **1** after naming them. Deselected workers, intentionally stopped
  OpenRC workers and unallocated ports keep their soft skip. On a tenant this
  class of silent skip had surfaced only as
  `Connection refused (os error 111)` at dispatch. The pebble bridge now waits
  up to 120 seconds, not 30, for the task runner's socket and logs the wait
  (`pebble4lunarwing/entrypoint-split.sh`).
- **The startup guide names LunarDepth in the lane order.** The shared
  startup pass was already `pebble opencode kimi opencode2 lunardepth` at
  v2.2.1.0; only the guide sentence changed.
- **Worker readiness fails an enabled lane with no WSS port (WI#882).** A lane
  that has no allocated WSS slot in the ports registry now reports `no-port`
  without dialing, and the readiness pass returns non-zero for it; before, a
  lane with no port was skipped as though it were deselected, so onboarding
  could succeed on a tenant whose enabled lane had nowhere to listen. A lane
  whose port is allocated but does not answer still reports as not listening,
  with the `worker-failure-log` hint. All lanes share one polling deadline, so
  a slow lane consumes the window for everyone, and the pass never stops or
  restarts a lane itself. `mt-admin`'s help now describes a missing worker
  port, and the onboarding worker-readiness contract and the Kageho runbook
  state the requirement.
- **An invalid credential binding blocks tenant startup (WI#893, WI#901).**
  If a granted remote is bound to a variable whose name does not match
  `SANDBOX_GIT_TOKEN_<NAME>`, the env render warns that the daemon will refuse
  `SANDBOX_GIT_CREDENTIALS` at startup and names the tenant, the remote and the
  variable. The render keeps the binding in the setting, so the tenant cannot
  start until you rename the credential variable to `SANDBOX_GIT_TOKEN_<NAME>`
  and update the binding, or remove the binding.
- **The relay's limits and its failure attribution are documented (WI#834,
  WI#836).** `ic/src/NETWORK_SECURITY.md` now states the 32-live-connection
  cap, that finished connections are reaped while the lifetime accept count
  keeps rising, the wait-for-a-slot behavior, the doubling retry after a failed
  `accept`, and the three conditions that make a run `proxy_error`. The
  in-container `relay.py` keeps the same cap, and its test was updated for the
  probe's socket-creation order.

## Documentation

- **Architecture and guide refresh (WI#803).** Fifty-four documents — 12 under
  `docs/architecture/`, 31 top-level guides and 11 nested ones — were read and
  source-checked, each with a row in the merge request's record. Nine
  statements on seven stamped pages were corrected, along with the five rows
  left open from the v2.2.1.0 refresh. Seven archives already under
  `docs/guides/history/` were edited to mark their historical boundaries and
  name their current replacements. Two relative-link and
  anchor walkers passed on all 54 documents, and no live provisioning or
  migration is claimed.
- **New and extended operator material.**
  [`docs/guides/SANDBOX_CAPABILITY_MANIFEST.md`](docs/guides/SANDBOX_CAPABILITY_MANIFEST.md)
  (WI#797, extended by WI#816),
  [`docs/ops/SELF_IMPROVEMENT_OPTIMIZATION_WI793.md`](docs/ops/SELF_IMPROVEMENT_OPTIMIZATION_WI793.md)
  (WI#793),
  [`docs/ops/v2.2.2.0_CARGO_DENY_AUDIT.md`](docs/ops/v2.2.2.0_CARGO_DENY_AUDIT.md)
  and
  [`docs/ops/v2.2.2.0_STANDING_ADVISORY_OWNERS.md`](docs/ops/v2.2.2.0_STANDING_ADVISORY_OWNERS.md)
  (WI#800, WI#779), the bounded-turn section of
  [`docs/ops/PRE-RELEASE-TESTING.md`](docs/ops/PRE-RELEASE-TESTING.md) (WI#807),
  and the artifact-retention correction (WI#805).
- **The plans index is `docs/plans/README.md` (WI#819, WI#794).** The file was
  renamed from `REORG.md`, its four inbound links were updated, and superseded
  plans moved under `docs/plans/history/` with a README that names their
  current replacements.
- **The worker container guide carries the 2.7.2 bump entry and the WI#796
  startup contract (WI#804, WI#796, WI#833, WI#840).** The bump checklist, the
  guard's listener rules and the `start-worker` status table are the versions
  this tree implements.
- **Small corrections.** Three guide version stamps the 2.2.1 sweep left
  behind (SSH tool testing, the vision OCR sidecar and the gotify guide) and a
  false dispositions sentence now name the current versions and state that
  WI#799 refreshed the baseline in the same merge request (WI#827). The
  deny-guard README sentence, the finished-message memory window and the
  `cpu_shares` schema description were corrected (WI#820).

## Compatibility

| Area | v2.2.2.0 |
|---|---|
| Owned packages / core Rust | Eleven packages at `2.2.2` (daemon, five workspace crates, XMPP bridge, four WASM channels); all eight inter-crate path requirements at `2.2.2`; Rust 2024, MSRV `1.96.1` |
| WASM runtime | Wasmtime/WASI `36.0.23` in `ic/Cargo.lock` (manifest floor `36.0.22`); `[bans]` denies `wasmtime` before `36.0.20` and `wasmtime-wasi` before `36.0.22` within 36.x; the `wasmtime-internal-cache` patch stays on that line |
| Channel registry | darkirc `0.1.18`, multica `0.1.22`, weechat `0.2.19`, xmpp `0.1.25` |
| Tool registry | cavepony-tool `0.4.13`, gitlab `0.1.19`, gotify `0.1.9`, llm-context `0.1.11`, multica-bridge `0.1.12`, ssh `0.1.10`, vision-analyze `0.2.4`, voice-transcribe `0.1.10`, web-search `0.2.12` |
| External workers | Kimi Code `2.7.2`, OpenCode `v1.19.6` (`f4e363c82…`), OpenCode 2 `v2.3.2` (`c0ad18b40…`), Pebble `2d0f3786…`, LunarDepth `ca905dfe5…` |
| Worker toolchain | Debian snapshot `20261105T082236Z`; Node `22.23.4` (Kimi, LunarDepth); pnpm `11.7.0` (LunarDepth); Bun `1.4.4` (OpenCode lanes) |
| Sandbox | Linux default `proxy-enforced`; `tunnel` opt-in; optional version-1 `capability_manifest` that can only narrow host policy; one credential-free git grant (`git-remotes`) when `SANDBOX_GIT_REMOTES` is set; relay serves 32 live connections and reaps them |
| Runtime estimation | Observe by default; approval hints need `LUNARWING_TOOL_ESTIMATES=1`; tool models per-user, per-tool; LLM call timing daemon-wide (`LUNARWING_LLM_TIMINGS=0` disables); all in memory, lost on restart |
| Terminal client / local REPL | Wingtip `0.1.0` over HTTP/SSE; TUI history recovery covers the daemon's 2,530 s longest late window while the legacy `unanswered` seal stays at 610 s; daemon Unix socket supports protocol 1 and legacy compatibility responses |
| Advisory baseline | 23 accepted IDs, all owned by the standing record; no Wasmtime ignore; RUSTSEC-2026-0587 cleared by `wasmtime-wasi 36.0.23`; audit true at advisory-db `55e1c3df` |
| CI | `cargo-deny 0.20.2`, `cargo-audit 0.23.0`, 27 tracked non-vendor lock roots; `wasm-tools-check` blocks on eight WASM tool crates, `cavepony-wasm` builds the ninth |

No database migration is added by this release, and no existing wire or
database contract changes. The estimation routes are additive and read-only,
the capability manifest is opt-in per call, the git grant is off without
`SANDBOX_GIT_REMOTES`, and estimation is observe-only until an operator sets a
flag.

Registry bumps require rebuilt WASM artifacts. Rebuild every enabled worker
image through the protected candidate, scan and promotion path; local builds
do not replace those gates.

## Upgrade Notes

1. **Reinstall the extensions the registries moved.** Rebuild the daemon and
   the XMPP bridge, reinstall the four channel and nine tool extensions, and
   rebuild enabled worker images. `./ic/scripts/build-wasm-extensions.sh`
   builds the WASM channels and tools from this tree; the registry entries
   expect the versions in the table above.
2. **Rebuild the kimi worker image at `2.7.2`.** The other lanes keep their
   upstream pins, but their image inputs changed too: the pebble locks and
   `entrypoint-split.sh` (the 120-second runner wait, WI#796), the OpenCode v1
   tool plugin, and the OpenCode 2 plugin and exposure-guard scripts; only
   LunarDepth's inputs are unchanged. Build through the protected candidate,
   scan and promotion jobs. The
   candidate artifacts expire after two days, so play the manual
   `worker-kimi-promote` job from the pipeline that produced its candidate; if
   that pipeline's artifacts are gone, re-run the candidate and then the scan.
3. **Refresh worker containers after an upgrade.** `upgrade-tenant` does the
   removal itself now. If you promoted an image by hand or skipped the upgrade
   path, run the standalone verb and then start the tenant:
   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh refresh-worker-containers <tenant>
   sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>
   ```
   The verb removes stale bridge and task containers; it does not start their
   replacements.
4. **Read the startup status of a lane you rely on.** A missing image returns
   **3** and a bridge Quadlet that failed to start returns **5**; both are now
   failures, not soft skips. `start-worker <tenant> <worker>` prints the status
   and the container-log hint.
5. **Grant git to the shell only where you want it.** With no
   `SANDBOX_GIT_REMOTES`, a manifest that asks for `git-remotes` is refused and
   every other call behaves as before. When you set it, list the remotes as
   `name=url`, keep the hosts on the tenant sandbox egress allowlist
   (`SANDBOX_EXTRA_DOMAINS` plus defaults), and remember that the grant carries
   no credential. Do not hand-edit the
   Gotify capability file: `configure-gotify` writes the workspace config and
   the allowlist together.
6. **Leave estimation on observe unless you want the hints.** Setting
   `LUNARWING_TOOL_ESTIMATES=1` adds one line to approval descriptions; the
   `GET /api/estimates*` routes work in either mode and add no listener. LLM
   call timing observes unless `LUNARWING_LLM_TIMINGS=0`. There is nothing to
   migrate — the models are in memory and start cold.
7. **Leave the advisory acceptances as they are.** `ic/deny.toml` still carries
   23 IDs, and the audit found none stale. `cargo-deny` on this tree should
   pass with no warnings. If a gate reports an ID as `advisory-not-detected`,
   re-read the audit rather than deleting the entry.
8. **Create or keep the secret-history schedule.** The blocking scan runs on
   schedules and protected tags only, and the schedule is a project setting,
   not committed YAML. The fingerprints this release's vendor commit added are
   already pinned; a future vendor refresh needs the same step.
9. **Play a promotion within 30 days, and its candidate within two.** Both the
   scan and the promotion evidence now live exactly as long as their pipeline,
   and the project deletes pipelines after 30 days. Promotion never rebuilds,
   so an expired candidate means re-running the candidate and then the scan.

## Known Limitations

- **NetBird custom DNS is unsupported (WI#603).** Setting `SANDBOX_TUNNEL_DNS`
  with the NetBird provider is refused during configuration validation, before
  any container-runtime call (`ic/src/sandbox/tunnel_sidecar.rs`). Use NetBird
  nameserver groups reachable through the overlay; an upstream outside the
  overlay fails closed because its resolver sockets are unmarked.
- **Harness redaction remains bounded and best-effort.** Offset or grouped
  dumps, other encodings, arbitrary fragments and unmarked truncation are not
  comprehensively covered. The external-worker isolation boundary remains
  per-task-container authority; see
  [`docs/architecture/HARNESS_WORKER_CAPABILITIES.md`](docs/architecture/HARNESS_WORKER_CAPABILITIES.md).
- **Wingtip socket work is still M4.** The daemon Unix socket supports
  protocol 1, but Wingtip's own socket transport and reconnect suite remain
  future work. A missing or replaced daemon socket is logged without automatic
  rebind.
- **RSA policy stops at the shared client.** Worker-mode OpenSSH and
  `ssh_git`'s post-pin OpenSSH transport retain their own host-key algorithm
  policies. RSA private keys are rejected on every LunarWing SSH path; RSA host
  keys are unsupported only on the shared `russh` paths.
- **Harness job directories follow the daemon's umask (WI#803).** The
  `harness/` base is mode 0700. A job subdirectory is created without an
  explicit mode, so it is 0700 under the units `ic/systemd/` and `mt-admin`
  ship (umask 0077) and 0755 under umask 022 — a daemon started by hand, and
  the systemd user unit `lunarwing service install` writes, which sets no
  `UMask`. Run the daemon through a shipped unit if job directories must not be
  group- or world-readable.
- **The Kimi guard reads text, not a shell.** Its coverage is bounded and, by
  the maintainer's decision of 2026-11-26, is not extended further (WI#904,
  WI#907, WI#909): literal text handed to a shell another way
  (`echo 'kimi server' | sh`, a file written and then run, a variable given to
  `eval` or `sh -c`, a `trap` action), the further routes by which decoded or
  fetched content reaches a shell (`x=$(curl …); y=$x; $y`), and Git's own
  listeners (`git daemon`, `git instaweb`) and Git programs named by their
  dashed path are documented limits, not defects. `rename` and interpreter
  renames still pass. WI#887 closed `flock -c`, `watch`, `su -c` and `parallel`
  as launchers, and WI#858 already reads `eval`, `taskset`, `script -c`,
  process substitutions `<(…)` and brace expansion, so those are no longer
  limits. The egress allowlist and the container remain the boundary.
- **The shell git grant carries no credential (WI#816).**
  Host-held authentication and push from inside the shell are not part of this
  release. The manifest preflight rejects recognized `git push` invocations,
  but it is not a repository write filter for arbitrary shell processes; use
  server-side permissions and branch protection for write policy.
- **The capability manifest does not reach worker containers (WI#797).**
  External worker lanes stay governed by `mt-admin`'s per-tenant container
  configuration and egress allowlists. The shell's git grant does not grant
  access to a worker's credentials.
- **Promotion evidence lives as long as its pipeline (WI#805).** The project
  deletes pipelines after 30 days, so the scan SBOM and the promotion record
  are gone with it. There is no durable record of what a release published
  outside the pipeline.
- **Tool runtime estimates are per registry and per process (WI#318).** Each
  registry starts cold, no learned factor survives a restart, and the display
  is a heuristic EMA labelled `low` or `moderate`, not a probability, a
  percentile or an SLA. Cost calibration has no production baseline yet, so
  production cost remains unknown.

クラン
