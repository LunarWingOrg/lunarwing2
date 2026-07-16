use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;
use uuid::Uuid;

use lunarwing::channels::{IncomingMessage, StatusUpdate};
use lunarwing::error::LlmError;
use lunarwing::llm::{
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream, LlmStreamChunk,
    Role, TokenUsage, ToolCall, ToolCompletionRequest, ToolCompletionResponse,
};
use lunarwing::secrets::SecretsStore;
use lunarwing::tools::mcp::McpClient;
use lunarwing::tools::mcp::config::McpServerConfig;
use lunarwing::tools::mcp::session::McpSessionManager;

use crate::support;
use crate::support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use crate::support::mock_mcp_server::{MOCK_MCP_TOKEN, MockToolResponse, start_mock_mcp_server};
use crate::support::test_rig::TestRigBuilder;

include!("common.rs");
include!("transport_llm.rs");
include!("auth_llm.rs");
include!("oauth.rs");
include!("transport_test.rs");
include!("auth_test.rs");
