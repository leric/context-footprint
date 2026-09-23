/// Integration tests for interface/abstract method functionality
mod common;

use common::mock::{MockDocScorer, MockSizeFunction};
use context_footprint::domain::builder::GraphBuilder;
use context_footprint::domain::edge::EdgeKind;
use context_footprint::domain::node::Node;
use context_footprint::domain::policy::{PruningParams, evaluate};
use context_footprint::domain::ports::SourceReader;
use context_footprint::domain::semantic::{
    DocumentSemantics, FunctionDetails, FunctionModifiers, Parameter, ReferenceRole, SemanticData,
    SourceLocation, SourceSpan, SymbolDefinition, SymbolDetails, SymbolKind, SymbolReference,
    TypeDetails, Visibility,
};
use std::path::Path;

struct MockSourceReader;

impl SourceReader for MockSourceReader {
    fn read(&self, _path: &Path) -> anyhow::Result<String> {
        // Return a mock Python interface method
        Ok("class Repository(Protocol):\n    def load(self, id: str) -> dict:\n        \"\"\"Load data\"\"\"\n        ...\n".to_string())
    }

    fn read_lines(&self, _path: &str, _start: usize, _end: usize) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
}

#[test]
fn test_interface_method_becomes_node_with_flag() {
    // Create semantic data with an interface and a method
    let interface_id = "test#Repository#";
    let method_id = "test#Repository#load().";

    let semantic_data = SemanticData {
        project_root: "/test".to_string(),
        documents: vec![DocumentSemantics {
            relative_path: "test.py".to_string(),
            language: "python".to_string(),
            definitions: vec![
                // Interface/Protocol definition
                SymbolDefinition {
                    symbol_id: interface_id.to_string(),
                    kind: SymbolKind::Type,
                    name: "Repository".to_string(),
                    display_name: "Repository".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 0,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 0,
                        start_column: 0,
                        end_line: 3,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec!["Repository interface".to_string()],
                    details: SymbolDetails::Type(TypeDetails {
                        kind: context_footprint::domain::semantic::TypeKind::Interface,
                        is_abstract: true,
                        is_final: false,
                        visibility: Visibility::Public,
                        type_params: vec![],
                        implements: vec![],
                        inherits: vec![],
                        fields: vec![],
                    }),
                },
                // Method definition in the interface
                SymbolDefinition {
                    symbol_id: method_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "load".to_string(),
                    display_name: "load".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 1,
                        column: 4,
                    },
                    span: SourceSpan {
                        start_line: 1,
                        start_column: 4,
                        end_line: 3,
                        end_column: 12,
                    },
                    enclosing_symbol: Some(interface_id.to_string()),
                    is_external: false,
                    documentation: vec!["Load data".to_string()],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "id".to_string(),
                            param_type: Some("str#".to_string()),
                            is_high_freedom_type: false,
                            has_default: false,
                            is_variadic: false,
                        }],
                        return_types: vec!["dict#".to_string()],
                        type_params: vec![],
                        surface_spans: vec![],
                        implementation_spans: vec![],
                        modifiers: FunctionModifiers {
                            is_async: false,
                            is_generator: false,
                            is_static: false,
                            is_abstract: true,
                            is_constructor: false,
                            is_di_wired: false,
                            use_signature_only_for_size: false,
                            visibility: Visibility::Public,
                        },
                    }),
                },
            ],
            references: vec![],
        }],
        external_symbols: vec![],
    };

    let builder = GraphBuilder::new(
        Box::new(MockSizeFunction::with_size(10)),
        Box::new(MockDocScorer::with_score(0.8)),
    );
    let graph = builder
        .build(semantic_data, &MockSourceReader)
        .expect("Failed to build graph");

    // Verify interface type is in TypeRegistry
    assert!(graph.type_registry.contains(interface_id));
    let type_info = graph.type_registry.get(interface_id).unwrap();
    assert!(type_info.definition.is_abstract);

    // Verify method node exists and is marked as interface method
    let method_idx = graph
        .get_node_by_symbol(method_id)
        .expect("Method should exist as a node");
    let method_node = graph.node(method_idx);

    match method_node {
        Node::Function(f) => {
            assert!(
                f.is_interface_method,
                "Method should be marked as interface method"
            );
            assert!(
                f.is_signature_complete(),
                "Method should have complete signature"
            );
            assert_eq!(f.core.doc_score, 0.8, "Should use mock doc score");
            // Surface charges the mock-sized signature and documentation fragments.
            assert_eq!(f.core.context_size, 20, "Should use surface fragment sizes");
        }
        _ => panic!("Method should be a FunctionNode"),
    }
}

