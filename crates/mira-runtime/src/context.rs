//! Caller-supplied context composition for one request.
//!
//! A [`ContextManager`] composes the output of zero or more [`ContextProvider`]s into a derived
//! request context. It runs inside the child agent's `ContextTransform` seam, so it sees the
//! canonical transcript by immutable reference and returns a context for exactly one request: an
//! injected snippet is never written into the canonical transcript and is therefore never
//! replayed, duplicated across a tool loop, or persisted.
//!
//! There is no built-in memory, project or attachment source and no implicit network access.
//! Concrete sources are application-owned and injected explicitly.
//!
//! # Trust
//!
//! A [`ContextItem`]'s text is data, not policy. It is labeled and added to the derived system
//! prompt, so a provider that returns attacker-controlled text can influence the model. This
//! crate never turns a context item into an executable tool and never changes the tool policy:
//! the tool set of a run is exactly the selected agent's registry. Callers that surface untrusted
//! text through a provider own the decision to do so.
//!
//! # Ordering and budget
//!
//! Selection is deterministic: items are ordered by descending [`ContextItem::priority`], then by
//! provider registration order, then by the order the provider returned them. Items are included
//! whole while the accumulated **rendered** character count stays within
//! [`ContextManager::max_injected_chars`]; an item that does not fit is skipped, and later smaller
//! items may still be included.
//!
//! The budget counts every character the composition adds to the derived system prompt: the
//! provenance label, the item text and the separators between snippets (and the separator after a
//! preserved base prompt). A long label therefore cannot smuggle text past the ceiling. The count
//! is in Unicode scalar values, not bytes and not tokens, and it never truncates the canonical
//! current user message or the canonical history — only the injected snippets are selected.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use mira_agent::{
    CancellationToken, ContextTransform, ContextTransformError, ContextTransformFuture,
};
use mira_ai::{Context, InputContent, Message, Model, Usage};

/// Default ceiling for the characters a manager may inject into one request.
pub const DEFAULT_MAX_INJECTED_CHARS: usize = 16 * 1024;

/// Constant failure of a context provider.
///
/// The type carries no prose on purpose: a provider may record its own diagnostics elsewhere, and
/// the runtime only needs a safe category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a context provider failed")]
pub struct ContextError;

/// Future returned by [`ContextProvider::provide`].
pub type ContextProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ContextItem>, ContextError>> + Send + 'a>>;

/// Immutable description of the request a provider is asked to enrich.
///
/// It carries no desktop entity, database row or credential: the selected model identity, the
/// latest original user query and the canonical context of this request only.
#[derive(Debug)]
pub struct ContextRequest<'a> {
    /// Model identity selected for this run.
    pub model: &'a Model,
    /// Latest original user message of the canonical transcript, when there is one.
    pub query: Option<String>,
    /// Canonical request context: system prompt, history and tool declarations.
    pub context: &'a Context,
}

/// One labeled text snippet a provider offers for injection.
///
/// `source` and `id` are provenance labels, `priority` is the selection weight (higher is kept
/// first) and `token_estimate` is informational only: the runtime never uses it for budgeting, so
/// an estimate is never presented as an exact token count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    /// Provider or source identifier shown as the snippet's provenance.
    pub source: String,
    /// Identifier of the item within its source.
    pub id: String,
    /// Text offered for injection. May be untrusted data.
    pub text: String,
    /// Selection weight; higher priority is included first.
    pub priority: i32,
    /// Informational token estimate, if the provider has one. Never used for budgeting.
    pub token_estimate: Option<u64>,
}

impl ContextItem {
    /// An item at priority `0` with no token estimate.
    pub fn new(source: impl Into<String>, id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            id: id.into(),
            text: text.into(),
            priority: 0,
            token_estimate: None,
        }
    }

    /// Set the selection weight.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Attach an informational token estimate.
    pub fn with_token_estimate(mut self, token_estimate: u64) -> Self {
        self.token_estimate = Some(token_estimate);
        self
    }
}

/// Result of one successful composition.
///
/// `injected_chars` is exactly the number of characters the composition added to the system
/// prompt: the derived prompt's character count minus the canonical one's. It is bounded by
/// `max_injected_chars`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextComposition {
    /// Derived context for one request. The canonical context is untouched.
    pub context: Context,
    /// Characters added to the derived system prompt.
    pub injected_chars: usize,
    /// Snippets that were injected.
    pub injected_items: usize,
    /// Ceiling this composition was bounded by.
    pub max_injected_chars: usize,
}

