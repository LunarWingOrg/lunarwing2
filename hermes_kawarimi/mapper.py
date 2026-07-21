"""Pure transform: ``HermesAgentSnapshot`` -> ``MappedAgent`` (LunarWing rows).

No I/O, no DB, no owner id. Deterministic: identical input yields identical
output (``uuid5`` ids, sorted iteration), so a re-import updates rather than
duplicates. ``user_id`` (owner scope) is bound later by ``loader.py``.
"""

from __future__ import annotations

import json
import re
import uuid
from datetime import datetime, timezone

from hermes_kawarimi.model import (
    HermesAgentSnapshot,
    MappedAgent,
    MappedConversation,
    MappedMemoryDoc,
    MappedMessage,
    MappedSecret,
    MappedSetting,
)

# Fixed namespace so conversation/message UUIDs are stable across runs.
NAMESPACE = uuid.uuid5(uuid.NAMESPACE_URL, "https://lunarwing/hermes_kawarimi")

# Matches secrets_ops.validate_secret_name — names outside this are skipped.
_SECRET_NAME_RE = re.compile(r"^[a-zA-Z0-9_/-]+$")

# Env keys carrying credentials -> a friendly provider tag.
_PROVIDER_MAP = {
    "ANTHROPIC_API_KEY": "anthropic",
    "OPENAI_API_KEY": "openai",
    "OPENROUTER_API_KEY": "openrouter",
    "GROQ_API_KEY": "groq",
    "GEMINI_API_KEY": "google",
    "GOOGLE_API_KEY": "google",
    "MISTRAL_API_KEY": "mistral",
    "DEEPSEEK_API_KEY": "deepseek",
    "XAI_API_KEY": "xai",
    "TOGETHER_API_KEY": "together",
    "FIREWORKS_API_KEY": "fireworks",
    "COHERE_API_KEY": "cohere",
    "HF_TOKEN": "huggingface",
    "HUGGINGFACE_API_KEY": "huggingface",
}

# Substrings that mark an env key as a secret rather than plain config.
_SECRET_HINTS = (
    "API_KEY",
    "APIKEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "PRIVATE_KEY",
    "ACCESS_KEY",
    "BEARER",
)


def map_agent(snap: HermesAgentSnapshot) -> MappedAgent:
    out = MappedAgent(warnings=list(snap.warnings))
    _map_memory(snap, out)
    _map_conversations(snap, out)
    _map_secrets(snap, out)
    _map_settings(snap, out)
    return out


# --------------------------------------------------------------------------- #
# memory_documents
# --------------------------------------------------------------------------- #


def _map_memory(snap: HermesAgentSnapshot, out: MappedAgent) -> None:
    for path in sorted(snap.markdown_docs):
        out.memory_docs.append(
            MappedMemoryDoc(
                path=path,
                content=snap.markdown_docs[path],
                metadata={"imported_from": "hermes"},
            )
        )
    # Non-destructive reference copy of the full config so nothing is lost when
    # only a curated subset is promoted to settings.
    if snap.config:
        out.memory_docs.append(
            MappedMemoryDoc(
                path="imported/hermes-config.json",
                content=json.dumps(
                    snap.config, indent=2, sort_keys=True, default=str
                ),
                metadata={"imported_from": "hermes", "kind": "config"},
            )
        )


# --------------------------------------------------------------------------- #
# conversations + conversation_messages
# --------------------------------------------------------------------------- #


