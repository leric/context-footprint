use crate::domain::edge::EdgeKind;
use crate::domain::graph::ContextGraph;
use crate::domain::node::{
    ContextFragment, FunctionNode, Mutability as NodeMutability, Node, NodeCore, SourceSpan,
    VariableKind, VariableNode, Visibility as NodeVisibility,
};
use crate::domain::policy::{DocumentationScorer, NodeInfo, NodeType, SizeFunction};
use crate::domain::ports::SourceReader;
use crate::domain::semantic::{
    Mutability, ReferenceRole, SemanticData, SourceSpan as SemanticSpan, SymbolDefinition,
    SymbolDetails, SymbolId, SymbolKind, SymbolReference, VariableScope as SemanticVarScope,
    Visibility,
};
use crate::domain::type_registry::{
    TypeDefAttribute, TypeInfo, TypeKind, TypeRegistry, TypeVarInfo,
};
use anyhow::Result;
use petgraph::graph::NodeIndex;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Graph builder - Domain Service for constructing ContextGraph
pub struct GraphBuilder {
    size_function: Box<dyn SizeFunction>,
    doc_scorer: Box<dyn DocumentationScorer>,
}

impl GraphBuilder {
    pub fn new(
        size_function: Box<dyn SizeFunction>,
        doc_scorer: Box<dyn DocumentationScorer>,
    ) -> Self {
        Self {
            size_function,
            doc_scorer,
        }
    }

