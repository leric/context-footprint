use crate::adapters::doc_scorer::heuristic::HeuristicDocScorer;
use crate::adapters::size_function::tiktoken::TiktokenSizeFunction;
use crate::adapters::test_detector::UniversalTestDetector;
use crate::app::dto::*;
use crate::domain::builder::GraphBuilder;
use crate::domain::edge::EdgeKind;
use crate::domain::graph::ContextGraph;
use crate::domain::node::{Node, NodeId};
use crate::domain::policy::{PruningDecision, PruningParams};
use crate::domain::ports::SourceReader;
use crate::domain::semantic::SemanticData;
use crate::domain::solver::{BoundaryStop, CfSolver, ReachabilityOptions};
use anyhow::{Context as _, Result, anyhow};
use petgraph::graph::NodeIndex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct ContextEngine {
    inner: Arc<RwLock<EngineData>>,
}

struct EngineData {
    semantic_path: PathBuf,
    project_root: PathBuf,
    project_root_override: Option<PathBuf>,
    graph: Arc<ContextGraph>,
    node_id_to_index: HashMap<NodeId, NodeIndex>,
    node_id_to_symbol: HashMap<NodeId, String>,
    source_reader: Arc<dyn SourceReader>,
}

impl ContextEngine {
    /// Construct an engine from an already-built graph.
    ///
    /// Used for testing or when the graph is built by an external semantic data source (e.g. LSP extractor).
    pub fn from_prebuilt(
        semantic_path: PathBuf,
        project_root: PathBuf,
        graph: ContextGraph,
        source_reader: Arc<dyn SourceReader>,
    ) -> Self {
        let (node_id_to_index, node_id_to_symbol) = build_node_maps(&graph);
        Self {
            inner: Arc::new(RwLock::new(EngineData {
                semantic_path,
                project_root,
                project_root_override: None,
                graph: Arc::new(graph),
                node_id_to_index,
                node_id_to_symbol,
                source_reader,
            })),
        }
    }

    pub fn load_from_json(json_path: &Path) -> Result<Self> {
        let json_content =
            std::fs::read_to_string(json_path).context("Failed to read JSON file")?;
        let semantic_data: SemanticData =
            serde_json::from_str(&json_content).context("Failed to parse SemanticData JSON")?;

        let project_root = PathBuf::from(&semantic_data.project_root);

        struct SimpleSourceReader {
            project_root: String,
        }

        impl SourceReader for SimpleSourceReader {
            fn read(&self, path: &Path) -> Result<String> {
                let full_path = Path::new(&self.project_root).join(path);
                std::fs::read_to_string(&full_path)
                    .with_context(|| format!("Failed to read source file: {}", full_path.display()))
            }

            fn read_lines(
                &self,
                path: &str,
                start_line: usize,
                end_line: usize,
            ) -> Result<Vec<String>> {
                let content = self.read(Path::new(path))?;
                let lines: Vec<String> = content.lines().map(String::from).collect();
                let start_idx = start_line.min(lines.len());
                let end_idx = (end_line + 1).min(lines.len()); // end_line inclusive
                Ok(lines[start_idx..end_idx].to_vec())
            }
        }

        let source_reader: Arc<dyn SourceReader> = Arc::new(SimpleSourceReader {
            project_root: semantic_data.project_root.clone(),
        });

        let size_function = Box::new(TiktokenSizeFunction::new());
        let doc_scorer = Box::new(HeuristicDocScorer);
        let builder = GraphBuilder::new(size_function, doc_scorer);

        let graph = builder
            .build(semantic_data, source_reader.as_ref())
            .context("Failed to build context graph")?;

        let (node_id_to_index, node_id_to_symbol) = build_node_maps(&graph);

        Ok(Self {
            inner: Arc::new(RwLock::new(EngineData {
                semantic_path: json_path.to_path_buf(),
                project_root,
                project_root_override: None,
                graph: Arc::new(graph),
                node_id_to_index,
                node_id_to_symbol,
                source_reader,
            })),
        })
    }

    pub fn reload(&self) -> Result<HealthResponse> {
        let path = {
            let data = self.inner.read().unwrap();
            data.semantic_path.clone()
        };
        let new_engine = Self::load_from_json(&path)?;
        let new_data = new_engine.inner.read().unwrap();

        let mut data = self.inner.write().unwrap();
        data.project_root = new_data.project_root.clone();
        data.project_root_override = new_data.project_root_override.clone();
        data.graph = new_data.graph.clone();
        data.node_id_to_index = new_data.node_id_to_index.clone();
        data.node_id_to_symbol = new_data.node_id_to_symbol.clone();
        data.source_reader = new_data.source_reader.clone();

        Ok(HealthResponse {
            semantic_path: data.semantic_path.to_string_lossy().to_string(),
            project_root: data.project_root.to_string_lossy().to_string(),
            node_count: data.graph.graph.node_count(),
            edge_count: data.graph.graph.edge_count(),
        })
    }

    pub fn health(&self) -> HealthResponse {
        let data = self.inner.read().unwrap();
        HealthResponse {
            semantic_path: data.semantic_path.to_string_lossy().to_string(),
            project_root: data.project_root.to_string_lossy().to_string(),
            node_count: data.graph.graph.node_count(),
            edge_count: data.graph.graph.edge_count(),
        }
    }

