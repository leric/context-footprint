use crate::domain::edge::EdgeKind;
use crate::domain::graph::ContextGraph;
use crate::domain::node::Node;
use crate::domain::type_registry::TypeRegistry;

/// Node type for documentation scoring
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    Function,
    Variable,
}

/// Pruning decision
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruningDecision {
    Boundary,    // Stop traversal here; node is a valid abstraction
    Transparent, // Continue traversal through this node
}

/// Direction-specific reason for loading an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasoningMode {
    NeedSurface,
    NeedBehavior,
    NeedValueProvenance,
    NeedUsageEvidence,
    NeedContractEvidence,
    NeedImplementation,
    NeedParentUsage,
}

/// A reasoning dependency derived from one or more program facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasoningRelation {
    Call,
    Read,
    Write,
    Annotates,
    DynamicDispatch,
    ValueProvenance,
    UsageEvidence,
    ParentContract,
    ChildImplementationEvidence,
    ObservedBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryDecision {
    StopAtSurface,
    EnterImplementation,
    Unknown,
}

/// Evaluates boundary reliability independently from traversal semantics.
pub trait BoundaryPolicy: Send + Sync {
    fn evaluate(
        &self,
        reasoning_mode: ReasoningMode,
        relation: ReasoningRelation,
        source: &Node,
        target: &Node,
        type_registry: &TypeRegistry,
    ) -> BoundaryDecision;

    fn evaluate_contract(
        &self,
        reasoning_mode: ReasoningMode,
        node: &Node,
        type_registry: &TypeRegistry,
    ) -> BoundaryDecision;

    fn id(&self) -> String;
}

/// Pruning parameters for the CF solver.
/// Only [doc_threshold] is configurable; "document completeness" is defined by doc_score (from doc_scorer).
#[derive(Debug, Clone)]
pub struct PruningParams {
    /// Documentation score threshold: doc_score >= this value is "sufficient documentation".
    pub doc_threshold: f32,
    /// If true (Academic): internal function is Boundary when sig complete and doc_score >= doc_threshold.
    /// If false (Strict): only abstract factory is Boundary for internal functions.
    pub treat_typed_documented_function_as_boundary: bool,
}

impl Default for PruningParams {
    fn default() -> Self {
        Self::academic(0.5)
    }
}

impl PruningParams {
    /// Academic mode: internal function IS a boundary if typed + documented.
    pub fn academic(doc_threshold: f32) -> Self {
        Self {
            doc_threshold,
            treat_typed_documented_function_as_boundary: true,
        }
    }

    /// Strict mode: internal function is TRANSPARENT unless it's an abstract factory.
    pub fn strict(doc_threshold: f32) -> Self {
        Self {
            doc_threshold,
            treat_typed_documented_function_as_boundary: false,
        }
    }
}

/// Syntactic policy used by the existing Academic and Strict configurations.
#[derive(Debug, Clone)]
pub struct SyntacticBoundaryPolicy {
    params: PruningParams,
}

impl SyntacticBoundaryPolicy {
    pub fn new(params: PruningParams) -> Self {
        Self { params }
    }

    pub fn params(&self) -> &PruningParams {
        &self.params
    }

    fn evaluate_function(&self, target: &Node, type_registry: &TypeRegistry) -> BoundaryDecision {
        let Node::Function(function) = target else {
            return BoundaryDecision::Unknown;
        };
        let signature_complete = function.is_signature_complete_with_registry(type_registry);

        if function.is_di_wired && signature_complete {
            return BoundaryDecision::StopAtSurface;
        }
        if function.is_interface_method {
            return if signature_complete && function.core.doc_score >= self.params.doc_threshold {
                BoundaryDecision::StopAtSurface
            } else {
                BoundaryDecision::EnterImplementation
            };
        }
        if is_abstract_factory(target, type_registry, self.params.doc_threshold) {
            return BoundaryDecision::StopAtSurface;
        }
        if self.params.treat_typed_documented_function_as_boundary
            && signature_complete
            && function.core.doc_score >= self.params.doc_threshold
        {
            return BoundaryDecision::StopAtSurface;
        }
        BoundaryDecision::EnterImplementation
    }
}

