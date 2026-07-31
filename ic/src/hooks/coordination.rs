//! Multi-agent team coordination hooks.
//!
//! Implements Phase 2 of the coordination pipeline
//! (`docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`):
//! stateful hooks enforcing a "pager system / ops room" discipline on
//! designated coordination channels (XMPP MUC, IRC):
//!
//! - [`SecretEntropyHook`] (priority 5) — Shannon-entropy secret detection
//! - [`EngagementPolicyHook`] (priority 15) — only respond when targeted
//! - [`FirstResponderHook`] (priority 16) — one agent claims a message
//! - [`TagSchemaHook`] (priority 20) — messages must start with an allowed tag
//! - [`RateLimitHook`] (priority 25) — 2 messages/hour per agent
//!
//! All hooks are no-ops outside the configured coordination channels, and
//! nothing is registered unless `COORD_ENABLED=true`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::gate::violation_store::{ViolationStatus, ViolationStore};
use crate::hooks::HookRegistrationSummary;
use crate::hooks::hook::{
    Hook, HookContext, HookError, HookEvent, HookFailureMode, HookOutcome, HookPoint,
};
use crate::hooks::registry::HookRegistry;

/// Default maximum coordination message length (single line).
pub const DEFAULT_MAX_MESSAGE_LENGTH: usize = 420;

/// Default allowed coordination message tags.
pub const DEFAULT_ALLOWED_TAGS: [&str; 5] = ["STATUS", "ALERT", "COORD", "ANNOUNCE", "QUERY"];

/// Maximum accepted coordination agent identifier length.
pub const MAX_COORD_AGENT_ID_LEN: usize = 128;

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

/// Configuration for the coordination hooks, loaded from environment.
#[derive(Debug, Clone)]
pub struct CoordinationHookConfig {
    /// Master switch (`COORD_ENABLED`).
    pub enabled: bool,
    /// This agent's unique identifier (`COORD_AGENT_ID`).
    pub agent_id: String,
    /// Lowercase @mention aliases that target this agent (`COORD_AGENT_ALIASES`).
    pub agent_aliases: Vec<String>,
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
}

