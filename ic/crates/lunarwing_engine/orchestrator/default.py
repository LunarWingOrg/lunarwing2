# Engine v2 Orchestrator (default, v0)
#
# This is the self-modifiable execution loop. It replaces the Rust
# ExecutionLoop::run() with Python that can be patched at runtime
# by the self-improvement Mission.
#
# Host functions (provided by Rust via Monty suspension):
#   __llm_complete__(messages, actions, config)  -> response dict
#   __execute_code_step__(code, state)           -> result dict
#   __execute_action__(name, params)             -> result dict
#   __execute_actions_parallel__(calls)          -> list of result dicts (parallel execution)
#   __check_signals__()                          -> None | "stop" | {"inject": msg}
#   __emit_event__(kind, **data)                 -> None
#   __save_checkpoint__(state, counters)         -> None
#   __transition_to__(state, reason)             -> None
#   __retrieve_docs__(goal, max_docs)            -> list of doc dicts
#   __check_budget__()                           -> budget dict
#   __get_actions__()                            -> list of action dicts
#
# Context variables (injected by Rust before execution):
#   context  - list of prior messages [{role, content}]
#   goal     - thread goal string
#   actions  - list of available action defs
#   state    - persisted state dict from prior steps
#   config   - thread config dict


# Conservative context limits. Structured action results use same cap as
# CodeAct output so one tool cannot poison persisted inference history.
CHARS_PER_TOKEN = 4
MESSAGE_OVERHEAD_CHARS = 4
MAX_ACTION_OUTPUT_CHARS = 8000
RECOVERY_SCHEMA_VERSION = 1
RECOVERY_STATE_KEY = "_failure_recovery"
NO_PROGRESS_FAILURE_THRESHOLD = 3
MAX_RECOVERY_ITERATIONS = 3
RECOVERY_CONTEXT_MAX_CHARS = 24000

# Per-kind engine recovery policy. max_attempts caps engine-visible recovery
# retries for that failure kind (further capped by the thread config's
# model_recovery_max_attempts). shrink (context_overflow only) scales the
# recovery context budget per attempt. Kinds absent here — and unknown kinds —
# get no engine recovery attempt: authentication/request_rejected fail fast by
# design, and rate_limited is owned by the provider layer's Retry-After
# handling, so the engine must not re-hammer.
MODEL_RECOVERY_POLICY = {
    "context_overflow": {"max_attempts": 2, "shrink": [0.60, 0.35]},
    "transient": {"max_attempts": 2},
    "invalid_output": {"max_attempts": 1},
    "rate_limited": {"max_attempts": 0},
    "unavailable": {"max_attempts": 1},
    "authentication": {"max_attempts": 0},
    "request_rejected": {"max_attempts": 0},
    "internal": {"max_attempts": 1},
}


# ── Helper functions (self-modifiable glue) ──────────────────
# Defined before run_loop so they are in scope when called.


def extract_final(text):
    """Extract FINAL() content from text. Returns None if not found."""
    idx = text.find("FINAL(")
    if idx < 0:
        return None
    after = text[idx + 6:]
    # Handle triple-quoted strings
    for q in ['"""', "'''"]:
        if after.startswith(q):
            end = after.find(q, len(q))
            if end >= 0:
                return after[len(q):end]
    # Handle single/double quoted strings
    if after and after[0] in ('"', "'"):
        quote = after[0]
        end = after.find(quote, 1)
        if end >= 0:
            return after[1:end]
    # Handle balanced parens
    depth = 1
    for i, ch in enumerate(after):
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return after[:i]
    return None


def strip_quoted_strings(line):
    """Remove double-quoted string literals from a line."""
    result = []
    in_quote = False
    prev = ""
    for ch in line:
        if ch == '"' and prev != "\\":
            in_quote = not in_quote
            prev = ch
            continue
        if not in_quote:
            result.append(ch)
        prev = ch
    return "".join(result)


def strip_code_blocks(text):
    """Strip fenced code blocks, indented code lines, and double-quoted strings."""
    result = []
    in_fence = False
    for line in text.split("\n"):
        trimmed = line.lstrip()
        if trimmed.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if line.startswith("    ") or line.startswith("\t"):
            continue
        result.append(strip_quoted_strings(line))
    return "\n".join(result)


def signals_tool_intent(text):
    """Detect when text expresses intent to call a tool without actually doing so.

    Ported from V1 Rust llm_signals_tool_intent(): strips code blocks and
    quoted strings, checks exclusion phrases, then requires a future-tense
    prefix ("let me", "I'll", "I will", "I'm going to") immediately followed
    by an action verb ("search", "fetch", "check", etc.).
    """
    stripped = strip_code_blocks(text)
    lower = stripped.lower()

    EXCLUSIONS = [
        "let me explain", "let me know", "let me think",
        "let me summarize", "let me clarify", "let me describe",
        "let me help", "let me understand", "let me break",
        "let me outline", "let me walk you", "let me provide",
        "let me suggest", "let me elaborate", "let me start by",
    ]
    for exc in EXCLUSIONS:
        if exc in lower:
            return False

    PREFIXES = ["let me ", "i'll ", "i will ", "i'm going to "]
    ACTION_VERBS = [
        "search", "look up", "check", "fetch", "find",
        "read the", "write the", "create", "run the", "execute",
        "query", "retrieve", "add it", "add the", "add this",
        "add that", "update the", "delete", "remove the", "look into",
    ]

    for prefix in PREFIXES:
        start = 0
        while True:
            i = lower.find(prefix, start)
            if i < 0:
                break
            after = lower[i + len(prefix):]
            for verb in ACTION_VERBS:
                if after.startswith(verb) or (" " + verb) in after.split("\n")[0]:
                    return True
            start = i + 1

    return False


