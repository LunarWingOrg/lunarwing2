"""Prompt editor browser regressions using the shipped UI and a routed API.

No daemon/LLM is needed here. The real prompt routes, persistence, auth and
scope isolation are covered by Rust's multi_tenant::prompt_handlers tests.
"""

import asyncio
import json
import mimetypes
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import pytest
from playwright.async_api import async_playwright, expect


STATIC = Path(__file__).resolve().parents[3] / "src/channels/web/static"
SOURCE_PATHS = [
    "AGENTS.md", "SOUL.md", "USER.md", "IDENTITY.md", "TOOLS.md", "MEMORY.md", "BOOTSTRAP.md",
]
HTML_TEXT = '<img src=x onerror="window.promptXss=true">'


@pytest.fixture(scope="session")
async def prompt_browser():
    async with async_playwright() as playwright:
        browser = await playwright.chromium.launch()
        yield browser
        await browser.close()


@pytest.fixture
async def prompt_ui(prompt_browser):
    context = await prompt_browser.new_context(viewport={"width": 1280, "height": 900})
    page = await context.new_page()
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    state = {
        "files": {
            path: {
                "path": path,
                "content": HTML_TEXT if path == "AGENTS.md" else "",
                "exists": path == "AGENTS.md",
                "updated_at": "2026-09-30T12:00:00Z" if path == "AGENTS.md" else None,
            }
            for path in SOURCE_PATHS
        },
        "writes": [],
        "write_status": 200,
        "list_status": 200,
        "preview_status": 200,
        "scope": "tenant-owner",
        "hold_preview": False,
        "preview_started": asyncio.Event(),
        "release_preview": asyncio.Event(),
        "preview_finished": asyncio.Event(),
    }

    async def route_request(route):
        url = urlparse(route.request.url)
        path = url.path
        data = {}
        status = 200
        if path.startswith("/api/prompts"):
            assert route.request.headers.get("authorization") == "Bearer prompt-test-token"
        if path == "/api/prompts":
            data = {"scope": state["scope"], "files": list(state["files"].values())}
            status = state["list_status"]
        elif path == "/api/prompts/write":
            body = route.request.post_data_json
            state["writes"].append(body)
            status = state["write_status"]
            if status == 200:
                state["files"][body["path"]].update(
                    content=body["content"], exists=True, updated_at="2026-09-30T13:00:00Z",
                )
                data = state["files"][body["path"]]
        elif path == "/api/prompts/preview":
            query = parse_qs(url.query)
            data = {
                "scope": state["scope"], "kind": "workspace",
                "timezone": query["timezone"][0],
                "is_group_chat": query["is_group_chat"][0] == "true",
                "prompt": "\n".join(file["content"] for file in state["files"].values())
                + "\nGroup=" + query["is_group_chat"][0],
            }
            status = state["preview_status"]
            if state["hold_preview"]:
                state["hold_preview"] = False
                state["preview_started"].set()
                await state["release_preview"].wait()
                await route.fulfill(status=status, json=data)
                state["preview_finished"].set()
                return
        elif path == "/api/chat/threads":
            data = {"threads": []}
        elif path == "/api/settings":
            data = {"settings": {}}
        elif path == "/api/gateway/status":
            data = {"status": "running"}
        elif path.endswith("/events"):
            await route.fulfill(content_type="text/event-stream", body=": connected\n\n")
            return
        elif not path.startswith("/api/"):
            asset = STATIC / (path.lstrip("/") or "index.html")
            if asset.is_file() and asset.resolve().is_relative_to(STATIC):
                await route.fulfill(path=asset, content_type=mimetypes.guess_type(str(asset))[0])
            else:
                await route.fulfill(status=404, body="Not found")
            return
        await route.fulfill(
            status=status, content_type="application/json",
            body=json.dumps(data) if status == 200 else "Fixture request rejected",
        )

    await page.route("http://prompt.test/**", route_request)
    await page.goto("http://prompt.test/")
    assert not errors, errors
    await page.locator("#token-input").fill("prompt-test-token")
    await page.locator("#auth-connect-btn").click()
    await expect(page.locator("#auth-screen")).to_be_hidden()
    await page.locator('[data-tab="settings"]').click()
    await page.locator('[data-settings-subtab="prompts"]').click()
    await expect(page.locator("#prompt-editor")).to_be_enabled()
    yield page, state
    await context.close()
    assert not errors, errors


