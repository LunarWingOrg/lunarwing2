# Design: `secrets` subcommand for `lunarwing_mt_onboard`

**Date:** 2026-07-10
**Status:** Draft
**Author:** Christopher (via brainstorming with Sisyphus)

## Problem

Inserting secrets into a running tenant today requires the operator to manually:

1. Look up the tenant's `DATABASE_URL` from `/home/<tenant>/lunarwing/env/lunarwing.env`
2. Look up the `SECRETS_MASTER_KEY` from the same file
3. Look up the `LUNARWING_OWNER_ID` from the same file
4. Export all three as environment variables
5. Ensure `cryptography` and `psycopg2-binary` are installed
6. Run `ic_sm/scripts_4_db/insert_secret_pg.py <name> <value>`

This is tedious, error-prone, and requires knowledge of internal file paths.

## Solution

Add a `secrets` subcommand to `lunarwing_mt_onboard` that automates tenant env resolution, dependency management, and interactive secret entry.

## Design

### Subcommand registration

A new `secrets` subparser is added to `_build_parser()` in `cli.py`, following the exact pattern used by `upgrade` and `export`:

```python
secrets = subparsers.add_parser(
    "secrets",
    help="Insert secrets into a running tenant's encrypted secrets store.",
)
_add_secrets_args(secrets)
```

The dispatch in `main()` gains one new branch:

```python
if getattr(args, "command", None) == "secrets":
    return run_secrets_flow(SecretsCliArgs.from_namespace(args))
```

This slots in before the default provision path, alongside the existing `upgrade` and `export` dispatches.

### CLI flags

```
python3 -m lunarwing_mt_onboard secrets [OPTIONS]

Options:
  --tenant NAME         Tenant name (skip interactive picker)
  --secretname NAME     Secret name (skip interactive prompt; requires --non-interactive)
  --secretvalue VALUE   Secret value (skip interactive prompt; requires --non-interactive)
  --non-interactive     Run without prompts; requires --tenant, --secretname, --secretvalue
  --yes                 Skip confirmation prompts in non-interactive mode
```

Flag names are `--secretname` and `--secretvalue` (not `--name`/`--value`) to avoid ambiguity with `--tenant` and other generic flags.

### Interactive flow

```
┌─────────────────────────────────────────────┐
│  1. Dependency check                        │
│     - Verify cryptography importable        │
│     - Verify psycopg2 importable            │
│     - If missing: prompt to pip install     │
│       → "Install missing dependencies? [Y/n]"│
│       → pip install cryptography            │
│         psycopg2-binary                     │
│       → re-check; abort if still failing    │
├─────────────────────────────────────────────┤
│  2. Tenant selection                        │
│     - Read /etc/lunarwing/ports.json        │
│     - Extract tenant names from .tenants    │
│     - questionary.select("Select tenant:")  │
│     - Abort if no tenants registered        │
├─────────────────────────────────────────────┤
│  3. Env auto-resolution                     │
│     - Read /home/<tenant>/lunarwing/env/    │
│       lunarwing.env                         │
│     - Extract DATABASE_URL                  │
│     - Extract SECRETS_MASTER_KEY            │
│     - Extract LUNARWING_OWNER_ID            │
│     - Abort with clear error if any missing │
├─────────────────────────────────────────────┤
│  4. Secret entry (loops)                    │
│     a. Secret name (questionary.text)       │
│        - Validate: non-empty,               │
│          ^[a-zA-Z0-9_/-]+$                  │
│     b. Secret value (questionary.password)  │
│        - Hidden input, no echo              │
│     c. Confirm value (questionary.password) │
│        - Must match; mismatch → re-enter    │
│     d. Encrypt + upsert into DB             │
│     e. Show rich Panel:                     │
│        OK: secret '<name>' stored            │
│        for tenant '<tenant>'                 │
│     f. questionary.confirm("Add another?")  │
│        - Yes → goto (a)                     │
│        - No → exit 0                        │
└─────────────────────────────────────────────┘
```

### Non-interactive flow

When `--non-interactive` is passed with `--tenant`, `--secretname`, and `--secretvalue`:

1. Same dependency check (abort if missing, no pip prompt)
2. Same env auto-resolution for the named tenant
3. Encrypt + insert directly (no prompts, no loop)
4. Print result to stdout, exit 0/1

### Crypto

The crypto logic is moved from `insert_secret_pg.py` into `secrets_ops.py` as reusable functions. The constants and algorithm are identical to what the Rust runtime uses (`src/secrets/crypto.rs`) and what the existing script uses:

- **Key derivation**: HKDF-SHA256 with per-secret random 32-byte salt
- **Encryption**: AES-256-GCM
- **Nonce**: 12 bytes random, prepended to ciphertext
- **HKDF info**: `b"near-agent-secrets-v1"`
- **Master key**: hex-encoded, ≥32 bytes, from `SECRETS_MASTER_KEY`

