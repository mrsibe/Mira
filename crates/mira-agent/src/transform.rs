//! Optional per-request context derivation.

use std::future::Future;
use std::pin::Pin;

use mira_ai::Context;
use tokio_util::sync::CancellationToken;

pub use crate::error::ContextTransformError;

/// Future returned by [`ContextTransform::transform`].
pub type ContextTransformFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Context, ContextTransformError>> + Send + 'a>>;

/// Derives the context of one provider request from the agent's canonical transcript.
///
/// The agent passes the transcript by immutable reference, so a transform cannot change agent
/// state: the canonical transcript grows only from committed turns. The returned context is used
/// for that one request, and the transform runs before every request, including the first.
///
/// Implementations must observe `cancellation`, must return a context instead of panicking, and
/// must not perform blocking I/O.
pub trait ContextTransform: Send + Sync {
    /// Derive the context for the next request.
    fn transform<'a>(
        &'a self,
        context: &'a Context,
        cancellation: CancellationToken,
    ) -> ContextTransformFuture<'a>;
}
