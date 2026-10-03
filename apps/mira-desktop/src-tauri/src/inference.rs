//! Desktop inference adapter.
//!
//! Both chat and background-memory inference are routed through the reusable
//! `mira-runtime` → `mira-agent` → `mira-ai` stack instead of a hand-written HTTP/SSE
//! transport. The packages deliberately leave application policy to their consumer, so this
//! module owns what is Mira-specific:
//!
//! - the exact system prompt and the last-20-message window, byte-for-byte identical to the
//!   previous gateway;
//! - the DeepSeek reasoning rule (enabled for `v4`/`reasoner` chat models only);
//! - explicit OpenAI-compatible compatibility switches, derived in the app and never from a
//!   provider name or URL by the packages;
//! - the mapping of provider failures to safe Chinese categories that never echo provider
//!   prose, headers, prompts or a credential.
//!
//! A credential is never stored here. The caller passes the key it already loaded from the OS
//! credential store for exactly one run, and the injected resolver is a bypass-only stub that
//! fails rather than duplicating that lookup.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mira_ai::api::openai_completions::{
    MaxTokensField, OpenAiCompletionsCompat, OpenAiCompletionsConfig, OpenAiCompletionsProvider,
    ReasoningEncoding, DEFAULT_CONNECT_TIMEOUT,
};
use mira_ai::{
    Api, AssistantContent, AssistantEvent, AssistantMessage, AssistantSource, BlockKind,
    Credential, Message, Model, ReasoningLevel, RequestOptions, StopReason, TextBlock, UserMessage,
    DEFAULT_MAX_RETRIES, DEFAULT_RESPONSE_HEADER_TIMEOUT,
};
use mira_runtime::{
    AgentError, AgentEvent, AgentLimits, CancellationToken, CredentialError, CredentialFuture,
    CredentialId, CredentialRequest, CredentialResolver, ModelBinding, ModelRegistry, RunLimit,
    RuntimeError, RuntimeLimits, Session, SessionConfig, SessionRun,
};

use crate::cancellation::{Attempt, CancellationState};
use crate::types::{ChatMessage, Memory, ModelConfig};

/// Sentinel returned when the user cancelled the run.
///
/// `chat.rs` maps it to the existing `assistant_message: None` result instead of an error.
pub(crate) const CANCELLED: &str = "__CANCELLED__";

/// Provenance recorded for assistant turns restored from SQLite history.
///
/// Mira stores only text for old assistant turns and does not know which model produced them,
/// so the restored turn is attributed to this constant. It never matches a configured model,
/// which also keeps unknown history from being replayed as current-model reasoning.
const HISTORY_PROVENANCE: &str = "mira-history";

/// Number of most recent history turns the chat request keeps.
const HISTORY_WINDOW: usize = 20;

/// Character cap for one project-context message.
const PROJECT_CONTEXT_MAX_CHARS: usize = 180;

/// Wall-clock budget of one desktop run.
///
/// Finite on purpose: the packages enforce a deadline, and the app never grants an unbounded
/// run. It matches the packages' own default.
const RUN_TIME: Duration = Duration::from_secs(300);

/// A credential resolver that always fails.
///
/// The desktop always supplies an explicit credential with `prompt_with_credential`, because
/// the key is loaded once by the app from the OS credential store. This resolver exists so a
/// code path that forgets to bypass it fails loudly instead of performing a second lookup.
struct BypassOnlyResolver;

impl CredentialResolver for BypassOnlyResolver {
    fn resolve<'a>(
        &'a self,
        _request: &'a CredentialRequest,
        _cancellation: CancellationToken,
    ) -> CredentialFuture<'a> {
        Box::pin(async { Err(CredentialError) })
    }
}

/// Inputs of one chat request: the window, the assembled context blocks and the new user turn.
pub(crate) struct ChatRequest<'a> {
    /// Prior conversation, oldest first, before the last-20 window is applied.
    pub history: &'a [ChatMessage],
    /// Other messages of the same project.
    pub project_context: &'a [ChatMessage],
    /// Long-term memories injected for this turn.
    pub injected_memories: &'a [Memory],
    /// The new user turn, appended after the history window.
    pub user_content: &'a str,
    /// User-configured extra system instructions.
    pub system_prompt_extra: &'a str,
}