    pub fn compute(&self, req: ComputeRequest) -> Result<ComputeResponse> {
        let data = self.inner.read().unwrap();
        let graph = data.graph.as_ref();

        let mut starts = Vec::with_capacity(req.symbols.len());
        let mut resolutions = Vec::with_capacity(req.symbols.len());
        let mut effective_symbols = Vec::new();

        for sym in &req.symbols {
            let resolution = self.resolve_anchor_locked(&data, sym);
            match &resolution.expanded_to {
                Some(expanded) => {
                    for expanded_sym in expanded {
                        if let Some(idx) = graph.get_node_by_symbol(expanded_sym) {
                            starts.push(idx);
                            effective_symbols.push(expanded_sym.clone());
                        }
                    }
                }
                None => {
                    if resolution.unresolved_reason.is_some() {
                        return Err(anyhow!("Symbol not found: {}", sym));
                    }
                    if let Some(idx) = graph.get_node_by_symbol(sym) {
                        starts.push(idx);
                        effective_symbols.push(sym.clone());
                    }
                }
            }
            resolutions.push(resolution);
        }

        let solver = CfSolver::new(data.graph.clone(), pruning_params(req.policy));
        let result = solver.compute_cf(&starts, req.max_tokens);
        let boundary_stops = boundary_stop_dtos(&result.boundary_stops);

        let reachable_nodes_ordered = result
            .reachable_nodes_ordered
            .iter()
            .filter_map(|id| self.node_id_to_reachable_node_locked(&data, *id))
            .collect::<Vec<_>>();

        let reachable_nodes_by_layer = result
            .reachable_nodes_by_layer
            .iter()
            .map(|layer| {
                layer
                    .iter()
                    .filter_map(|id| self.node_id_to_reachable_node_locked(&data, *id))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        Ok(ComputeResponse {
            starting_symbols: effective_symbols,
            total_context_size: result.total_context_size,
            cf_total: result.cf_total,
            cf_out: result.cf_out,
            cf_in: result.cf_in,
            cf_overlap: result.cf_overlap,
            out_fragments: result.out_fragments,
            in_fragments: result.in_fragments,
            truncated: result.truncated,
            unresolved_state_count: result.unresolved_states.len(),
            boundary_stop_count: result.boundary_stops.len(),
            boundary_stops,
            boundary_policy_id: result.boundary_policy_id,
            size_function_id: result.size_function_id,
            measurement_scope_id: result.measurement_scope_id,
            graph_total_references: result.graph_total_references,
            graph_unresolved_calls: result.graph_unresolved_calls,
            graph_total_functions: result.graph_total_functions,
            graph_functions_with_explicit_fragments: result.graph_functions_with_explicit_fragments,
            reachable_node_count: result.reachable_set.len(),
            reachable_nodes_by_layer,
            reachable_nodes_ordered,
            anchor_resolutions: Some(resolutions),
        })
    }

    pub fn reachable(&self, req: ReachabilityRequest) -> Result<ReachabilityResponse> {
        let data = self.inner.read().unwrap();
        let graph = data.graph.as_ref();

        let mut start_indices = Vec::new();
        let mut unresolved_from = Vec::new();
        for symbol in &req.from {
            if let Some(idx) = graph.get_node_by_symbol(symbol) {
                start_indices.push(idx);
            } else {
                unresolved_from.push(symbol.clone());
            }
        }

        let mut resolved_targets: Vec<(String, NodeIndex)> = Vec::new();
        let mut unresolved_to = Vec::new();
        for symbol in &req.to {
            if let Some(idx) = graph.get_node_by_symbol(symbol) {
                resolved_targets.push((symbol.clone(), idx));
            } else {
                unresolved_to.push(symbol.clone());
            }
        }

        let solver = CfSolver::new(data.graph.clone(), pruning_params(req.policy));
        let result = solver.reachable(
            &start_indices,
            &resolved_targets
                .iter()
                .map(|(_, idx)| *idx)
                .collect::<Vec<_>>(),
            ReachabilityOptions {
                witness_paths: req.witness_paths,
                max_paths: req.max_paths,
            },
        );

        let hit_target_ids: HashSet<NodeId> = result.hit_targets.iter().copied().collect();
        let mut hit_targets = Vec::new();
        for (symbol, idx) in &resolved_targets {
            let node_id = graph.node(*idx).core().id;
            if hit_target_ids.contains(&node_id) && !hit_targets.contains(symbol) {
                hit_targets.push(symbol.clone());
            }
        }

        let witness_paths = result
            .witness_paths
            .into_iter()
            .map(|path| {
                path.into_iter()
                    .filter_map(|node_id| data.node_id_to_symbol.get(&node_id).cloned())
                    .collect::<Vec<_>>()
            })
            .collect();

        Ok(ReachabilityResponse {
            reachable: result.reachable,
            hit_targets,
            unresolved_from,
            unresolved_to,
            visited_node_count: result.visited_node_count,
            witness_paths,
        })
    }

    pub fn stats(&self, include_tests: bool, policy: PolicyKind) -> Result<StatsResponse> {
        let data = self.inner.read().unwrap();
        let graph = data.graph.as_ref();
        let solver = CfSolver::new(data.graph.clone(), pruning_params(policy));
        let test_detector = UniversalTestDetector::new();

        let mut function_cf: Vec<u32> = Vec::new();

        for node_idx in graph.graph.node_indices() {
            let node = graph.node(node_idx);

            // Only count function nodes
            if !matches!(node, Node::Function(_)) {
                continue;
            }

            if !include_tests {
                let symbol = data
                    .node_id_to_symbol
                    .get(&node.core().id)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                if test_detector.is_test_code(symbol, &node.core().file_path) {
                    continue;
                }
            }

            let cf = solver.compute_cf_total(node_idx);
            function_cf.push(cf);
        }

        Ok(StatsResponse {
            functions: compute_distribution(function_cf),
        })
    }

    pub fn top(
        &self,
        limit: usize,
        node_type: &str,
        include_tests: bool,
        policy: PolicyKind,
    ) -> Result<TopResponse> {
        let data = self.inner.read().unwrap();
        let graph = data.graph.as_ref();
        let solver = CfSolver::new(data.graph.clone(), pruning_params(policy));
        let test_detector = UniversalTestDetector::new();

        let mut results: Vec<TopItem> = Vec::new();
        for (symbol, &node_idx) in &graph.symbol_to_node {
            let node = graph.node(node_idx);

            let type_str = detailed_node_type_str(node);
            let base_type = node_type_str(node);
            if node_type != "all" && node_type != type_str && node_type != base_type {
                continue;
            }

            if !include_tests && test_detector.is_test_code(symbol, &node.core().file_path) {
                continue;
            }

            let cf = solver.compute_cf_total(node_idx);
            results.push(TopItem {
                symbol: symbol.clone(),
                node_type: type_str.to_string(),
                cf,
            });
        }

        results.sort_by(|a, b| b.cf.cmp(&a.cf));
        results.truncate(limit);
        Ok(TopResponse { items: results })
    }

    pub fn search(
        &self,
        pattern: &str,
        with_cf: bool,
        limit: Option<usize>,
        include_tests: bool,
        policy: PolicyKind,
    ) -> Result<SearchResponse> {
        let data = self.inner.read().unwrap();
        let graph = data.graph.as_ref();
        let solver = CfSolver::new(data.graph.clone(), pruning_params(policy));
        let test_detector = UniversalTestDetector::new();

        let pattern_lower = pattern.to_lowercase();
        let mut matches: Vec<(String, String, u32)> = Vec::new();

        for (symbol, &node_idx) in &graph.symbol_to_node {
            if !symbol.to_lowercase().contains(&pattern_lower) {
                continue;
            }

            let node = graph.node(node_idx);
            let type_str = detailed_node_type_str(node).to_string();

            if !include_tests && test_detector.is_test_code(symbol, &node.core().file_path) {
                continue;
            }

            // Always compute CF for sorting (same as current CLI behavior).
            let cf = solver.compute_cf_total(node_idx);
            matches.push((symbol.clone(), type_str, cf));
        }

        // Also search for class symbols in TypeRegistry
        let type_ids: Vec<_> = graph.type_registry.type_ids().cloned().collect();
        for type_id in &type_ids {
            if !type_id.to_lowercase().contains(&pattern_lower) {
                continue;
            }
            // Skip if already matched as a node
            if graph.symbol_to_node.contains_key(type_id) {
                continue;
            }
            let expanded = expand_class_anchor(graph, type_id);
            if expanded.is_empty() {
                matches.push((type_id.clone(), "class".to_string(), 0));
            } else {
                let start_indices: Vec<_> = expanded
                    .iter()
                    .filter_map(|s| graph.get_node_by_symbol(s))
                    .collect();
                if with_cf && !start_indices.is_empty() {
                    let result = solver.compute_cf(&start_indices, None);
                    matches.push((
                        type_id.clone(),
                        "class".to_string(),
                        result.total_context_size,
                    ));
                } else {
                    matches.push((type_id.clone(), "class".to_string(), 0));
                }
            }
        }

        matches.sort_by(|a, b| b.2.cmp(&a.2));
        let total_matches = matches.len();

        let display_count = limit.unwrap_or(matches.len());
        let matches_to_show = &matches[..matches.len().min(display_count)];

        Ok(SearchResponse {
            items: matches_to_show
                .iter()
                .map(|(symbol, node_type, cf)| SearchItem {
                    symbol: symbol.clone(),
                    node_type: node_type.clone(),
                    cf: if with_cf || limit.is_some() {
                        Some(*cf)
                    } else {
                        None
                    },
                })
                .collect(),
            total_matches,
        })
    }

    pub fn context(&self, req: ContextRequest) -> Result<ContextResponse> {
        let data = self.inner.read().unwrap();
        let graph = data.graph.as_ref();
        let node_idx = graph
            .get_node_by_symbol(&req.symbol)
            .ok_or_else(|| anyhow!("Symbol not found: {}", req.symbol))?;

        let solver = CfSolver::new(data.graph.clone(), pruning_params(req.policy));
        let result = solver.compute_cf(&[node_idx], req.max_tokens);
        let boundary_stops = boundary_stop_dtos(&result.boundary_stops);

        let mut layers: Vec<ContextLayer> = Vec::new();

        for (depth, layer) in result.reachable_nodes_by_layer.iter().enumerate() {
            if layer.is_empty() {
                continue;
            }

            let mut files_map: HashMap<String, Vec<NodeIndex>> = HashMap::new();
            for node_id in layer {
                let idx = data.node_id_to_index.get(node_id).copied().ok_or_else(|| {
                    anyhow!("Internal error: missing node_id_to_index for {node_id}")
                })?;
                let file_path = graph.node(idx).core().file_path.clone();
                files_map.entry(file_path).or_default().push(idx);
            }

            let mut file_list: Vec<(String, Vec<NodeIndex>)> = files_map.into_iter().collect();
            file_list.sort_by(|a, b| a.0.cmp(&b.0));

            let mut files: Vec<ContextFile> = Vec::new();
            for (file_path, mut nodes) in file_list {
                nodes.sort_by_key(|&idx| graph.node(idx).core().span.start_line);

                let top_level_nodes = filter_top_level_nodes(graph, &data.node_id_to_symbol, nodes);

                let mut out_nodes: Vec<ContextNode> = Vec::new();
                for idx in top_level_nodes {
                    let n = graph.node(idx);
                    let core = n.core();
                    let symbol = data
                        .node_id_to_symbol
                        .get(&core.id)
                        .cloned()
                        .unwrap_or_else(|| core.name.clone());

                    let code = if req.include_code {
                        let full_path = data.project_root.join(&core.file_path);
                        let lines = data.source_reader.read_lines(
                            &full_path.to_string_lossy(),
                            core.span.start_line as usize,
                            core.span.end_line as usize,
                        )?;
                        Some(
                            lines
                                .into_iter()
                                .enumerate()
                                .map(|(i, text)| CodeLine {
                                    line_number: core.span.start_line + 1 + i as u32,
                                    text,
                                })
                                .collect(),
                        )
                    } else {
                        None
                    };

                    out_nodes.push(ContextNode {
                        id: core.id,
                        symbol,
                        node_type: node_type_str(n).to_string(),
                        context_size: core.context_size,
                        span: span_dto(&core.span),
                        doc_score: core.doc_score,
                        is_external: core.is_external,
                        code,
                    });
                }

                files.push(ContextFile {
                    file_path,
                    nodes: out_nodes,
                });
            }

            layers.push(ContextLayer { depth, files });
        }

        let traversal_steps = if req.show_traversal {
            let mut steps = Vec::with_capacity(result.traversal_steps.len());
            for step in &result.traversal_steps {
                let node = self
                    .node_id_to_reachable_node_locked(&data, step.node_id)
                    .ok_or_else(|| {
                        anyhow!("Internal error: missing node for id {}", step.node_id)
                    })?;
                let node_idx = *data.node_id_to_index.get(&step.node_id).ok_or_else(|| {
                    anyhow!(
                        "Internal error: missing node_id_to_index for {}",
                        step.node_id
                    )
                })?;
                let is_signature_complete = match data.graph.node(node_idx) {
                    Node::Function(f) => Some(f.is_signature_complete()),
                    _ => None,
                };

                steps.push(TraversalStepDto {
                    node,
                    edge_kind: step
                        .incoming_edge_kind
                        .as_ref()
                        .map(edge_kind_display)
                        .map(String::from),
                    decision: step
                        .decision
                        .as_ref()
                        .map(decision_display)
                        .map(String::from),
                    is_signature_complete,
                });
            }
            Some(steps)
        } else {
            None
        };

        Ok(ContextResponse {
            symbol: req.symbol,
            total_context_size: result.total_context_size,
            cf_total: result.cf_total,
            cf_out: result.cf_out,
            cf_in: result.cf_in,
            cf_overlap: result.cf_overlap,
            out_fragments: result.out_fragments,
            in_fragments: result.in_fragments,
            truncated: result.truncated,
            unresolved_state_count: result.unresolved_states.len(),
            boundary_stop_count: result.boundary_stops.len(),
            boundary_stops,
            boundary_policy_id: result.boundary_policy_id,
            size_function_id: result.size_function_id,
            measurement_scope_id: result.measurement_scope_id,
            graph_total_references: result.graph_total_references,
            graph_unresolved_calls: result.graph_unresolved_calls,
            graph_total_functions: result.graph_total_functions,
            graph_functions_with_explicit_fragments: result.graph_functions_with_explicit_fragments,
            reachable_node_count: result.reachable_set.len(),
            layers,
            traversal_steps,
        })
    }

    /// Resolve an input anchor symbol: function/method, class, or variable.
    fn resolve_anchor_locked(&self, data: &EngineData, symbol: &str) -> AnchorResolution {
        let graph = data.graph.as_ref();

        // 1. Direct graph node match (function/method/variable)
        if let Some(idx) = graph.get_node_by_symbol(symbol) {
            let node = graph.node(idx);
            let kind = detailed_node_type_str(node);
            return AnchorResolution {
                input: symbol.to_string(),
                resolved_kind: kind.to_string(),
                expanded_to: None,
                unresolved_reason: None,
            };
        }

        // 2. Class anchor: symbol exists in TypeRegistry
        if graph.type_registry.contains(symbol) {
            let expanded = expand_class_anchor(graph, symbol);
            if expanded.is_empty() {
                return AnchorResolution {
                    input: symbol.to_string(),
                    resolved_kind: "class".to_string(),
                    expanded_to: Some(vec![]),
                    unresolved_reason: Some(
                        "Class resolved but has no expandable members".to_string(),
                    ),
                };
            }
            return AnchorResolution {
                input: symbol.to_string(),
                resolved_kind: "class".to_string(),
                expanded_to: Some(expanded),
                unresolved_reason: None,
            };
        }

        // 3. Unresolved
        AnchorResolution {
            input: symbol.to_string(),
            resolved_kind: "unknown".to_string(),
            expanded_to: None,
            unresolved_reason: Some("Symbol not found in graph or type registry".to_string()),
        }
    }

    fn node_id_to_reachable_node_locked(
        &self,
        data: &EngineData,
        id: NodeId,
    ) -> Option<ReachableNode> {
        let idx = *data.node_id_to_index.get(&id)?;
        let node = data.graph.node(idx);
        let core = node.core();
        let symbol = data
            .node_id_to_symbol
            .get(&id)
            .cloned()
            .unwrap_or_else(|| core.name.clone());
        Some(ReachableNode {
            id,
            symbol,
            node_type: node_type_str(node).to_string(),
            context_size: core.context_size,
            file_path: core.file_path.clone(),
            span: span_dto(&core.span),
            doc_score: core.doc_score,
            is_external: core.is_external,
        })
    }
}

fn build_node_maps(graph: &ContextGraph) -> (HashMap<NodeId, NodeIndex>, HashMap<NodeId, String>) {
    let mut node_id_to_index = HashMap::new();
    let mut node_id_to_symbol = HashMap::new();

    for idx in graph.graph.node_indices() {
        let id = graph.node(idx).core().id;
        node_id_to_index.insert(id, idx);
    }

    for (symbol, &idx) in &graph.symbol_to_node {
        let id = graph.node(idx).core().id;
        node_id_to_symbol
            .entry(id)
            .or_insert_with(|| symbol.clone());
    }

    (node_id_to_index, node_id_to_symbol)
}

fn pruning_params(kind: PolicyKind) -> PruningParams {
    match kind {
        PolicyKind::Academic => PruningParams::academic(0.5),
        PolicyKind::Strict => PruningParams::strict(0.8),
    }
}

fn boundary_stop_dtos(stops: &[BoundaryStop]) -> Vec<BoundaryStopDto> {
    stops
        .iter()
        .map(|stop| BoundaryStopDto {
            source_node_id: stop.source,
            target_node_id: stop.target,
            direction: format!("{:?}", stop.direction),
            relation: format!("{:?}", stop.relation),
        })
        .collect()
}

fn node_type_str(node: &Node) -> &'static str {
    match node {
        Node::Function(_) => "function",
        Node::Variable(_) => "variable",
    }
}

/// Return a more specific kind label for display and search results.
fn detailed_node_type_str(node: &Node) -> &'static str {
    match node {
        Node::Function(f) => {
            if f.core.scope.is_some() {
                "method"
            } else {
                "function"
            }
        }
        Node::Variable(v) => match (&v.variable_kind, &v.mutability) {
            (crate::domain::node::VariableKind::ClassField, _) => "class_variable",
            (_, crate::domain::node::Mutability::Const) => "constant",
            (crate::domain::node::VariableKind::Global, _) => "module_variable",
            _ => "variable",
        },
    }
}