def signals_execution_intent(text):
    """Detect explicit execution requests in user-authored text."""
    stripped = strip_code_blocks(text)
    lower = stripped.lower()

    exec_phrases = [
        "run it", "run that", "run them", "run this", "run the ",
        "execute it", "execute that", "execute them", "execute this",
        "execute the ",
        "ship it", "deploy it", "deploy that", "deploy this", "deploy the ",
        "send it", "send that", "send the ",
        "fetch it", "fetch that", "fetch the ",
        "please run ", "please execute ", "please fetch ",
        "please send ", "please deploy ",
    ]
    return any(phrase in lower for phrase in exec_phrases)


def strip_stage_directions(text):
    """Remove paired *roleplay directions* before classifying prose."""
    parts = []
    cursor = 0
    while cursor < len(text):
        start = text.find("*", cursor)
        if start < 0:
            parts.append(text[cursor:])
            break
        end = text.find("*", start + 1)
        if end < 0:
            parts.append(text[cursor:])
            break
        parts.append(text[cursor:start])
        cursor = end + 1
    return "".join(parts).strip()


def is_simple_goal(goal):
    """Return whether a short goal is conversational rather than task-oriented."""
    goal_lower = goal.lower().strip()
    if len(goal_lower) >= 60:
        return False

    normalized = goal_lower
    for punctuation in [".", ",", "!", "?", ";", ":"]:
        normalized = normalized.replace(punctuation, " ")
    normalized = " ".join(normalized.split())
    if normalized in ["restarted", "restart complete", "rebooted", "back online"]:
        return True

    padded = " " + normalized + " "
    simple_patterns = [
        "standby", "thanks", "thank you", "hello", "got it",
        "understood", "ok", "okay", "ready", "proceed",
    ]
    for pattern in simple_patterns:
        if " " + pattern + " " in padded:
            return True
    return False


def signals_leaked_roleplay_or_meta(text):
    """Detect strong roleplay or retry/meta leakage in a text response."""
    cleaned = strip_stage_directions(text)
    lower = cleaned.lower()
    meta_patterns = [
        "oops", "let me fix", "let me try again", "i need to retry",
        "i should retry", "my mistake", "i made a mistake",
    ]
    normalized = lower
    for punctuation in [".", ",", "!", "?", ";", ":", "-"]:
        normalized = normalized.replace(punctuation, " ")
    padded = " " + " ".join(normalized.split()) + " "
    for pattern in meta_patterns:
        if " " + pattern + " " in padded:
            return True

    if cleaned:
        return False

    lower_text = text.lower()
    roleplay_patterns = [
        "chortle", "flicks tail", "tail flick", "smirks", "grins",
        "nods", "sighs", "laughs", "giggles", "purrs", "growls",
    ]
    return any(pattern in lower_text for pattern in roleplay_patterns)


def format_output(result, max_chars=8000):
    """Format code execution result for the next LLM context message."""
    parts = []

    stdout = result.get("stdout", "")
    if stdout:
        parts.append("[stdout]\n" + stdout)

    for r in result.get("action_results", []):
        name = r.get("action_name", "?")
        output = str(r.get("output", ""))
        if r.get("is_error"):
            parts.append("[" + name + " ERROR] " + output)
        else:
            preview = output[:500] + "..." if len(output) > 500 else output
            parts.append("[" + name + "] " + preview)

    ret = result.get("return_value")
    if ret is not None:
        parts.append("[return] " + str(ret))

    text = "\n\n".join(parts)

    # CodeAct step output is untrusted: nested-VM stdout, action previews,
    # and return values can all carry attacker-influenced bytes. Break every
    # ChatML-family token opener before bounding (mirrors format_action_output)
    # so re-tokenizing history cannot forge a role boundary, and so truncation
    # can never cut an intact opener back out of a neutralized marker.
    text = neutralize_role_token_openers(text)

    # Truncate from the front (keep the tail with most recent results)
    if len(text) > max_chars:
        text = "... (truncated) ...\n" + text[-max_chars:]

    if not text:
        text = "[code executed, no output]"

    return text


def neutralize_role_token_openers(content):
    """Break ChatML-family token openers in untrusted prompt content."""
    # This is idempotent and deliberately independent of optional injection
    # scanning so every prompt-construction boundary can share the invariant.
    return str(content).replace("<|", "<\u200b|")


def format_action_output(output, max_chars=MAX_ACTION_OUTPUT_CHARS):
    """Neutralize role tokens and bound output before storing LLM history."""
    # Action output is untrusted even when it retains a structured host shape.
    # Break every ChatML-family token opener before persistence so re-tokenizing
    # history cannot turn tool data into a forged role boundary.
    text = neutralize_role_token_openers(output)
    marker = "\n... (tool output truncated)"
    if len(text) <= max_chars:
        return text
    if max_chars <= len(marker):
        return marker[:max_chars]
    return text[:max_chars - len(marker)] + marker


def sanitize_action_results(messages):
    """Repair unsafe or oversized messages in persisted state."""
    for msg in messages:
        role = msg.get("role")
        if role in ("ActionResult", "action_result"):
            msg["content"] = format_action_output(msg.get("content", ""))
        elif role in ("User", "user"):
            # Repair CodeAct step blobs ("[stdout]", "[name]", "[return]")
            # persisted before format_output neutralized role tokens. No
            # truncation here: User blobs are already bounded at creation,
            # and neutralization is idempotent for fresh history.
            msg["content"] = neutralize_role_token_openers(msg.get("content", ""))
    return messages


def is_gate_result(result):
    """Return whether a structured host result interrupts for a gate."""
    if not isinstance(result, dict):
        return False
    if result.get("gate_paused") == True:
        return True
    if result.get("need_authentication") == True:
        return True
    if result.get("need_approval") == True:
        return True
    return False


