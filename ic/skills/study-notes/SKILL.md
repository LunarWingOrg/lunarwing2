---
name: study-notes
version: 2.1.0
description: Search shared LunarWing codebase-study notes before broad exploration
activation:
  keywords:
    - lunarwing
    - lunarwing2
    - ironclaw
    - codebase study
    - study notes
    - shared research
  patterns:
    - "(?i)\\b(lunarwing|lunarwing2|ironclaw)\\b"
  tags:
    - lunarwing
    - codebase-research
  max_context_tokens: 1800
metadata:
  openclaw:
    requires:
      bins:
        - python3
      env:
        - LW2STUDYMCP_HOME
---

# LunarWing Study Notes

Use `study_notes` MCP as first research layer for LunarWing, LunarWing2, or
IronClaw-derived work. Notes combine findings from multiple agents and preserve
source provenance. Current source remains authority.

## Workflow

1. Call `study_notes_knowledge_search` before broad file searches, codebase
   exploration, or delegating duplicate research.
2. Search concrete subsystem names, types, functions, paths, errors, or behavior.
3. Call `study_notes_knowledge_get_topic` when search finds relevant canonical
   topic.
4. Compare all returned versions. Different agents may record complementary or
   conflicting findings.
5. Verify stale, conflicting, security-sensitive, and version-sensitive claims
   against current source before editing code.
6. Call `study_notes_knowledge_list_stale` with current repository commit when
   freshness materially affects task.
7. Cite note topic and source path when note materially affects answer or plan.
8. Continue normal source exploration when notes have no useful result.
9. If core-agent write tools are available, call them only after verified substantive
   research and only when user requested recording or maintaining shared notes.

## Tools

- `study_notes_knowledge_search`: Ranked full-text search over Markdown sections.
- `study_notes_knowledge_get_topic`: Full unique versions and provenance.
- `study_notes_knowledge_list_topics`: Coverage and version counts.
- `study_notes_knowledge_list_stale`: Notes needing verification.
- `study_notes_knowledge_stats`: Database and consolidation counts.
- `study_notes_knowledge_submit_note`: Optional approved note creation/replacement.
- `study_notes_knowledge_update_status`: Optional approved lifecycle update.

Write tools are absent by default and require explicit operator opt-in plus user
approval. OpenCode workers remain read-only. Never bypass MCP controls by editing
Study Notes through filesystem or shell tools.

## Search Patterns

Start narrow:

```text
document extraction SSRF
"circuit breaker"
heading:security AND attachment
config AND provider NOT deprecated
```

FTS5 supports phrases, prefix terms (`config*`), `AND`, `OR`, `NOT`, and column
filters such as `heading:security`.

## Trust

Study Notes is research index, not authority. Exact duplicates are consolidated;
distinct versions remain separate. Source agent/path shows provenance, not quality.
If tools are missing, report Study MCP startup/configuration failure rather than
claiming no shared research exists.