The DB upsert uses `ON CONFLICT (user_id, name) DO UPDATE` so re-inserting a secret with the same name updates it in place.

### Post-provisioning tip

After successful provisioning in the standard `python3 -m lunarwing_mt_onboard` flow (after `verify_tenant` passes), print:

```
💡 You can add more secrets (API keys, tokens) to this tenant at any time:
   python3 -m lunarwing_mt_onboard secrets
```

This is added to `main()` in `cli.py`, right after the `_show_verify()` call in the success path (around line 517).

### Tenant env parsing

A helper function `parse_tenant_env(tenant: str) -> dict[str, str]` reads `/home/<tenant>/lunarwing/env/lunarwing.env` and parses KEY=VALUE lines into a dict. This is the same env file that `mt-admin.sh` generates via `write_tenant_lunarwing_env()`.

Required keys:
- `DATABASE_URL` — PostgreSQL connection string for the tenant's DB
- `SECRETS_MASTER_KEY` — hex-encoded master encryption key
- `LUNARWING_OWNER_ID` — the tenant's user scope in the secrets table

If any key is missing or the file doesn't exist, abort with a clear error message naming the missing key and the file path.

### Tenant listing

A helper function `list_tenants() -> list[str]` reads `/etc/lunarwing/ports.json`, parses the JSON, and returns the keys of the `.tenants` object. Used to populate the interactive tenant picker. If the file doesn't exist or `.tenants` is empty, abort with: "No tenants registered. Run `python3 -m lunarwing_mt_onboard` first."

## Files

| File | Change | Description |
|---|---|---|
| `lunarwing_mt_onboard/secrets_ops.py` | **NEW** | Crypto functions (moved from `insert_secret_pg.py`), DB connect/insert, `parse_tenant_env()`, `list_tenants()`, `ensure_dependencies()` |
| `lunarwing_mt_onboard/secrets_cli.py` | **NEW** | `SecretsCliArgs` dataclass, `run_secrets_flow()`, interactive prompts, rich display, dependency check + pip install |
| `lunarwing_mt_onboard/cli.py` | **EDIT** | Add `secrets` subparser + `_add_secrets_args()`, add dispatch branch in `main()`, add post-verify tip |
| `lunarwing_mt_onboard/README.md` | **EDIT** | Document the `secrets` subcommand: usage, flags, examples |

No changes to existing files outside the package. No changes to `insert_secret_pg.py` or `mt-admin.sh`.

## Module structure

```
secrets_ops.py
├── ensure_dependencies() -> bool
├── install_dependencies() -> bool
├── list_tenants() -> list[str]
├── parse_tenant_env(tenant: str) -> dict[str, str]
├── derive_key(master_key: bytes, salt: bytes) -> bytes
├── encrypt(master_key: bytes, plaintext: bytes) -> tuple[bytes, bytes]
├── insert_secret(db_url, master_key_hex, owner_id, name, value) -> str
│           # returns secret_id
└── # Constants: KEY_SIZE, NONCE_SIZE, SALT_SIZE, HKDF_INFO

secrets_cli.py
├── SecretsCliArgs (dataclass)
│   ├── tenant: str
│   ├── secretname: str
│   ├── secretvalue: str
│   ├── non_interactive: bool
│   ├── yes: bool
│   └── from_namespace(args) -> SecretsCliArgs
├── run_secrets_flow(args: SecretsCliArgs) -> int
├── _interactive_tenant_select() -> str
├── _interactive_secret_loop(tenant_info) -> None
└── _non_interactive_insert(tenant_info, name, value) -> int
```

## Security

- Secret values entered via `questionary.password` (hidden, no echo)
- Master key and DATABASE_URL read from env file — never printed to terminal
- No secrets passed on argv in interactive mode
- Non-interactive `--secretvalue` on argv is visible in `/proc/<pid>/cmdline` — this is an accepted tradeoff for CI use (same as the existing `insert_secret_pg.py` script)
- The master key is hex-encoded and ≥32 bytes; raw bytes used only for HKDF derivation
- DB connection uses the tenant's own credentials from the env file

## Testing

Unit tests in a new `secrets_tests.py` module, run via the existing `test-mt-onboard.sh` harness:

- `test_derive_key` — known vector: same master_key + salt = same derived key
- `test_encrypt_decrypt` — encrypt then manually decrypt (AES-GCM), verify roundtrip
- `test_parse_tenant_env` — parse a sample env file, verify key extraction
- `test_parse_tenant_env_missing_key` — missing `DATABASE_URL` → clear error
- `test_list_tenants_empty` — no ports.json → empty list
- `test_list_tenants_populated` — mock ports.json → correct names
- `test_secret_name_validation` — reject empty, accept valid names

## Out of scope

- List or delete secret operations (can be added later)
- Changes to `insert_secret_pg.py` or other existing scripts
- Changes to `mt-admin.sh`
- libSQL support (PostgreSQL-only, matching the primary backend)
- Bulk import from file
