use crate::domain::type_registry::TypeRegistry;

/// Unique identifier for a node in the graph
pub type NodeId = u32;

/// Scope identifier (module/namespace)
pub type ScopeId = String;

/// Source code span
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Stable identifier for a separately loadable piece of source context.
pub type ContextFragmentId = String;

/// A source fragment that is independently reached and charged by CF traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFragment {
    pub id: ContextFragmentId,
    pub span: Option<SourceSpan>,
    pub normalized_text: Option<String>,
    pub context_size: u32,
}

impl ContextFragment {
    pub fn new(
        id: ContextFragmentId,
        span: Option<SourceSpan>,
        normalized_text: Option<String>,
        context_size: u32,
    ) -> Self {
        Self {
            id,
            span,
            normalized_text,
            context_size,
        }
    }
}

/// Shared core attributes for all nodes
#[derive(Debug, Clone)]
pub struct NodeCore {
    pub id: NodeId,
    pub name: String,
    pub scope: Option<ScopeId>,
    /// Compatibility total. New CF computations charge the two fragments below.
    pub context_size: u32, // Abstract context size (computed by SizeFunction)
    pub surface_fragment: ContextFragment,
    pub implementation_fragment: Option<ContextFragment>,
    pub span: SourceSpan,
    pub doc_score: f32, // Documentation quality score [0.0, 1.0]
    pub documentation: Vec<String>,
    pub is_external: bool,
    pub file_path: String, // Path to source file (relative to project root)
}

impl NodeCore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: NodeId,
        name: String,
        scope: Option<ScopeId>,
        context_size: u32,
        span: SourceSpan,
        doc_score: f32,
        is_external: bool,
        file_path: String,
    ) -> Self {
        let surface_fragment = ContextFragment::new(
            format!("node:{id}:surface"),
            Some(span.clone()),
            None,
            context_size,
        );
        Self {
            id,
            name,
            scope,
            context_size,
            surface_fragment,
            implementation_fragment: None,
            span,
            doc_score,
            documentation: Vec::new(),
            is_external,
            file_path,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_fragments(
        id: NodeId,
        name: String,
        scope: Option<ScopeId>,
        surface_fragment: ContextFragment,
        implementation_fragment: Option<ContextFragment>,
        span: SourceSpan,
        doc_score: f32,
        documentation: Vec<String>,
        is_external: bool,
        file_path: String,
    ) -> Self {
        let context_size = surface_fragment.context_size.saturating_add(
            implementation_fragment
                .as_ref()
                .map_or(0, |fragment| fragment.context_size),
        );
        Self {
            id,
            name,
            scope,
            context_size,
            surface_fragment,
            implementation_fragment,
            span,
            doc_score,
            documentation,
            is_external,
            file_path,
        }
    }
}

/// Visibility level
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
}

/// Function node
#[derive(Debug, Clone)]
pub struct FunctionNode {
    pub core: NodeCore,

    // Signature completeness signals
    // Behavioral signals
    pub is_async: bool,
    pub is_generator: bool,
    pub visibility: Visibility,

    // Signature - type references are stored as TypeIds (symbols)
    // The actual type information is in TypeRegistry
    pub parameters: Vec<Parameter>,
    pub return_types: Vec<String>, // TypeId (symbol) of return types

    // Interface/abstract method flag
    // True if this method is defined in an Interface/Protocol/Trait/Abstract Class
    // Such methods have no implementation body, only signature + documentation
    pub is_interface_method: bool,

    /// True if this function is a type constructor (e.g. Python __init__).
    /// Set from semantic data by language extractor; used for boundary rules.
    pub is_constructor: bool,

    /// True if this function is DI-wired (e.g. FastAPI Depends() or cf:di_wired pragma).
    /// Boundary when signature is complete.
    pub is_di_wired: bool,
}

impl FunctionNode {
    /// Count parameters with type annotations
    pub fn typed_param_count(&self) -> usize {
        self.parameters
            .iter()
            .filter(|p| p.param_type.is_some())
            .count()
    }

    /// Count parameters that are effectively typed, considering TypeVar bounds.
    /// A parameter typed with an unbounded TypeVar is NOT effectively typed.
    pub fn effectively_typed_param_count(&self, type_registry: &TypeRegistry) -> usize {
        self.parameters
            .iter()
            .filter(|p| is_param_effectively_typed(p, type_registry))
            .count()
    }

    /// Total parameter count
    pub fn param_count(&self) -> usize {
        self.parameters.len()
    }

    /// Has return type annotation
    pub fn has_return_type(&self) -> bool {
        !self.return_types.is_empty()
    }

    /// Check if function signature is complete (all params typed + has return type).
    /// This is the legacy check without TypeVar awareness.
    pub fn is_signature_complete(&self) -> bool {
        self.typed_param_count() == self.param_count() && self.has_return_type()
    }

    /// Check if function signature is complete with TypeVar awareness.
    /// Parameters typed with unbounded TypeVars are NOT considered effectively typed.
    pub fn is_signature_complete_with_registry(&self, type_registry: &TypeRegistry) -> bool {
        self.effectively_typed_param_count(type_registry) == self.param_count()
            && self.has_return_type()
    }

    /// Get return type IDs
    pub fn return_type_ids(&self) -> &[String] {
        &self.return_types
    }
}

/// Function parameter
#[derive(Debug, Clone)]
pub struct Parameter {
    pub name: String,
    /// Type ID (symbol) of the parameter type, stored in TypeRegistry
    pub param_type: Option<String>,
    /// Whether this parameter has a high-freedom type (like dict, list, str)
    pub is_high_freedom_type: bool,
    // We could add default value presence, etc.
}

/// Check if a parameter is effectively typed, considering TypeVar bounds.
/// A param typed with an unbounded TypeVar (no bound, no constraints) is NOT effectively typed.
pub fn is_param_effectively_typed(param: &Parameter, type_registry: &TypeRegistry) -> bool {
    let Some(ref type_id) = param.param_type else {
        return false;
    };

    let Some(type_info) = type_registry.get(type_id) else {
        return true;
    };

    if type_info.is_type_var() {
        match &type_info.definition.type_var_info {
            Some(tv) => tv.is_effectively_typed(),
            None => false,
        }
    } else {
        true
    }
}

/// Mutability
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutability {
    Const,     // Compile-time constant
    Immutable, // Runtime immutable
    Mutable,   // Mutable (Expansion trigger)
}

/// Variable kind
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableKind {
    Global,     // Module-level
    ClassField, // Class/struct field
    Local,      // Local variable
}

/// Variable node
#[derive(Debug, Clone)]
pub struct VariableNode {
    pub core: NodeCore,

    // Type ID of this variable (stored in TypeRegistry)
    pub var_type: Option<String>,

    // Mutability (critical for Expansion)
    pub mutability: Mutability,

    // Scope kind
    pub variable_kind: VariableKind,
}

/// Polymorphic node type
#[derive(Debug, Clone)]
pub enum Node {
    Function(FunctionNode),
    Variable(VariableNode),
}

impl Node {
    pub fn core(&self) -> &NodeCore {
        match self {
            Node::Function(f) => &f.core,
            Node::Variable(v) => &v.core,
        }
    }

    pub fn core_mut(&mut self) -> &mut NodeCore {
        match self {
            Node::Function(f) => &mut f.core,
            Node::Variable(v) => &mut v.core,
        }
    }
}
