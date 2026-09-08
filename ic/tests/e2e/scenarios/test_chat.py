"""Scenario 2: Chat message round-trip via SSE streaming."""

import pytest
from helpers import SEL


async def test_send_message_and_receive_response(page):
    """Type a message, receive a streamed response from mock LLM."""
    chat_input = page.locator(SEL["chat_input"])
    await chat_input.wait_for(state="visible", timeout=5000)

    # Send message
    await chat_input.fill("What is 2+2?")
    await chat_input.press("Enter")

    # Wait for assistant response
    assistant_msg = page.locator(SEL["message_assistant"]).last
    await assistant_msg.wait_for(state="visible", timeout=15000)

    # Verify user message
    user_msgs = page.locator(SEL["message_user"])
    assert await user_msgs.count() >= 1
    last_user = user_msgs.last
    user_text = await last_user.text_content()
    assert "2+2" in user_text or "2 + 2" in user_text

    # Verify assistant response contains "4" (from mock LLM canned response)
    assistant_text = await assistant_msg.text_content()
    assert "4" in assistant_text, f"Expected '4' in response, got: '{assistant_text}'"


async def test_multiple_messages(page):
    """Send two messages, verify both get responses."""
    chat_input = page.locator(SEL["chat_input"])
    await chat_input.wait_for(state="visible", timeout=5000)

    # First message
    await chat_input.fill("Hello")
    await chat_input.press("Enter")

    # Wait for first response
    await page.locator(SEL["message_assistant"]).first.wait_for(
        state="visible", timeout=15000
    )

    # Second message
    await chat_input.fill("What is 2+2?")
    await chat_input.press("Enter")

    # Wait for second response (at least 2 assistant messages)
    await page.wait_for_function(
        """() => document.querySelectorAll('#chat-messages .message.assistant').length >= 2""",
        timeout=15000,
    )

    # Verify counts
    user_count = await page.locator(SEL["message_user"]).count()
    assistant_count = await page.locator(SEL["message_assistant"]).count()
    assert user_count >= 2, f"Expected >= 2 user messages, got {user_count}"
    assert assistant_count >= 2, f"Expected >= 2 assistant messages, got {assistant_count}"


async def test_empty_message_not_sent(page):
    """Pressing Enter with empty input should not create a message."""
    chat_input = page.locator(SEL["chat_input"])
    await chat_input.wait_for(state="visible", timeout=5000)

    initial_count = await page.locator(f"{SEL['message_user']}, {SEL['message_assistant']}").count()

    # Press Enter with empty input
    await chat_input.press("Enter")

    # Wait a moment and verify no new messages
    await page.wait_for_timeout(2000)
    final_count = await page.locator(f"{SEL['message_user']}, {SEL['message_assistant']}").count()
    assert final_count == initial_count, "Empty message should not create new messages"


async def test_copy_from_chat_forces_plain_text(page):
    """Copying selected chat text should populate plain text clipboard data only."""
    await page.evaluate("addMessage('assistant', 'Copy me into Sheets')")

    copied = await page.evaluate(
        """
        () => {
          const content = Array.from(document.querySelectorAll('#chat-messages .message.assistant .message-content'))
            .find((el) => (el.textContent || '').includes('Copy me into Sheets'));
          if (!content) return {ok: false, reason: 'no content'};
          const range = document.createRange();
          range.selectNodeContents(content);
          const sel = window.getSelection();
          sel.removeAllRanges();
          sel.addRange(range);

          const store = {};
          const evt = new Event('copy', { bubbles: true, cancelable: true });
          evt.clipboardData = {
            clearData: () => { Object.keys(store).forEach((k) => delete store[k]); },
            setData: (t, v) => { store[t] = v; },
            getData: (t) => store[t] || '',
          };

          content.dispatchEvent(evt);
          return {
            ok: true,
            defaultPrevented: evt.defaultPrevented,
            text: store['text/plain'] || '',
            html: store['text/html'] || '',
          };
        }
        """
    )

    assert copied["ok"], copied.get("reason", "copy setup failed")
    assert copied["defaultPrevented"] is True
    assert "Copy me into Sheets" in copied["text"]
    assert copied["html"] == ""


async def test_live_work_and_reasoning_stay_separate_from_final_answer(page):
    """Intermediate work remains inspectable without becoming final response text."""
    result = await page.evaluate(
        """
        () => {
          finalizeActivityGroup();
          resetStreamingState();
          const container = document.getElementById('chat-messages');
          container.replaceChildren();

          showActivityReasoning(
            'Selected 1 tool based on task requirements and available context.',
            [{tool_name: 'web_search', rationale: 'Gather current external information relevant to the request.'}],
          );
          getOrCreateStreamingMessage('test-thread');
          _streamBuffer = 'private working draft';
          flushStreamBuffer();
          finishStreamingWork('test-thread');
          finalizeActivityGroup();
          addMessage('assistant', 'Authoritative final answer');

          return {
            workLog: container.querySelector('.activity-summary')?.textContent || '',
            liveWork: container.querySelector('.activity-live-work-output')?.textContent || '',
            rationale: container.querySelector('.reasoning-decision-rationale')?.textContent || '',
            finalText: container.querySelector('.message.assistant .message-content')?.textContent || '',
          };
        }
        """
    )

    assert "Work log" in result["workLog"]
    assert result["liveWork"] == "private working draft"
    assert "external information" in result["rationale"]
    assert result["finalText"] == "Authoritative final answer"
    assert "working draft" not in result["finalText"]


