//! In-memory representation of parsed XSD schema elements.
//!
//! Defines the types representing elements, types, and constraints extracted
//! from the Universal Command and Control Interface (UCI) schema files.

use std::collections::HashMap;

use crate::facets::CompiledFacets;

// ---------------------------------------------------------------------------
// Core schema model types
// ---------------------------------------------------------------------------

/// Parsed XSD schema — the in-memory representation used for validation.
#[derive(Debug)]
pub struct Schema {
    /// The `targetNamespace` of the schema, if declared.
    pub target_namespace: Option<String>,
    /// The `version` attribute of the schema, if declared.
    pub version: Option<String>,
    /// Top-level `xs:element` declarations keyed by element name.
    pub global_elements: HashMap<String, GlobalElement>,
    /// Named `xs:complexType` definitions keyed by type name.
    pub complex_types: HashMap<String, ComplexType>,
    /// Named `xs:simpleType` definitions (including XSD built-ins) keyed by type name.
    pub simple_types: HashMap<String, SimpleType>,
    /// Precompiled facet constraints keyed by simple type name.
    pub compiled_facets: HashMap<String, CompiledFacets>,
}

/// A top-level `xs:element` declaration (a message root).
#[derive(Debug, Clone)]
pub struct GlobalElement {
    /// The type this element references (complex or simple type name).
    pub type_name: String,
}

/// A named `xs:complexType` definition.
#[derive(Debug, Clone)]
pub struct ComplexType {
    /// Type name (empty string for anonymous types).
    pub name: String,
    /// Whether this type is declared `abstract="true"`.
    pub abstract_: bool,
    /// The content model (sequence, choice, extension, or empty).
    pub content: ComplexContent,
}

/// The content model of a complex type.
#[derive(Debug, Clone)]
pub enum ComplexContent {
    /// No child elements (empty content).
    Empty,
    /// `xs:sequence` containing child elements.
    Sequence { elements: Vec<Element> },
    /// `xs:choice` containing child elements.
    Choice { elements: Vec<Element> },
    /// `xs:complexContent/xs:extension` — inherits from a base type.
    Extension {
        base: String,
        content: Option<Box<ExtensionContent>>,
    },
}

/// Content added by an extension (inside `xs:complexContent/xs:extension`).
#[derive(Debug, Clone)]
pub enum ExtensionContent {
    /// Extension adds a `xs:sequence`.
    Sequence { elements: Vec<Element> },
    /// Extension adds a `xs:choice`.
    Choice { elements: Vec<Element> },
}

/// A local `xs:element` declaration within a complex type.
#[derive(Debug, Clone)]
pub struct Element {
    /// Element name as it appears in JSON keys.
    pub name: String,
    /// Type of this element (named reference or inline anonymous type).
    pub type_ref: TypeRef,
    /// Minimum number of occurrences (`minOccurs`, default 1).
    pub min_occurs: u32,
    /// Maximum number of occurrences (`maxOccurs`, default 1).
    pub max_occurs: MaxOccurs,
}

/// How an element references its type.
#[derive(Debug, Clone)]
pub enum TypeRef {
    /// References a named type (complex or simple) by name.
    Named(String),
    /// Inline anonymous complex type.
    AnonymousComplex(Box<ComplexType>),
    /// Inline anonymous simple type.
    AnonymousSimple(Box<SimpleType>),
}

/// Upper bound on element occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaxOccurs {
    /// A finite upper bound.
    Bounded(u32),
    /// `maxOccurs="unbounded"`.
    Unbounded,
}

/// A named or anonymous `xs:simpleType`.
#[derive(Debug, Clone)]
pub struct SimpleType {
    /// Type name (`None` for anonymous inline types).
    pub name: Option<String>,
    /// The restriction that defines this type's base and facets.
    pub restriction: Restriction,
}

