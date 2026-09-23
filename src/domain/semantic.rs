//! SemanticData - Language-agnostic semantic model for CF graph construction
//!
//! # Design Principles
//!
//! 1. **Graph-centric**: Designed from CF algorithm needs, not indexer format
//! 2. **Language-agnostic**: Abstracts away language differences - builder never checks language
//! 3. **Adapter contract**: Each field has precise semantics that adapter MUST implement correctly
//! 4. **Three symbol types**: Function, Variable, Type (no locals, no standalone parameters)
//! 5. **Declared types only**: No type inference required from adapter (DI abstraction preserved)
//!
//! # Extractor Behavioral Contract
//!
//! This module defines the **data shape** of SemanticData. For the **behavioral completeness
//! requirements** — which source-code patterns MUST produce which references — see
//! [`docs/extractor-spec.md`](../../docs/extractor-spec.md).
//!
//! # Symbol Organization
//!
//! - **Functions**: Standalone functions, methods, constructors
//!   - Methods use `enclosing_symbol` to point to their Type (static relationship)
//! - **Variables**: Global variables and class/struct fields only
//!   - Fields use `scope=Field` and `enclosing_symbol` to point to their Type
//!   - Local variables are NOT extracted (filtered at adapter layer)
//! - **Types**: Classes, interfaces, structs, enums, etc.
//!   - Methods are separate Function definitions (not embedded in Type)
//!   - Fields are referenced in `TypeDetails.fields` and have corresponding Variable definitions
//!
//! # References and Access Patterns
//!
//! References capture usage relationships with context:
//! - **Direct access**: `foo()`, `global_var` → `receiver=None`
//! - **Instance access**: `self.method()`, `obj.field` → `receiver=Some("self" or "obj")`
//! - **Static access**: `Class.static_method()` → depends on language representation

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Symbol identifier - Globally unique within project
///
/// **Adapter Contract**:
/// - Must be globally unique across all files
/// - Should be deterministic (same symbol → same ID across runs)
/// - Format is adapter-specific but must support equality comparison
/// - Recommended: hierarchical format like "pkg.module.Class.method"
/// - For builtin types (int, str, etc.): use language-standard names
pub type SymbolId = String;

/// Type reference - Points to a Type symbol or builtin type
///
/// **Adapter Contract**:
/// - For user-defined types: use the SymbolId of the Type definition
/// - For builtin types (int, str, List, etc.): use standardized names
/// - For generic types: include type parameters (e.g., "List[int]" or "List<T>")
/// - For union types: adapter can use single ref or create synthetic union type
pub type TypeRef = SymbolId;

/// ============================================================================
/// Top-level Container
/// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticData {
    /// Project root directory (absolute path)
    ///
    /// **Adapter Contract**:
    /// - Must be an absolute path to the project root
    /// - All `DocumentSemantics.relative_path` are relative to this root
    pub project_root: String,

    /// Semantic information for all source files in the project
    ///
    /// **Adapter Contract**:
    /// - Include all project source files (exclude test files if configured)
    /// - Each document contains definitions and references for that file
    /// - No duplicate files
    pub documents: Vec<DocumentSemantics>,

    /// External symbols (third-party dependencies, standard library, etc.)
    ///
    /// **Adapter Contract**:
    /// - Definitions for symbols not defined in the project source files
    /// - Used to provide signatures and docs for boundary size calculation
    /// - Must have `is_external: true`
    #[serde(default)]
    pub external_symbols: Vec<SymbolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSemantics {
    /// Relative path from project root
    ///
    /// **Adapter Contract**:
    /// - Use forward slashes (/) even on Windows
    /// - Must be relative to `SemanticData.project_root`
    /// - Should match actual file path for source code reading
    pub relative_path: String,

    /// Programming language identifier
    ///
    /// **Adapter Contract**:
    /// - Use lowercase names: "python", "typescript", "rust", "java", etc.
    /// - Used by DocumentationScorer for language-specific heuristics
    /// - Must be consistent across all documents of the same language
    pub language: String,

    /// Symbol definitions in this file
    ///
    /// **Adapter Contract**:
    /// - Include: Functions, Methods, Constructors, Global Variables, Fields, Types
    /// - Exclude: Local variables, parameters (embedded in FunctionDetails)
    /// - For methods: set `enclosing_symbol` to the Type symbol
    /// - For fields: set `scope=Field` and `enclosing_symbol` to the Type symbol
    /// - Ensure `span` covers the entire definition for accurate context_size
    pub definitions: Vec<SymbolDefinition>,

    /// Symbol references (for edge construction)
    ///
    /// **Adapter Contract**:
    /// - Include all function calls, variable reads/writes, decorators
    /// - Set `receiver` for member access (self.field, obj.method)
    /// - For ambiguous cases (dynamic dispatch), use declared type of receiver
    pub references: Vec<SymbolReference>,
}