/// Counts reported about the last context composition of a session.
///
/// `model_usage` is the last usage a provider actually reported for a committed assistant turn,
/// or `None` when no provider reported one: the runtime never fabricates zero counts and never
/// converts characters into tokens. `injected_chars` counts every character the composition added
/// to the derived system prompt, bounded by `max_injected_chars`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextUsage {
    /// Last usage reported by a provider, if any.
    pub model_usage: Option<Usage>,
    /// Characters injected into the derived system prompt by the last composition.
    pub injected_chars: usize,
    /// Snippets injected by the last composition.
    pub injected_items: usize,
    /// Ceiling the last composition was bounded by.
    pub max_injected_chars: usize,
}

/// A source of context snippets for one request.
///
/// Implementations must observe `cancellation`, must return a value instead of panicking and must
/// not perform blocking I/O. A provider has no network of its own: any retrieval is the caller's
/// explicit decision and must be cancelled with the token.
pub trait ContextProvider: Send + Sync {
    /// Offer snippets for one request.
    fn provide<'a>(
        &'a self,
        request: &'a ContextRequest<'a>,
        cancellation: CancellationToken,
    ) -> ContextProviderFuture<'a>;
}

/// Composes [`ContextProvider`]s into one derived request context under a character budget.
///
/// The manager itself is immutable and stateless, so several sessions may share one
/// `Arc<ContextManager>`: each composition reports its own counts to its caller.
pub struct ContextManager {
    providers: Vec<Arc<dyn ContextProvider>>,
    max_injected_chars: usize,
}

impl ContextManager {
    /// A manager over the given providers, in registration order.
    pub fn new(providers: Vec<Arc<dyn ContextProvider>>) -> Self {
        Self {
            providers,
            max_injected_chars: DEFAULT_MAX_INJECTED_CHARS,
        }
    }

    /// A manager with no providers: it returns every request context unchanged.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Replace the injected-character ceiling.
    pub fn with_max_injected_chars(mut self, max_injected_chars: usize) -> Self {
        self.max_injected_chars = max_injected_chars;
        self
    }

    /// The configured injected-character ceiling.
    pub fn max_injected_chars(&self) -> usize {
        self.max_injected_chars
    }

    /// Registered providers, in registration order.
    pub fn providers(&self) -> &[Arc<dyn ContextProvider>] {
        &self.providers
    }

    /// Derive the context for one request.
    ///
    /// The returned context keeps the canonical system prompt, history and tool declarations and
    /// appends the selected snippets once to the system prompt. The base context is left
    /// untouched, so the canonical transcript is never modified.
    ///
    /// # Errors
    ///
    /// Returns the constant [`ContextError`] when a provider fails or the token is already
    /// cancelled. The failure carries no provider prose.
    pub async fn compose<'a>(
        &'a self,
        request: &'a ContextRequest<'a>,
        cancellation: CancellationToken,
    ) -> Result<ContextComposition, ContextError> {
        if cancellation.is_cancelled() {
            return Err(ContextError);
        }
        let mut offered: Vec<(usize, usize, ContextItem)> = Vec::new();
        for (provider_index, provider) in self.providers.iter().enumerate() {
            let items = provider
                .provide(request, cancellation.clone())
                .await
                .map_err(|_| ContextError)?;
            for (item_index, item) in items.into_iter().enumerate() {
                offered.push((provider_index, item_index, item));
            }
        }
        // Higher priority first, then provider registration order, then the provider's item order.
        // The comparator is total, so the selection is deterministic for any provider input.
        offered.sort_by(|left, right| {
            right
                .2
                .priority
                .cmp(&left.2.priority)
                .then(left.0.cmp(&right.0))
                .then(left.1.cmp(&right.1))
        });

        let base_prompt = request.context.system_prompt.as_deref();
        let base_present = base_prompt.is_some_and(|prompt| !prompt.is_empty());

        // Build the injected text and count it as it is actually rendered: label, text and the
        // separators included. Nothing is appended that the ceiling did not pay for.
        let mut injected = String::new();
        let mut injected_chars = 0usize;
        let mut injected_items = 0usize;
        for (_, _, item) in offered {
            let rendered = render_snippet(&item);
            let separator = if injected_items == 0 {
                if base_present {
                    "\n\n"
                } else {
                    ""
                }
            } else {
                "\n\n"
            };
            let added = char_count(separator).saturating_add(char_count(&rendered));
            if injected_chars.saturating_add(added) > self.max_injected_chars {
                continue;
            }
            injected.push_str(separator);
            injected.push_str(&rendered);
            injected_chars += added;
            injected_items += 1;
        }

        let mut context = request.context.clone();
        if injected_items > 0 {
            context.system_prompt = Some(match base_prompt {
                Some(existing) if !existing.is_empty() => format!("{existing}{injected}"),
                _ => injected,
            });
        }
        Ok(ContextComposition {
            context,
            injected_chars,
            injected_items,
            max_injected_chars: self.max_injected_chars,
        })
    }
}

