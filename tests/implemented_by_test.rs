mod common;

use common::mock::{MockDocScorer, MockSizeFunction, MockSourceReader};
use context_footprint::domain::builder::GraphBuilder;
use context_footprint::domain::edge::EdgeKind;
use context_footprint::domain::policy::PruningParams;
use context_footprint::domain::semantic::{
    DocumentSemantics, FunctionDetails, FunctionModifiers, Parameter, ReferenceRole, SemanticData,
    SourceLocation, SourceSpan, SymbolDefinition, SymbolDetails, SymbolKind, SymbolReference,
    TypeDetails, Visibility,
};
use context_footprint::domain::solver::CfSolver;
use std::sync::Arc;

fn build_payment_gateway_fixture(interface_doc_score: f32) -> SemanticData {
    let interface_id = "test#IPaymentGateway#";
    let charge_method_id = "test#IPaymentGateway#charge().";
    let stripe_type_id = "test#StripeGateway#";
    let stripe_charge_id = "test#StripeGateway#charge().";
    let paypal_type_id = "test#PayPalGateway#";
    let paypal_charge_id = "test#PayPalGateway#charge().";
    let paypal_call_api_id = "test#PayPalGateway#_call_paypal_api().";
    let process_order_id = "test#process_order().";

    let has_docs = interface_doc_score > 0.0;
    let charge_docs = if has_docs {
        vec!["Charges the given amount. Returns True on success.".to_string()]
    } else {
        vec![]
    };

    SemanticData {
        project_root: "/test".to_string(),
        documents: vec![DocumentSemantics {
            relative_path: "payment.py".to_string(),
            language: "python".to_string(),
            definitions: vec![
                // IPaymentGateway interface
                SymbolDefinition {
                    symbol_id: interface_id.to_string(),
                    kind: SymbolKind::Type,
                    name: "IPaymentGateway".to_string(),
                    display_name: "IPaymentGateway".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
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
                    documentation: vec![],
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
                // IPaymentGateway.charge() - interface method
                SymbolDefinition {
                    symbol_id: charge_method_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "charge".to_string(),
                    display_name: "charge".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
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
                    documentation: charge_docs,
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "amount".to_string(),
                            param_type: Some("float#".to_string()),
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
                // StripeGateway (implements IPaymentGateway)
                SymbolDefinition {
                    symbol_id: stripe_type_id.to_string(),
                    kind: SymbolKind::Type,
                    name: "StripeGateway".to_string(),
                    display_name: "StripeGateway".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 5,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 5,
                        start_column: 0,
                        end_line: 10,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec![],
                    details: SymbolDetails::Type(TypeDetails {
                        kind: context_footprint::domain::semantic::TypeKind::Class,
                        is_abstract: false,
                        is_final: false,
                        visibility: Visibility::Public,
                        type_params: vec![],
                        implements: vec![interface_id.to_string()],
                        inherits: vec![],
                        fields: vec![],
                    }),
                },
                // StripeGateway.charge() - documented implementation
                SymbolDefinition {
                    symbol_id: stripe_charge_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "charge".to_string(),
                    display_name: "charge".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 6,
                        column: 4,
                    },
                    span: SourceSpan {
                        start_line: 6,
                        start_column: 4,
                        end_line: 9,
                        end_column: 0,
                    },
                    enclosing_symbol: Some(stripe_type_id.to_string()),
                    is_external: false,
                    documentation: vec![
                        "Charges via Stripe API. Returns True on success.".to_string(),
                    ],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "amount".to_string(),
                            param_type: Some("float#".to_string()),
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
                            is_abstract: false,
                            is_constructor: false,
                            is_di_wired: false,
                            use_signature_only_for_size: false,
                            visibility: Visibility::Public,
                        },
                    }),
                },
                // PayPalGateway (implements IPaymentGateway)
                SymbolDefinition {
                    symbol_id: paypal_type_id.to_string(),
                    kind: SymbolKind::Type,
                    name: "PayPalGateway".to_string(),
                    display_name: "PayPalGateway".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 12,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 12,
                        start_column: 0,
                        end_line: 18,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec![],
                    details: SymbolDetails::Type(TypeDetails {
                        kind: context_footprint::domain::semantic::TypeKind::Class,
                        is_abstract: false,
                        is_final: false,
                        visibility: Visibility::Public,
                        type_params: vec![],
                        implements: vec![interface_id.to_string()],
                        inherits: vec![],
                        fields: vec![],
                    }),
                },
                // PayPalGateway.charge() - undocumented implementation
                SymbolDefinition {
                    symbol_id: paypal_charge_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "charge".to_string(),
                    display_name: "charge".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 13,
                        column: 4,
                    },
                    span: SourceSpan {
                        start_line: 13,
                        start_column: 4,
                        end_line: 16,
                        end_column: 0,
                    },
                    enclosing_symbol: Some(paypal_type_id.to_string()),
                    is_external: false,
                    documentation: vec![],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "amount".to_string(),
                            param_type: Some("float#".to_string()),
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
                            is_abstract: false,
                            is_constructor: false,
                            is_di_wired: false,
                            use_signature_only_for_size: false,
                            visibility: Visibility::Public,
                        },
                    }),
                },
                // PayPalGateway._call_paypal_api() - internal method
                SymbolDefinition {
                    symbol_id: paypal_call_api_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "_call_paypal_api".to_string(),
                    display_name: "_call_paypal_api".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 17,
                        column: 4,
                    },
                    span: SourceSpan {
                        start_line: 17,
                        start_column: 4,
                        end_line: 18,
                        end_column: 0,
                    },
                    enclosing_symbol: Some(paypal_type_id.to_string()),
                    is_external: false,
                    documentation: vec![],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![Parameter {
                            name: "amount".to_string(),
                            param_type: Some("float#".to_string()),
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
                            is_abstract: false,
                            is_constructor: false,
                            is_di_wired: false,
                            use_signature_only_for_size: false,
                            visibility: Visibility::Private,
                        },
                    }),
                },
                // process_order() - caller
                SymbolDefinition {
                    symbol_id: process_order_id.to_string(),
                    kind: SymbolKind::Function,
                    name: "process_order".to_string(),
                    display_name: "process_order".to_string(),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 20,
                        column: 0,
                    },
                    span: SourceSpan {
                        start_line: 20,
                        start_column: 0,
                        end_line: 22,
                        end_column: 0,
                    },
                    enclosing_symbol: None,
                    is_external: false,
                    documentation: vec![],
                    details: SymbolDetails::Function(FunctionDetails {
                        parameters: vec![],
                        return_types: vec!["bool#".to_string()],
                        type_params: vec![],
                        surface_spans: vec![],
                        implementation_spans: vec![],
                        modifiers: FunctionModifiers::default(),
                    }),
                },
            ],
            references: vec![
                // process_order() calls IPaymentGateway.charge()
                SymbolReference {
                    target_symbol: Some(charge_method_id.to_string()),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 21,
                        column: 4,
                    },
                    enclosing_symbol: process_order_id.to_string(),
                    role: ReferenceRole::Call,
                    receiver: None,
                    method_name: None,
                    assigned_to: None,
                },
                // PayPalGateway.charge() calls _call_paypal_api()
                SymbolReference {
                    target_symbol: Some(paypal_call_api_id.to_string()),
                    location: SourceLocation {
                        file_path: "payment.py".to_string(),
                        line: 15,
                        column: 8,
                    },
                    enclosing_symbol: paypal_charge_id.to_string(),
                    role: ReferenceRole::Call,
                    receiver: None,
                    method_name: None,
                    assigned_to: None,
                },
            ],
        }],
        external_symbols: vec![],
    }
}

