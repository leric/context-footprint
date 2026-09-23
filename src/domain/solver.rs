use crate::domain::edge::EdgeKind;
use crate::domain::graph::ContextGraph;
use crate::domain::node::{ContextFragment, ContextFragmentId, Mutability, Node, NodeId};
use crate::domain::policy::{
    BoundaryDecision, BoundaryPolicy, PruningDecision, PruningParams, ReasoningMode,
    ReasoningRelation, SyntacticBoundaryPolicy, evaluate_forward, should_explore_callers,
};
use petgraph::graph::NodeIndex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReasoningState {
    pub node_index: NodeIndex,
    pub direction: Direction,
    pub reasoning_mode: ReasoningMode,
}

#[derive(Debug, Clone)]
pub struct BoundaryStop {
    pub source: NodeId,
    pub target: NodeId,
    pub direction: Direction,
    pub relation: ReasoningRelation,
}

/// How the current node was reached (for edge-aware pruning and reverse exploration).
#[derive(Debug, Clone)]
enum ReachedVia {
    Start,
    Forward(EdgeKind),
    /// Reached by following incoming Call edges (call-in exploration).
    CallIn,
    /// Reached by following incoming Write edges (shared-state write exploration).
    SharedStateWrite,
}

/// Single step in BFS traversal: node plus the edge/decision that led to it.
#[derive(Debug, Clone)]
pub struct TraversalStep {
    pub node_id: NodeId,
    pub incoming_edge_kind: Option<EdgeKind>,
    pub decision: Option<PruningDecision>,
}

/// CF computation result
#[derive(Debug, Clone)]
pub struct CfResult {
    pub reachable_set: HashSet<NodeId>,
    pub reachable_nodes_ordered: Vec<NodeId>,
    pub reachable_nodes_by_layer: Vec<Vec<NodeId>>,
    /// Traversal steps in BFS order: for each node, the edge kind and decision that led to it (None for start nodes).
    pub traversal_steps: Vec<TraversalStep>,
    pub total_context_size: u32,
    pub cf_total: u32,
    pub cf_out: u32,
    pub cf_in: u32,
    pub cf_overlap: u32,
    pub out_fragments: Vec<ContextFragmentId>,
    pub in_fragments: Vec<ContextFragmentId>,
    pub boundary_stops: Vec<BoundaryStop>,
    pub unresolved_states: Vec<ReasoningState>,
    pub truncated: bool,
    pub boundary_policy_id: String,
    pub size_function_id: String,
    pub measurement_scope_id: String,
    pub graph_total_references: usize,
    pub graph_unresolved_calls: usize,
    pub graph_total_functions: usize,
    pub graph_functions_with_explicit_fragments: usize,
}

#[derive(Debug, Default)]
struct DirectionalTraversal {
    fragments: HashMap<ContextFragmentId, u32>,
    prepaid_fragments: HashMap<ContextFragmentId, u32>,
    visited_states: HashSet<ReasoningState>,
    nodes: Vec<NodeIndex>,
    seen_nodes: HashSet<NodeIndex>,
    boundary_stops: Vec<BoundaryStop>,
    unresolved_states: Vec<ReasoningState>,
    truncated: bool,
}

#[derive(Debug, Clone)]
pub struct ReachabilityOptions {
    pub witness_paths: bool,
    pub max_paths: usize,
}

