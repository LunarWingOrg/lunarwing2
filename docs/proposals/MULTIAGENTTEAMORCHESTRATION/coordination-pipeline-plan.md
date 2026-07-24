# Coordination Pipeline Implementation Plan — Engine V2

## Status: DRAFT (pending sun approval)
## Date: 2026-07-24
## Author: Captain Ruffles
## Based on: `projects/openclaw/AgentFramework.md` + verified engine V2 source

---

## Overview

Implement the Agent Framework coordination pipeline directly into the LunarWing v2 engine (Option A). This gives uniform enforcement across all siblings without relying on per-agent skills.

The framework maps onto **two existing engine systems**:
- **Hooks** (ic/src/hooks/) — message interception and transformation
- **Gates** (lunarwing_engine/src/gate/) — pre-execution authorization

---

## Architecture: How the Framework Maps to Engine V2

### Agent Framework Pipeline → Engine V2 Layers

| Framework Hook | Engine Layer | Mechanism |
|---|---|---|
| `on_inbound_message` | **BeforeInbound** hook point | New `CoordinationInboundHook` |
| `on_should_engage` | **BeforeInbound** hook (post-validation) | New `EngagementPolicyHook` |
| `on_response_filter` | **BeforeOutbound** + **TransformResponse** hooks | New `CoordinationOutboundHook` |

### Why Hooks (not Gates) for Most of This

**Gates** (`ExecutionGate`) are for **tool execution authorization** — they run before a tool call is dispatched and return Allow/Pause/Deny. They have `GateContext` with action_name, parameters, thread_id. They're the wrong shape for message-level coordination.

**Hooks** (`Hook` trait) are for **message and event interception** — they run at lifecycle points (BeforeInbound, BeforeOutbound, etc.), can modify content, and can reject events. `HookEvent::Inbound` has user_id, channel, content. `HookEvent::Outbound` has the same. **This is exactly what we need.**

### Exception: Three-Strike Auto-Disable

The three-strike enforcement needs persistent state and the ability to **block all future activity** by an agent. This is better as a **Gate** because it needs to deny tool execution, not just message delivery. A `CoordinationGate` at priority 15 (after LeaseGate at 10, before ToolTierGate at 20) can deny all actions for an agent that's been auto-disabled.

---

## Implementation Plan — 6 Components

### Component 1: CoordinationConfig (config struct)

**File:** `ic/src/hooks/coordination.rs` (new)
**Type:** `CoordinationConfig` struct

```rust
pub struct CoordinationConfig {
    pub max_message_length: usize,           // 420
    pub rate_limit_per_hour: u32,            // 2
    pub allowed_tags: Vec<String>,           // ["STATUS", "ALERT", "COORD", "ANNOUNCE", "QUERY"]
    pub coalesce_window_ms: u64,             // 1500-3000
    pub coordination_channels: Vec<String>,  // MUC JIDs that enforce coordination mode
    pub enabled: bool,
}
```

Loaded from workspace config or env vars. Default: disabled.

---

### Component 2: CoordinationInboundHook (on_inbound_message)

**File:** `ic/src/hooks/coordination.rs`
**Hook Point:** `BeforeInbound`
**Priority:** 50 (runs early, after audit log at 25)

**Responsibilities:**
1. Check if message is from a coordination channel — if not, pass through (no-op)
2. Validate message structure: starts with allowed tag, single-line, ≤420 chars
3. Run secret detection (regex + entropy)
4. Enforce rate limiting (2 msgs/hr per agent per coordination channel)
5. If any check fails → `HookOutcome::Reject { reason }` (silent halt)

**State needed:**
- `RateLimiterState` — per-agent message timestamps (sliding window)
- `SecretPatterns` — compiled regex set for common secret formats

---

### Component 3: EngagementPolicyHook (on_should_engage)

**File:** `ic/src/hooks/coordination.rs`
**Hook Point:** `BeforeInbound` (runs after Component 2)
**Priority:** 60

**Responsibilities:**
1. Check if message is from coordination channel — if not, pass through
2. Determine if this agent should engage:
   - Message tagged with `@this_agent` → Engage
   - Message is direct reply → Engage
   - Message is DM → Engage
   - Otherwise → Pass (Reject with special reason "pass")
3. First-responder correlation: check if another agent already claimed this message
   - Uses correlation ID (message ID or hash)
   - If already claimed → Pass

**State needed:**
- `FirstResponderStore` — maps correlation_id → agent_who_claimed (with TTL)