    /// Three-pass build strategy
    pub fn build(
        &self,
        semantic_data: SemanticData,
        source_reader: &dyn SourceReader,
    ) -> Result<ContextGraph> {
        let mut graph = ContextGraph::new();
        let mut type_registry = TypeRegistry::new();

        // Pre-compute enclosing map for symbol resolution
        let enclosing_map = semantic_data.build_enclosing_map();

        // Collect all node candidate symbols
        let mut node_symbols: HashSet<SymbolId> = HashSet::new();

        // Pass 1: Node Allocation - Create FunctionNode/VariableNode and TypeRegistry entries
        for document in &semantic_data.documents {
            let source_path = Path::new(&semantic_data.project_root).join(&document.relative_path);
            let source_code = source_reader.read(&source_path)?;

            for def in &document.definitions {
                let node_id = graph.graph.node_count() as u32;
                let doc_texts = def.documentation.clone();
                let span = convert_span(&def.span);

                // Check if this is an interface/abstract method
                // Now we check is_abstract directly from FunctionModifiers
                let is_interface_method = if def.kind == SymbolKind::Function {
                    if let Some(func_details) = def.as_function() {
                        func_details.modifiers.is_abstract
                    } else {
                        false
                    }
                } else {
                    false
                };

                let (surface_fragment, implementation_fragment) = create_context_fragments(
                    def,
                    &source_code,
                    self.size_function.as_ref(),
                    is_interface_method,
                );

                // Use all documentation entries for scoring (e.g. Annotated Doc() per parameter);
                // joining so the heuristic can see parameter coverage across all entries.
                let doc_text_combined = doc_texts.join("\n\n");
                let doc_text = if doc_text_combined.is_empty() {
                    None
                } else {
                    Some(doc_text_combined.as_str())
                };

                let language = document
                    .relative_path
                    .split('.')
                    .next_back()
                    .map(|ext| ext.to_lowercase());

                let node_info = NodeInfo {
                    node_type: infer_node_type_from_kind(&def.kind),
                    name: def.name.clone(),
                    signature: extract_signature(def),
                    language,
                };
                let doc_score = self.doc_scorer.score(&node_info, doc_text);

                match def.kind {
                    SymbolKind::Type => {
                        // Register in TypeRegistry
                        let type_info = create_type_info(def, surface_fragment, doc_score);
                        type_registry.register(def.symbol_id.clone(), type_info);

                        // Register implementor relationships for OverriddenBy edges
                        if let SymbolDetails::Type(type_details) = &def.details {
                            for interface_id in &type_details.implements {
                                type_registry.register_implementor(
                                    interface_id.clone(),
                                    def.symbol_id.clone(),
                                );
                            }
                            for base_id in &type_details.inherits {
                                type_registry
                                    .register_implementor(base_id.clone(), def.symbol_id.clone());
                            }
                        }
                    }
                    SymbolKind::Function | SymbolKind::Variable => {
                        // Create graph node
                        node_symbols.insert(def.symbol_id.clone());

                        let core = NodeCore::with_fragments(
                            node_id,
                            def.name.clone(),
                            def.enclosing_symbol.clone(),
                            surface_fragment,
                            implementation_fragment,
                            span,
                            doc_score,
                            doc_texts,
                            def.is_external,
                            document.relative_path.clone(),
                        );

                        let node = create_node_from_definition(core, def, is_interface_method)?;
                        graph.add_node(def.symbol_id.clone(), node);
                    }
                }
            }
        }

        // Pass 1 (continued): External symbol nodes
        // External symbols have no project source file. For context_size we use ONLY the
        // signature (no doc/implementation) since external library bodies are not useful
        // for understanding project code.
        for def in &semantic_data.external_symbols {
            // Skip if this symbol was already found as an internal project symbol
            // (Extractors might incorrectly flag intra-project imports as external)
            if node_symbols.contains(&def.symbol_id) || type_registry.contains(&def.symbol_id) {
                continue;
            }

            let node_id = graph.graph.node_count() as u32;
            let doc_texts = def.documentation.clone();

            let signature = extract_signature(def);
            let mut synthetic_source = build_external_signature_only(&signature, def);
            if !doc_texts.is_empty() {
                synthetic_source.push('\n');
                synthetic_source.push_str(&doc_texts.join("\n\n"));
            }
            let line_count = synthetic_source.lines().count().max(1) as u32;
            let last_line_length = synthetic_source
                .lines()
                .last()
                .map_or(0, |line| line.chars().count() as u32);
            let synthetic_span = crate::domain::node::SourceSpan {
                start_line: 0,
                start_column: 0,
                end_line: line_count.saturating_sub(1),
                end_column: last_line_length,
            };

            let raw_size = self
                .size_function
                .compute(&synthetic_source, &synthetic_span, &[]);
            let context_size = raw_size.min(EXTERNAL_SYMBOL_MAX_TOKENS);
            let surface_fragment = ContextFragment::new(
                format!("{}:surface", def.symbol_id),
                None,
                Some(synthetic_source.clone()),
                context_size,
            );

            let doc_text_combined = doc_texts.join("\n\n");
            let doc_text = if doc_text_combined.is_empty() {
                None
            } else {
                Some(doc_text_combined.as_str())
            };

            let language = def
                .location
                .file_path
                .split('.')
                .next_back()
                .map(|ext| ext.to_lowercase());

            let node_info = NodeInfo {
                node_type: infer_node_type_from_kind(&def.kind),
                name: def.name.clone(),
                signature,
                language,
            };
            let doc_score = self.doc_scorer.score(&node_info, doc_text);

            match def.kind {
                SymbolKind::Type => {
                    let type_info = create_type_info(def, surface_fragment, doc_score);
                    type_registry.register(def.symbol_id.clone(), type_info);
                }
                SymbolKind::Function | SymbolKind::Variable => {
                    node_symbols.insert(def.symbol_id.clone());

                    let core = NodeCore::with_fragments(
                        node_id,
                        def.name.clone(),
                        def.enclosing_symbol.clone(),
                        surface_fragment,
                        None,
                        convert_span(&def.span),
                        doc_score,
                        doc_texts,
                        true, // always external
                        def.location.file_path.clone(),
                    );

                    let node = create_node_from_definition(core, def, false)?;
                    graph.add_node(def.symbol_id.clone(), node);
                }
            }
        }

        // Build constructor init map: type_symbol -> init_node_symbol
        let mut init_map: HashMap<SymbolId, SymbolId> = HashMap::new();
        for document in &semantic_data.documents {
            for def in &document.definitions {
                if def.kind == SymbolKind::Function
                    && def.name == "__init__"
                    && let Some(ref enclosing) = def.enclosing_symbol
                    && type_registry.contains(enclosing)
                    && node_symbols.contains(&def.symbol_id)
                {
                    init_map.insert(enclosing.clone(), def.symbol_id.clone());
                }
            }
        }

        // Pass 2: Edge Wiring - Process references to create edges (forward edges only)
        // Collect unresolved calls (target unknown) and call_assignments (for type propagation)
        let mut unresolved_calls: Vec<(SymbolReference, NodeIndex)> = Vec::new();
        let mut call_assignments: HashMap<SymbolId, (NodeIndex, Option<SymbolId>)> = HashMap::new();

        for document in &semantic_data.documents {
            for reference in &document.references {
                let source_node_sym = Self::resolve_to_node_symbol(
                    &reference.enclosing_symbol,
                    &node_symbols,
                    &enclosing_map,
                );

                // Resolve target only when target_symbol is Some
                let target_node_sym = reference
                    .target_symbol
                    .as_ref()
                    .and_then(|t| Self::resolve_to_node_symbol(t, &node_symbols, &enclosing_map));

                if let Some(source_sym) = source_node_sym {
                    let source_idx = match graph.get_node_by_symbol(&source_sym) {
                        Some(idx) => idx,
                        None => continue,
                    };

                    if reference.role == ReferenceRole::Call {
                        let resolved_target = target_node_sym
                            .as_ref()
                            .and_then(|sym| graph.get_node_by_symbol(sym))
                            .map(|idx| (target_node_sym.as_ref().unwrap().clone(), idx))
                            .or_else(|| {
                                reference.target_symbol.as_ref().and_then(|t| {
                                    init_map.get(t).and_then(|init_sym| {
                                        graph
                                            .get_node_by_symbol(init_sym)
                                            .map(|idx| (init_sym.clone(), idx))
                                    })
                                })
                            });

                        if let Some((resolved_sym, target_idx)) = resolved_target {
                            if source_idx != target_idx {
                                graph.add_edge(source_idx, target_idx, EdgeKind::Call);
                            }
                            if let Some(assigned_var) = &reference.assigned_to {
                                call_assignments
                                    .insert(assigned_var.clone(), (source_idx, Some(resolved_sym)));
                            }
                        } else {
                            // Target could not be resolved; may recover in Pass 3 via type propagation
                            unresolved_calls.push((reference.clone(), source_idx));
                        }
                    }

                    if matches!(reference.role, ReferenceRole::Read | ReferenceRole::Write)
                        && let Some(target_sym) = &target_node_sym
                        && let Some(target_idx) = graph.get_node_by_symbol(target_sym)
                        && source_idx != target_idx
                    {
                        let edge_kind = if reference.role == ReferenceRole::Write {
                            EdgeKind::Write
                        } else {
                            EdgeKind::Read
                        };
                        graph.add_edge(source_idx, target_idx, edge_kind);
                    }

                    if reference.role == ReferenceRole::Decorate
                        && let Some(target_sym) = &target_node_sym
                        && let Some(target_idx) = graph.get_node_by_symbol(target_sym)
                        && source_idx != target_idx
                    {
                        graph.add_edge(source_idx, target_idx, EdgeKind::Annotates);
                    }
                }
            }
        }

        // Pass 2.5: Fill in type references in nodes from SymbolDetails
        for document in &semantic_data.documents {
            for def in &document.definitions {
                if let Some(node_idx) = graph.get_node_by_symbol(&def.symbol_id) {
                    match &def.details {
                        SymbolDetails::Function(func_details) => {
                            if let Some(Node::Function(func_node)) =
                                graph.graph.node_weight_mut(node_idx)
                            {
                                // Set return types
                                for type_id in &func_details.return_types {
                                    if type_registry.contains(type_id) {
                                        func_node.return_types.push(type_id.clone());
                                    }
                                }
                                // Note: Parameters are already set in create_node_from_definition
                            }
                        }
                        SymbolDetails::Variable(var_details) => {
                            if let Some(Node::Variable(var_node)) =
                                graph.graph.node_weight_mut(node_idx)
                                && let Some(ref type_id) = var_details.var_type
                                && type_registry.contains(type_id)
                            {
                                var_node.var_type = Some(type_id.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Pass 2.5: External Call Return Type Propagation
        // For variables that receive the result of an external call, propagate return type if variable has no type yet
        for (assigned_var_sym, (_caller_idx, target_sym)) in &call_assignments {
            let Some(var_idx) = graph.get_node_by_symbol(assigned_var_sym) else {
                continue;
            };
            let Some(target_sym) = target_sym else {
                continue;
            };
            let Some(target_idx) = graph.get_node_by_symbol(target_sym) else {
                continue;
            };
            let (var_has_no_type, target_is_external_with_return) = {
                let var_node = graph.graph.node_weight(var_idx);
                let target_node = graph.graph.node_weight(target_idx);
                let var_empty = matches!(var_node, Some(Node::Variable(v)) if v.var_type.is_none());
                let target_ok = matches!(
                    target_node,
                    Some(Node::Function(f)) if f.core.is_external && !f.return_types.is_empty()
                );
                (var_empty, target_ok)
            };
            if var_has_no_type && target_is_external_with_return {
                let first_ret = graph
                    .graph
                    .node_weight(target_idx)
                    .and_then(|n| match n {
                        Node::Function(f) => f.return_types.first().cloned(),
                        _ => None,
                    })
                    .filter(|id| type_registry.contains(id));
                if let Some(return_type_id) = first_ret
                    && let Some(Node::Variable(var_node)) = graph.graph.node_weight_mut(var_idx)
                {
                    var_node.var_type = Some(return_type_id);
                }
            }
        }

        // Pass 3: OverriddenBy edges (interface/override). Reverse exploration (SharedStateWrite, CallIn) is done at query time.
        // OverriddenBy edges: Parent method → Child method (interface implementation + concrete override)
        // Build a lookup: (enclosing_type, method_name) → node_idx for all methods
        let mut method_by_scope: HashMap<(SymbolId, String), Vec<petgraph::graph::NodeIndex>> =
            HashMap::new();
        for &node_idx in graph.symbol_to_node.values() {
            let node = graph.node(node_idx);
            if let Node::Function(f) = node
                && let Some(ref scope) = f.core.scope
            {
                method_by_scope
                    .entry((scope.clone(), f.core.name.clone()))
                    .or_default()
                    .push(node_idx);
            }
        }

        // For each interface method, find concrete implementations via implementors map
        let interface_methods: Vec<_> = graph
            .symbol_to_node
            .values()
            .filter_map(|&idx| {
                let node = graph.node(idx);
                if let Node::Function(f) = node
                    && f.is_interface_method
                    && let Some(ref scope) = f.core.scope
                {
                    return Some((idx, scope.clone(), f.core.name.clone()));
                }
                None
            })
            .collect();

        for (iface_idx, interface_type_id, method_name) in &interface_methods {
            if let Some(concrete_types) = type_registry.get_implementors(interface_type_id) {
                let concrete_types = concrete_types.clone();
                for concrete_type_id in &concrete_types {
                    let key = (concrete_type_id.clone(), method_name.clone());
                    if let Some(concrete_indices) = method_by_scope.get(&key) {
                        for &concrete_idx in concrete_indices {
                            if *iface_idx != concrete_idx {
                                graph.add_edge(*iface_idx, concrete_idx, EdgeKind::OverriddenBy);
                            }
                        }
                    }
                }
            }
        }

        // Pass 3: Type-Driven Call Edge Recovery (fixpoint)
        // Resolve unresolved_calls using receiver's var_type and method_name until no progress
        loop {
            let mut resolved_any = false;
            let mut still_unresolved = Vec::new();
            for (reference, source_idx) in unresolved_calls {
                let Some(receiver_sym) = &reference.receiver else {
                    still_unresolved.push((reference, source_idx));
                    continue;
                };
                let Some(method_name) = &reference.method_name else {
                    still_unresolved.push((reference, source_idx));
                    continue;
                };
                // Resolve receiver to a node (variable)
                let receiver_node_sym =
                    Self::resolve_to_node_symbol(receiver_sym, &node_symbols, &enclosing_map);
                let Some(receiver_sym) = receiver_node_sym else {
                    still_unresolved.push((reference, source_idx));
                    continue;
                };
                let Some(receiver_idx) = graph.get_node_by_symbol(&receiver_sym) else {
                    still_unresolved.push((reference, source_idx));
                    continue;
                };
                let var_type = match graph.graph.node_weight(receiver_idx) {
                    Some(Node::Variable(v)) => v.var_type.clone(),
                    _ => {
                        still_unresolved.push((reference, source_idx));
                        continue;
                    }
                };
                let Some(type_id) = var_type else {
                    still_unresolved.push((reference, source_idx));
                    continue;
                };
                let key = (type_id, method_name.clone());
                if let Some(target_indices) = method_by_scope.get(&key)
                    && let Some(&target_idx) = target_indices.first()
                {
                    if source_idx != target_idx {
                        graph.add_edge(source_idx, target_idx, EdgeKind::Call);
                    }
                    resolved_any = true;
                    continue;
                }
                still_unresolved.push((reference, source_idx));
            }
            unresolved_calls = still_unresolved;
            if !resolved_any {
                break;
            }
        }

        graph.coverage.total_references = semantic_data
            .documents
            .iter()
            .map(|document| document.references.len())
            .sum();
        graph.coverage.unresolved_calls = unresolved_calls.len();
        graph.coverage.total_functions = semantic_data
            .documents
            .iter()
            .flat_map(|document| &document.definitions)
            .filter(|definition| definition.kind == SymbolKind::Function)
            .count();
        graph.coverage.functions_with_explicit_fragments = semantic_data
            .documents
            .iter()
            .flat_map(|document| &document.definitions)
            .filter_map(SymbolDefinition::as_function)
            .filter(|function| !function.surface_spans.is_empty())
            .count();
        graph.size_function_id = self.size_function.id().to_string();
        graph.measurement_scope_id = semantic_data.project_root.clone();
        graph.type_registry = type_registry;
        Ok(graph)
    }

    /// Check if a variable is mutable (kept for future builder logic).
    #[allow(dead_code)]
    fn is_variable_mutable(&self, symbol: &str, semantic_data: &SemanticData) -> bool {
        for doc in &semantic_data.documents {
            for def in &doc.definitions {
                if def.symbol_id == symbol
                    && let SymbolDetails::Variable(var_details) = &def.details
                {
                    return matches!(var_details.mutability, Mutability::Mutable);
                }
            }
        }
        // Default to mutable for safety
        true
    }

    /// Check if a function is underspecified (incomplete signature)
    #[allow(dead_code)]
    fn is_function_underspecified(&self, symbol: &str, graph: &ContextGraph) -> bool {
        if let Some(node_idx) = graph.get_node_by_symbol(symbol)
            && let Some(Node::Function(func)) = graph.graph.node_weight(node_idx)
        {
            // Use is_signature_complete() from FunctionNode
            return !func.is_signature_complete();
        }
        false
    }

    /// Resolve a symbol to the nearest ancestor that is a node
    fn resolve_to_node_symbol(
        symbol: &str,
        node_symbols: &HashSet<SymbolId>,
        enclosing_map: &HashMap<SymbolId, SymbolId>,
    ) -> Option<SymbolId> {
        let mut current = symbol.to_string();

        loop {
            if node_symbols.contains(&current) {
                return Some(current);
            }

            match enclosing_map.get(&current) {
                Some(parent) => current = parent.clone(),
                None => return None,
            }
        }
    }
}

/// Convert semantic span to node SourceSpan
fn convert_span(span: &SemanticSpan) -> SourceSpan {
    SourceSpan {
        start_line: span.start_line,
        start_column: span.start_column,
        end_line: span.end_line,
        end_column: span.end_column,
    }
}

/// Convert semantic span for size function (0-indexed to what size_function expects)
fn convert_span_for_size(span: &SemanticSpan) -> crate::domain::node::SourceSpan {
    crate::domain::node::SourceSpan {
        start_line: span.start_line,
        start_column: span.start_column,
        end_line: span.end_line,
        end_column: span.end_column,
    }
}

/// Build a synthetic source string for an external symbol (no project source file available).
/// Combines the signature line with up to 5 doc lines. Kept for potential future use (e.g. display).
#[allow(dead_code)]
fn build_external_source(signature: &Option<String>, doc_texts: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(sig) = signature {
        lines.push(sig.clone());
    }
    if let Some(doc) = doc_texts.first() {
        for line in doc.lines().take(5) {
            lines.push(format!("    {line}"));
        }
    }
    if lines.is_empty() {
        "pass".to_string()
    } else {
        lines.join("\n")
    }
}

/// Max context_size for external symbols; signatures only, no implementation.
/// We assign a small fixed size to external symbols because we don't explore their bodies.
const EXTERNAL_SYMBOL_MAX_TOKENS: u32 = 50;

/// Length at which to truncate external symbol signatures to avoid token explosion.
const EXTERNAL_SIGNATURE_TRUNCATE_LEN: usize = 200;

/// Build a minimal synthetic source for external symbol context_size: signature only.
/// Does not include doc or implementation; external library bodies are not useful for CF.
/// Long signatures (e.g. FastAPI File/Form) are truncated to avoid token explosion.
fn build_external_signature_only(signature: &Option<String>, def: &SymbolDefinition) -> String {
    if let Some(sig) = signature {
        let truncated = if sig.len() > EXTERNAL_SIGNATURE_TRUNCATE_LEN {
            format!("{}...", &sig[..EXTERNAL_SIGNATURE_TRUNCATE_LEN])
        } else {
            sig.clone()
        };
        format!("{} {}", def.name, truncated)
    } else {
        def.name.to_string()
    }
}

/// Infer node type from symbol kind
fn infer_node_type_from_kind(kind: &SymbolKind) -> NodeType {
    match kind {
        SymbolKind::Function => NodeType::Function,
        SymbolKind::Variable => NodeType::Variable,
        SymbolKind::Type => NodeType::Variable, // Should not happen, but default to Variable
    }
}

/// Extract signature text for documentation scoring
fn extract_signature(def: &SymbolDefinition) -> Option<String> {
    match &def.details {
        SymbolDetails::Function(func) => {
            let params: Vec<String> = func
                .parameters
                .iter()
                .map(|p| {
                    if let Some(ref type_id) = p.param_type {
                        format!("{}: {}", p.name, type_id)
                    } else {
                        p.name.clone()
                    }
                })
                .collect();
            let sig = format!("({}) -> {:?}", params.join(", "), func.return_types);
            Some(sig)
        }
        _ => None,
    }
}

/// Create a graph node from symbol definition
fn create_node_from_definition(
    core: NodeCore,
    def: &SymbolDefinition,
    is_interface_method: bool,
) -> Result<Node> {
    match &def.details {
        SymbolDetails::Function(func_details) => {
            let parameters: Vec<crate::domain::node::Parameter> = func_details
                .parameters
                .iter()
                .map(|p| crate::domain::node::Parameter {
                    name: p.name.clone(),
                    param_type: p.param_type.clone(),
                    is_high_freedom_type: p.is_high_freedom_type,
                })
                .collect();

            Ok(Node::Function(FunctionNode {
                core,
                parameters,
                is_async: func_details.modifiers.is_async,
                is_generator: func_details.modifiers.is_generator,
                visibility: convert_visibility(&func_details.modifiers.visibility),
                return_types: func_details.return_types.clone(),
                is_interface_method,
                is_constructor: func_details.modifiers.is_constructor,
                is_di_wired: func_details.modifiers.is_di_wired,
            }))
        }
        SymbolDetails::Variable(var_details) => {
            let variable_kind = match var_details.scope {
                SemanticVarScope::Global => VariableKind::Global,
                SemanticVarScope::Field => VariableKind::ClassField,
            };

            Ok(Node::Variable(VariableNode {
                core,
                var_type: var_details.var_type.clone(),
                mutability: convert_mutability(&var_details.mutability),
                variable_kind,
            }))
        }
        SymbolDetails::Type(_) => {
            // Types should not become nodes, this is an error case
            anyhow::bail!(
                "Type symbol should not be converted to node: {}",
                def.symbol_id
            )
        }
    }
}

/// Convert semantic visibility to node visibility
fn convert_visibility(vis: &Visibility) -> NodeVisibility {
    match vis {
        Visibility::Public => NodeVisibility::Public,
        Visibility::Private => NodeVisibility::Private,
        Visibility::Protected => NodeVisibility::Protected,
        Visibility::Internal => NodeVisibility::Internal,
    }
}

/// Convert semantic mutability to node mutability
fn convert_mutability(mutability: &Mutability) -> NodeMutability {
    match mutability {
        Mutability::Const => NodeMutability::Const,
        Mutability::Immutable => NodeMutability::Immutable,
        Mutability::Mutable => NodeMutability::Mutable,
    }
}

fn create_context_fragments(
    def: &SymbolDefinition,
    source_code: &str,
    size_function: &dyn SizeFunction,
    is_interface_method: bool,
) -> (ContextFragment, Option<ContextFragment>) {
    let full_span = convert_span_for_size(&def.span);
    let surface_id = format!("{}:surface", def.symbol_id);

    if def.kind != SymbolKind::Function {
        let surface_size = size_function.compute(source_code, &full_span, &[]);
        return (
            ContextFragment::new(
                surface_id,
                Some(convert_span(&def.span)),
                None,
                surface_size,
            ),
            None,
        );
    }

    let use_signature_only = is_interface_method
        || def
            .as_function()
            .is_some_and(|function| function.modifiers.use_signature_only_for_size);

    if let Some(function) = def.as_function()
        && !function.surface_spans.is_empty()
    {
        let surface_size = function
            .surface_spans
            .iter()
            .map(|span| size_function.compute(source_code, &convert_span_for_size(span), &[]))
            .sum();
        let surface_span =
            (function.surface_spans.len() == 1).then(|| convert_span(&function.surface_spans[0]));
        let surface = ContextFragment::new(surface_id, surface_span, None, surface_size);
        let implementation_size = function
            .implementation_spans
            .iter()
            .map(|span| size_function.compute(source_code, &convert_span_for_size(span), &[]))
            .sum();
        let implementation = (!use_signature_only && !function.implementation_spans.is_empty())
            .then(|| {
                ContextFragment::new(
                    format!("{}:implementation", def.symbol_id),
                    (function.implementation_spans.len() == 1)
                        .then(|| convert_span(&function.implementation_spans[0])),
                    None,
                    implementation_size,
                )
            });
        return (surface, implementation);
    }

    let signature_span = extract_signature_span(&def.span, source_code);
    let signature_size = size_function.compute(source_code, &signature_span, &[]);
    let documentation = def.documentation.join("\n\n");
    let documentation_size = compute_text_size(size_function, &documentation);
    let surface_size = signature_size.saturating_add(documentation_size);
    let surface = ContextFragment::new(
        surface_id,
        Some(signature_span),
        (!documentation.is_empty()).then_some(documentation),
        surface_size,
    );

    if use_signature_only {
        return (surface, None);
    }

    // The implementation excludes the signature and documentation already charged
    // by the surface fragment.
    let logic_size = size_function.compute(source_code, &full_span, &def.documentation);
    let implementation_size = logic_size.saturating_sub(signature_size);
    let implementation = ContextFragment::new(
        format!("{}:implementation", def.symbol_id),
        Some(convert_span(&def.span)),
        None,
        implementation_size,
    );
    (surface, Some(implementation))
}

fn compute_text_size(size_function: &dyn SizeFunction, text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let lines: Vec<_> = text.lines().collect();
    let end_line = lines.len().saturating_sub(1) as u32;
    let end_column = lines.last().map_or(0, |line| line.chars().count() as u32);
    size_function.compute(
        text,
        &SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line,
            end_column,
        },
        &[],
    )
}

/// Create TypeInfo for registering in TypeRegistry
fn create_type_info(
    def: &SymbolDefinition,
    surface_fragment: ContextFragment,
    doc_score: f32,
) -> TypeInfo {
    let mut type_kind = TypeKind::Class;
    let mut is_abstract = false;
    let mut type_param_count = 0;
    let mut type_var_info = None;

    if let SymbolDetails::Type(type_details) = &def.details {
        type_kind = match type_details.kind {
            crate::domain::semantic::TypeKind::Class => TypeKind::Class,
            crate::domain::semantic::TypeKind::Interface => TypeKind::Interface,
            crate::domain::semantic::TypeKind::Struct => TypeKind::Struct,
            crate::domain::semantic::TypeKind::Enum => TypeKind::Enum,
            crate::domain::semantic::TypeKind::TypeAlias => TypeKind::TypeAlias,
            crate::domain::semantic::TypeKind::TypeVar => {
                if let Some(tp) = type_details.type_params.first() {
                    let bound = tp.bounds.first().cloned();
                    let constraints = if tp.bounds.len() > 1 {
                        tp.bounds.clone()
                    } else {
                        Vec::new()
                    };
                    type_var_info = Some(TypeVarInfo { bound, constraints });
                } else {
                    type_var_info = Some(TypeVarInfo {
                        bound: None,
                        constraints: Vec::new(),
                    });
                }
                TypeKind::TypeVar
            }
            _ => TypeKind::Class,
        };
        is_abstract = type_details.is_abstract;
        type_param_count = type_details.type_params.len() as u32;
    }

    TypeInfo {
        definition: TypeDefAttribute {
            type_kind,
            is_abstract,
            type_param_count,
            type_var_info,
        },
        context_size: surface_fragment.context_size,
        surface_fragment,
        doc_score,
        documentation: def.documentation.clone(),
    }
}

/// Extract only the signature portion of a method span (first line to colon/semicolon)
/// For interface methods, we only want to count the signature, not any implementation body
fn extract_signature_span(span: &SemanticSpan, source_code: &str) -> SourceSpan {
    let lines: Vec<&str> = source_code.lines().collect();
    let start_line = span.start_line as usize;

    if start_line >= lines.len() {
        // Fallback: use original span
        return SourceSpan {
            start_line: span.start_line,
            start_column: span.start_column,
            end_line: span.end_line,
            end_column: span.end_column,
        };
    }

    // Find first colon or semicolon (Python/TypeScript signature end marker)
    for (i, line) in lines.iter().enumerate().skip(start_line) {
        if line.contains(':') || line.contains(';') {
            return SourceSpan {
                start_line: span.start_line,
                start_column: span.start_column,
                end_line: i as u32,
                end_column: line.len() as u32,
            };
        }
    }

    // Fallback: use original span
    SourceSpan {
        start_line: span.start_line,
        start_column: span.start_column,
        end_line: span.end_line,
        end_column: span.end_column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::semantic::{FunctionDetails, SourceLocation};

    fn test_function_def(symbol_id: &str) -> SymbolDefinition {
        SymbolDefinition {
            symbol_id: symbol_id.to_string(),
            kind: SymbolKind::Function,
            name: "test_func".to_string(),
            display_name: "test_func".to_string(),
            location: SourceLocation {
                file_path: "test.py".to_string(),
                line: 0,
                column: 0,
            },
            span: SemanticSpan {
                start_line: 0,
                start_column: 0,
                end_line: 10,
                end_column: 0,
            },
            enclosing_symbol: None,
            is_external: false,
            documentation: vec![],
            details: SymbolDetails::Function(FunctionDetails::default()),
        }
    }

    #[test]
    fn test_convert_visibility() {
        assert!(matches!(
            convert_visibility(&Visibility::Public),
            NodeVisibility::Public
        ));
        assert!(matches!(
            convert_visibility(&Visibility::Private),
            NodeVisibility::Private
        ));
    }

    #[test]
    fn test_create_type_info_from_class() {
        let def = test_function_def("TestClass");
        let info = create_type_info(
            &def,
            ContextFragment::new("TestClass:surface".to_string(), None, None, 100),
            0.8,
        );
        assert_eq!(info.definition.type_kind, TypeKind::Class);
        assert!(!info.definition.is_abstract);
    }

    #[test]
    fn test_extract_signature_span_python() {
        let source = "    def method(self, x: int) -> str:\n        return str(x)\n        pass\n";
        let span = SemanticSpan {
            start_line: 0,
            start_column: 4,
            end_line: 2,
            end_column: 12,
        };

        let sig_span = extract_signature_span(&span, source);

        // Should stop at the first line with colon
        assert_eq!(sig_span.start_line, 0);
        assert_eq!(sig_span.end_line, 0);
    }

    struct SpanSize;

    impl SizeFunction for SpanSize {
        fn compute(&self, _source: &str, span: &SourceSpan, _doc_texts: &[String]) -> u32 {
            span.end_line.saturating_sub(span.start_line) + 1
        }
    }

    #[test]
    fn test_explicit_fragment_spans_drive_surface_and_implementation_costs() {
        let mut def = test_function_def("test_func");
        let SymbolDetails::Function(function) = &mut def.details else {
            panic!("function fixture");
        };
        function.surface_spans = vec![
            SemanticSpan {
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 10,
            },
            SemanticSpan {
                start_line: 1,
                start_column: 0,
                end_line: 1,
                end_column: 10,
            },
        ];
        function.implementation_spans = vec![SemanticSpan {
            start_line: 2,
            start_column: 0,
            end_line: 3,
            end_column: 10,
        }];

        let (surface, implementation) = create_context_fragments(
            &def,
            "def f():\n    \"doc\"\n    return 1\n",
            &SpanSize,
            false,
        );

        assert_eq!(surface.context_size, 2);
        assert_eq!(implementation.unwrap().context_size, 2);
    }
}
