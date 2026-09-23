"""
Pydantic models mirroring the SemanticData schema from src/domain/semantic.rs.
Output JSON must be consumable by the Rust GraphBuilder.
Rust serde serializes enum SymbolDetails as externally tagged: {"Function": {...}} etc.
"""

from enum import Enum
from typing import Any, Literal, Optional

from pydantic import BaseModel, Field as PydanticField, model_serializer


# --- Enums (serialize as PascalCase to match Rust serde default) ---


class SymbolKind(str, Enum):
    Function = "Function"
    Variable = "Variable"
    Type = "Type"


class Visibility(str, Enum):
    Public = "Public"
    Private = "Private"
    Protected = "Protected"
    Internal = "Internal"


class Mutability(str, Enum):
    Const = "Const"
    Immutable = "Immutable"
    Mutable = "Mutable"


class VariableScope(str, Enum):
    Global = "Global"
    Field = "Field"


class ReferenceRole(str, Enum):
    Call = "Call"
    Read = "Read"
    Write = "Write"
    Decorate = "Decorate"


class TypeKind(str, Enum):
    Class = "Class"
    Interface = "Interface"
    Struct = "Struct"
    Enum = "Enum"
    TypeAlias = "TypeAlias"
    Union = "Union"
    Intersection = "Intersection"
    TypeVar = "TypeVar"


# --- Common ---

SymbolId = str
TypeRef = str


class SourceLocation(BaseModel):
    file_path: str
    line: int = PydanticField(..., description="0-based line number")
    column: int = PydanticField(..., description="0-based column offset")


class SourceSpan(BaseModel):
    start_line: int
    start_column: int
    end_line: int  # exclusive
    end_column: int  # exclusive


# --- Function details ---


class Parameter(BaseModel):
    name: str
    param_type: Optional[TypeRef] = None
    is_high_freedom_type: bool = False
    has_default: bool = False
    is_variadic: bool = False


class TypeParam(BaseModel):
    name: str
    bounds: list[TypeRef] = PydanticField(default_factory=list)


class FunctionModifiers(BaseModel):
    is_async: bool = False
    is_generator: bool = False
    is_static: bool = False
    is_abstract: bool = False
    is_constructor: bool = False
    is_di_wired: bool = False
    use_signature_only_for_size: bool = False
    visibility: Visibility = Visibility.Public


class FunctionDetails(BaseModel):
    parameters: list[Parameter] = PydanticField(default_factory=list)
    return_types: list[TypeRef] = PydanticField(default_factory=list)
    type_params: list[TypeParam] = PydanticField(default_factory=list)
    surface_spans: list[SourceSpan] = PydanticField(default_factory=list)
    implementation_spans: list[SourceSpan] = PydanticField(default_factory=list)
    modifiers: FunctionModifiers = PydanticField(default_factory=FunctionModifiers)


# --- Variable details ---


class VariableDetails(BaseModel):
    var_type: Optional[TypeRef] = None
    mutability: Mutability = Mutability.Mutable
    scope: VariableScope = VariableScope.Global
    visibility: Visibility = Visibility.Public


# --- Type details ---


class TypeField(BaseModel):
    """A field of a type (class/struct). Named TypeField to avoid shadowing pydantic.Field."""
    name: str
    field_type: Optional[TypeRef] = None
    mutability: Mutability = Mutability.Mutable
    visibility: Visibility = Visibility.Public
    symbol_id: SymbolId


class TypeDetails(BaseModel):
    kind: TypeKind = TypeKind.Class
    is_abstract: bool = False
    is_final: bool = False
    visibility: Visibility = Visibility.Public
    type_params: list[TypeParam] = PydanticField(default_factory=list)
    fields: list[TypeField] = PydanticField(default_factory=list)
    inherits: list[TypeRef] = PydanticField(default_factory=list)
    implements: list[TypeRef] = PydanticField(default_factory=list)


# --- Symbol definition (discriminated union for details) ---


class SymbolDefinition(BaseModel):
    symbol_id: SymbolId
    kind: SymbolKind
    name: str
    display_name: str
    location: SourceLocation
    span: SourceSpan
    enclosing_symbol: Optional[SymbolId] = None
    is_external: bool = False
    documentation: list[str] = PydanticField(default_factory=list)
    details: FunctionDetails | VariableDetails | TypeDetails

    @model_serializer(mode="plain")
    def _serialize_for_rust(self) -> dict[str, Any]:
        """Emit details as Rust-externally-tagged enum: {"Function": {...}} etc.
        Build dict manually to avoid recursion (model_dump would re-invoke this serializer).
        """
        def _serialize_value(v: Any) -> Any:
            if isinstance(v, BaseModel):
                return v.model_dump(mode="json")
            if isinstance(v, list):
                return [_serialize_value(item) for item in v]
            if isinstance(v, Enum):
                return v.value
            return v

        d: dict[str, Any] = {}
        for name in self.__class__.model_fields:
            if name == "details":
                det = self.details
                if isinstance(det, FunctionDetails):
                    d["details"] = {"Function": det.model_dump(mode="json")}
                elif isinstance(det, VariableDetails):
                    d["details"] = {"Variable": det.model_dump(mode="json")}
                else:
                    d["details"] = {"Type": det.model_dump(mode="json")}
            else:
                d[name] = _serialize_value(getattr(self, name))
        return d


# --- Symbol reference ---


class SymbolReference(BaseModel):
    target_symbol: Optional[SymbolId] = None
    location: SourceLocation
    enclosing_symbol: SymbolId
    role: ReferenceRole
    receiver: Optional[SymbolId] = None
    method_name: Optional[str] = None
    assigned_to: Optional[SymbolId] = None


# --- Document and top-level ---


class DocumentSemantics(BaseModel):
    relative_path: str
    language: str = "python"
    definitions: list[SymbolDefinition] = PydanticField(default_factory=list)
    references: list[SymbolReference] = PydanticField(default_factory=list)


class SemanticData(BaseModel):
    project_root: str
    documents: list[DocumentSemantics] = PydanticField(default_factory=list)
    external_symbols: list[SymbolDefinition] = PydanticField(default_factory=list)

    def model_dump_json(self, **kwargs: Any) -> str:
        return super().model_dump_json(**kwargs)