**Design note:** The "Pass" behavior needs to be a silent no-op, not a Reject. The hook returns `HookOutcome::Continue { modified: None }` — the engine just won't generate a response because the message wasn't directed at this agent. The engagement check is really about whether to *wake up and respond*, which is the engine's thread-spawning decision. This may require coordination with the router to prevent thread spawning for non-targeted messages.

---

### Component 4: CoordinationOutboundHook (on_response_filter)

**File:** `ic/src/hooks/coordination.rs`
**Hook Point:** `BeforeOutbound` + `TransformResponse`
**Priority:** 50

**Responsibilities:**
1. Check if destination is coordination channel — if not, pass through
2. Enforce single-line (replace \n with space)
3. Enforce ≤420 chars (truncate or reject)
4. Validate tag prefix (must start with allowed tag)
5. If invalid → `HookOutcome::Reject` (silent halt)

**Post-response coalesce window:** 1.5-3 second delay before sending to allow for edits. This needs timer integration in the channel adapter, not just a hook. May require a new mechanism in the XMPP channel adapter.

---

### Component 5: ViolationTracker + CoordinationGate (three-strike)

**File:** `ic/src/hooks/coordination.rs` (tracker) + `lunarwing_engine/src/gate/coordination.rs` (gate)
**Gate Priority:** 15 (after LeaseGate at 10, before ToolTierGate at 20)

**Responsibilities:**
1. Track violations per agent (in-memory + persisted to workspace state)
2. When a violation occurs (Components 2-4 reject), increment counter
3. Three-strike escalation:
   - 1st: silent halt (already handled by hook Reject)
   - 2nd: ops alert via Gotify/DM (webhook hook or direct send)
   - 3rd: auto-disable for 24h → CoordinationGate denies all tool execution
4. Reset counters after 24h or admin reset

**State needed:**
- `ViolationStore` — per-agent violation count + last violation timestamp
- Persisted to `$LUNARWING_BASE_DIR/coordination-violations.json`

**CoordinationGate implementation:**
```rust
pub struct CoordinationGate {
    violation_store: Arc<ViolationStore>,
}

// priority: 15
// evaluate(): check if agent is disabled → Deny { reason: "Auto-disabled: 3 coordination violations" }
```

---

### Component 6: Declarative Hook Bundle (coordination.json)

**File:** Workspace config `coordination/hooks.json` (or similar)
**Loaded by:** Existing `HookBundleConfig` system

For rules that can be expressed declaratively (tag validation, length caps, newline stripping), we can use the **existing RuleHook system** without writing Rust:

```json
{
  "rules": [
    {
      "name": "coord-single-line",
      "points": ["beforeOutbound"],
      "when_regex": "\\n",
      "replacements": [{"pattern": "\\n", "replacement": " "}]
    },
    {
      "name": "coord-secret-block",
      "points": ["beforeInbound", "beforeOutbound"],
      "when_regex": "(?i)(api_key|secret|password|token)\\s*[=:][^\\s]+",
      "reject_reason": "coordination: secret detected"
    }
  ]
}
```

This lets us ship basic enforcement immediately and iterate without recompiling.

---

## File Inventory (new files)

| File | Purpose | LOC estimate |
|---|---|---|
| `ic/src/hooks/coordination.rs` | Inbound/outbound hooks, config, rate limiter, first-responder store | ~500 |
| `ic/src/hooks/coordination_test.rs` | Unit tests for all hooks | ~300 |
| `lunarwing_engine/src/gate/coordination.rs` | CoordinationGate + ViolationStore | ~250 |
| `lunarwing_engine/src/gate/mod.rs` | Add `pub mod coordination;` | +1 line |
| `ic/src/hooks/mod.rs` | Add `pub mod coordination;` | +1 line |
| `coordination/hooks.json` | Declarative rules bundle (workspace) | ~50 |

**Total new code:** ~1,100 lines Rust + ~50 lines JSON

---

## Phased Delivery

### Phase 1: Declarative Rules (no Rust changes)
- Ship `coordination/hooks.json` with basic tag validation, single-line enforcement, secret regex
- Works immediately via existing HookBundleConfig
- **Delivers:** structure enforcement, secret blocking, length caps

### Phase 2: CoordinationInboundHook + CoordinationOutboundHook (Rust)
- Implement rate limiting (Component 2)
- Implement outbound normalization (Component 4)
- Engagement policy (Component 3) — needs router integration discussion
- **Delivers:** rate limiting, engagement policy, full inbound validation

