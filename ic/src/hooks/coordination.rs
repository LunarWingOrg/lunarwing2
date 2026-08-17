//! Multi-agent team coordination hooks.
//!
//! Implements Phase 2 of the coordination pipeline
//! (`docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`):
//! stateful hooks enforcing a "pager system / ops room" discipline on
//! designated coordination channels (XMPP MUC, IRC):
//!
//! - [`SecretEntropyHook`] (priority 5) — Shannon-entropy secret detection
//! - [`crate::hooks::coordination_knowledge::KnowledgeShareHook`] (priority 13,
//!   optional) — cross-tenant `KNOW:` knowledge-share ingestion
//! - [`EngagementPolicyHook`] (priority 15) — target/elect one responder
//! - [`FirstResponderHook`] (priority 16) — suppress replayed deliveries
//! - [`TagSchemaHook`] (priority 14) — messages must start with an allowed tag
//! - [`RateLimitHook`] (priority 25) — 2 messages/hour per agent
//!
//! All hooks are no-ops outside the configured coordination channels, and
//! nothing is registered unless `COORD_ENABLED=true`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::gate::violation_store::{ViolationStatus, ViolationStore};
use crate::hooks::HookRegistrationSummary;
use crate::hooks::hook::{
    Hook, HookContext, HookError, HookEvent, HookFailureMode, HookOutcome, HookPoint,
};
use crate::hooks::registry::HookRegistry;

/// Default maximum coordination message length (single line).
pub const DEFAULT_MAX_MESSAGE_LENGTH: usize = 420;

/// Default allowed coordination message tags. `KNOW` remains schema-valid on
/// daemons that do not ingest it, so mixed opt-in fleets never strike senders.
pub const DEFAULT_ALLOWED_TAGS: [&str; 6] =
    ["STATUS", "ALERT", "COORD", "ANNOUNCE", "QUERY", "KNOW"];

/// Maximum accepted coordination agent identifier length.
pub const MAX_COORD_AGENT_ID_LEN: usize = 128;

/// Cap on distinct rate-limit keys retained in memory. Real fleets derive
/// keys from configured agent/channel pairs; exceeding this means keys are
/// being rotated, at which point callers share one bucket instead of
/// growing the map. Expired windows are pruned on every call regardless.
pub const MAX_RATE_LIMIT_KEYS: usize = 4_096;

/// Shared coordination agent-ID contract used by hooks and admin routes.
pub fn valid_coordination_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_COORD_AGENT_ID_LEN
        && id.bytes().all(|byte| {
            matches!(byte,
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'
                | b'@' | b'.' | b'-' | b'_' | b'/' | b':' | b'+' | b'~')
        })
}

/// Build shared case-insensitive exact matcher for configured coordination channels.
pub(crate) fn coordination_channel_guard(channels: &[String]) -> String {
    let alternatives = channels
        .iter()
        .map(|channel| regex::escape(channel))
        .collect::<Vec<_>>()
        .join("|");
    format!("(?i)^(?:{alternatives})$")
}

fn push_alias(aliases: &mut Vec<String>, alias: &str) {
    let alias = alias.trim().to_lowercase();
    if !alias.is_empty() && !aliases.iter().any(|existing| existing == &alias) {
        aliases.push(alias);
    }
}

fn normalized_agent_aliases(agent_id: &str, configured: Vec<String>) -> Vec<String> {
    let mut aliases = Vec::new();
    for alias in configured {
        push_alias(&mut aliases, &alias);
    }
    if valid_coordination_agent_id(agent_id) {
        push_alias(&mut aliases, agent_id);
        if !agent_id.contains('@') && !agent_id.contains('/') {
            push_alias(&mut aliases, &format!("@{agent_id}"));
        }
        if let Some(local_name) = agent_id.split(['@', '/']).next() {
            push_alias(&mut aliases, local_name);
            push_alias(&mut aliases, &format!("@{local_name}"));
        }
    }
    aliases
}

fn normalized_responder_roster(configured: Vec<String>) -> Vec<String> {
    if configured
        .iter()
        .any(|agent| !valid_coordination_agent_id(agent))
    {
        tracing::warn!(
            "COORD_RESPONDER_ROSTER contains an invalid agent ID; deterministic election disabled"
        );
        return Vec::new();
    }
    let mut roster = Vec::new();
    for agent in configured {
        if !roster
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&agent))
        {
            roster.push(agent);
        }
    }
    roster.sort_by_key(|agent| agent.to_ascii_lowercase());
    roster
}

fn is_agent_id_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '@' | '.' | '-' | '_' | '/' | ':' | '+' | '~')
}

fn contains_addressed_alias(content: &str, aliases: &[String]) -> bool {
    let content = content.to_lowercase();
    aliases.iter().any(|alias| {
        content.match_indices(alias).any(|(start, matched)| {
            let before = content[..start].chars().next_back();
            let after = content[start + matched.len()..].chars().next();
            before.is_none_or(|character| !is_agent_id_char(character))
                && after.is_none_or(|character| !is_agent_id_char(character))
        })
    })
}

fn addresses_roster_member(content: &str, roster: &[String]) -> bool {
    roster.iter().any(|agent| {
        contains_addressed_alias(content, &normalized_agent_aliases(agent, Vec::new()))
    })
}

fn has_explicit_mention(content: &str) -> bool {
    content.match_indices('@').any(|(start, _)| {
        let before = content[..start].chars().next_back();
        let after = content[start + 1..].chars().next();
        before.is_none_or(|character| !is_agent_id_char(character))
            && after.is_some_and(is_agent_id_char)
    })
}

fn is_coordination_query(content: &str) -> bool {
    content
        .trim_start()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("QUERY:"))
}

fn elected_responder<'a>(correlation_id: &str, roster: &'a [String]) -> Option<&'a str> {
    roster
        .iter()
        .max_by_key(|agent| {
            let mut hasher = Sha256::new();
            hasher.update(correlation_id.as_bytes());
            hasher.update([0]);
            hasher.update(agent.to_ascii_lowercase().as_bytes());
            hasher.finalize().to_vec()
        })
        .map(String::as_str)
}

/// Configuration for the coordination hooks, loaded from environment.
#[derive(Debug, Clone)]
pub struct CoordinationHookConfig {
    /// Master switch (`COORD_ENABLED`).
    pub enabled: bool,
    /// This agent's unique identifier (`COORD_AGENT_ID`).
    pub agent_id: String,
    /// Lowercase @mention aliases that target this agent (`COORD_AGENT_ALIASES`).
    pub agent_aliases: Vec<String>,
    /// Agents eligible for deterministic QUERY election (`COORD_RESPONDER_ROSTER`).
    pub responder_roster: Vec<String>,
    /// Coordination channel JIDs/names (`COORD_CHANNELS`).
    pub coordination_channels: Vec<String>,
    /// Max messages per rate window per agent (`COORD_MAX_MESSAGES`, default 2).
    pub max_messages: usize,
    /// Rate limit window (`COORD_RATE_WINDOW_SECS`, default 3600).
    pub rate_window: Duration,
    /// First-responder claim TTL (`COORD_COALESCE_MS`, default 2000).
    pub coalesce_window: Duration,
    /// Shannon entropy threshold in bits/char (`COORD_ENTROPY_THRESHOLD`, default 4.8).
    pub entropy_threshold: f64,
    /// Minimum token length for entropy checks (`COORD_MIN_SECRET_LENGTH`, default 32).
    pub min_secret_length: usize,
    /// Admin user IDs exempt from engagement policy (`COORD_ADMIN_USERS`).
    pub admin_users: Vec<String>,
    /// Maximum message length enforced by the tag/schema hooks.
    pub max_message_length: usize,
    /// Allowed tag prefixes (without the colon).
    pub allowed_tags: Vec<String>,
    /// Cross-tenant knowledge-share ingestion switch
    /// (`COORD_KNOWLEDGE_SHARE`, default false).
    pub knowledge_share_enabled: bool,
    /// Cross-tenant knowledge-share emission switch
    /// (`COORD_KNOWLEDGE_PUBLISH`, default false).
    ///
    /// Ingesting `KNOW:` records is safe in any fleet: receivers that do not
    /// know the tag still treat it as schema-valid. Emitting is not — a
    /// pre-`KNOW` daemon enforces the original five-tag schema and strikes
    /// the sender. Enabling this asserts the operator has confirmed every
    /// daemon in the coordination room accepts the tag.
    pub knowledge_publish_enabled: bool,
}

