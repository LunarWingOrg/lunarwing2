# SSH Git ref handling and bare-remote HEAD diagnostics

> **Overall status: PARTIALLY-FIXED (verified against current source on
> 2026-08-13).** Null-like `ref` values are normalized. A bare repository whose
> remote HEAD points at a missing branch still produces an empty/unborn checkout
> that `ssh_git` marks successful, with only Git's raw stderr warning.

This consolidates `BUG-ssh-git-bare-repo-head-mismatch.md` and
`BUG-ssh-git-null-ref-serialization.md`.

## 1. Null-like `ref` values

**Status: FIXED (static/unit-test evidence; full SSH runtime not run).**

The actual boundary bug was acceptance of an upstream literal string `"null"`,
blank text, or whitespace after JSON serialization. Missing fields and JSON
`null` already became `None` in Rust; the old report incorrectly blamed the
tool for serializing JSON null itself.

Current normalization is centralized in
`ic/src/tools/builtin/ssh_git.rs:68-74`: it trims the value and drops missing,
JSON-null, empty, whitespace-only, and case-insensitive `"null"`. The execute
path uses that result (`:140`, with option-injection validation at `:156-158`),
and argv construction adds `--branch` only when a real ref is present
(`:363-378`). Regression cases and preservation of
`main` are covered at `:646-667`.

The fix commit `c8b277d` is contained in `HEAD` (`merge-base --is-ancestor`
passed; the branch tip is an ancestor). The historical repro and workaround are
kept for context, but no longer describe current behavior.

## 2. Bare remote HEAD mismatch

**Status: STILL-OPEN (tool diagnostics/UX).**

### Reproduction

```bash
git init --bare test-repo.git
git clone test-repo.git seed
cd seed
echo test > README.md
git add README.md && git commit -m initial
git branch -M main
git push origin main
git clone test-repo.git clone-test
```

When the bare repository still points `HEAD` at `refs/heads/master`, the final
clone command exits successfully but prints Git's warning that the remote HEAD
refers to a nonexistent ref. The checkout is empty/unborn; the old title and
symptom saying the clone command fails are inaccurate. A local reproduction of
this exact Git sequence returned exit 0 with that warning (re-run again on
2026-08-13 with the same result).

### Current code evidence

`ref` remains optional in the tool schema
(`ic/src/tools/builtin/ssh_git.rs:111-122`). With no ref, argv is ordinary
`git clone` (`:363-376`), and result handling reports the process exit/stderr
without checking whether the remote HEAD resolved to an actual branch
(`:246-258`). The raw Git warning is returned in `stderr`, but `success` remains
`true` (it is `code == 0` at `:255`, and Git exits 0 on this warning); no
regression test or structured warning detector for this state was found.

### Workarounds and follow-up

Normalize a bare repository with:

```bash
git --git-dir=test-repo.git symbolic-ref HEAD refs/heads/main
```

or pass `ref=main` explicitly. A future tool fix could inspect clone output and
return a specific warning, but making `ref` mandatory is not current behavior.

## Verification record

Verification used current Rust source/tests, Git ancestry for `c8b277d`, and a
local bare-Git reproduction (re-run on 2026-08-13 against revision `7e813db4`:
exit 0, `warning: remote HEAD refers to nonexistent ref, unable to checkout`,
empty checkout). No Cargo command or SSH service was run. The only source
change since the previous pass is an added `allows_autonomous_execution() ->
false` override (`ssh_git.rs:267-271`), which does not affect either finding.