### Phase 3: Three-Strike Gate (Rust)
- Implement ViolationStore + CoordinationGate (Component 5)
- First-responder correlation store (Component 3 state)
- **Delivers:** three-strike enforcement, auto-disable, first-responder

### Phase 4: Coalesce Window (channel adapter)
- Add pre-send coalesce delay to XMPP channel adapter
- 1.5-3 second window for edits
- **Delivers:** coalesce window, full framework compliance

---

## Open Questions for Sun

1. **Engagement policy integration:** The "should this agent respond?" decision currently lives in the router (does an inbound message spawn a thread?). Do we want the EngagementPolicyHook to influence thread spawning, or should it run *after* thread spawn and just gate the response?

2. **First-responder store:** Should this be per-host (each agent instance has its own) or shared (Redis/file-based for cross-agent coordination)? Per-host is simpler but agents won't know if another sibling already responded.

3. **Coalesce window:** Should this be in the channel adapter (delays all messages) or a hook-specific timer? The adapter approach is cleaner but affects non-coordination messages too.

4. **Coordination channel identification:** How do we know which channels are coordination MUCs? Config list of JIDs? Convention-based naming (e.g., `coord@conference.*`)?

5. **Admin commands:** The framework mentions localhost-only HTTP for admin control. Should this go through the existing router HTTP endpoints or a new dedicated endpoint?

6. **Violation persistence:** Should violations survive agent restarts? If so, we need a JSON file or DB-backed store. If not, in-memory is fine.

---

## Dependencies

- Existing HookRegistry and Hook system (✅ ready)
- Existing GatePipeline (✅ ready)
- XMPP channel adapter (for coalesce window — Phase 4)
- Router integration for engagement policy (Phase 2 — needs discussion)

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Engagement policy doesn't cleanly integrate with router | Medium | High | Start with response-side gating, iterate on router integration |
| First-responder store adds latency | Low | Medium | Use in-memory store with short TTL |
| Coalesce window delays critical messages | Medium | Low | Only apply to coordination channels, not all messages |
| Declarative regex rules too rigid | Low | Low | Phase 2 Rust hooks provide more sophisticated logic |

---

*"The purpose of strength is not domination, but restraint." — Captain Ruffles*


## Coordination Pipeline — Phase 2: Stateful Rust Hooks

### Context from Source Review

**Hook trait** (`ic/src/hooks/hook.rs`):
```rust
#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    fn hook_points(&self) -> &[HookPoint];
    fn failure_mode(&self) -> HookFailureMode { HookFailureMode::FailOpen }
    fn timeout(&self) -> Duration { Duration::from_secs(30) }
    async fn execute(&self, event: &HookEvent, ctx: &HookContext) -> Result<HookOutcome, HookError>;
}
```

**HookEvent variants** carry `user_id`, `channel`, `content`, `thread_id` (Inbound/Outbound).

**HookOutcome**: `Continue { modified: Option<String> }` or `Reject { reason: String }`.

**HookRegistry**: `RwLock<Vec<HookEntry>>`, priority-ordered, clone hooks before running.

**Key constraint**: `HookContext` only has `metadata: serde_json::Value` — no shared state injection. Stateful hooks must own their state internally via `Arc<Mutex<...>>`.

**Gate system** (`lunarwing_engine/src/gate/`):
- `ExecutionGate` trait: name(), priority(), evaluate(&GateContext) -> GateDecision
- GateDecision: Allow/Pause{reason, ResumeKind}/Deny{reason}
- GatePipeline sorts by priority, first Pause/Deny wins
- GateContext: user_id, thread_id, source_channel, action_name, parameters, execution_mode
- **Gates are for tool execution authorization, NOT message interception.** Phase 3 uses gates for three-strike auto-disable.

### New Files

```
ic/src/hooks/
├── coordination.rs    ← NEW: 5 stateful hooks + shared state
├── bundled.rs         ← register_coordination_hooks() called from bootstrap
└── bootstrap.rs       ← updated to register coordination hooks
```

### Component 1: CoordinationState (shared state store)

```rust
// ic/src/hooks/coordination.rs

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::{Duration, Instant};

/// Shared coordination state accessible to all coordination hooks.
#[derive(Debug, Default)]
pub struct CoordinationState {
    inner: Arc<Mutex<CoordinationStateInner>>,
}

#[derive(Debug, Default)]
struct CoordinationStateInner {
    /// Rate limit: keyed by (agent_id, channel) → Vec<Instant> (message timestamps in current window)
    rate_limit_windows: HashMap<String, Vec<Instant>>,
    /// First-responder: keyed by correlation_id → agent_id who first claimed it
    responder_claims: HashMap<String, String>,
    /// Violation counters: keyed by agent_id → violation count
    violation_counts: HashMap<String, u32>,
    /// Disabled agents: keyed by agent_id → Instant when disabled
    disabled_agents: HashMap<String, Instant>,
    /// Coordination channels: JIDs/names that enforce coordination rules
    coordination_channels: Vec<String>,
}
```

