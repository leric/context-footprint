//! Type Registry - stores type definitions outside of the graph
//!
//! Types are no longer nodes in the graph. Instead, they are stored in a separate
//! registry that can be queried during traversal for type-related information.

use crate::domain::node::ContextFragment;
use std::collections::HashMap;

/// Type kind - language-agnostic classification for abstract types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Interface, // Java/Go/TypeScript Interface, Python/Swift Protocol, Rust Trait
    Struct,
    Enum,
    TypeAlias,    // type UserId = string
    FunctionType, // (int, int) -> bool
    Union,        // A | B
    Intersection, // A & B
    TypeVar,      // T, U (generic type parameters)
}

/// Information about a type variable (generic type parameter)
#[derive(Debug, Clone)]
pub struct TypeVarInfo {
    pub bound: Option<TypeId>,
    pub constraints: Vec<TypeId>,
}

impl TypeVarInfo {
    pub fn is_effectively_typed(&self) -> bool {
        self.bound.is_some() || !self.constraints.is_empty()
    }
}

/// Type definition attributes (stored in TypeRegistry, not in graph nodes)
#[derive(Debug, Clone)]
pub struct TypeDefAttribute {
    pub type_kind: TypeKind,
    pub is_abstract: bool,
    pub type_param_count: u32,
    pub type_var_info: Option<TypeVarInfo>,
}

/// Type identifier (symbol string)
pub type TypeId = String;

/// Type information stored in the registry
#[derive(Debug, Clone)]
pub struct TypeInfo {
    /// The type definition attributes
    pub definition: TypeDefAttribute,
    /// Context size of the type definition (tokens needed to understand the type)
    pub context_size: u32,
    /// Surface that must be loaded when the type participates in a contract.
    pub surface_fragment: ContextFragment,
    /// Documentation score of the type
    pub doc_score: f32,
    pub documentation: Vec<String>,
}

impl TypeInfo {
    pub fn is_type_var(&self) -> bool {
        self.definition.type_kind == TypeKind::TypeVar
    }
}

/// Type Registry - stores all type definitions
#[derive(Debug, Default)]
pub struct TypeRegistry {
    types: HashMap<TypeId, TypeInfo>,
    implementors: HashMap<TypeId, Vec<TypeId>>,
}

impl TypeRegistry {
    /// Create a new empty type registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new type
    pub fn register(&mut self, type_id: TypeId, info: TypeInfo) {
        self.types.insert(type_id, info);
    }

    /// Get type information by ID
    pub fn get(&self, type_id: &str) -> Option<&TypeInfo> {
        self.types.get(type_id)
    }

    /// Check if a type exists in the registry
    pub fn contains(&self, type_id: &str) -> bool {
        self.types.contains_key(type_id)
    }

    /// Get all type IDs
    pub fn type_ids(&self) -> impl Iterator<Item = &TypeId> {
        self.types.keys()
    }

    /// Get mutable reference to type info
    pub fn get_mut(&mut self, type_id: &str) -> Option<&mut TypeInfo> {
        self.types.get_mut(type_id)
    }

    /// Get count of registered types
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn register_implementor(&mut self, interface_id: TypeId, concrete_id: TypeId) {
        self.implementors
            .entry(interface_id)
            .or_default()
            .push(concrete_id);
    }

    pub fn get_implementors(&self, interface_id: &str) -> Option<&Vec<TypeId>> {
        self.implementors.get(interface_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_type_info() -> TypeInfo {
        TypeInfo {
            definition: TypeDefAttribute {
                type_kind: TypeKind::Class,
                is_abstract: false,
                type_param_count: 0,
                type_var_info: None,
            },
            context_size: 100,
            surface_fragment: ContextFragment::new(
                "type:test:surface".to_string(),
                None,
                None,
                100,
            ),
            doc_score: 0.8,
            documentation: Vec::new(),
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = TypeRegistry::new();
        let type_id = "MyClass#".to_string();
        let info = test_type_info();

        registry.register(type_id.clone(), info);

        let retrieved = registry.get(&type_id).unwrap();
        assert_eq!(retrieved.context_size, 100);
        assert!(!retrieved.definition.is_abstract);
    }

    #[test]
    fn test_contains() {
        let mut registry = TypeRegistry::new();
        registry.register("TypeA#".to_string(), test_type_info());

        assert!(registry.contains("TypeA#"));
        assert!(!registry.contains("TypeB#"));
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let registry = TypeRegistry::new();
        assert!(registry.get("NonExistent#").is_none());
    }
}