def append_structured_action_results(messages, calls, results):
    """Record one result per call except the first pending gate."""
    gate_seen = False
    for idx in range(len(calls)):
        call = calls[idx] if isinstance(calls[idx], dict) else {}
        result = results[idx] if idx < len(results) else None
        gate_result = is_gate_result(result)
        if gate_result and not gate_seen:
            gate_seen = True
            continue

        action_name = call.get("name", "")
        if gate_result:
            action_name = result.get("action_name", action_name)
            resume_output = result.get("resume_output")
            if resume_output is None:
                output = {
                    "error": "tool was not executed because another call paused the batch"
                }
            else:
                output = {
                    "status": "tool executed; additional gate deferred",
                    "output": resume_output,
                }
        elif isinstance(result, dict):
            action_name = result.get("action_name", action_name)
            if "output" in result:
                output = result["output"]
            else:
                output = {"error": "tool result missing from host response"}
        elif gate_seen:
            output = {"error": "tool was not executed because another call paused the batch"}
        else:
            output = {"error": "tool result missing from host response"}

        messages.append({
            "role": "ActionResult",
            "content": format_action_output(str(output)),
            "action_name": action_name,
            "action_call_id": call.get("call_id", ""),
        })
    return messages


def format_docs(docs):
    """Format memory docs for context injection."""
    parts = ["## Prior Knowledge (from completed threads)\n"]
    for doc in docs:
        label = doc.get("type", "NOTE").upper()
        content = doc.get("content", "")[:500]
        truncated = "..." if len(doc.get("content", "")) > 500 else ""
        parts.append("### [" + label + "] " + doc.get("title", "") +
                      "\n" + content + truncated + "\n")
    # Neutralize the complete section so title, body, type, and future fields
    # cannot forge a system-message boundary when the prompt is re-tokenized.
    return neutralize_role_token_openers("\n".join(parts))


def estimate_context_tokens(messages):
    """Estimate token count for a transcript using a rough chars/token heuristic."""
    total_chars = 0
    for msg in messages:
        total_chars += len(msg.get("content", ""))
        total_chars += len(msg.get("action_name", "") or "")
        total_chars += MESSAGE_OVERHEAD_CHARS
    return (total_chars + CHARS_PER_TOKEN - 1) // CHARS_PER_TOKEN


def compact_if_needed(state, config):
    """Compact thread context when the active message history grows too large.

    The orchestrator owns compaction policy. Rust only provides helpers for
    token estimation, explicit LLM calls, and replacing the active message
    scaffold after a summary has been produced.
    """
    if not config.get("enable_compaction", False):
        return False
    recovery = state.get(RECOVERY_STATE_KEY)
    if (isinstance(recovery, dict)
            and recovery.get("pending_model_error") is not None):
        return False

    context_limit = config.get("model_context_limit", 128000)
    threshold_pct = config.get("compaction_threshold", 0.85)
    threshold = int(context_limit * threshold_pct)
    working_messages = state.get("working_messages")
    if not isinstance(working_messages, list) or not working_messages:
        return False

    current_tokens = estimate_context_tokens(working_messages)
    if current_tokens < threshold:
        return False

    snapshot = list(working_messages)

    history = state.get("history")
    if not isinstance(history, list):
        history = []
        state["history"] = history

    compaction_count = state.get("compaction_count", 0) + 1

    summary_prompt = (
        "Summarize progress so far in a concise but complete way.\n"
        "Include:\n"
        "1. What has been accomplished\n"
        "2. Key intermediate results, facts, and variable values\n"
        "3. Tool results or findings worth preserving\n"
        "4. What still needs to be done\n"
        "5. Errors encountered and how they were handled\n\n"
        "Preserve all information needed to continue the task."
    )
    summary_messages = list(snapshot)
    summary_messages.append({"role": "User", "content": summary_prompt})
    summary_resp = __llm_complete__(summary_messages, None, {"force_text": True})
    if summary_resp.get("type") == "model_error":
        return summary_resp

    history.append({
        "kind": "compaction",
        "index": compaction_count,
        "tokens_before": current_tokens,
        "messages": snapshot,
    })

    summary_text = summary_resp.get("content", "")
    if not summary_text:
        summary_text = "[compaction produced no summary]"

    state["working_messages"] = []
    system_message = None
    for msg in snapshot:
        if msg.get("role") == "System":
            system_message = {"role": "System", "content": msg.get("content", "")}
            break
    if system_message is not None:
        state["working_messages"].append(system_message)
    append_message(state["working_messages"], "Assistant", summary_text)
    append_message(
        state["working_messages"],
        "User",
        "Your conversation has been compacted. The summary above captures prior progress. "
        "Older details remain available through state['history'] and project retrieval. Continue working on the task.",
    )
    state["compaction_count"] = compaction_count
    return True


# ── Skill selection and injection (self-modifiable) ────────


def score_skill(skill, message_lower):
    """Score a skill against a user message. Returns 0 if vetoed."""
    meta = skill.get("metadata", {})
    activation = meta.get("activation", {})

    # Exclude keyword veto
    for excl in activation.get("exclude_keywords", []):
        if excl.lower() in message_lower:
            return 0

    score = 0

    # Keyword scoring: exact word = 10, substring = 5 (cap 30)
    kw_score = 0
    words = message_lower.split()
    for kw in activation.get("keywords", []):
        kw_lower = kw.lower()
        if kw_lower in words:
            kw_score += 10
        elif kw_lower in message_lower:
            kw_score += 5
    score += min(kw_score, 30)

    # Tag scoring: substring = 3 (cap 15)
    tag_score = 0
    for tag in activation.get("tags", []):
        if tag.lower() in message_lower:
            tag_score += 3
    score += min(tag_score, 15)

    # Confidence factor for extracted skills
    source = meta.get("source", "authored")
    if source == "extracted":
        metrics = meta.get("metrics", {})
        total = metrics.get("success_count", 0) + metrics.get("failure_count", 0)
        confidence = metrics.get("success_count", 0) / total if total > 0 else 1.0
        factor = 0.5 + 0.5 * max(0.0, min(1.0, confidence))
        score = int(score * factor)

    return score