impl Default for ReachabilityOptions {
    fn default() -> Self {
        Self {
            witness_paths: false,
            max_paths: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReachabilityResult {
    pub reachable: bool,
    pub hit_targets: Vec<NodeId>,
    pub visited_set: HashSet<NodeId>,
    pub visited_node_count: usize,
    pub witness_paths: Vec<Vec<NodeId>>,
}

#[derive(Debug, Clone)]
struct TraversalState {
    visited: HashSet<NodeIndex>,
    predecessors: HashMap<NodeIndex, NodeIndex>,
}

/// CF Solver - computes Context-Footprint for a given node.
///
/// Holds graph and pruning params (doc_threshold + mode).
pub struct CfSolver {
    graph: Arc<ContextGraph>,
    params: PruningParams,
    boundary_policy: Arc<dyn BoundaryPolicy>,
}

impl CfSolver {
    pub fn new(graph: Arc<ContextGraph>, params: PruningParams) -> Self {
        let boundary_policy = Arc::new(SyntacticBoundaryPolicy::new(params.clone()));
        Self {
            graph,
            params,
            boundary_policy,
        }
    }

    pub fn with_policy(
        graph: Arc<ContextGraph>,
        params: PruningParams,
        boundary_policy: Arc<dyn BoundaryPolicy>,
    ) -> Self {
        Self {
            graph,
            params,
            boundary_policy,
        }
    }

    /// Compute CF for a given set of starting nodes (full result with layers, etc.).
    pub fn compute_cf(&self, starts: &[NodeIndex], max_tokens: Option<u32>) -> CfResult {
        let graph = self.graph.as_ref();
        let out = self.traverse_reasoning(starts, Direction::Out, max_tokens, &HashMap::new());
        let incoming = self.traverse_reasoning(starts, Direction::In, max_tokens, &out.fragments);

        let mut ordered_indices = out.nodes.clone();
        let mut seen: HashSet<NodeIndex> = ordered_indices.iter().copied().collect();
        for index in &incoming.nodes {
            if seen.insert(*index) {
                ordered_indices.push(*index);
            }
        }

        let out_ids: HashSet<_> = out.fragments.keys().cloned().collect();
        let in_ids: HashSet<_> = incoming.fragments.keys().cloned().collect();
        let mut all_ids = out_ids.clone();
        all_ids.extend(in_ids.iter().cloned());

        let fragment_size = |id: &ContextFragmentId| {
            out.fragments
                .get(id)
                .or_else(|| incoming.fragments.get(id))
                .copied()
                .unwrap_or(0)
        };
        let cf_out = out.fragments.values().copied().sum();
        let cf_in = incoming.fragments.values().copied().sum();
        let cf_total = all_ids.iter().map(fragment_size).sum();
        let cf_overlap = out_ids.intersection(&in_ids).map(fragment_size).sum();

        let reachable_set = ordered_indices
            .iter()
            .map(|idx| graph.node(*idx).core().id)
            .collect();
        let reachable_nodes_ordered: Vec<_> = ordered_indices
            .iter()
            .map(|idx| graph.node(*idx).core().id)
            .collect();
        let traversal_steps = reachable_nodes_ordered
            .iter()
            .map(|node_id| TraversalStep {
                node_id: *node_id,
                incoming_edge_kind: None,
                decision: None,
            })
            .collect();
        let mut boundary_stops = out.boundary_stops;
        boundary_stops.extend(incoming.boundary_stops);
        let mut unresolved_states = out.unresolved_states;
        unresolved_states.extend(incoming.unresolved_states);

        CfResult {
            reachable_set,
            reachable_nodes_ordered: reachable_nodes_ordered.clone(),
            // Reasoning traversal is a graph of modes rather than a single BFS tree.
            // Keep the compatibility field as one complete layer; fragment sets and
            // boundary stops are the authoritative explanation.
            reachable_nodes_by_layer: vec![reachable_nodes_ordered],
            traversal_steps,
            total_context_size: cf_total,
            cf_total,
            cf_out,
            cf_in,
            cf_overlap,
            out_fragments: sorted_fragment_ids(out_ids),
            in_fragments: sorted_fragment_ids(in_ids),
            boundary_stops,
            unresolved_states,
            truncated: out.truncated || incoming.truncated,
            boundary_policy_id: self.boundary_policy.id(),
            size_function_id: graph.size_function_id.clone(),
            measurement_scope_id: graph.measurement_scope_id.clone(),
            graph_total_references: graph.coverage.total_references,
            graph_unresolved_calls: graph.coverage.unresolved_calls,
            graph_total_functions: graph.coverage.total_functions,
            graph_functions_with_explicit_fragments: graph
                .coverage
                .functions_with_explicit_fragments,
        }
    }

    fn traverse_reasoning(
        &self,
        starts: &[NodeIndex],
        direction: Direction,
        max_tokens: Option<u32>,
        prepaid_fragments: &HashMap<ContextFragmentId, u32>,
    ) -> DirectionalTraversal {
        let graph = self.graph.as_ref();
        let mut result = DirectionalTraversal {
            prepaid_fragments: prepaid_fragments.clone(),
            ..DirectionalTraversal::default()
        };
        let mut queue = VecDeque::new();
        let initial_mode = match direction {
            Direction::Out => ReasoningMode::NeedBehavior,
            Direction::In => ReasoningMode::NeedContractEvidence,
        };
        for &node_index in starts {
            queue.push_back(ReasoningState {
                node_index,
                direction,
                reasoning_mode: initial_mode,
            });
        }

        while let Some(state) = queue.pop_front() {
            if !result.visited_states.insert(state) {
                continue;
            }
            let node = graph.node(state.node_index);
            if result.seen_nodes.insert(state.node_index) {
                result.nodes.push(state.node_index);
            }
            if !add_fragment_with_limit(
                &mut result,
                &node.core().surface_fragment,
                max_tokens,
                state,
            ) {
                result.unresolved_states.extend(queue);
                return result;
            }
            for type_fragment in referenced_type_fragments(node, &graph.type_registry) {
                if !add_fragment_with_limit(&mut result, type_fragment, max_tokens, state) {
                    result.unresolved_states.extend(queue);
                    return result;
                }
            }
            if mode_requires_implementation(state.reasoning_mode)
                && let Some(fragment) = &node.core().implementation_fragment
                && !add_fragment_with_limit(&mut result, fragment, max_tokens, state)
            {
                result.unresolved_states.extend(queue);
                return result;
            }

            if node.core().is_external {
                continue;
            }

            match (state.direction, state.reasoning_mode) {
                (Direction::Out, ReasoningMode::NeedBehavior) => {
                    for (target_index, edge_kind) in graph.outgoing_edges(state.node_index) {
                        let (relation, next_mode) = match edge_kind {
                            EdgeKind::Call => {
                                (ReasoningRelation::Call, ReasoningMode::NeedBehavior)
                            }
                            EdgeKind::Read => {
                                (ReasoningRelation::Read, ReasoningMode::NeedValueProvenance)
                            }
                            EdgeKind::Write => {
                                (ReasoningRelation::Write, ReasoningMode::NeedSurface)
                            }
                            EdgeKind::Annotates => {
                                (ReasoningRelation::Annotates, ReasoningMode::NeedBehavior)
                            }
                            EdgeKind::OverriddenBy => (
                                ReasoningRelation::DynamicDispatch,
                                ReasoningMode::NeedBehavior,
                            ),
                        };
                        self.enqueue_target(
                            &mut result,
                            &mut queue,
                            state,
                            target_index,
                            relation,
                            next_mode,
                        );
                    }
                }
                (Direction::Out, ReasoningMode::NeedValueProvenance) => {
                    if let Node::Variable(variable) = node
                        && variable.mutability == Mutability::Mutable
                    {
                        for (writer_index, _) in
                            graph.incoming_edges(state.node_index, Some(EdgeKind::Write))
                        {
                            self.enqueue_target(
                                &mut result,
                                &mut queue,
                                state,
                                writer_index,
                                ReasoningRelation::ValueProvenance,
                                ReasoningMode::NeedBehavior,
                            );
                        }
                    }
                }
                (Direction::In, ReasoningMode::NeedContractEvidence) => {
                    let contract_is_leaky = !matches!(
                        self.boundary_policy.evaluate_contract(
                            ReasoningMode::NeedContractEvidence,
                            node,
                            &graph.type_registry,
                        ),
                        BoundaryDecision::StopAtSurface
                    );
                    if contract_is_leaky {
                        self.enqueue_callers(&mut result, &mut queue, state);
                        self.enqueue_override_obligations(&mut result, &mut queue, state);
                        self.enqueue_state_readers(&mut result, &mut queue, state);
                    } else {
                        self.enqueue_parent_contracts(&mut result, &mut queue, state, false);
                    }
                }
                (Direction::In, ReasoningMode::NeedUsageEvidence)
                | (Direction::In, ReasoningMode::NeedParentUsage) => {
                    let contract_is_leaky = !matches!(
                        self.boundary_policy.evaluate_contract(
                            ReasoningMode::NeedUsageEvidence,
                            node,
                            &graph.type_registry,
                        ),
                        BoundaryDecision::StopAtSurface
                    );
                    if contract_is_leaky {
                        self.enqueue_callers(&mut result, &mut queue, state);
                    }
                }
                _ => {}
            }
        }

        result
    }

    fn enqueue_target(
        &self,
        result: &mut DirectionalTraversal,
        queue: &mut VecDeque<ReasoningState>,
        source_state: ReasoningState,
        target_index: NodeIndex,
        relation: ReasoningRelation,
        enter_mode: ReasoningMode,
    ) {
        let graph = self.graph.as_ref();
        let source = graph.node(source_state.node_index);
        let target = graph.node(target_index);
        let decision = self.boundary_policy.evaluate(
            source_state.reasoning_mode,
            relation,
            source,
            target,
            &graph.type_registry,
        );
        let reasoning_mode = match decision {
            BoundaryDecision::StopAtSurface => {
                result.boundary_stops.push(BoundaryStop {
                    source: source.core().id,
                    target: target.core().id,
                    direction: source_state.direction,
                    relation,
                });
                if relation == ReasoningRelation::DynamicDispatch {
                    return;
                }
                ReasoningMode::NeedSurface
            }
            BoundaryDecision::EnterImplementation | BoundaryDecision::Unknown => enter_mode,
        };
        queue.push_back(ReasoningState {
            node_index: target_index,
            direction: source_state.direction,
            reasoning_mode,
        });
    }

    fn enqueue_callers(
        &self,
        result: &mut DirectionalTraversal,
        queue: &mut VecDeque<ReasoningState>,
        state: ReasoningState,
    ) {
        for (caller_index, _) in self
            .graph
            .incoming_edges(state.node_index, Some(EdgeKind::Call))
        {
            self.enqueue_target(
                result,
                queue,
                state,
                caller_index,
                ReasoningRelation::UsageEvidence,
                ReasoningMode::NeedUsageEvidence,
            );
        }
    }

    fn enqueue_parent_contracts(
        &self,
        result: &mut DirectionalTraversal,
        queue: &mut VecDeque<ReasoningState>,
        state: ReasoningState,
        continue_to_parent_usage: bool,
    ) {
        for (parent_index, _) in self
            .graph
            .incoming_edges(state.node_index, Some(EdgeKind::OverriddenBy))
        {
            self.enqueue_target(
                result,
                queue,
                state,
                parent_index,
                ReasoningRelation::ParentContract,
                if continue_to_parent_usage {
                    ReasoningMode::NeedParentUsage
                } else {
                    ReasoningMode::NeedSurface
                },
            );
            if continue_to_parent_usage {
                queue.push_back(ReasoningState {
                    node_index: parent_index,
                    direction: Direction::In,
                    reasoning_mode: ReasoningMode::NeedParentUsage,
                });
            }
        }
    }

    fn enqueue_override_obligations(
        &self,
        result: &mut DirectionalTraversal,
        queue: &mut VecDeque<ReasoningState>,
        state: ReasoningState,
    ) {
        self.enqueue_parent_contracts(result, queue, state, true);
        for (child_index, edge_kind) in self.graph.outgoing_edges(state.node_index) {
            if matches!(edge_kind, EdgeKind::OverriddenBy) {
                self.enqueue_target(
                    result,
                    queue,
                    state,
                    child_index,
                    ReasoningRelation::ChildImplementationEvidence,
                    ReasoningMode::NeedImplementation,
                );
            }
        }
    }

    fn enqueue_state_readers(
        &self,
        result: &mut DirectionalTraversal,
        queue: &mut VecDeque<ReasoningState>,
        state: ReasoningState,
    ) {
        for (variable_index, edge_kind) in self.graph.outgoing_edges(state.node_index) {
            if !matches!(edge_kind, EdgeKind::Write) {
                continue;
            }
            let Node::Variable(variable) = self.graph.node(variable_index) else {
                continue;
            };
            if variable.mutability != Mutability::Mutable {
                continue;
            }
            self.enqueue_target(
                result,
                queue,
                state,
                variable_index,
                ReasoningRelation::Write,
                ReasoningMode::NeedSurface,
            );
            for (reader_index, _) in self
                .graph
                .incoming_edges(variable_index, Some(EdgeKind::Read))
            {
                self.enqueue_target(
                    result,
                    queue,
                    state,
                    reader_index,
                    ReasoningRelation::ObservedBy,
                    ReasoningMode::NeedImplementation,
                );
            }
        }
    }

    pub fn reachable(
        &self,
        starts: &[NodeIndex],
        targets: &[NodeIndex],
        options: ReachabilityOptions,
    ) -> ReachabilityResult {
        let graph = self.graph.as_ref();
        let traversal = self.traverse(starts, None);
        let visited_set: HashSet<NodeId> = traversal
            .visited
            .iter()
            .map(|idx| graph.node(*idx).core().id)
            .collect();

        let mut hit_targets = Vec::new();
        let mut witness_paths = Vec::new();
        let mut seen_hits = HashSet::new();

        for &target in targets {
            if !traversal.visited.contains(&target) {
                continue;
            }

            let target_id = graph.node(target).core().id;
            if seen_hits.insert(target_id) {
                hit_targets.push(target_id);
                if options.witness_paths && witness_paths.len() < options.max_paths {
                    witness_paths.push(self.reconstruct_path(target, &traversal.predecessors));
                }
            }
        }

        ReachabilityResult {
            reachable: !hit_targets.is_empty(),
            hit_targets,
            visited_node_count: traversal.visited.len(),
            visited_set,
            witness_paths,
        }
    }

    /// Compute CF total context size for a single start node.
    /// Does not return traversal order / layers; ignores max_tokens.
    pub fn compute_cf_total(&self, start: NodeIndex) -> u32 {
        self.compute_cf(&[start], None).cf_total
    }

    fn traverse(&self, starts: &[NodeIndex], max_tokens: Option<u32>) -> TraversalState {
        let graph = self.graph.as_ref();
        let params = &self.params;
        let mut idx_to_symbol: HashMap<NodeIndex, &str> =
            HashMap::with_capacity(graph.symbol_to_node.len());
        for (sym, &idx) in &graph.symbol_to_node {
            idx_to_symbol.insert(idx, sym.as_str());
        }

        let start_set: HashSet<NodeIndex> = starts.iter().copied().collect();
        let mut visited = HashSet::new();
        let mut predecessors = HashMap::new();
        let mut queue: VecDeque<(NodeIndex, ReachedVia)> = VecDeque::new();
        let mut total_size = 0;

        for &start in starts {
            queue.push_back((start, ReachedVia::Start));
        }

        while let Some((current, reached_via)) = queue.pop_front() {
            let current_node = graph.node(current);

            if !visited.insert(current) {
                continue;
            }

            total_size += current_node.core().context_size;

            if let Some(limit) = max_tokens
                && total_size >= limit
            {
                break;
            }

            if matches!(
                reached_via,
                ReachedVia::CallIn | ReachedVia::SharedStateWrite
            ) {
                continue;
            }

            let mut out_edges: Vec<_> = graph.outgoing_edges(current).collect();
            out_edges.sort_by(|(a_idx, _), (b_idx, _)| {
                let a_sym = idx_to_symbol.get(a_idx).copied().unwrap_or("");
                let b_sym = idx_to_symbol.get(b_idx).copied().unwrap_or("");
                a_sym.cmp(b_sym)
            });

            for (neighbor, edge_kind) in out_edges {
                let neighbor_node = graph.node(neighbor);
                let decision =
                    evaluate_forward(params, current_node, neighbor_node, edge_kind, graph);

                if matches!(decision, PruningDecision::Transparent) {
                    if !start_set.contains(&neighbor) {
                        predecessors.entry(neighbor).or_insert(current);
                    }
                    queue.push_back((neighbor, ReachedVia::Forward(edge_kind.clone())));
                } else if !visited.contains(&neighbor) {
                    let boundary_size = neighbor_node.core().context_size;
                    if let Some(limit) = max_tokens
                        && total_size + boundary_size > limit
                    {
                        break;
                    }

                    if !start_set.contains(&neighbor) {
                        predecessors.entry(neighbor).or_insert(current);
                    }
                    if visited.insert(neighbor) {
                        total_size += boundary_size;
                    }
                }
            }

            if let Node::Function(f) = current_node {
                let incoming_edge = match &reached_via {
                    ReachedVia::Forward(ek) => Some(ek),
                    _ => None,
                };
                if should_explore_callers(f, current, incoming_edge, params, graph) {
                    let mut callers: Vec<_> = graph
                        .incoming_edges(current, Some(EdgeKind::Call))
                        .collect();
                    callers.sort_by(|(a_idx, _), (b_idx, _)| {
                        let a_sym = idx_to_symbol.get(a_idx).copied().unwrap_or("");
                        let b_sym = idx_to_symbol.get(b_idx).copied().unwrap_or("");
                        a_sym.cmp(b_sym)
                    });

                    for (caller_idx, _) in callers {
                        if !visited.contains(&caller_idx) {
                            if !start_set.contains(&caller_idx) {
                                predecessors.entry(caller_idx).or_insert(current);
                            }
                            queue.push_back((caller_idx, ReachedVia::CallIn));
                        }
                    }
                }
            }

            if let Node::Variable(v) = current_node
                && v.mutability == crate::domain::node::Mutability::Mutable
                && matches!(reached_via, ReachedVia::Forward(EdgeKind::Read))
            {
                let mut writers: Vec<_> = graph
                    .incoming_edges(current, Some(EdgeKind::Write))
                    .collect();
                writers.sort_by(|(a_idx, _), (b_idx, _)| {
                    let a_sym = idx_to_symbol.get(a_idx).copied().unwrap_or("");
                    let b_sym = idx_to_symbol.get(b_idx).copied().unwrap_or("");
                    a_sym.cmp(b_sym)
                });

                for (writer_idx, _) in writers {
                    if !visited.contains(&writer_idx) {
                        if !start_set.contains(&writer_idx) {
                            predecessors.entry(writer_idx).or_insert(current);
                        }
                        queue.push_back((writer_idx, ReachedVia::SharedStateWrite));
                    }
                }
            }

            if let Some(limit) = max_tokens
                && total_size >= limit
            {
                break;
            }
        }

        TraversalState {
            visited,
            predecessors,
        }
    }

    fn reconstruct_path(
        &self,
        target: NodeIndex,
        predecessors: &HashMap<NodeIndex, NodeIndex>,
    ) -> Vec<NodeId> {
        let graph = self.graph.as_ref();
        let mut path = vec![graph.node(target).core().id];
        let mut cursor = target;
        let mut seen = HashSet::new();

        while let Some(&parent) = predecessors.get(&cursor) {
            if !seen.insert(cursor) {
                break;
            }
            path.push(graph.node(parent).core().id);
            cursor = parent;
        }

        path.reverse();
        path
    }
}

fn mode_requires_implementation(mode: ReasoningMode) -> bool {
    matches!(
        mode,
        ReasoningMode::NeedBehavior
            | ReasoningMode::NeedValueProvenance
            | ReasoningMode::NeedUsageEvidence
            | ReasoningMode::NeedContractEvidence
            | ReasoningMode::NeedImplementation
    )
}

fn add_fragment_with_limit(
    traversal: &mut DirectionalTraversal,
    fragment: &ContextFragment,
    max_tokens: Option<u32>,
    state: ReasoningState,
) -> bool {
    if traversal.fragments.contains_key(&fragment.id) {
        return true;
    }
    let current: u32 = traversal
        .prepaid_fragments
        .values()
        .copied()
        .sum::<u32>()
        .saturating_add(
            traversal
                .fragments
                .iter()
                .filter(|(id, _)| !traversal.prepaid_fragments.contains_key(*id))
                .map(|(_, size)| *size)
                .sum(),
        );
    if traversal.prepaid_fragments.contains_key(&fragment.id) {
        traversal
            .fragments
            .insert(fragment.id.clone(), fragment.context_size);
        return true;
    }
    if max_tokens.is_some_and(|limit| current.saturating_add(fragment.context_size) > limit) {
        traversal.truncated = true;
        traversal.unresolved_states.push(state);
        return false;
    }
    traversal
        .fragments
        .insert(fragment.id.clone(), fragment.context_size);
    true
}

fn sorted_fragment_ids(ids: HashSet<ContextFragmentId>) -> Vec<ContextFragmentId> {
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort();
    ids
}

fn referenced_type_fragments<'a>(
    node: &'a Node,
    registry: &'a crate::domain::type_registry::TypeRegistry,
) -> Vec<&'a ContextFragment> {
    let type_ids: Vec<&str> = match node {
        Node::Function(function) => function
            .parameters
            .iter()
            .filter_map(|parameter| parameter.param_type.as_deref())
            .chain(function.return_types.iter().map(String::as_str))
            .collect(),
        Node::Variable(variable) => variable.var_type.as_deref().into_iter().collect(),
    };
    let mut seen = HashSet::new();
    type_ids
        .into_iter()
        .filter(|type_id| seen.insert(*type_id))
        .filter_map(|type_id| registry.get(type_id))
        .map(|type_info| &type_info.surface_fragment)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::edge::EdgeKind;
    use crate::domain::graph::ContextGraph;
    use crate::domain::node::{
        ContextFragment, FunctionNode, Node, NodeCore, SourceSpan, Visibility,
    };
    use crate::domain::policy::PruningParams;
    use std::sync::Arc;

    fn test_node(id: u32, name: &str, context_size: u32) -> Node {
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 1,
            end_column: 10,
        };
        let core = NodeCore::new(
            id,
            name.to_string(),
            None,
            context_size,
            span,
            0.5,
            false,
            "test.py".to_string(),
        );
        Node::Function(FunctionNode {
            core,
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        })
    }