### Component 2: TagSchemaHook (BeforeOutbound)

**Purpose:** Reject messages in coordination channels that don't start with an allowed tag.

```rust
const ALLOWED_TAGS: [&str; 5] = ["STATUS:", "ALERT:", "COORD:", "ANNOUNCE:", "QUERY:"];

pub struct TagSchemaHook {
    state: CoordinationState,
    failure_mode: HookFailureMode, // FailClosed
}

#[async_trait]
impl Hook for TagSchemaHook {
    fn name(&self) -> &str { "coordination.tag_schema" }
    fn hook_points(&self) -> &[HookPoint] { &[HookPoint::BeforeOutbound, HookPoint::TransformResponse] }
    fn failure_mode(&self) -> HookFailureMode { HookFailureMode::FailClosed }

    async fn execute(&self, event: &HookEvent, _ctx: &HookContext) -> Result<HookOutcome, HookError> {
        let (channel, content, user_id) = match event {
            HookEvent::Outbound { channel, content, user_id, .. } => (channel, content, user_id),
            HookEvent::ResponseTransform { response, user_id, .. } => {
                // TransformResponse doesn't carry channel — check all coordination channels
                // Use user_id as proxy
                (self.state.get_any_coord_channel().as_str(), response, user_id)
            }
            _ => return Ok(HookOutcome::ok()),
        };

        // Only enforce in coordination channels
        if !self.state.is_coordination_channel(channel).await {
            return Ok(HookOutcome::ok());
        }

        let trimmed = content.trim();
        let starts_with_tag = ALLOWED_TAGS.iter().any(|tag| trimmed.starts_with(tag));

        if !starts_with_tag {
            self.state.record_violation(user_id).await;
            return Ok(HookOutcome::reject("Message in coordination channel must start with STATUS/ALERT/COORD/ANNOUNCE/QUERY"));
        }

        Ok(HookOutcome::ok())
    }
}
```

### Component 3: RateLimitHook (BeforeOutbound)

**Purpose:** Enforce 2 messages per hour per agent in coordination channels.

```rust
pub struct RateLimitHook {
    state: CoordinationState,
    max_messages: usize,        // default: 2
    window: Duration,           // default: 1 hour
}

#[async_trait]
impl Hook for RateLimitHook {
    fn name(&self) -> &str { "coordination.rate_limit" }
    fn hook_points(&self) -> &[HookPoint] { &[HookPoint::BeforeOutbound] }
    fn failure_mode(&self) -> HookFailureMode { HookFailureMode::FailClosed }

    async fn execute(&self, event: &HookEvent, _ctx: &HookContext) -> Result<HookOutcome, HookError> {
        let (channel, user_id) = match event {
            HookEvent::Outbound { channel, user_id, .. } => (channel, user_id),
            _ => return Ok(HookOutcome::ok()),
        };

        if !self.state.is_coordination_channel(channel).await {
            return Ok(HookOutcome::ok());
        }

        if self.state.is_disabled(user_id).await {
            return Ok(HookOutcome::reject("Agent disabled due to coordination violations"));
        }

        let key = format!("{}:{}", user_id, channel);
        let now = Instant::now();

        // Try to record the message
        let allowed = self.state.try_consume_rate_limit(&key, self.max_messages, self.window, now).await;

        if !allowed {
            self.state.record_violation(user_id).await;
            return Ok(HookOutcome::reject("Rate limit exceeded (2 messages/hour in coordination channel)"));
        }

        Ok(HookOutcome::ok())
    }
}
```

### Component 4: EngagementPolicyHook (BeforeInbound)

**Purpose:** Determine if the agent should engage based on @mentions, direct replies, or tag targeting.

