//! Structural validation of OMS JSON messages against an XSD-derived schema.

use std::collections::HashSet;

use serde_json::Value;

use crate::facets::{CompiledFacets, FacetViolation as RuntimeFacetViolation};
use crate::schema::model::{
    ComplexContent, ComplexType, Element, MaxOccurs, Schema, SimpleType, TypeRef,
};

/// Successful validation result containing the extracted message name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    /// The OMS message name (root JSON key, matching a global element).
    pub message_name: String,
}

/// Errors from structural validation of an OMS JSON message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationError {
    /// Root value is not a JSON object with exactly one key.
    InvalidRootShape,
    /// The root key does not match any global element in the schema.
    UnknownGlobalElement { name: String },
    /// A complex-type value is not a JSON object.
    ExpectedObject { path: String },
    /// A JSON key does not match any declared element in the type.
    UnknownMember { path: String, member: String },
    /// A required element is missing from the JSON object.
    MissingRequiredMember { path: String, member: String },
    /// Array/scalar mismatch based on `maxOccurs`.
    OccurrenceMismatch { path: String, expected_array: bool },
    /// An `xs:choice` group has zero or more than one member present.
    ChoiceViolation { path: String, present: usize },
    /// A referenced type name is not defined in the schema.
    UnknownType { path: String, type_name: String },
    /// JSON value type does not match the expected XSD primitive.
    TypeMismatch {
        path: String,
        expected: &'static str,
        found: &'static str,
    },
    /// The value violates one or more XSD facets.
    FacetViolation {
        path: String,
        facet: String,
        detail: String,
    },
    /// The `$type` field references an unknown complex type.
    InvalidPolymorphicType { path: String, type_name: String },
    /// The `$type` type is not derived from the declared base type.
    NotDerivedType {
        path: String,
        base: String,
        actual: String,
    },
    /// An abstract type was used without a `$type` override.
    AbstractTypeInstantiation { path: String, type_name: String },
}

/// Validate an OMS JSON message against the given schema.
///
/// The root value must be a JSON object with exactly one key matching a global
/// element name. Returns the message name on success.
pub fn validate_oms_json(
    schema: &Schema,
    value: &Value,
) -> Result<ValidationResult, ValidationError> {
    let Value::Object(root) = value else {
        return Err(ValidationError::InvalidRootShape);
    };

    if root.len() != 1 {
        return Err(ValidationError::InvalidRootShape);
    }

    let Some((message_name, payload)) = root.iter().next() else {
        return Err(ValidationError::InvalidRootShape);
    };

    let Some(global) = schema.global_elements.get(message_name) else {
        return Err(ValidationError::UnknownGlobalElement {
            name: message_name.clone(),
        });
    };

    validate_named_type(schema, &global.type_name, payload, message_name)?;

    Ok(ValidationResult {
        message_name: message_name.clone(),
    })
}

fn validate_named_type(
    schema: &Schema,
    type_name: &str,
    value: &Value,
    path: &str,
) -> Result<(), ValidationError> {
    schema.complex_types.get(type_name).map_or_else(
        || {
            schema.simple_types.get(type_name).map_or_else(
                || {
                    Err(ValidationError::UnknownType {
                        path: path.to_owned(),
                        type_name: type_name.to_owned(),
                    })
                },
                |st| validate_simple_type(schema, st, value, path),
            )
        },
        |ct| validate_complex_type(schema, ct, value, path),
    )
}

fn validate_type_ref(
    schema: &Schema,
    type_ref: &TypeRef,
    value: &Value,
    path: &str,
) -> Result<(), ValidationError> {
    match type_ref {
        TypeRef::Named(type_name) => validate_named_type(schema, type_name, value, path),
        TypeRef::AnonymousComplex(ct) => validate_complex_type(schema, ct, value, path),
        TypeRef::AnonymousSimple(st) => validate_simple_type(schema, st, value, path),
    }
}