/// Run one chat completion through the runtime stack and forward its streamed deltas.
///
/// `on_delta` receives visible text, `on_reasoning` receives thinking text; the two streams stay
/// separate. The returned string is the terminal assistant text. Cancellation is bridged to the
/// run's own token, so a frontend cancel interrupts a stalled header, body or retry immediately
/// instead of waiting for the next byte.
pub(crate) async fn complete_chat_streaming<F, G>(
    config: &ModelConfig,
    request: ChatRequest<'_>,
    on_delta: F,
    on_reasoning: G,
    cancel_state: &CancellationState,
    attempt: &Attempt,
) -> Result<String, String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    // A superseded attempt must not start work at all. Registration below repeats this check
    // atomically to close the race with a concurrent reset.
    if !cancel_state.is_current(attempt) {
        return Err(CANCELLED.to_string());
    }
    let credential = credential_for(config)?;
    let reasoning = supports_deepseek_reasoning(config);
    let session = build_session(
        config,
        build_system_prompt(
            request.injected_memories,
            request.project_context,
            request.system_prompt_extra,
        ),
        history_window(request.history),
        consumer_options(reasoning),
        consumer_compat(reasoning),
    )?;
    let run = session
        .prompt_with_credential(request.user_content.to_string(), credential)
        .map_err(|error| safe_error(&error))?;
    // Register the run's own cancellation token so the frontend cancel command stops this run
    // even while it waits for response headers or a retry backoff. A superseded attempt is
    // refused here, cancels and drains its own run, and never touches the successor's slot.
    let _registration = match cancel_state.register(attempt, run.cancellation()) {
        Ok(registration) => registration,
        Err(_stale) => {
            run.cancel();
            let _ = run.outcome().await;
            return Err(CANCELLED.to_string());
        }
    };
    drive_chat_run(run, on_delta, on_reasoning).await
}

/// Forward one run's events to the delta callbacks and return its terminal text.
///
/// Split from [`complete_chat_streaming`] so the event mapping can be driven directly by tests.
pub(crate) async fn drive_chat_run<F, G>(
    mut run: SessionRun,
    mut on_delta: F,
    mut on_reasoning: G,
) -> Result<String, String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    let mut block_kinds: HashMap<usize, BlockKind> = HashMap::new();
    while let Some(event) = run.recv().await {
        if let Err(error) =
            forward_event(&event, &mut block_kinds, &mut on_delta, &mut on_reasoning)
        {
            // A failed callback means the consumer (the window) is gone. Cancel the run and
            // drain its terminal outcome, so the session is left idle and no work outlives it.
            run.cancel();
            let _ = run.outcome().await;
            return Err(error);
        }
    }

    match run.outcome().await {
        Ok(outcome) => {
            // A truncated message is a legitimate, non-empty answer, exactly as before.
            let content = outcome.message().text();
            if content.trim().is_empty() {
                Err("模型返回了空内容".to_string())
            } else {
                Ok(content)
            }
        }
        Err(RuntimeError::Cancelled) => Err(CANCELLED.to_string()),
        Err(error) => Err(safe_error(&error)),
    }
}