/// `xs:restriction` on a simple type.
#[derive(Debug, Clone)]
pub struct Restriction {
    /// Base type this restriction derives from (e.g., `"xs:string"`).
    pub base: String,
    /// Facet constraints (enumeration, pattern, min/max, etc.).
    pub facets: Vec<Facet>,
}

/// XSD facet constraint on a simple type restriction.
#[derive(Debug, Clone)]
pub enum Facet {
    Enumeration(String),
    Pattern(String),
    MinInclusive(String),
    MaxInclusive(String),
    MinExclusive(String),
    MaxExclusive(String),
    MinLength(u32),
    MaxLength(u32),
    Length(u32),
    TotalDigits(u32),
    FractionDigits(u32),
    PatternGroup(Vec<String>),
}

impl Facet {
    /// Return the facet value as a string for diagnostics.
    pub fn value_as_str(&self) -> String {
        match self {
            Self::Enumeration(v)
            | Self::Pattern(v)
            | Self::MinInclusive(v)
            | Self::MaxInclusive(v)
            | Self::MinExclusive(v)
            | Self::MaxExclusive(v) => v.clone(),
            Self::MinLength(n)
            | Self::MaxLength(n)
            | Self::Length(n)
            | Self::TotalDigits(n)
            | Self::FractionDigits(n) => n.to_string(),
            Self::PatternGroup(patterns) => format!("group({})", patterns.join(" | ")),
        }
    }
}

// ---------------------------------------------------------------------------
// XSD built-in type hierarchy
// ---------------------------------------------------------------------------

/// Known XSD built-in primitive type names (with `xs:` prefix).
const XSD_PRIMITIVES: &[&str] = &[
    "xs:string",
    "xs:boolean",
    "xs:decimal",
    "xs:float",
    "xs:double",
    "xs:hexBinary",
    "xs:base64Binary",
    "xs:dateTime",
    "xs:date",
    "xs:time",
    "xs:duration",
    "xs:gYear",
    "xs:gYearMonth",
    "xs:gMonth",
    "xs:gMonthDay",
    "xs:gDay",
    "xs:anyURI",
    "xs:anySimpleType",
    "xs:QName",
    "xs:NOTATION",
];

/// XSD built-in derived types and what they restrict (child → parent).
const XSD_DERIVED: &[(&str, &str)] = &[
    // integer branch
    ("xs:integer", "xs:decimal"),
    ("xs:nonPositiveInteger", "xs:integer"),
    ("xs:negativeInteger", "xs:nonPositiveInteger"),
    ("xs:long", "xs:integer"),
    ("xs:int", "xs:long"),
    ("xs:short", "xs:int"),
    ("xs:byte", "xs:short"),
    ("xs:nonNegativeInteger", "xs:integer"),
    ("xs:positiveInteger", "xs:nonNegativeInteger"),
    ("xs:unsignedLong", "xs:nonNegativeInteger"),
    ("xs:unsignedInt", "xs:unsignedLong"),
    ("xs:unsignedShort", "xs:unsignedInt"),
    ("xs:unsignedByte", "xs:unsignedShort"),
    // string derivatives
    ("xs:normalizedString", "xs:string"),
    ("xs:token", "xs:normalizedString"),
    ("xs:language", "xs:token"),
    ("xs:Name", "xs:token"),
    ("xs:NMTOKEN", "xs:token"),
    ("xs:NCName", "xs:Name"),
    ("xs:ID", "xs:NCName"),
    ("xs:IDREF", "xs:NCName"),
];

// ---------------------------------------------------------------------------
// Schema construction
// ---------------------------------------------------------------------------