async def test_prompt_edit_save_create_and_drafts_survive_navigation(prompt_ui):
    page, state = prompt_ui
    await expect(page.locator("#prompt-scope")).to_have_text("tenant-owner")
    await expect(page.locator("#prompt-editor")).to_have_value(HTML_TEXT)
    await expect(page.locator("#prompt-preview")).to_contain_text(HTML_TEXT)
    assert await page.locator("#prompt-preview img").count() == 0
    assert not await page.evaluate("window.promptXss === true")
    await page.locator("#prompt-editor").fill("Tenant instructions draft")
    await page.locator("#prompt-source").select_option("SOUL.md")
    await expect(page.locator("#prompt-source-info")).to_contain_text("Not yet stored")
    await page.locator("#prompt-editor").fill("Tenant values draft")
    await page.locator('[data-settings-subtab="agent"]').click()
    await page.locator('[data-settings-subtab="prompts"]').click()
    await expect(page.locator("#prompt-editor")).to_have_value("Tenant values draft")
    await page.locator("#prompt-save").click()
    await expect(page.locator("#prompt-status")).to_contain_text("Saved SOUL.md")
    await expect(page.locator("#prompt-preview")).to_contain_text("Tenant values draft")
    await expect(page.locator("#prompt-preview")).not_to_contain_text("Tenant instructions draft")
    await page.locator("#prompt-source").select_option("AGENTS.md")
    await expect(page.locator("#prompt-editor")).to_have_value("Tenant instructions draft")
    await page.locator("#prompt-save").click()
    await expect(page.locator("#prompt-save")).to_be_disabled()
    await expect(page.locator("#prompt-preview")).to_contain_text("Tenant instructions draft")
    await page.locator("#prompt-reload").click()
    await expect(page.locator("#prompt-editor")).to_have_value("Tenant instructions draft")
    assert state["writes"] == [
        {"path": "SOUL.md", "content": "Tenant values draft"},
        {"path": "AGENTS.md", "content": "Tenant instructions draft"},
    ]


async def test_prompt_save_error_keeps_draft_and_reload_requires_discard(prompt_ui):
    page, state = prompt_ui
    state["write_status"] = 422
    await page.locator("#prompt-editor").fill("Retain this unsaved edit")
    await page.locator("#prompt-save").click()
    await expect(page.locator("#prompt-status")).to_contain_text("draft retained")
    await expect(page.locator("#prompt-editor")).to_have_value("Retain this unsaved edit")
    await expect(page.locator("#prompt-save")).to_be_enabled()
    await page.locator("#prompt-reload").click()
    await expect(page.locator("#confirm-modal")).to_be_visible()
    await page.locator("#confirm-modal-cancel-btn").click()
    await expect(page.locator("#prompt-editor")).to_have_value("Retain this unsaved edit")
    await page.locator("#prompt-reload").click()
    await page.locator("#confirm-modal-btn").click()
    await expect(page.locator("#prompt-editor")).to_have_value(HTML_TEXT)
    assert state["files"]["AGENTS.md"]["content"] == HTML_TEXT


async def test_prompt_preview_error_and_out_of_order_responses(prompt_ui):
    page, state = prompt_ui
    state["preview_status"] = 400
    await page.locator("#prompt-preview-refresh").click()
    await expect(page.locator("#prompt-preview-status")).to_contain_text("Could not load preview")
    await expect(page.locator("#prompt-preview")).to_be_empty()
    state["preview_status"] = 200
    state["hold_preview"] = True
    await page.locator("#prompt-preview-refresh").click()
    await asyncio.wait_for(state["preview_started"].wait(), timeout=5)
    await page.locator("#prompt-group").check()
    await expect(page.locator("#prompt-preview")).to_contain_text("Group=true")
    state["release_preview"].set()
    await asyncio.wait_for(state["preview_finished"].wait(), timeout=5)
    await expect(page.locator("#prompt-preview")).to_contain_text("Group=true")
    await page.locator("#prompt-timezone").fill("Europe/Bucharest")
    await page.locator("#prompt-timezone").press("Tab")
    await expect(page.locator("#prompt-preview-status")).to_contain_text("Europe/Bucharest")


async def test_prompt_auth_expiry_clears_drafts_and_pending_preview(prompt_ui):
    page, state = prompt_ui
    await page.locator("#prompt-editor").fill("Private draft")
    state["hold_preview"] = True
    await page.locator("#prompt-preview-refresh").click()
    await asyncio.wait_for(state["preview_started"].wait(), timeout=5)
    state["list_status"] = 401
    await page.evaluate("apiFetch('/api/prompts').catch(() => {})")
    await expect(page.locator("#auth-screen")).to_be_visible()
    state["release_preview"].set()
    await asyncio.wait_for(state["preview_finished"].wait(), timeout=5)
    await expect(page.locator("#prompt-preview")).to_be_empty()
    await expect(page.locator("#prompt-editor")).to_have_value("")
    state["scope"] = "another-owner"
    state["list_status"] = 200
    state["files"]["AGENTS.md"]["content"] = "Another owner's instructions"
    await page.locator("#token-input").press("Enter")
    await expect(page.locator("#auth-screen")).to_be_hidden()
    await page.locator('[data-settings-subtab="prompts"]').click()
    await expect(page.locator("#prompt-editor")).to_have_value("Another owner's instructions")
    await expect(page.locator("#prompt-scope")).to_have_text("another-owner")


async def test_prompt_loading_error_can_be_retried(prompt_ui):
    page, state = prompt_ui
    state["list_status"] = 503
    await page.locator("#prompt-reload").click()
    await expect(page.locator("#prompt-status")).to_contain_text("Could not load prompt files")
    await expect(page.locator("#prompt-editor")).to_be_disabled()
    state["list_status"] = 200
    await page.locator("#prompt-reload").click()
    await expect(page.locator("#prompt-editor")).to_have_value(HTML_TEXT)
    await expect(page.locator("#prompt-editor")).to_be_enabled()