fn validate_complex_type(
    schema: &Schema,
    declared_type: &ComplexType,
    value: &Value,
    path: &str,
) -> Result<(), ValidationError> {
    let Value::Object(obj) = value else {
        return Err(ValidationError::ExpectedObject {
            path: path.to_owned(),
        });
    };

    let effective_type =
        resolve_effective_complex_type_for_value(schema, declared_type, obj, path)?;

    let content = &effective_type.content;
    let declared_elements = collect_declared_elements(schema, effective_type, path)?;
    let choice_names: HashSet<String> = collect_choice_elements(schema, content)
        .into_iter()
        .collect();

    for key in obj.keys() {
        if key == "$type" {
            continue;
        }

        if !declared_elements.iter().any(|element| element.name == *key) {
            return Err(ValidationError::UnknownMember {
                path: path.to_owned(),
                member: key.clone(),
            });
        }
    }

    for element in &declared_elements {
        let member_path = format!("{path}.{}", element.name);
        match obj.get(&element.name) {
            Some(member_value) => {
                validate_element_occurrence(schema, element, member_value, &member_path)?;
            }
            None => {
                if element.min_occurs >= 1 && !choice_names.contains(&element.name) {
                    return Err(ValidationError::MissingRequiredMember {
                        path: path.to_owned(),
                        member: element.name.clone(),
                    });
                }
            }
        }
    }

    validate_choice(schema, content, obj, path)
}

fn resolve_effective_complex_type_for_value<'a>(
    schema: &'a Schema,
    declared_type: &'a ComplexType,
    obj: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<&'a ComplexType, ValidationError> {
    match obj.get("$type") {
        Some(Value::String(type_name)) => {
            let Some(candidate) = schema.complex_types.get(type_name) else {
                return Err(ValidationError::InvalidPolymorphicType {
                    path: path.to_owned(),
                    type_name: type_name.clone(),
                });
            };

            if candidate.abstract_ {
                return Err(ValidationError::AbstractTypeInstantiation {
                    path: path.to_owned(),
                    type_name: type_name.clone(),
                });
            }

            if candidate.name != declared_type.name
                && !schema.is_derived_from(&candidate.name, &declared_type.name)
            {
                return Err(ValidationError::NotDerivedType {
                    path: path.to_owned(),
                    base: declared_type.name.clone(),
                    actual: candidate.name.clone(),
                });
            }

            Ok(candidate)
        }
        Some(_) => Err(ValidationError::InvalidPolymorphicType {
            path: path.to_owned(),
            type_name: "$type".to_owned(),
        }),
        None => {
            if declared_type.abstract_ {
                return Err(ValidationError::AbstractTypeInstantiation {
                    path: path.to_owned(),
                    type_name: declared_type.name.clone(),
                });
            }
            Ok(declared_type)
        }
    }
}

fn collect_declared_elements<'a>(
    schema: &'a Schema,
    ct: &'a ComplexType,
    path: &str,
) -> Result<Vec<&'a Element>, ValidationError> {
    match &ct.content {
        ComplexContent::Extension { .. } => {
            schema
                .flatten_elements(&ct.name)
                .ok_or_else(|| ValidationError::UnknownType {
                    path: path.to_owned(),
                    type_name: ct.name.clone(),
                })
        }
        ComplexContent::Sequence { elements } | ComplexContent::Choice { elements } => {
            Ok(elements.iter().collect())
        }
        ComplexContent::Empty => Ok(Vec::new()),
    }
}

fn validate_element_occurrence(
    schema: &Schema,
    element: &Element,
    value: &Value,
    path: &str,
) -> Result<(), ValidationError> {
    let expects_array = match element.max_occurs {
        MaxOccurs::Unbounded => true,
        MaxOccurs::Bounded(max) => max > 1,
    };

    if expects_array {
        let Value::Array(items) = value else {
            return Err(ValidationError::OccurrenceMismatch {
                path: path.to_owned(),
                expected_array: true,
            });
        };

        if items.len() < element.min_occurs as usize {
            return Err(ValidationError::OccurrenceMismatch {
                path: path.to_owned(),
                expected_array: true,
            });
        }

        if let MaxOccurs::Bounded(max) = element.max_occurs
            && items.len() > max as usize
        {
            return Err(ValidationError::OccurrenceMismatch {
                path: path.to_owned(),
                expected_array: true,
            });
        }

        for (index, item) in items.iter().enumerate() {
            let item_path = format!("{path}[{index}]");
            validate_type_ref(schema, &element.type_ref, item, &item_path)?;
        }
    } else {
        if value.is_array() {
            return Err(ValidationError::OccurrenceMismatch {
                path: path.to_owned(),
                expected_array: false,
            });
        }
        validate_type_ref(schema, &element.type_ref, value, path)?;
    }

    Ok(())
}