/// Expand a class anchor into its public-visibility member methods.
///
/// Language-specific naming conventions (e.g. Python dunder methods) are handled
/// by the extractor layer which sets the correct `Visibility` on each method.
/// The engine only checks visibility, staying language-agnostic.
fn expand_class_anchor(graph: &ContextGraph, class_symbol: &str) -> Vec<String> {
    let members = graph.find_class_members(class_symbol);
    let mut result = Vec::new();
    for (symbol, idx) in &members {
        let node = graph.node(*idx);
        if let Node::Function(f) = node
            && f.visibility == crate::domain::node::Visibility::Public
        {
            result.push(symbol.clone());
        }
    }
    result
}

fn span_dto(span: &crate::domain::node::SourceSpan) -> SpanDto {
    // Domain span is 0-based; both start_line and end_line are inclusive.
    // 1-based display: add 1 to each.
    SpanDto {
        start_line: span.start_line,
        start_column: span.start_column,
        end_line: span.end_line,
        end_column: span.end_column,
        start_line_1based: span.start_line + 1,
        end_line_1based: span.end_line + 1,
    }
}

fn edge_kind_display(ek: &EdgeKind) -> &'static str {
    match ek {
        EdgeKind::Call => "Call",
        EdgeKind::Read => "Read",
        EdgeKind::Write => "Write",
        EdgeKind::OverriddenBy => "OverriddenBy",
        EdgeKind::Annotates => "Annotates",
    }
}