impl fmt::Debug for ContextManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextManager")
            .field("providers", &self.providers.len())
            .field("max_injected_chars", &self.max_injected_chars)
            .finish()
    }
}

/// Render one snippet exactly as it is added to the derived system prompt.
fn render_snippet(item: &ContextItem) -> String {
    format!(
        "[context source={} id={}]\n{}",
        item.source, item.id, item.text
    )
}

/// Number of Unicode scalar values in `text`.
fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// The latest original user message of a canonical transcript, when there is one.
///
/// Text parts of the most recent user turn are joined with a newline. Tool results and assistant
/// turns are ignored, so a tool loop still reports the prompt that started it.
pub fn latest_user_query(context: &Context) -> Option<String> {
    context
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::User(user) => {
                let text = user
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        InputContent::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(text)
            }
            _ => None,
        })
        .filter(|text| !text.is_empty())
}

/// Per-session composition counts, written by the session's context transform.
pub(crate) type UsageSlot = Arc<Mutex<ContextUsage>>;

/// A fresh, empty counts slot.
pub(crate) fn new_usage_slot() -> UsageSlot {
    Arc::new(Mutex::new(ContextUsage::default()))
}

/// Read the counts of a slot.
pub(crate) fn read_usage(slot: &UsageSlot) -> ContextUsage {
    *lock_usage(slot)
}

/// Lock a counts slot, ignoring poisoning: no lock is held across an await, so a panic cannot
/// leave it half-updated.
pub(crate) fn lock_usage(slot: &UsageSlot) -> MutexGuard<'_, ContextUsage> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The child agent's `ContextTransform`: binds a manager to the model and counts slot of one
/// session.
pub(crate) struct BoundContextTransform {
    manager: Arc<ContextManager>,
    model: Model,
    usage: UsageSlot,
}

impl BoundContextTransform {
    pub(crate) fn new(manager: Arc<ContextManager>, model: Model, usage: UsageSlot) -> Self {
        Self {
            manager,
            model,
            usage,
        }
    }
}