impl BoundaryPolicy for SyntacticBoundaryPolicy {
    fn evaluate(
        &self,
        _reasoning_mode: ReasoningMode,
        relation: ReasoningRelation,
        source: &Node,
        target: &Node,
        type_registry: &TypeRegistry,
    ) -> BoundaryDecision {
        if target.core().is_external {
            return BoundaryDecision::StopAtSurface;
        }

        match relation {
            ReasoningRelation::UsageEvidence
            | ReasoningRelation::ValueProvenance
            | ReasoningRelation::ChildImplementationEvidence
            | ReasoningRelation::ObservedBy => BoundaryDecision::EnterImplementation,
            ReasoningRelation::ParentContract | ReasoningRelation::Write => {
                BoundaryDecision::StopAtSurface
            }
            ReasoningRelation::DynamicDispatch => {
                match self.evaluate_contract(
                    ReasoningMode::NeedContractEvidence,
                    source,
                    type_registry,
                ) {
                    BoundaryDecision::StopAtSurface => BoundaryDecision::StopAtSurface,
                    BoundaryDecision::EnterImplementation | BoundaryDecision::Unknown => {
                        BoundaryDecision::EnterImplementation
                    }
                }
            }
            ReasoningRelation::Read => match target {
                Node::Variable(variable) => match variable.mutability {
                    crate::domain::node::Mutability::Const
                    | crate::domain::node::Mutability::Immutable => BoundaryDecision::StopAtSurface,
                    crate::domain::node::Mutability::Mutable => {
                        BoundaryDecision::EnterImplementation
                    }
                },
                Node::Function(_) => BoundaryDecision::Unknown,
            },
            ReasoningRelation::Call | ReasoningRelation::Annotates => {
                self.evaluate_function(target, type_registry)
            }
        }
    }

    fn evaluate_contract(
        &self,
        _reasoning_mode: ReasoningMode,
        node: &Node,
        type_registry: &TypeRegistry,
    ) -> BoundaryDecision {
        if node.core().is_external {
            return BoundaryDecision::StopAtSurface;
        }
        match node {
            Node::Variable(variable) => match variable.mutability {
                crate::domain::node::Mutability::Const
                | crate::domain::node::Mutability::Immutable => BoundaryDecision::StopAtSurface,
                crate::domain::node::Mutability::Mutable => BoundaryDecision::EnterImplementation,
            },
            Node::Function(function) => {
                let signature_complete =
                    function.is_signature_complete_with_registry(type_registry);
                if signature_complete
                    && (function.is_di_wired
                        || function.core.doc_score >= self.params.doc_threshold)
                {
                    BoundaryDecision::StopAtSurface
                } else {
                    BoundaryDecision::EnterImplementation
                }
            }
        }
    }

    fn id(&self) -> String {
        let mode = if self.params.treat_typed_documented_function_as_boundary {
            "academic"
        } else {
            "strict"
        };
        format!("syntactic:{mode}:doc={:.3}", self.params.doc_threshold)
    }
}

/// Threshold for tokens per caller. If a function has fewer tokens per caller than this,
/// it's considered a utility and we don't explore its callers during call-in exploration.
const UTILITY_TOKENS_PER_CALLER_THRESHOLD: usize = 10;

// -----------------------------------------------------------------------------
// Core algorithm (domain layer)
// -----------------------------------------------------------------------------