fn build_graph_with_score(doc_score: f32) -> context_footprint::domain::graph::ContextGraph {
    let semantic_data = build_payment_gateway_fixture(doc_score);
    let source_reader = MockSourceReader::new().with_file(
        "/test/payment.py",
        "class IPaymentGateway(Protocol):\n    def charge(self, amount: float) -> bool:\n        ...\n\n\nclass StripeGateway:\n    def charge(self, amount: float) -> bool:\n        \"\"\"Charges via Stripe API.\"\"\"\n        return True\n\n\nclass PayPalGateway:\n    def charge(self, amount: float) -> bool:\n        result = self._call_paypal_api(amount)\n        return result\n\n    def _call_paypal_api(self, amount: float) -> bool:\n        return True\n\n\ndef process_order():\n    pass\n",
    );
    let builder = GraphBuilder::new(
        Box::new(MockSizeFunction::with_size(10)),
        Box::new(MockDocScorer::with_score(doc_score)),
    );
    builder
        .build(semantic_data, &source_reader)
        .expect("Failed to build graph")
}

#[test]
fn test_implemented_by_edges_created() {
    let graph = build_graph_with_score(0.0);

    let interface_method_idx = graph
        .get_node_by_symbol("test#IPaymentGateway#charge().")
        .expect("Interface method should exist");

    let neighbors: Vec<_> = graph.neighbors(interface_method_idx).collect();
    let implemented_by_edges: Vec<_> = neighbors
        .iter()
        .filter(|(_, kind)| matches!(kind, EdgeKind::OverriddenBy))
        .collect();

    assert_eq!(
        implemented_by_edges.len(),
        2,
        "Should have OverriddenBy edges to both StripeGateway.charge and PayPalGateway.charge"
    );
}