impl ContextTransform for BoundContextTransform {
    fn transform<'a>(
        &'a self,
        context: &'a Context,
        cancellation: CancellationToken,
    ) -> ContextTransformFuture<'a> {
        Box::pin(async move {
            let request = ContextRequest {
                model: &self.model,
                query: latest_user_query(context),
                context,
            };
            let composition = self
                .manager
                .compose(&request, cancellation)
                .await
                .map_err(|_| ContextTransformError)?;
            // The counts belong to this session only, so sharing one manager across sessions
            // cannot mix their metrics.
            *lock_usage(&self.usage) = ContextUsage {
                model_usage: None,
                injected_chars: composition.injected_chars,
                injected_items: composition.injected_items,
                max_injected_chars: composition.max_injected_chars,
            };
            Ok(composition.context)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mira_ai::{Api, TextBlock, UserMessage};

    struct FixedProvider {
        items: Vec<ContextItem>,
    }

    impl ContextProvider for FixedProvider {
        fn provide<'a>(
            &'a self,
            _request: &'a ContextRequest<'a>,
            _cancellation: CancellationToken,
        ) -> ContextProviderFuture<'a> {
            let items = self.items.clone();
            Box::pin(async move { Ok(items) })
        }
    }

    fn base_context() -> Context {
        Context {
            system_prompt: Some("be brief".to_string()),
            messages: vec![Message::User(UserMessage {
                content: vec![InputContent::Text(TextBlock::new("hello"))],
            })],
            tools: Vec::new(),
        }
    }

    fn request<'a>(context: &'a Context, model: &'a Model) -> ContextRequest<'a> {
        ContextRequest {
            model,
            query: latest_user_query(context),
            context,
        }
    }

    /// Characters the rendered snippet adds, including its label and its leading separator.
    fn rendered_cost(item: &ContextItem, base_present: bool) -> usize {
        let separator = if base_present { 2 } else { 0 };
        separator + char_count(&render_snippet(item))
    }

    async fn compose(manager: &ContextManager, context: &Context) -> ContextComposition {
        let model = Model::new(Api::OpenAiCompletions, "p", "m");
        let request = request(context, &model);
        manager
            .compose(&request, CancellationToken::new())
            .await
            .expect("composes")
    }

    #[tokio::test]
    async fn orders_by_priority_then_registration_then_item_order() {
        let first = Arc::new(FixedProvider {
            items: vec![
                ContextItem::new("first", "a", "low"),
                ContextItem::new("first", "b", "high").with_priority(5),
            ],
        });
        let second = Arc::new(FixedProvider {
            items: vec![ContextItem::new("second", "c", "tie").with_priority(5)],
        });
        let manager = ContextManager::new(vec![first, second]);
        let context = base_context();
        let composition = compose(&manager, &context).await;
        let prompt = composition.context.system_prompt.expect("system prompt");
        let high = prompt.find("high").expect("high present");
        let tie = prompt.find("tie").expect("tie present");
        let low = prompt.find("low").expect("low present");
        assert!(high < tie && tie < low, "unexpected order: {prompt}");
        assert!(prompt.starts_with("be brief"));
        assert_eq!(composition.injected_items, 3);
        assert_eq!(
            composition.injected_chars,
            prompt.chars().count() - "be brief".chars().count()
        );
    }

    #[tokio::test]
    async fn skips_items_over_the_budget_without_truncating_them() {
        let big = ContextItem::new("s", "big", "0123456789");
        let small = ContextItem::new("s", "small", "ab");
        // Exactly enough for the small snippet as it is rendered, label included.
        let budget = rendered_cost(&small, true);
        assert!(budget < rendered_cost(&big, true));
        let provider = Arc::new(FixedProvider {
            items: vec![big, small.clone()],
        });
        let manager = ContextManager::new(vec![provider]).with_max_injected_chars(budget);
        let composition = compose(&manager, &base_context()).await;
        let prompt = composition.context.system_prompt.expect("system prompt");
        assert!(!prompt.contains("0123456789"));
        assert!(prompt.contains("ab"));
        assert_eq!(composition.injected_chars, budget);
        assert_eq!(composition.injected_items, 1);
    }

    #[tokio::test]
    async fn an_empty_selection_is_an_exact_no_op() {
        let manager = ContextManager::empty();
        let context = base_context();
        let composition = compose(&manager, &context).await;
        assert_eq!(composition.context, context);
        assert_eq!(composition.injected_chars, 0);
        assert_eq!(composition.injected_items, 0);
    }

    #[tokio::test]
    async fn a_long_label_cannot_bypass_the_injection_budget() {
        let item = ContextItem::new("s".repeat(200), "i".repeat(200), "");
        let provider = Arc::new(FixedProvider {
            items: vec![item.clone()],
        });
        let manager = ContextManager::new(vec![provider]).with_max_injected_chars(16);
        let composition = compose(&manager, &base_context()).await;
        assert_eq!(composition.injected_items, 0);
        assert_eq!(composition.injected_chars, 0);
        assert_eq!(
            composition.context.system_prompt.as_deref(),
            Some("be brief")
        );
    }

    #[tokio::test]
    async fn unicode_snippets_are_budgeted_by_characters_not_bytes() {
        let item = ContextItem::new("s", "1", "日本語");
        let cost = rendered_cost(&item, true);
        assert!(cost < render_snippet(&item).len(), "the text is multi-byte");

        let provider = Arc::new(FixedProvider {
            items: vec![item.clone()],
        });
        let exact = ContextManager::new(vec![Arc::clone(&provider) as Arc<dyn ContextProvider>])
            .with_max_injected_chars(cost);
        let composition = compose(&exact, &base_context()).await;
        assert_eq!(composition.injected_items, 1);
        assert_eq!(composition.injected_chars, cost);

        let tight = ContextManager::new(vec![provider]).with_max_injected_chars(cost - 1);
        let composition = compose(&tight, &base_context()).await;
        assert_eq!(composition.injected_items, 0);
        assert_eq!(composition.injected_chars, 0);
    }

    #[tokio::test]
    async fn a_base_prompt_separator_counts_against_the_budget() {
        let item = ContextItem::new("s", "1", "ab");
        // The rendered snippet alone fits, but the separator after the preserved base does not.
        let provider = Arc::new(FixedProvider {
            items: vec![item.clone()],
        });
        let without_separator = char_count(&render_snippet(&item));
        let manager =
            ContextManager::new(vec![provider]).with_max_injected_chars(without_separator);
        let composition = compose(&manager, &base_context()).await;
        assert_eq!(composition.injected_items, 0);

        // The same snippet is injected when there is no base prompt to separate from.
        let provider = Arc::new(FixedProvider { items: vec![item] });
        let manager =
            ContextManager::new(vec![provider]).with_max_injected_chars(without_separator);
        let composition = compose(&manager, &Context::new()).await;
        assert_eq!(composition.injected_items, 1);
        assert_eq!(composition.injected_chars, without_separator);
    }
}
