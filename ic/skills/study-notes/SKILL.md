---
name: study-notes
version: 3.1.0
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
---

# LunarWing Study Notes (native)

Use the built-in `knowledge_*` tools as the first research layer for LunarWing,
LunarWing2, or IronClaw-derived work. Notes are workspace documents backed by
PostgreSQL full-text search, stored per-tenant with a shared herd scope. They
combine findings from multiple agents and preserve source provenance
(submitting agent, user scope, session, studied timestamp, source commit).
Current source remains authority.

## Workflow

1. Call `knowledge_search` before broad file searches, codebase exploration,
   or delegating duplicate research.
2. Search concrete subsystem names, types, functions, paths, errors, or behavior.
3. Call `knowledge_get_topic` when search finds a relevant canonical topic.
4. Compare all returned versions. Different agents may record complementary or
   conflicting findings; `submitted_by` and `session_id` show who recorded each.
5. Verify stale, conflicting, security-sensitive, and version-sensitive claims
   against current source before editing code.
6. Call `knowledge_list_stale` with the current repository commit when
   freshness materially affects the task. A background sweep also marks notes
   stale automatically once they age past the configured threshold.
7. Cite note topic and source path when a note materially affects the answer
   or plan.
8. Continue normal source exploration when notes have no useful result.

## Writing notes

Record durable, source-verified findings with `knowledge_submit_note` — the
native write path. Submit only after verifying against current source, and
only when the user requested recording or maintaining shared notes.

- Defaults to your private tenant scope; use `scope="shared"` for herd
  knowledge every agent may read.
- Notes are per-agent versions of a canonical topic. Resubmit with
  `replace=true` to update YOUR OWN agent's version, never another agent's.
- Use `status="needs-verification"` for unresolved findings; use `active` only
  for findings verified against current source in this session.
- Include `source_commit`, `confidence`, and lowercase `tags` when known.
- `stale_after_days` sets a per-note staleness threshold (default 90 days);
  `status_reason` records why a lifecycle status was chosen.
- Adjust lifecycle later with `knowledge_update_status` — metadata only
  (status, confidence, tags, studied timestamp, source commit, staleness
  threshold, status reason, supersession). Content changes always go through
  `knowledge_submit_note` with `replace=true`.

## Tools

- `knowledge_search`: Ranked PostgreSQL FTS over study-note chunks with
  topic/agent/status/min-confidence filters and every-listed-tag matching;
  covers private + shared scopes by default.
- `knowledge_get_topic`: All per-agent versions of one topic with content and
  provenance.
- `knowledge_list_topics`: Topic coverage survey — per-topic version counts,
  scopes, agents, statuses, and tags.
- `knowledge_submit_note`: Validated note creation/replacement (native write
  path; no external MCP required).
- `knowledge_update_status`: Metadata-only lifecycle updates on an existing
  note version; `superseded` requires `superseded_by`.
- `knowledge_list_stale`: Notes needing verification by status, age (explicit
  limit or the note's own `stale_after_days`), or source commit.
- `knowledge_stats`: Store totals, lifecycle status counts, per-scope counts.

## Search patterns

Start narrow: concrete subsystem names, types, functions, paths, or errors.
PostgreSQL `websearch_to_tsquery` semantics apply — plain words are ANDed,
quoted phrases match exactly, `OR` and `-` (negation) are supported.

## Trust

Study Notes is a research index, not authority. Distinct agent versions remain
separate. Provenance (agent, submitted_by, session) shows origin, not quality.
The background stale sweep marks aging notes, but `active` status is not proof
of current correctness — verify before relying on a claim.