impl Default for CoordinationHookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            agent_id: String::new(),
            agent_aliases: Vec::new(),
            coordination_channels: Vec::new(),
            max_messages: 2,
            rate_window: Duration::from_secs(3600),
            coalesce_window: Duration::from_millis(2000),
            entropy_threshold: 4.8,
            min_secret_length: 32,
            admin_users: Vec::new(),
            max_message_length: DEFAULT_MAX_MESSAGE_LENGTH,
            allowed_tags: DEFAULT_ALLOWED_TAGS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl CoordinationHookConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self {
            enabled: env_flag("COORD_ENABLED"),
            ..Self::default()
        };
        if let Ok(v) = std::env::var("COORD_AGENT_ID") {
            config.agent_id = v.trim().to_string();
        }
        config.agent_aliases = env_list("COORD_AGENT_ALIASES")
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect();
        config.coordination_channels = env_list("COORD_CHANNELS");
        config.admin_users = env_list("COORD_ADMIN_USERS");
        if let Some(v) = env_parse::<usize>("COORD_MAX_MESSAGES") {
            config.max_messages = v;
        }
        if let Some(v) = env_parse::<u64>("COORD_RATE_WINDOW_SECS") {
            config.rate_window = Duration::from_secs(v);
        }
        if let Some(v) = env_parse::<u64>("COORD_COALESCE_MS") {
            config.coalesce_window = Duration::from_millis(v);
        }
        if let Some(v) = env_parse::<f64>("COORD_ENTROPY_THRESHOLD") {
            config.entropy_threshold = v;
        }
        if let Some(v) = env_parse::<usize>("COORD_MIN_SECRET_LENGTH") {
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
    pub async fn try_consume_rate_limit(
        &self,
        key: &str,
        max_messages: usize,
        window: Duration,
    ) -> bool {
        let mut guard = self.inner.lock().await;
        let now = std::time::Instant::now();
        let stamps = guard.rate_limit_windows.entry(key.to_string()).or_default();
        stamps.retain(|t| now.duration_since(*t) < window);
        if stamps.len() >= max_messages {
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
            Some((claimer, _)) if claimer != agent_id => {
                ClaimResult::AlreadyClaimed(claimer.clone())
            }
            Some(_) => ClaimResult::Claimed,
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

/// Known-safe token shapes that should not trip entropy detection:
/// git SHAs (40 hex chars), file paths, and URLs.
fn is_allowlisted_token(token: &str) -> bool {
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    if token.len() == 40 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    false
}

/// Extract (user_id, policy scope, content, is_outbound) from
/// inbound/outbound events. The scope is the canonical destination from
/// routing metadata when present (e.g. the XMPP room JID), falling back to
/// the transport channel name (ISSUE-001).
fn event_content(event: &HookEvent) -> Option<(&str, &str, &str, bool)> {
    let scope = event.policy_scope()?;
    match event {
        HookEvent::Inbound {
            user_id, content, ..
        } => Some((user_id, scope, content, false)),
        HookEvent::Outbound {
            user_id, content, ..
        } => Some((user_id, scope, content, true)),
        _ => None,
    }
}

/// Priority 5 — high-entropy secret detection beyond the Phase 1 regexes.
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
        &[HookPoint::BeforeInbound, HookPoint::BeforeOutbound]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailClosed
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let Some((user_id, scope, content, is_outbound)) = event_content(event) else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        for token in content.split_whitespace() {
            if token.len() >= self.min_length
                && !is_allowlisted_token(token)
                && shannon_entropy(token) >= self.entropy_threshold
            {
                // Inbound secrets belong to the remote sender; outbound
                // secrets are emitted by the local agent (ISSUE-004).
                let actor = if is_outbound {
                    self.agent_id.as_str()
                } else {
                    user_id
                };
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
}

impl EngagementPolicyHook {
    /// Create with shared state, this agent's id, and its mention aliases.
    pub fn new(state: CoordinationState, agent_id: String, agent_aliases: Vec<String>) -> Self {
        Self {
            state,
            agent_id,
            agent_aliases,
        }
    }

    fn is_targeted(&self, content: &str) -> bool {
        let lower = content.to_lowercase();
        self.agent_aliases.iter().any(|alias| lower.contains(alias))
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
            user_id, content, ..
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
        // Messages from this agent or from admins always pass.
        if *user_id == self.agent_id || self.state.is_admin(user_id) {
            return Ok(HookOutcome::ok());
        }
        if !self.is_targeted(content) {
            // Silent pass: message wasn't directed at this agent. The reject
            // keeps it out of the conversation so the agent loop exits without
            // generating a response (Option B from the integration plan).
            return Ok(HookOutcome::reject("coord: not targeted at this agent"));
        }
        Ok(HookOutcome::ok())
    }
}

/// Priority 16 — first responder wins; duplicates are rejected.
///
/// Scope note (ISSUE-013): claims live in this process's in-memory
/// `CoordinationState`, so deduplication is **process-local** — it covers
/// hooks sharing one daemon's state, NOT separate tenant daemons watching
/// the same MUC. Two independent agents can both claim and both respond.
/// A shared compare-and-insert backend (host-local store or broker op) is
/// deferred; treat this as opportunistic self-deduplication only.
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

/// Correlation id from channel + content (FNV-style via DefaultHasher).
fn correlation_id(channel: &str, content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    channel.hash(&mut hasher);
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
        let HookEvent::Inbound { content, .. } = event else {
            return Ok(HookOutcome::ok());
        };
        let Some(scope) = event.policy_scope() else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        let id = correlation_id(scope, content);
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
                    "coordination: message already claimed by another agent"
                );
                Ok(HookOutcome::reject(
                    "coord: another agent already claimed this message",
                ))
            }
        }
    }
}

