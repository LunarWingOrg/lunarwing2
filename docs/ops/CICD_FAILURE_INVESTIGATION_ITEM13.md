ration.rs:650:    if try_connect(&pool).await.is_none() {
tests/workspace_integration.rs:693:    if try_connect(&pool).await.is_none() {
--- Check 6: LLM module isolation ---
WARNING: src/llm/ has 27 reference(s) to modules outside crate::llm:
src/llm/config.rs:159:        let api_key = crate::config::helpers::env_or_override("LUNARWING_CLOUD_API_KEY")
src/llm/config.rs:168:        let base_url = crate::config::helpers::env_or_override("LUNARWING_CLOUD_BASE_URL")
src/llm/lunarwing_cloud_chat.rs:310:            let truncated = crate::agent::truncate_for_preview(&response_text, 512);
src/llm/lunarwing_cloud_chat.rs:318:            let truncated = crate::agent::truncate_for_preview(&response_text, 512);
src/llm/lunarwing_cloud_chat.rs:370:            let truncated = crate::agent::truncate_for_preview(&response_text, 512);
src/llm/lunarwing_cloud_chat.rs:665:    fn set_model(&self, model: &str) -> Result<(), crate::error::LlmError> {
src/llm/models.rs:109:pub(crate) fn build_lunarwing_cloud_model_fetch_config() -> crate::config::LlmConfig {
src/llm/models.rs:110:    let auth_base_url = crate::config::helpers::env_or_override("LUNARWING_CLOUD_AUTH_URL")
src/llm/models.rs:113:    let lunarwing_cloud = crate::config::LunarWingCloudConfig::for_model_discovery();
src/llm/models.rs:114:    crate::config::LlmConfig {
src/llm/models.rs:118:            session_path: crate::config::llm::default_session_path(),
src/llm/oauth_helpers.rs:43:    crate::config::helpers::env_or_override("LUNARWING_OAUTH_CALLBACK_URL")
src/llm/oauth_helpers.rs:59:    crate::config::helpers::env_or_override("OAUTH_CALLBACK_HOST")
src/llm/oauth_helpers.rs:365:    use crate::config::helpers::lock_env;
src/llm/recording.rs:616:    pub async fn snapshot_memory(&self, workspace: &crate::workspace::Workspace) {
src/llm/registry.rs:316:    Some(crate::bootstrap::lunarwing_base_dir().join("providers.json"))
src/llm/session.rs:58:    store: RwLock<Option<Arc<dyn crate::db::Database>>>,
src/llm/session.rs:121:    pub async fn attach_store(&self, store: Arc<dyn crate::db::Database>, user_id: &str) {
src/llm/session.rs:199:        let preview = crate::agent::truncate_for_preview(&body, 200);
src/llm/session.rs:398:            crate::setup::secret_input("API key").map_err(|e| LlmError::SessionRenewalFailed {
src/llm/session.rs:416:        crate::config::helpers::set_runtime_env("LUNARWING_CLOUD_API_KEY", &key);
src/llm/session.rs:421:        if let Err(e) = crate::bootstrap::upsert_bootstrap_var("LUNARWING_CLOUD_API_KEY", &key) {
src/llm/session.rs:426:        crate::setup::print_success("LunarWing Cloud Cloud API key saved.");
src/llm/transcription/mod.rs:101:    pub async fn process(&self, msg: &mut crate::channels::IncomingMessage) {
src/llm/transcription/mod.rs:102:        use crate::channels::AttachmentKind;
src/llm/transcription/mod.rs:167:    use crate::channels::{AttachmentKind, IncomingAttachment, IncomingMessage};
src/llm/reasoning.rs:2001:        other => crate::channels::budget::budget_for_channel(other)
(These are pre-existing; fix them before extracting the crate.)
(New 'use crate::' imports are hard violations — see below.)
HARD VIOLATION: new 'use crate::' imports in src/llm/:
src/llm/oauth_helpers.rs:365:    use crate::config::helpers::lock_env;
src/llm/transcription/mod.rs:102:        use crate::channels::AttachmentKind;
src/llm/transcription/mod.rs:167:    use crate::channels::{AttachmentKind, IncomingAttachment, IncomingMessage};
=== Summary ===
FAILED: 4 hard violation(s) found
Cleaning up project directory and file based variables 00:00
ERROR: Job failed: exit code 1