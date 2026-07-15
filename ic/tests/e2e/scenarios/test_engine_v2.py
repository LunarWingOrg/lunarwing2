"""Isolated browser compatibility scenarios for Engine V2."""

import asyncio
import json
import time

import httpx

from helpers import SEL


STREAM_TEXT = "e2e-engine-v2-stream-text"
APPROVAL = "e2e-engine-v2-approval"
AUTH = "e2e-engine-v2-auth"
LONG_STREAM = "e2e-engine-v2-long-stream"
RECOVERY = "e2e-engine-v2-recovery"
SYNTHETIC_AUTH_TOKEN = "engine-v2-private-token-7391"


async def _reset_mock(mock_llm_server: str) -> None:
    async with httpx.AsyncClient() as client:
        response = await client.post(
            f"{mock_llm_server}/__mock/engine-v2/reset",
            timeout=5,
        )
    response.raise_for_status()


async def _mock_calls(mock_llm_server: str) -> dict[str, int]:
    async with httpx.AsyncClient() as client:
        response = await client.get(
            f"{mock_llm_server}/__mock/engine-v2/state",
            timeout=5,
        )
    response.raise_for_status()
    return response.json()["calls"]


async def _send(page, content: str) -> None:
    chat_input = page.locator(SEL["chat_input"])
    await chat_input.wait_for(state="visible", timeout=5000)
    await chat_input.fill(content)
    await chat_input.press("Enter")


async def _sse_events(page, event_type: str) -> list[dict]:
    events = await page.evaluate(
        "eventType => window.__engineV2SseEvents.filter(event => event.type === eventType)",
        arg=event_type,
    )
    decoded = []
    for event in events:
        try:
            data = json.loads(event["data"])
        except json.JSONDecodeError:
            data = event["data"]
        decoded.append({**event, "decoded": data})
    return decoded


async def _wait_for_assistant_text(page, expected: str, timeout: int = 15000) -> None:
    await page.wait_for_function(
        """
        expected => {
          const messages = document.querySelectorAll('#chat-messages .message.assistant');
          const last = messages[messages.length - 1];
          return !!last && (last.textContent || '').includes(expected);
        }
        """,
        arg=expected,
        timeout=timeout,
    )


async def test_engine_v2_streams_partial_text_into_one_terminal_bubble(
    engine_v2_page,
    mock_llm_server,
):
    await _reset_mock(mock_llm_server)
    assistant = engine_v2_page.locator(SEL["message_assistant"])
    before = await assistant.count()

    await _send(engine_v2_page, STREAM_TEXT)
    await engine_v2_page.wait_for_function(
        """
        () => {
          const messages = document.querySelectorAll('#chat-messages .message.assistant');
          const last = messages[messages.length - 1];
          return last?.dataset.streaming === 'true'
            && (last.textContent || '').includes('engine-v2 partial');
        }
        """,
        timeout=5000,
    )

    partial_events = await _sse_events(engine_v2_page, "stream_chunk")
    assert any(
        event["decoded"].get("content") == "engine-v2 partial "
        for event in partial_events
        if isinstance(event["decoded"], dict)
    )
    assert await _sse_events(engine_v2_page, "response") == []

    await _wait_for_assistant_text(engine_v2_page, "engine-v2 partial streaming complete")
    await engine_v2_page.wait_for_function(
        """
        () => {
          const messages = document.querySelectorAll('#chat-messages .message.assistant');
          const last = messages[messages.length - 1];
          return !!last && !last.hasAttribute('data-streaming');
        }
        """,
        timeout=5000,
    )
    assert await assistant.count() == before + 1
    assert await assistant.last.get_attribute("data-streaming") is None

    stream_events = await _sse_events(engine_v2_page, "stream_chunk")
    response_events = await _sse_events(engine_v2_page, "response")
    assert response_events
    assert stream_events[0]["timestamp"] < response_events[-1]["timestamp"]
    assert (await _mock_calls(mock_llm_server))["stream_text"] == 1