/// Returns true if the function returns an abstract type (Protocol/Interface/Trait)
/// i.e. "abstract factory" pattern.
///
/// Rationale: Abstract factory is a good design pattern. If a function returns an abstract type
/// and has a complete signature, it's a valid boundary regardless of the return type's documentation.
/// The abstract type's method signatures are more important than prose documentation.
pub fn is_abstract_factory(
    function_node: &Node,
    type_registry: &TypeRegistry,
    _doc_threshold: f32,
) -> bool {
    let Node::Function(f) = function_node else {
        return false;
    };
    if f.return_types.is_empty() || !f.is_signature_complete_with_registry(type_registry) {
        return false;
    }

    // Check if ANY return type is an abstract type
    // We don't require the return type itself to be well-documented,
    // because the abstract type's method signatures are sufficient documentation.
    for return_type_id in &f.return_types {
        if let Some(type_info) = type_registry.get(return_type_id)
            && type_info.definition.is_abstract
        {
            return true;
        }
    }
    false
}

/// Whether to explore callers of the current function (call-in exploration).
/// Used when traversing: if true, follow incoming Call edges from this function.
pub fn should_explore_callers(
    func_node: &crate::domain::node::FunctionNode,
    func_idx: petgraph::graph::NodeIndex,
    incoming_edge: Option<&EdgeKind>,
    params: &PruningParams,
    graph: &ContextGraph,
) -> bool {
    // Already arrived via Call — caller context is known
    if matches!(incoming_edge, Some(EdgeKind::Call)) {
        return false;
    }

    // Constructors (e.g. __init__) are called from many instantiation sites.
    // Call-in exploration would add all callers, inflating CF without adding
    // semantic value — the constructor's purpose is self-evident.
    if func_node.is_constructor {
        return false;
    }

    let caller_count = graph.incoming_edges(func_idx, Some(EdgeKind::Call)).count();

    // 1. Highly reused utility exception (Size vs CallIn ratio)
    // If a function is called from many places relative to its size, it's a utility.
    // Exploring all callers would inflate CF without aiding understanding.
    if caller_count > 1 {
        let tokens_per_caller = func_node.core.context_size as usize / caller_count;
        if tokens_per_caller < UTILITY_TOKENS_PER_CALLER_THRESHOLD {
            return false;
        }
    }

    // 2. Side-effect-free exception (Pure-like)
    // A function's behavior doesn't affect the rest of the system if it doesn't write to mutable state.
    // Deep check: no outgoing Write edges in this function or any function it calls.
    // We intentionally ignore Read edges because reading global state doesn't produce side effects
    // that would necessitate exploring callers to understand system state changes.
    let mut is_side_effect_free = true;
    let mut queue = std::collections::VecDeque::new();
    let mut visited_pure_check = std::collections::HashSet::new();
    queue.push_back(func_idx);
    visited_pure_check.insert(func_idx);

    while let Some(curr_idx) = queue.pop_front() {
        for (target_idx, edge_kind) in graph.outgoing_edges(curr_idx) {
            match edge_kind {
                EdgeKind::Write => {
                    is_side_effect_free = false;
                    break;
                }
                EdgeKind::Call => {
                    if visited_pure_check.insert(target_idx) {
                        queue.push_back(target_idx);
                    }
                }
                _ => {}
            }
        }
        if !is_side_effect_free {
            break;
        }
    }

    // If it's side-effect-free, we don't need to explore callers to understand its impact.
    if is_side_effect_free {
        return false;
    }

    // Specification complete check
    if !func_node.is_signature_complete_with_registry(&graph.type_registry) {
        return true; // Signature is incomplete, must explore callers
    }

    // Check for high-freedom types in parameters (e.g. dict, list, str, Any).
    // High-freedom types do not provide strong structural constraints, making them
    // "leaky" by default unless well-documented.
    let has_high_freedom_params = func_node.parameters.iter().any(|p| p.is_high_freedom_type);

    if has_high_freedom_params {
        // High freedom params require documentation to establish contract
        if func_node.core.doc_score < params.doc_threshold {
            return true;
        }
    }

    // If all params are strong types (or no params) OR doc_score is sufficient for high-freedom types,
    // the function is well-specified.
    false
}