impl Default for CoordinationHookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            agent_id: String::new(),
            agent_aliases: Vec::new(),
            responder_roster: Vec::new(),
            coordination_channels: Vec::new(),
            max_messages: 2,
            rate_window: Duration::from_secs(3600),
            coalesce_window: Duration::from_millis(2000),
            entropy_threshold: 4.8,
            min_secret_length: 32,
            admin_users: Vec::new(),
            max_message_length: DEFAULT_MAX_MESSAGE_LENGTH,
            allowed_tags: DEFAULT_ALLOWED_TAGS.iter().map(|s| s.to_string()).collect(),
            knowledge_share_enabled: false,
            knowledge_publish_enabled: false,
        }
    }
}

impl CoordinationHookConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self {
            enabled: env_flag("COORD_ENABLED"),
            knowledge_share_enabled: env_flag("COORD_KNOWLEDGE_SHARE"),
            knowledge_publish_enabled: env_flag("COORD_KNOWLEDGE_PUBLISH"),
            ..Self::default()
        };
        if let Ok(v) = std::env::var("COORD_AGENT_ID") {
            config.agent_id = v.trim().to_string();
        }
        config.agent_aliases =
            normalized_agent_aliases(&config.agent_id, env_list("COORD_AGENT_ALIASES"));
        config.responder_roster = normalized_responder_roster(env_list("COORD_RESPONDER_ROSTER"));
        config.coordination_channels = env_list("COORD_CHANNELS");
        config.admin_users = env_list("COORD_ADMIN_USERS");
        if let Some(v) = env_parse::<usize>("COORD_MAX_MESSAGES").filter(|value| *value > 0) {
            config.max_messages = v;
        }
        if let Some(v) = env_parse::<u64>("COORD_RATE_WINDOW_SECS").filter(|value| *value > 0) {
            config.rate_window = Duration::from_secs(v);
        }
        if let Some(v) = env_parse::<u64>("COORD_COALESCE_MS").filter(|value| *value > 0) {
            config.coalesce_window = Duration::from_millis(v);
        }
        if let Some(v) = env_parse::<f64>("COORD_ENTROPY_THRESHOLD")
            .filter(|value| value.is_finite() && *value > 0.0 && *value <= 8.0)
        {
            config.entropy_threshold = v;
        }
        if let Some(v) = env_parse::<usize>("COORD_MIN_SECRET_LENGTH").filter(|value| *value > 0) {
            config.min_secret_length = v;
        }
        config
    }

    /// Whether the config is usable (enabled and minimally configured).
    pub fn is_actionable(&self) -> bool {
        self.enabled
            && valid_coordination_agent_id(&self.agent_id)
            && !self.coordination_channels.is_empty()
    }

    fn effective_allowed_tags(&self) -> Vec<String> {
        let mut tags = self.allowed_tags.clone();
        if !tags.iter().any(|tag| {
            tag.eq_ignore_ascii_case(crate::hooks::coordination_knowledge::KNOWLEDGE_SHARE_TAG)
        }) {
            tags.push(crate::hooks::coordination_knowledge::KNOWLEDGE_SHARE_TAG.to_string());
        }
        tags
    }

    /// Tags the model may emit. `KNOW` appears only when the operator has
    /// explicitly enabled emission (`COORD_KNOWLEDGE_PUBLISH`): mixed-version
    /// rooms can still strike pre-`KNOW` senders, so sharing (ingestion)
    /// alone must not teach the model the wire format.
    fn prompt_allowed_tags(&self) -> Vec<String> {
        let mut tags = self.allowed_tags.clone();
        tags.retain(|tag| {
            self.knowledge_publish_enabled
                || !tag
                    .eq_ignore_ascii_case(crate::hooks::coordination_knowledge::KNOWLEDGE_SHARE_TAG)
        });
        if self.knowledge_publish_enabled
            && !tags.iter().any(|tag| {
                tag.eq_ignore_ascii_case(crate::hooks::coordination_knowledge::KNOWLEDGE_SHARE_TAG)
            })
        {
            tags.push(crate::hooks::coordination_knowledge::KNOWLEDGE_SHARE_TAG.to_string());
        }
        tags
    }
}

/// Per-turn system instruction for model responses in coordination channels.
#[derive(Debug, Clone)]
pub struct CoordinationPromptPolicy {
    channel_matcher: regex::Regex,
    instruction: Arc<str>,
}

impl CoordinationPromptPolicy {
    /// Build a prompt policy when coordination hooks are actionable.
    pub fn from_config(config: &CoordinationHookConfig) -> Option<Self> {
        if !config.is_actionable() {
            return None;
        }
        let channel_matcher =
            regex::Regex::new(&coordination_channel_guard(&config.coordination_channels)).ok()?;
        let allowed_tags = config.prompt_allowed_tags();
        let knowledge_instruction = config.knowledge_publish_enabled.then_some(
            " Share durable codebase findings with sibling tenants as \
             'KNOW: <topic-slug> | <one-line finding>' (topic slug: lowercase, digits, dashes).",
        );
        let instruction = format!(
            "Coordination-channel protocol overrides persona and style instructions for this turn. \
             Final user-visible response (inside <final> when required) must be one line, at most {} \
             characters, and begin immediately with one of these tags followed by a colon: {}. \
             Put no preamble before the tag. Use concise operational prose only; no markdown or roleplay.{}",
            config.max_message_length,
            allowed_tags.join(", "),
            knowledge_instruction.unwrap_or_default()
        );
        Some(Self {
            channel_matcher,
            instruction: instruction.into(),
        })
    }

