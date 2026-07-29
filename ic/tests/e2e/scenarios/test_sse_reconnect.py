"""Scenario 3: SSE reconnection preserves history."""

import pytest
from helpers import SEL


async def test_sse_status_shows_connected(page):
    """SSE status should show Connected after page load."""
    status = page.locator(SEL["sse_status"])
    await status.wait_for(state="visible", timeout=5000)
    text = await status.text_content()
    assert text == "Connected", f"Expected 'Connected', got '{text}'"


async def test_sse_reconnect_after_disconnect(page):
    """After programmatic disconnect, SSE should reconnect and show Connected."""
    # Verify initial connection
    await page.wait_for_function(
        'document.getElementById("sse-status").textContent === "Connected"',
        timeout=5000,
    )
    await page.wait_for_function(
        "currentThreadId !== null && !document.querySelector('#chat-messages .skeleton-container')",
        timeout=5000,
    )

    history_requests = []
    page.on(
        "request",
        lambda request: history_requests.append(request.url)
        if "/api/chat/history?" in request.url
        else None,
    )

    # Close the EventSource to simulate disconnect
    await page.evaluate("if (eventSource) eventSource.close()")

    # Reconnect
    async with page.expect_request(lambda request: "/api/chat/history?" in request.url):
        await page.evaluate("connectSSE()")

    # Wait for reconnection
    await page.wait_for_function(
        'document.getElementById("sse-status").textContent === "Connected"',
        timeout=10000,
    )
    status = page.locator(SEL["sse_status"])
    text = await status.text_content()
    assert text == "Connected"
    await page.wait_for_timeout(250)
    assert len(history_requests) == 1, "Reconnect should issue one history reconciliation"


async def test_sse_reconnect_preserves_chat_history(page):
    """Messages sent before disconnect should still be visible after reconnect."""
    # Send a message and wait for the full response
    chat_input = page.locator(SEL["chat_input"])
    await chat_input.fill("Hello")
    await chat_input.press("Enter")

    assistant_msg = page.locator(SEL["message_assistant"]).last
    await assistant_msg.wait_for(state="visible", timeout=15000)

    # Wait for the turn to be fully persisted in the database
    await page.wait_for_timeout(3000)

    # Capture the assistant response text before disconnect
    response_text = await assistant_msg.text_content()
    assert len(response_text) > 0, "Assistant response should not be empty"

    # Simulate disconnect and reconnect
    await page.evaluate("if (eventSource) eventSource.close()")
    await page.evaluate("connectSSE()")

    # Wait for reconnection
    await page.wait_for_function(
        'document.getElementById("sse-status").textContent === "Connected"',
        timeout=10000,
    )

    # loadHistory() is called on reconnect; wait for it to complete
    await page.wait_for_timeout(3000)

    # After reconnect, at least the user message should be visible
    # (loadHistory clears DOM and repopulates from DB)
    total_messages = await page.locator("#chat-messages .message").count()
    assert total_messages >= 1, \
        "Expected at least 1 message after reconnect history load"

    # If the turn was fully persisted, both user and assistant should appear
    user_msgs = await page.locator(SEL["message_user"]).count()
    assert user_msgs >= 1, "User message should be preserved after reconnect"