/// Forward-edge pruning: evaluates Boundary vs Transparent for outgoing edges only.
/// Reverse exploration (call-in, shared-state write) is decided in the solver via
/// should_explore_callers and mutability + Read.
pub fn evaluate_forward(
    params: &PruningParams,
    source: &Node,
    target: &Node,
    edge_kind: &EdgeKind,
    graph: &ContextGraph,
) -> PruningDecision {
    // 1. Do not expand from stub nodes (context_size 0: package/module/synthetic).
    // Otherwise reverse traversal (CallIn) into such a node would pull in the whole package.
    if source.core().context_size == 0 {
        return PruningDecision::Boundary;
    }

    // 2. External dependencies are always boundaries
    if target.core().is_external {
        return PruningDecision::Boundary;
    }

    // 3. Node type dispatch
    match target {
        Node::Variable(v) => {
            // For Read edges: immutable values are boundaries (behavior is fully determined)
            // mutable variables trigger expansion (need to find all writers)
            // For Write edges: always transparent (writing to any variable is an action)
            match edge_kind {
                EdgeKind::Write => PruningDecision::Transparent,
                _ => match v.mutability {
                    crate::domain::node::Mutability::Const
                    | crate::domain::node::Mutability::Immutable => PruningDecision::Boundary,
                    crate::domain::node::Mutability::Mutable => PruningDecision::Transparent,
                },
            }
        }
        Node::Function(f) => {
            let sig_complete = f.is_signature_complete_with_registry(&graph.type_registry);

            // DI-wired function with complete signature: boundary (no doc requirement)
            if f.is_di_wired && sig_complete {
                return PruningDecision::Boundary;
            }

            // Interface/abstract methods: boundary if signature complete and documented
            if f.is_interface_method {
                if sig_complete && f.core.doc_score >= params.doc_threshold {
                    return PruningDecision::Boundary;
                }
                // Undocumented interface method is a leaky abstraction
                return PruningDecision::Transparent;
            }

            if is_abstract_factory(target, &graph.type_registry, params.doc_threshold) {
                return PruningDecision::Boundary;
            }
            if params.treat_typed_documented_function_as_boundary
                && sig_complete
                && f.core.doc_score >= params.doc_threshold
            {
                return PruningDecision::Boundary;
            }
            PruningDecision::Transparent
        }
    }
}

/// Legacy name: delegates to evaluate_forward (all edges are now forward in the graph).
pub fn evaluate(
    params: &PruningParams,
    source: &Node,
    target: &Node,
    edge_kind: &EdgeKind,
    graph: &ContextGraph,
) -> PruningDecision {
    evaluate_forward(params, source, target, edge_kind, graph)
}

// SourceSpan is defined in node.rs
pub use crate::domain::node::SourceSpan;

/// Size function trait - computes context size
pub trait SizeFunction: Send + Sync {
    /// Compute the context size for a given source code span,
    /// potentially excluding documentation to avoid "punishing" well-documented code.
    fn compute(&self, source: &str, span: &SourceSpan, doc_texts: &[String]) -> u32;

    fn id(&self) -> &'static str {
        "custom"
    }
}

/// Documentation scorer trait - evaluates documentation quality
pub trait DocumentationScorer: Send + Sync {
    /// Evaluate documentation quality, returns [0.0, 1.0] score
    /// - 0.0: No documentation or meaningless documentation
    /// - 1.0: Complete, clear documentation
    fn score(&self, node_info: &NodeInfo, doc_text: Option<&str>) -> f32;
}

/// Node information for documentation scoring
pub struct NodeInfo {
    pub node_type: NodeType,
    pub name: String,
    pub signature: Option<String>, // Function signature
    pub language: Option<String>,  // Programming language
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::edge::EdgeKind;
    use crate::domain::graph::ContextGraph;
    use crate::domain::node::{FunctionNode, Node, NodeCore, SourceSpan, Visibility};
    use crate::domain::type_registry::{TypeDefAttribute, TypeInfo, TypeVarInfo};