#[test]
fn test_interface_method_with_good_doc_is_boundary() {
    let interface_id = "test#IService#";
    let method_id = "test#IService#process().";

    let semantic_data = SemanticData {
        project_root: "/test".to_string(),
        documents: vec![DocumentSemantics {
            relative_path: "test.py".to_string(),
            language: "python".to_string(),
            definitions: vec![
                SymbolDefinition {
                    symbol_id: interface_id.to_string(),
                    kind: SymbolKind::Type,
                    name: "IService".to_string(),
                    display_name: "IService".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 0,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 0,
                        start_column: 0,
                        end_line: 2,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec!["Service interface".to_string()],
                    details: SymbolDetails::Type(TypeDetails {
                        kind: context_footprint::domain::semantic::TypeKind::Interface,
                        is_abstract: true,
                        is_final: false,
                        visibility: Visibility::Public,
                        type_params: vec![],
                        implements: vec![],
                        inherits: vec![],
                        fields: vec![],
                    }),
                },
                SymbolDefinition {
                    symbol_id: method_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "process".to_string(),
                    display_name: "process".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 1,
                        column: 4,
                    },
                    span: SourceSpan {
                        start_line: 1,
                        start_column: 4,
                        end_line: 2,
                        end_column: 12,
                    },
                    enclosing_symbol: Some(interface_id.to_string()),
                    is_external: false,
                    documentation: vec![
                        "Process data with detailed documentation explaining the contract"
                            .to_string(),
                    ],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "data".to_string(),
                            param_type: Some("str#".to_string()),
                            is_high_freedom_type: false,
                            has_default: false,
                            is_variadic: false,
                        }],
                        return_types: vec!["bool#".to_string()],
                        type_params: vec![],
                        surface_spans: vec![],
                        implementation_spans: vec![],
                        modifiers: FunctionModifiers {
                            is_async: false,
                            is_generator: false,
                            is_static: false,
                            is_abstract: true,
                            is_constructor: false,
                            is_di_wired: false,
                            use_signature_only_for_size: false,
                            visibility: Visibility::Public,
                        },
                    }),
                },
            ],
            references: vec![],
        }],
        external_symbols: vec![],
    };

    let builder = GraphBuilder::new(
        Box::new(MockSizeFunction::with_size(10)),
        Box::new(MockDocScorer::with_score(0.8)),
    );
    let graph = builder
        .build(semantic_data, &MockSourceReader)
        .expect("Failed to build graph");

    let method_idx = graph.get_node_by_symbol(method_id).unwrap();
    let method_node = graph.node(method_idx);

    // Test pruning decision
    let params = PruningParams::academic(0.5);
    let dummy_caller = create_dummy_function(999);

    let decision = evaluate(&params, &dummy_caller, method_node, &EdgeKind::Call, &graph);

    assert_eq!(
        decision,
        context_footprint::domain::policy::PruningDecision::Boundary,
        "Well-documented interface method should be a boundary"
    );
}