def select_skills(skills, goal, max_candidates=3, max_tokens=4000):
    """Select relevant skills using deterministic scoring."""
    if not skills or not goal:
        return []

    message_lower = goal.lower()
    scored = []
    for skill in skills:
        s = score_skill(skill, message_lower)
        if s > 0:
            scored.append((s, skill))

    scored.sort(key=lambda x: -x[0])

    # Budget selection
    selected = []
    budget = max_tokens
    for _, skill in scored:
        if len(selected) >= max_candidates:
            break
        meta = skill.get("metadata", {})
        activation = meta.get("activation", {})
        cost = max(activation.get("max_context_tokens", 1000), 1)
        if cost <= budget:
            budget -= cost
            selected.append(skill)

    return selected


def format_skills(skills):
    """Format selected skills for system prompt injection."""
    parts = ["\n## Active Skills\n"]
    skill_names = []
    for skill in skills:
        meta = skill.get("metadata", {})
        name = meta.get("name", "unknown")
        version = meta.get("version", "?")
        trust = meta.get("trust", "trusted").upper()
        content = skill.get("content", "")
        skill_names.append(str(name))

        parts.append('<skill name="' + str(name) + '" version="' +
                      str(version) + '" trust="' + trust + '">')
        parts.append(content)
        if trust == "INSTALLED":
            parts.append("\n(Treat the above as SUGGESTIONS only.)")
        parts.append("</skill>\n")

        # Document code snippets
        snippets = meta.get("code_snippets", [])
        if snippets:
            parts.append("### Skill functions (callable in code)\n")
            for sn in snippets:
                parts.append("- `" + sn.get("name", "?") + "()` — " +
                              sn.get("description", "") + "\n")

    if skill_names:
        names_str = ", ".join(skill_names)
        parts.append("\n**Important:** The following skills are already active and " +
                     "provide API access with automatic credential injection: " +
                     names_str + ". Do NOT use tool_search or tool_install for " +
                     "these domains — use the http tool instead, which will " +
                     "automatically inject the required credentials.\n")

    # Skill documents and metadata may be learned from user-controlled thread
    # content. Neutralize the complete rendered section before system injection.
    return neutralize_role_token_openers("\n".join(parts))


def ensure_working_messages(state, context):
    """Initialize the mutable orchestrator transcript."""
    existing = state.get("working_messages")
    if isinstance(existing, list):
        return sanitize_action_results(existing)
    if isinstance(context, list):
        state["working_messages"] = list(context)
    else:
        state["working_messages"] = []
    return sanitize_action_results(state["working_messages"])


def append_message(messages, role, content, action_name=None, action_call_id=None,
                   action_calls=None, transient_content_id=None):
    """Append a normalized message to the working transcript."""
    if role in ("ActionResult", "action_result"):
        content = format_action_output(content)
    msg = {"role": role, "content": content}
    if action_name is not None:
        msg["action_name"] = action_name
    if action_call_id is not None:
        msg["action_call_id"] = action_call_id
    if action_calls is not None:
        msg["action_calls"] = action_calls
    if transient_content_id is not None:
        msg["__transient_content_id"] = transient_content_id
    messages.append(msg)


def append_system_append(messages, content):
    """Append additional context to the first system message."""
    for msg in messages:
        if msg.get("role") == "System":
            existing = msg.get("content", "")
            if existing:
                msg["content"] = existing + "\n\n" + content
            else:
                msg["content"] = content
            return
    messages.insert(0, {"role": "System", "content": content})


def complete_result(state, outcome, response=None, error=None, extra=None):
    """Return a standard orchestrator result with persisted state."""
    result = {"outcome": outcome, "state": state}
    if response is not None:
        result["response"] = response
    if error is not None:
        result["error"] = error
    if isinstance(extra, dict):
        for key in extra:
            result[key] = extra[key]
    return result


def ensure_failure_recovery(state):
    """Load bounded recovery accounting, replacing malformed state safely."""
    recovery = state.get(RECOVERY_STATE_KEY)
    if (not isinstance(recovery, dict)
            or recovery.get("schema_version") != RECOVERY_SCHEMA_VERSION):
        recovery = {
            "schema_version": RECOVERY_SCHEMA_VERSION,
            "terminal_attempted": [],
            "pending_terminal": None,
            "active_terminal": None,
            "model_recovery_attempts": 0,
            "model_recovery_total_attempts": 0,
            "pending_model_error": None,
            "consecutive_failed_work": 0,
        }
        state[RECOVERY_STATE_KEY] = recovery
        return recovery

    if not isinstance(recovery.get("terminal_attempted"), list):
        recovery["terminal_attempted"] = []
    if not isinstance(recovery.get("consecutive_failed_work"), int):
        recovery["consecutive_failed_work"] = 0
    # Migrate the legacy one-shot boolean to the checkpointed counter.
    legacy_attempted = recovery.pop("model_recovery_attempted", None)
    if not isinstance(recovery.get("model_recovery_attempts"), int):
        recovery["model_recovery_attempts"] = 1 if legacy_attempted else 0
    if not isinstance(recovery.get("model_recovery_total_attempts"), int):
        recovery["model_recovery_total_attempts"] = 0
    return recovery


def schedule_terminal_warning(state, kind, limit=None, repeated_failures=None):
    """Schedule one checkpointed warning per terminal class."""
    recovery = ensure_failure_recovery(state)
    if (recovery.get("pending_terminal") is not None
            or recovery.get("active_terminal") is not None
            or kind in recovery["terminal_attempted"]):
        return False
    recovery["terminal_attempted"].append(kind)
    recovery["pending_terminal"] = {
        "kind": kind,
        "limit": limit,
        "repeated_failures": repeated_failures,
    }
    return True


def terminal_warning_instruction(observation):
    """Render only typed, bounded terminal facts into model control text."""
    kind = observation.get("kind") if isinstance(observation, dict) else ""
    if kind == "iteration_limit":
        limit = observation.get("limit", 0)
        return (
            "Loop warning: iteration limit " + str(limit) +
            " reached. This is the final recovery iteration. Complete the task "
            "or provide the best final answer now."
        )
    if kind == "no_progress":
        count = observation.get("repeated_failures", NO_PROGRESS_FAILURE_THRESHOLD)
        return (
            "Loop warning: no progress after " + str(count) +
            " consecutive failed tool or code steps. Change approach, use a "
            "different allowed capability or parameters, or provide the best "
            "final answer now."
        )
    if kind == "consecutive_errors":
        count = observation.get("repeated_failures", 1)
        return (
            "Loop warning: " + str(count) +
            " consecutive code steps failed. This is the final recovery "
            "iteration. Change approach or provide the best final answer now."
        )
    return "Loop warning: finish with the best available answer now."