    /// Return model instruction only for an exact configured destination.
    pub fn instruction_for_scope(&self, scope: &str) -> Option<&str> {
        self.channel_matcher
            .is_match(scope)
            .then_some(self.instruction.as_ref())
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Outcome of a first-responder claim attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimResult {
    /// This agent claimed the message first.
    Claimed,
    /// Another agent already claimed it.
    AlreadyClaimed(String),
}

#[derive(Debug, Default)]
struct CoordinationStateInner {
    /// Rate limit timestamps keyed by `agent:channel`.
    rate_limit_windows: HashMap<String, Vec<std::time::Instant>>,
    /// First-responder claims: correlation id → (agent id, claim time).
    responder_claims: HashMap<String, (String, std::time::Instant)>,
}

/// Shared state for the coordination hooks.
#[derive(Debug, Clone)]
pub struct CoordinationState {
    inner: Arc<Mutex<CoordinationStateInner>>,
    channel_matcher: Arc<Option<regex::Regex>>,
    admins: Arc<Vec<String>>,
    violations: Option<Arc<ViolationStore>>,
}

impl CoordinationState {
    /// Create shared state for the given coordination channels and admins.
    pub fn new(
        coordination_channels: Vec<String>,
        admin_users: Vec<String>,
        violations: Option<Arc<ViolationStore>>,
    ) -> Self {
        let channel_matcher = if coordination_channels.is_empty() {
            None
        } else {
            let guard = coordination_channel_guard(&coordination_channels);
            match regex::Regex::new(&guard) {
                Ok(matcher) => Some(matcher),
                Err(error) => {
                    tracing::warn!(%error, "Invalid coordination channel matcher; hooks disabled");
                    None
                }
            }
        };
        Self {
            inner: Arc::new(Mutex::new(CoordinationStateInner::default())),
            channel_matcher: Arc::new(channel_matcher),
            admins: Arc::new(admin_users),
            violations,
        }
    }

    /// Case-insensitive exact match against a configured channel JID/name.
    pub async fn is_coordination_channel(&self, channel: &str) -> bool {
        self.channel_matcher
            .as_ref()
            .as_ref()
            .is_some_and(|matcher| matcher.is_match(channel))
    }

    /// Whether a user id is a configured admin.
    pub fn is_admin(&self, user_id: &str) -> bool {
        self.admins.iter().any(|a| a == user_id)
    }

    /// Sliding-window rate limit. Records the attempt and returns whether it
    /// is under the limit.
    ///
    /// Keys are caller-derived, so the map is defensively bounded: expired
    /// windows are dropped on every call and a pathological number of
    /// distinct keys tips the whole map into a shared-budget mode rather
    /// than growing without bound.
    pub async fn try_consume_rate_limit(
        &self,
        key: &str,
        max_messages: usize,
        window: Duration,
    ) -> bool {
        let mut guard = self.inner.lock().await;
        let now = std::time::Instant::now();
        guard.rate_limit_windows.retain(|_, stamps| {
            stamps.retain(|t| now.duration_since(*t) < window);
            !stamps.is_empty()
        });
        let saturated = guard.rate_limit_windows.len() >= MAX_RATE_LIMIT_KEYS;
        // Too many distinct keys for per-key isolation: collapse the callers
        // into one shared bucket (sized to the original budget so a
        // rotating-key client cannot buy fresh allowances). Configured
        // fleets keep per-key behavior.
        let (effective_key, budget) = if saturated {
            (String::new(), max_messages)
        } else {
            (key.to_string(), max_messages)
        };
        let stamps = guard.rate_limit_windows.entry(effective_key).or_default();
        stamps.retain(|t| now.duration_since(*t) < window);
        if stamps.len() >= budget {
            return false;
        }
        stamps.push(now);
        true
    }

    /// Try to claim a correlation id for first-responder correlation.
    /// Claims expire after `ttl`.
    pub async fn try_claim(
        &self,
        correlation_id: &str,
        agent_id: &str,
        ttl: Duration,
    ) -> ClaimResult {
        let mut guard = self.inner.lock().await;
        let now = std::time::Instant::now();
        guard
            .responder_claims
            .retain(|_, (_, at)| now.duration_since(*at) < ttl);
        match guard.responder_claims.get(correlation_id) {
            Some((claimer, _)) => ClaimResult::AlreadyClaimed(claimer.clone()),
            None => {
                guard
                    .responder_claims
                    .insert(correlation_id.to_string(), (agent_id.to_string(), now));
                ClaimResult::Claimed
            }
        }
    }

    /// Record a coordination violation against an agent. When a violation
    /// store is attached this persists and escalates; strike 2 logs an ops
    /// alert, strike 3+ auto-disables.
    pub async fn record_violation(&self, agent: &str, reason: &str, hook: &str, channel: &str) {
        if let Some(store) = &self.violations {
            match store.record_violation(agent, reason, hook, channel) {
                ViolationStatus::FinalWarning => {
                    tracing::warn!(
                        agent = agent,
                        hook = hook,
                        channel = channel,
                        reason = reason,
                        "coordination ops alert: agent at final warning (strike 2)"
                    );
                }
                ViolationStatus::Disabled { until_secs } => {
                    tracing::warn!(
                        agent = agent,
                        hook = hook,
                        until_secs = until_secs,
                        "coordination: agent auto-disabled after 3 strikes"
                    );
                }
                _ => {}
            }
        } else {
            tracing::debug!(
                agent = agent,
                hook = hook,
                reason = reason,
                "coordination violation (no violation store attached)"
            );
        }
    }

    /// Whether the agent is currently auto-disabled (requires a store).
    pub fn is_disabled(&self, agent: &str) -> bool {
        self.violations
            .as_ref()
            .is_some_and(|store| store.is_disabled(agent))
    }
}

/// Shannon entropy of a string in bits per character.
pub fn shannon_entropy(s: &str) -> f64 {
    let mut freq: HashMap<char, usize> = HashMap::new();
    let len = s.chars().count();
    if len == 0 {
        return 0.0;
    }
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

static EXPLICIT_SECRET_PATTERNS: LazyLock<Result<Vec<(&'static str, regex::Regex)>, String>> =
    LazyLock::new(|| {
        [
            (
                "credential assignment",
                r#"(?i)(api[_-]?key|secret|token|password|bearer|authorization)\s*[:=]\s*['"]?[A-Za-z0-9+/=_-]{16,}"#,
            ),
            (
                "private key material",
                r"-----BEGIN\s+(RSA\s+|EC\s+|OPENSSH\s+|PGP\s+)?PRIVATE KEY-----",
            ),
            (
                "JWT token",
                r"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
            ),
        ]
        .into_iter()
        .map(|(kind, pattern)| {
            regex::Regex::new(pattern)
                .map(|compiled| (kind, compiled))
                .map_err(|error| error.to_string())
        })
        .collect()
    });

fn explicit_secret_kind(content: &str) -> Result<Option<&'static str>, HookError> {
    let patterns =
        EXPLICIT_SECRET_PATTERNS
            .as_ref()
            .map_err(|error| HookError::ExecutionFailed {
                reason: format!("invalid built-in secret pattern: {error}"),
            })?;
    Ok(patterns
        .iter()
        .find_map(|(kind, pattern)| pattern.is_match(content).then_some(*kind)))
}

/// Known-safe token shapes that should not trip entropy detection: exact git
/// SHAs, explicit file paths, and absolute HTTP(S) URLs.
fn is_allowlisted_token(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ','
        )
    });
    if token.len() == 40 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    if token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
    {
        return true;
    }
    url::Url::parse(token).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// Extract (actor, policy scope, content, is_local_output) from scoped events.
fn event_content<'a>(
    event: &'a HookEvent,
    local_agent: &'a str,
) -> Option<(&'a str, &'a str, &'a str, bool)> {
    let scope = event.policy_scope()?;
    match event {
        HookEvent::Inbound {
            user_id,
            sender_id,
            content,
            ..
        } => Some((
            sender_id.as_deref().unwrap_or(user_id),
            scope,
            content,
            false,
        )),
        HookEvent::Outbound { content, .. } => Some((local_agent, scope, content, true)),
        HookEvent::ResponseTransform { response, .. } => Some((local_agent, scope, response, true)),
        _ => None,
    }
}

/// Priority 5 — strike-aware explicit and high-entropy secret detection.
pub struct SecretEntropyHook {
    state: CoordinationState,
    /// This agent's own identifier (`COORD_AGENT_ID`). Outbound violations
    /// are attributed to it: the event `user_id` is the conversation peer,
    /// not the emitter (ISSUE-004).
    agent_id: String,
    entropy_threshold: f64,
    min_length: usize,
}

impl SecretEntropyHook {
    /// Create with the given shared state, local agent id, and tunables.
    pub fn new(
        state: CoordinationState,
        agent_id: String,
        entropy_threshold: f64,
        min_length: usize,
    ) -> Self {
        Self {
            state,
            agent_id,
            entropy_threshold,
            min_length,
        }
    }
}

#[async_trait]
impl Hook for SecretEntropyHook {
    fn name(&self) -> &str {
        "coordination.secret_entropy"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[
            HookPoint::BeforeInbound,
            HookPoint::BeforeOutbound,
            HookPoint::TransformResponse,
        ]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailClosed
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let Some((actor, scope, content, _is_local_output)) = event_content(event, &self.agent_id)
        else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        if let Some(kind) = explicit_secret_kind(content)? {
            self.state
                .record_violation(actor, kind, self.name(), scope)
                .await;
            return Ok(HookOutcome::reject(format!("coord: {kind} detected")));
        }
        for token in content.split_whitespace() {
            if token.len() >= self.min_length
                && !is_allowlisted_token(token)
                && shannon_entropy(token) >= self.entropy_threshold
            {
                self.state
                    .record_violation(actor, "high-entropy token", self.name(), scope)
                    .await;
                return Ok(HookOutcome::reject(
                    "coord: potential secret detected (high entropy)",
                ));
            }
        }
        Ok(HookOutcome::ok())
    }
}

/// Priority 15 — agents only engage when @mentioned, queried, or admin-sent.
pub struct EngagementPolicyHook {
    state: CoordinationState,
    agent_id: String,
    agent_aliases: Vec<String>,
    responder_roster: Vec<String>,
}

impl EngagementPolicyHook {
    /// Create with shared state, this agent's id, and its mention aliases.
    pub fn new(state: CoordinationState, agent_id: String, agent_aliases: Vec<String>) -> Self {
        Self::new_with_roster(state, agent_id, agent_aliases, Vec::new())
    }

    /// Create with a team roster for deterministic unaddressed QUERY election.
    pub fn new_with_roster(
        state: CoordinationState,
        agent_id: String,
        agent_aliases: Vec<String>,
        responder_roster: Vec<String>,
    ) -> Self {
        Self {
            state,
            agent_id,
            agent_aliases,
            responder_roster,
        }
    }

    fn is_targeted(&self, content: &str) -> bool {
        contains_addressed_alias(content, &self.agent_aliases)
    }
}

