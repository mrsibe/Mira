//! Tool declarations, execution and the immutable registry a run reads from.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mira_ai::{InputContent, ToolDefinition};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;

/// Future returned by [`Tool::execute`].
///
/// Tool execution is cancellation-aware: the returned future is dropped when the run is
/// cancelled, so an implementation must not leave unbounded work behind.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;

/// Outcome of one tool call.
///
/// A tool reports failure by setting [`ToolResult::is_error`]; it has no other failure channel,
/// because a panic or a raw exception is not a diagnostic a model or a user should see.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// Text and image parts returned to the model.
    pub content: Vec<InputContent>,
    /// True when the call failed.
    pub is_error: bool,
}

impl ToolResult {
    /// A successful result with a single text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![InputContent::text(text)],
            is_error: false,
        }
    }

    /// A failed result with a single text part.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![InputContent::text(text)],
            is_error: true,
        }
    }
}

/// A callable tool.
///
/// The definition is portable: it is exactly the declaration sent to the model. Arguments reach
/// [`Tool::execute`] only after they parsed into a JSON object and validated against the declared
/// schema, and only for a turn that stopped with `StopReason::ToolUse`.
///
/// ```
/// use mira_agent::{CancellationToken, Tool, ToolFuture, ToolRegistry, ToolResult};
/// use mira_agent::mira_ai::ToolDefinition;
/// use serde_json::{json, Value};
/// use std::sync::Arc;
///
/// struct Double {
///     definition: ToolDefinition,
/// }
///
/// impl Tool for Double {
///     fn definition(&self) -> &ToolDefinition {
///         &self.definition
///     }
///
///     fn execute<'a>(&'a self, arguments: Value, _cancellation: CancellationToken) -> ToolFuture<'a> {
///         Box::pin(async move {
///             let value = arguments.get("value").and_then(Value::as_i64).unwrap_or_default();
///             ToolResult::text((value * 2).to_string())
///         })
///     }
/// }
///
/// let tool = Arc::new(Double {
///     definition: ToolDefinition::new(
///         "double",
///         "Doubles a number.",
///         json!({
///             "type": "object",
///             "properties": { "value": { "type": "integer" } },
///             "required": ["value"],
///             "additionalProperties": false
///         }),
///     ),
/// });
///
/// let registry = ToolRegistry::new().with_tool(tool)?;
/// assert_eq!(registry.names().collect::<Vec<_>>(), ["double"]);
/// # Ok::<(), mira_agent::AgentError>(())
/// ```
pub trait Tool: Send + Sync {
    /// Name, description and JSON Schema arguments declared to the model.
    fn definition(&self) -> &ToolDefinition;

    /// Execute one validated call.
    ///
    /// `cancellation` is cancelled when the run is cancelled and also when the run ends for any
    /// other reason, so it is the lifetime signal for work started by this call; implementations
    /// should stop and return as soon as they observe it. Panics are contained by the agent —
    /// both a panic in this method and a panic in the future it returns — and reported as a
    /// generic failed result, so implementations should return a descriptive [`ToolResult`]
    /// instead. Containment does not install a panic hook: the process hook still runs, and a
    /// caller that needs silence must install its own.
    fn execute<'a>(&'a self, arguments: Value, cancellation: CancellationToken) -> ToolFuture<'a>;
}

/// A set of tools one agent run may execute.
///
/// A registry is validated once, when a tool is inserted: names must be unique, the parameter
/// schema must be a JSON Schema object and must compile without fetching anything. References
/// that would require retrieval over the network or the filesystem are refused, so building and
/// validating a registry never performs I/O. A reference that resolves inside the same schema
/// document needs no retrieval and is accepted: a local fragment such as `#/$defs/item`, or an
/// absolute URI that matches an `$id` declared in that document.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<RegisteredTool>,
}

struct RegisteredTool {
    definition: ToolDefinition,
    tool: Arc<dyn Tool>,
    validator: jsonschema::Validator,
}

/// Result of resolving a tool name and validating one call's arguments.
pub(crate) enum ArgumentCheck {
    /// The tool exists and the arguments are a valid JSON object for its schema.
    Valid(Arc<dyn Tool>),
    /// No tool with this name is registered.
    Unknown,
    /// The call has no parsed argument object, or the parsed value is not an object.
    Malformed,
    /// The argument object does not satisfy the tool's schema.
    SchemaMismatch,
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one tool.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::DuplicateTool`] when the name is already registered, and
    /// [`AgentError::InvalidToolSchema`] when the parameter schema is not a JSON Schema object,
    /// does not compile, or needs a reference that would have to be retrieved offline.
    pub fn insert(&mut self, tool: Arc<dyn Tool>) -> Result<(), AgentError> {
        let definition = tool.definition().clone();
        if self
            .tools
            .iter()
            .any(|entry| entry.definition.name == definition.name)
        {
            return Err(AgentError::DuplicateTool(definition.name));
        }
        if !definition.parameters.is_object() {
            return Err(AgentError::InvalidToolSchema(definition.name));
        }
        let validator = jsonschema::options()
            .offline()
            .build(&definition.parameters)
            .map_err(|_| AgentError::InvalidToolSchema(definition.name.clone()))?;
        self.tools.push(RegisteredTool {
            definition,
            tool,
            validator,
        });
        Ok(())
    }

    /// Builder form of [`ToolRegistry::insert`].
    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Result<Self, AgentError> {
        self.insert(tool)?;
        Ok(self)
    }

    /// The tool registered under `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|entry| entry.definition.name == name)
            .map(|entry| &entry.tool)
    }

    /// Declarations for every registered tool, in registration order.
    ///
    /// The declarations are cloned because a request context owns them.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    /// Registered tool names, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools
            .iter()
            .map(|entry| entry.definition.name.as_str())
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tool is registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Resolve `name` and validate `arguments` against the tool's schema.
    pub(crate) fn check(&self, name: &str, arguments: Option<&Value>) -> ArgumentCheck {
        let Some(entry) = self
            .tools
            .iter()
            .find(|entry| entry.definition.name == name)
        else {
            return ArgumentCheck::Unknown;
        };
        let Some(arguments) = arguments else {
            return ArgumentCheck::Malformed;
        };
        if !arguments.is_object() {
            return ArgumentCheck::Malformed;
        }
        if entry.validator.is_valid(arguments) {
            ArgumentCheck::Valid(entry.tool.clone())
        } else {
            ArgumentCheck::SchemaMismatch
        }
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolRegistry")
            .field("tools", &self.names().collect::<Vec<_>>())
            .finish()
    }
}