def _map_conversations(snap: HermesAgentSnapshot, out: MappedAgent) -> None:
    for session in snap.sessions:
        conv_id = _uuid5(f"session:{session.id}")

        msg_times = [m.timestamp for m in session.messages if m.timestamp > 0]
        started = _epoch(session.started_at) or (
            _epoch(min(msg_times)) if msg_times else _EPOCH0
        )
        last = _epoch(session.ended_at or 0) or (
            _epoch(max(msg_times)) if msg_times else started
        )

        metadata = _clean(
            {
                "imported_from": "hermes",
                "hermes_session_id": session.id,
                "source": session.source,
                "title": session.title,
                "model": session.model,
                "profile_name": session.profile_name,
                "archived": session.archived or None,
            }
        )

        conv = MappedConversation(
            id=conv_id,
            channel=session.source or "hermes",
            thread_id=session.thread_id or session.chat_id,
            started_at=started,
            last_activity=last,
            metadata=metadata,
        )

        for msg in session.messages:
            conv.messages.append(
                MappedMessage(
                    id=_uuid5(f"message:{session.id}:{msg.id}"),
                    conversation_id=conv_id,
                    role=(msg.role or "user").lower(),
                    content=_message_content(msg.content, msg.tool_calls),
                    created_at=_epoch(msg.timestamp) or started,
                )
            )
        out.conversations.append(conv)


def _message_content(content: str | None, tool_calls: str | None) -> str:
    """conversation_messages.content is NOT NULL; coalesce a usable string."""
    if content:
        return content
    if tool_calls:
        return tool_calls
    return ""


# --------------------------------------------------------------------------- #
# secrets
# --------------------------------------------------------------------------- #


def _map_secrets(snap: HermesAgentSnapshot, out: MappedAgent) -> None:
    seen: set[str] = set()
    for key in sorted(snap.env):
        value = snap.env[key]
        if not value or not _is_secret_key(key):
            continue
        name = key.lower()
        if not _SECRET_NAME_RE.match(name):
            out.warnings.append(f"skipped secret with unsupported name: {key}")
            continue
        if name in seen:
            continue
        seen.add(name)
        out.secrets.append(
            MappedSecret(name=name, value=value, provider=_provider_for(key))
        )

    # auth.json (oauth grants) is nested and schema-variable; carry it whole,
    # encrypted, so nothing is lost and no fragile per-field parsing is needed.
    if snap.auth:
        name = "hermes_auth_json"
        if name not in seen:
            out.secrets.append(
                MappedSecret(
                    name=name,
                    value=json.dumps(snap.auth, sort_keys=True),
                    provider="hermes",
                )
            )


def _is_secret_key(key: str) -> bool:
    up = key.upper()
    if up in _PROVIDER_MAP:
        return True
    return any(hint in up for hint in _SECRET_HINTS)


def _provider_for(key: str) -> str | None:
    up = key.upper()
    if up in _PROVIDER_MAP:
        return _PROVIDER_MAP[up]
    if up.endswith("_API_KEY"):
        prefix = key[: -len("_API_KEY")]
        return prefix.lower() or None
    return None


# --------------------------------------------------------------------------- #
# settings (curated, namespaced to avoid colliding with LunarWing's own keys)
# --------------------------------------------------------------------------- #


def _map_settings(snap: HermesAgentSnapshot, out: MappedAgent) -> None:
    for key in sorted(snap.config):
        value = snap.config[key]
        # Only promote top-level scalars; nested structures stay in the
        # reference config doc. Namespaced under "hermes." so we never clobber
        # LunarWing's own settings keys (e.g. "agent.name", "sandbox.enabled").
        if isinstance(value, (str, int, float, bool)):
            out.settings.append(MappedSetting(key=f"hermes.{key}", value=value))


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #

_EPOCH0 = datetime(1970, 1, 1, tzinfo=timezone.utc)


def _uuid5(name: str) -> str:
    return str(uuid.uuid5(NAMESPACE, name))


def _epoch(ts: float | None) -> datetime | None:
    """Epoch seconds -> aware UTC datetime, or None for missing/invalid."""
    if not ts or ts <= 0:
        return None
    try:
        return datetime.fromtimestamp(ts, tz=timezone.utc)
    except (OverflowError, OSError, ValueError):
        return None


def _clean(d: dict[str, object]) -> dict[str, object]:
    return {k: v for k, v in d.items() if v is not None}
