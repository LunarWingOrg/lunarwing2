"""Browser regressions for Web UI hot paths."""


async def test_stream_chunks_stay_in_live_work_until_final(page):
    result = await page.evaluate(
        """
        () => {
          finalizeActivityGroup();
          resetStreamingState();
          _streamSnapshots.clear();
          const container = document.getElementById('chat-messages');
          container.replaceChildren();
          currentThreadId = 'perf-stream';

          const message = getOrCreateStreamingMessage(currentThreadId);
          _streamBuffer += 'first';
          flushStreamBuffer();
          _streamBuffer += ' second';
          flushStreamBuffer();

          const streaming = {
            raw: message.getAttribute('data-raw'),
            copy: message.getAttribute('data-copy-text'),
            text: currentStreamingText(),
          };
          finishStreamingWork(currentThreadId);
          return {
            streaming,
            finalRaw: message.getAttribute('data-raw'),
            finalCopy: message.getAttribute('data-copy-text'),
            finalText: message.querySelector('.activity-live-work-output').textContent,
            stillStreaming: message.hasAttribute('data-streaming'),
          };
        }
        """
    )

    assert result["streaming"] == {
        "raw": None,
        "copy": None,
        "text": "first second",
    }
    assert result["finalRaw"] is None
    assert result["finalCopy"] is None
    assert result["finalText"] == "first second"
    assert result["stillStreaming"] is False


async def test_stream_snapshot_keeps_unflushed_chunk_across_thread_switch(page):
    result = await page.evaluate(
        """
        () => {
          finalizeActivityGroup();
          resetStreamingState();
          _streamSnapshots.clear();
          const container = document.getElementById('chat-messages');
          container.replaceChildren();
          currentThreadId = 'thread-a';

          getOrCreateStreamingMessage(currentThreadId);
          _streamBuffer += 'rendered ';
          flushStreamBuffer();
          _streamBuffer += 'pending';
          stashStreamingState();

          currentThreadId = 'thread-b';
          container.replaceChildren();
          currentThreadId = 'thread-a';
          const restored = getOrCreateStreamingMessage(currentThreadId);
          return {
            text: restored.querySelector('.activity-live-work-output').textContent,
            streamText: currentStreamingText(),
            snapshotStillStored: _streamSnapshots.has(currentThreadId),
          };
        }
        """
    )

    assert result == {
        "text": "rendered pending",
        "streamText": "rendered pending",
        "snapshotStillStored": False,
    }


async def test_hidden_logs_defer_dom_work_until_tab_opens(page):
    result = await page.evaluate(
        """
        async () => {
          switchTab('chat');
          const output = document.getElementById('logs-output');
          output.replaceChildren();
          _deferredLogEntries = [];
          _pendingLogEntries = [];

          const entries = [
            {timestamp: '2026-07-28T12:00:00.000Z', level: 'INFO', target: 'perf-test', message: 'older'},
            {timestamp: '2026-07-28T12:00:01.000Z', level: 'INFO', target: 'perf-test', message: 'newer'},
          ];
          entries.forEach(queueLogEntry);
          const hiddenCount = output.querySelectorAll('[data-target="perf-test"]').length;

          switchTab('logs');
          await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
          const rendered = Array.from(output.querySelectorAll('[data-target="perf-test"]'))
            .map(element => element.querySelector('.log-msg').textContent);
          return {hiddenCount, rendered};
        }
        """
    )

    assert result["hiddenCount"] == 0
    assert result["rendered"] == ["newer", "older"]


async def test_stream_does_not_force_scroll_while_reading_history(page):
    result = await page.evaluate(
        """
        async () => {
          finalizeActivityGroup();
          resetStreamingState();
          const container = document.getElementById('chat-messages');
          container.replaceChildren();
          hasMore = false;

          for (let i = 0; i < 80; i++) {
            const row = document.createElement('div');
            row.style.height = '30px';
            row.style.flex = '0 0 30px';
            container.appendChild(row);
          }
          container.scrollTop = 0;
          container.dispatchEvent(new Event('scroll'));
          addMessage('system', 'new background content');
          await new Promise(resolve => requestAnimationFrame(resolve));
          const whileReading = container.scrollTop;

          container.scrollTop = container.scrollHeight;
          container.dispatchEvent(new Event('scroll'));
          addMessage('system', 'new pinned content');
          await new Promise(resolve => requestAnimationFrame(resolve));
          return {
            whileReading,
            distanceFromBottom: container.scrollHeight - container.scrollTop - container.clientHeight,
          };
        }
        """
    )

    assert result["whileReading"] == 0
    assert result["distanceFromBottom"] <= 1