fn validate_choice(
    schema: &Schema,
    content: &ComplexContent,
    obj: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), ValidationError> {
    let choice_elements: Vec<String> = match content {
        ComplexContent::Choice { elements } => elements
            .iter()
            .map(|element| element.name.clone())
            .collect(),
        ComplexContent::Extension { base, content } => {
            let base_choice = schema
                .complex_types
                .get(base)
                .map_or_else(Vec::new, |base_type| {
                    collect_choice_elements(schema, &base_type.content)
                });

            let ext_choice = content
                .as_ref()
                .map_or_else(Vec::new, |ext| match ext.as_ref() {
                    crate::schema::model::ExtensionContent::Choice { elements } => elements
                        .iter()
                        .map(|element| element.name.clone())
                        .collect(),
                    crate::schema::model::ExtensionContent::Sequence { .. } => Vec::new(),
                });

            [base_choice, ext_choice].concat()
        }
        _ => Vec::new(),
    };

    if choice_elements.is_empty() {
        return Ok(());
    }

    let present = choice_elements
        .iter()
        .filter(|name| obj.contains_key(name.as_str()))
        .count();

    if present != 1 {
        return Err(ValidationError::ChoiceViolation {
            path: path.to_owned(),
            present,
        });
    }

    Ok(())
}

fn collect_choice_elements(schema: &Schema, content: &ComplexContent) -> Vec<String> {
    match content {
        ComplexContent::Choice { elements } => elements
            .iter()
            .map(|element| element.name.clone())
            .collect(),
        ComplexContent::Extension { base, content } => {
            let mut acc = schema
                .complex_types
                .get(base)
                .map_or_else(Vec::new, |base_type| {
                    collect_choice_elements(schema, &base_type.content)
                });
            if let Some(ext) = content
                && let crate::schema::model::ExtensionContent::Choice { elements } = ext.as_ref()
            {
                acc.extend(elements.iter().map(|element| element.name.clone()));
            }
            acc
        }
        _ => Vec::new(),
    }
}

fn validate_simple_type(
    schema: &Schema,
    st: &SimpleType,
    value: &Value,
    path: &str,
) -> Result<(), ValidationError> {
    let declared_type = st.name.as_deref().unwrap_or(st.restriction.base.as_str());
    let primitive = schema
        .resolve_primitive(declared_type)
        .or_else(|| schema.resolve_primitive(&st.restriction.base))
        .ok_or_else(|| ValidationError::UnknownType {
            path: path.to_owned(),
            type_name: st.restriction.base.clone(),
        })?;
    let value_kind_for_type = resolve_simple_value_kind(schema, declared_type)
        .unwrap_or_else(|| SimpleValueKind::from_primitive(&primitive));

    match value_kind_for_type {
        SimpleValueKind::Boolean => {
            if !value.is_boolean() {
                return Err(ValidationError::TypeMismatch {
                    path: path.to_owned(),
                    expected: "boolean",
                    found: value_kind(value),
                });
            }
        }
        SimpleValueKind::Decimal => {
            if !value.is_number() {
                return Err(ValidationError::TypeMismatch {
                    path: path.to_owned(),
                    expected: "number",
                    found: value_kind(value),
                });
            }
        }
        SimpleValueKind::Integer(constraint) => {
            validate_integer_value(value, path, constraint)?;
        }
        SimpleValueKind::Float => {
            if !value.is_number() {
                let Some(text) = value.as_str() else {
                    return Err(ValidationError::TypeMismatch {
                        path: path.to_owned(),
                        expected: "number-or-special-float-string",
                        found: value_kind(value),
                    });
                };

                if text != "NaN" && text != "Infinity" && text != "-Infinity" {
                    return Err(ValidationError::TypeMismatch {
                        path: path.to_owned(),
                        expected: "number-or-special-float-string",
                        found: "string",
                    });
                }
            }
        }
        SimpleValueKind::String => {
            if !value.is_string() {
                return Err(ValidationError::TypeMismatch {
                    path: path.to_owned(),
                    expected: "string",
                    found: value_kind(value),
                });
            }
        }
    }

    validate_facets(schema, st, value, path, &primitive)
}