/// ============================================================================
/// Symbol Definition
/// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolDefinition {
    /// Globally unique identifier
    ///
    /// **Adapter Contract**: See `SymbolId` documentation
    pub symbol_id: SymbolId,

    /// Symbol classification (Function, Variable, or Type)
    ///
    /// **Adapter Contract**:
    /// - Choose based on primary purpose in code graph:
    ///   * Function: executable units (functions, methods, constructors)
    ///   * Variable: data storage (global vars, class fields)
    ///   * Type: type definitions (classes, interfaces, structs, enums)
    pub kind: SymbolKind,

    /// Short name (without module/package path)
    ///
    /// **Adapter Contract**:
    /// - For methods: just method name (not "Class.method")
    /// - For operators: use canonical name (e.g., "__add__", "operator+")
    /// - Should match the identifier in source code
    pub name: String,

    /// Display name (may include signature or decorations)
    ///
    /// **Adapter Contract**:
    /// - For functions: may include signature (e.g., "foo(x: int) -> str")
    /// - For variables: usually same as `name`
    /// - Used for human-readable output, not semantic matching
    pub display_name: String,

    /// Definition location (file + line/column)
    ///
    /// **Adapter Contract**:
    /// - Points to the start of the symbol definition
    /// - `file_path` must match `DocumentSemantics.relative_path`
    /// - Line/column are 0-indexed
    pub location: SourceLocation,

    /// Source span (for context_size calculation)
    ///
    /// **Adapter Contract** (CRITICAL for accurate CF):
    /// - **Functions**: ENTIRE function body including signature
    ///   * For interface/abstract methods: signature only (no body)
    ///   * Include decorators that affect behavior
    /// - **Variables**: declaration line (including initializer if present)
    /// - **Types**: full type definition including:
    ///   * Class header (class Name(Base):)
    ///   * All field declarations
    ///   * EXCLUDE method bodies (methods are separate definitions)
    /// - Lines are 0-indexed
    /// - end_line/end_column are exclusive (span does NOT include that position)
    pub span: SourceSpan,

    /// Enclosing scope symbol
    ///
    /// **Adapter Contract**:
    /// - **Methods**: MUST point to the Type symbol that defines this method
    /// - **Fields**: MUST point to the Type symbol that owns this field
    /// - **Nested functions**: point to the enclosing Function (if language supports)
    /// - **Top-level symbols**: None
    /// - **Static relationship only**: reflects where symbol is defined, not how it's accessed
    pub enclosing_symbol: Option<SymbolId>,

    /// Whether this is an external dependency
    ///
    /// **Adapter Contract**:
    /// - `true`: stdlib or third-party library symbol (definition not in semantic data)
    /// - `false`: project code (definition found in semantic data)
    /// - External symbols act as CF boundaries (traversal stops but includes them)
    /// - If omitted: defaults to `false` (symbol is in definitions → internal)
    #[serde(default)]
    pub is_external: bool,

    /// Documentation strings (for doc_score calculation)
    ///
    /// **Adapter Contract**:
    /// - Extract all documentation comments/docstrings
    /// - Order: most relevant first (e.g., primary docstring before inline comments)
    /// - Include full text (DocumentationScorer will parse)
    /// - For functions: include parameter descriptions, return descriptions
    /// - Empty vec if no documentation found
    pub documentation: Vec<String>,

    /// Symbol-specific details (selected based on `kind`)
    ///
    /// **Adapter Contract**:
    /// - Must match `kind`: Function→Function, Variable→Variable, Type→Type
    pub details: SymbolDetails,
}

/// Symbol kind - Three types only
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    /// Functions, methods, constructors
    /// Methods have enclosing_symbol pointing to their Type
    Function,

    /// Global variables and class/struct fields
    /// Fields have enclosing_symbol pointing to their Type
    /// Local variables are NOT extracted
    Variable,

    /// Type definitions: Class, Interface, Struct, Enum, etc.
    /// Methods are separate Function symbols, not embedded in Type
    Type,
}