    fn test_var_node(id: u32, name: &str, mutability: crate::domain::node::Mutability) -> Node {
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 1,
            end_column: 10,
        };
        let core = NodeCore::new(
            id,
            name.to_string(),
            None,
            1,
            span,
            0.5,
            false,
            "test.py".to_string(),
        );
        Node::Variable(crate::domain::node::VariableNode {
            core,
            var_type: None,
            mutability,
            variable_kind: crate::domain::node::VariableKind::Global,
        })
    }

    fn fragment_node(
        id: u32,
        name: &str,
        surface_size: u32,
        implementation_size: u32,
        reliable_contract: bool,
    ) -> Node {
        let span = SourceSpan {
            start_line: id,
            start_column: 0,
            end_line: id + 1,
            end_column: 0,
        };
        let core = NodeCore::with_fragments(
            id,
            name.to_string(),
            None,
            ContextFragment::new(
                format!("{name}:surface"),
                Some(span.clone()),
                None,
                surface_size,
            ),
            Some(ContextFragment::new(
                format!("{name}:implementation"),
                Some(span.clone()),
                None,
                implementation_size,
            )),
            span,
            if reliable_contract { 1.0 } else { 0.0 },
            Vec::new(),
            false,
            "test.py".to_string(),
        );
        Node::Function(FunctionNode {
            core,
            parameters: Vec::new(),
            is_async: false,
            is_generator: false,
            visibility: Visibility::Public,
            return_types: if reliable_contract {
                vec!["int".to_string()]
            } else {
                Vec::new()
            },
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        })
    }

    /// Node that qualifies as boundary under academic(0.5): sig complete + doc_score >= 0.5.
    fn test_node_boundary(id: u32, name: &str, context_size: u32) -> Node {
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 1,
            end_column: 10,
        };
        let core = NodeCore::new(
            id,
            name.to_string(),
            None,
            context_size,
            span,
            0.8,
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
    fn test_single_node_cf() {
        let mut graph = ContextGraph::new();
        let idx = graph.add_node("sym::a".into(), test_node(0, "a", 100));
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[idx], None);
        assert_eq!(result.reachable_set.len(), 1);
        assert!(result.reachable_set.contains(&0));
        assert_eq!(result.total_context_size, 100);
    }

    #[test]
    fn test_linear_dependency_chain() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 3);
        assert_eq!(result.total_context_size, 10 + 20 + 30);
    }

    #[test]
    fn test_diamond_dependency() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        let d = graph.add_node("sym::d".into(), test_node(3, "d", 40));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(a, c, EdgeKind::Call);
        graph.add_edge(b, d, EdgeKind::Call);
        graph.add_edge(c, d, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 4);
        assert_eq!(result.total_context_size, 10 + 20 + 30 + 40);
    }

    #[test]
    fn test_cycle_detection() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        graph.add_edge(c, a, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 3);
        assert_eq!(result.total_context_size, 10 + 20 + 30);
    }

    #[test]
    fn test_boundary_stops_traversal() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node_boundary(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 2); // a and b; c is not traversed
        assert_eq!(result.total_context_size, 10 + 20); // a and b both count
    }

    #[test]
    fn test_transparent_node_continues() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 3);
        assert_eq!(result.total_context_size, 60);
    }

    #[test]
    fn test_reachable_respects_boundary_pruning() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node_boundary(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5));
        let reachable_b = solver.reachable(&[a], &[b], ReachabilityOptions::default());
        let reachable_c = solver.reachable(&[a], &[c], ReachabilityOptions::default());

        assert!(reachable_b.reachable);
        assert!(!reachable_c.reachable);
    }

    #[test]
    fn test_reachable_supports_reverse_call_in_and_witness_paths() {
        let mut graph = ContextGraph::new();
        let caller = graph.add_node("sym::caller".into(), test_node(0, "caller", 10));
        let callee = graph.add_node("sym::callee".into(), test_node(1, "callee", 20));
        let state = graph.add_node(
            "sym::state".into(),
            test_var_node(2, "state", crate::domain::node::Mutability::Mutable),
        );

        graph.add_edge(caller, callee, EdgeKind::Call);
        graph.add_edge(callee, state, EdgeKind::Write);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.reachable(
            &[callee],
            &[caller],
            ReachabilityOptions {
                witness_paths: true,
                max_paths: 1,
            },
        );

        assert!(result.reachable);
        assert_eq!(result.hit_targets, vec![0]);
        assert_eq!(result.witness_paths, vec![vec![1, 0]]);
    }

    #[test]
    fn test_reachable_uses_union_of_multiple_starts_and_targets() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 10));
        let x = graph.add_node("sym::x".into(), test_node(2, "x", 10));
        let y = graph.add_node("sym::y".into(), test_node(3, "y", 10));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(x, y, EdgeKind::Call);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.reachable(
            &[a, x],
            &[b, y],
            ReachabilityOptions {
                witness_paths: true,
                max_paths: 2,
            },
        );

        assert!(result.reachable);
        assert_eq!(result.hit_targets, vec![1, 3]);
        assert_eq!(result.witness_paths, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn test_shared_state_write_expansion() {
        // Reader R reads mutable var V; W1 and W2 write to V. Reverse exploration from V follows incoming Write to W1, W2.
        let mut graph = ContextGraph::new();
        let r = graph.add_node("sym::r".into(), test_node(0, "r", 10));
        let v_span = crate::domain::node::SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 1,
            end_column: 5,
        };
        let v_core = crate::domain::node::NodeCore::new(
            1,
            "v".to_string(),
            None,
            1,
            v_span,
            0.0,
            false,
            "test.py".to_string(),
        );
        let var = Node::Variable(crate::domain::node::VariableNode {
            core: v_core,
            var_type: Some("int#".to_string()),
            mutability: crate::domain::node::Mutability::Mutable,
            variable_kind: crate::domain::node::VariableKind::Global,
        });
        let var_idx = graph.add_node("sym::v".into(), var);
        let w1 = graph.add_node("sym::w1".into(), test_node(2, "w1", 20));
        let w2 = graph.add_node("sym::w2".into(), test_node(3, "w2", 30));
        graph.add_edge(r, var_idx, EdgeKind::Read);
        graph.add_edge(w1, var_idx, EdgeKind::Write);
        graph.add_edge(w2, var_idx, EdgeKind::Write);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[r], None);
        assert_eq!(result.reachable_set.len(), 4); // r, v, w1, w2
        assert_eq!(result.total_context_size, 10 + 1 + 20 + 30);
    }

    #[test]
    fn test_call_in_expansion() {
        // Caller --Call--> Callee. Start at Callee; call-in exploration follows incoming Call to Caller.
        let mut graph = ContextGraph::new();
        let callee = graph.add_node("sym::callee".into(), test_node(0, "callee", 10));
        let caller = graph.add_node("sym::caller".into(), test_node(1, "caller", 25));
        let var = graph.add_node(
            "sym::var".into(),
            test_var_node(2, "var", crate::domain::node::Mutability::Mutable),
        );
        graph.add_edge(caller, callee, EdgeKind::Call);
        // Make callee impure so call-in happens
        graph.add_edge(callee, var, EdgeKind::Write);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[callee], None);
        assert_eq!(result.reachable_set.len(), 3); // callee, var (forward), caller (call-in)
        assert_eq!(result.total_context_size, 10 + 25 + 1);
    }

    #[test]
    fn test_different_policies_different_results() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node_boundary(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        let graph_arc = Arc::new(graph);
        let solver_trans = CfSolver::new(Arc::clone(&graph_arc), PruningParams::strict(0.5));
        let solver_bound = CfSolver::new(graph_arc, PruningParams::academic(0.5));
        let res_trans = solver_trans.compute_cf(&[a], None);
        let res_bound = solver_bound.compute_cf(&[a], None);
        assert_eq!(res_trans.total_context_size, 60);
        assert_eq!(res_bound.total_context_size, 30); // a and b, c not traversed
        assert_eq!(res_trans.reachable_set.len(), 3);
        assert_eq!(res_bound.reachable_set.len(), 2);
    }

    #[test]
    fn test_disconnected_component_not_reached() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let _b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        graph.add_edge(a, a, EdgeKind::Call); // self-loop only, b is disconnected
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 1);
        assert_eq!(result.total_context_size, 10);
    }

    #[test]
    fn test_multiple_edges_from_same_source() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(a, c, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[a], None);
        assert_eq!(result.reachable_set.len(), 3);
        assert_eq!(result.total_context_size, 60);
    }

    #[test]
    fn test_start_at_middle_of_chain() {
        // A -> B -> C. Start at B. B has incomplete spec so call-in exploration reaches A.
        // B's context_size (250) is above LEAF_UTILITY_SIZE_THRESHOLD so call-in is explored.
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 250));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        let var = graph.add_node(
            "sym::var".into(),
            test_var_node(3, "var", crate::domain::node::Mutability::Mutable),
        );
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        // Make b impure so call-in happens
        graph.add_edge(b, var, EdgeKind::Write);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));
        let result = solver.compute_cf(&[b], None);
        assert_eq!(result.reachable_set.len(), 4); // B, then C and var (forward), A (call-in)
        assert_eq!(result.total_context_size, 10 + 250 + 30 + 1);
    }

    #[test]
    fn test_boundary_node_still_in_reachable_set() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node_boundary(1, "b", 99));
        graph.add_edge(a, b, EdgeKind::Call);
        let solver = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5));
        let result = solver.compute_cf(&[a], None);
        assert!(result.reachable_set.contains(&0));
        assert!(result.reachable_set.contains(&1));
        assert_eq!(result.total_context_size, 10 + 99);
    }

    #[test]
    fn test_multi_node_union_cf() {
        let mut graph = ContextGraph::new();
        // A -> C, B -> C, C -> D. E is independent.
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        let d = graph.add_node("sym::d".into(), test_node(3, "d", 40));
        let _e = graph.add_node("sym::e".into(), test_node(4, "e", 50));

        graph.add_edge(a, c, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        graph.add_edge(c, d, EdgeKind::Call);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::strict(0.5));

        // CF(A) = {A, C, D} = 10 + 30 + 40 = 80
        // CF(B) = {B, C, D} = 20 + 30 + 40 = 90
        // CF({A, B}) = {A, B, C, D} = 10 + 20 + 30 + 40 = 100
        let result = solver.compute_cf(&[a, b], None);

        assert_eq!(result.reachable_set.len(), 4);
        assert!(result.reachable_set.contains(&0)); // A
        assert!(result.reachable_set.contains(&1)); // B
        assert!(result.reachable_set.contains(&2)); // C
        assert!(result.reachable_set.contains(&3)); // D
        assert!(!result.reachable_set.contains(&4)); // E (not reachable)
        assert_eq!(result.total_context_size, 100);

        // Mode-aware reasoning does not have a unique BFS depth, so the
        // compatibility layer contains the complete reached-node union.
        assert_eq!(result.reachable_nodes_by_layer.len(), 1);
        assert_eq!(result.reachable_nodes_by_layer[0].len(), 4);
        assert!(result.reachable_nodes_by_layer[0].contains(&0));
        assert!(result.reachable_nodes_by_layer[0].contains(&1));
        assert!(result.reachable_nodes_by_layer[0].contains(&2));
        assert!(result.reachable_nodes_by_layer[0].contains(&3));
    }

    #[test]
    fn test_cached_total_matches_compute_cf_for_each_node() {
        let mut graph = ContextGraph::new();
        // A -> B -> C, and A -> D (diamond-ish), plus a cycle E <-> F.
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        let d = graph.add_node("sym::d".into(), test_node(3, "d", 40));
        let e = graph.add_node("sym::e".into(), test_node(4, "e", 5));
        let f = graph.add_node("sym::f".into(), test_node(5, "f", 6));

        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);
        graph.add_edge(a, d, EdgeKind::Call);
        graph.add_edge(c, d, EdgeKind::Call);
        graph.add_edge(e, f, EdgeKind::Call);
        graph.add_edge(f, e, EdgeKind::Call);

        let graph_arc = Arc::new(graph);
        let solver = CfSolver::new(Arc::clone(&graph_arc), PruningParams::strict(0.5));

        for idx in graph_arc.graph.node_indices() {
            let expected = solver.compute_cf(&[idx], None).total_context_size;
            let got = solver.compute_cf_total(idx);
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn test_cached_total_respects_boundary_semantics() {
        let mut graph = ContextGraph::new();
        // A -> B -> C; B is boundary (sig complete + doc), so from A only A and B count.
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node_boundary(1, "b", 20));
        let c = graph.add_node("sym::c".into(), test_node(2, "c", 30));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(b, c, EdgeKind::Call);

        let solver = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5));

        let expected = solver.compute_cf(&[a], None).total_context_size;
        let got = solver.compute_cf_total(a);
        assert_eq!(got, expected);
        assert_eq!(got, 10 + 20);
    }

    #[test]
    fn reliable_callee_charges_surface_only() {
        let mut graph = ContextGraph::new();
        let target = graph.add_node("target".into(), fragment_node(0, "target", 2, 8, false));
        let callee = graph.add_node("callee".into(), fragment_node(1, "callee", 3, 30, true));
        graph.add_edge(target, callee, EdgeKind::Call);

        let result = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5))
            .compute_cf(&[target], None);

        assert_eq!(result.cf_out, 13);
        assert_eq!(result.cf_in, 10);
        assert_eq!(result.cf_overlap, 10);
        assert_eq!(result.cf_total, 13);
        assert!(result.out_fragments.contains(&"callee:surface".to_string()));
        assert!(
            !result
                .out_fragments
                .contains(&"callee:implementation".to_string())
        );
    }

    #[test]
    fn usage_mode_follows_caller_chain_without_callees() {
        let mut graph = ContextGraph::new();
        let target = graph.add_node("target".into(), fragment_node(0, "target", 1, 1, false));
        let caller = graph.add_node("caller".into(), fragment_node(1, "caller", 2, 3, false));
        let root = graph.add_node("root".into(), fragment_node(2, "root", 4, 5, false));
        let unrelated = graph.add_node(
            "unrelated".into(),
            fragment_node(3, "unrelated", 6, 7, false),
        );
        graph.add_edge(caller, target, EdgeKind::Call);
        graph.add_edge(root, caller, EdgeKind::Call);
        graph.add_edge(caller, unrelated, EdgeKind::Call);

        let result = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5))
            .compute_cf(&[target], None);

        assert_eq!(result.cf_in, 16);
        assert!(
            result
                .in_fragments
                .contains(&"root:implementation".to_string())
        );
        assert!(
            !result
                .in_fragments
                .contains(&"unrelated:surface".to_string())
        );
    }

    #[test]
    fn mutable_state_writer_continues_in_behavior_mode() {
        let mut graph = ContextGraph::new();
        let reader = graph.add_node("reader".into(), fragment_node(0, "reader", 1, 1, false));
        let state = graph.add_node(
            "state".into(),
            test_var_node(1, "state", crate::domain::node::Mutability::Mutable),
        );
        let writer = graph.add_node("writer".into(), fragment_node(2, "writer", 2, 2, false));
        let dependency = graph.add_node(
            "dependency".into(),
            fragment_node(3, "dependency", 3, 5, false),
        );
        graph.add_edge(reader, state, EdgeKind::Read);
        graph.add_edge(writer, state, EdgeKind::Write);
        graph.add_edge(writer, dependency, EdgeKind::Call);

        let result = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5))
            .compute_cf(&[reader], None);

        assert!(
            result
                .out_fragments
                .contains(&"dependency:implementation".to_string())
        );
    }

    #[test]
    fn mutable_state_readers_are_writer_obligations() {
        let mut graph = ContextGraph::new();
        let writer = graph.add_node("writer".into(), fragment_node(0, "writer", 2, 2, false));
        let state = graph.add_node(
            "state".into(),
            test_var_node(1, "state", crate::domain::node::Mutability::Mutable),
        );
        let reader = graph.add_node("reader".into(), fragment_node(2, "reader", 2, 3, false));
        graph.add_edge(writer, state, EdgeKind::Write);
        graph.add_edge(reader, state, EdgeKind::Read);

        let result = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5))
            .compute_cf(&[writer], None);

        assert!(
            result
                .in_fragments
                .contains(&"reader:implementation".to_string())
        );
        assert_eq!(result.cf_total, 10);
    }

    #[test]
    fn child_override_always_includes_parent_surface() {
        let mut graph = ContextGraph::new();
        let parent = graph.add_node("parent".into(), fragment_node(0, "parent", 3, 30, true));
        let child = graph.add_node("child".into(), fragment_node(1, "child", 2, 2, false));
        graph.add_edge(parent, child, EdgeKind::OverriddenBy);

        let result =
            CfSolver::new(Arc::new(graph), PruningParams::academic(0.5)).compute_cf(&[child], None);

        assert!(result.in_fragments.contains(&"parent:surface".to_string()));
        assert!(
            !result
                .in_fragments
                .contains(&"parent:implementation".to_string())
        );
    }

    #[test]
    fn truncation_reports_lower_bound_and_unresolved_state() {
        let mut graph = ContextGraph::new();
        let target = graph.add_node("target".into(), fragment_node(0, "target", 2, 8, false));

        let result = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5))
            .compute_cf(&[target], Some(5));

        assert!(result.truncated);
        assert_eq!(result.cf_total, 2);
        assert!(!result.unresolved_states.is_empty());
    }

    #[test]
    fn truncation_budget_is_shared_across_in_and_out() {
        let mut graph = ContextGraph::new();
        let target = graph.add_node("target".into(), fragment_node(0, "target", 1, 1, false));
        let dependency = graph.add_node(
            "dependency".into(),
            fragment_node(1, "dependency", 1, 1, false),
        );
        let caller = graph.add_node("caller".into(), fragment_node(2, "caller", 1, 1, false));
        graph.add_edge(target, dependency, EdgeKind::Call);
        graph.add_edge(caller, target, EdgeKind::Call);

        let result = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5))
            .compute_cf(&[target], Some(5));

        assert!(result.truncated);
        assert_eq!(result.cf_total, 5);
        assert!(result.cf_total <= 5);
    }
}