fn decision_display(d: &PruningDecision) -> &'static str {
    match d {
        PruningDecision::Boundary => "Boundary",
        PruningDecision::Transparent => "Transparent",
    }
}

fn compute_distribution(mut sizes: Vec<u32>) -> CfDistribution {
    if sizes.is_empty() {
        return CfDistribution {
            count: 0,
            percentiles: vec![],
            average: 0,
            median: 0,
            min: 0,
            max: 0,
        };
    }

    sizes.sort_unstable();
    let count = sizes.len();

    let percentiles = (5..=100)
        .step_by(5)
        .map(|p| {
            let idx = ((p * (count - 1)) / 100).min(count - 1);
            PercentileValue {
                percentile: p as u32,
                tokens: sizes[idx],
            }
        })
        .collect::<Vec<_>>();

    let sum: u64 = sizes.iter().map(|&s| s as u64).sum();
    let average = sum / count as u64;
    let median = sizes[count / 2];

    CfDistribution {
        count,
        percentiles,
        average,
        median,
        min: sizes[0],
        max: sizes[count - 1],
    }
}

fn symbol_is_parameter(symbol: &str) -> bool {
    symbol.contains("().(") && symbol.ends_with(')')
}

fn filter_top_level_nodes(
    graph: &ContextGraph,
    node_id_to_symbol: &HashMap<NodeId, String>,
    sorted_nodes: Vec<NodeIndex>,
) -> Vec<NodeIndex> {
    let mut top_level_nodes: Vec<NodeIndex> = Vec::new();

    for idx in sorted_nodes {
        let core = graph.node(idx).core();

        let symbol = node_id_to_symbol
            .get(&core.id)
            .map(|s| s.as_str())
            .unwrap_or("");
        let is_sub_node = symbol_is_parameter(symbol);

        let is_contained = top_level_nodes.iter().any(|&prev_idx| {
            let prev_core = graph.node(prev_idx).core();
            core.span.start_line >= prev_core.span.start_line
                && core.span.end_line <= prev_core.span.end_line
                && idx != prev_idx
        });

        if !is_contained && !is_sub_node {
            top_level_nodes.push(idx);
        }
    }

    top_level_nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::edge::EdgeKind;
    use crate::domain::node::{
        FunctionNode, Mutability, Node, NodeCore, SourceSpan, VariableKind, VariableNode,
        Visibility,
    };

    struct MockReader;
    impl SourceReader for MockReader {
        fn read(&self, _path: &Path) -> Result<String> {
            Ok("line1\nline2\nline3\nline4\n".into())
        }

        fn read_lines(
            &self,
            _path: &str,
            start_line: usize,
            end_line: usize,
        ) -> Result<Vec<String>> {
            let lines = [
                "line1".to_string(),
                "line2".to_string(),
                "line3".to_string(),
                "line4".to_string(),
            ];
            let start = start_line.min(lines.len());
            let end = (end_line + 1).min(lines.len()); // end_line inclusive
            Ok(lines[start..end].to_vec())
        }
    }

    fn make_core(id: u32, name: &str, file_path: &str, start_line: u32, end_line: u32) -> NodeCore {
        NodeCore::new(
            id,
            name.to_string(),
            None,
            10,
            SourceSpan {
                start_line,
                start_column: 0,
                end_line,
                end_column: 0,
            },
            1.0,
            false,
            file_path.to_string(),
        )
    }

    fn test_graph() -> ContextGraph {
        let mut g = ContextGraph::new();

        let f1 = Node::Function(FunctionNode {
            core: make_core(0, "func1", "app/main.py", 0, 1),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });

        let v1 = Node::Variable(VariableNode {
            core: make_core(1, "var1", "app/main.py", 0, 1),
            var_type: None,
            mutability: Mutability::Mutable,
            variable_kind: VariableKind::Global,
        });

        let i_f1 = g.add_node("sym/func1().".into(), f1);
        let i_v1 = g.add_node("sym/var1.".into(), v1);
        g.add_edge(i_f1, i_v1, EdgeKind::Read);

        g
    }

    #[test]
    fn test_engine_health_and_compute() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            test_graph(),
            Arc::new(MockReader),
        );

        let health = engine.health();
        assert_eq!(health.node_count, 2);
        assert_eq!(health.edge_count, 1);

        let res = engine
            .compute(ComputeRequest {
                symbols: vec!["sym/func1().".into()],
                policy: PolicyKind::Academic,
                max_tokens: None,
            })
            .unwrap();
        assert!(res.total_context_size > 0);
        assert_eq!(res.reachable_node_count, 2);
        assert!(!res.reachable_nodes_ordered.is_empty());
    }

    #[test]
    fn test_engine_search_and_top() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            test_graph(),
            Arc::new(MockReader),
        );

        let search = engine
            .search("func", true, None, true, PolicyKind::Academic)
            .unwrap();
        assert_eq!(search.total_matches, 1);
        assert_eq!(search.items[0].symbol, "sym/func1().");

        let top = engine.top(10, "all", true, PolicyKind::Academic).unwrap();
        assert_eq!(top.items.len(), 2);
    }

    #[test]
    fn test_engine_context_include_code() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            test_graph(),
            Arc::new(MockReader),
        );

        let ctx = engine
            .context(ContextRequest {
                symbol: "sym/func1().".into(),
                policy: PolicyKind::Academic,
                max_tokens: None,
                include_code: true,
                show_traversal: false,
            })
            .unwrap();
        assert_eq!(ctx.symbol, "sym/func1().");
        assert!(!ctx.layers.is_empty());
        let any_code = ctx
            .layers
            .iter()
            .flat_map(|l| l.files.iter())
            .flat_map(|f| f.nodes.iter())
            .any(|n| n.code.as_ref().is_some_and(|c| !c.is_empty()));
        assert!(any_code);
    }

    #[test]
    fn test_engine_reachable_reports_unresolved_and_witness_paths() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            test_graph(),
            Arc::new(MockReader),
        );

        let result = engine
            .reachable(ReachabilityRequest {
                from: vec!["sym/func1().".into(), "missing/from".into()],
                to: vec!["sym/var1.".into(), "missing/to".into()],
                policy: PolicyKind::Academic,
                witness_paths: true,
                max_paths: 1,
            })
            .unwrap();

        assert!(result.reachable);
        assert_eq!(result.hit_targets, vec!["sym/var1.".to_string()]);
        assert_eq!(result.unresolved_from, vec!["missing/from".to_string()]);
        assert_eq!(result.unresolved_to, vec!["missing/to".to_string()]);
        assert_eq!(
            result.witness_paths,
            vec![vec!["sym/func1().".to_string(), "sym/var1.".to_string()]]
        );
        assert_eq!(result.visited_node_count, 2);
    }

    fn make_method_core(
        id: u32,
        name: &str,
        scope: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> NodeCore {
        NodeCore::new(
            id,
            name.to_string(),
            Some(scope.to_string()),
            10,
            SourceSpan {
                start_line,
                start_column: 0,
                end_line,
                end_column: 0,
            },
            1.0,
            false,
            file_path.to_string(),
        )
    }

    fn class_anchor_graph() -> ContextGraph {
        use crate::domain::type_registry::{TypeDefAttribute, TypeInfo, TypeKind};

        let mut g = ContextGraph::new();

        // Register class in TypeRegistry
        g.type_registry.register(
            "pkg/Plugin#".to_string(),
            TypeInfo {
                definition: TypeDefAttribute {
                    type_kind: TypeKind::Class,
                    is_abstract: false,
                    type_param_count: 0,
                    type_var_info: None,
                },
                context_size: 5,
                surface_fragment: crate::domain::node::ContextFragment::new(
                    "pkg/Plugin#:surface".to_string(),
                    None,
                    None,
                    5,
                ),
                doc_score: 0.5,
                documentation: Vec::new(),
            },
        );

        // Public methods
        let run = Node::Function(FunctionNode {
            core: make_method_core(0, "run", "pkg/Plugin#", "plugin.py", 2, 5),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });
        let render = Node::Function(FunctionNode {
            core: make_method_core(1, "render", "pkg/Plugin#", "plugin.py", 6, 10),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });

        // Dunder method
        let call = Node::Function(FunctionNode {
            core: make_method_core(2, "__call__", "pkg/Plugin#", "plugin.py", 11, 15),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });

        // Private helper (should be excluded from expansion)
        let helper = Node::Function(FunctionNode {
            core: make_method_core(3, "_helper", "pkg/Plugin#", "plugin.py", 16, 20),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Private,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });

        // External dependency
        let ext_func = Node::Function(FunctionNode {
            core: make_core(4, "ext_func", "lib.py", 0, 5),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });

        let i_run = g.add_node("pkg/Plugin#run().".into(), run);
        let i_render = g.add_node("pkg/Plugin#render().".into(), render);
        let _i_call = g.add_node("pkg/Plugin#__call__().".into(), call);
        let i_helper = g.add_node("pkg/Plugin#_helper().".into(), helper);
        let i_ext = g.add_node("lib/ext_func().".into(), ext_func);

        g.add_edge(i_run, i_helper, EdgeKind::Call);
        g.add_edge(i_run, i_ext, EdgeKind::Call);
        g.add_edge(i_render, i_ext, EdgeKind::Call);

        g
    }

    #[test]
    fn test_class_anchor_expands_to_public_and_dunder_methods() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            class_anchor_graph(),
            Arc::new(MockReader),
        );

        let res = engine
            .compute(ComputeRequest {
                symbols: vec!["pkg/Plugin#".into()],
                policy: PolicyKind::Academic,
                max_tokens: None,
            })
            .unwrap();

        let resolutions = res.anchor_resolutions.unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].resolved_kind, "class");
        let expanded = resolutions[0].expanded_to.as_ref().unwrap();
        assert!(expanded.contains(&"pkg/Plugin#__call__().".to_string()));
        assert!(expanded.contains(&"pkg/Plugin#render().".to_string()));
        assert!(expanded.contains(&"pkg/Plugin#run().".to_string()));
        // _helper should NOT be in the expansion
        assert!(!expanded.contains(&"pkg/Plugin#_helper().".to_string()));

        // CF should cover run, render, __call__ + their transitive deps (_helper via run, ext_func)
        assert!(res.total_context_size > 0);
        assert!(res.reachable_node_count >= 3); // at least the 3 expanded methods
    }

    #[test]
    fn test_variable_anchor_compute() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            test_graph(),
            Arc::new(MockReader),
        );

        let res = engine
            .compute(ComputeRequest {
                symbols: vec!["sym/var1.".into()],
                policy: PolicyKind::Academic,
                max_tokens: None,
            })
            .unwrap();

        let resolutions = res.anchor_resolutions.unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].resolved_kind, "module_variable");
        assert!(resolutions[0].expanded_to.is_none());
        assert!(resolutions[0].unresolved_reason.is_none());
        assert!(res.total_context_size > 0);
    }

    #[test]
    fn test_mixed_anchor_compute() {
        let mut g = class_anchor_graph();

        let standalone_func = Node::Function(FunctionNode {
            core: make_core(10, "standalone", "main.py", 0, 5),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });
        g.add_node("pkg/standalone().".into(), standalone_func);

        let var = Node::Variable(VariableNode {
            core: make_core(11, "CONFIG", "main.py", 6, 6),
            var_type: None,
            mutability: Mutability::Const,
            variable_kind: VariableKind::Global,
        });
        g.add_node("pkg/CONFIG.".into(), var);

        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            g,
            Arc::new(MockReader),
        );

        let res = engine
            .compute(ComputeRequest {
                symbols: vec![
                    "pkg/Plugin#".into(),
                    "pkg/standalone().".into(),
                    "pkg/CONFIG.".into(),
                ],
                policy: PolicyKind::Academic,
                max_tokens: None,
            })
            .unwrap();

        let resolutions = res.anchor_resolutions.unwrap();
        assert_eq!(resolutions.len(), 3);
        assert_eq!(resolutions[0].resolved_kind, "class");
        assert!(resolutions[0].expanded_to.is_some());
        assert_eq!(resolutions[1].resolved_kind, "function");
        assert!(resolutions[1].expanded_to.is_none());
        assert_eq!(resolutions[2].resolved_kind, "constant");
        assert!(resolutions[2].expanded_to.is_none());

        // Starting symbols should include expanded class members + function + variable
        assert!(res.starting_symbols.len() >= 5);
    }

    #[test]
    fn test_compute_unresolved_anchor_fails() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            test_graph(),
            Arc::new(MockReader),
        );

        let err = engine
            .compute(ComputeRequest {
                symbols: vec!["nonexistent/symbol".into()],
                policy: PolicyKind::Academic,
                max_tokens: None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_search_returns_class_symbols() {
        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            class_anchor_graph(),
            Arc::new(MockReader),
        );

        let result = engine
            .search("Plugin", false, None, true, PolicyKind::Academic)
            .unwrap();

        let class_items: Vec<_> = result
            .items
            .iter()
            .filter(|item| item.node_type == "class")
            .collect();
        assert!(
            !class_items.is_empty(),
            "Search should return class symbols from TypeRegistry"
        );
        assert_eq!(class_items[0].symbol, "pkg/Plugin#");
    }

    #[test]
    fn test_search_shows_detailed_variable_kinds() {
        let mut g = ContextGraph::new();

        let const_var = Node::Variable(VariableNode {
            core: make_core(0, "MAX_SIZE", "config.py", 0, 0),
            var_type: None,
            mutability: Mutability::Const,
            variable_kind: VariableKind::Global,
        });
        g.add_node("pkg/MAX_SIZE.".into(), const_var);

        let mod_var = Node::Variable(VariableNode {
            core: make_core(1, "registry", "config.py", 1, 1),
            var_type: None,
            mutability: Mutability::Mutable,
            variable_kind: VariableKind::Global,
        });
        g.add_node("pkg/registry.".into(), mod_var);

        let engine = ContextEngine::from_prebuilt(
            PathBuf::from("semantic_data.json"),
            PathBuf::from("/repo"),
            g,
            Arc::new(MockReader),
        );

        let result = engine
            .search("pkg", false, None, true, PolicyKind::Academic)
            .unwrap();

        let kinds: Vec<_> = result.items.iter().map(|i| i.node_type.as_str()).collect();
        assert!(kinds.contains(&"constant"));
        assert!(kinds.contains(&"module_variable"));
    }

    #[test]
    fn test_class_expansion_uses_visibility_not_name() {
        use crate::domain::type_registry::{TypeDefAttribute, TypeInfo, TypeKind};

        let mut g = ContextGraph::new();
        g.type_registry.register(
            "pkg/MyClass#".to_string(),
            TypeInfo {
                definition: TypeDefAttribute {
                    type_kind: TypeKind::Class,
                    is_abstract: false,
                    type_param_count: 0,
                    type_var_info: None,
                },
                context_size: 5,
                surface_fragment: crate::domain::node::ContextFragment::new(
                    "pkg/MyClass#:surface".to_string(),
                    None,
                    None,
                    5,
                ),
                doc_score: 0.5,
                documentation: Vec::new(),
            },
        );

        // _internal method marked Public by extractor (e.g. Python dunder convention)
        let internal_pub = Node::Function(FunctionNode {
            core: make_method_core(0, "_internal", "pkg/MyClass#", "a.py", 0, 2),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });
        g.add_node("pkg/MyClass#_internal().".into(), internal_pub);

        // public_helper marked Private by extractor
        let pub_name_priv = Node::Function(FunctionNode {
            core: make_method_core(1, "public_helper", "pkg/MyClass#", "a.py", 3, 5),
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Private,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        });
        g.add_node("pkg/MyClass#public_helper().".into(), pub_name_priv);

        let expanded = expand_class_anchor(&g, "pkg/MyClass#");
        assert!(expanded.contains(&"pkg/MyClass#_internal().".to_string()));
        assert!(!expanded.contains(&"pkg/MyClass#public_helper().".to_string()));
    }
}