```rust
pub struct EngagementPolicyHook {
    state: CoordinationState,
    agent_id: String,       // this agent's identifier
    agent_aliases: Vec<String>,  // e.g., ["ruffles", "@ruffles", "captain"]
}

#[async_trait]
impl Hook for EngagementPolicyHook {
    fn name(&self) -> &str { "coordination.engagement_policy" }
    fn hook_points(&self) -> &[HookPoint] { &[HookPoint::BeforeInbound] }
    fn failure_mode(&self) -> HookFailureMode { HookFailureMode::FailOpen }

    async fn execute(&self, event: &HookEvent, _ctx: &HookContext) -> Result<HookOutcome, HookError> {
        let (channel, content, user_id) = match event {
            HookEvent::Inbound { channel, content, user_id, .. } => (channel, content, user_id),
            _ => return Ok(HookOutcome::ok()),
        };

        // Non-coordination channels: allow everything
        if !self.state.is_coordination_channel(channel).await {
            return Ok(HookOutcome::ok());
        }

        // Messages from self or admin: always allow
        if *user_id == self.agent_id || self.state.is_admin(user_id).await {
            return Ok(HookOutcome::ok());
        }

        // Check engagement criteria:
        // 1. Message contains @agent or alias
        let is_targeted = self.agent_aliases.iter()
            .any(|alias| content.to_lowercase().contains(&alias.to_lowercase()));

        // 2. Message starts with a QUERY tag directed at this agent
        let is_query = content.trim_start().to_uppercase().starts_with("QUERY:")
            && self.agent_aliases.iter()
                .any(|alias| content.to_lowercase().contains(&alias.to_lowercase()));

        if !is_targeted && !is_query {
            // Silent pass — don't engage
            return Ok(HookOutcome::reject("Not targeted at this agent"));
        }

        Ok(HookOutcome::ok())
    }
}
```

### Component 5: FirstResponderHook (BeforeInbound)

**Purpose:** Prevent duplicate responses when multiple agents could respond.

```rust
pub struct FirstResponderHook {
    state: CoordinationState,
    agent_id: String,
    coalesce_window: Duration,  // default: 2 seconds
}

#[async_trait]
impl Hook for FirstResponderHook {
    fn name(&self) -> &str { "coordination.first_responder" }
    fn hook_points(&self) -> &[HookPoint] { &[HookPoint::BeforeInbound] }
    fn failure_mode(&self) -> HookFailureMode { HookFailureMode::FailOpen }

    async fn execute(&self, event: &HookEvent, _ctx: &HookContext) -> Result<HookOutcome, HookError> {
        let (content, user_id) = match event {
            HookEvent::Inbound { content, user_id, .. } => (content, user_id),
            _ => return Ok(HookOutcome::ok()),
        };

        // Generate correlation ID from message content hash + channel
        let correlation_id = simple_hash(content);

        // Check if someone already claimed this message
        match self.state.try_claim(&correlation_id, &self.agent_id, self.coalesce_window).await {
            ClaimResult::AlreadyClaimed(other_agent) => {
                tracing::debug!(
                    agent = %self.agent_id,
                    claimed_by = %other_agent,
                    "First-responder: message already claimed"
                );
                return Ok(HookOutcome::reject("Another agent already claimed this message"));
            }
            ClaimResult::Claimed => {
                // We're first! Continue processing
                return Ok(HookOutcome::ok());
            }
        }
    }
}
```

### Component 6: SecretEntropyHook (BeforeInbound + BeforeOutbound)

**Purpose:** Beyond Phase 1 regex, detect high-entropy strings that might be secrets.

```rust
pub struct SecretEntropyHook {
    // Shannon entropy threshold (default: 4.5 bits/char)
    entropy_threshold: f64,
    // Minimum length to check (default: 32 chars)
    min_length: usize,
}

#[async_trait]
impl Hook for SecretEntropyHook {
    fn name(&self) -> &str { "coordination.secret_entropy" }
    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::BeforeInbound, HookPoint::BeforeOutbound]
    }
    fn failure_mode(&self) -> HookFailureMode { HookFailureMode::FailClosed }

    async fn execute(&self, event: &HookEvent, _ctx: &HookContext) -> Result<HookOutcome, HookError> {
        let content = extract_primary_content(event);

        for token in content.split_whitespace() {
            if token.len() >= self.min_length {
                let entropy = shannon_entropy(token);
                if entropy >= self.entropy_threshold {
                    return Ok(HookOutcome::reject("Potential secret detected (high entropy)"));
                }
            }
        }

        Ok(HookOutcome::ok())
    }
}

fn shannon_entropy(s: &str) -> f64 {
    // Standard Shannon entropy calculation
    // H = -sum(p_i * log2(p_i)) for each character
    let mut freq: HashMap<char, usize> = HashMap::new();
    let len = s.chars().count();
    if len == 0 { return 0.0; }
    for c in s.chars() {
        *freq.entry(c).or_insert(0) += 1;
    }
    let mut entropy = 0.0;
    for &count in freq.values() {
        let p = count as f64 / len as f64;
        entropy -= p * p.log2();
    }
    entropy
}
```

