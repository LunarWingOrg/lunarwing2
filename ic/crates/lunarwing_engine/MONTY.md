# Monty Integration

Monty is the embedded Python interpreter used for Tier 1 (CodeAct) execution. It's a lightweight Rust-native Python implementation — not CPython — so it has a restricted feature set.

**Source**: `git = "https://github.com/pydantic/monty.git", branch = "main"`
**Pinned at**: `7a0d4b7` (2026-03-29, "Support multi-module import statements")

## Upgrade Process

1. **Update the pin**: `cargo update -p monty`
2. **Check for new features**: `cd ~/.cargo/git/checkouts/monty-*/*/` and `git log --oneline` since last pin
3. **Update the preamble**: If a previously-unsupported feature now works, remove it from the "Runtime environment" section in `prompts/codeact_preamble.md`
4. **Update this file**: Record the new pin and what changed
5. **Run tests**: `cargo test -p lunarwing_engine`
6. **Watch traces**: After deploying, check traces for new `NotImplementedError` patterns (self-improvement mission catches these)

## Current Limitations (as of pin `7a0d4b7`)

These are documented in `prompts/codeact_preamble.md` so the LLM avoids them:

### Syntax not supported
| Feature | Workaround |
|---------|-----------|
| `class Foo:` | Use functions and dicts |
| `with` statements | Use try/finally or direct calls |
| `match` statements | Use if/elif chains |
| `del` statement | Reassign to None |
| `yield` / `yield from` | Use lists and list comprehensions |
| `*expr` (starred expressions) | Unpack explicitly |
| `async` / `await` | Not available; tool calls suspend the VM automatically |
| Type aliases (`type X = ...`) | Omit type annotations |
| Template strings (t-strings) | Use f-strings |
| Complex number literals | Use floats |
| Exception groups (`try*/except*`) | Use regular try/except |

### Limited standard library
`import csv`, `import os`, `import io`, etc. still fail.

Available built-in modules:
- `datetime` — date and time handling
- `json` — JSON encoding/decoding
- `math` — standard math functions
- `re` — regex (basic)
- `sys` — system info (limited)
- `os.path` — path manipulation (limited)
- `typing` — type hints (limited, for annotation only)

### Available builtins
`abs`, `all`, `any`, `bin`, `chr`, `divmod`, `enumerate`, `filter`, `getattr`, `hash`, `hex`, `id`, `isinstance`, `len`, `map`, `min`, `max`, `next`, `oct`, `ord`, `pow`, `print`, `repr`, `reversed`, `round`, `sorted`, `sum`, `type`, `zip`

### Host-provided functions (always available)
These are injected by the LunarWing executor, not by Monty:
- `FINAL(answer)` / `FINAL_VAR(name)` — terminate with result
- `llm_query(prompt, context)` — recursive LLM sub-call
- `llm_query_batched(prompts)` — parallel sub-calls
- `rlm_query(prompt)` — full sub-agent with tools
- `globals()` / `locals()` — returns dict of known tool names
- All tool functions (web_search, http, time, etc.)

### Auto-import pre-injection (WI#364)
Each MontyRun is block-scoped — imports do not carry across code blocks. When
LLM-generated code references `asyncio.`, `json.`, `re.`, `math.`,
`datetime.`, or `sys.` without a matching `import` in the same block, the
LunarWing executor prepends the missing `import` statement before the code
reaches Monty (`ic/crates/lunarwing_engine/src/executor/scripting.rs`,
`inject_missing_imports`). Explicit imports take precedence — the helper only
adds imports that the code itself does not already provide. Two guards keep the
injection safe: a name the block calls as a bare function (`json(...)`, the
builtin tool) is never injected (it would rebind the tool's name to the
module), and only imports that bind the bare module name count as existing
(`from module import x` and `import module as alias` do not suppress
injection). Reported error positions are shifted back to the model's own
block, so line numbers and byte ranges in tracebacks stay truthful. This is
the orchestrator-level (Level 1.5) mitigation for the recurring
asyncio-NameError pattern documented in WI#364; prompt-level rules alone had
saturated.

## Upgrade Changelog

| Date | Pin | Notable changes |
|------|-----|-----------------|
| 2026-03-29 | `7a0d4b7` | Multi-module imports, `datetime` module, `json` module, nested subscript assignment, `str.expandtabs()`. |
| 2026-03-20 | `6053820` | Initial integration. max() kwargs support. |