    fn test_node(doc_score: f32) -> Node {
        let core = NodeCore::new(
            0,
            "f".to_string(),
            None,
            10,
            SourceSpan {
                start_line: 0,
                start_column: 0,
                end_line: 1,
                end_column: 5,
            },
            doc_score,
            false,
            "test.py".to_string(),
        );
        Node::Function(FunctionNode {
            core,
            parameters: vec![crate::domain::node::Parameter {
                name: "x".to_string(),
                param_type: Some("int#".to_string()),
                is_high_freedom_type: false,
            }],
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec!["int#".to_string()],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        })
    }

    #[test]
    fn test_default_pruning_params() {
        let p = PruningParams::default();
        assert!((p.doc_threshold - 0.5).abs() < 1e-5);
        assert!(p.treat_typed_documented_function_as_boundary);
    }

    #[test]
    fn test_academic_vs_strict() {
        let graph = ContextGraph::new();
        let target = test_node(0.8);
        let source = test_node(0.0);
        let edge = EdgeKind::Call;
        let academic = PruningParams::default();
        let strict = PruningParams {
            doc_threshold: 0.5,
            treat_typed_documented_function_as_boundary: false,
        };
        assert!(matches!(
            evaluate(&academic, &source, &target, &edge, &graph),
            PruningDecision::Boundary
        ));
        assert!(matches!(
            evaluate(&strict, &source, &target, &edge, &graph),
            PruningDecision::Transparent
        ));
    }

    // Helper to create variable nodes for testing
    fn test_variable_node(mutability: crate::domain::node::Mutability) -> Node {
        let core = NodeCore::new(
            1,
            "test_var".to_string(),
            None,
            5,
            SourceSpan {
                start_line: 0,
                start_column: 0,
                end_line: 1,
                end_column: 10,
            },
            0.5,
            false,
            "test.py".to_string(),
        );
        Node::Variable(crate::domain::node::VariableNode {
            core,
            var_type: Some("int#".to_string()),
            mutability,
            variable_kind: crate::domain::node::VariableKind::Global,
        })
    }