### Registration in Bootstrap

```rust
// ic/src/hooks/bundled.rs — add function:

pub async fn register_coordination_hooks(
    registry: &Arc<HookRegistry>,
    agent_id: String,
    agent_aliases: Vec<String>,
    coordination_channels: Vec<String>,
    config: CoordinationHookConfig,
) -> HookRegistrationSummary {
    let state = CoordinationState::new(coordination_channels);
    let mut summary = HookRegistrationSummary::default();

    // Priority order (lower = runs first):
    //  5 — secret entropy (highest priority, reject before anything else)
    // 15 — engagement policy
    // 16 — first responder
    // 20 — tag schema
    // 25 — rate limit

    let hooks: Vec<(Arc<dyn Hook>, u32)> = vec![
        (Arc::new(SecretEntropyHook::new(config.entropy_threshold, config.min_length)), 5),
        (Arc::new(EngagementPolicyHook::new(state.clone(), agent_id.clone(), agent_aliases.clone())), 15),
        (Arc::new(FirstResponderHook::new(state.clone(), agent_id, config.coalesce_window)), 16),
        (Arc::new(TagSchemaHook::new(state.clone())), 20),
        (Arc::new(RateLimitHook::new(state, config.max_messages, config.rate_window)), 25),
    ];

    for (hook, priority) in hooks {
        registry.register_with_priority(hook, priority).await;
        summary.hooks += 1;
    }

    summary
}

// ic/src/hooks/bootstrap.rs — add call in bootstrap_hooks():

// After workspace bundles, if coordination is enabled:
if coordination_enabled {
    let coord_config = CoordinationHookConfig::from_env();
    register_coordination_hooks(
        registry,
        coord_config.agent_id,
        coord_config.agent_aliases,
        coord_config.coordination_channels,
        coord_config,
    ).await;
}
```

### Configuration

```rust
/// ic/src/hooks/coordination.rs

#[derive(Debug, Clone)]
pub struct CoordinationHookConfig {
    pub agent_id: String,
    pub agent_aliases: Vec<String>,
    pub coordination_channels: Vec<String>,
    pub max_messages: usize,         // default: 2
    pub rate_window: Duration,       // default: 1 hour
    pub coalesce_window: Duration,   // default: 2 seconds
    pub entropy_threshold: f64,      // default: 4.5
    pub min_secret_length: usize,    // default: 32
    pub admin_users: Vec<String>,    // users exempt from restrictions
}

impl Default for CoordinationHookConfig {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            agent_aliases: Vec::new(),
            coordination_channels: Vec::new(),
            max_messages: 2,
            rate_window: Duration::from_secs(3600),
            coalesce_window: Duration::from_millis(2000),
            entropy_threshold: 4.5,
            min_secret_length: 32,
            admin_users: Vec::new(),
        }
    }
}

impl CoordinationHookConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        // COORD_AGENT_ID, COORD_AGENT_ALIASES (comma-separated),
        // COORD_CHANNELS (comma-separated JIDs),
        // COORD_MAX_MESSAGES, COORD_COALESCE_MS, etc.
        if let Ok(id) = std::env::var("COORD_AGENT_ID") {
            config.agent_id = id;
        }
        if let Ok(aliases) = std::env::var("COORD_AGENT_ALIASES") {
            config.agent_aliases = aliases.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(channels) = std::env::var("COORD_CHANNELS") {
            config.coordination_channels = channels.split(',').map(|s| s.trim().to_string()).collect();
        }
        // ... other env vars
        config
    }
}
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_ENABLED` | `false` | Enable coordination hooks |
| `COORD_AGENT_ID` | (required) | This agent's unique identifier |
| `COORD_AGENT_ALIASES` | (required) | Comma-separated @mentions that target this agent |
| `COORD_CHANNELS` | (required) | Comma-separated coordination channel JIDs |
| `COORD_MAX_MESSAGES` | `2` | Max messages per hour per agent |
| `COORD_COALESCE_MS` | `2000` | First-responder coalesce window in ms |
| `COORD_ENTROPY_THRESHOLD` | `4.5` | Shannon entropy threshold for secret detection |
| `COORD_MIN_SECRET_LENGTH` | `32` | Minimum token length for entropy check |
| `COORD_ADMIN_USERS` | (empty) | Comma-separated admin user IDs |

### Implementation Order