async def test_engine_v2_approval_card_resumes_once(
    engine_v2_page,
    mock_llm_server,
):
    await _reset_mock(mock_llm_server)
    assistant = engine_v2_page.locator(SEL["message_assistant"])
    before = await assistant.count()

    await _send(engine_v2_page, APPROVAL)
    card = engine_v2_page.locator(SEL["approval_card"])
    await card.wait_for(state="visible", timeout=10000)
    assert await card.count() == 1
    assert "tool install" in (await card.locator(SEL["approval_tool_name"]).inner_text()).lower()

    await card.locator(SEL["approval_approve_btn"]).click()
    await _wait_for_assistant_text(engine_v2_page, "engine-v2 approval complete")
    assert await assistant.count() == before + 1
    assert await card.count() == 1
    assert (await _mock_calls(mock_llm_server))["approval"] == 2

    network = await engine_v2_page.evaluate("window.__engineV2Network")
    approval_posts = [
        event
        for event in network
        if event["method"] == "POST" and event["url"].endswith("/api/chat/approval")
    ]
    assert len(approval_posts) == 1


async def test_engine_v2_auth_token_resumes_without_dom_leak(
    engine_v2_page,
    mock_llm_server,
):
    await _reset_mock(mock_llm_server)
    assistant = engine_v2_page.locator(SEL["message_assistant"])
    before = await assistant.count()

    await _send(engine_v2_page, AUTH)
    auth_card = engine_v2_page.locator(SEL["auth_card"])
    await auth_card.wait_for(state="visible", timeout=15000)
    assert await auth_card.count() == 1

    await auth_card.locator("input[type='password']").fill(SYNTHETIC_AUTH_TOKEN)
    await auth_card.locator(SEL["auth_submit_btn"]).click()
    await auth_card.wait_for(state="hidden", timeout=10000)
    await _wait_for_assistant_text(
        engine_v2_page,
        "engine-v2 authentication complete",
        timeout=20000,
    )
    assert await assistant.count() == before + 1
    assert SYNTHETIC_AUTH_TOKEN not in await engine_v2_page.locator("body").inner_text()
    assert SYNTHETIC_AUTH_TOKEN not in await engine_v2_page.content()
    assert (await _mock_calls(mock_llm_server))["auth"] == 2

    assert len(await _sse_events(engine_v2_page, "auth_required")) == 1
    assert len(await _sse_events(engine_v2_page, "auth_completed")) == 1


async def test_engine_v2_interrupts_long_stream_and_recovers_same_thread(
    engine_v2_page,
    mock_llm_server,
):
    await _reset_mock(mock_llm_server)
    assistant = engine_v2_page.locator(SEL["message_assistant"])
    before = await assistant.count()
    thread_before = await engine_v2_page.evaluate("currentThreadId")

    await _send(engine_v2_page, LONG_STREAM)
    await _wait_for_assistant_text(engine_v2_page, "engine-v2 long partial", timeout=5000)
    partial_events = await _sse_events(engine_v2_page, "stream_chunk")
    assert any(
        event["decoded"].get("content") == "engine-v2 long partial"
        for event in partial_events
        if isinstance(event["decoded"], dict)
    )

    started = time.monotonic()
    await _send(engine_v2_page, "/interrupt")
    await _wait_for_assistant_text(engine_v2_page, "Interrupted.", timeout=2000)
    assert time.monotonic() - started <= 2.0

    await asyncio.sleep(1)
    all_text = await engine_v2_page.locator(SEL["chat_messages"]).inner_text()
    assert "terminal should not arrive" not in all_text
    assert await assistant.count() == before + 1

    await _send(engine_v2_page, RECOVERY)
    await _wait_for_assistant_text(engine_v2_page, "engine-v2 recovery complete")
    assert await assistant.count() == before + 2
    assert await engine_v2_page.evaluate("currentThreadId") == thread_before

    calls = await _mock_calls(mock_llm_server)
    assert calls["long_stream"] == 1
    assert calls["recovery"] == 1
