"""Data model for the Hermes -> LunarWing import.

Two layers:

* **Source IR** (``HermesAgentSnapshot`` and friends) — a neutral, in-memory
  representation of what ``extract.py`` reads off disk from a Hermes agent.
* **Mapped rows** (``MappedAgent`` and friends) — owner-agnostic LunarWing row
  objects produced by ``mapper.py``. ``user_id`` (the owner scope) is
  intentionally NOT baked in here: it is only known after a tenant is
  provisioned, so ``loader.py`` binds it at insert time.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

# --------------------------------------------------------------------------- #
# Source IR — mirrors the Hermes on-disk footprint
# --------------------------------------------------------------------------- #


@dataclass
class HermesMessage:
    """One row of Hermes ``state.db`` ``messages``."""

    id: int
    role: str
    content: str | None
    timestamp: float
    tool_calls: str | None = None
    tool_name: str | None = None


@dataclass
class HermesSession:
    """One row of Hermes ``state.db`` ``sessions`` plus its messages."""

    id: str
    source: str
    started_at: float
    user_id: str | None = None
    chat_id: str | None = None
    thread_id: str | None = None
    title: str | None = None
    model: str | None = None
    profile_name: str | None = None
    ended_at: float | None = None
    archived: bool = False
    messages: list[HermesMessage] = field(default_factory=list)


@dataclass
class HermesAgentSnapshot:
    """Everything ``extract.py`` reads from a single Hermes agent home."""

    source_path: str
    # Relative path (as it will land in the LunarWing workspace) -> markdown text.
    # e.g. {"SOUL.md": "...", "MEMORY.md": "...", "memories/notes.md": "..."}
    markdown_docs: dict[str, str] = field(default_factory=dict)
    sessions: list[HermesSession] = field(default_factory=list)
    config: dict[str, Any] = field(default_factory=dict)  # parsed config.yaml
    env: dict[str, str] = field(default_factory=dict)  # parsed .env
    auth: dict[str, Any] = field(default_factory=dict)  # parsed auth.json
    # Non-fatal problems encountered while reading (surfaced in reports).
    warnings: list[str] = field(default_factory=list)

    @property
    def message_count(self) -> int:
        return sum(len(s.messages) for s in self.sessions)


# --------------------------------------------------------------------------- #
# Mapped rows — owner-agnostic LunarWing targets
# --------------------------------------------------------------------------- #


@dataclass
class MappedMemoryDoc:
    """A row for ``memory_documents`` (``user_id``/``agent_id`` bound at load)."""

    path: str
    content: str
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class MappedMessage:
    """A row for ``conversation_messages``."""

    id: str  # UUID
    conversation_id: str  # UUID
    role: str
    content: str
    created_at: datetime


@dataclass
class MappedConversation:
    """A row for ``conversations`` plus its messages."""

    id: str  # UUID
    channel: str
    thread_id: str | None
    started_at: datetime
    last_activity: datetime
    metadata: dict[str, Any] = field(default_factory=dict)
    messages: list[MappedMessage] = field(default_factory=list)


@dataclass
class MappedSecret:
    """A secret to encrypt into the ``secrets`` table via ``secrets_ops``."""

    name: str
    value: str
    provider: str | None = None


@dataclass
class MappedSetting:
    """A row for ``settings`` (``value`` serialized to JSONB at load)."""

    key: str
    value: Any


@dataclass
class MappedAgent:
    """The full owner-agnostic mapping produced by ``mapper.py``."""

    memory_docs: list[MappedMemoryDoc] = field(default_factory=list)
    conversations: list[MappedConversation] = field(default_factory=list)
    secrets: list[MappedSecret] = field(default_factory=list)
    settings: list[MappedSetting] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    @property
    def message_count(self) -> int:
        return sum(len(c.messages) for c in self.conversations)

    def counts(self) -> dict[str, int]:
        """Summary counts for the dry-run/inspect report."""
        return {
            "memory_docs": len(self.memory_docs),
            "conversations": len(self.conversations),
            "messages": self.message_count,
            "secrets": len(self.secrets),
            "settings": len(self.settings),
        }
