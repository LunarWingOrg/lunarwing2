"""Unit tests for loader._populate and loader._insert_secrets.

These cover the SQL generation + transaction shape (mock psycopg2) and the
secret upsert/warning path (mock secrets_ops) without needing root or a live
PostgreSQL. Mirrors the deferred-coverage gap flagged in the v2.0.2.0 review.
"""

from __future__ import annotations

import sys
import types
import unittest
from datetime import datetime, timezone
from unittest import mock

# psycopg2 is a lazy import inside _populate; patching "psycopg2.connect"
# requires the module to be importable. Inject a stub so the tests run
# without the native driver installed.
if "psycopg2" not in sys.modules:
    _stub = types.ModuleType("psycopg2")
    _stub.connect = lambda *a, **k: None  # patched per-test
    _extras = types.ModuleType("psycopg2.extras")
    _extras.Json = lambda obj: obj
    _stub.extras = _extras
    sys.modules["psycopg2"] = _stub
    sys.modules["psycopg2.extras"] = _extras

from hermes_kawarimi.loader import ImportResult, _insert_secrets, _populate
from hermes_kawarimi.model import (
    MappedAgent,
    MappedConversation,
    MappedMemoryDoc,
    MappedMessage,
    MappedSecret,
    MappedSetting,
)


def _mapped() -> MappedAgent:
    now = datetime(2024, 1, 1, tzinfo=timezone.utc)
    conv = MappedConversation(
        id="c1",
        channel="cli",
        thread_id="t1",
        started_at=now,
        last_activity=now,
        metadata={"imported_from": "hermes"},
    )
    conv.messages = [
        MappedMessage(
            id="m1", conversation_id="c1", role="user", content="hi", created_at=now
        )
    ]
    return MappedAgent(
        memory_docs=[MappedMemoryDoc(path="SOUL.md", content="x")],
        conversations=[conv],
        secrets=[MappedSecret(name="anthropic_api_key", value="sk-x", provider="anthropic")],
        settings=[MappedSetting(key="hermes.model", value="sonnet")],
    )


class _FakeCursor:
    def __init__(self) -> None:
        self.executed: list[tuple[str, tuple]] = []
        self.rowcount = 0

    def execute(self, sql: str, params: tuple) -> None:
        self.executed.append((sql, params))
        # Simulate the UPDATE-then-INSERT idempotency shape in _populate:
        # any UPDATE statement reports 0 rows so the INSERT path runs.
        if "UPDATE" in sql.upper():
            self.rowcount = 0

    def fetchone(self) -> tuple[object,] | None:
        return None


class _FakeConn:
    def __init__(self) -> None:
        self.autocommit = False
        self.committed = False
        self.rolled_back = False
        self.closed = False
        self._cursor = _FakeCursor()

    def cursor(self) -> _FakeCursor:
        return self._cursor

    def commit(self) -> None:
        self.committed = True

    def rollback(self) -> None:
        self.rolled_back = True

    def close(self) -> None:
        self.closed = True


class PopulateTest(unittest.TestCase):
    def test_writes_all_rows_and_commits(self) -> None:
        fake = _FakeConn()
        with mock.patch("psycopg2.connect", return_value=fake):
            counts = _populate("dsn", "owner-1", _mapped())

        self.assertTrue(fake.committed)
        self.assertFalse(fake.rolled_back)
        self.assertTrue(fake.closed)
        self.assertEqual(counts["memory_docs"], 1)
        self.assertEqual(counts["conversations"], 1)
        self.assertEqual(counts["messages"], 1)
        self.assertEqual(counts["settings"], 1)

        sqls = [s.upper() for s, _ in fake._cursor.executed]
        # memory_documents: UPDATE then INSERT (rowcount 0 → INSERT fires)
        self.assertTrue(any("UPDATE MEMORY_DOCUMENTS" in s for s in sqls))
        self.assertTrue(any("INSERT INTO MEMORY_DOCUMENTS" in s for s in sqls))
        # conversations + messages: upserts
        self.assertTrue(any("INSERT INTO CONVERSATIONS" in s and "ON CONFLICT" in s for s in sqls))
        self.assertTrue(
            any("INSERT INTO CONVERSATION_MESSAGES" in s and "ON CONFLICT" in s for s in sqls)
        )
        # settings: upsert
        self.assertTrue(any("INSERT INTO SETTINGS" in s and "ON CONFLICT" in s for s in sqls))

    def test_rollback_on_error(self) -> None:
        fake = _FakeConn()

        def boom(*args: object, **kwargs: object) -> None:
            raise RuntimeError("db down")

        fake._cursor.execute = boom  # type: ignore[assignment]
        with mock.patch("psycopg2.connect", return_value=fake):
            with self.assertRaises(RuntimeError):
                _populate("dsn", "owner-1", _mapped())
        self.assertTrue(fake.rolled_back)
        self.assertFalse(fake.committed)
        self.assertTrue(fake.closed)

    def test_update_hit_skips_insert(self) -> None:
        fake = _FakeConn()
        fake._cursor.rowcount = 1  # UPDATE matched → INSERT must NOT fire

        with mock.patch("psycopg2.connect", return_value=fake):
            _populate("dsn", "owner-1", _mapped())

        inserts = [
            s
            for s, _ in fake._cursor.executed
            if "INSERT INTO memory_documents" in s.upper()
        ]
        self.assertEqual(inserts, [])


class InsertSecretsTest(unittest.TestCase):
    def test_inserts_each_secret_and_counts(self) -> None:
        result = ImportResult()
        with mock.patch(
            "hermes_kawarimi.loader.secrets_ops.insert_secret"
        ) as mocked:
            _insert_secrets("dsn", "key", "owner-1", _mapped(), result)
        self.assertEqual(result.counts["secrets"], 1)
        self.assertEqual(mocked.call_count, 1)
        mocked.assert_called_once_with(
            "dsn", "key", "owner-1", "anthropic_api_key", "sk-x"
        )

    def test_failure_appends_warning_not_error(self) -> None:
        result = ImportResult()
        with mock.patch(
            "hermes_kawarimi.loader.secrets_ops.insert_secret",
            side_effect=RuntimeError("encrypt failed"),
        ):
            _insert_secrets("dsn", "key", "owner-1", _mapped(), result)
        self.assertEqual(result.counts.get("secrets"), 0)
        self.assertTrue(
            any("anthropic_api_key" in w and "encrypt failed" in w for w in result.warnings)
        )
        # populate-level error stays None — per-secret failures are warnings
        self.assertIsNone(result.error)


if __name__ == "__main__":
    unittest.main()