#[async_trait]
impl Hook for EngagementPolicyHook {
    fn name(&self) -> &str {
        "coordination.engagement_policy"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::BeforeInbound]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailOpen
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let HookEvent::Inbound {
            user_id,
            sender_id,
            content,
            ..
        } = event
        else {
            return Ok(HookOutcome::ok());
        };
        let Some(scope) = event.policy_scope() else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        let actor = sender_id.as_deref().unwrap_or(user_id);
        // Messages from this agent or from admins always pass.
        if actor == self.agent_id || self.state.is_admin(actor) {
            return Ok(HookOutcome::ok());
        }
        if self.is_targeted(content) {
            return Ok(HookOutcome::ok());
        }
        if addresses_roster_member(content, &self.responder_roster) || has_explicit_mention(content)
        {
            return Ok(HookOutcome::reject(
                "coord: query explicitly targets another agent",
            ));
        }
        if is_coordination_query(content)
            && let Some(elected) = elected_responder(
                &correlation_id(scope, actor, content),
                &self.responder_roster,
            )
        {
            return if elected.eq_ignore_ascii_case(&self.agent_id) {
                Ok(HookOutcome::ok())
            } else {
                Ok(HookOutcome::reject(
                    "coord: another agent elected for query",
                ))
            };
        }
        // Silent pass: message wasn't directed at this agent. The reject keeps
        // it out of the conversation so the agent loop exits without generating
        // a response (Option B from the integration plan).
        Ok(HookOutcome::reject("coord: not targeted at this agent"))
    }
}

/// Priority 16 — suppresses replayed inbound deliveries inside one daemon.
///
/// Cross-daemon first-responder selection for unaddressed `QUERY:` traffic is
/// deterministic in [`EngagementPolicyHook`] when `COORD_RESPONDER_ROSTER` is
/// configured. This hook is the second layer: it prevents one selected daemon
/// from processing the same inbound delivery more than once during the TTL.
pub struct FirstResponderHook {
    state: CoordinationState,
    agent_id: String,
    claim_ttl: Duration,
}

impl FirstResponderHook {
    /// Create with shared state, this agent's id, and the claim TTL.
    pub fn new(state: CoordinationState, agent_id: String, claim_ttl: Duration) -> Self {
        Self {
            state,
            agent_id,
            claim_ttl,
        }
    }
}

