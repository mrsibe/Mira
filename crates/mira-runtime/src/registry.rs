//! Explicit model bindings and the immutable registry a session selects from.
//!
//! The registry is caller-constructed and validated once. It never discovers a provider, reads a
//! base URL from the environment, generates a catalog or guesses token limits, pricing or
//! capabilities: every binding is an explicit statement by the caller. A credential is never
//! stored here — a binding carries only a lookup identifier that the caller's
//! [`CredentialResolver`](crate::CredentialResolver) can interpret.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use mira_ai::{Model, Provider, RequestOptions};

use crate::error::RuntimeError;

/// Caller-defined lookup identifier for a binding's credential.
///
/// This is metadata, never a credential: a resolver receives it to decide which credential to
/// return. It is printed by `Debug` and may be shown to a user, so it must not contain a secret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialId(String);

impl CredentialId {
    /// Wrap a caller-defined identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as written by the caller.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One selectable model: its identity, the provider that serves it, its default request options
/// and the identifier of the credential a resolver should return for it.
///
/// The `Model` is the identity only — API family, registry provider label, model id and declared
/// capabilities. The binding never stores a token limit, a price or a credential.
///
/// The provider is the exact `Arc<dyn Provider>` the caller injected. The registry never
/// constructs one from a name or a base URL.
pub struct ModelBinding {
    key: String,
    model: Model,
    provider: Arc<dyn Provider>,
    options: RequestOptions,
    credential_id: CredentialId,
}

impl ModelBinding {
    /// A binding with baseline request options.
    pub fn new(
        key: impl Into<String>,
        model: Model,
        provider: Arc<dyn Provider>,
        credential_id: CredentialId,
    ) -> Self {
        Self {
            key: key.into(),
            model,
            provider,
            options: RequestOptions::default(),
            credential_id,
        }
    }

    /// Replace the default request options of this binding.
    pub fn with_options(mut self, options: RequestOptions) -> Self {
        self.options = options;
        self
    }

    /// Caller-defined key this binding is registered and selected under.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Model identity addressed by this binding.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Provider that serves this binding.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Default request options applied when a session does not override them.
    pub fn options(&self) -> &RequestOptions {
        &self.options
    }

    /// Identifier the resolver receives to look this binding's credential up.
    pub fn credential_id(&self) -> &CredentialId {
        &self.credential_id
    }
}

/// The provider is not printable, and a binding never holds a credential, so `Debug` reports the
/// key, the model identity, the option struct and the credential lookup identifier.
impl fmt::Debug for ModelBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelBinding")
            .field("key", &self.key)
            .field("model", &self.model.id)
            .field("api", &self.model.api)
            .field("provider", &self.model.provider)
            .field("options", &self.options)
            .field("credential_id", &self.credential_id)
            .field("provider_impl", &"<injected>")
            .finish()
    }
}

/// An immutable set of [`ModelBinding`]s with unique keys.
///
/// Constructed once and then shared as `Arc<ModelRegistry>`; there is no way to register a
/// provider after construction and no global mutable registry.
#[derive(Default)]
pub struct ModelRegistry {
    bindings: Vec<Arc<ModelBinding>>,
    index: HashMap<String, usize>,
}

impl ModelRegistry {
    /// Build a registry, rejecting duplicate and empty keys.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::DuplicateBinding`] when two bindings share a key and
    /// [`RuntimeError::InvalidBinding`] when a key or a model id is empty.
    pub fn new(bindings: Vec<ModelBinding>) -> Result<Self, RuntimeError> {
        let mut registry = Self::default();
        for binding in bindings {
            if binding.key.is_empty() || binding.model.id.is_empty() {
                return Err(RuntimeError::InvalidBinding(binding.key));
            }
            if registry.index.contains_key(&binding.key) {
                return Err(RuntimeError::DuplicateBinding(binding.key));
            }
            registry
                .index
                .insert(binding.key.clone(), registry.bindings.len());
            registry.bindings.push(Arc::new(binding));
        }
        Ok(registry)
    }

    /// A registry with a single binding, as a convenience for one-model consumers.
    ///
    /// # Errors
    ///
    /// The same validation as [`ModelRegistry::new`].
    pub fn single(binding: ModelBinding) -> Result<Self, RuntimeError> {
        Self::new(vec![binding])
    }

    /// The binding registered under `key`.
    pub fn get(&self, key: &str) -> Option<&Arc<ModelBinding>> {
        self.index
            .get(key)
            .and_then(|position| self.bindings.get(*position))
    }

    /// Whether a binding is registered under `key`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// Registered keys, in registration order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.bindings.iter().map(|binding| binding.key.as_str())
    }

    /// Number of registered bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether no binding is registered.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

impl fmt::Debug for ModelRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelRegistry")
            .field("bindings", &self.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mira_ai::Api;
    use std::num::NonZeroUsize;

    struct NullProvider;

    impl Provider for NullProvider {
        fn stream(
            &self,
            _request: mira_ai::StreamRequest,
        ) -> Result<mira_ai::StreamHandle, mira_ai::AiError> {
            let (mut producer, handle) =
                mira_ai::AssistantEmitter::new(NonZeroUsize::new(1).expect("non-zero"));
            producer.finish(Err(mira_ai::AiError::Cancelled));
            Ok(handle)
        }
    }

    fn binding(key: &str, model: &str) -> ModelBinding {
        ModelBinding::new(
            key,
            Model::new(Api::OpenAiCompletions, "test-provider", model),
            Arc::new(NullProvider),
            CredentialId::new(format!("credential-{key}")),
        )
    }

    #[test]
    fn rejects_duplicate_and_empty_keys() {
        assert_eq!(
            ModelRegistry::new(vec![binding("fast", "a"), binding("fast", "b")]).unwrap_err(),
            RuntimeError::DuplicateBinding("fast".to_string())
        );
        assert_eq!(
            ModelRegistry::new(vec![binding("", "a")]).unwrap_err(),
            RuntimeError::InvalidBinding(String::new())
        );
    }

    #[test]
    fn keeps_registration_order_and_looks_up_by_key() {
        let registry = ModelRegistry::new(vec![binding("fast", "a"), binding("deep", "b")])
            .expect("valid registry");
        assert_eq!(
            registry.keys().collect::<Vec<_>>(),
            vec!["fast".to_string(), "deep".to_string()]
        );
        assert!(registry.contains_key("deep"));
        assert_eq!(
            registry
                .get("deep")
                .map(|binding| binding.model().id.as_str()),
            Some("b")
        );
        assert!(registry.get("missing").is_none());
    }
}
