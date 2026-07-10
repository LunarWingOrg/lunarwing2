# BUG: ssh_git tool serializes null ref as literal string "null"

**Severity:** Low
**Found:** 2026-07-03 during v1.1.8 SSH tool validation on tenant `starforce`
**Status:** Fixed
**Affects:** `ssh_git` built-in tool (`ic/src/tools/builtin/ssh_git.rs`)

## Symptoms

1. Agent calls `ssh_git` with `ref` omitted (or set to null in the JSON parameters)
2. The tool serializes the null value as the literal string `"null"`
3. Git interprets `"null"` as a branch/tag name
4. Clone/push fails or operates on the wrong ref

## Reproduction

```
Use the ssh_git tool to clone a repo. Parameters: operation=clone, host=127.0.0.1, repo=lunarwing/test-repo.git, path=test-clone
```

The agent omits `ref` (it's optional in the schema). The tool passes `null` as the refspec to git.

## Root Cause

Before the fix, the Rust tool parsed `ref` with:
```rust
let git_ref = params.get("ref").and_then(|v| v.as_str());
```
This returned `None` for a missing field or JSON null, but accepted every JSON string
verbatim. If the LLM serialization layer supplied the literal string `"null"`, an empty
string, or whitespace, the value remained `Some` and flowed into Git's branch/refspec
arguments.

## Impact

- Clone/push may fail when `ref` is omitted or set to null
- Agent must know to always pass an explicit `ref` value

## Resolution

Fixed 2026-07-10. `ssh_git` now normalizes missing fields, JSON null, blank strings,
and case-insensitive string `"null"` values to no ref before validation and Git argv
construction. Real refs such as `main` remain unchanged; with no ref, clone follows the
remote HEAD and other operations retain Git's default ref behavior. Unit tests cover the
regression cases.