async def _install_fake_eventsource(page):
    """Install a controllable EventSource fake before app.js binds.

    app.js constructs `new EventSource('/api/chat/events?...')` and attaches
    named listeners via addEventListener. The fake records listeners and
    exposes window.__emitSSE(type, data) to dispatch to them, so tests can
    drive the exact SSE contract without a live server stream."""
    await page.add_init_script(
        """
        window.__sseListeners = {};
        window.__sseHistory = { open: false, url: '' };
        class FakeEventSource {
          constructor(url) {
            this.url = url;
            this._listeners = {};
            window.__sseHistory = { open: true, url };
            window.__sseTarget = this;
          }
          addEventListener(type, fn) {
            (this._listeners[type] = this._listeners[type] || []).push(fn);
          }
          removeEventListener(type, fn) {
            const arr = this._listeners[type] || [];
            const i = arr.indexOf(fn);
            if (i >= 0) arr.splice(i, 1);
          }
          dispatch(type, dataObj) {
            const evt = new MessageEvent(type, {
              data: JSON.stringify(dataObj),
            });
            (this._listeners[type] || []).forEach((fn) => fn(evt));
          }
          close() { /* no-op for fake */ }
        }
        window.EventSource = FakeEventSource;
        """
    )


async def test_stream_gap_reloads_history(page):
    """A server-reported broadcast gap discards partial stream state and
    immediately reloads persisted history."""
    await _install_fake_eventsource(page)
    await page.goto("/")

    await page.evaluate(
        """
        () => {
          window.__loadHistoryCalls = 0;
          window.__realLoadHistory = window.loadHistory;
          window.loadHistory = () => { window.__loadHistoryCalls += 1; };
          markResponseRendered('test-thread');
          getOrCreateStreamingMessage('test-thread');
          _streamParts = ['incomplete streamed text'];
        }
        """
    )

    await page.evaluate(
        """
        () => window.__sseTarget.dispatch('stream_gap', {
          missed_events: 4,
        })
        """
    )

    result = await page.evaluate(
        """
        () => ({
          calls: window.__loadHistoryCalls,
          streamCleared: _streamingMessage === null && _streamParts.length === 0,
          responseMarkerCleared: !_responseRenderedAt.has('test-thread'),
        })
        """
    )
    assert result == {
        "calls": 1,
        "streamCleared": True,
        "responseMarkerCleared": True,
    }

    await page.evaluate(
        "() => { window.loadHistory = window.__realLoadHistory; }"
    )


async def test_done_without_response_reloads_history(page):
    """Done without a terminal response reloads history even when live stream
    text arrived before the terminal event."""
    await _install_fake_eventsource(page)

    await page.goto("/")

    # App.js bound its SSE listeners to the fake. Prime the stream state:
    # chunks for the current thread, NO response event ever dispatched.
    await page.evaluate(
        """
        () => {
          window.__loadHistoryCalls = 0;
          window.__realLoadHistory = window.loadHistory;
          window.loadHistory = (...args) => { window.__loadHistoryCalls += 1; };

          const container = document.getElementById('chat-messages');
          container.replaceChildren();

          getOrCreateStreamingMessage('test-thread');
          _streamParts = ['partial answer that WILL be lost'];
          _streamBuffer = '';
        }
        """
    )

    # Dispatch Done through the fake EventSource — the real app.js handler
    # (bound via addEventListener) receives it exactly as it would live.
    await page.evaluate(
        """
        () => window.__sseTarget.dispatch('status', {
          thread_id: 'test-thread',
          message: 'Done',
        })
        """
    )

    # Guard schedules loadHistory after its 1.5s grace period.
    await page.wait_for_function(
        "() => window.__loadHistoryCalls >= 1",
        timeout=5000,
    )

    calls = await page.evaluate("() => window.__loadHistoryCalls")
    assert calls == 1, f"expected exactly 1 loadHistory call, got {calls}"

    await page.evaluate(
        "() => { window.loadHistory = window.__realLoadHistory; }"
    )


async def test_done_after_response_does_not_reload_history(page):
    """Healthy path: response event arrives and renders (markResponseRendered
    fires inside the real handler), so the Done guard must NOT schedule a
    history reload — no duplicate render, no wasted fetch."""
    await _install_fake_eventsource(page)

    await page.goto("/")

    await page.evaluate(
        """
        () => {
          window.__loadHistoryCalls = 0;
          window.__realLoadHistory = window.loadHistory;
          window.loadHistory = (...args) => { window.__loadHistoryCalls += 1; };
          const container = document.getElementById('chat-messages');
          container.replaceChildren();
        }
        """
    )

    # Response first (healthy path) — dispatched through the fake so the REAL
    # app.js handler runs, including markResponseRendered.
    await page.evaluate(
        """
        () => window.__sseTarget.dispatch('response', {
          thread_id: 'test-thread',
          content: 'healthy final answer',
          emotes: [],
        })
        """
    )

    # Then Done.
    await page.evaluate(
        """
        () => window.__sseTarget.dispatch('status', {
          thread_id: 'test-thread',
          message: 'Done',
        })
        """
    )

    # Past the grace window: no reload may have been scheduled.
    await page.wait_for_timeout(2000)
    calls = await page.evaluate("() => window.__loadHistoryCalls")
    assert calls == 0, f"healthy path must not reload history, got {calls} calls"

    await page.evaluate(
        "() => { window.loadHistory = window.__realLoadHistory; }"
    )