impl Schema {
    /// Create a new empty schema with XSD built-in types pre-populated.
    pub fn new() -> Self {
        let mut simple_types = HashMap::new();

        // Register XSD primitives as simple types with self-referencing restriction.
        for &prim in XSD_PRIMITIVES {
            simple_types.insert(
                prim.to_owned(),
                SimpleType {
                    name: Some(prim.to_owned()),
                    restriction: Restriction {
                        base: prim.to_owned(),
                        facets: Vec::new(),
                    },
                },
            );
        }

        // Register derived built-in types with their parent.
        for &(derived, base) in XSD_DERIVED {
            simple_types.insert(
                derived.to_owned(),
                SimpleType {
                    name: Some(derived.to_owned()),
                    restriction: Restriction {
                        base: base.to_owned(),
                        facets: Vec::new(),
                    },
                },
            );
        }

        Self {
            target_namespace: None,
            version: None,
            global_elements: HashMap::new(),
            complex_types: HashMap::new(),
            simple_types,
            compiled_facets: HashMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Query functions
    // -----------------------------------------------------------------------

    /// Walk the restriction chain from a simple type to its XSD built-in
    /// primitive. Returns the primitive name (e.g., `"xs:decimal"`).
    ///
    /// Returns `None` if the type is not found or the chain is broken.
    pub fn resolve_primitive(&self, type_name: &str) -> Option<String> {
        let mut current = type_name.to_owned();
        // Guard against infinite loops (shouldn't happen but defensive).
        let mut depth = 0;
        let max_depth = 50;

        loop {
            if depth > max_depth {
                return None;
            }
            depth += 1;

            let st = self.simple_types.get(&current)?;

            // If the base is the same as the current name, it's a primitive.
            if st.restriction.base == current {
                return Some(current);
            }

            current.clone_from(&st.restriction.base);
        }
    }

    /// Collect all effective facets for a simple type by walking its
    /// restriction chain.
    ///
    /// Inheritance rules:
    /// - Single-valued facets (numeric bounds, length, digits) are overridden
    ///   by descendants.
    /// - `enumeration` uses the most-derived list.
    /// - `pattern` facets accumulate across all levels.
    pub fn collect_facet_chain(&self, type_name: &str) -> Vec<Facet> {
        let mut chain = Vec::new();
        let mut current = type_name;
        let mut depth = 0;
        let max_depth = 50;

        while depth <= max_depth {
            let Some(st) = self.simple_types.get(current) else {
                break;
            };
            chain.push(st);

            let next = st.restriction.base.as_str();
            if next.starts_with("xs:") || next == current {
                break;
            }

            current = next;
            depth += 1;
        }

        if chain.is_empty() {
            return Vec::new();
        }

        let mut enumerations: Option<Vec<String>> = None;
        let mut patterns = Vec::new();
        let mut min_inclusive = None;
        let mut max_inclusive = None;
        let mut min_exclusive = None;
        let mut max_exclusive = None;
        let mut min_length = None;
        let mut max_length = None;
        let mut length = None;
        let mut total_digits = None;
        let mut fraction_digits = None;

        for st in chain.into_iter().rev() {
            let mut level_enumerations = Vec::new();
            let mut level_patterns = Vec::new();

            for facet in &st.restriction.facets {
                match facet {
                    Facet::Enumeration(value) => level_enumerations.push(value.clone()),
                    Facet::Pattern(value) => level_patterns.push(value.clone()),
                    Facet::MinInclusive(value) => {
                        min_inclusive = Some(Facet::MinInclusive(value.clone()));
                    }
                    Facet::MaxInclusive(value) => {
                        max_inclusive = Some(Facet::MaxInclusive(value.clone()));
                    }
                    Facet::MinExclusive(value) => {
                        min_exclusive = Some(Facet::MinExclusive(value.clone()));
                    }
                    Facet::MaxExclusive(value) => {
                        max_exclusive = Some(Facet::MaxExclusive(value.clone()));
                    }
                    Facet::MinLength(value) => min_length = Some(Facet::MinLength(*value)),
                    Facet::MaxLength(value) => max_length = Some(Facet::MaxLength(*value)),
                    Facet::Length(value) => length = Some(Facet::Length(*value)),
                    Facet::TotalDigits(value) => total_digits = Some(Facet::TotalDigits(*value)),
                    Facet::FractionDigits(value) => {
                        fraction_digits = Some(Facet::FractionDigits(*value));
                    }
                    Facet::PatternGroup(_) => {} // PatternGroups are only synthesized during collection
                }
            }

            if !level_enumerations.is_empty() {
                enumerations = Some(level_enumerations);
            }
            if !level_patterns.is_empty() {
                patterns.push(Facet::PatternGroup(level_patterns));
            }
        }

        let mut facets = Vec::new();
        if let Some(values) = enumerations {
            facets.extend(values.into_iter().map(Facet::Enumeration));
        }
        facets.extend(patterns);
        facets.extend(min_inclusive);
        facets.extend(max_inclusive);
        facets.extend(min_exclusive);
        facets.extend(max_exclusive);
        facets.extend(min_length);
        facets.extend(max_length);
        facets.extend(length);
        facets.extend(total_digits);
        facets.extend(fraction_digits);

        facets
    }

    /// Compile all effective facets for named user-defined simple types.
    pub fn compile_facets(&mut self) {
        self.compiled_facets.clear();

        let simple_type_names: Vec<String> = self
            .simple_types
            .keys()
            .filter(|name| !name.starts_with("xs:"))
            .cloned()
            .collect();

        for name in simple_type_names {
            let chain = self.collect_facet_chain(&name);
            if chain.is_empty() {
                continue;
            }

            let (compiled, warnings) = CompiledFacets::compile(&chain);
            for warning in warnings {
                tracing::warn!(type_name = %name, warning = %warning, "facet compile warning");
            }

            if compiled.has_constraints() {
                self.compiled_facets.insert(name, compiled);
            }
        }
    }

    /// Resolve extension inheritance into a flat list of element names.
    ///
    /// For a type with `ComplexContent::Extension`, recursively collects
    /// base type elements first, then extension elements.
    ///
    /// Returns `None` if the type is not found.
    pub fn flatten_elements(&self, complex_type_name: &str) -> Option<Vec<&Element>> {
        let ct = self.complex_types.get(complex_type_name)?;
        Some(self.flatten_elements_inner(ct))
    }

    fn flatten_elements_inner<'a>(&'a self, ct: &'a ComplexType) -> Vec<&'a Element> {
        match &ct.content {
            ComplexContent::Empty => Vec::new(),
            ComplexContent::Sequence { elements } | ComplexContent::Choice { elements } => {
                elements.iter().collect()
            }
            ComplexContent::Extension { base, content } => {
                let mut result = self
                    .complex_types
                    .get(base)
                    .map_or_else(Vec::new, |base_ct| self.flatten_elements_inner(base_ct));

                if let Some(ext_content) = content {
                    match ext_content.as_ref() {
                        ExtensionContent::Sequence { elements }
                        | ExtensionContent::Choice { elements } => {
                            result.extend(elements.iter());
                        }
                    }
                }

                result
            }
        }
    }

    /// Check whether `candidate` is derived from `base` by walking the
    /// extension chain.
    pub fn is_derived_from(&self, candidate: &str, base: &str) -> bool {
        if candidate == base {
            return false;
        }

        let mut current = candidate;
        let mut depth = 0;
        let max_depth = 50;

        loop {
            if depth > max_depth {
                return false;
            }
            depth += 1;

            let Some(ct) = self.complex_types.get(current) else {
                return false;
            };

            match &ct.content {
                ComplexContent::Extension { base: ext_base, .. } => {
                    if ext_base == base {
                        return true;
                    }
                    current = ext_base;
                }
                _ => return false,
            }
        }
    }

    /// Check if `name` is a known global element.
    pub fn is_global_element(&self, name: &str) -> bool {
        self.global_elements.contains_key(name)
    }

    /// Count the total number of facet constraints across all simple types.
    ///
    /// This also validates that all facets have non-empty values.
    pub fn facet_count(&self) -> usize {
        self.simple_types
            .values()
            .flat_map(|st| &st.restriction.facets)
            .inspect(|f| debug_assert!(!f.value_as_str().is_empty()))
            .count()
    }
}

impl Default for Schema {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn elem(name: &str, type_name: &str, min: u32, max: MaxOccurs) -> Element {
        Element {
            name: name.to_owned(),
            type_ref: TypeRef::Named(type_name.to_owned()),
            min_occurs: min,
            max_occurs: max,
        }
    }

    fn add_complex_types(schema: &mut Schema) {
        schema.complex_types.insert(
            "BaseType".to_owned(),
            ComplexType {
                name: "BaseType".to_owned(),
                abstract_: false,
                content: ComplexContent::Sequence {
                    elements: vec![
                        elem("field_a", "xs:string", 1, MaxOccurs::Bounded(1)),
                        elem("field_b", "xs:int", 0, MaxOccurs::Bounded(1)),
                    ],
                },
            },
        );
        schema.complex_types.insert(
            "DerivedType".to_owned(),
            ComplexType {
                name: "DerivedType".to_owned(),
                abstract_: false,
                content: ComplexContent::Extension {
                    base: "BaseType".to_owned(),
                    content: Some(Box::new(ExtensionContent::Sequence {
                        elements: vec![elem("field_c", "xs:boolean", 1, MaxOccurs::Bounded(1))],
                    })),
                },
            },
        );
        schema.complex_types.insert(
            "GrandchildType".to_owned(),
            ComplexType {
                name: "GrandchildType".to_owned(),
                abstract_: false,
                content: ComplexContent::Extension {
                    base: "DerivedType".to_owned(),
                    content: Some(Box::new(ExtensionContent::Sequence {
                        elements: vec![elem("field_d", "xs:double", 0, MaxOccurs::Unbounded)],
                    })),
                },
            },
        );
        schema.complex_types.insert(
            "EmptyDerived".to_owned(),
            ComplexType {
                name: "EmptyDerived".to_owned(),
                abstract_: false,
                content: ComplexContent::Extension {
                    base: "BaseType".to_owned(),
                    content: None,
                },
            },
        );
        schema.complex_types.insert(
            "AbstractBase".to_owned(),
            ComplexType {
                name: "AbstractBase".to_owned(),
                abstract_: true,
                content: ComplexContent::Sequence {
                    elements: vec![elem("id", "xs:string", 1, MaxOccurs::Bounded(1))],
                },
            },
        );
    }

    /// Build a schema with a variety of types for testing query functions.
    fn test_schema() -> Schema {
        let mut schema = Schema::new();
        add_complex_types(&mut schema);

        schema.global_elements.insert(
            "TestMessage".to_owned(),
            GlobalElement {
                type_name: "DerivedType".to_owned(),
            },
        );
        schema.simple_types.insert(
            "CustomInt".to_owned(),
            SimpleType {
                name: Some("CustomInt".to_owned()),
                restriction: Restriction {
                    base: "xs:unsignedByte".to_owned(),
                    facets: vec![Facet::MinInclusive("0".to_owned())],
                },
            },
        );
        schema.simple_types.insert(
            "IdentifierType".to_owned(),
            SimpleType {
                name: Some("IdentifierType".to_owned()),
                restriction: Restriction {
                    base: "xs:string".to_owned(),
                    facets: vec![Facet::Pattern("[A-Za-z]+".to_owned())],
                },
            },
        );
        schema
    }

    // -- resolve_primitive --

    #[test]
    fn resolve_primitive_xs_string() {
        let schema = test_schema();
        assert_eq!(schema.resolve_primitive("xs:string").unwrap(), "xs:string");
    }

    #[test]
    fn resolve_primitive_xs_decimal() {
        let schema = test_schema();
        assert_eq!(
            schema.resolve_primitive("xs:decimal").unwrap(),
            "xs:decimal"
        );
    }

    #[test]
    fn resolve_primitive_xs_boolean() {
        let schema = test_schema();
        assert_eq!(
            schema.resolve_primitive("xs:boolean").unwrap(),
            "xs:boolean"
        );
    }

    #[test]
    fn resolve_primitive_xs_double() {
        let schema = test_schema();
        assert_eq!(schema.resolve_primitive("xs:double").unwrap(), "xs:double");
    }

    #[test]
    fn resolve_primitive_custom_int_to_decimal() {
        let schema = test_schema();
        // CustomInt → xs:unsignedByte → xs:unsignedShort → xs:unsignedInt
        //           → xs:unsignedLong → xs:nonNegativeInteger → xs:integer → xs:decimal
        assert_eq!(schema.resolve_primitive("CustomInt").unwrap(), "xs:decimal");
    }

    #[test]
    fn resolve_primitive_identifier_to_string() {
        let schema = test_schema();
        assert_eq!(
            schema.resolve_primitive("IdentifierType").unwrap(),
            "xs:string"
        );
    }

    #[test]
    fn resolve_primitive_unknown_returns_none() {
        let schema = test_schema();
        assert!(schema.resolve_primitive("NonexistentType").is_none());
    }

    // -- flatten_elements --

    #[test]
    fn flatten_elements_simple_sequence() {
        let schema = test_schema();
        let elems = schema.flatten_elements("BaseType").unwrap();
        let names: Vec<&str> = elems.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["field_a", "field_b"]);
    }

    #[test]
    fn flatten_elements_extension_merges() {
        let schema = test_schema();
        let elems = schema.flatten_elements("DerivedType").unwrap();
        let names: Vec<&str> = elems.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["field_a", "field_b", "field_c"]);
    }