#[derive(Debug, Clone, Copy)]
enum SimpleValueKind {
    Boolean,
    Decimal,
    Integer(IntegerConstraint),
    Float,
    String,
}

impl SimpleValueKind {
    fn from_primitive(primitive: &str) -> Self {
        match primitive {
            "xs:boolean" => Self::Boolean,
            "xs:decimal" => Self::Decimal,
            "xs:float" | "xs:double" => Self::Float,
            _ => Self::String,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IntegerConstraint {
    type_name: &'static str,
    min: Option<i128>,
    max: Option<i128>,
}

fn resolve_simple_value_kind(schema: &Schema, type_name: &str) -> Option<SimpleValueKind> {
    let mut current = type_name.to_owned();

    for _ in 0..=50 {
        if let Some(kind) = builtin_value_kind(&current) {
            return Some(kind);
        }

        let st = schema.simple_types.get(&current)?;
        if st.restriction.base == current {
            return builtin_value_kind(&current);
        }
        current.clone_from(&st.restriction.base);
    }

    None
}

fn builtin_value_kind(type_name: &str) -> Option<SimpleValueKind> {
    match type_name {
        "xs:boolean" => Some(SimpleValueKind::Boolean),
        "xs:decimal" => Some(SimpleValueKind::Decimal),
        "xs:float" | "xs:double" => Some(SimpleValueKind::Float),
        "xs:integer" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:integer",
            min: None,
            max: None,
        })),
        "xs:nonPositiveInteger" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:nonPositiveInteger",
            min: None,
            max: Some(0),
        })),
        "xs:negativeInteger" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:negativeInteger",
            min: None,
            max: Some(-1),
        })),
        "xs:long" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:long",
            min: Some(i128::from(i64::MIN)),
            max: Some(i128::from(i64::MAX)),
        })),
        "xs:int" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:int",
            min: Some(i128::from(i32::MIN)),
            max: Some(i128::from(i32::MAX)),
        })),
        "xs:short" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:short",
            min: Some(i128::from(i16::MIN)),
            max: Some(i128::from(i16::MAX)),
        })),
        "xs:byte" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:byte",
            min: Some(i128::from(i8::MIN)),
            max: Some(i128::from(i8::MAX)),
        })),
        "xs:nonNegativeInteger" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:nonNegativeInteger",
            min: Some(0),
            max: None,
        })),
        "xs:positiveInteger" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:positiveInteger",
            min: Some(1),
            max: None,
        })),
        "xs:unsignedLong" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:unsignedLong",
            min: Some(0),
            max: Some(i128::from(u64::MAX)),
        })),
        "xs:unsignedInt" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:unsignedInt",
            min: Some(0),
            max: Some(i128::from(u32::MAX)),
        })),
        "xs:unsignedShort" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:unsignedShort",
            min: Some(0),
            max: Some(i128::from(u16::MAX)),
        })),
        "xs:unsignedByte" => Some(SimpleValueKind::Integer(IntegerConstraint {
            type_name: "xs:unsignedByte",
            min: Some(0),
            max: Some(i128::from(u8::MAX)),
        })),
        other if other.starts_with("xs:") => Some(SimpleValueKind::String),
        _ => None,
    }
}

fn validate_integer_value(
    value: &Value,
    path: &str,
    constraint: IntegerConstraint,
) -> Result<(), ValidationError> {
    let Some(number) = value.as_number() else {
        return Err(ValidationError::TypeMismatch {
            path: path.to_owned(),
            expected: "integer",
            found: value_kind(value),
        });
    };

    let integer = number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
        .ok_or_else(|| ValidationError::TypeMismatch {
            path: path.to_owned(),
            expected: "integer",
            found: "number",
        })?;

    if constraint.min.is_some_and(|min| integer < min)
        || constraint.max.is_some_and(|max| integer > max)
    {
        return Err(ValidationError::FacetViolation {
            path: path.to_owned(),
            facet: "builtinRange".to_owned(),
            detail: format!(
                "value {integer} is outside the range for {}",
                constraint.type_name
            ),
        });
    }

    Ok(())
}