    #[test]
    fn test_variable_immutable_is_boundary_on_read() {
        let graph = ContextGraph::new();
        let source = test_node(0.0);
        let target = test_variable_node(crate::domain::node::Mutability::Immutable);
        let edge = EdgeKind::Read;
        let params = PruningParams::default();

        // Immutable variable should be a boundary on Read
        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Boundary
        ));
    }

    #[test]
    fn test_variable_const_is_boundary_on_read() {
        let graph = ContextGraph::new();
        let source = test_node(0.0);
        let target = test_variable_node(crate::domain::node::Mutability::Const);
        let edge = EdgeKind::Read;
        let params = PruningParams::default();

        // Const variable should be a boundary on Read
        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Boundary
        ));
    }

    #[test]
    fn test_source_context_size_zero_is_boundary() {
        let graph = ContextGraph::new();
        let source_core = NodeCore::new(
            0,
            "stub".to_string(),
            None,
            0, // context_size 0 => do not expand
            SourceSpan {
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
            0.0,
            false,
            "test.py".to_string(),
        );
        let source = Node::Function(FunctionNode {
            core: source_core,
            parameters: vec![],
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });
        let target = test_node(0.0);
        let edge = EdgeKind::Read;
        let params = PruningParams::default();

        // Do not expand from stub nodes (context_size 0)
        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Boundary
        ));
    }

    #[test]
    fn test_variable_mutable_is_transparent_on_read() {
        let graph = ContextGraph::new();
        let source = test_node(0.0);
        let target = test_variable_node(crate::domain::node::Mutability::Mutable);
        let edge = EdgeKind::Read;
        let params = PruningParams::default();

        // Mutable variable should be transparent on Read (triggers expansion)
        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Transparent
        ));
    }

    #[test]
    fn test_variable_any_mutability_is_transparent_on_write() {
        let graph = ContextGraph::new();
        let source = test_node(0.0);
        let params = PruningParams::default();

        // All variable types should be transparent on Write
        for mutability in [
            crate::domain::node::Mutability::Const,
            crate::domain::node::Mutability::Immutable,
            crate::domain::node::Mutability::Mutable,
        ] {
            let target = test_variable_node(mutability.clone());
            let edge = EdgeKind::Write;
            assert!(
                matches!(
                    evaluate(&params, &source, &target, &edge, &graph),
                    PruningDecision::Transparent
                ),
                "Variable with {:?} should be transparent on Write",
                mutability
            );
        }
    }

    fn make_func_with_typevar_param(param_type: &str, doc_score: f32) -> Node {
        let core = NodeCore::new(
            0,
            "f".to_string(),
            None,
            10,
            SourceSpan {
                start_line: 0,
                start_column: 0,
                end_line: 1,
                end_column: 5,
            },
            doc_score,
            false,
            "test.py".to_string(),
        );
        Node::Function(FunctionNode {
            core,
            parameters: vec![crate::domain::node::Parameter {
                name: "x".to_string(),
                param_type: Some(param_type.to_string()),
                is_high_freedom_type: false,
            }],
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec!["int#".to_string()],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        })
    }

    fn register_typevar(
        graph: &mut ContextGraph,
        type_id: &str,
        type_var_info: Option<TypeVarInfo>,
    ) {
        graph.type_registry.register(
            type_id.to_string(),
            TypeInfo {
                definition: TypeDefAttribute {
                    type_kind: crate::domain::type_registry::TypeKind::TypeVar,
                    is_abstract: false,
                    type_param_count: 0,
                    type_var_info,
                },
                context_size: 0,
                surface_fragment: crate::domain::node::ContextFragment::new(
                    format!("{type_id}:surface"),
                    None,
                    None,
                    0,
                ),
                doc_score: 0.0,
                documentation: Vec::new(),
            },
        );
    }

    #[test]
    fn test_unbounded_typevar_makes_signature_incomplete() {
        let mut graph = ContextGraph::new();
        register_typevar(
            &mut graph,
            "T#",
            Some(TypeVarInfo {
                bound: None,
                constraints: Vec::new(),
            }),
        );

        let source = test_node(0.0);
        let target = make_func_with_typevar_param("T#", 0.8);
        let edge = EdgeKind::Call;
        let params = PruningParams::academic(0.5);

        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Transparent
        ));
    }

    #[test]
    fn test_bounded_typevar_keeps_signature_complete() {
        let mut graph = ContextGraph::new();
        register_typevar(
            &mut graph,
            "T#",
            Some(TypeVarInfo {
                bound: Some("Protocol#".to_string()),
                constraints: Vec::new(),
            }),
        );

        let source = test_node(0.0);
        let target = make_func_with_typevar_param("T#", 0.8);
        let edge = EdgeKind::Call;
        let params = PruningParams::academic(0.5);

        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Boundary
        ));
    }

    #[test]
    fn test_constrained_typevar_keeps_signature_complete() {
        let mut graph = ContextGraph::new();
        register_typevar(
            &mut graph,
            "T#",
            Some(TypeVarInfo {
                bound: None,
                constraints: vec!["int#".to_string(), "str#".to_string()],
            }),
        );

        let source = test_node(0.0);
        let target = make_func_with_typevar_param("T#", 0.8);
        let edge = EdgeKind::Call;
        let params = PruningParams::academic(0.5);

        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Boundary
        ));
    }

    #[test]
    fn test_typevar_no_info_makes_signature_incomplete() {
        let mut graph = ContextGraph::new();
        register_typevar(&mut graph, "T#", None);

        let source = test_node(0.0);
        let target = make_func_with_typevar_param("T#", 0.8);
        let edge = EdgeKind::Call;
        let params = PruningParams::academic(0.5);

        assert!(matches!(
            evaluate(&params, &source, &target, &edge, &graph),
            PruningDecision::Transparent
        ));
    }
}