/// Stable correlation ID shared by independent daemon builds and processes.
fn correlation_id(channel: &str, sender: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(channel.to_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(sender.to_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(content.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[async_trait]
impl Hook for FirstResponderHook {
    fn name(&self) -> &str {
        "coordination.first_responder"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::BeforeInbound]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailOpen
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let HookEvent::Inbound {
            user_id,
            sender_id,
            content,
            ..
        } = event
        else {
            return Ok(HookOutcome::ok());
        };
        let Some(scope) = event.policy_scope() else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        let sender = sender_id.as_deref().unwrap_or(user_id);
        let id = correlation_id(scope, sender, content);
        match self
            .state
            .try_claim(&id, &self.agent_id, self.claim_ttl)
            .await
        {
            ClaimResult::Claimed => Ok(HookOutcome::ok()),
            ClaimResult::AlreadyClaimed(other) => {
                tracing::debug!(
                    agent = %self.agent_id,
                    claimed_by = %other,
                    "coordination: replayed message already processed"
                );
                Ok(HookOutcome::reject(
                    "coord: replayed message already processed",
                ))
            }
        }
    }
}

/// Priority 14 — messages in coordination channels must start with an
/// allowed tag, be single-line, and fit the length cap.
pub struct TagSchemaHook {
    state: CoordinationState,
    agent_id: String,
    allowed_tags: Vec<String>,
    max_message_length: usize,
    knowledge_publish_enabled: bool,
}

impl TagSchemaHook {
    /// Create with shared state, this agent's id, and the schema limits.
    pub fn new(
        state: CoordinationState,
        agent_id: String,
        allowed_tags: Vec<String>,
        max_message_length: usize,
    ) -> Self {
        Self {
            state,
            agent_id,
            allowed_tags,
            max_message_length,
            knowledge_publish_enabled: false,
        }
    }

    /// Allow outbound `KNOW:` records after fleet compatibility is confirmed.
    pub fn with_knowledge_publish_enabled(mut self, enabled: bool) -> Self {
        self.knowledge_publish_enabled = enabled;
        self
    }

    /// Actor, policy scope, and content for schema-scoped events.
    /// Response transforms are attributed to the local emitting agent.
    fn event_parts<'a>(&'a self, event: &'a HookEvent) -> Option<(&'a str, &'a str, &'a str)> {
        match event {
            HookEvent::Inbound {
                user_id,
                sender_id,
                content,
                ..
            } => Some((
                sender_id.as_deref().unwrap_or(user_id),
                event.policy_scope()?,
                content,
            )),
            HookEvent::Outbound { content, .. } => {
                Some((&self.agent_id, event.policy_scope()?, content))
            }
            HookEvent::ResponseTransform { response, .. } => {
                Some((&self.agent_id, event.policy_scope()?, response))
            }
            _ => None,
        }
    }

    fn starts_with_tag(&self, content: &str) -> bool {
        let trimmed = content.trim_start();
        self.allowed_tags.iter().any(|tag| {
            match trimmed.get(..tag.len()) {
                // get() returns None when the boundary splits a multi-byte
                // char, so non-ASCII prefixes can't panic here.
                Some(prefix) => {
                    prefix.eq_ignore_ascii_case(tag)
                        && trimmed.as_bytes().get(tag.len()) == Some(&b':')
                }
                None => false,
            }
        })
    }

    fn is_knowledge_record(content: &str) -> bool {
        let trimmed = content.trim_start();
        let tag = crate::hooks::coordination_knowledge::KNOWLEDGE_SHARE_TAG;
        trimmed
            .get(..tag.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(tag))
            && trimmed.as_bytes().get(tag.len()) == Some(&b':')
    }
}

#[async_trait]
impl Hook for TagSchemaHook {
    fn name(&self) -> &str {
        "coordination.tag_schema"
    }

    fn hook_points(&self) -> &[HookPoint] {
        // Inbound validation prevents malformed room traffic from waking the
        // model. Schema validation stays at TransformResponse so malformed
        // output consumes one local-agent strike (ISSUE-004). BeforeOutbound
        // only enforces the non-striking KNOW publish gate as a final safety
        // net for callers that bypass response transforms.
        &[
            HookPoint::BeforeInbound,
            HookPoint::BeforeOutbound,
            HookPoint::TransformResponse,
        ]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailClosed
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let Some((agent, channel, content)) = self.event_parts(event) else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(channel).await {
            return Ok(HookOutcome::ok());
        }
        if !matches!(event, HookEvent::Inbound { .. })
            && !self.knowledge_publish_enabled
            && Self::is_knowledge_record(content)
        {
            tracing::warn!(
                agent,
                channel,
                "outbound coordination knowledge share blocked: publishing is disabled"
            );
            return Ok(HookOutcome::reject(
                "coord: KNOW publishing is disabled; set COORD_KNOWLEDGE_PUBLISH=true only after confirming fleet compatibility",
            ));
        }
        if matches!(event, HookEvent::Outbound { .. }) {
            return Ok(HookOutcome::ok());
        }
        if content.contains('\n') || content.contains('\r') {
            self.state
                .record_violation(agent, "multi-line message", self.name(), channel)
                .await;
            return Ok(HookOutcome::reject(
                "coord: messages in coordination channels must be single-line",
            ));
        }
        if content.chars().count() > self.max_message_length {
            self.state
                .record_violation(agent, "message too long", self.name(), channel)
                .await;
            return Ok(HookOutcome::reject(format!(
                "coord: messages in coordination channels must not exceed {} characters",
                self.max_message_length
            )));
        }
        if !self.starts_with_tag(content) {
            self.state
                .record_violation(agent, "missing tag", self.name(), channel)
                .await;
            return Ok(HookOutcome::reject(format!(
                "coord: messages in coordination channels must start with one of: {}",
                self.allowed_tags.join("/")
            )));
        }
        Ok(HookOutcome::ok())
    }
}

/// Priority 25 — sliding-window rate limit per agent per coordination
/// channel. Also refuses to send while this agent is auto-disabled.
pub struct RateLimitHook {
    state: CoordinationState,
    agent_id: String,
    max_messages: usize,
    window: Duration,
}

impl RateLimitHook {
    /// Create with shared state, this agent's id, and the window tunables.
    pub fn new(
        state: CoordinationState,
        agent_id: String,
        max_messages: usize,
        window: Duration,
    ) -> Self {
        Self {
            state,
            agent_id,
            max_messages,
            window,
        }
    }
}

#[async_trait]
impl Hook for RateLimitHook {
    fn name(&self) -> &str {
        "coordination.rate_limit"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::BeforeOutbound]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailClosed
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let HookEvent::Outbound { .. } = event else {
            return Ok(HookOutcome::ok());
        };
        let Some(scope) = event.policy_scope() else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        if self.state.is_disabled(&self.agent_id) {
            return Ok(HookOutcome::reject(
                "coord: agent disabled due to coordination violations",
            ));
        }
        let key = format!("{}:{}", self.agent_id, scope.to_lowercase());
        if !self
            .state
            .try_consume_rate_limit(&key, self.max_messages, self.window)
            .await
        {
            let agent = self.agent_id.clone();
            self.state
                .record_violation(&agent, "rate limit exceeded", self.name(), scope)
                .await;
            return Ok(HookOutcome::reject(format!(
                "coord: rate limit exceeded ({} messages per {}s in coordination channels)",
                self.max_messages,
                self.window.as_secs()
            )));
        }
        Ok(HookOutcome::ok())
    }
}

/// Register all coordination hooks from the given configuration.
///
/// This compatibility entry point registers the original five hooks. Use
/// [`register_coordination_hooks_with_knowledge`] when a database-backed
/// shared-scope workspace is available.
pub async fn register_coordination_hooks(
    registry: &Arc<HookRegistry>,
    config: CoordinationHookConfig,
    violations: Option<Arc<ViolationStore>>,
) -> HookRegistrationSummary {
    register_coordination_hooks_with_knowledge(registry, config, violations, None).await
}

/// Register coordination hooks, optionally including cross-tenant knowledge
/// ingestion into the supplied shared-scope workspace.
pub async fn register_coordination_hooks_with_knowledge(
    registry: &Arc<HookRegistry>,
    config: CoordinationHookConfig,
    violations: Option<Arc<ViolationStore>>,
    knowledge_workspace: Option<Arc<crate::workspace::Workspace>>,
) -> HookRegistrationSummary {
    let mut summary = HookRegistrationSummary::default();
    if !config.is_actionable() {
        if config.enabled {
            if !valid_coordination_agent_id(&config.agent_id) {
                tracing::warn!(
                    "COORD_ENABLED=true but COORD_AGENT_ID is missing or invalid; \
                     coordination hooks not registered"
                );
            } else {
                tracing::warn!(
                    "COORD_ENABLED=true but COORD_CHANNELS is missing; \
                     coordination hooks not registered"
                );
            }
            summary.errors += 1;
        }
        return summary;
    }

    let state = CoordinationState::new(
        config.coordination_channels.clone(),
        config.admin_users.clone(),
        violations,
    );
    let allowed_tags = config.effective_allowed_tags();
    let mut hooks: Vec<(Arc<dyn Hook>, u32)> = vec![
        (
            Arc::new(SecretEntropyHook::new(
                state.clone(),
                config.agent_id.clone(),
                config.entropy_threshold,
                config.min_secret_length,
            )),
            5,
        ),
        (
            Arc::new(EngagementPolicyHook::new_with_roster(
                state.clone(),
                config.agent_id.clone(),
                config.agent_aliases.clone(),
                config.responder_roster.clone(),
            )),
            15,
        ),
        (
            Arc::new(FirstResponderHook::new(
                state.clone(),
                config.agent_id.clone(),
                config.coalesce_window,
            )),
            16,
        ),
        (
            Arc::new(
                TagSchemaHook::new(
                    state.clone(),
                    config.agent_id.clone(),
                    allowed_tags,
                    config.max_message_length,
                )
                .with_knowledge_publish_enabled(config.knowledge_publish_enabled),
            ),
            14,
        ),
        (
            Arc::new(RateLimitHook::new(
                state.clone(),
                config.agent_id.clone(),
                config.max_messages,
                config.rate_window,
            )),
            25,
        ),
    ];

    if config.knowledge_share_enabled {
        match knowledge_workspace {
            Some(workspace) => hooks.push((
                Arc::new(
                    crate::hooks::coordination_knowledge::KnowledgeShareHook::new(
                        state,
                        workspace,
                        crate::hooks::coordination_knowledge::KnowledgeShareLimits::from_env(),
                    )
                    .with_max_message_chars(config.max_message_length),
                ),
                13,
            )),
            None => {
                tracing::warn!(
                    "COORD_KNOWLEDGE_SHARE=true but no database-backed shared workspace is \
                     available; coordination knowledge ingestion disabled"
                );
                summary.errors += 1;
            }
        }
    }

    for (hook, priority) in hooks {
        registry.register_with_priority(hook, priority).await;
        summary.hooks += 1;
    }
    tracing::info!(
        agent_id = %config.agent_id,
        channels = ?config.coordination_channels,
        knowledge_share = config.knowledge_share_enabled && summary.errors == 0,
        "Coordination hooks registered"
    );
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordination_agent_id_contract_matches_admin_paths() {
        assert!(valid_coordination_agent_id("agent-1"));
        assert!(valid_coordination_agent_id(
            "luna@conference.example/resource"
        ));
        assert!(valid_coordination_agent_id(
            &"x".repeat(MAX_COORD_AGENT_ID_LEN)
        ));
        assert!(!valid_coordination_agent_id(""));
        assert!(!valid_coordination_agent_id(
            &"x".repeat(MAX_COORD_AGENT_ID_LEN + 1)
        ));
        for invalid in ["bad id", "bad%20id", "bad\nid", "bad?id", "üni"] {
            assert!(!valid_coordination_agent_id(invalid));
        }
    }

    #[test]
    fn agent_id_supplies_safe_default_engagement_aliases() {
        assert_eq!(
            normalized_agent_aliases("Applejack", Vec::new()),
            vec!["applejack", "@applejack"]
        );
        assert_eq!(
            normalized_agent_aliases(
                "Applejack@example.test/resource",
                vec!["@AJ".to_string(), "@aj".to_string()]
            ),
            vec![
                "@aj",
                "applejack@example.test/resource",
                "applejack",
                "@applejack"
            ]
        );
    }

    #[test]
    fn responder_roster_is_case_insensitive_and_fails_closed_on_invalid_ids() {
        assert_eq!(
            normalized_responder_roster(vec![
                "Rarity".to_string(),
                "Applejack".to_string(),
                "applejack".to_string()
            ]),
            vec!["Applejack", "Rarity"]
        );
        assert!(
            normalized_responder_roster(vec!["Applejack".to_string(), "bad id".to_string()])
                .is_empty()
        );
    }

    #[test]
    fn engagement_aliases_match_identifiers_not_substrings() {
        let aliases = vec!["aj".to_string(), "@applejack".to_string()];
        assert!(contains_addressed_alias("QUERY: aj status", &aliases));
        assert!(contains_addressed_alias(
            "QUERY: @Applejack status",
            &aliases
        ));
        assert!(!contains_addressed_alias("QUERY: adjust status", &aliases));
        assert!(!contains_addressed_alias(
            "QUERY: @applejack2 status",
            &aliases
        ));
        assert!(has_explicit_mention("QUERY: @unknown status"));
        assert!(!has_explicit_mention("QUERY: mail me@example.test"));
        assert!(!has_explicit_mention("QUERY: status"));
    }

    #[test]
    fn independent_daemons_elect_the_same_responder() {
        let roster_a = normalized_responder_roster(vec!["Rarity".into(), "Applejack".into()]);
        let roster_b = normalized_responder_roster(vec!["applejack".into(), "rarity".into()]);
        let id = correlation_id(
            "COORD@conference.example",
            "operator@example.test",
            "QUERY: report status",
        );
        let elected_a = elected_responder(&id, &roster_a).map(str::to_ascii_lowercase);
        let elected_b = elected_responder(&id, &roster_b).map(str::to_ascii_lowercase);
        assert_eq!(elected_a, elected_b);
        assert!(matches!(elected_a.as_deref(), Some("applejack" | "rarity")));
    }

    fn test_config() -> CoordinationHookConfig {
        CoordinationHookConfig {
            enabled: true,
            agent_id: "agent-1".to_string(),
            agent_aliases: vec!["@agent1".to_string(), "agent1".to_string()],
            responder_roster: Vec::new(),
            coordination_channels: vec!["coord@conference.example".to_string()],
            max_messages: 2,
            rate_window: Duration::from_secs(3600),
            coalesce_window: Duration::from_millis(2000),
            entropy_threshold: 4.8,
            min_secret_length: 32,
            admin_users: vec!["sun".to_string()],
            max_message_length: DEFAULT_MAX_MESSAGE_LENGTH,
            allowed_tags: DEFAULT_ALLOWED_TAGS.iter().map(|s| s.to_string()).collect(),
            knowledge_share_enabled: false,
            knowledge_publish_enabled: false,
        }
    }

    fn test_state() -> CoordinationState {
        let config = test_config();
        CoordinationState::new(config.coordination_channels, config.admin_users, None)
    }

    fn inbound(user: &str, channel: &str, content: &str) -> HookEvent {
        HookEvent::Inbound {
            user_id: user.to_string(),
            sender_id: Some(user.to_string()),
            channel: channel.to_string(),
            content: content.to_string(),
            thread_id: None,
            policy_scope: None,
        }
    }

    fn transformed(channel: &str, content: &str) -> HookEvent {
        HookEvent::ResponseTransform {
            user_id: "remote-peer".to_string(),
            thread_id: "t-1".to_string(),
            response: content.to_string(),
            channel: Some(channel.to_string()),
            policy_scope: None,
        }
    }

    fn outbound(user: &str, channel: &str, content: &str) -> HookEvent {
        HookEvent::Outbound {
            user_id: user.to_string(),
            channel: channel.to_string(),
            content: content.to_string(),
            thread_id: None,
            policy_scope: None,
        }
    }

    fn ctx() -> HookContext {
        HookContext::default()
    }

    #[test]
    fn test_shannon_entropy() {
        assert!(shannon_entropy("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") < 1.0);
        assert!(shannon_entropy("A9f3Kx82LmQpZ7wT1vBnR4yUeC6sGhJ0") > 4.8);
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn test_allowlisted_tokens() {
        assert!(is_allowlisted_token("/usr/local/bin/some-long-path-token"));
        assert!(is_allowlisted_token("../relative/file-name"));
        assert!(is_allowlisted_token(
            "https://example.test/releases/long-artifact-name"
        ));
        assert!(is_allowlisted_token(
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        ));
        assert!(!is_allowlisted_token(
            "prefix/A9f3Kx82LmQpZ7wT1vBnR4yUeC6sGhJ0"
        ));
        assert!(!is_allowlisted_token("A9f3Kx82LmQpZ7wT1vBnR4yUeC6sGhJ0"));
    }

    #[test]
    fn explicit_secret_patterns_cover_credentials_keys_and_jwts() {
        assert_eq!(
            explicit_secret_kind("STATUS: api_key=abcdefghijklmnop").unwrap(),
            Some("credential assignment")
        );
        assert_eq!(
            explicit_secret_kind("-----BEGIN PRIVATE KEY-----").unwrap(),
            Some("private key material")
        );
        assert_eq!(
            explicit_secret_kind("STATUS: eyJaaaaaaaaaa.eyJbbbbbbbbbb.cccccccccccc").unwrap(),
            Some("JWT token")
        );
        assert_eq!(explicit_secret_kind("STATUS: safe").unwrap(), None);
    }

    #[tokio::test]
    async fn test_state_channel_matching() {
        let state = test_state();
        assert!(
            state
                .is_coordination_channel("coord@conference.example")
                .await
        );
        assert!(
            state
                .is_coordination_channel("COORD@conference.example")
                .await
        );
        assert!(
            !state
                .is_coordination_channel("xmpp:coord@conference.example/group")
                .await
        );
        assert!(
            !state
                .is_coordination_channel("other-coord@conference.example")
                .await
        );
        assert!(!state.is_coordination_channel("random-channel").await);
        let unicode_state =
            CoordinationState::new(vec!["Ä-team@conference.example".into()], Vec::new(), None);
        assert!(
            unicode_state
                .is_coordination_channel("ä-team@conference.example")
                .await
        );
        assert!(state.is_admin("sun"));
        assert!(!state.is_admin("agent-2"));
    }

    #[test]
    fn coordination_prompt_policy_is_exactly_channel_scoped() {
        let policy = CoordinationPromptPolicy::from_config(&test_config())
            .expect("actionable coordination config");
        let instruction = policy
            .instruction_for_scope("COORD@conference.example")
            .expect("case-insensitive coordination channel match");
        assert!(instruction.contains("STATUS, ALERT, COORD, ANNOUNCE, QUERY"));
        assert!(!instruction.contains("QUERY, KNOW"));
        assert!(!instruction.contains("KNOW: <topic-slug>"));
        assert!(instruction.contains("420 characters"));
        assert!(
            policy
                .instruction_for_scope("other@conference.example")
                .is_none()
        );
    }

    #[test]
    fn knowledge_share_ingests_without_emitting_until_publish_opt_in() {
        let mut config = test_config();
        config.allowed_tags = vec!["STATUS".to_string()];
        // Ingestion alone: schema keeps KNOW valid for mixed fleets, but the
        // prompt must not advertise emission.
        config.knowledge_share_enabled = true;
        assert_eq!(
            config.effective_allowed_tags(),
            vec!["STATUS".to_string(), "KNOW".to_string()]
        );
        let policy = CoordinationPromptPolicy::from_config(&config).expect("actionable config");
        let instruction = policy
            .instruction_for_scope("coord@conference.example")
            .expect("configured room");
        assert!(!instruction.contains("KNOW"));
        assert!(!instruction.contains("followed by a colon: STATUS, KNOW"));

        // Emission requires the explicit fleet-readiness opt-in.
        config.knowledge_publish_enabled = true;
        let policy = CoordinationPromptPolicy::from_config(&config).expect("actionable config");
        let instruction = policy
            .instruction_for_scope("coord@conference.example")
            .expect("configured room");
        assert!(instruction.contains("followed by a colon: STATUS, KNOW"));
        assert!(instruction.contains("KNOW: <topic-slug> | <one-line finding>"));
    }

    #[tokio::test]
    async fn test_rate_limit_allows_under_limit() {
        let state = test_state();
        assert!(
            state
                .try_consume_rate_limit("a:c", 2, Duration::from_secs(60))
                .await
        );
        assert!(
            state
                .try_consume_rate_limit("a:c", 2, Duration::from_secs(60))
                .await
        );
    }

    #[tokio::test]
    async fn test_rate_limit_rejects_over_limit() {
        let state = test_state();
        assert!(
            state
                .try_consume_rate_limit("a:c", 2, Duration::from_secs(60))
                .await
        );
        assert!(
            state
                .try_consume_rate_limit("a:c", 2, Duration::from_secs(60))
                .await
        );
        assert!(
            !state
                .try_consume_rate_limit("a:c", 2, Duration::from_secs(60))
                .await
        );
    }

    #[tokio::test]
    async fn test_rate_limit_key_rotation_collapses_into_a_shared_bucket() {
        // A rotating-key client must not buy a fresh allowance per key: once
        // the key count saturates, callers share one budget-sized bucket, so
        // total throughput is capped no matter how many new keys appear.
        let state = test_state();
        let window = Duration::from_secs(60);
        for i in 0..MAX_RATE_LIMIT_KEYS {
            assert!(
                state
                    .try_consume_rate_limit(&format!("rotator-{i}"), 2, window)
                    .await,
                "pre-saturation key {i} must pass"
            );
        }
        // Map saturated: the shared bucket accepts the remaining budget...
        assert!(
            state
                .try_consume_rate_limit("rotator-fresh-1", 2, window)
                .await
        );
        assert!(
            state
                .try_consume_rate_limit("rotator-fresh-2", 2, window)
                .await
        );
        // ...and nothing beyond it, regardless of key novelty.
        assert!(
            !state
                .try_consume_rate_limit("rotator-fresh-3", 2, window)
                .await
        );
    }

    #[tokio::test]
    async fn test_first_responder_first_caller_wins() {
        let state = test_state();
        let r1 = state
            .try_claim("corr-1", "agent-1", Duration::from_secs(2))
            .await;
        assert_eq!(r1, ClaimResult::Claimed);
    }

    #[tokio::test]
    async fn test_first_responder_suppresses_cross_agent_and_replayed_claims() {
        let state = test_state();
        state
            .try_claim("corr-2", "agent-1", Duration::from_secs(2))
            .await;
        let cross_agent = state
            .try_claim("corr-2", "agent-2", Duration::from_secs(2))
            .await;
        assert_eq!(
            cross_agent,
            ClaimResult::AlreadyClaimed("agent-1".to_string())
        );
        let replay = state
            .try_claim("corr-2", "agent-1", Duration::from_secs(2))
            .await;
        assert_eq!(replay, ClaimResult::AlreadyClaimed("agent-1".to_string()));
    }

    #[tokio::test]
    async fn test_secret_entropy_detects_high_entropy() {
        let hook = SecretEntropyHook::new(test_state(), "agent-1".to_string(), 4.8, 32);
        let event = inbound(
            "agent-2",
            "coord@conference.example",
            "STATUS: key A9f3Kx82LmQpZ7wT1vBnR4yUeC6sGhJ0x2 done",
        );
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Reject { .. })));
    }

    #[tokio::test]
    async fn test_secret_entropy_allows_normal_text_and_known_safe_tokens() {
        let hook = SecretEntropyHook::new(test_state(), "agent-1".to_string(), 4.8, 32);
        for content in [
            "STATUS: deployment finished successfully on all hosts",
            "STATUS: commit da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "STATUS: artifact /usr/local/bin/some-long-path-token",
            "STATUS: docs https://example.test/releases/long-artifact-name",
        ] {
            let event = inbound("agent-2", "coord@conference.example", content);
            let outcome = hook.execute(&event, &ctx()).await;
            assert!(matches!(outcome, Ok(HookOutcome::Continue { .. })));
        }
    }

    #[tokio::test]
    async fn test_policy_scope_drives_channel_matching() {
        // ISSUE-001 regression: the WASM XMPP path sets the event channel to
        // the transport name ("xmpp"); the room JID arrives via policy_scope.
        let config = test_config();
        let hook =
            EngagementPolicyHook::new(test_state(), config.agent_id.clone(), config.agent_aliases);
        let scoped_inbound = |scope: &str| HookEvent::Inbound {
            user_id: "agent-2".to_string(),
            sender_id: Some("agent-2".to_string()),
            channel: "xmpp".to_string(),
            content: "untargeted room chatter".to_string(),
            thread_id: None,
            policy_scope: Some(scope.to_string()),
        };
        // Coordination room via scope: untargeted message is rejected even
        // though the transport name alone would not match.
        assert!(matches!(
            hook.execute(&scoped_inbound("coord@conference.example"), &ctx())
                .await,
            Ok(HookOutcome::Reject { .. })
        ));
        // A different room via scope: out of scope, message passes.
        assert!(matches!(
            hook.execute(&scoped_inbound("other@conference.example"), &ctx())
                .await,
            Ok(HookOutcome::Continue { .. })
        ));
        // Tag enforcement likewise follows the scope, not the transport name.
        let tag_hook = TagSchemaHook::new(
            test_state(),
            config.agent_id.clone(),
            config.allowed_tags,
            config.max_message_length,
        );
        let rt = HookEvent::ResponseTransform {
            user_id: config.agent_id,
            thread_id: "t-1".to_string(),
            response: "no tag here".to_string(),
            channel: Some("xmpp".to_string()),
            policy_scope: Some("coord@conference.example".to_string()),
        };
        assert!(matches!(
            tag_hook.execute(&rt, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
    }

    #[tokio::test]
    async fn secret_detection_strikes_sender_and_local_transform_once_each() {
        let dir = tempfile::tempdir().expect("temporary secret violation directory");
        let store = Arc::new(ViolationStore::new(
            dir.path().join("violations.json"),
            crate::gate::violation_store::ViolationStoreConfig {
                max_violations: 3,
                disable_duration_secs: 3600,
                decay_secs: 600,
            },
        ));
        let config = test_config();
        let state = CoordinationState::new(
            config.coordination_channels,
            config.admin_users,
            Some(Arc::clone(&store)),
        );
        let hook = SecretEntropyHook::new(state, config.agent_id, 4.8, 32);
        let inbound_secret = inbound(
            "remote-peer",
            "coord@conference.example",
            "STATUS: api_key=abcdefghijklmnop",
        );
        assert!(matches!(
            hook.execute(&inbound_secret, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        let transformed_secret =
            transformed("coord@conference.example", "STATUS: token=qrstuvwxyzabcdef");
        assert!(matches!(
            hook.execute(&transformed_secret, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        assert_eq!(
            store.status("remote-peer"),
            crate::gate::violation_store::ViolationStatus::Warning
        );
        assert_eq!(
            store.status("agent-1"),
            crate::gate::violation_store::ViolationStatus::Warning
        );
    }

    #[test]
    fn test_tag_schema_enforces_inbound_transform_and_outbound_publish_gate() {
        // Inbound structure is checked before generation. Model output keeps
        // one schema-enforcement point so one malformed response costs one
        // strike; BeforeOutbound only provides the hard KNOW publish gate.
        let config = test_config();
        let hook = TagSchemaHook::new(
            test_state(),
            config.agent_id,
            config.allowed_tags,
            config.max_message_length,
        );
        assert_eq!(
            hook.hook_points(),
            &[
                HookPoint::BeforeInbound,
                HookPoint::BeforeOutbound,
                HookPoint::TransformResponse,
            ]
        );
    }

    #[tokio::test]
    async fn knowledge_publish_gate_blocks_only_outbound_records_until_enabled() {
        let config = test_config();
        let disabled = TagSchemaHook::new(
            test_state(),
            config.agent_id.clone(),
            config.allowed_tags.clone(),
            config.max_message_length,
        );
        let record = "KNOW: fleet-readiness | Every room daemon accepts this tag";

        // Receiving remains compatible and available independently of emission.
        assert!(matches!(
            disabled
                .execute(
                    &inbound("remote-peer", "coord@conference.example", record),
                    &ctx(),
                )
                .await,
            Ok(HookOutcome::Continue { .. })
        ));
        // Both normal transformed responses and direct outbound callers are
        // fail-closed while the operator opt-in remains disabled.
        for event in [
            transformed("coord@conference.example", record),
            outbound("agent-1", "coord@conference.example", record),
        ] {
            assert!(matches!(
                disabled.execute(&event, &ctx()).await,
                Ok(HookOutcome::Reject { .. })
            ));
        }
        // BeforeOutbound does not repeat schema enforcement for normal model
        // responses; TransformResponse remains the single strike point.
        assert!(matches!(
            disabled
                .execute(
                    &outbound("agent-1", "coord@conference.example", "untagged"),
                    &ctx(),
                )
                .await,
            Ok(HookOutcome::Continue { .. })
        ));

        let enabled = TagSchemaHook::new(
            test_state(),
            config.agent_id,
            config.allowed_tags,
            config.max_message_length,
        )
        .with_knowledge_publish_enabled(true);
        for event in [
            transformed("coord@conference.example", record),
            outbound("agent-1", "coord@conference.example", record),
        ] {
            assert!(matches!(
                enabled.execute(&event, &ctx()).await,
                Ok(HookOutcome::Continue { .. })
            ));
        }
    }

    #[tokio::test]
    async fn test_engagement_passes_targeted_message() {
        let config = test_config();
        let hook = EngagementPolicyHook::new(test_state(), config.agent_id, config.agent_aliases);
        let event = inbound(
            "agent-2",
            "coord@conference.example",
            "QUERY: @agent1 please confirm the deploy window",
        );
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Continue { .. })));
    }

    #[tokio::test]
    async fn test_engagement_rejects_untargeted_message() {
        let config = test_config();
        let hook = EngagementPolicyHook::new(test_state(), config.agent_id, config.agent_aliases);
        let event = inbound(
            "agent-2",
            "coord@conference.example",
            "STATUS: routine update for the room",
        );
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Reject { .. })));
    }

    #[tokio::test]
    async fn team_roster_elects_exactly_one_daemon_for_unaddressed_query() {
        let roster = vec!["agent-1".to_string(), "agent-2".to_string()];
        let hook_a = EngagementPolicyHook::new_with_roster(
            test_state(),
            "agent-1".into(),
            vec!["@agent1".into()],
            roster.clone(),
        );
        let hook_b = EngagementPolicyHook::new_with_roster(
            test_state(),
            "agent-2".into(),
            vec!["@agent2".into()],
            roster,
        );
        let event = inbound(
            "operator",
            "coord@conference.example",
            "QUERY: report deployment status",
        );
        let accepted = [
            hook_a.execute(&event, &ctx()).await,
            hook_b.execute(&event, &ctx()).await,
        ]
        .into_iter()
        .filter(|result| matches!(result, Ok(HookOutcome::Continue { .. })))
        .count();
        assert_eq!(accepted, 1);
    }

    #[tokio::test]
    async fn explicit_peer_mention_never_falls_through_to_roster_election() {
        let hook = EngagementPolicyHook::new_with_roster(
            test_state(),
            "agent-1".into(),
            vec!["@agent1".into()],
            vec!["agent-1".into(), "agent-2".into()],
        );
        for content in ["QUERY: @agent2 status", "QUERY: @unknown status"] {
            assert!(matches!(
                hook.execute(
                    &inbound("operator", "coord@conference.example", content),
                    &ctx()
                )
                .await,
                Ok(HookOutcome::Reject { .. })
            ));
        }
    }

    #[tokio::test]
    async fn test_engagement_allows_admin_and_casual_channels() {
        let config = test_config();
        let hook = EngagementPolicyHook::new(test_state(), config.agent_id, config.agent_aliases);
        // Admin actor passes even when storage scope belongs to the owner.
        let admin_event = HookEvent::Inbound {
            user_id: "workspace-owner".to_string(),
            sender_id: Some("sun".to_string()),
            channel: "xmpp".to_string(),
            content: "STATUS: hello room".to_string(),
            thread_id: None,
            policy_scope: Some("coord@conference.example".to_string()),
        };
        assert!(matches!(
            hook.execute(&admin_event, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
        // Casual channel passes untouched.
        let casual = inbound("agent-2", "general", "hello anyone");
        assert!(matches!(
            hook.execute(&casual, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
    }

    #[tokio::test]
    async fn test_tag_schema_rejects_untagged_in_coord_channel() {
        let config = test_config();
        let hook = TagSchemaHook::new(
            test_state(),
            config.agent_id,
            config.allowed_tags,
            config.max_message_length,
        );
        let event = inbound(
            "agent-2",
            "coord@conference.example",
            "just a plain message",
        );
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Reject { .. })));
    }

    #[tokio::test]
    async fn test_tag_schema_inbound_strikes_sender_not_local_agent() {
        let dir = std::env::temp_dir().join(format!(
            "coord-tag-inbound-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let store = Arc::new(ViolationStore::new(
            dir.join("violations.json"),
            crate::gate::violation_store::ViolationStoreConfig {
                max_violations: 3,
                disable_duration_secs: 3600,
                decay_secs: 600,
            },
        ));
        let config = test_config();
        let state = CoordinationState::new(
            config.coordination_channels,
            config.admin_users,
            Some(Arc::clone(&store)),
        );
        let hook = TagSchemaHook::new(
            state,
            config.agent_id.clone(),
            config.allowed_tags,
            config.max_message_length,
        );
        let event = HookEvent::Inbound {
            user_id: "workspace-owner".to_string(),
            sender_id: Some("coord@conference.example/cmc".to_string()),
            channel: "xmpp".to_string(),
            content: "hello!".to_string(),
            thread_id: None,
            policy_scope: Some("coord@conference.example".to_string()),
        };

        assert!(matches!(
            hook.execute(&event, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        assert_eq!(
            store.status("coord@conference.example/cmc"),
            crate::gate::violation_store::ViolationStatus::Warning
        );
        assert_eq!(
            store.status(&config.agent_id),
            crate::gate::violation_store::ViolationStatus::Active
        );
        assert_eq!(
            store.status("workspace-owner"),
            crate::gate::violation_store::ViolationStatus::Active
        );
    }

    #[tokio::test]
    async fn test_tag_schema_allows_untagged_in_casual_channel() {
        let config = test_config();
        let hook = TagSchemaHook::new(
            test_state(),
            config.agent_id,
            config.allowed_tags,
            config.max_message_length,
        );
        let event = inbound("agent-2", "general", "just a plain message");
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Continue { .. })));
    }

    #[tokio::test]
    async fn test_tag_schema_allows_tagged_and_checks_response_transform() {
        let config = test_config();
        let hook = TagSchemaHook::new(
            test_state(),
            config.agent_id.clone(),
            config.allowed_tags,
            config.max_message_length,
        );
        let ok_event = inbound(
            "agent-2",
            "coord@conference.example",
            "STATUS: deploy complete",
        );
        assert!(matches!(
            hook.execute(&ok_event, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
        // ResponseTransform with a coordination channel is enforced too.
        let rt = HookEvent::ResponseTransform {
            user_id: config.agent_id,
            thread_id: "t-1".to_string(),
            response: "no tag here".to_string(),
            channel: Some("coord@conference.example".to_string()),
            policy_scope: None,
        };
        assert!(matches!(
            hook.execute(&rt, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        // ResponseTransform without a channel is out of scope.
        let rt_none = HookEvent::ResponseTransform {
            user_id: "agent-1".to_string(),
            thread_id: "t-1".to_string(),
            response: "no tag here".to_string(),
            channel: None,
            policy_scope: None,
        };
        assert!(matches!(
            hook.execute(&rt_none, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
    }

    #[tokio::test]
    async fn test_tag_schema_rejects_multiline_and_too_long() {
        let config = test_config();
        let max = config.max_message_length;
        let hook = TagSchemaHook::new(test_state(), config.agent_id, config.allowed_tags, max);
        let multiline = transformed("coord@conference.example", "STATUS: one\ntwo");
        assert!(matches!(
            hook.execute(&multiline, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        let long = transformed(
            "coord@conference.example",
            &format!("STATUS: {}", "x".repeat(max + 1)),
        );
        assert!(matches!(
            hook.execute(&long, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
    }

    #[tokio::test]
    async fn test_tag_schema_exact_tag_only_does_not_panic() {
        // ISSUE-011 regression: content equal to exactly an allowed tag (no
        // colon) previously indexed past the end of the byte slice.
        let config = test_config();
        let hook = TagSchemaHook::new(
            test_state(),
            config.agent_id,
            config.allowed_tags.clone(),
            config.max_message_length,
        );
        for tag in &config.allowed_tags {
            for content in [
                tag.to_string(),
                tag.to_lowercase(),
                format!("{tag} "),
                String::new(),
            ] {
                let event = transformed("coord@conference.example", &content);
                let outcome = hook.execute(&event, &ctx()).await;
                assert!(
                    matches!(outcome, Ok(HookOutcome::Reject { .. })),
                    "content {content:?} must be rejected without panic"
                );
            }
            // Tag plus colon (with body) is well-formed and passes.
            let ok = transformed("coord@conference.example", &format!("{tag}: body"));
            assert!(matches!(
                hook.execute(&ok, &ctx()).await,
                Ok(HookOutcome::Continue { .. })
            ));
        }
    }

    #[tokio::test]
    async fn test_rate_limit_hook() {
        let config = test_config();
        let hook = RateLimitHook::new(test_state(), config.agent_id, 2, Duration::from_secs(3600));
        let mk = || outbound("someone", "coord@conference.example", "STATUS: ping");
        assert!(matches!(
            hook.execute(&mk(), &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
        assert!(matches!(
            hook.execute(&mk(), &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
        // Third message in the window is rejected.
        assert!(matches!(
            hook.execute(&mk(), &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        // Casual channels are unlimited.
        let casual = outbound("someone", "general", "hello");
        assert!(matches!(
            hook.execute(&casual, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
    }

    #[tokio::test]
    async fn test_register_coordination_hooks_counts() {
        let registry = Arc::new(HookRegistry::new());
        let config = test_config();
        let summary = register_coordination_hooks(&registry, config, None).await;
        assert_eq!(summary.hooks, 5);
        assert_eq!(summary.errors, 0);
        let names = registry.list().await;
        assert!(names.contains(&"coordination.tag_schema".to_string()));
        assert!(names.contains(&"coordination.rate_limit".to_string()));
        assert!(names.contains(&"coordination.engagement_policy".to_string()));
        assert!(names.contains(&"coordination.first_responder".to_string()));
        assert!(names.contains(&"coordination.secret_entropy".to_string()));
    }

    #[tokio::test]
    async fn test_register_coordination_hooks_disabled_is_noop() {
        let registry = Arc::new(HookRegistry::new());
        let config = CoordinationHookConfig::default(); // enabled=false
        let summary = register_coordination_hooks(&registry, config, None).await;
        assert_eq!(summary.hooks, 0);
        assert_eq!(summary.errors, 0);
        assert!(registry.list().await.is_empty());
    }

    #[tokio::test]
    async fn knowledge_share_requires_shared_workspace() {
        let registry = Arc::new(HookRegistry::new());
        let mut config = test_config();
        config.knowledge_share_enabled = true;
        let summary =
            register_coordination_hooks_with_knowledge(&registry, config, None, None).await;
        assert_eq!(summary.hooks, 5);
        assert_eq!(summary.errors, 1);
        assert!(
            !registry
                .list()
                .await
                .contains(&"coordination.knowledge_share".to_string())
        );
    }

    #[tokio::test]
    async fn test_first_responder_hook_flow() {
        // Two agents sharing one state store: first claim wins.
        let state = test_state();
        let hook_a =
            FirstResponderHook::new(state.clone(), "agent-1".to_string(), Duration::from_secs(2));
        let hook_b = FirstResponderHook::new(state, "agent-2".to_string(), Duration::from_secs(2));
        let event = inbound(
            "agent-2",
            "coord@conference.example",
            "QUERY: @agent1 status?",
        );
        assert!(matches!(
            hook_a.execute(&event, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
        assert!(matches!(
            hook_b.execute(&event, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        // Non-coordination channels never participate in claiming.
        let casual = inbound("agent-2", "general", "hello");
        assert!(matches!(
            hook_b.execute(&casual, &ctx()).await,
            Ok(HookOutcome::Continue { .. })
        ));
    }
}
