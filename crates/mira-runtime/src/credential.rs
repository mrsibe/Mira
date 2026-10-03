//! The caller-owned credential seam.
//!
//! The runtime never reads the environment, a file, a database or an OS keyring. A credential is
//! produced per run by a caller-supplied [`CredentialResolver`] and is used for that run only: it
//! is never stored in the registry, a session snapshot, the session configuration, an event, an
//! error or a `Debug` rendering.

use std::future::Future;
use std::pin::Pin;

use mira_ai::{Credential, Model};
use tokio_util::sync::CancellationToken;

use crate::registry::CredentialId;

/// Constant failure of a credential lookup.
///
/// The type carries no prose on purpose: a resolver may record its own diagnostics elsewhere, and
/// the runtime only needs to know that no credential was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the credential could not be resolved")]
pub struct CredentialError;

/// Immutable identity of the model a run is about to address.
///
/// The resolver receives this to decide which credential to return. It intentionally exposes no
/// provider handle and no credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRequest {
    /// Caller-defined key of the selected binding.
    pub binding: String,
    /// Model identity of the selected binding.
    pub model: Model,
    /// Lookup identifier declared by the selected binding. Never a credential.
    pub credential_id: CredentialId,
}

/// Future returned by [`CredentialResolver::resolve`].
pub type CredentialFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Credential, CredentialError>> + Send + 'a>>;

/// Async, object-safe credential lookup owned by the consumer.
///
/// The runtime implementation must observe `cancellation`: a resolution that can outlive its run
/// would keep a credential lookup alive after the caller stopped caring about it. A resolver never
/// receives a provider, a database handle or a keyring from this crate, and returns either a
/// credential or the constant [`CredentialError`].
pub trait CredentialResolver: Send + Sync {
    /// Produce the credential for one run of the selected binding.
    fn resolve<'a>(
        &'a self,
        request: &'a CredentialRequest,
        cancellation: CancellationToken,
    ) -> CredentialFuture<'a>;
}
