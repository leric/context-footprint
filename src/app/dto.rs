use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PolicyKind {
    #[default]
    Academic,
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub semantic_path: String,
    pub project_root: String,
    pub node_count: usize,
    pub edge_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ComputeRequest {
    pub symbols: Vec<String>,
    #[serde(default)]
    pub policy: PolicyKind,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ComputeResponse {
    pub starting_symbols: Vec<String>,
    /// Compatibility alias for `cf_total`.
    pub total_context_size: u32,
    pub cf_total: u32,
    pub cf_out: u32,
    pub cf_in: u32,
    pub cf_overlap: u32,
    pub out_fragments: Vec<String>,
    pub in_fragments: Vec<String>,
    pub truncated: bool,
    pub unresolved_state_count: usize,
    pub boundary_stop_count: usize,
    pub boundary_stops: Vec<BoundaryStopDto>,
    pub boundary_policy_id: String,
    pub size_function_id: String,
    pub measurement_scope_id: String,
    pub graph_total_references: usize,
    pub graph_unresolved_calls: usize,
    pub graph_total_functions: usize,
    pub graph_functions_with_explicit_fragments: usize,
    pub reachable_node_count: usize,
    pub reachable_nodes_by_layer: Vec<Vec<ReachableNode>>,
    pub reachable_nodes_ordered: Vec<ReachableNode>,
    /// How each input anchor was resolved (class expansion, variable lookup, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_resolutions: Option<Vec<AnchorResolution>>,
}

/// Describes how an input anchor symbol was interpreted and resolved.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnchorResolution {
    pub input: String,
    pub resolved_kind: String,
    /// Symbols the anchor was expanded into (for class anchors: member methods).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded_to: Option<Vec<String>>,
    /// Reason the anchor could not be resolved, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolved_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BoundaryStopDto {
    pub source_node_id: u32,
    pub target_node_id: u32,
    pub direction: String,
    pub relation: String,
}

fn default_max_paths() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReachabilityRequest {
    pub from: Vec<String>,
    pub to: Vec<String>,
    #[serde(default)]
    pub policy: PolicyKind,
    #[serde(default)]
    pub witness_paths: bool,
    #[serde(default = "default_max_paths")]
    pub max_paths: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReachabilityResponse {
    pub reachable: bool,
    pub hit_targets: Vec<String>,
    pub unresolved_from: Vec<String>,
    pub unresolved_to: Vec<String>,
    pub visited_node_count: usize,
    pub witness_paths: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReachableNode {
    pub id: u32,
    pub symbol: String,
    pub node_type: String,
    pub context_size: u32,
    pub file_path: String,
    pub span: SpanDto,
    pub doc_score: f32,
    pub is_external: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpanDto {
    /// 0-based, inclusive start line.
    pub start_line: u32,
    /// 0-based, inclusive start column.
    pub start_column: u32,
    /// 0-based, inclusive end line.
    pub end_line: u32,
    /// 0-based, inclusive end column.
    pub end_column: u32,

    /// 1-based, inclusive start line (for display).
    pub start_line_1based: u32,
    /// 1-based, inclusive end line (for display).
    pub end_line_1based: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatsResponse {
    pub functions: CfDistribution,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CfDistribution {
    pub count: usize,
    pub percentiles: Vec<PercentileValue>,
    pub average: u64,
    pub median: u32,
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PercentileValue {
    pub percentile: u32,
    pub tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TopResponse {
    pub items: Vec<TopItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TopItem {
    pub symbol: String,
    pub node_type: String,
    pub cf: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub items: Vec<SearchItem>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchItem {
    pub symbol: String,
    pub node_type: String,
    pub cf: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextRequest {
    pub symbol: String,
    #[serde(default)]
    pub policy: PolicyKind,
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub include_code: bool,
    /// When true, include traversal_steps (edge kind + decision per node) for debugging.
    #[serde(default)]
    pub show_traversal: bool,
}

/// One step in BFS traversal: node plus the edge and decision that led to it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TraversalStepDto {
    pub node: ReachableNode,
    /// Edge kind that led to this node (e.g. "Call", "CallIn"); absent for start node(s).
    pub edge_kind: Option<String>,
    /// Pruning decision at that edge ("Boundary" or "Transparent").
    pub decision: Option<String>,
    /// For functions only: whether the signature is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_signature_complete: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextResponse {
    pub symbol: String,
    /// Compatibility alias for `cf_total`.
    pub total_context_size: u32,
    pub cf_total: u32,
    pub cf_out: u32,
    pub cf_in: u32,
    pub cf_overlap: u32,
    pub out_fragments: Vec<String>,
    pub in_fragments: Vec<String>,
    pub truncated: bool,
    pub unresolved_state_count: usize,
    pub boundary_stop_count: usize,
    pub boundary_stops: Vec<BoundaryStopDto>,
    pub boundary_policy_id: String,
    pub size_function_id: String,
    pub measurement_scope_id: String,
    pub graph_total_references: usize,
    pub graph_unresolved_calls: usize,
    pub graph_total_functions: usize,
    pub graph_functions_with_explicit_fragments: usize,
    pub reachable_node_count: usize,
    pub layers: Vec<ContextLayer>,
    /// Traversal steps in BFS order (only set when request had show_traversal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traversal_steps: Option<Vec<TraversalStepDto>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextLayer {
    pub depth: usize,
    pub files: Vec<ContextFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextFile {
    pub file_path: String,
    pub nodes: Vec<ContextNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextNode {
    pub id: u32,
    pub symbol: String,
    pub node_type: String,
    pub context_size: u32,
    pub span: SpanDto,
    pub doc_score: f32,
    pub is_external: bool,
    pub code: Option<Vec<CodeLine>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeLine {
    pub line_number: u32, // 1-based
    pub text: String,
}
