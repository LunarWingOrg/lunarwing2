# Adding pre-delegated subtree support for OpenRC multi-tenant setups

> **Status (v2.0.8.0): implemented in mt-admin.** `lunarwing-mt-admin.sh setup-cgroups`
> automates steps 1–3 below (boot-time `/etc/local.d/lunarwing-cgroups.start`, the
> `pam_exec` login hook, PAM wiring for `system-login`/`sshd`/`su`/`login` — never
> `sudo`/shared includes), and `add-tenant`/`remove-tenant` keep the subtree and
> boot script in sync automatically. Step 4 uses **Option A**: `build-tenant`
> places each build into `lunarwing-<name>/build` by root-side `setpriv`
> migration behind a rendezvous gate — no tenant self-migration, no chown
> delegation, no PAM dependency. The manual recipe below is kept as reference
> documentation of the deployed layout.

Recipe. But first key correctness point: PAM subtree alone does NOT fix mt-admin builds, because mt-admin spawns build via `sudo -u` from operator shell — fork inherits **caller's** cgroup, PAM hook never fires. Login subtree fixes tenant **login sessions**; for mt-admin build path you still need root-side migration (setpriv patch) or switch spawn to `su`/ssh (PAM-authenticated). Both included below.

## 1. Hierarchy layout (respects no-internal-process rule)

```
/sys/fs/cgroup/                          root, controllers enabled
└── lunarwing-<tenant>/                  tenant-owned, +memory, NO procs
    ├── session/                         tenant-owned, login shells live here
    └── build/                           created by mt-admin per build
```

Controllers enabled on `lunarwing-<tenant>` means that dir itself must hold no processes — hence `session/` child.

## 2. Boot-time setup (not persistent across reboot — must recreate)

`/etc/local.d/lunarwing-cgroups.start` (or proper init.d):

```sh
#!/bin/bash
CG=/sys/fs/cgroup

# controllers must be listed in cgroup.controllers; enable down the tree
grep -qw memory "$CG/cgroup.controllers" || exit 0
echo +memory +pids > "$CG/cgroup.subtree_control"



for t in ruffles lunarium; do
  getent passwd "$t" >/dev/null || continue
  uid=$(id -u "$t"); gid=$(id -g "$t")
  d="$CG/lunarwing-$t"
  mkdir -p "$d/session"
  echo +memory +pids > "$d/cgroup.subtree_control"
  chown "$uid:$gid" "$d" "$d/session" \
    "$d/cgroup.procs" "$d/cgroup.subtree_control" \
    "$d/session/cgroup.procs"
done
```

`chmod +x`. Root is exempt from no-internal-process, so enabling at root with procs present is fine.

## 3. PAM hook — place login shells into `session/`

Helper `/usr/local/sbin/lunarwing-cgroup-login`:

```bash
#!/bin/bash
# pam_exec helper: move the PAM application (PPID) into tenant cgroup
[[ "$PAM_TYPE" == open_session ]] || exit 0
id -u "$PAM_USER" >/dev/null 2>&1 || exit 0
cg="/sys/fs/cgroup/lunarwing-$PAM_USER/session"
[[ -w "$cg/cgroup.procs" ]] || exit 0
echo "$PPID" >"$cg/cgroup.procs" 2>/dev/null || true
exit 0
```

`PPID` of pam_exec child = sshd per-connection process / login → children (shell) inherit membership. Must be `optional` + never fail auth.

Wire into `/etc/pam.d/system-login` AND `/etc/pam.d/sshd` AND `/etc/pam.d/su`:

```
session    optional    pam_exec.so /usr/local/sbin/lunarwing-cgroup-login
```

Do NOT add to `/etc/pam.d/sudo` — would migrate operator's sudo process, wrong direction.

Alternative: `pam_cgroup.so` from `dev-libs/libcgroup` — exists but cgroup v1-era, rules engine stale; pam_exec is cleaner.

## 4. mt-admin changes

a) Create build cgroup inside tenant subtree:

```sh
cg="$root/lunarwing-$name/build"    # was $root/lunarwing-build-$name
```

Skip chown-delegate block — whole branch already tenant-owned. Tenant self-migration now works: source (`session/`), target (`build/`), common ancestor `lunarwing-$t` — tenant owns all three `cgroup.procs`.

b) But build shell spawned via `sudo -u` sits in **operator** cgroup, not `session/`. Two fixes:

**Option A (recommended, no host deps):** root-side migrate + drop privs — setpriv patch from before. Independent of PAM entirely, 5 lines, works today.

**Option B:** replace `sudo -u "$name" bash -c ...` with `su -s /bin/bash -c "$build_cmd" "$name"` — su runs PAM session stack → hook fires → shell lands in `session/` → existing self-migration works. Requires host PAM setup from steps 1–3 deployed everywhere.

## 5. Verify

```sh
# as tenant, after ssh login

cat /proc/self/cgroup        # expect .../lunarwing-<t>/session
# self-migration test
d=/sys/fs/cgroup/lunarwing-<t>/build; mkdir "$d"
echo $$ >"$d/cgroup.procs" && cat /proc/self/cgroup   # expect .../build
rmdir ... cleanup
```

## Honest recommendation

Steps 1–3 = real infra, worth it for containing tenant login sessions generally. For the build bounding bug specifically, Option A (setpriv) strictly simpler: zero host config, fixes every OpenRC host immediately. PAM route only pays off if you also want login-session containment.