#[test]
fn test_implementors_registered_in_type_registry() {
    let graph = build_graph_with_score(0.0);

    let implementors = graph
        .type_registry
        .get_implementors("test#IPaymentGateway#")
        .expect("Should have implementors for the interface");

    assert_eq!(implementors.len(), 2);
    assert!(implementors.contains(&"test#StripeGateway#".to_string()));
    assert!(implementors.contains(&"test#PayPalGateway#".to_string()));
}

#[test]
fn test_undocumented_interface_expands_to_implementations() {
    let graph = build_graph_with_score(0.0);

    let process_order_idx = graph
        .get_node_by_symbol("test#process_order().")
        .expect("process_order should exist");

    let solver = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5));
    let result = solver.compute_cf(&[process_order_idx], None);

    // process_order -> IPaymentGateway.charge (transparent, undocumented interface)
    //   -> StripeGateway.charge (undocumented with score 0.0 -> transparent)
    //   -> PayPalGateway.charge (undocumented -> transparent)
    //     -> PayPalGateway._call_paypal_api (transparent)
    // All nodes should be reachable
    assert!(
        result.reachable_set.len() >= 4,
        "Undocumented interface should expand to all implementations. Got {} reachable nodes",
        result.reachable_set.len()
    );
}

#[test]
fn test_documented_interface_stops_at_boundary() {
    let graph = build_graph_with_score(0.8);

    let process_order_idx = graph
        .get_node_by_symbol("test#process_order().")
        .expect("process_order should exist");

    let solver = CfSolver::new(Arc::new(graph), PruningParams::academic(0.5));
    let result = solver.compute_cf(&[process_order_idx], None);

    // process_order -> IPaymentGateway.charge (Boundary: sig complete + doc_score 0.8 >= 0.5)
    // Traversal stops at the interface method. OverriddenBy edges are never followed.
    assert_eq!(
        result.reachable_set.len(),
        2,
        "Documented interface method should be a boundary, stopping traversal. Got {} nodes",
        result.reachable_set.len()
    );

    // The boundary pays its signature and documentation surface, but not implementations.
    assert_eq!(result.total_context_size, 30);
}

#[test]
fn test_no_implemented_by_for_non_interface_methods() {
    let graph = build_graph_with_score(0.0);

    // StripeGateway.charge is NOT an interface method, should have no OverriddenBy edges
    let stripe_charge_idx = graph
        .get_node_by_symbol("test#StripeGateway#charge().")
        .expect("StripeGateway.charge should exist");

    let implemented_by_count = graph
        .neighbors(stripe_charge_idx)
        .filter(|(_, kind)| matches!(kind, EdgeKind::OverriddenBy))
        .count();

    assert_eq!(
        implemented_by_count, 0,
        "Non-interface methods should not have OverriddenBy edges"
    );
}
