//! Test fixture generators for integration tests.
#![allow(dead_code)]

use context_footprint::domain::semantic::{
    DocumentSemantics, FunctionDetails, FunctionModifiers, Mutability, Parameter, ReferenceRole,
    SemanticData, SourceLocation, SourceSpan, SymbolDefinition, SymbolDetails, SymbolKind,
    SymbolReference, TypeDetails, TypeKind, VariableDetails, VariableScope, Visibility,
};

fn default_location() -> SourceLocation {
    SourceLocation {
        file_path: "test.py".to_string(),
        line: 0,
        column: 0,
    }
}

fn default_span() -> SourceSpan {
    SourceSpan {
        start_line: 0,
        start_column: 0,
        end_line: 1,
        end_column: 10,
    }
}

pub fn function_def(
    symbol_id: &str,
    name: &str,
    documentation: Vec<String>,
    parameters: Vec<Parameter>,
    return_type: Option<String>,
) -> SymbolDefinition {
    let return_types = return_type.map(|s| vec![s]).unwrap_or_default();
    SymbolDefinition {
        symbol_id: symbol_id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        display_name: name.to_string(),
        location: default_location(),
        span: SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 5,
            end_column: 20,
        },
        enclosing_symbol: None,
        is_external: false,
        documentation,
        details: SymbolDetails::Function(FunctionDetails {
            parameters,
            return_types,
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
    }
}

/// One file, one function with use_signature_only_for_size=true and a long span (0..25).
/// Used to assert that context_size is computed from signature only, not full body.
pub fn create_semantic_data_annotated_style_factory() -> SemanticData {
    let sym = "mod::Body";
    let mut def = function_def(
        sym,
        "Body",
        vec!["Doc for param.".into()],
        vec![Parameter {
            name: "default".into(),
            param_type: Some("Any".into()),
            is_high_freedom_type: true,
            has_default: true,
            is_variadic: false,
        }],
        Some("Any".into()),
    );
    def.span = SourceSpan {
        start_line: 0,
        start_column: 0,
        end_line: 25,
        end_column: 0,
    };
    if let SymbolDetails::Function(ref mut fd) = def.details {
        fd.modifiers.use_signature_only_for_size = true;
    }
    let documents = vec![DocumentSemantics {
        relative_path: "param_functions.py".into(),
        language: "python".into(),
        definitions: vec![def],
        references: vec![],
    }];
    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

pub fn variable_def(
    symbol_id: &str,
    name: &str,
    documentation: Vec<String>,
    var_type: Option<String>,
    mutability: Mutability,
) -> SymbolDefinition {
    SymbolDefinition {
        symbol_id: symbol_id.to_string(),
        kind: SymbolKind::Variable,
        name: name.to_string(),
        display_name: name.to_string(),
        location: default_location(),
        span: default_span(),
        enclosing_symbol: None,
        is_external: false,
        documentation,
        details: SymbolDetails::Variable(VariableDetails {
            var_type,
            mutability,
            scope: VariableScope::Global,
            visibility: Visibility::Public,
        }),
    }
}

pub fn type_def(
    symbol_id: &str,
    name: &str,
    documentation: Vec<String>,
    type_kind: TypeKind,
    is_abstract: bool,
) -> SymbolDefinition {
    SymbolDefinition {
        symbol_id: symbol_id.to_string(),
        kind: SymbolKind::Type,
        name: name.to_string(),
        display_name: name.to_string(),
        location: default_location(),
        span: default_span(),
        enclosing_symbol: None,
        is_external: false,
        documentation,
        details: SymbolDetails::Type(TypeDetails {
            kind: type_kind,
            is_abstract,
            is_final: false,
            visibility: Visibility::Public,
            type_params: vec![],
            fields: vec![],
            implements: vec![],
            inherits: vec![],
        }),
    }
}

pub fn method_def(
    symbol_id: &str,
    name: &str,
    enclosing_symbol: &str,
    documentation: Vec<String>,
    parameters: Vec<Parameter>,
    return_type: Option<String>,
) -> SymbolDefinition {
    let mut def = function_def(symbol_id, name, documentation, parameters, return_type);
    def.enclosing_symbol = Some(enclosing_symbol.to_string());
    def
}

pub fn call_reference(target: &str, enclosing: &str) -> SymbolReference {
    SymbolReference {
        target_symbol: Some(target.to_string()),
        location: default_location(),
        enclosing_symbol: enclosing.to_string(),
        role: ReferenceRole::Call,
        receiver: None,
        method_name: None,
        assigned_to: None,
    }
}

pub fn read_reference(target: &str, enclosing: &str) -> SymbolReference {
    SymbolReference {
        target_symbol: Some(target.to_string()),
        location: default_location(),
        enclosing_symbol: enclosing.to_string(),
        role: ReferenceRole::Read,
        receiver: None,
        method_name: None,
        assigned_to: None,
    }
}

pub fn write_reference(target: &str, enclosing: &str) -> SymbolReference {
    SymbolReference {
        target_symbol: Some(target.to_string()),
        location: default_location(),
        enclosing_symbol: enclosing.to_string(),
        role: ReferenceRole::Write,
        receiver: None,
        method_name: None,
        assigned_to: None,
    }
}

/// Simplest case: one file, two symbols (func_a, func_b), func_a calls func_b.
pub fn create_semantic_data_simple() -> SemanticData {
    let sym_a = "sym::func_a";
    let sym_b = "sym::func_b";

    let documents = vec![DocumentSemantics {
        relative_path: "main.py".into(),
        language: "python".into(),
        definitions: vec![
            function_def(
                sym_a,
                "func_a",
                vec!["Doc for A".into()],
                vec![Parameter {
                    name: "x".into(),
                    param_type: Some("int".into()),
                    is_high_freedom_type: true,
                    has_default: false,
                    is_variadic: false,
                }],
                Some("int".into()),
            ),
            function_def(sym_b, "func_b", vec![], vec![], None),
        ],
        references: vec![call_reference(sym_b, sym_a)],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Two documents: main.py (func_main) and utils.py (func_util). func_main calls func_util.
pub fn create_semantic_data_two_files() -> SemanticData {
    let sym_main = "sym::main::func_main";
    let sym_util = "sym::utils::func_util";

    let documents = vec![
        DocumentSemantics {
            relative_path: "main.py".into(),
            language: "python".into(),
            definitions: vec![function_def(
                sym_main,
                "func_main",
                vec!["Main".into()],
                vec![],
                Some("()".into()),
            )],
            references: vec![call_reference(sym_util, sym_main)],
        },
        DocumentSemantics {
            relative_path: "utils.py".into(),
            language: "python".into(),
            definitions: vec![function_def(sym_util, "func_util", vec![], vec![], None)],
            references: vec![],
        },
    ];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Cycle: A -> B -> C -> A.
pub fn create_semantic_data_with_cycle() -> SemanticData {
    let sym_a = "sym::a";
    let sym_b = "sym::b";
    let sym_c = "sym::c";

    let documents = vec![DocumentSemantics {
        relative_path: "cycle.py".into(),
        language: "python".into(),
        definitions: vec![
            function_def(sym_a, "a", vec![], vec![], None),
            function_def(sym_b, "b", vec![], vec![], None),
            function_def(sym_c, "c", vec![], vec![], None),
        ],
        references: vec![
            call_reference(sym_b, sym_a),
            call_reference(sym_c, sym_b),
            call_reference(sym_a, sym_c),
        ],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Shared mutable state: reader R and writers W1, W2. R reads var V; W1 and W2 write V.
/// Builder should add SharedStateWrite edges R->W1, R->W2.
pub fn create_semantic_data_with_shared_state() -> SemanticData {
    let sym_r = "sym::reader";
    let sym_w1 = "sym::writer1";
    let sym_w2 = "sym::writer2";
    let sym_v = "sym::global_var";

    let documents = vec![DocumentSemantics {
        relative_path: "state.py".into(),
        language: "python".into(),
        definitions: vec![
            function_def(sym_r, "reader", vec![], vec![], None),
            function_def(sym_w1, "writer1", vec![], vec![], None),
            function_def(sym_w2, "writer2", vec![], vec![], None),
            variable_def(
                sym_v,
                "global_var",
                vec![],
                Some("int".into()),
                Mutability::Mutable,
            ),
        ],
        references: vec![
            read_reference(sym_v, sym_r),
            write_reference(sym_v, sym_w1),
            write_reference(sym_v, sym_w2),
        ],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Chain A -> B -> C with B well-documented. Used to compare policies: Academic stops at B, Strict continues to C.
pub fn create_semantic_data_chain_well_documented_middle() -> SemanticData {
    let sym_a = "sym::chain_a";
    let sym_b = "sym::chain_b";
    let sym_c = "sym::chain_c";

    let documents = vec![DocumentSemantics {
        relative_path: "chain.py".into(),
        language: "python".into(),
        definitions: vec![
            function_def(sym_a, "chain_a", vec![], vec![], None),
            function_def(
                sym_b,
                "chain_b",
                vec!["Well documented.".into()],
                vec![],
                Some("int".into()),
            ),
            function_def(sym_c, "chain_c", vec![], vec![], None),
        ],
        references: vec![call_reference(sym_b, sym_a), call_reference(sym_c, sym_b)],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// One document with no definitions and no references. Builder should produce 0 nodes.
pub fn create_semantic_data_empty_document() -> SemanticData {
    let documents = vec![DocumentSemantics {
        relative_path: "empty.py".into(),
        language: "python".into(),
        definitions: vec![],
        references: vec![],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Multiple callers: callee C is called by A and by B. Builder should add CallIn edges C->A and C->B.
pub fn create_semantic_data_multiple_callers() -> SemanticData {
    let sym_a = "sym::caller_a";
    let sym_b = "sym::caller_b";
    let sym_c = "sym::callee";

    let documents = vec![DocumentSemantics {
        relative_path: "multi_call.py".into(),
        language: "python".into(),
        definitions: vec![
            function_def(sym_a, "caller_a", vec![], vec![], None),
            function_def(sym_b, "caller_b", vec![], vec![], None),
            function_def(sym_c, "callee", vec![], vec![], None),
        ],
        references: vec![call_reference(sym_c, sym_a), call_reference(sym_c, sym_b)],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Constructor call to Type: caller invokes MyClass() which targets the Type symbol.
/// Builder should resolve this to MyClass.__init__ if it exists.
pub fn create_semantic_data_with_constructor_call() -> SemanticData {
    let sym_class = "sym::MyClass";
    let sym_init = "sym::MyClass.__init__";
    let sym_caller = "sym::caller";

    let documents = vec![DocumentSemantics {
        relative_path: "ctor.py".into(),
        language: "python".into(),
        definitions: vec![
            type_def(sym_class, "MyClass", vec![], TypeKind::Class, false),
            method_def(sym_init, "__init__", sym_class, vec![], vec![], None),
            function_def(sym_caller, "caller", vec![], vec![], None),
        ],
        references: vec![call_reference(sym_class, sym_caller)],
    }];

    SemanticData {
        project_root: "/test".into(),
        documents,
        external_symbols: vec![],
    }
}

/// Helper to build a MockSourceReader that has file contents for all documents in the semantic data.
/// Caller can pass the SemanticData and optionally override content per path.
pub fn source_reader_for_semantic_data(
    data: &SemanticData,
    default_content: &str,
) -> super::mock::MockSourceReader {
    let mut reader = super::mock::MockSourceReader::new();
    for doc in &data.documents {
        let path = std::path::Path::new(&data.project_root).join(&doc.relative_path);
        reader.add_file(path, default_content);
    }
    reader
}