def messages_with_recovery_controls(state, working_messages):
    """Build one request with ephemeral recovery controls."""
    recovery = ensure_failure_recovery(state)
    controls = []
    model_error = recovery.get("pending_model_error")
    if isinstance(model_error, dict):
        instruction = model_error.get("instruction", "")
        if isinstance(instruction, str) and instruction:
            controls.append(instruction)
    terminal = recovery.get("pending_terminal")
    if isinstance(terminal, dict):
        controls.append(terminal_warning_instruction(terminal))
    if not controls:
        return working_messages

    control_text = "\n\n".join(controls)
    messages = []
    injected = False
    for msg in working_messages:
        if not injected and msg.get("role") in ("System", "system"):
            content = msg.get("content", "")
            messages.append({
                "role": "System",
                "content": content + "\n\n" + control_text,
            })
            injected = True
        else:
            messages.append(msg)
    if not injected:
        messages.insert(0, {"role": "System", "content": control_text})
    return messages


def activate_delivered_recovery(state):
    """Consume controls only after a successful model response."""
    recovery = ensure_failure_recovery(state)
    changed = False
    if recovery.get("pending_model_error") is not None:
        recovery["pending_model_error"] = None
        recovery["model_recovery_attempts"] = 0
        changed = True
    terminal = recovery.get("pending_terminal")
    if isinstance(terminal, dict):
        recovery["active_terminal"] = terminal.get("kind")
        recovery["pending_terminal"] = None
        changed = True
    return changed


def shrink_context_for_model_recovery(state, goal, budget_factor=1.0):
    """Replace oversized protocol history with a bounded plain-text view."""
    if not isinstance(budget_factor, (int, float)) or budget_factor <= 0:
        budget_factor = 1.0
    messages = state.get("working_messages", [])
    system_message = None
    recent = []
    remaining = int(RECOVERY_CONTEXT_MAX_CHARS * budget_factor)
    goal_cap = int(8000 * budget_factor)
    message_cap = int(2000 * budget_factor)

    for msg in messages:
        if msg.get("role") in ("System", "system"):
            system_message = {
                "role": "System",
                "content": msg.get("content", ""),
            }
            break

    goal_text = str(goal)
    if len(goal_text) > goal_cap:
        goal_text = goal_text[:goal_cap] + "\n... (goal truncated for recovery)"
    remaining -= len(goal_text)

    for msg in reversed(messages):
        role = msg.get("role", "Message")
        if role in ("System", "system"):
            continue
        content = str(msg.get("content", ""))
        if not content:
            continue
        if len(content) > message_cap:
            content = content[:message_cap] + "\n... (message truncated for recovery)"
        line = str(role) + ": " + content
        if len(line) > remaining:
            continue
        recent.append(line)
        remaining -= len(line)
        if remaining <= 0:
            break
    recent.reverse()

    reduced = []
    if system_message is not None:
        reduced.append(system_message)
    reduced.append({
        "role": "User",
        "content": (
            "Context was reduced after provider context overflow.\n"
            "Original goal:\n" + goal_text + "\n\nRecent bounded context:\n" +
            "\n\n".join(recent)
        ),
    })
    state["working_messages"] = reduced


def model_failure_result(state, response):
    """Create typed terminal failure without exposing provider diagnostics."""
    kind = response.get("kind", "internal")
    if not isinstance(kind, str) or not kind:
        kind = "internal"
    message = response.get("terminal_message", "The model gateway failed. Retry later.")
    if not isinstance(message, str) or not message:
        message = "The model gateway failed. Retry later."
    __transition_to__("failed", "model failure: " + kind)
    return complete_result(
        state,
        "failed",
        error=message,
        extra={"failure_kind": "model_" + kind},
    )


def terminal_recovery_result(state, kind):
    """Finalize a terminal condition after its warning turn was consumed."""
    if kind == "iteration_limit":
        __transition_to__("failed", "iteration limit reached after recovery")
        return complete_result(state, "max_iterations")
    if kind == "consecutive_errors":
        __transition_to__("failed", "consecutive code errors after recovery")
        return complete_result(
            state,
            "failed",
            error=(
                "Code execution kept failing after a final recovery attempt. "
                "Change approach, parameters, or required capabilities, then retry."
            ),
            extra={"failure_kind": "consecutive_errors"},
        )
    __transition_to__("failed", "no progress after recovery")
    return complete_result(
        state,
        "failed",
        error=(
            "Execution made no progress after a final recovery attempt. Change "
            "approach, enable the required capability, or retry with clearer constraints."
        ),
        extra={"failure_kind": "no_progress"},
    )


def observe_work_progress(state, failed):
    """Track only consecutive failed work; successful work resets detection."""
    recovery = ensure_failure_recovery(state)
    active = recovery.get("active_terminal")
    if active == "iteration_limit":
        recovery["active_terminal"] = None
        return "iteration_limit"
    if active in ("no_progress", "consecutive_errors"):
        recovery["active_terminal"] = None
        recovery["consecutive_failed_work"] = 0
        return active if failed else None

    if not failed:
        recovery["consecutive_failed_work"] = 0
        return None

    count = recovery.get("consecutive_failed_work", 0) + 1
    recovery["consecutive_failed_work"] = count
    if count < NO_PROGRESS_FAILURE_THRESHOLD:
        return None

    recovery["consecutive_failed_work"] = 0
    if schedule_terminal_warning(
            state,
            "no_progress",
            repeated_failures=count):
        return "warning_scheduled"
    return "no_progress"