#[test]
fn test_call_to_interface_method_creates_edge() {
    let interface_id = "test#DataRepo#";
    let method_id = "test#DataRepo#save().";
    let caller_id = "test#service().";

    let semantic_data = SemanticData {
        project_root: "/test".to_string(),
        documents: vec![DocumentSemantics {
            relative_path: "test.py".to_string(),
            language: "python".to_string(),
            definitions: vec![
                // Interface
                SymbolDefinition {
                    symbol_id: interface_id.to_string(),
                    kind: SymbolKind::Type,
                    name: "DataRepo".to_string(),
                    display_name: "DataRepo".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 0,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 0,
                        start_column: 0,
                        end_line: 2,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec!["Data repository".to_string()],
                    details: SymbolDetails::Type(TypeDetails {
                        kind: context_footprint::domain::semantic::TypeKind::Interface,
                        is_abstract: true,
                        is_final: false,
                        visibility: Visibility::Public,
                        type_params: vec![],
                        implements: vec![],
                        inherits: vec![],
                        fields: vec![],
                    }),
                },
                // Interface method
                SymbolDefinition {
                    symbol_id: method_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "save".to_string(),
                    display_name: "save".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 1,
                        column: 4,
                    },
                    span: SourceSpan {
                        start_line: 1,
                        start_column: 4,
                        end_line: 2,
                        end_column: 12,
                    },
                    enclosing_symbol: Some(interface_id.to_string()),
                    is_external: false,
                    documentation: vec!["Save data".to_string()],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "data".to_string(),
                            param_type: Some("dict#".to_string()),
                            is_high_freedom_type: false,
                            has_default: false,
                            is_variadic: false,
                        }],
                        return_types: vec!["bool#".to_string()],
                        type_params: vec![],
                        surface_spans: vec![],
                        implementation_spans: vec![],
                        modifiers: FunctionModifiers {
                            is_async: false,
                            is_generator: false,
                            is_static: false,
                            is_abstract: true,
                            is_constructor: false,
                            is_di_wired: false,
                            use_signature_only_for_size: false,
                            visibility: Visibility::Public,
                        },
                    }),
                },
                // Caller function
                SymbolDefinition {
                    symbol_id: caller_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "service".to_string(),
                    display_name: "service".to_string(),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 4,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 4,
                        start_column: 0,
                        end_line: 5,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec![],
                    details: SymbolDetails::Function(FunctionDetails::default()),
                },
            ],
            references: vec![
                // service() calls DataRepo.save()
                SymbolReference {
                    target_symbol: Some(method_id.to_string()),
                    location: SourceLocation {
                        file_path: "test.py".to_string(),
                        line: 5,
                        column: 8,
                    },
                    enclosing_symbol: caller_id.to_string(),
                    role: ReferenceRole::Call,
                    receiver: None,
                    method_name: None,
                    assigned_to: None,
                },
            ],
        }],
        external_symbols: vec![],
    };

    let builder = GraphBuilder::new(
        Box::new(MockSizeFunction::with_size(10)),
        Box::new(MockDocScorer::with_score(0.8)),
    );
    let graph = builder
        .build(semantic_data, &MockSourceReader)
        .expect("Failed to build graph");

    // Verify nodes exist
    let caller_idx = graph
        .get_node_by_symbol(caller_id)
        .expect("Caller should exist");
    let method_idx = graph
        .get_node_by_symbol(method_id)
        .expect("Interface method should exist");

    // Verify Call edge exists
    let has_call_edge = graph
        .graph
        .edges_connecting(caller_idx, method_idx)
        .any(|e| matches!(e.weight(), EdgeKind::Call));

    assert!(
        has_call_edge,
        "Should have a Call edge from caller to interface method"
    );
}

// Helper function
fn create_dummy_function(id: u32) -> Node {
    use context_footprint::domain::node::{FunctionNode, NodeCore, SourceSpan, Visibility};

    Node::Function(FunctionNode {
        core: NodeCore::new(
            id,
            "dummy".to_string(),
            None,
            10,
            SourceSpan {
                start_line: 0,
                start_column: 0,
                end_line: 1,
                end_column: 0,
            },
            0.5,
            false,
            "dummy.py".to_string(),
        ),
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