/// Priority 20 — messages in coordination channels must start with an
/// allowed tag, be single-line, and fit the length cap.
pub struct TagSchemaHook {
    state: CoordinationState,
    agent_id: String,
    allowed_tags: Vec<String>,
    max_message_length: usize,
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
        }
    }

    /// Event policy-scope/content for outbound-side events.
    /// `ResponseTransform` events only participate when they carry a
    /// channel or scope.
    fn outbound_parts<'a>(&self, event: &'a HookEvent) -> Option<(&'a str, &'a str)> {
        match event {
            HookEvent::Outbound { content, .. } => Some((event.policy_scope()?, content)),
            HookEvent::ResponseTransform { response, .. } => {
                Some((event.policy_scope()?, response))
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
}

#[async_trait]
impl Hook for TagSchemaHook {
    fn name(&self) -> &str {
        "coordination.tag_schema"
    }

    fn hook_points(&self) -> &[HookPoint] {
        // Single enforcement point (ISSUE-004): every model response passes
        // TransformResponse exactly once per turn, including drain-loop
        // intermediate responses that bypass BeforeOutbound. Registering
        // both stages made one malformed response consume two strikes — the
        // rejection replacement text reached BeforeOutbound untagged.
        &[HookPoint::TransformResponse]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailClosed
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let Some((channel, content)) = self.outbound_parts(event) else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(channel).await {
            return Ok(HookOutcome::ok());
        }
        if content.contains('\n') || content.contains('\r') {
            let agent = self.agent_id.clone();
            self.state
                .record_violation(&agent, "multi-line message", self.name(), channel)
                .await;
            return Ok(HookOutcome::reject(
                "coord: messages in coordination channels must be single-line",
            ));
        }
        if content.chars().count() > self.max_message_length {
            let agent = self.agent_id.clone();
            self.state
                .record_violation(&agent, "message too long", self.name(), channel)
                .await;
            return Ok(HookOutcome::reject(format!(
                "coord: messages in coordination channels must not exceed {} characters",
                self.max_message_length
            )));
        }
        if !self.starts_with_tag(content) {
            let agent = self.agent_id.clone();
            self.state
                .record_violation(&agent, "missing tag", self.name(), channel)
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
/// Called from hook bootstrap when `COORD_ENABLED=true`. Returns the number
/// of hooks registered (0 when the config is not actionable).
pub async fn register_coordination_hooks(
    registry: &Arc<HookRegistry>,
    config: CoordinationHookConfig,
    violations: Option<Arc<ViolationStore>>,
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

    let hooks: Vec<(Arc<dyn Hook>, u32)> = vec![
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
            Arc::new(EngagementPolicyHook::new(
                state.clone(),
                config.agent_id.clone(),
                config.agent_aliases.clone(),
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
            Arc::new(TagSchemaHook::new(
                state.clone(),
                config.agent_id.clone(),
                config.allowed_tags.clone(),
                config.max_message_length,
            )),
            20,
        ),
        (
            Arc::new(RateLimitHook::new(
                state,
                config.agent_id.clone(),
                config.max_messages,
                config.rate_window,
            )),
            25,
        ),
    ];

    for (hook, priority) in hooks {
        registry.register_with_priority(hook, priority).await;
        summary.hooks += 1;
    }
    tracing::info!(
        agent_id = %config.agent_id,
        channels = ?config.coordination_channels,
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

    fn test_config() -> CoordinationHookConfig {
        CoordinationHookConfig {
            enabled: true,
            agent_id: "agent-1".to_string(),
            agent_aliases: vec!["@agent1".to_string(), "agent1".to_string()],
            coordination_channels: vec!["coord@conference.example".to_string()],
            max_messages: 2,
            rate_window: Duration::from_secs(3600),
            coalesce_window: Duration::from_millis(2000),
            entropy_threshold: 4.8,
            min_secret_length: 32,
            admin_users: vec!["sun".to_string()],
            max_message_length: DEFAULT_MAX_MESSAGE_LENGTH,
            allowed_tags: DEFAULT_ALLOWED_TAGS.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn test_state() -> CoordinationState {
        let config = test_config();
        CoordinationState::new(config.coordination_channels, config.admin_users, None)
    }

    fn inbound(user: &str, channel: &str, content: &str) -> HookEvent {
        HookEvent::Inbound {
            user_id: user.to_string(),
            channel: channel.to_string(),
            content: content.to_string(),
            thread_id: None,
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
        assert!(is_allowlisted_token(
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        ));
        assert!(!is_allowlisted_token("A9f3Kx82LmQpZ7wT1vBnR4yUeC6sGhJ0"));
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
    async fn test_first_responder_first_caller_wins() {
        let state = test_state();
        let r1 = state
            .try_claim("corr-1", "agent-1", Duration::from_secs(2))
            .await;
        assert_eq!(r1, ClaimResult::Claimed);
    }

    #[tokio::test]
    async fn test_first_responder_second_caller_rejected() {
        let state = test_state();
        state
            .try_claim("corr-2", "agent-1", Duration::from_secs(2))
            .await;
        let r2 = state
            .try_claim("corr-2", "agent-2", Duration::from_secs(2))
            .await;
        assert_eq!(r2, ClaimResult::AlreadyClaimed("agent-1".to_string()));
        // Same agent re-claiming is fine (idempotent).
        let r3 = state
            .try_claim("corr-2", "agent-1", Duration::from_secs(2))
            .await;
        assert_eq!(r3, ClaimResult::Claimed);
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
    async fn test_secret_entropy_allows_normal_text() {
        let hook = SecretEntropyHook::new(test_state(), "agent-1".to_string(), 4.8, 32);
        let event = inbound(
            "agent-2",
            "coord@conference.example",
            "STATUS: deployment finished successfully on all hosts",
        );
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Continue { .. })));
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
    async fn test_secret_entropy_outbound_strikes_local_agent() {
        // ISSUE-004 regression: an outbound secret must be attributed to the
        // local emitting agent, not the conversation peer named by the event.
        let dir = std::env::temp_dir().join(format!(
            "coord-entropy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
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
        let hook = SecretEntropyHook::new(state, config.agent_id, 4.8, 32);
        let event = outbound(
            "remote-peer",
            "coord@conference.example",
            "STATUS: key A9f3Kx82LmQpZ7wT1vBnR4yUeC6sGhJ0x2 done",
        );
        assert!(matches!(
            hook.execute(&event, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        assert_eq!(
            store.status("agent-1"),
            crate::gate::violation_store::ViolationStatus::Warning
        );
        assert_eq!(
            store.status("remote-peer"),
            crate::gate::violation_store::ViolationStatus::Active
        );
    }

    #[test]
    fn test_tag_schema_enforces_at_transform_only() {
        // ISSUE-004 regression: single enforcement point — one malformed
        // response must cost exactly one strike.
        let config = test_config();
        let hook = TagSchemaHook::new(
            test_state(),
            config.agent_id,
            config.allowed_tags,
            config.max_message_length,
        );
        assert_eq!(hook.hook_points(), &[HookPoint::TransformResponse]);
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
    async fn test_engagement_allows_admin_and_casual_channels() {
        let config = test_config();
        let hook = EngagementPolicyHook::new(test_state(), config.agent_id, config.agent_aliases);
        // Admin sender passes even untargeted.
        let admin_event = inbound("sun", "coord@conference.example", "STATUS: hello room");
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
        let event = outbound(
            "agent-1",
            "coord@conference.example",
            "just a plain message",
        );
        let outcome = hook.execute(&event, &ctx()).await;
        assert!(matches!(outcome, Ok(HookOutcome::Reject { .. })));
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
        let event = outbound("agent-1", "general", "just a plain message");
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
        let ok_event = outbound(
            "agent-1",
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
        let multiline = outbound("agent-1", "coord@conference.example", "STATUS: one\ntwo");
        assert!(matches!(
            hook.execute(&multiline, &ctx()).await,
            Ok(HookOutcome::Reject { .. })
        ));
        let long = outbound(
            "agent-1",
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
                let event = outbound("agent-1", "coord@conference.example", &content);
                let outcome = hook.execute(&event, &ctx()).await;
                assert!(
                    matches!(outcome, Ok(HookOutcome::Reject { .. })),
                    "content {content:?} must be rejected without panic"
                );
            }
            // Tag plus colon (with body) is well-formed and passes.
            let ok = outbound(
                "agent-1",
                "coord@conference.example",
                &format!("{tag}: body"),
            );
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