def code_step_failed(result):
    """Count a code step failed only when it produced no successful work."""
    if not result.get("had_error", False):
        return False
    for action_result in result.get("action_results", []):
        if not action_result.get("is_error", False):
            return False
    return result.get("return_value") is None


def action_batch_failed(calls, results):
    """Count a structured batch failed only when every requested action failed."""
    if not calls:
        return True
    for result in results:
        if result is not None and not result.get("is_error", False):
            return False
    return True


def mark_gate_pause(state):
    """Keep warning accounting across gate resume without replaying warning text."""
    ensure_failure_recovery(state)["gate_paused"] = True


# ── Main execution loop ─────────────────────────────────────


def run_loop(context, goal, actions, state, config):
    """Main execution loop. Returns an outcome dict."""
    max_iterations = config.get("max_iterations", 30)
    max_nudges = config.get("max_tool_intent_nudges", 2)
    nudge_enabled = config.get("enable_tool_intent_nudge", True)
    max_consecutive_errors = config.get("max_consecutive_errors", 5)
    if max_consecutive_errors is None:
        max_consecutive_errors = 5
    obligation_enabled = config.get("require_action_attempt", False)
    max_obligation_nudges = config.get("max_action_requirement_nudges", 2)
    consecutive_nudges = 0
    consecutive_errors = 0
    step_count = config.get("step_count", 0)
    if not isinstance(state, dict):
        state = {}
    state.setdefault("history", [])
    state.setdefault("compaction_count", 0)
    state.setdefault("_obligation_resolved", False)
    state.setdefault("_obligation_nudge_count", 0)
    recovery = ensure_failure_recovery(state)
    if recovery.get("gate_paused", False):
        recovery["gate_paused"] = False
        recovery["active_terminal"] = None
    if not obligation_enabled and context:
        for msg in reversed(context):
            if msg.get("role") in ("User", "user"):
                if signals_execution_intent(msg.get("content", "")):
                    obligation_enabled = True
                    state["_obligation_resolved"] = False
                    state["_obligation_nudge_count"] = 0
                break
    working_messages = ensure_working_messages(state, context)

    hard_iteration_limit = max_iterations + MAX_RECOVERY_ITERATIONS
    for step in range(step_count, hard_iteration_limit):
        # 1. Check signals
        signal = __check_signals__()
        if signal == "stop":
            __transition_to__("completed", "stopped by signal")
            return complete_result(state, "stopped")
        if signal and isinstance(signal, dict) and "inject" in signal:
            injected_text = signal["inject"]
            append_message(
                working_messages,
                "User",
                injected_text,
                transient_content_id=signal.get("__transient_content_id"),
            )
            if signals_execution_intent(injected_text):
                obligation_enabled = True
                state["_obligation_resolved"] = False
                state["_obligation_nudge_count"] = 0

        recovery = ensure_failure_recovery(state)
        if recovery.get("active_terminal") is not None:
            return terminal_recovery_result(state, recovery["active_terminal"])
        if step >= max_iterations and recovery.get("pending_terminal") is None:
            if schedule_terminal_warning(
                    state,
                    "iteration_limit",
                    limit=max_iterations):
                __save_checkpoint__(state, {
                    "nudge_count": consecutive_nudges,
                    "consecutive_errors": consecutive_errors,
                    "compaction_count": state.get("compaction_count", 0),
                })
            else:
                return terminal_recovery_result(state, "iteration_limit")

        # 2. Check budget
        budget = __check_budget__()
        if budget.get("tokens_remaining", 1) <= 0:
            __transition_to__("completed", "token budget exhausted")
            return complete_result(state, "completed", "Token budget exhausted.")
        if budget.get("time_remaining_ms", 1) <= 0:
            __transition_to__("completed", "time budget exhausted")
            return complete_result(state, "completed", "Time budget exhausted.")
        if budget.get("usd_remaining") is not None and budget["usd_remaining"] <= 0:
            __transition_to__("completed", "cost budget exhausted")
            return complete_result(state, "completed", "Cost budget exhausted.")

        # 3. Inject prior knowledge and activate skills on first step
        if step == 0:
            docs = __retrieve_docs__(goal, 5)
            if docs:
                knowledge = format_docs(docs)
                append_system_append(working_messages, knowledge)

            # Select and inject skills based on goal keywords
            all_skills = __list_skills__()
            active_skills = select_skills(all_skills, goal, max_candidates=3, max_tokens=4000)
            if active_skills:
                skill_text = format_skills(active_skills)
                append_system_append(working_messages, skill_text)
                # Emit skill activation event for CLI/gateway display
                skill_names = ",".join(s.get("metadata", {}).get("name", "?") for s in active_skills)
                skill_doc_ids = ",".join(s.get("doc_id", "") for s in active_skills if s.get("doc_id", ""))
                __emit_event__("skill_activated", skill_names=skill_names, skill_doc_ids=skill_doc_ids)
                # Store active skill IDs in state for tracking
                state["active_skill_ids"] = [s.get("doc_id", "") for s in active_skills]
                state["skill_snippet_names"] = []
                for s in active_skills:
                    for sn in s.get("metadata", {}).get("code_snippets", []):
                        state["skill_snippet_names"].append(sn.get("name", ""))

        # 3.5 Compact context before the next model call when needed.
        compaction_result = compact_if_needed(state, config)
        working_messages = ensure_working_messages(state, context)

        # 4. Call LLM
        __emit_event__("step_started", step=step)
        if isinstance(compaction_result, dict):
            response = compaction_result
        else:
            request_messages = messages_with_recovery_controls(state, working_messages)
            response = __llm_complete__(request_messages, actions, None)
        __emit_event__("step_completed", step=step,
                       input_tokens=response.get("usage", {}).get("input_tokens", 0),
                       output_tokens=response.get("usage", {}).get("output_tokens", 0))

        # 5. Handle response based on type
        resp_type = response.get("type", "text")

        if resp_type == "model_error":
            recovery = ensure_failure_recovery(state)
            kind = response.get("kind", "internal")
            if not isinstance(kind, str) or not kind:
                kind = "internal"
            policy = MODEL_RECOVERY_POLICY.get(kind, {"max_attempts": 0})
            max_attempts = min(policy.get("max_attempts", 0),
                               config.get("model_recovery_max_attempts", 1))
            total_cap = config.get("model_recovery_total_attempts", 4)
            attempts = recovery.get("model_recovery_attempts", 0)
            total_attempts = recovery.get("model_recovery_total_attempts", 0)
            if (not response.get("recoverable", False)
                    or attempts >= max_attempts
                    or total_attempts >= total_cap):
                return model_failure_result(state, response)

            recovery["model_recovery_attempts"] = attempts + 1
            recovery["model_recovery_total_attempts"] = total_attempts + 1
            recovery["pending_model_error"] = {
                "kind": kind,
                "instruction": response.get(
                    "instruction",
                    "Model request failed. Continue concisely from current state.",
                ),
            }
            if kind == "context_overflow":
                shrink_factors = policy.get("shrink") or [1.0]
                factor = shrink_factors[min(attempts, len(shrink_factors) - 1)]
                shrink_context_for_model_recovery(state, goal, factor)
                working_messages = ensure_working_messages(state, context)
            __save_checkpoint__(state, {
                "nudge_count": consecutive_nudges,
                "consecutive_errors": consecutive_errors,
                "compaction_count": state.get("compaction_count", 0),
                "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
            })
            continue

        delivered_recovery = activate_delivered_recovery(state)

        if resp_type == "text":
            text = response.get("content", "")
            append_message(working_messages, "Assistant", text)
            if delivered_recovery:
                __save_checkpoint__(state, {
                    "nudge_count": consecutive_nudges,
                    "consecutive_errors": consecutive_errors,
                    "compaction_count": state.get("compaction_count", 0),
                    "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                })

            # Check for FINAL()
            final_answer = extract_final(text)
            if final_answer is not None:
                __transition_to__("completed", "FINAL() in text")
                return complete_result(state, "completed", final_answer)

            # Check for tool intent nudge. The same bounded counter also guards
            # format-recovery nudges below.
            if nudge_enabled and consecutive_nudges < max_nudges and signals_tool_intent(text):
                consecutive_nudges += 1
                append_message(
                    working_messages,
                    "User",
                    "You said you would perform an action, but you did not include any tool calls.\n"
                    "Do NOT describe what you intend to do — actually call the tool now.\n"
                    "Use the tool_calls mechanism to invoke the appropriate tool.",
                )
                continue

            # Strong roleplay or retry/meta leakage means the model slipped out
            # of CodeAct mode. Concise plain-text answers are valid completions.
            if (nudge_enabled
                    and consecutive_nudges < max_nudges
                    and not is_simple_goal(goal)
                    and signals_leaked_roleplay_or_meta(text)):
                consecutive_nudges += 1
                append_message(
                    working_messages,
                    "User",
                    "Your last response contained roleplay or retry commentary and did not complete this task.\n"
                    "Respond with a ```repl code block containing tool calls or FINAL().\n"
                    "Do not respond with plain text only.",
                )
                continue

            available_actions = __get_actions__()
            if (obligation_enabled
                    and consecutive_nudges == 0
                    and len(available_actions) > 0
                    and not state.get("_obligation_resolved", False)
                    and state.get("_obligation_nudge_count", 0) < max_obligation_nudges):
                state["_obligation_nudge_count"] = state.get("_obligation_nudge_count", 0) + 1
                append_message(
                    working_messages,
                    "User",
                    "You were asked to perform an action, but you responded with text only.\n"
                    "Do NOT describe or explain — call the appropriate tool now.\n"
                    "Use the tool_calls mechanism to invoke the tool.",
                )
                continue

            # Non-intent text response — reset nudge counter and finish
            if not signals_tool_intent(text):
                consecutive_nudges = 0

            # Plain text response - done
            __transition_to__("completed", "text response")
            return complete_result(state, "completed", text)

        elif resp_type == "code":
            state["_obligation_resolved"] = True
            code = response.get("code", "")
            append_message(working_messages, "Assistant", "```repl\n" + code + "\n```")
            if delivered_recovery:
                __save_checkpoint__(state, {
                    "nudge_count": consecutive_nudges,
                    "consecutive_errors": consecutive_errors,
                    "compaction_count": state.get("compaction_count", 0),
                    "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                })

            # Execute code in nested Monty VM
            result = __execute_code_step__(code, state)

            # Update persisted state with results
            if result.get("return_value") is not None:
                state["step_" + str(step) + "_return"] = result["return_value"]
                state["last_return"] = result["return_value"]
            for r in result.get("action_results", []):
                state[r.get("action_name", "unknown")] = r.get("output")

            # Format output for next LLM context
            output = format_output(result)
            append_message(working_messages, "User", output)

            if (result.get("had_error")
                    and "SyntaxError" in output
                    and nudge_enabled
                    and consecutive_nudges < max_nudges):
                consecutive_nudges += 1
                append_message(
                    working_messages,
                    "User",
                    "Your last ```repl block was not valid Python.\n"
                    "Respond with a corrected ```repl block containing tool calls or FINAL().",
                )
            elif not result.get("had_error"):
                consecutive_nudges = 0

            # Check for FINAL() in code output
            if result.get("final_answer") is not None:
                __transition_to__("completed", "FINAL() in code")
                return complete_result(state, "completed", result["final_answer"])

            # Check for unified gate pause (new path)
            gate = result.get("pending_gate")
            if gate is None:
                gate = result.get("need_approval")
            if gate is not None and isinstance(gate, dict) and gate.get("gate_paused"):
                mark_gate_pause(state)
                __save_checkpoint__(state, {
                    "nudge_count": consecutive_nudges,
                    "consecutive_errors": consecutive_errors,
                    "compaction_count": state.get("compaction_count", 0),
                    "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                })
                __transition_to__("waiting", "gate paused: " + gate.get("gate_name", "unknown"))
                return {
                    "outcome": "gate_paused",
                    "state": state,
                    "gate_name": gate.get("gate_name", ""),
                    "action_name": gate.get("action_name", ""),
                    "call_id": gate.get("call_id", ""),
                    "parameters": gate.get("parameters", {}),
                    "resume_kind": gate.get("resume_kind", {}),
                    "resume_output": gate.get("resume_output"),
                }

            # Check for approval or authentication needed (legacy path)
            if result.get("need_approval") is not None:
                approval = result["need_approval"]
                mark_gate_pause(state)
                __save_checkpoint__(state, {
                    "nudge_count": consecutive_nudges,
                    "consecutive_errors": consecutive_errors,
                    "compaction_count": state.get("compaction_count", 0),
                    "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                })
                if approval.get("need_authentication"):
                    __transition_to__("waiting", "authentication needed")
                    return {
                        "outcome": "need_authentication",
                        "state": state,
                        "credential_name": approval.get("credential_name", ""),
                        "action_name": approval.get("action_name", ""),
                        "call_id": approval.get("call_id", ""),
                        "parameters": approval.get("parameters", {}),
                    }
                __transition_to__("waiting", "approval needed")
                return {
                    "outcome": "need_approval",
                    "state": state,
                    "action_name": approval.get("action_name", ""),
                    "call_id": approval.get("call_id", ""),
                    "parameters": approval.get("parameters", {}),
                }

            progress = observe_work_progress(state, code_step_failed(result))
            if progress in ("iteration_limit", "no_progress", "consecutive_errors"):
                return terminal_recovery_result(state, progress)

            # Track consecutive errors
            if result.get("had_error"):
                consecutive_errors += 1
                if consecutive_errors >= max_consecutive_errors:
                    if schedule_terminal_warning(
                            state,
                            "consecutive_errors",
                            repeated_failures=consecutive_errors):
                        progress = "warning_scheduled"
                    else:
                        return terminal_recovery_result(state, "consecutive_errors")
            else:
                consecutive_errors = 0

            __save_checkpoint__(state, {
                "nudge_count": consecutive_nudges,
                "consecutive_errors": consecutive_errors,
                "compaction_count": state.get("compaction_count", 0),
                "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
            })
            if progress == "warning_scheduled":
                continue

        elif resp_type == "actions":
            state["_obligation_resolved"] = True
            # Tier 0: structured tool calls.
            # Structured calls do not reset the counter; successful CodeAct
            # execution and accepted non-intent text do.
            calls = response.get("calls", [])
            append_message(
                working_messages,
                "Assistant",
                response.get("content", "") or "",
                action_calls=calls,
            )
            if delivered_recovery:
                __save_checkpoint__(state, {
                    "nudge_count": consecutive_nudges,
                    "consecutive_errors": consecutive_errors,
                    "compaction_count": state.get("compaction_count", 0),
                    "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                })

            # Execute all tool calls in parallel via the batch host function.
            # Rust handles preflight (lease/policy), parallel execution via
            # JoinSet, and event emission in call order.
            results = __execute_actions_parallel__(calls)
            append_structured_action_results(working_messages, calls, results)

            # Check results for auth/approval interrupts
            for r_idx, r in enumerate(results):
                if r is None:
                    continue

                if r.get("gate_paused"):
                    # Unified gate pause (replaces separate need_approval/need_authentication)
                    mark_gate_pause(state)
                    __save_checkpoint__(state, {
                        "nudge_count": consecutive_nudges,
                        "consecutive_errors": consecutive_errors,
                        "compaction_count": state.get("compaction_count", 0),
                        "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                    })
                    gate = r
                    # Get action info from the original call or the result
                    orig_call = calls[r_idx] if r_idx < len(calls) else {}
                    __transition_to__("waiting", "gate paused: " + gate.get("gate_name", "unknown"))
                    return {
                        "outcome": "gate_paused",
                        "state": state,
                        "gate_name": gate.get("gate_name", ""),
                        "action_name": gate.get("action_name", orig_call.get("name", "")),
                        "call_id": orig_call.get("call_id", ""),
                        "parameters": orig_call.get("params", {}),
                        "resume_kind": gate.get("resume_kind", {}),
                        "resume_output": gate.get("resume_output"),
                    }

                if r.get("need_authentication"):
                    mark_gate_pause(state)
                    __save_checkpoint__(state, {
                        "nudge_count": consecutive_nudges,
                        "consecutive_errors": consecutive_errors,
                        "compaction_count": state.get("compaction_count", 0),
                        "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                    })
                    __transition_to__("waiting", "authentication needed")
                    return {
                        "outcome": "need_authentication",
                        "state": state,
                        "credential_name": r.get("credential_name", ""),
                        "action_name": r.get("action_name", ""),
                        "call_id": r.get("call_id", ""),
                        "parameters": r.get("parameters", {}),
                    }

                if r.get("need_approval"):
                    mark_gate_pause(state)
                    __save_checkpoint__(state, {
                        "nudge_count": consecutive_nudges,
                        "consecutive_errors": consecutive_errors,
                        "compaction_count": state.get("compaction_count", 0),
                        "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
                    })
                    __transition_to__("waiting", "approval needed")
                    return {
                        "outcome": "need_approval",
                        "state": state,
                        "action_name": r.get("action_name", ""),
                        "call_id": r.get("call_id", ""),
                        "parameters": r.get("parameters", {}),
                    }

            progress = observe_work_progress(state, action_batch_failed(calls, results))
            if progress in ("iteration_limit", "no_progress", "consecutive_errors"):
                return terminal_recovery_result(state, progress)

            __save_checkpoint__(state, {
                "nudge_count": consecutive_nudges,
                "consecutive_errors": consecutive_errors,
                "compaction_count": state.get("compaction_count", 0),
                "obligation_nudge_count": state.get("_obligation_nudge_count", 0),
            })
            if progress == "warning_scheduled":
                continue

    return terminal_recovery_result(state, "iteration_limit")


# Entry point: call run_loop with injected context variables
result = run_loop(context, goal, actions, state, config)
FINAL(result)