fn validate_facets(
    schema: &Schema,
    st: &SimpleType,
    value: &Value,
    path: &str,
    primitive: &str,
) -> Result<(), ValidationError> {
    if let Some(type_name) = &st.name
        && let Some(compiled) = schema.compiled_facets.get(type_name)
    {
        return compiled
            .validate(value, primitive, path)
            .map_err(map_facet_violation);
    }

    if st.restriction.facets.is_empty() {
        return Ok(());
    }

    let (compiled, warnings) = CompiledFacets::compile(&st.restriction.facets);
    for warning in warnings {
        tracing::warn!(path = %path, warning = %warning, "inline facet compile warning");
    }

    if !compiled.has_constraints() {
        return Ok(());
    }

    compiled
        .validate(value, primitive, path)
        .map_err(map_facet_violation)
}

fn map_facet_violation(violation: RuntimeFacetViolation) -> ValidationError {
    ValidationError::FacetViolation {
        path: violation.path,
        facet: violation.facet,
        detail: violation.detail,
    }
}

const fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::schema::model::{
        ComplexContent, ComplexType, Element, ExtensionContent, Facet, GlobalElement, MaxOccurs,
        Restriction, Schema, SimpleType, TypeRef,
    };

    fn named_element(
        name: &str,
        type_name: &str,
        min_occurs: u32,
        max_occurs: MaxOccurs,
    ) -> Element {
        Element {
            name: name.to_owned(),
            type_ref: TypeRef::Named(type_name.to_owned()),
            min_occurs,
            max_occurs,
        }
    }

    fn simple(name: &str, base: &str) -> SimpleType {
        SimpleType {
            name: Some(name.to_owned()),
            restriction: Restriction {
                base: base.to_owned(),
                facets: Vec::new(),
            },
        }
    }

    fn add_simple_types(schema: &mut Schema) {
        for (name, base) in [
            ("IdType", "xs:string"),
            ("ScoreType", "xs:int"),
            ("BoolType", "xs:boolean"),
            ("FloatType", "xs:float"),
        ] {
            schema
                .simple_types
                .insert(name.to_owned(), simple(name, base));
        }

        schema.simple_types.insert(
            "StatusBaseType".to_owned(),
            SimpleType {
                name: Some("StatusBaseType".to_owned()),
                restriction: Restriction {
                    base: "xs:string".to_owned(),
                    facets: vec![Facet::Pattern("[A-Z]+".to_owned())],
                },
            },
        );
        schema.simple_types.insert(
            "StatusDerivedType".to_owned(),
            SimpleType {
                name: Some("StatusDerivedType".to_owned()),
                restriction: Restriction {
                    base: "StatusBaseType".to_owned(),
                    facets: vec![Facet::Enumeration("ACTIVE".to_owned())],
                },
            },
        );
        schema.simple_types.insert(
            "StatusPatternDerivedType".to_owned(),
            SimpleType {
                name: Some("StatusPatternDerivedType".to_owned()),
                restriction: Restriction {
                    base: "StatusBaseType".to_owned(),
                    facets: Vec::new(),
                },
            },
        );
    }

    #[allow(clippy::too_many_lines)]
    fn add_complex_types(schema: &mut Schema) {
        schema.complex_types.insert(
            "BaseMessageType".to_owned(),
            ComplexType {
                name: "BaseMessageType".to_owned(),
                abstract_: true,
                content: ComplexContent::Sequence {
                    elements: vec![
                        named_element("Id", "IdType", 1, MaxOccurs::Bounded(1)),
                        named_element("Values", "ScoreType", 0, MaxOccurs::Unbounded),
                    ],
                },
            },
        );
        schema.complex_types.insert(
            "DerivedMessageType".to_owned(),
            ComplexType {
                name: "DerivedMessageType".to_owned(),
                abstract_: false,
                content: ComplexContent::Extension {
                    base: "BaseMessageType".to_owned(),
                    content: Some(Box::new(ExtensionContent::Sequence {
                        elements: vec![
                            named_element("Enabled", "BoolType", 1, MaxOccurs::Bounded(1)),
                            named_element("Weight", "FloatType", 0, MaxOccurs::Bounded(1)),
                        ],
                    })),
                },
            },
        );
        schema.complex_types.insert(
            "ChoiceType".to_owned(),
            ComplexType {
                name: "ChoiceType".to_owned(),
                abstract_: false,
                content: ComplexContent::Choice {
                    elements: vec![
                        named_element("ById", "IdType", 1, MaxOccurs::Bounded(1)),
                        named_element("ByScore", "ScoreType", 1, MaxOccurs::Bounded(1)),
                    ],
                },
            },
        );
        schema.complex_types.insert(
            "WrapperType".to_owned(),
            ComplexType {
                name: "WrapperType".to_owned(),
                abstract_: false,
                content: ComplexContent::Sequence {
                    elements: vec![
                        named_element("Payload", "DerivedMessageType", 1, MaxOccurs::Bounded(1)),
                        Element {
                            name: "AnonSimple".to_owned(),
                            type_ref: TypeRef::AnonymousSimple(Box::new(SimpleType {
                                name: None,
                                restriction: Restriction {
                                    base: "xs:string".to_owned(),
                                    facets: vec![Facet::Enumeration("OK".to_owned())],
                                },
                            })),
                            min_occurs: 1,
                            max_occurs: MaxOccurs::Bounded(1),
                        },
                        Element {
                            name: "AnonComplex".to_owned(),
                            type_ref: TypeRef::AnonymousComplex(Box::new(ComplexType {
                                name: String::new(),
                                abstract_: false,
                                content: ComplexContent::Sequence {
                                    elements: vec![named_element(
                                        "Inner",
                                        "StatusDerivedType",
                                        1,
                                        MaxOccurs::Bounded(1),
                                    )],
                                },
                            })),
                            min_occurs: 1,
                            max_occurs: MaxOccurs::Bounded(1),
                        },
                    ],
                },
            },
        );
        schema.complex_types.insert(
            "NestedFacetType".to_owned(),
            ComplexType {
                name: "NestedFacetType".to_owned(),
                abstract_: false,
                content: ComplexContent::Sequence {
                    elements: vec![named_element(
                        "State",
                        "StatusDerivedType",
                        1,
                        MaxOccurs::Bounded(1),
                    )],
                },
            },
        );
        schema.complex_types.insert(
            "PatternFacetType".to_owned(),
            ComplexType {
                name: "PatternFacetType".to_owned(),
                abstract_: false,
                content: ComplexContent::Sequence {
                    elements: vec![named_element(
                        "State",
                        "StatusPatternDerivedType",
                        1,
                        MaxOccurs::Bounded(1),
                    )],
                },
            },
        );
    }

    fn build_schema() -> Schema {
        let mut schema = Schema::new();
        add_simple_types(&mut schema);
        add_complex_types(&mut schema);

        for (name, type_name) in [
            ("DerivedMessage", "BaseMessageType"),
            ("ChoiceMessage", "ChoiceType"),
            ("WrapperMessage", "WrapperType"),
            ("NestedFacetMessage", "NestedFacetType"),
            ("PatternFacetMessage", "PatternFacetType"),
        ] {
            schema.global_elements.insert(
                name.to_owned(),
                GlobalElement {
                    type_name: type_name.to_owned(),
                },
            );
        }

        schema.compile_facets();
        schema
    }

    #[test]
    fn validate_success_with_type_extension_and_special_float() {
        let schema = build_schema();
        let value = json!({
            "DerivedMessage": {
                "$type": "DerivedMessageType",
                "Id": "abc",
                "Enabled": true,
                "Values": [1, 2, 3],
                "Weight": "Infinity"
            }
        });

        let result = validate_oms_json(&schema, &value).unwrap();
        assert_eq!(result.message_name, "DerivedMessage");
    }

    #[test]
    fn validate_success_choice_single_arm() {
        let schema = build_schema();
        let value = json!({
            "ChoiceMessage": {
                "ById": "abc"
            }
        });

        assert!(validate_oms_json(&schema, &value).is_ok());
    }

    #[test]
    fn validate_success_with_anonymous_types() {
        let schema = build_schema();
        let value = json!({
            "WrapperMessage": {
                "Payload": {
                    "$type": "DerivedMessageType",
                    "Id": "root",
                    "Enabled": false
                },
                "AnonSimple": "OK",
                "AnonComplex": {
                    "Inner": "ACTIVE"
                }
            }
        });

        assert!(validate_oms_json(&schema, &value).is_ok());
    }

    #[test]
    fn error_invalid_root_shape_not_object() {
        let schema = build_schema();
        let err = validate_oms_json(&schema, &json!([1, 2])).unwrap_err();
        assert_eq!(err, ValidationError::InvalidRootShape);
    }

    #[test]
    fn error_invalid_root_shape_multiple_keys() {
        let schema = build_schema();
        let err = validate_oms_json(&schema, &json!({"A":{},"B":{}})).unwrap_err();
        assert_eq!(err, ValidationError::InvalidRootShape);
    }

    #[test]
    fn error_unknown_global_element() {
        let schema = build_schema();
        let err = validate_oms_json(&schema, &json!({"Unknown": {}})).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::UnknownGlobalElement { ref name } if name == "Unknown"
        ));
    }

    #[test]
    fn error_expected_object_for_complex_type() {
        let schema = build_schema();
        let err = validate_oms_json(&schema, &json!({"DerivedMessage": 1})).unwrap_err();
        assert!(matches!(err, ValidationError::ExpectedObject { .. }));
    }

    #[test]
    fn error_missing_required_member() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "DerivedMessageType",
                    "Enabled": true
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::MissingRequiredMember { ref member, .. } if member == "Id"
        ));
    }

    #[test]
    fn error_unknown_member() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "DerivedMessageType",
                    "Id": "id",
                    "Enabled": true,
                    "Extra": 5
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::UnknownMember { ref member, .. } if member == "Extra"
        ));
    }

    #[test]
    fn error_occurrence_expected_array() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "DerivedMessageType",
                    "Id": "id",
                    "Enabled": true,
                    "Values": 7
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::OccurrenceMismatch {
                expected_array: true,
                ..
            }
        ));
    }

    #[test]
    fn error_occurrence_expected_scalar() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "DerivedMessageType",
                    "Id": ["id"],
                    "Enabled": true
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::OccurrenceMismatch {
                expected_array: false,
                ..
            }
        ));
    }

    #[test]
    fn error_choice_zero_present() {
        let schema = build_schema();
        let err = validate_oms_json(&schema, &json!({"ChoiceMessage": {}})).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::ChoiceViolation { present: 0, .. }
        ));
    }

    #[test]
    fn error_choice_multiple_present() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "ChoiceMessage": {
                    "ById": "id",
                    "ByScore": 1
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::ChoiceViolation { present: 2, .. }
        ));
    }

    #[test]
    fn error_invalid_type_name() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "Nope",
                    "Id": "id",
                    "Enabled": true
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::InvalidPolymorphicType { .. }
        ));
    }

    #[test]
    fn error_not_derived_type() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "ChoiceType",
                    "Id": "id",
                    "Enabled": true
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(err, ValidationError::NotDerivedType { .. }));
    }

    #[test]
    fn error_abstract_without_type_override() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "Id": "id",
                    "Enabled": true
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::AbstractTypeInstantiation { .. }
        ));
    }

    #[test]
    fn error_type_mismatch_boolean() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "DerivedMessageType",
                    "Id": "id",
                    "Enabled": "yes"
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::TypeMismatch {
                expected: "boolean",
                ..
            }
        ));
    }

    #[test]
    fn error_type_mismatch_integer() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "ChoiceMessage": {
                    "ByScore": "x"
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::TypeMismatch {
                expected: "integer",
                ..
            }
        ));
    }

    #[test]
    fn integer_type_rejects_fractional_number() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "ChoiceMessage": {
                    "ByScore": 1.5
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::TypeMismatch {
                expected: "integer",
                found: "number",
                ..
            }
        ));
    }

    #[test]
    fn integer_type_rejects_builtin_range_violation() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "ChoiceMessage": {
                    "ByScore": 2_147_483_648_i64
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::FacetViolation { ref facet, .. } if facet == "builtinRange"
        ));
    }

    #[test]
    fn custom_unsigned_integer_type_rejects_negative_number() {
        let mut schema = Schema::new();
        schema.simple_types.insert(
            "ByteAlias".to_owned(),
            SimpleType {
                name: Some("ByteAlias".to_owned()),
                restriction: Restriction {
                    base: "xs:unsignedByte".to_owned(),
                    facets: Vec::new(),
                },
            },
        );
        let st = schema.simple_types.get("ByteAlias").expect("test type");
        let err = validate_simple_type(&schema, st, &json!(-1), "root").unwrap_err();

        assert!(matches!(
            err,
            ValidationError::FacetViolation { ref facet, .. } if facet == "builtinRange"
        ));
    }

    #[test]
    fn decimal_type_accepts_fractional_number() {
        let schema = Schema::new();
        let st = schema.simple_types.get("xs:decimal").expect("xs:decimal");

        assert!(validate_simple_type(&schema, st, &json!(1.5), "root").is_ok());
    }

    #[test]
    fn error_type_mismatch_float_special() {
        let schema = build_schema();
        let err = validate_oms_json(
            &schema,
            &json!({
                "DerivedMessage": {
                    "$type": "DerivedMessageType",
                    "Id": "id",
                    "Enabled": true,
                    "Weight": "BAD"
                }
            }),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ValidationError::TypeMismatch {
                expected: "number-or-special-float-string",
                ..
            }
        ));
    }

    #[test]
    fn facet_validation_on_nested_element() {
        let schema = build_schema();
        let valid = json!({
            "NestedFacetMessage": {
                "State": "ACTIVE"
            }
        });
        assert!(validate_oms_json(&schema, &valid).is_ok());

        let invalid = json!({
            "NestedFacetMessage": {
                "State": "INACTIVE"
            }
        });
        let error = validate_oms_json(&schema, &invalid).expect_err("invalid enum should fail");
        assert!(matches!(
            error,
            ValidationError::FacetViolation { ref facet, .. } if facet == "enumeration"
        ));
    }

    #[test]
    fn facet_inheritance_applies_base_pattern_and_derived_enum() {
        let schema = build_schema();

        let base_pattern_failure = json!({
            "PatternFacetMessage": {
                "State": "active"
            }
        });
        let error = validate_oms_json(&schema, &base_pattern_failure)
            .expect_err("base pattern should be enforced");
        assert!(matches!(
            error,
            ValidationError::FacetViolation { ref facet, .. } if facet == "pattern"
        ));
    }

    #[test]
    fn anonymous_simple_type_facets_are_validated() {
        let schema = build_schema();

        let invalid = json!({
            "WrapperMessage": {
                "Payload": {
                    "$type": "DerivedMessageType",
                    "Id": "root",
                    "Enabled": false
                },
                "AnonSimple": "BAD",
                "AnonComplex": {
                    "Inner": "ACTIVE"
                }
            }
        });

        let error = validate_oms_json(&schema, &invalid)
            .expect_err("anonymous simple type enumeration should fail");
        assert!(matches!(
            error,
            ValidationError::FacetViolation { ref facet, .. } if facet == "enumeration"
        ));
    }

    #[test]
    fn mini_schema_end_to_end_recursion() {
        let schema = build_schema();
        let value = json!({
            "WrapperMessage": {
                "Payload": {
                    "$type": "DerivedMessageType",
                    "Id": "r1",
                    "Enabled": true,
                    "Values": [10]
                },
                "AnonSimple": "OK",
                "AnonComplex": {
                    "Inner": "ACTIVE"
                }
            }
        });

        let result = validate_oms_json(&schema, &value).unwrap();
        assert_eq!(result.message_name, "WrapperMessage");
    }
}