1. **CoordinationState** — the shared state store (all methods, with tests)
2. **SecretEntropyHook** — standalone, no state dependency, includes `shannon_entropy()` function
3. **RateLimitHook** — depends on CoordinationState rate limiting methods
4. **TagSchemaHook** — depends on CoordinationState channel detection
5. **EngagementPolicyHook** — depends on CoordinationState channel + admin detection
6. **FirstResponderHook** — depends on CoordinationState claim system
7. **register_coordination_hooks()** — wire it all together
8. **Bootstrap integration** — call from `bootstrap_hooks()` when `COORD_ENABLED=true`

### Testing Strategy

Each hook gets unit tests with mock HookEvent:
- `test_tag_schema_rejects_untagged_in_coord_channel`
- `test_tag_schema_allows_untagged_in_casual_channel`
- `test_rate_limit_allows_under_limit`
- `test_rate_limit_rejects_over_limit`
- `test_engagement_passes_targeted_message`
- `test_engagement_rejects_untargeted_message`
- `test_first_responder_first_caller_wins`
- `test_first_responder_second_caller_rejected`
- `test_secret_entropy_detects_high_entropy`
- `test_secret_entropy_allows_normal_text`

### Estimated LOC
- `coordination.rs`: ~500-600 lines (state + 5 hooks + config + helpers)
- `bundled.rs` additions: ~40 lines (registration function)
- `bootstrap.rs` additions: ~10 lines (call site)
- Tests: ~300 lines
- **Total: ~850-950 lines**



## Phase 3: Three-Strike Enforcement Gate + Auto-Disable

### Components
1. **ViolationStore** (gate/violation_store.rs, ~150 LOC) — persistent per-agent violation tracking
   - File: $LUNARWING_BASE_DIR/coordination-violations.json (atomic write)
   - Methods: record_violation, check_status, auto_disable, manual_disable, clear_violations
   - Status: Active(0) → Warning(1) → Warning+OpsAlert(2) → Disabled(3+, 24h)
   
2. **CoordinationGate** (gate/coordination.rs, ~80 LOC) — ExecutionGate impl
   - Priority: 5 (above LeaseGate at 10)
   - Deny(silent) when agent is auto-disabled
   - Auto-clears when disable period expires
   
3. **Admin HTTP endpoints** (~50 LOC) — 127.0.0.1 only
   - /coord/status, /coord/clear, /coord/disable, /coord/enable

4. **Phase 2 integration** (~20 LOC) — CoordinationState gains Arc<ViolationStore>
   - Hooks call record_violation on detection
   - Strike 2 fires Gotify ops alert
   - Strike 3 triggers auto-disable

### Config: COORD_VIOLATION_DISABLE_DURATION, COORD_VIOLATION_MAX, COORD_VIOLATION_DECAY
### Total: ~400 LOC, low-medium complexity
### Key design: Silent halts preferred — disabled agents go dark, no channel message



## Phase 3: Three-Strike Enforcement Gate + Auto-Disable (DETAILED)

### Architecture
1. **ViolationStore** (ic/src/gate/violation_store.rs, ~150 LOC)
   - Persistent per-agent violation tracking
   - File: $LUNARWING_BASE_DIR/coordination-violations.json (atomic write: temp+rename)
   - Single Mutex, same pattern as PendingGateStore
   - ViolationStatus: Active(0) → Warning(1) → FinalWarning(2) → Disabled(3+, 24h)
   - ViolationRecord: timestamp, reason, hook, channel
   - Methods: record_violation, get_status, clear_violations, manual_disable, manual_enable, expire_check
   - Violation decay: 7 days (configurable via COORD_VIOLATION_DECAY)

2. **CoordinationGate** (ic/crates/lunarwing_engine/src/gate/coordination.rs, ~80 LOC)
   - ExecutionGate impl, priority 5 (above LeaseGate at 10)
   - Deny(silent=true) when agent is auto-disabled
   - Allow otherwise — gate only checks disabled status, hooks do the counting
   - Permissive mode via COORD_PERMISSIVE=true

3. **GateDecision Enhancement** (~10 LOC change to gate/mod.rs)
   - Add silent: bool to GateDecision::Deny variant
   - Allows silent denies for disabled agents (no channel message, no error surfaced)
   - Existing Deny calls default to silent=false

4. **Phase 2 Integration** (~30 LOC)
   - CoordinationState gains Arc<ViolationStore>
   - Hooks call record_violation on detection
   - Strike 1: silent halt (hook already rejects message)
   - Strike 2: Gotify ops alert fired
   - Strike 3: auto-disable set by store, CoordinationGate blocks all activity