    #[test]
    fn flatten_elements_multi_level_inheritance() {
        let schema = test_schema();
        let elems = schema.flatten_elements("GrandchildType").unwrap();
        let names: Vec<&str> = elems.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["field_a", "field_b", "field_c", "field_d"]);
    }

    #[test]
    fn flatten_elements_empty_extension_returns_base() {
        let schema = test_schema();
        let elems = schema.flatten_elements("EmptyDerived").unwrap();
        let names: Vec<&str> = elems.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["field_a", "field_b"]);
    }

    #[test]
    fn flatten_elements_unknown_returns_none() {
        let schema = test_schema();
        assert!(schema.flatten_elements("Nonexistent").is_none());
    }

    // -- is_derived_from --

    #[test]
    fn is_derived_from_direct() {
        let schema = test_schema();
        assert!(schema.is_derived_from("DerivedType", "BaseType"));
    }

    #[test]
    fn is_derived_from_transitive() {
        let schema = test_schema();
        assert!(schema.is_derived_from("GrandchildType", "BaseType"));
        assert!(schema.is_derived_from("GrandchildType", "DerivedType"));
    }

    #[test]
    fn is_derived_from_same_type_is_false() {
        let schema = test_schema();
        assert!(!schema.is_derived_from("BaseType", "BaseType"));
    }

    #[test]
    fn is_derived_from_unrelated_is_false() {
        let schema = test_schema();
        assert!(!schema.is_derived_from("BaseType", "DerivedType"));
        assert!(!schema.is_derived_from("AbstractBase", "BaseType"));
    }

    #[test]
    fn is_derived_from_unknown_is_false() {
        let schema = test_schema();
        assert!(!schema.is_derived_from("Nonexistent", "BaseType"));
    }

    // -- is_global_element --

    #[test]
    fn is_global_element_known() {
        let schema = test_schema();
        assert!(schema.is_global_element("TestMessage"));
    }

    #[test]
    fn is_global_element_unknown() {
        let schema = test_schema();
        assert!(!schema.is_global_element("UnknownMessage"));
    }

    // -- abstract flag --

    #[test]
    fn abstract_type_stored() {
        let schema = test_schema();
        assert!(schema.complex_types.get("AbstractBase").unwrap().abstract_);
        assert!(!schema.complex_types.get("BaseType").unwrap().abstract_);
    }
}