/// Symbol-specific details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolDetails {
    Function(FunctionDetails),
    Variable(VariableDetails),
    Type(TypeDetails),
}

/// ============================================================================
/// Function Details
/// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FunctionDetails {
    /// Parameters (in definition order)
    ///
    /// **Adapter Contract**:
    /// - Include all parameters except `self`/`this`/`cls` (language-specific receiver)
    /// - Maintain declaration order
    /// - For each parameter, extract declared type from annotation if present
    /// - Set `param_type=None` if no type annotation (untyped parameter)
    /// - **NO type inference required**: only extract explicit annotations
    pub parameters: Vec<Parameter>,

    /// Return type references (declared types only)
    ///
    /// **Adapter Contract**:
    /// - Extract from explicit return type annotation
    /// - Empty vec if:
    ///   * No return type annotation (untyped function)
    ///   * Function returns void/None (language-dependent: some have explicit void type)
    /// - For union returns (`int | str`, `Result<T, E>`):
    ///   * Option 1: Multiple entries (e.g., ["int", "str"])
    ///   * Option 2: Single synthetic union type (e.g., ["int|str"])
    ///   * Adapter choice, but must be consistent
    /// - **NO type inference**: only explicit annotations
    pub return_types: Vec<TypeRef>,

    /// Generic type parameters
    ///
    /// **Adapter Contract**:
    /// - Extract generic parameters (e.g., `<T>`, `[T]`, `::<T>`)
    /// - Include bounds/constraints (e.g., `T: Display`, `T extends Base`)
    /// - For unbounded generics, `bounds` is empty vec
    pub type_params: Vec<TypeParam>,

    /// Source ranges forming the callable contract. Extractors should include
    /// signatures, contract-relevant decorators, and source documentation.
    #[serde(default)]
    pub surface_spans: Vec<SourceSpan>,

    /// Source ranges containing behavior beyond the callable contract.
    #[serde(default)]
    pub implementation_spans: Vec<SourceSpan>,

    /// Function modifiers and attributes
    pub modifiers: FunctionModifiers,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Parameter {
    pub name: String,

    /// Declared parameter type (from annotation)
    ///
    /// **Adapter Contract**:
    /// - Extract from explicit type annotation only
    /// - `None` if no annotation (untyped parameter)
    /// - **NO type inference from usage**: DI abstraction is preserved
    ///   * Example: `def f(reader: Reader)` → param_type = "Reader"
    ///   * Even if all callers pass `FileReader`, keep type as "Reader"
    ///   * This preserves interface abstraction in CF calculation
    /// - For generic types: use instantiated type if available (e.g., "List[int]")
    pub param_type: Option<TypeRef>,

    /// Whether this parameter uses a high-freedom type (e.g. dict, list, str, Any)
    /// High-freedom types require more documentation because they lack strong structural constraints.
    #[serde(default)]
    pub is_high_freedom_type: bool,

    /// Whether parameter has a default value
    ///
    /// **Adapter Contract**:
    /// - `true` if parameter has default value in signature
    /// - Used for API boundary detection (default params are part of public API)
    pub has_default: bool,

    /// Whether this is variadic (*args, ...rest, etc.)
    ///
    /// **Adapter Contract**:
    /// - `true` for variadic parameters (Python *args, TS ...rest, etc.)
    /// - For variadic params, `param_type` is the element type (if annotated)
    pub is_variadic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeParam {
    pub name: String,

    /// Type constraints (e.g., `T extends Base` → bounds = [Base])
    pub bounds: Vec<TypeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionModifiers {
    /// Whether function is async/awaitable
    ///
    /// **Adapter Contract**:
    /// - `true` for: async/await functions, coroutines, Promises, Futures
    /// - Affects: behavioral complexity (async functions are more complex)
    pub is_async: bool,

    /// Whether function is a generator/iterator
    ///
    /// **Adapter Contract**:
    /// - `true` for: Python generators (yield), JS generators, Rust iterators
    /// - Affects: behavioral complexity
    pub is_generator: bool,

    /// Whether function is static/class method
    ///
    /// **Adapter Contract**:
    /// - `true` for: static methods, class methods (not instance methods)
    /// - For static methods, `enclosing_symbol` still points to Type
    /// - Affects: access pattern analysis
    pub is_static: bool,

    /// Whether this is an abstract/interface method (signature only, no implementation)
    ///
    /// **Adapter Contract** (CRITICAL for CF boundary detection):
    /// - `true` if:
    ///   * Method in Interface/Protocol/Trait/Abstract Class with no body
    ///   * Python: `@abstractmethod` or Protocol method
    ///   * Java: interface method or abstract method
    ///   * Rust: trait method without default impl
    ///   * TypeScript: interface method
    /// - `false` if method has implementation body
    /// - When `true`, `span` should cover signature only (not body)
    /// - Abstract methods with good docs are CF boundaries
    pub is_abstract: bool,

    /// Whether this function is a type constructor (e.g. Python __init__).
    ///
    /// **Adapter Contract**: Set by language extractor. Constructors implicitly return
    /// the instance/None; extractor should set return type so signature is complete.
    #[serde(default)]
    pub is_constructor: bool,

    /// True if this function participates in DI wiring (e.g. FastAPI Depends(),
    /// or manually marked with `# cf:di_wired` pragma).
    /// When true and signature is complete, treated as CF boundary.
    #[serde(default)]
    pub is_di_wired: bool,

    /// Use only the function signature for context_size (exclude body).
    /// Set by extractors when the function is an "Annotated-style" documented factory
    /// (e.g. Python Body()/Query() with Doc() in params); full body is mostly docs
    /// and would inflate CF without adding understanding. When true, context_size
    /// is computed from the signature span only (same as is_abstract).
    #[serde(default)]
    pub use_signature_only_for_size: bool,

    pub visibility: Visibility,
}

impl Default for FunctionModifiers {
    fn default() -> Self {
        Self {
            is_async: false,
            is_generator: false,
            is_static: false,
            is_abstract: false,
            is_constructor: false,
            is_di_wired: false,
            use_signature_only_for_size: false,
            visibility: Visibility::Public,
        }
    }
}

/// ============================================================================
/// Variable Details
/// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDetails {
    /// Declared variable type
    ///
    /// **Adapter Contract**:
    /// - Extract from explicit type annotation
    /// - `None` if no annotation (untyped variable)
    /// - For fields: extract from class-level type annotation
    /// - **NO type inference from assignment**: keep declared type
    ///   * Example: `x: Base = Derived()` → var_type = "Base" (not "Derived")
    ///   * Preserves abstraction in DI scenarios
    pub var_type: Option<TypeRef>,

    /// Mutability (CRITICAL for SharedStateWrite edge detection)
    ///
    /// **Adapter Contract**:
    /// - **Const**: compile-time constant, immutable value
    ///   * Python: typing.Final at module level with literal value
    ///   * Java: `static final`
    ///   * Rust: `const`
    /// - **Immutable**: runtime immutable, cannot be reassigned
    ///   * Java: `final`
    ///   * Rust: `let` (non-mut)
    ///   * Python: typing.Final (runtime)
    ///   * TypeScript: `readonly`
    /// - **Mutable**: can be reassigned or mutated
    ///   * Default if no immutability marker
    ///   * Rust: `let mut`, `static mut`
    /// - **When in doubt, use Mutable** (conservative for CF)
    /// - Only Mutable variables trigger SharedStateWrite expansion
    pub mutability: Mutability,

    /// Variable scope (Global or Field only)
    ///
    /// **Adapter Contract**:
    /// - **Global**: module-level, package-level, or namespace-level variable
    /// - **Field**: class/struct field (must have `enclosing_symbol` pointing to Type)
    /// - **Local variables MUST NOT be extracted** (filtered at adapter)
    pub scope: VariableScope,

    pub visibility: Visibility,
}

impl Default for VariableDetails {
    fn default() -> Self {
        Self {
            var_type: None,
            mutability: Mutability::Mutable,
            scope: VariableScope::Global,
            visibility: Visibility::Public,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mutability {
    /// Compile-time constant (e.g., `const`, `final static`)
    Const,

    /// Runtime immutable (e.g., `final` in Java, `let` in Rust)
    Immutable,

    /// Mutable (triggers SharedStateWrite expansion in CF)
    Mutable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VariableScope {
    /// Module/global scope variable
    Global,

    /// Class/struct field
    /// Must have enclosing_symbol pointing to Type
    Field,
}

/// ============================================================================
/// Type Details
/// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDetails {
    /// Type classification
    ///
    /// **Adapter Contract**:
    /// - Map language-specific concepts:
    ///   * Interface: Java interface, Python Protocol, TS interface, Rust trait
    ///   * Class: Python class, Java class, TS class, Rust struct (with impl)
    ///   * Struct: Rust struct, C struct
    ///   * Enum: Rust enum, Java enum, TS enum
    /// - Choose based on primary purpose
    pub kind: TypeKind,

    /// Whether this is an abstract type (CRITICAL for CF boundary detection)
    ///
    /// **Adapter Contract**:
    /// - `true` if:
    ///   * Python: Protocol, ABC with @abstractmethod
    ///   * Java: interface, abstract class
    ///   * Rust: trait
    ///   * TypeScript: interface, abstract class
    /// - `false` if concrete class/struct with full implementation
    /// - Used for:
    ///   1. Determining if methods should have `is_abstract=true`
    ///   2. Abstract factory detection (function returning abstract type is boundary)
    ///   3. DI interface detection
    pub is_abstract: bool,

    /// Whether type is final/sealed (cannot be inherited)
    ///
    /// **Adapter Contract**:
    /// - `true` for: Java `final` class, Python `@final`, Rust non-pub struct
    /// - Affects: boundary decisions (final types are complete, no subclass expansion)
    pub is_final: bool,

    pub visibility: Visibility,

    /// Generic type parameters
    ///
    /// **Adapter Contract**:
    /// - Extract type parameters from type definition
    /// - Example: `class List<T>` → type_params = [TypeParam { name: "T", bounds: [] }]
    pub type_params: Vec<TypeParam>,

    /// Fields (data members)
    ///
    /// **Adapter Contract**:
    /// - Include all class/struct fields (instance and static)
    /// - Each field here should have a corresponding Variable definition:
    ///   * Variable with `kind=Variable`, `scope=Field`, `enclosing_symbol=this_type`
    ///   * Field.symbol_id = Variable.symbol_id
    /// - **Methods are NOT fields**: they are separate Function definitions
    /// - Extract field type from annotation if present
    pub fields: Vec<Field>,

    /// Type hierarchy - inheritance relationships
    ///
    /// **Adapter Contract**:
    /// - **inherits**: base classes, superclasses
    ///   * Python: `class A(B, C)` → inherits = ["B", "C"]
    ///   * Java: `class A extends B` → inherits = ["B"]
    /// - **implements**: interfaces, protocols, traits
    ///   * Python: use Protocol in inherits (no separate implements)
    ///   * Java: `class A implements I1, I2` → implements = ["I1", "I2"]
    ///   * Rust: trait impls are tracked separately (not in Type definition)
    /// - References to Type symbols (not builtin types typically)
    pub inherits: Vec<TypeRef>,
    pub implements: Vec<TypeRef>,
}

impl Default for TypeDetails {
    fn default() -> Self {
        Self {
            kind: TypeKind::Class,
            is_abstract: false,
            is_final: false,
            visibility: Visibility::Public,
            type_params: Vec::new(),
            fields: Vec::new(),
            inherits: Vec::new(),
            implements: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeKind {
    Class,
    Interface, // Java Interface, Python Protocol, TypeScript Interface, Rust Trait
    Struct,
    Enum,
    TypeAlias,
    Union,        // Union types (e.g., `int | str`)
    Intersection, // Intersection types (e.g., `A & B`)
    TypeVar,      // Generic type variable (e.g., T in List[T])
}

/// Field information
/// Note: Fields also exist as Variable definitions (with scope=Field)
/// This struct provides type-level view of the field
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub name: String,

    /// Field type reference
    pub field_type: Option<TypeRef>,

    pub mutability: Mutability,
    pub visibility: Visibility,

    /// Symbol ID of the corresponding Variable definition
    /// Allows linking field declaration to its Variable node
    pub symbol_id: SymbolId,
}

/// ============================================================================
/// Common Attributes
/// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    /// Public visibility (accessible from anywhere)
    ///
    /// **Adapter Contract**:
    /// - Python: no leading underscore, or explicitly public
    /// - Java: `public`
    /// - Rust: `pub`
    /// - TypeScript: `public` or default for exported symbols
    Public,

    /// Private visibility (accessible only within defining scope)
    ///
    /// **Adapter Contract**:
    /// - Python: leading underscore `_name` or `__name`
    /// - Java: `private`
    /// - Rust: no `pub` keyword
    /// - TypeScript: `private`
    Private,

    /// Protected visibility (accessible in subclasses)
    ///
    /// **Adapter Contract**:
    /// - Java: `protected`
    /// - TypeScript: `protected`
    /// - Python: single underscore by convention (also map to Private)
    Protected,

    /// Internal/package visibility
    ///
    /// **Adapter Contract**:
    /// - Java: package-private (no modifier)
    /// - Rust: `pub(crate)`
    /// - Kotlin: `internal`
    Internal,
}

/// ============================================================================
/// Symbol References (for edge construction)
/// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolReference {
    /// Referenced symbol ID (target of the reference)
    ///
    /// **Adapter Contract**:
    /// - `Some(id)`: matches a `symbol_id` in definitions when target is statically known
    /// - `None`: unresolved (e.g. dynamic dispatch, reflection); builder may recover via type propagation
    ///   *CRITICAL WARNING*: Builder's edge recovery ONLY works if `receiver` is a Field or Global variable.
    ///   For method calls on **local variables** or **parameters**, the Extractor MUST resolve `target_symbol`
    ///   itself, because locals/parameters are not graph nodes and cannot be recovered by the Builder!
    #[serde(default)]
    pub target_symbol: Option<SymbolId>,

    /// Location where reference occurs
    pub location: SourceLocation,

    /// Enclosing context (Function or module containing this reference)
    ///
    /// **Adapter Contract**:
    /// - For references inside functions: symbol_id of the Function
    /// - For references at module/file level: use module symbol or file-level synthetic symbol
    /// - Must be a Function or Module symbol
    pub enclosing_symbol: SymbolId,

    /// Reference role (determines edge type in graph)
    pub role: ReferenceRole,

    /// Receiver variable for member access (symbol_id of the variable/parameter)
    ///
    /// **Adapter Contract**:
    /// - `None`: Direct access (function call, global variable)
    /// - `Some(receiver_symbol_id)`: Member access; symbol_id of the variable (e.g. "self", or the var holding the object)
    /// - Builder uses this to resolve method calls via receiver's `var_type` when target_symbol is None
    ///   *NOTE*: If `receiver` points to a local variable or parameter, the Builder will ignore it during recovery.
    #[serde(default)]
    pub receiver: Option<SymbolId>,

    /// Method name for method calls (when target is resolved via receiver type)
    ///
    /// **Adapter Contract**:
    /// - For method calls: the name of the method being called (e.g. "foo", "__init__")
    /// - Used by builder for type-driven call edge recovery when target_symbol is None
    #[serde(default)]
    pub method_name: Option<String>,

    /// Variable that receives the result of this call (for Call references)
    ///
    /// **Adapter Contract**:
    /// - For Call: symbol_id of the variable assigned the return value (e.g. `x = foo()` → assigned_to = x's symbol_id)
    /// - Enables external call return type propagation in Pass 2.5
    /// - *NOTE*: If assigned to a local variable, leave as `None` (locals are not tracked).
    #[serde(default)]
    pub assigned_to: Option<SymbolId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceRole {
    /// Function call → Call edge in graph
    ///
    /// **Adapter Contract**:
    /// - Use for: function calls, method calls, constructor calls (including `new Class()` / `Class()`)
    /// - `target_symbol`: the Function symbol being called (for constructors, target the constructor function)
    /// - For method calls: set `receiver` to indicate instance/static access
    /// - Example: `obj.method()` → role=Call, receiver=Some("obj"), target=method_symbol
    Call,

    /// Variable read → Read edge in graph
    ///
    /// **Adapter Contract**:
    /// - Use for: reading variable value (not assignment)
    /// - `target_symbol`: the Variable symbol being read
    /// - For field access: set `receiver`
    /// - Example: `x = obj.field` → role=Read, receiver=Some("obj"), target=field_symbol
    /// - For immutable variables, Read edges don't cause expansion
    Read,

    /// Variable write → Write edge in graph
    ///
    /// **Adapter Contract**:
    /// - Use for: assignment, mutation
    /// - `target_symbol`: the Variable symbol being written
    /// - For field write: set `receiver`
    /// - Example: `obj.field = 1` → role=Write, receiver=Some("obj"), target=field_symbol
    /// - For mutable shared state, Write triggers SharedStateWrite expansion
    Write,

    /// Decorator/annotation application → Annotates edge in graph
    ///
    /// **Adapter Contract**:
    /// - Use for: Python decorators, Java annotations, TS decorators
    /// - `target_symbol`: the decorator Function
    /// - `enclosing_symbol`: the decorated Function (for class decorators, use __init__)
    Decorate,
}

/// Source location (single point in source code)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Relative path from project root
    ///
    /// **Adapter Contract**:
    /// - Must match `DocumentSemantics.relative_path`
    /// - Use forward slashes (/)
    pub file_path: String,

    /// 0-based line number
    ///
    /// **Adapter Contract**:
    /// - First line of file is line 0
    /// - Must match line numbering used in `SourceSpan`
    pub line: u32,

    /// 0-based column offset
    ///
    /// **Adapter Contract**:
    /// - First column is 0
    /// - Typically byte offset or character offset (be consistent)
    pub column: u32,
}

/// Source span (range in source code)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    /// 0-based start line (inclusive)
    pub start_line: u32,

    /// 0-based start column (inclusive)
    pub start_column: u32,

    /// 0-based end line (exclusive)
    ///
    /// **Adapter Contract**:
    /// - End position is EXCLUSIVE (does not include this line/column)
    /// - For single-line span: end_line = start_line + 1 or end_column marks end
    pub end_line: u32,

    /// 0-based end column (exclusive)
    pub end_column: u32,
}

impl SemanticData {
    /// Get all symbol definitions from all documents and external symbols
    pub fn all_definitions(&self) -> impl Iterator<Item = &SymbolDefinition> {
        self.documents
            .iter()
            .flat_map(|doc| doc.definitions.iter())
            .chain(self.external_symbols.iter())
    }

    /// Build enclosing map (symbol_id → parent_symbol_id)
    pub fn build_enclosing_map(&self) -> HashMap<SymbolId, SymbolId> {
        let mut map = HashMap::new();

        for def in self.all_definitions() {
            if let Some(parent) = &def.enclosing_symbol {
                map.insert(def.symbol_id.clone(), parent.clone());
            }
        }

        map
    }

    /// Find definition by symbol ID
    pub fn find_definition(&self, symbol_id: &str) -> Option<&SymbolDefinition> {
        self.all_definitions()
            .find(|def| def.symbol_id == symbol_id)
    }
}

impl SymbolDefinition {
    /// Check if this is a method (Function with Type enclosing)
    pub fn is_method(&self) -> bool {
        if self.kind != SymbolKind::Function {
            return false;
        }

        self.enclosing_symbol.is_some()
    }

    /// Check if this is a field (Variable with Type enclosing)
    pub fn is_field(&self) -> bool {
        if self.kind != SymbolKind::Variable {
            return false;
        }

        matches!(
            &self.details,
            SymbolDetails::Variable(var) if var.scope == VariableScope::Field
        )
    }

    /// Get function details (convenience accessor)
    pub fn as_function(&self) -> Option<&FunctionDetails> {
        match &self.details {
            SymbolDetails::Function(f) => Some(f),
            _ => None,
        }
    }

    /// Get variable details (convenience accessor)
    pub fn as_variable(&self) -> Option<&VariableDetails> {
        match &self.details {
            SymbolDetails::Variable(v) => Some(v),
            _ => None,
        }
    }

    /// Get type details (convenience accessor)
    pub fn as_type(&self) -> Option<&TypeDetails> {
        match &self.details {
            SymbolDetails::Type(t) => Some(t),
            _ => None,
        }
    }
}

impl FunctionDetails {
    /// Count parameters with type annotations
    pub fn typed_param_count(&self) -> usize {
        self.parameters
            .iter()
            .filter(|p| p.param_type.is_some())
            .count()
    }

    /// Total parameter count
    pub fn param_count(&self) -> usize {
        self.parameters.len()
    }

    /// Check if function has return type annotation
    pub fn has_return_type(&self) -> bool {
        !self.return_types.is_empty()
    }

    /// Check if signature is complete (all params typed + has return type)
    pub fn is_signature_complete(&self) -> bool {
        self.typed_param_count() == self.param_count() && self.has_return_type()
    }
}
