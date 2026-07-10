"""Operations layer for inserting encrypted secrets into tenant databases."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import uuid
from datetime import datetime, timezone

KEY_SIZE = 32
NONCE_SIZE = 12
SALT_SIZE = 32
HKDF_INFO = b"near-agent-secrets-v1"

_PORTS_JSON = "/etc/lunarwing/ports.json"
_SECRET_NAME_RE = re.compile(r"^[a-zA-Z0-9_/-]+$")


def ensure_dependencies() -> bool:
    """Return True if cryptography and psycopg2 are importable."""
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: F401
        from cryptography.hazmat.primitives.kdf.hkdf import HKDF  # noqa: F401
        from cryptography.hazmat.primitives import hashes  # noqa: F401
        import psycopg2  # noqa: F401
    except ImportError:
        return False
    return True


def install_dependencies() -> bool:
    """pip install cryptography psycopg2-binary; return True on success."""
    result = subprocess.run(
        [sys.executable, "-m", "pip", "install", "cryptography", "psycopg2-binary"],
        capture_output=True,
    )
    return result.returncode == 0


def list_tenants() -> list[str]:
    """Return sorted tenant names from ports.json, or [] on error."""
    try:
        with open(_PORTS_JSON) as f:
            data = json.load(f)
        tenants = data.get("tenants", {})
        return sorted(tenants.keys())
    except (FileNotFoundError, json.JSONDecodeError, AttributeError):
        return []


def parse_tenant_env(tenant: str) -> dict[str, str]:
    """Parse /home/{tenant}/lunarwing/env/lunarwing.env into a dict."""
    path = f"/home/{tenant}/lunarwing/env/lunarwing.env"
    result: dict[str, str] = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" not in line:
                continue
            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
                value = value[1:-1]
            result[key] = value
    return result


def derive_key(master_key: bytes, salt: bytes) -> bytes:
    """HKDF-SHA256 derivation matching crypto.rs exactly."""
    from cryptography.hazmat.primitives.kdf.hkdf import HKDF
    from cryptography.hazmat.primitives import hashes

    hkdf = HKDF(
        algorithm=hashes.SHA256(),
        length=KEY_SIZE,
        salt=salt,
        info=HKDF_INFO,
    )
    return hkdf.derive(master_key)


def encrypt(master_key: bytes, plaintext: bytes) -> tuple[bytes, bytes]:
    """AES-GCM encrypt; return (nonce + ciphertext, salt)."""
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    salt = os.urandom(SALT_SIZE)
    derived_key = derive_key(master_key, salt)

    aesgcm = AESGCM(derived_key)
    nonce = os.urandom(NONCE_SIZE)
    ciphertext = aesgcm.encrypt(nonce, plaintext, None)
    encrypted = nonce + ciphertext
    return encrypted, salt


def _get_column_type(cursor, table_name: str, column_name: str) -> str | None:
    """Return the PostgreSQL data_type for a column, or None."""
    cursor.execute(
        """
        SELECT data_type
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = %s
          AND column_name = %s
        """,
        (table_name, column_name),
    )
    row = cursor.fetchone()
    return row[0] if row else None


def _resolve_user_id(cursor, owner_id: str) -> str:
    """Resolve owner_id handling both text and UUID user_id schemas."""
    user_id_type = _get_column_type(cursor, "secrets", "user_id")

    if user_id_type != "uuid":
        return owner_id

    if owner_id == "default":
        cursor.execute(
            """
            SELECT id::text
            FROM users
            ORDER BY created_at NULLS LAST, id
            LIMIT 2
            """
        )
        rows = cursor.fetchall()
        if len(rows) == 1:
            return rows[0][0]
        if len(rows) == 0:
            raise RuntimeError(
                "secrets.user_id is UUID, but no users found. "
                "Pass owner_id explicitly."
            )
        raise RuntimeError(
            "secrets.user_id is UUID and multiple users exist. "
            "Pass owner_id explicitly."
        )

    try:
        return str(uuid.UUID(owner_id))
    except ValueError:
        raise RuntimeError(
            f"secrets.user_id is UUID, but owner_id is not a valid UUID: {owner_id}"
        )


def insert_secret(
    db_url: str,
    master_key_hex: str,
    owner_id: str,
    name: str,
    value: str,
) -> str:
    """Encrypt and insert a secret; return the secret_id."""
    import psycopg2

    master_key = master_key_hex.encode("utf-8")
    plaintext = value.encode("utf-8")
    encrypted_value, key_salt = encrypt(master_key, plaintext)

    secret_id = str(uuid.uuid4())
    now = datetime.now(timezone.utc)

    conn = psycopg2.connect(db_url)
    cursor = conn.cursor()

    resolved_user_id = _resolve_user_id(cursor, owner_id)

    cursor.execute(
        """
        INSERT INTO secrets (
            id, user_id, name, encrypted_value, key_salt,
            provider, expires_at, created_at, updated_at
        )
        VALUES (%s, %s, %s, %s, %s, NULL, NULL, %s, %s)
        ON CONFLICT (user_id, name) DO UPDATE SET
            encrypted_value = EXCLUDED.encrypted_value,
            key_salt = EXCLUDED.key_salt,
            updated_at = EXCLUDED.updated_at
        """,
        (
            secret_id,
            resolved_user_id,
            name.lower(),
            encrypted_value,
            key_salt,
            now,
            now,
        ),
    )

    conn.commit()
    cursor.close()
    conn.close()

    return secret_id


def validate_secret_name(name: str) -> str | None:
    """Return None if valid, error message string otherwise."""
    if not name:
        return "Secret name cannot be empty"
    if not _SECRET_NAME_RE.match(name):
        return (
            "Secret name may only contain letters, numbers, "
            "underscores, slashes, and hyphens"
        )
    return None