/// Run one non-streaming-style background completion and return its terminal text.
///
/// Used by the memory planner: it collects the streamed terminal text but requests neither
/// reasoning nor usage, keeping the planner request identical to the previous non-streaming
/// call apart from the transport. The run owns an independent session and cancellation token,
/// so a foreground cancel never reaches it.
pub(crate) async fn complete_background_text(
    config: &ModelConfig,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, String> {
    let credential = credential_for(config)?;
    let session = build_session(
        config,
        system_prompt.to_string(),
        Vec::new(),
        memory_options(),
        consumer_compat(false),
    )?;
    let mut run = session
        .prompt_with_credential(user_prompt.to_string(), credential)
        .map_err(|error| safe_error(&error))?;
    // The planner needs only the terminal text. Drain events so the bounded channel never
    // backpressures the run.
    while run.recv().await.is_some() {}
    let outcome = run.outcome().await.map_err(|error| safe_error(&error))?;
    let content = outcome.message().text();
    if content.trim().is_empty() {
        return Err("模型返回了空内容".to_string());
    }
    Ok(content)
}

/// Forward one runtime event to the delta callbacks.
///
/// Only `Text` and `Thinking` blocks are projected: `Text` goes to `on_delta`, `Thinking` to
/// `on_reasoning`, and every other block (including tool-call arguments and any unknown kind)
/// is dropped so it can never reach the UI as assistant text.
fn forward_event<F, G>(
    event: &AgentEvent,
    block_kinds: &mut HashMap<usize, BlockKind>,
    on_delta: &mut F,
    on_reasoning: &mut G,
) -> Result<(), String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    let AgentEvent::MessageUpdate { update, .. } = event else {
        return Ok(());
    };
    match update {
        AssistantEvent::BlockStart { index, kind } => {
            block_kinds.insert(*index, *kind);
        }
        AssistantEvent::BlockDelta { index, delta } => match block_kinds.get(index) {
            Some(BlockKind::Text) => on_delta(delta)?,
            Some(BlockKind::Thinking) => on_reasoning(delta)?,
            // Tool-call arguments are not assistant text and the app declares no tools, so they
            // are dropped rather than displayed. A delta for an unopened block is never exposed
            // either, which also keeps unknown block kinds from leaking their payload.
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

/// Build the exact system prompt the previous gateway sent.
pub(crate) fn build_system_prompt(
    injected_memories: &[Memory],
    project_context: &[ChatMessage],
    system_prompt_extra: &str,
) -> String {
    let memory_block = if injected_memories.is_empty() {
        "暂无长期记忆。".to_string()
    } else {
        injected_memories
            .iter()
            .map(|memory| format!("- {}", memory.fact))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let project_block = if project_context.is_empty() {
        "当前会话不在项目中，或项目内暂无其他对话上下文。".to_string()
    } else {
        project_context
            .iter()
            .map(|message| {
                let role = if message.role == "user" {
                    "用户"
                } else {
                    "助手"
                };
                format!(
                    "- {role}: {}",
                    compact(&message.content, PROJECT_CONTEXT_MAX_CHARS)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let extra = if system_prompt_extra.is_empty() {
        String::new()
    } else {
        format!("\n\n用户自定义指令：\n{}", system_prompt_extra)
    };
    format!(
        "你是 Mira，一个本地个人 AI 记忆客户端。请自然使用长期记忆和项目上下文，但不要逐条暴露。\n\n长期记忆：\n{}\n\n同项目其他对话上下文：\n{}{}",
        memory_block, project_block, extra
    )
}

/// Keep the last [`HISTORY_WINDOW`] history turns, oldest first, then drop roles other than
/// `user` and `assistant`.
///
/// The window is taken before the role filter, exactly as the previous gateway did, so an
/// unknown role inside the window still consumes a slot.
pub(crate) fn history_window(history: &[ChatMessage]) -> Vec<Message> {
    history
        .iter()
        .rev()
        .take(HISTORY_WINDOW)
        .rev()
        .filter_map(|message| match message.role.as_str() {
            "user" => Some(Message::User(UserMessage::text(message.content.clone()))),
            "assistant" => Some(Message::Assistant(history_assistant(&message.content))),
            _ => None,
        })
        .collect()
}

/// A restored assistant turn that carries only its text.
///
/// SQLite does not record which model produced the turn, so no reasoning is replayed and the
/// provenance deliberately does not match any configured model.
fn history_assistant(content: &str) -> AssistantMessage {
    AssistantMessage {
        source: AssistantSource {
            api: Api::OpenAiCompletions,
            provider: HISTORY_PROVENANCE.to_string(),
            model: HISTORY_PROVENANCE.to_string(),
            response_model: None,
            response_id: None,
        },
        content: vec![AssistantContent::Text(TextBlock::new(content))],
        stop_reason: StopReason::EndTurn,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    }
}

/// Whether the configured label or base URL marks a DeepSeek endpoint.
pub(crate) fn is_deepseek_config(config: &ModelConfig) -> bool {
    config.provider.eq_ignore_ascii_case("deepseek") || config.base_url.contains("api.deepseek.com")
}

/// Whether this chat model requests DeepSeek reasoning.
///
/// Only `v4` and `reasoner` chat models enable thinking; every other chat model omits the
/// reasoning fields.
pub(crate) fn supports_deepseek_reasoning(config: &ModelConfig) -> bool {
    if !is_deepseek_config(config) {
        return false;
    }
    let model = config.model.to_lowercase();
    model.contains("v4") || model.contains("reasoner")
}

/// Request options for one chat run.
fn consumer_options(reasoning: bool) -> RequestOptions {
    RequestOptions {
        // Explicit mapping of the previous gateway: 45s to response headers and three attempts.
        // The 15s connect timeout lives on the provider configuration.
        response_header_timeout: DEFAULT_RESPONSE_HEADER_TIMEOUT,
        max_retries: DEFAULT_MAX_RETRIES,
        reasoning: reasoning.then_some(ReasoningLevel::High),
        ..RequestOptions::default()
    }
}

/// Request options for one memory-planner run.
fn memory_options() -> RequestOptions {
    RequestOptions {
        // The planner is deterministic and never asks for reasoning or usage.
        temperature: Some(0.0),
        response_header_timeout: DEFAULT_RESPONSE_HEADER_TIMEOUT,
        max_retries: DEFAULT_MAX_RETRIES,
        ..RequestOptions::default()
    }
}

/// Explicit OpenAI-compatible compatibility for this app.
///
/// `stream_options` is never sent, because the previous gateway never sent it and an existing
/// endpoint may reject an unknown field. Reasoning is only enabled for the DeepSeek thinking
/// profile, and even then only when the caller requested it.
fn consumer_compat(reasoning: bool) -> OpenAiCompletionsCompat {
    let mut compat = OpenAiCompletionsCompat::default();
    compat.supports_usage_in_streaming = false;
    if reasoning {
        compat.reasoning = Some(ReasoningEncoding::DeepSeekThinking);
        compat.supports_reasoning_effort = true;
        compat.max_tokens_field = MaxTokensField::MaxTokens;
    }
    compat
}

/// Build a session for one run: provider, binding, prompt, history and budgets.
fn build_session(
    config: &ModelConfig,
    system_prompt: String,
    history: Vec<Message>,
    options: RequestOptions,
    compat: OpenAiCompletionsCompat,
) -> Result<Session, String> {
    let model = Model::new(
        Api::OpenAiCompletions,
        config.provider.clone(),
        config.model.clone(),
    );
    let provider = build_provider(config, compat)?;
    let binding = ModelBinding::new(
        config.id.clone(),
        model,
        Arc::new(provider),
        CredentialId::new(config.id.clone()),
    )
    .with_options(options);
    let registry = Arc::new(ModelRegistry::new(vec![binding]).map_err(|error| safe_error(&error))?);
    let session_config =
        SessionConfig::new(registry, config.id.clone(), Arc::new(BypassOnlyResolver))
            .with_system_prompt(system_prompt)
            .with_history(history)
            .with_limits(AgentLimits::default())
            .with_runtime(RuntimeLimits {
                total_time: RUN_TIME,
                ..RuntimeLimits::default()
            });
    Session::new(session_config).map_err(|error| safe_error(&error))
}

/// Build the OpenAI-compatible provider for one model configuration.
fn build_provider(
    config: &ModelConfig,
    compat: OpenAiCompletionsCompat,
) -> Result<OpenAiCompletionsProvider, String> {
    let mut provider_config = OpenAiCompletionsConfig::new(config.base_url.clone())
        .map_err(|_| "模型配置的 Base URL 无效".to_string())?;
    // Explicit mapping of the previous gateway: 15s to connect, 45s to response headers and
    // three attempts. The header deadline and attempt count live on the request options.
    provider_config.connect_timeout = DEFAULT_CONNECT_TIMEOUT;
    provider_config.compat = compat;
    OpenAiCompletionsProvider::new(provider_config).map_err(|_| "模型客户端初始化失败".to_string())
}

/// Take the per-run credential the app already loaded from the OS credential store.
fn credential_for(config: &ModelConfig) -> Result<Credential, String> {
    let api_key = config
        .api_key
        .as_deref()
        .filter(|value| !value.trim().is_empty() && *value != "******")
        .ok_or_else(|| format!("模型配置 {} 缺少 API Key", config.name))?;
    Ok(Credential::new(api_key.to_string()))
}

/// Map a runtime failure to a safe Chinese category.
///
/// The packages already carry no provider prose, URL or credential; this function only turns
/// their constant categories into user-facing text and never adds detail.
pub(crate) fn safe_error(error: &RuntimeError) -> String {
    match error {
        RuntimeError::Busy => "模型正忙，请稍后重试".to_string(),
        RuntimeError::NoRuntime => "模型运行环境不可用".to_string(),
        RuntimeError::Cancelled => CANCELLED.to_string(),
        RuntimeError::Timeout => "模型请求超时，请稍后重试".to_string(),
        RuntimeError::Credential => "模型配置缺少 API Key".to_string(),
        RuntimeError::Context => "模型上下文处理失败".to_string(),
        RuntimeError::UnknownBinding(_)
        | RuntimeError::DuplicateBinding(_)
        | RuntimeError::InvalidBinding(_) => "模型配置无效".to_string(),
        RuntimeError::InvalidRunTime { .. } | RuntimeError::InvalidEventBuffer { .. } => {
            "模型运行配置无效".to_string()
        }
        RuntimeError::Agent(error) => safe_agent_error(error),
        RuntimeError::Internal => "模型请求失败".to_string(),
        _ => "模型请求失败".to_string(),
    }
}

/// Map a child agent failure to a safe Chinese category.
fn safe_agent_error(error: &AgentError) -> String {
    match error {
        AgentError::Busy => "模型正忙，请稍后重试".to_string(),
        AgentError::Cancelled => CANCELLED.to_string(),
        AgentError::NoRuntime => "模型运行环境不可用".to_string(),
        AgentError::Limit(RunLimit::Time) => "模型请求超时，请稍后重试".to_string(),
        AgentError::Limit(_) => "模型运行超出限制".to_string(),
        AgentError::Provider(_) => "模型请求失败，请检查网络或模型服务配置".to_string(),
        _ => "模型请求失败".to_string(),
    }
}

/// Collapse whitespace and truncate to `max_chars`, matching the previous gateway exactly.
fn compact(content: &str, max_chars: usize) -> String {
    let mut compacted = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacted.chars().count() > max_chars {
        compacted = compacted.chars().take(max_chars).collect::<String>();
        compacted.push_str("...");
    }
    compacted
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::num::NonZeroUsize;
    use std::sync::mpsc::Receiver;

    use mira_ai::{AiError, AssistantEmitter, Provider, StreamHandle, StreamRequest};
    use serde_json::Value;

    fn model_config(provider: &str, model: &str, base_url: &str) -> ModelConfig {
        ModelConfig {
            id: "model-1".to_string(),
            provider: provider.to_string(),
            name: "Mock provider".to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key: Some("test-key".to_string()),
            credential_status: None,
            credential_error: None,
            is_default: true,
            created_at: None,
            updated_at: None,
        }
    }

    fn chat_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: "message-1".to_string(),
            conversation_id: "conversation-1".to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            reasoning: None,
        }
    }

    fn memory(fact: &str) -> Memory {
        Memory {
            id: 1,
            fact: fact.to_string(),
            source_conversation_id: None,
            memory_type: Some("saved".to_string()),
            importance: 8,
            confidence: 1.0,
            tags: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            last_used_at: None,
            use_count: 0,
            is_archived: false,
        }
    }

    // --- Prompt and selection parity -------------------------------------------------------

    #[test]
    fn system_prompt_matches_the_legacy_format() {
        let memories = vec![memory("likes tea"), memory("prefers short answers")];
        let project_context = vec![
            chat_message("user", "how do I build?"),
            chat_message("assistant", "run pnpm build"),
        ];

        let prompt = build_system_prompt(&memories, &project_context, "be brief");

        let expected = "你是 Mira，一个本地个人 AI 记忆客户端。请自然使用长期记忆和项目上下文，但不要逐条暴露。\n\n长期记忆：\n- likes tea\n- prefers short answers\n\n同项目其他对话上下文：\n- 用户: how do I build?\n- 助手: run pnpm build\n\n用户自定义指令：\nbe brief";
        assert_eq!(prompt, expected);
    }

    #[test]
    fn system_prompt_marks_missing_memory_and_project_context() {
        let prompt = build_system_prompt(&[], &[], "");

        assert!(prompt.contains("暂无长期记忆。"));
        assert!(prompt.contains("当前会话不在项目中，或项目内暂无其他对话上下文。"));
        assert!(!prompt.contains("用户自定义指令"));
    }

    #[test]
    fn compact_collapses_whitespace_and_truncates() {
        assert_eq!(compact("  a   b  ", 10), "a b");
        assert_eq!(compact("abcdef", 3), "abc...");
    }

    #[test]
    fn project_context_is_compacted_to_one_hundred_and_eighty_characters() {
        let long = "x".repeat(200);
        let prompt = build_system_prompt(&[], &[chat_message("user", &long)], "");

        let expected = format!("- 用户: {}...", "x".repeat(180));
        assert!(prompt.contains(&expected));
    }

    #[test]
    fn history_window_keeps_the_last_twenty_turns_in_order() {
        let history = (0..25)
            .map(|index| {
                chat_message(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    &format!("history-{index}"),
                )
            })
            .collect::<Vec<_>>();

        let window = history_window(&history);

        assert_eq!(window.len(), 20);
        assert_eq!(text_of(&window[0]), "history-5");
        assert_eq!(text_of(&window[19]), "history-24");
    }

    #[test]
    fn history_window_filters_unknown_roles_after_the_window() {
        let mut history = vec![
            chat_message("user", "old"),
            chat_message("assistant", "old reply"),
            chat_message("system", "not replayed"),
        ];
        history.extend((0..19).map(|index| chat_message("user", &format!("recent-{index}"))));

        let window = history_window(&history);

        // The unknown role consumed one of the twenty slots and was then dropped.
        assert_eq!(window.len(), 19);
        assert_eq!(text_of(&window[0]), "recent-0");
        assert!(window
            .iter()
            .all(|message| text_of(message) != "not replayed"));
    }

    #[test]
    fn restored_assistant_history_carries_only_text_and_unknown_provenance() {
        let message = history_assistant("previous answer");

        assert_eq!(message.content.len(), 1);
        assert!(matches!(message.content[0], AssistantContent::Text(_)));
        assert_eq!(message.source.provider, HISTORY_PROVENANCE);
        assert_eq!(message.source.model, HISTORY_PROVENANCE);
        assert_eq!(message.stop_reason, StopReason::EndTurn);
    }

    fn text_of(message: &Message) -> String {
        match message {
            Message::User(user) => user
                .content
                .iter()
                .filter_map(|part| match part {
                    mira_ai::InputContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Message::Assistant(assistant) => assistant.text(),
            Message::ToolResult(result) => format!("{result:?}"),
            _ => String::new(),
        }
    }

    // --- Explicit compatibility ------------------------------------------------------------

    #[test]
    fn deepseek_reasoner_selects_thinking_fields() {
        let config = model_config(
            "deepseek",
            "deepseek-reasoner",
            "https://api.deepseek.com/v1",
        );
        assert!(supports_deepseek_reasoning(&config));

        let options = consumer_options(true);
        let compat = consumer_compat(true);

        assert_eq!(options.reasoning, Some(ReasoningLevel::High));
        assert_eq!(options.max_retries, 2);
        assert_eq!(options.response_header_timeout, Duration::from_secs(45));
        assert_eq!(compat.reasoning, Some(ReasoningEncoding::DeepSeekThinking));
        assert!(compat.supports_reasoning_effort);
        assert!(!compat.supports_usage_in_streaming);
    }

    #[test]
    fn deepseek_chat_model_does_not_enable_reasoning() {
        let config = model_config("deepseek", "deepseek-chat", "https://api.deepseek.com/v1");
        assert!(!supports_deepseek_reasoning(&config));
        assert_eq!(consumer_options(false).reasoning, None);
    }

    #[test]
    fn other_models_omit_reasoning_fields() {
        let config = model_config("openai", "gpt-4o-mini", "https://api.example.com/v1");
        assert!(!supports_deepseek_reasoning(&config));

        let options = consumer_options(false);
        let compat = consumer_compat(false);

        assert_eq!(options.reasoning, None);
        assert_eq!(compat.reasoning, None);
        assert!(!compat.supports_reasoning_effort);
        assert!(!compat.supports_usage_in_streaming);
    }

    #[test]
    fn base_url_alone_marks_a_deepseek_endpoint() {
        let config = model_config("custom", "deepseek-reasoner", "https://api.deepseek.com/v1");
        assert!(is_deepseek_config(&config));
        assert!(supports_deepseek_reasoning(&config));
    }

    #[test]
    fn memory_options_are_deterministic_and_never_reasoning() {
        let options = memory_options();
        assert_eq!(options.temperature, Some(0.0));
        assert_eq!(options.reasoning, None);
        assert_eq!(options.max_retries, 2);
        assert_eq!(options.response_header_timeout, Duration::from_secs(45));
    }

    // --- Safe errors -----------------------------------------------------------------------

    #[test]
    fn safe_errors_are_chinese_categories_without_provider_prose() {
        let errors = [
            RuntimeError::Busy,
            RuntimeError::NoRuntime,
            RuntimeError::Timeout,
            RuntimeError::Credential,
            RuntimeError::Context,
            RuntimeError::UnknownBinding("x".to_string()),
            RuntimeError::Agent(AgentError::Provider(mira_runtime::ProviderFailure::Stream)),
            RuntimeError::Agent(AgentError::Limit(RunLimit::Turns)),
            RuntimeError::Internal,
        ];
        for error in errors {
            let message = safe_error(&error);
            assert!(!message.is_empty());
            // The categories carry no provider text, key, header or prompt.
            for needle in ["sk-", "Bearer", "authorization", "PRIVATE", "http"] {
                assert!(!message.contains(needle), "leaked '{needle}': {message}");
            }
            assert!(
                message
                    .chars()
                    .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
                "expected Chinese text: {message}"
            );
        }
        assert_eq!(safe_error(&RuntimeError::Cancelled), CANCELLED);
    }

    #[test]
    fn missing_api_key_is_reported_without_the_secret() {
        let mut config = model_config("openai", "gpt-4o-mini", "https://api.example.com/v1");
        config.api_key = None;
        assert_eq!(
            credential_for(&config).unwrap_err(),
            "模型配置 Mock provider 缺少 API Key"
        );

        config.api_key = Some("******".to_string());
        assert!(credential_for(&config).is_err());

        config.api_key = Some("sk-secret".to_string());
        let credential = credential_for(&config).expect("credential");
        assert_eq!(credential.expose_secret(), "sk-secret");
    }

    #[tokio::test]
    async fn the_bypass_resolver_refuses_instead_of_looking_a_key_up() {
        let resolver = BypassOnlyResolver;
        let request = CredentialRequest {
            binding: "model-1".to_string(),
            model: Model::new(Api::OpenAiCompletions, "openai", "gpt-4o-mini"),
            credential_id: CredentialId::new("model-1"),
        };
        let result = resolver.resolve(&request, CancellationToken::new()).await;
        assert!(result.is_err());
    }

    // --- Loopback fixtures against the real adapter ----------------------------------------

    /// Start a one-shot loopback HTTP server and return its base URL and the captured request.
    fn spawn_loopback(response_body: String) -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(header_end) = find(&buffer, b"\r\n\r\n") {
                            let headers =
                                String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
                            let content_length = headers
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length:"))
                                .and_then(|value| value.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if buffer.len() >= header_end + 4 + content_length {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = sender.send(String::from_utf8_lossy(&buffer).to_string());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{response_body}"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (format!("http://127.0.0.1:{port}"), receiver)
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn body_of(raw_request: &str) -> Value {
        let body = raw_request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("request body");
        serde_json::from_str(body).expect("json body")
    }

    fn sse(deltas: &[&str]) -> String {
        let mut body = String::new();
        for delta in deltas {
            body.push_str(&format!("data: {delta}\n\n"));
        }
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
        body.push_str("data: [DONE]\n\n");
        body
    }

    #[tokio::test]
    async fn loopback_chat_request_carries_the_legacy_prompt_and_omits_stream_options() {
        let (base_url, requests) = spawn_loopback(sse(&[
            "{\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}",
        ]));
        let config = model_config("openai", "gpt-4o-mini", &base_url);
        let history = (0..25)
            .map(|index| {
                chat_message(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    &format!("history-{index}"),
                )
            })
            .collect::<Vec<_>>();
        let project_context = vec![chat_message("assistant", "project context")];
        let memories = vec![memory("likes tea")];
        let cancel_state = CancellationState::new();
        let attempt = cancel_state.reset();

        let mut streamed = String::new();
        let mut reasoning = String::new();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            complete_chat_streaming(
                &config,
                ChatRequest {
                    history: &history,
                    project_context: &project_context,
                    injected_memories: &memories,
                    user_content: "latest question",
                    system_prompt_extra: "be brief",
                },
                |delta| {
                    streamed.push_str(delta);
                    Ok(())
                },
                |delta| {
                    reasoning.push_str(delta);
                    Ok(())
                },
                &cancel_state,
                &attempt,
            ),
        )
        .await
        .expect("bounded");

        assert_eq!(result.expect("chat"), "hello");
        assert_eq!(streamed, "hello");
        assert!(reasoning.is_empty());

        let body = body_of(
            &requests
                .recv_timeout(Duration::from_secs(5))
                .expect("captured"),
        );
        assert_eq!(body["stream"], true);
        assert!(body.get("stream_options").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());

        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 22);
        assert_eq!(
            messages[0]["content"],
            build_system_prompt(&memories, &project_context, "be brief")
        );
        assert_eq!(messages[1]["content"], "history-5");
        assert_eq!(messages[21]["role"], "user");
        assert_eq!(messages[21]["content"], "latest question");
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[tokio::test]
    async fn loopback_deepseek_request_sends_thinking_and_separates_reasoning() {
        let (base_url, requests) = spawn_loopback(sse(&[
            "{\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"},\"finish_reason\":null}]}",
            "{\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}",
        ]));
        let config = model_config("deepseek", "deepseek-reasoner", &base_url);
        let cancel_state = CancellationState::new();
        let attempt = cancel_state.reset();

        let mut streamed = String::new();
        let mut reasoning = String::new();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            complete_chat_streaming(
                &config,
                ChatRequest {
                    history: &[],
                    project_context: &[],
                    injected_memories: &[],
                    user_content: "question",
                    system_prompt_extra: "",
                },
                |delta| {
                    streamed.push_str(delta);
                    Ok(())
                },
                |delta| {
                    reasoning.push_str(delta);
                    Ok(())
                },
                &cancel_state,
                &attempt,
            ),
        )
        .await
        .expect("bounded");

        assert_eq!(result.expect("chat"), "hello");
        assert_eq!(streamed, "hello");
        assert_eq!(reasoning, "think");

        let body = body_of(
            &requests
                .recv_timeout(Duration::from_secs(5))
                .expect("captured"),
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("stream_options").is_none());
    }

    #[tokio::test]
    async fn loopback_background_request_uses_the_planner_prompt_and_temperature() {
        let (base_url, requests) = spawn_loopback(sse(&[
            "{\"choices\":[{\"delta\":{\"content\":\"{\\\"memories\\\":[]}\"},\"finish_reason\":null}]}",
        ]));
        let config = model_config("deepseek", "deepseek-reasoner", &base_url);
        let system_prompt = crate::memory::MEMORY_PLANNER_PROMPT;

        let content = tokio::time::timeout(
            Duration::from_secs(10),
            complete_background_text(&config, system_prompt, "用户消息：\nhi"),
        )
        .await
        .expect("bounded")
        .expect("planner text");

        assert_eq!(content, "{\"memories\":[]}");

        let body = body_of(
            &requests
                .recv_timeout(Duration::from_secs(5))
                .expect("captured"),
        );
        assert_eq!(body["temperature"], 0.0);
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("stream_options").is_none());
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], system_prompt);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "用户消息：\nhi");
    }

    // --- Event mapping, cancellation and failure handling ----------------------------------

    struct StallingProvider;

    impl Provider for StallingProvider {
        fn stream(&self, _request: StreamRequest) -> Result<StreamHandle, AiError> {
            let (producer, handle) = AssistantEmitter::new(NonZeroUsize::new(1).expect("non-zero"));
            tokio::spawn(async move {
                // Hold the producer open, so the stream never ends until it is cancelled.
                producer.cancellation().cancelled().await;
            });
            Ok(handle)
        }
    }

    struct DeltaThenStallProvider;

    impl Provider for DeltaThenStallProvider {
        fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError> {
            let (producer, handle) = AssistantEmitter::new(NonZeroUsize::new(1).expect("non-zero"));
            let source = AssistantSource {
                api: request.model.api,
                provider: request.model.provider.clone(),
                model: request.model.id.clone(),
                response_model: None,
                response_id: None,
            };
            tokio::spawn(async move {
                let _ = producer.emit(AssistantEvent::Start { source }).await;
                let _ = producer
                    .emit(AssistantEvent::BlockStart {
                        index: 0,
                        kind: BlockKind::Text,
                    })
                    .await;
                let _ = producer
                    .emit(AssistantEvent::BlockDelta {
                        index: 0,
                        delta: "hello".into(),
                    })
                    .await;
                producer.cancellation().cancelled().await;
            });
            Ok(handle)
        }
    }

    fn test_session(provider: Arc<dyn Provider>) -> Session {
        let binding = ModelBinding::new(
            "test",
            Model::new(Api::OpenAiCompletions, "test", "test-model"),
            provider,
            CredentialId::new("test"),
        );
        let registry = Arc::new(ModelRegistry::new(vec![binding]).expect("registry"));
        Session::new(SessionConfig::new(
            registry,
            "test",
            Arc::new(BypassOnlyResolver),
        ))
        .expect("session")
    }

    fn fake_run(provider: Arc<dyn Provider>) -> SessionRun {
        test_session(provider)
            .prompt_with_credential("hi", Credential::new("test-key"))
            .expect("run")
    }

    #[test]
    fn text_and_reasoning_deltas_stay_separate() {
        let mut kinds = HashMap::new();
        let mut text = Vec::new();
        let mut reasoning = Vec::new();

        {
            let mut on_delta = |value: &str| {
                text.push(value.to_string());
                Ok(())
            };
            let mut on_reasoning = |value: &str| {
                reasoning.push(value.to_string());
                Ok(())
            };
            let events = [
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockStart {
                        index: 0,
                        kind: BlockKind::Thinking,
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockStart {
                        index: 1,
                        kind: BlockKind::Text,
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockDelta {
                        index: 0,
                        delta: "think".into(),
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockDelta {
                        index: 1,
                        delta: "hello".into(),
                    },
                },
            ];
            for event in &events {
                forward_event(event, &mut kinds, &mut on_delta, &mut on_reasoning)
                    .expect("forwarded");
            }
        }

        assert_eq!(text, vec!["hello".to_string()]);
        assert_eq!(reasoning, vec!["think".to_string()]);
    }

    #[test]
    fn tool_call_argument_deltas_are_not_forwarded_as_text() {
        let mut kinds = HashMap::new();
        let mut text = Vec::new();
        let mut reasoning = Vec::new();

        {
            let mut on_delta = |value: &str| {
                text.push(value.to_string());
                Ok(())
            };
            let mut on_reasoning = |value: &str| {
                reasoning.push(value.to_string());
                Ok(())
            };
            let events = [
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockStart {
                        index: 0,
                        kind: BlockKind::ToolCall,
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockDelta {
                        index: 0,
                        delta: "{\"a\":1}".into(),
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockStart {
                        index: 1,
                        kind: BlockKind::Text,
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockDelta {
                        index: 1,
                        delta: "hello".into(),
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockStart {
                        index: 2,
                        kind: BlockKind::Thinking,
                    },
                },
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockDelta {
                        index: 2,
                        delta: "think".into(),
                    },
                },
                // A delta with no recorded block start is never exposed either.
                AgentEvent::MessageUpdate {
                    turn: 1,
                    update: AssistantEvent::BlockDelta {
                        index: 9,
                        delta: "orphan".into(),
                    },
                },
            ];
            for event in &events {
                forward_event(event, &mut kinds, &mut on_delta, &mut on_reasoning)
                    .expect("forwarded");
            }
        }

        assert_eq!(text, vec!["hello".to_string()]);
        assert_eq!(reasoning, vec!["think".to_string()]);
    }

    #[tokio::test]
    async fn a_stale_attempt_returns_cancelled_without_starting_a_run() {
        let cancel_state = CancellationState::new();
        let stale = cancel_state.reset();
        let _current = cancel_state.reset();
        let config = model_config("openai", "gpt-4o-mini", "https://api.example.com/v1");

        let result = complete_chat_streaming(
            &config,
            ChatRequest {
                history: &[],
                project_context: &[],
                injected_memories: &[],
                user_content: "hi",
                system_prompt_extra: "",
            },
            |_| Ok(()),
            |_| Ok(()),
            &cancel_state,
            &stale,
        )
        .await;

        assert_eq!(result.unwrap_err(), CANCELLED);
    }

    #[tokio::test]
    async fn cancelling_the_registered_token_stops_a_stalled_run() {
        let cancel_state = CancellationState::new();
        let attempt = cancel_state.reset();
        let run = fake_run(Arc::new(StallingProvider));
        let _registration = cancel_state
            .register(&attempt, run.cancellation())
            .expect("current attempt");

        cancel_state.cancel();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_chat_run(run, |_| Ok(()), |_| Ok(())),
        )
        .await
        .expect("cancellation must not wait for a byte");
        assert_eq!(result.unwrap_err(), CANCELLED);
    }

    #[tokio::test]
    async fn cancelling_before_the_run_starts_is_applied_to_the_new_token() {
        let cancel_state = CancellationState::new();
        let attempt = cancel_state.reset();
        cancel_state.cancel();
        let run = fake_run(Arc::new(StallingProvider));
        let _registration = cancel_state
            .register(&attempt, run.cancellation())
            .expect("current attempt");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_chat_run(run, |_| Ok(()), |_| Ok(())),
        )
        .await
        .expect("a cancel that raced the start must still apply");
        assert_eq!(result.unwrap_err(), CANCELLED);
    }

    #[tokio::test]
    async fn a_failed_callback_cancels_and_drains_the_run() {
        let run = fake_run(Arc::new(DeltaThenStallProvider));

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_chat_run(run, |_| Err("无法发送流式消息事件".to_string()), |_| Ok(())),
        )
        .await
        .expect("callback failure must not hang");

        assert_eq!(result.unwrap_err(), "无法发送流式消息事件");
    }

    #[tokio::test]
    async fn foreground_cancel_does_not_cancel_a_background_run() {
        let cancel_state = CancellationState::new();
        let attempt = cancel_state.reset();
        let chat_run = fake_run(Arc::new(StallingProvider));
        let _registration = cancel_state
            .register(&attempt, chat_run.cancellation())
            .expect("current attempt");

        // A background run owns an independent session and token that is never registered.
        let background_session = test_session(Arc::new(DeltaThenStallProvider));
        let background_run = background_session
            .prompt_with_credential("memory", Credential::new("test-key"))
            .expect("run");

        cancel_state.cancel();

        let chat_result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_chat_run(chat_run, |_| Ok(()), |_| Ok(())),
        )
        .await
        .expect("chat cancellation");
        assert_eq!(chat_result.unwrap_err(), CANCELLED);

        assert!(!background_run.is_cancelled());
        background_run.cancel();
        let _ = background_run.outcome().await;
    }
}
