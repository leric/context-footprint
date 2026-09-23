use crate::domain::edge::EdgeKind;
use crate::domain::node::Node;
use crate::domain::type_registry::TypeRegistry;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

/// Symbol identifier (globally unique symbol string)
pub type SymbolId = String;

#[derive(Debug, Clone, Default)]
pub struct GraphCoverage {
    pub total_references: usize,
    pub unresolved_calls: usize,
    pub total_functions: usize,
    pub functions_with_explicit_fragments: usize,
}

/// Context Graph - the core data structure
pub struct ContextGraph {
    /// The directed graph of nodes and edges
    pub graph: DiGraph<Node, EdgeKind>,

    /// Mapping from symbol to node index
    pub symbol_to_node: HashMap<SymbolId, NodeIndex>,

    /// Type registry - stores type definitions outside the graph
    pub type_registry: TypeRegistry,

    pub coverage: GraphCoverage,
    pub size_function_id: String,
    pub measurement_scope_id: String,
}

impl Default for ContextGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            symbol_to_node: HashMap::new(),
            type_registry: TypeRegistry::new(),
            coverage: GraphCoverage::default(),
            size_function_id: "unknown".to_string(),
            measurement_scope_id: "repository-local".to_string(),
        }
    }

    pub fn add_node(&mut self, symbol: SymbolId, node: Node) -> NodeIndex {
        let idx = self.graph.add_node(node);
        self.symbol_to_node.insert(symbol, idx);
        idx
    }

    pub fn add_edge(&mut self, source: NodeIndex, target: NodeIndex, kind: EdgeKind) {
        self.graph.add_edge(source, target, kind);
    }

    pub fn get_node_by_symbol(&self, symbol: &str) -> Option<NodeIndex> {
        self.symbol_to_node.get(symbol).copied()
    }

    pub fn node(&self, idx: NodeIndex) -> &Node {
        &self.graph[idx]
    }

    /// Outgoing edges from `idx` (forward traversal).
    pub fn outgoing_edges(&self, idx: NodeIndex) -> impl Iterator<Item = (NodeIndex, &EdgeKind)> {
        self.graph
            .neighbors_directed(idx, petgraph::Direction::Outgoing)
            .map(move |neighbor| {
                let edge = self.graph.find_edge(idx, neighbor).unwrap();
                (neighbor, self.graph.edge_weight(edge).unwrap())
            })
    }

    /// Incoming edges to `idx` (reverse exploration). Optionally filter by edge kind.
    pub fn incoming_edges(
        &self,
        idx: NodeIndex,
        filter: Option<EdgeKind>,
    ) -> impl Iterator<Item = (NodeIndex, &EdgeKind)> {
        self.graph
            .neighbors_directed(idx, petgraph::Direction::Incoming)
            .map(move |source| {
                let edge = self.graph.find_edge(source, idx).unwrap();
                (source, self.graph.edge_weight(edge).unwrap())
            })
            .filter(move |(_, kind)| {
                filter
                    .as_ref()
                    .is_none_or(|f| std::mem::discriminant(*kind) == std::mem::discriminant(f))
            })
    }

    /// Alias for [Self::outgoing_edges] (backward compatibility).
    pub fn neighbors(&self, idx: NodeIndex) -> impl Iterator<Item = (NodeIndex, &EdgeKind)> {
        self.outgoing_edges(idx)
    }

    /// Find all method nodes whose `scope` (enclosing type) matches the given type symbol.
    /// Returns `(symbol_id, NodeIndex)` pairs.
    pub fn find_class_members(&self, class_symbol: &str) -> Vec<(String, NodeIndex)> {
        let mut members = Vec::new();
        for (symbol, &idx) in &self.symbol_to_node {
            let node = self.node(idx);
            if let Node::Function(f) = node
                && f.core.scope.as_deref() == Some(class_symbol)
            {
                members.push((symbol.clone(), idx));
            }
        }
        members.sort_by(|a, b| a.0.cmp(&b.0));
        members
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::edge::EdgeKind;
    use crate::domain::node::{FunctionNode, Node, NodeCore, SourceSpan};

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
            visibility: crate::domain::node::Visibility::Public,
            return_types: vec![],
            is_interface_method: false,
            is_constructor: false,
            is_di_wired: false,
        })
    }

    #[test]
    fn test_create_empty_graph() {
        let graph = ContextGraph::new();
        assert_eq!(graph.graph.node_count(), 0);
        assert_eq!(graph.graph.edge_count(), 0);
        assert!(graph.symbol_to_node.is_empty());
    }

    #[test]
    fn test_add_node_returns_index() {
        let mut graph = ContextGraph::new();
        let idx = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        assert_eq!(graph.graph.node_count(), 1);
        assert_eq!(graph.graph[idx].core().id, 0);
    }

    #[test]
    fn test_add_edge_creates_connection() {
        let mut graph = ContextGraph::new();
        let idx_a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let idx_b = graph.add_node("sym::b".into(), test_node(1, "b", 20));
        graph.add_edge(idx_a, idx_b, EdgeKind::Call);
        assert_eq!(graph.graph.edge_count(), 1);
        let out: Vec<_> = graph.outgoing_edges(idx_a).collect();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, idx_b);
        assert!(matches!(out[0].1, EdgeKind::Call));
    }

    #[test]
    fn test_get_node_by_symbol() {
        let mut graph = ContextGraph::new();
        let idx = graph.add_node("sym::foo".into(), test_node(0, "foo", 15));
        assert_eq!(graph.get_node_by_symbol("sym::foo"), Some(idx));
        assert_eq!(
            graph
                .node(graph.get_node_by_symbol("sym::foo").unwrap())
                .core()
                .name,
            "foo"
        );
    }

    #[test]
    fn test_neighbors_iterator() {
        let mut graph = ContextGraph::new();
        let idx_a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let idx_b = graph.add_node("sym::b".into(), test_node(1, "b", 10));
        let idx_c = graph.add_node("sym::c".into(), test_node(2, "c", 10));
        graph.add_edge(idx_a, idx_b, EdgeKind::Call);
        graph.add_edge(idx_a, idx_c, EdgeKind::Call);
        let mut out: Vec<_> = graph
            .outgoing_edges(idx_a)
            .map(|(i, k)| (i, k.clone()))
            .collect();
        out.sort_by_key(|(i, _)| i.index());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, idx_b);
        assert_eq!(out[1].0, idx_c);
    }

    #[test]
    fn test_nonexistent_symbol_returns_none() {
        let graph = ContextGraph::new();
        assert_eq!(graph.get_node_by_symbol("nonexistent"), None);
        let mut g = ContextGraph::new();
        g.add_node("sym::x".into(), test_node(0, "x", 1));
        assert_eq!(g.get_node_by_symbol("sym::y"), None);
    }

    #[test]
    fn test_duplicate_symbol_overwrites() {
        let mut graph = ContextGraph::new();
        let n1 = test_node(0, "first", 10);
        let n2 = test_node(1, "second", 20);
        let _i1 = graph.add_node("sym::dup".into(), n1);
        let i2 = graph.add_node("sym::dup".into(), n2);
        assert_eq!(graph.graph.node_count(), 2);
        assert_eq!(graph.get_node_by_symbol("sym::dup"), Some(i2));
        assert_eq!(graph.node(i2).core().context_size, 20);
    }

    #[test]
    fn test_empty_neighbors() {
        let mut graph = ContextGraph::new();
        let idx = graph.add_node("sym::sink".into(), test_node(0, "sink", 5));
        let count = graph.outgoing_edges(idx).count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_node_content_preserved() {
        let mut graph = ContextGraph::new();
        let n = test_node(42, "preserved", 100);
        let idx = graph.add_node("sym::p".into(), n);
        let got = graph.node(idx);
        assert_eq!(got.core().id, 42);
        assert_eq!(got.core().name, "preserved");
        assert_eq!(got.core().context_size, 100);
    }

    #[test]
    fn test_multiple_edges_same_direction() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 10));
        graph.add_edge(a, b, EdgeKind::Call);
        graph.add_edge(a, b, EdgeKind::Call); // petgraph allows multi-edges
        assert!(graph.graph.edge_count() >= 2);
    }

    #[test]
    fn test_different_edge_kinds() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 10));
        graph.add_edge(a, b, EdgeKind::Read);
        let out: Vec<_> = graph.outgoing_edges(a).collect();
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].1, EdgeKind::Read));
    }

    #[test]
    fn test_symbol_to_node_consistency() {
        let mut graph = ContextGraph::new();
        let symbols = ["sym::x", "sym::y", "sym::z"];
        let mut indices = Vec::new();
        for (i, &s) in symbols.iter().enumerate() {
            let idx = graph.add_node(s.into(), test_node(i as u32, s, 1));
            indices.push((s, idx));
        }
        for (sym, idx) in indices {
            assert_eq!(graph.get_node_by_symbol(sym), Some(idx));
            assert_eq!(graph.symbol_to_node.get(sym).copied(), Some(idx));
        }
    }

    #[test]
    fn test_neighbors_only_outgoing() {
        let mut graph = ContextGraph::new();
        let a = graph.add_node("sym::a".into(), test_node(0, "a", 10));
        let b = graph.add_node("sym::b".into(), test_node(1, "b", 10));
        graph.add_edge(a, b, EdgeKind::Call);
        assert_eq!(graph.outgoing_edges(a).count(), 1);
        assert_eq!(graph.outgoing_edges(b).count(), 0);
    }

    #[test]
    fn test_add_three_nodes_linear_chain() {
        let mut graph = ContextGraph::new();
        let i1 = graph.add_node("sym::1".into(), test_node(0, "n1", 10));
        let i2 = graph.add_node("sym::2".into(), test_node(1, "n2", 20));
        let i3 = graph.add_node("sym::3".into(), test_node(2, "n3", 30));
        graph.add_edge(i1, i2, EdgeKind::Call);
        graph.add_edge(i2, i3, EdgeKind::Call);
        assert_eq!(graph.graph.node_count(), 3);
        assert_eq!(graph.graph.edge_count(), 2);
        assert_eq!(graph.outgoing_edges(i1).count(), 1);
        assert_eq!(graph.outgoing_edges(i2).count(), 1);
        assert_eq!(graph.outgoing_edges(i3).count(), 0);
    }
}