5. **Admin HTTP Endpoints** (~50 LOC, localhost-only in router.rs)
   - GET /coord/status — all agents
   - GET /coord/status/:agent — single agent
   - POST /coord/clear/:agent — reset violations
   - POST /coord/disable/:agent — manual disable
   - POST /coord/enable/:agent — manual enable

6. **Config**
   - COORD_VIOLATION_MAX=3 (strikes before auto-disable)
   - COORD_VIOLATION_DISABLE_DURATION=86400 (24h in seconds)
   - COORD_VIOLATION_DECAY=604800 (7 days in seconds)
   - COORD_PERMISSIVE=false (disable gate for testing)

### Total: ~320 LOC, low-medium complexity
### Key design: Silent halts preferred. Disabled agents go dark — no channel message.


## Phase 4 Draft — Coalesce Window (COMPLETE)

**Purpose:** Pre-send buffering window (1.5–3 seconds) that batches rapid-fire messages before channel delivery.

### Why It Exists
- Agents executing parallel actions may produce 2-3 near-identical messages within milliseconds
- FirstResponderHook (Phase 2) handles cross-agent duplication
- Coalesce handles INTRA-agent duplication — same agent, multiple outputs
- Also smooths UX: instead of "think..." + "actually..." + final answer arriving as 3 messages, they merge

### Design: CoalesceBuffer in Channel Adapter Layer

**New file:** `ic/src/channels/coalesce.rs` (~200 LOC)

#### CoalesceBuffer struct
- `pending: HashMap<ChannelKey, PendingMessage>`
- `ChannelKey` = (agent_id, channel_id, conversation_id)
- `PendingMessage` = { content, timestamp, timer, merge_count }
- Uses tokio::time::sleep for the window

#### Behavior
1. Message arrives at channel adapter for delivery
2. Check if pending entry exists for this ChannelKey
3. If YES: merge — append content with separator, reset timer
4. If NO: create new PendingMessage, start timer
5. Timer fires (default 2.0s, configurable COALESCE_WINDOW_SEC):
   - If merge_count > 0: run through TagSchemaHook + 420-char check AGAIN (merged content may exceed limits)
   - Deliver final content to channel
   - Remove from pending map

#### Merge Strategy
- If both messages have same tag prefix (e.g., both STATUS:): concatenate with " | " separator
- If different tags: preserve both with separator, keep first message's tag
- If either exceeds 420 chars after merge: split into two messages, send sequentially
- Strip duplicate content (exact string match within merge)

#### Config
- `COALESCE_WINDOW_SEC` (default: 2.0)
- `COALESCE_MAX_MERGES` (default: 3) — flush immediately if hit
- `COALESCE_ENABLED` (default: true) — can disable per-agent
- Only active on coordination channels (identified by COORD_CHANNEL_IDS config)

#### Integration Point
- Lives in `ic/src/channels/` — AFTER BeforeOutbound hooks, BEFORE channel.send()
- Channel adapter trait gets optional coalesce passthrough
- XMPP adapter: full support (primary target)
- WeeChat adapter: full support
- DarkIRC adapter: full support
- WASM channels: skipped (terminal responses, no streaming)

#### Edge Cases Handled
- **ALERT messages:** bypass coalesce window entirely (priority flush)
- **Thread termination:** flush all pending for that thread before exit
- **Engine shutdown:** flush all pending before graceful shutdown
- **Timer drift:** use tokio Instant, not Duration arithmetic
- **Concurrent merge:** CoalesceBuffer wrapped in Arc<Mutex<...>>

### LOC Breakdown
| Component | LOC |
|-----------|-----|
| coalesce.rs (CoalesceBuffer + merge logic + timer) | ~150 |
| Channel adapter trait extension | ~20 |
| XMPP adapter integration | ~15 |
| WeeChat adapter integration | ~15 |
| Config plumbing | ~10 |
| **Total** | **~210** |

### What Phase 4 Depends On
- Phase 1 (TagSchemaHook for post-merge validation)
- Phase 2 (EngagementPolicyHook — only coalesce messages that passed engagement check)

### What Phase 4 Does NOT Need
- Phase 3 (three-strike) — coalesce runs before violation detection on merged output
- But merged output SHOULD pass through BeforeOutbound hooks again for final validation

### Open Question for Sun
- Should ALERT messages bypass coalesce entirely (recommended), or should they get a shorter window (0.5s)?
- This matters for urgency vs. correctness tradeoff

---
**ALL 4 PHASES NOW DRAFTED.** Full pipeline plan complete.
