//! Fast XML streaming parser for XSD schema files.
//!
//! Uses `quick-xml` to read XSD files and produce the corresponding
//! `Schema` model structs used by the runtime payload validator.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

use super::model::{
    ComplexContent, ComplexType, Element, ExtensionContent, Facet, GlobalElement, MaxOccurs,
    Restriction, Schema, SimpleType, TypeRef,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors encountered while parsing XSD schema files.
#[derive(Debug)]
pub enum SchemaError {
    /// File I/O error.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Malformed XML content.
    Xml { path: PathBuf, message: String },
    /// Circular `xs:include` dependency detected.
    CircularInclude { path: PathBuf },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Xml { path, message } => write!(f, "{}: XML error: {message}", path.display()),
            Self::CircularInclude { path } => {
                write!(f, "{}: circular xs:include detected", path.display())
            }
        }
    }
}

impl std::error::Error for SchemaError {}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse an XSD file (and its `xs:include` dependencies) into a [`Schema`].
pub fn parse(path: &Path) -> Result<Schema, SchemaError> {
    let mut schema = Schema::new();
    let mut parsed = HashSet::new();
    let mut active = HashSet::new();
    parse_file(path, &mut schema, &mut parsed, &mut active)?;
    schema.compile_facets();
    Ok(schema)
}

// ---------------------------------------------------------------------------
// Parser state types
// ---------------------------------------------------------------------------

const XS_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentKind {
    Unknown,
    Sequence,
    Choice,
}

#[derive(Debug)]
enum ParseState {
    SchemaRoot,
    ComplexType {
        name: String,
        abstract_: bool,
        elements: Vec<Element>,
        content_kind: ContentKind,
        extension: Option<(String, Option<Box<ExtensionContent>>)>,
    },
    Extension {
        base: String,
        elements: Vec<Element>,
        content_kind: ContentKind,
    },
    Sequence {
        elements: Vec<Element>,
    },
    Choice {
        elements: Vec<Element>,
    },
    SimpleType {
        name: Option<String>,
        restriction: Option<Restriction>,
    },
    Restriction {
        base: String,
        facets: Vec<Facet>,
    },
    Element {
        name: String,
        type_ref: Option<TypeRef>,
        min_occurs: u32,
        max_occurs: MaxOccurs,
        is_global: bool,
    },
    ComplexContentWrapper,
    Skip {
        depth: u32,
    },
}

// ---------------------------------------------------------------------------
// File parser
// ---------------------------------------------------------------------------

fn parse_file(
    path: &Path,
    schema: &mut Schema,
    parsed: &mut HashSet<PathBuf>,
    active: &mut HashSet<PathBuf>,
) -> Result<(), SchemaError> {
    let canonical = path.canonicalize().map_err(|e| SchemaError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    if active.contains(&canonical) {
        return Err(SchemaError::CircularInclude {
            path: path.to_path_buf(),
        });
    }

    if parsed.contains(&canonical) {
        return Ok(());
    }

    active.insert(canonical.clone());

    let parse_result = (|| {
        let xml_text = std::fs::read_to_string(&canonical).map_err(|e| SchemaError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut reader = Reader::from_str(&xml_text);
        let base_dir = canonical.parent().unwrap_or_else(|| Path::new("."));
        let file_path = path.to_path_buf();

        let mut xs_prefix: Option<String> = None;
        let mut stack: Vec<ParseState> = Vec::new();
        let mut buf = Vec::new();

        loop {
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|e| SchemaError::Xml {
                    path: file_path.clone(),
                    message: e.to_string(),
                })?;

            match event {
                Event::Eof => break,

                Event::Start(ref e) => {
                    let name_bytes = e.name();
                    let full_name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                    let (prefix, local) = split_name(full_name);

                    if xs_prefix.is_none() && local == "schema" {
                        xs_prefix = detect_xs_prefix(e);
                        extract_schema_attrs(e, schema);
                        stack.push(ParseState::SchemaRoot);
                        buf.clear();
                        continue;
                    }

                    if !prefix_matches(prefix, xs_prefix.as_deref()) {
                        push_or_increment_skip(&mut stack);
                        buf.clear();
                        continue;
                    }

                    if increment_skip_if_active(&mut stack) {
                        buf.clear();
                        continue;
                    }

                    on_start(local, e, &mut stack, schema, base_dir, parsed, active)?;
                }

                Event::Empty(ref e) => {
                    let name_bytes = e.name();
                    let full_name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                    let (prefix, local) = split_name(full_name);

                    if !prefix_matches(prefix, xs_prefix.as_deref()) {
                        buf.clear();
                        continue;
                    }

                    if matches!(stack.last(), Some(ParseState::Skip { .. })) {
                        buf.clear();
                        continue;
                    }

                    on_empty(local, e, &mut stack, schema, base_dir, parsed, active)?;
                }

                Event::End(_) => {
                    on_end(&mut stack, schema);
                }

                _ => {}
            }

            buf.clear();
        }

        Ok(())
    })();

    active.remove(&canonical);

    if parse_result.is_ok() {
        parsed.insert(canonical);
    }

    parse_result
}

fn push_or_increment_skip(stack: &mut Vec<ParseState>) {
    if let Some(ParseState::Skip { depth }) = stack.last_mut() {
        *depth += 1;
    } else {
        stack.push(ParseState::Skip { depth: 1 });
    }
}

const fn increment_skip_if_active(stack: &mut [ParseState]) -> bool {
    if let Some(ParseState::Skip { depth }) = stack.last_mut() {
        *depth += 1;
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Start element
// ---------------------------------------------------------------------------

fn on_start(
    local: &str,
    e: &quick_xml::events::BytesStart<'_>,
    stack: &mut Vec<ParseState>,
    schema: &mut Schema,
    base_dir: &Path,
    parsed: &mut HashSet<PathBuf>,
    active: &mut HashSet<PathBuf>,
) -> Result<(), SchemaError> {
    match local {
        "include" => {
            if let Some(loc) = get_attr(e, "schemaLocation") {
                parse_file(&base_dir.join(&loc), schema, parsed, active)?;
            }
            stack.push(ParseState::Skip { depth: 1 });
        }
        "element" => push_element_start(e, stack, schema),
        "complexType" => {
            let name = get_attr(e, "name").unwrap_or_default();
            let abstract_ = get_attr(e, "abstract").as_deref() == Some("true");
            stack.push(ParseState::ComplexType {
                name,
                abstract_,
                elements: Vec::new(),
                content_kind: ContentKind::Unknown,
                extension: None,
            });
        }
        "simpleType" => {
            stack.push(ParseState::SimpleType {
                name: get_attr(e, "name"),
                restriction: None,
            });
        }
        "sequence" => stack.push(ParseState::Sequence {
            elements: Vec::new(),
        }),
        "choice" => stack.push(ParseState::Choice {
            elements: Vec::new(),
        }),
        "complexContent" => stack.push(ParseState::ComplexContentWrapper),
        "extension" => {
            let base = resolve_type_ref(&get_attr(e, "base").unwrap_or_default());
            stack.push(ParseState::Extension {
                base,
                elements: Vec::new(),
                content_kind: ContentKind::Unknown,
            });
        }
        "restriction" => {
            let base = resolve_type_ref(&get_attr(e, "base").unwrap_or_default());
            stack.push(ParseState::Restriction {
                base,
                facets: Vec::new(),
            });
        }
        "enumeration" | "pattern" | "minInclusive" | "maxInclusive" | "minExclusive"
        | "maxExclusive" | "minLength" | "maxLength" | "length" | "totalDigits"
        | "fractionDigits" => {
            if let Some(value) = get_attr(e, "value")
                && let Some(facet) = make_facet(local, &value)
                && let Some(ParseState::Restriction { facets, .. }) = stack.last_mut()
            {
                facets.push(facet);
            }
            stack.push(ParseState::Skip { depth: 1 });
        }
        _ => stack.push(ParseState::Skip { depth: 1 }),
    }
    Ok(())
}

fn push_element_start(
    e: &quick_xml::events::BytesStart<'_>,
    stack: &mut Vec<ParseState>,
    schema: &mut Schema,
) {
    let name = get_attr(e, "name").unwrap_or_default();
    let type_attr = get_attr(e, "type");
    let min_occurs = parse_u32_attr(e, "minOccurs", 1);
    let max_occurs = parse_max_occurs(e);
    let is_global = matches!(stack.last(), Some(ParseState::SchemaRoot));

    if is_global && let Some(ref type_name) = type_attr {
        schema.global_elements.insert(
            name.clone(),
            GlobalElement {
                type_name: resolve_type_ref(type_name),
            },
        );
    }

    stack.push(ParseState::Element {
        name,
        type_ref: type_attr.map(|t| TypeRef::Named(resolve_type_ref(&t))),
        min_occurs,
        max_occurs,
        is_global,
    });
}

// ---------------------------------------------------------------------------
// Empty element
// ---------------------------------------------------------------------------

fn on_empty(
    local: &str,
    e: &quick_xml::events::BytesStart<'_>,
    stack: &mut [ParseState],
    schema: &mut Schema,
    base_dir: &Path,
    parsed: &mut HashSet<PathBuf>,
    active: &mut HashSet<PathBuf>,
) -> Result<(), SchemaError> {
    match local {
        "include" => {
            if let Some(loc) = get_attr(e, "schemaLocation") {
                parse_file(&base_dir.join(&loc), schema, parsed, active)?;
            }
        }
        "element" => handle_empty_element(e, stack, schema),
        "extension" => {
            let base = resolve_type_ref(&get_attr(e, "base").unwrap_or_default());
            propagate_extension(stack, base, None);
        }
        "restriction" => {
            let base = resolve_type_ref(&get_attr(e, "base").unwrap_or_default());
            if let Some(ParseState::SimpleType { restriction, .. }) = stack.last_mut() {
                *restriction = Some(Restriction {
                    base,
                    facets: Vec::new(),
                });
            }
        }
        "enumeration" | "pattern" | "minInclusive" | "maxInclusive" | "minExclusive"
        | "maxExclusive" | "minLength" | "maxLength" | "length" | "totalDigits"
        | "fractionDigits" => {
            if let Some(value) = get_attr(e, "value")
                && let Some(facet) = make_facet(local, &value)
                && let Some(ParseState::Restriction { facets, .. }) = stack.last_mut()
            {
                facets.push(facet);
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_empty_element(
    e: &quick_xml::events::BytesStart<'_>,
    stack: &mut [ParseState],
    schema: &mut Schema,
) {
    let name = get_attr(e, "name").unwrap_or_default();
    let type_attr = get_attr(e, "type");
    let min_occurs = parse_u32_attr(e, "minOccurs", 1);
    let max_occurs = parse_max_occurs(e);
    let is_global = matches!(stack.last(), Some(ParseState::SchemaRoot));

    if is_global {
        let type_name = type_attr
            .as_ref()
            .map_or_else(|| "xs:anySimpleType".to_owned(), |t| resolve_type_ref(t));
        schema
            .global_elements
            .insert(name, GlobalElement { type_name });
    } else {
        let type_ref = type_attr.map_or_else(
            || TypeRef::Named("xs:anySimpleType".to_owned()),
            |t| TypeRef::Named(resolve_type_ref(&t)),
        );
        push_element_to_parent(
            stack,
            Element {
                name,
                type_ref,
                min_occurs,
                max_occurs,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// End element
// ---------------------------------------------------------------------------

fn on_end(stack: &mut Vec<ParseState>, schema: &mut Schema) {
    let Some(state) = stack.pop() else {
        return;
    };

    match state {
        ParseState::Skip { depth } if depth > 1 => {
            stack.push(ParseState::Skip { depth: depth - 1 });
        }
        ParseState::Skip { .. } | ParseState::SchemaRoot | ParseState::ComplexContentWrapper => {}

        ParseState::Restriction { base, facets } => {
            if let Some(ParseState::SimpleType { restriction, .. }) = stack.last_mut() {
                *restriction = Some(Restriction { base, facets });
            }
        }

        ParseState::SimpleType { name, restriction } => {
            let st = SimpleType {
                name: name.clone(),
                restriction: restriction.unwrap_or_else(|| Restriction {
                    base: "xs:anySimpleType".to_owned(),
                    facets: Vec::new(),
                }),
            };
            if let Some(ParseState::Element { type_ref, .. }) = stack.last_mut() {
                *type_ref = Some(TypeRef::AnonymousSimple(Box::new(st)));
            } else if let Some(n) = name {
                schema.simple_types.insert(n, st);
            }
        }

        ParseState::Sequence { elements } => {
            merge_elements_into_parent(stack, elements, ContentKind::Sequence);
        }

        ParseState::Choice { elements } => {
            merge_elements_into_parent(stack, elements, ContentKind::Choice);
        }

        ParseState::Extension {
            base,
            elements,
            content_kind,
        } => {
            let ext_content = if elements.is_empty() {
                None
            } else {
                Some(Box::new(match content_kind {
                    ContentKind::Choice => ExtensionContent::Choice { elements },
                    ContentKind::Sequence | ContentKind::Unknown => {
                        ExtensionContent::Sequence { elements }
                    }
                }))
            };
            propagate_extension(stack, base, ext_content);
        }

        ParseState::ComplexType {
            name,
            abstract_,
            elements,
            content_kind,
            extension,
        } => {
            end_complex_type(
                schema,
                stack,
                name,
                abstract_,
                elements,
                content_kind,
                extension,
            );
        }

        ParseState::Element {
            name,
            type_ref,
            min_occurs,
            max_occurs,
            is_global,
        } => {
            end_element(
                schema, stack, name, type_ref, min_occurs, max_occurs, is_global,
            );
        }
    }
}

fn merge_elements_into_parent(stack: &mut [ParseState], elements: Vec<Element>, kind: ContentKind) {
    let Some(parent) = stack.last_mut() else {
        return;
    };

    match parent {
        ParseState::ComplexType {
            elements: pe,
            content_kind: pk,
            ..
        }
        | ParseState::Extension {
            elements: pe,
            content_kind: pk,
            ..
        } => {
            pe.extend(elements);
            *pk = kind;
        }
        _ => {}
    }
}

fn end_complex_type(
    schema: &mut Schema,
    stack: &mut [ParseState],
    name: String,
    abstract_: bool,
    elements: Vec<Element>,
    content_kind: ContentKind,
    extension: Option<(String, Option<Box<ExtensionContent>>)>,
) {
    let content = if let Some((base, ext_content)) = extension {
        ComplexContent::Extension {
            base,
            content: ext_content,
        }
    } else if elements.is_empty() {
        ComplexContent::Empty
    } else {
        match content_kind {
            ContentKind::Choice => ComplexContent::Choice { elements },
            ContentKind::Sequence | ContentKind::Unknown => ComplexContent::Sequence { elements },
        }
    };

    let ct = ComplexType {
        name: name.clone(),
        abstract_,
        content,
    };

    if let Some(ParseState::Element { type_ref, .. }) = stack.last_mut() {
        *type_ref = Some(TypeRef::AnonymousComplex(Box::new(ct)));
    } else if !name.is_empty() {
        schema.complex_types.insert(name, ct);
    }
}

fn end_element(
    schema: &mut Schema,
    stack: &mut [ParseState],
    name: String,
    type_ref: Option<TypeRef>,
    min_occurs: u32,
    max_occurs: MaxOccurs,
    is_global: bool,
) {
    let final_type_ref = type_ref.unwrap_or_else(|| TypeRef::Named("xs:anySimpleType".to_owned()));

    if is_global {
        let type_name = resolve_global_type(&name, &final_type_ref, schema);
        schema
            .global_elements
            .insert(name.clone(), GlobalElement { type_name });
    } else {
        push_element_to_parent(
            stack,
            Element {
                name,
                type_ref: final_type_ref,
                min_occurs,
                max_occurs,
            },
        );
    }
}

fn resolve_global_type(name: &str, type_ref: &TypeRef, schema: &mut Schema) -> String {
    match type_ref {
        TypeRef::Named(tn) => tn.clone(),
        TypeRef::AnonymousComplex(ct) => {
            let synthetic = format!("{name}Type");
            let mut ct_named = ct.as_ref().clone();
            ct_named.name.clone_from(&synthetic);
            schema.complex_types.insert(synthetic.clone(), ct_named);
            synthetic
        }
        TypeRef::AnonymousSimple(st) => {
            let synthetic = format!("{name}Type");
            let mut st_named = st.as_ref().clone();
            st_named.name = Some(synthetic.clone());
            schema.simple_types.insert(synthetic.clone(), st_named);
            synthetic
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn split_name(full: &str) -> (Option<&str>, &str) {
    full.split_once(':')
        .map_or((None, full), |(p, l)| (Some(p), l))
}

fn prefix_matches(actual: Option<&str>, expected: Option<&str>) -> bool {
    actual == expected
}

fn detect_xs_prefix(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
        let value = std::str::from_utf8(&attr.value).unwrap_or("");

        if value == XS_NAMESPACE {
            if key == "xmlns" {
                return None;
            }
            if let Some(prefix) = key.strip_prefix("xmlns:") {
                return Some(prefix.to_owned());
            }
        }
    }

    let name_bytes = e.name();
    let full = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
    let (prefix, _) = split_name(full);
    prefix.map(String::from)
}

fn extract_schema_attrs(e: &quick_xml::events::BytesStart<'_>, schema: &mut Schema) {
    if let Some(ns) = get_attr(e, "targetNamespace") {
        schema.target_namespace = Some(ns);
    }
    if let Some(v) = get_attr(e, "version") {
        schema.version = Some(v);
    }
}

fn get_attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|attr| {
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
        if key == name {
            Some(String::from_utf8(attr.value.to_vec()).unwrap_or_default())
        } else {
            None
        }
    })
}

fn parse_u32_attr(e: &quick_xml::events::BytesStart<'_>, name: &str, default: u32) -> u32 {
    get_attr(e, name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_max_occurs(e: &quick_xml::events::BytesStart<'_>) -> MaxOccurs {
    match get_attr(e, "maxOccurs").as_deref() {
        Some("unbounded") => MaxOccurs::Unbounded,
        Some(n) => MaxOccurs::Bounded(n.parse().unwrap_or(1)),
        None => MaxOccurs::Bounded(1),
    }
}

fn resolve_type_ref(type_name: &str) -> String {
    if let Some((prefix, local)) = type_name.split_once(':') {
        if prefix == "xs" || prefix == "xsd" {
            format!("xs:{local}")
        } else {
            local.to_owned()
        }
    } else {
        type_name.to_owned()
    }
}

fn push_element_to_parent(stack: &mut [ParseState], elem: Element) {
    for state in stack.iter_mut().rev() {
        match state {
            ParseState::Sequence { elements }
            | ParseState::Choice { elements }
            | ParseState::Extension { elements, .. }
            | ParseState::ComplexType { elements, .. } => {
                elements.push(elem);
                return;
            }
            _ => {}
        }
    }
}

fn propagate_extension(
    stack: &mut [ParseState],
    base: String,
    ext_content: Option<Box<ExtensionContent>>,
) {
    for state in stack.iter_mut().rev() {
        if let ParseState::ComplexType { extension, .. } = state {
            *extension = Some((base, ext_content));
            return;
        }
    }
}

fn make_facet(local: &str, value: &str) -> Option<Facet> {
    match local {
        "enumeration" => Some(Facet::Enumeration(value.to_owned())),
        "pattern" => Some(Facet::Pattern(value.to_owned())),
        "minInclusive" => Some(Facet::MinInclusive(value.to_owned())),
        "maxInclusive" => Some(Facet::MaxInclusive(value.to_owned())),
        "minExclusive" => Some(Facet::MinExclusive(value.to_owned())),
        "maxExclusive" => Some(Facet::MaxExclusive(value.to_owned())),
        "minLength" => value.parse().ok().map(Facet::MinLength),
        "maxLength" => value.parse().ok().map(Facet::MaxLength),
        "length" => value.parse().ok().map(Facet::Length),
        "totalDigits" => value.parse().ok().map(Facet::TotalDigits),
        "fractionDigits" => value.parse().ok().map(Facet::FractionDigits),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::fs;

    pub fn parse_xsd(content: &str) -> Result<Schema, SchemaError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xsd");
        fs::write(&path, content).unwrap();
        parse(&path)
    }

    #[test]
    fn parse_minimal_schema_with_element_and_complex_type() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="https://www.vdl.afrl.af.mil/programs/oam"
           version="002.5.0">
  <xs:complexType name="TestType">
    <xs:sequence>
      <xs:element name="Field1" type="xs:string"/>
      <xs:element name="Field2" type="xs:int" minOccurs="0"/>
    </xs:sequence>
  </xs:complexType>
  <xs:element name="TestMessage" type="TestType"/>
</xs:schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.target_namespace.as_deref(),
            Some("https://www.vdl.afrl.af.mil/programs/oam")
        );
        assert_eq!(schema.version.as_deref(), Some("002.5.0"));
        assert!(schema.is_global_element("TestMessage"));
        assert_eq!(
            schema.global_elements.get("TestMessage").unwrap().type_name,
            "TestType"
        );

        let ct = schema.complex_types.get("TestType").unwrap();
        assert!(!ct.abstract_);
        let ComplexContent::Sequence { elements } = &ct.content else {
            panic!("expected Sequence, got {:?}", ct.content);
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].name, "Field1");
        assert_eq!(elements[0].min_occurs, 1);
        assert_eq!(elements[1].name, "Field2");
        assert_eq!(elements[1].min_occurs, 0);
    }

    #[test]
    fn parse_choice() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="ChoiceType">
    <xs:choice>
      <xs:element name="Option1" type="xs:string"/>
      <xs:element name="Option2" type="xs:int"/>
    </xs:choice>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("ChoiceType").unwrap();
        let ComplexContent::Choice { elements } = &ct.content else {
            panic!("expected Choice, got {:?}", ct.content);
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].name, "Option1");
        assert_eq!(elements[1].name, "Option2");
    }

    #[test]
    fn parse_extension() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="BaseType">
    <xs:sequence>
      <xs:element name="BaseField" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
  <xs:complexType name="DerivedType">
    <xs:complexContent>
      <xs:extension base="BaseType">
        <xs:sequence>
          <xs:element name="DerivedField" type="xs:int"/>
        </xs:sequence>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("DerivedType").unwrap();
        let ComplexContent::Extension { base, content } = &ct.content else {
            panic!("expected Extension, got {:?}", ct.content);
        };
        assert_eq!(base, "BaseType");
        let ext = content.as_ref().unwrap();
        let ExtensionContent::Sequence { elements } = ext.as_ref() else {
            panic!("expected Sequence, got {ext:?}");
        };
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].name, "DerivedField");

        let flat = schema.flatten_elements("DerivedType").unwrap();
        let names: Vec<&str> = flat.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["BaseField", "DerivedField"]);
    }

    #[test]
    fn parse_empty_extension() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="BaseType">
    <xs:sequence>
      <xs:element name="Field" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
  <xs:complexType name="EmptyDerived">
    <xs:complexContent>
      <xs:extension base="BaseType"/>
    </xs:complexContent>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("EmptyDerived").unwrap();
        let ComplexContent::Extension { base, content } = &ct.content else {
            panic!("expected Extension, got {:?}", ct.content);
        };
        assert_eq!(base, "BaseType");
        assert!(content.is_none());

        let flat = schema.flatten_elements("EmptyDerived").unwrap();
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].name, "Field");
    }

    #[test]
    fn parse_extension_with_choice() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="BaseType">
    <xs:sequence>
      <xs:element name="Id" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
  <xs:complexType name="QueryType">
    <xs:complexContent>
      <xs:extension base="BaseType">
        <xs:choice>
          <xs:element name="ByName" type="xs:string"/>
          <xs:element name="ById" type="xs:int"/>
        </xs:choice>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("QueryType").unwrap();
        let ComplexContent::Extension { base, content } = &ct.content else {
            panic!("expected Extension, got {:?}", ct.content);
        };
        assert_eq!(base, "BaseType");
        let ext = content.as_ref().unwrap();
        let ExtensionContent::Choice { elements } = ext.as_ref() else {
            panic!("expected Choice, got {ext:?}");
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].name, "ByName");
        assert_eq!(elements[1].name, "ById");
    }

    #[test]
    fn parse_simple_type_with_facets() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="StatusEnum">
    <xs:restriction base="xs:string">
      <xs:enumeration value="Active"/>
      <xs:enumeration value="Inactive"/>
      <xs:enumeration value="Unknown"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="IdPattern">
    <xs:restriction base="xs:string">
      <xs:pattern value="[A-Z]{3}-[0-9]{4}"/>
    </xs:restriction>
  </xs:simpleType>
</xs:schema>"#,
        )
        .unwrap();

        let status = schema.simple_types.get("StatusEnum").unwrap();
        assert_eq!(status.restriction.base, "xs:string");
        assert_eq!(status.restriction.facets.len(), 3);
        assert!(matches!(&status.restriction.facets[0], Facet::Enumeration(v) if v == "Active"));

        let pattern = schema.simple_types.get("IdPattern").unwrap();
        assert_eq!(pattern.restriction.facets.len(), 1);
        assert!(
            matches!(&pattern.restriction.facets[0], Facet::Pattern(v) if v == "[A-Z]{3}-[0-9]{4}")
        );
    }

    #[test]
    fn sibling_patterns_are_or() {
        use crate::facets::CompiledFacets;
        let facets = vec![Facet::PatternGroup(vec![
            " {11}".to_owned(),
            "[A-Z0-9]{11}".to_owned(),
            "[A-Z0-9]{2} {9}".to_owned(),
        ])];
        let (compiled, _) = CompiledFacets::compile(&facets);
        assert_eq!(compiled.pattern_sets.len(), 1);
        let set = &compiled.pattern_sets[0];
        assert!(set.is_match("           ")); // 11 spaces
        assert!(set.is_match("ABCDEFGHIJK"));
        assert!(set.is_match("JO         "));
    }

    #[test]
    fn sibling_patterns_do_not_all_need_to_match() {
        use crate::facets::CompiledFacets;
        let facets = vec![Facet::PatternGroup(vec![
            " {11}".to_owned(),
            "[A-Z0-9]{2} {9}".to_owned(),
        ])];
        let (compiled, _) = CompiledFacets::compile(&facets);
        let set = &compiled.pattern_sets[0];
        assert!(set.is_match("JO         "));
    }

    #[test]
    fn combined_pattern_is_whole_string_anchored() {
        use crate::facets::CompiledFacets;
        let facets = vec![Facet::PatternGroup(vec!["[A-Z0-9]{2} {9}".to_owned()])];
        let (compiled, _) = CompiledFacets::compile(&facets);
        let set = &compiled.pattern_sets[0];
        assert!(set.is_match("JO         "));
        assert!(!set.is_match("xJO         y"));
    }

    #[test]
    fn parse_multi_level_simple_type_chain() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="PositiveByteType">
    <xs:restriction base="xs:unsignedByte">
      <xs:minInclusive value="1"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="SmallPositiveByte">
    <xs:restriction base="PositiveByteType">
      <xs:maxInclusive value="100"/>
    </xs:restriction>
  </xs:simpleType>
</xs:schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.resolve_primitive("SmallPositiveByte").unwrap(),
            "xs:decimal"
        );
    }

    #[test]
    fn parse_min_max_occurs() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="ListType">
    <xs:sequence>
      <xs:element name="Required" type="xs:string"/>
      <xs:element name="Optional" type="xs:string" minOccurs="0"/>
      <xs:element name="Multiple" type="xs:string" maxOccurs="5"/>
      <xs:element name="Unbounded" type="xs:string" maxOccurs="unbounded"/>
      <xs:element name="OptionalList" type="xs:string" minOccurs="0" maxOccurs="unbounded"/>
    </xs:sequence>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("ListType").unwrap();
        let ComplexContent::Sequence { elements } = &ct.content else {
            panic!("expected Sequence");
        };
        assert_eq!(elements[0].min_occurs, 1);
        assert_eq!(elements[0].max_occurs, MaxOccurs::Bounded(1));
        assert_eq!(elements[1].min_occurs, 0);
        assert_eq!(elements[1].max_occurs, MaxOccurs::Bounded(1));
        assert_eq!(elements[2].max_occurs, MaxOccurs::Bounded(5));
        assert_eq!(elements[3].max_occurs, MaxOccurs::Unbounded);
        assert_eq!(elements[4].min_occurs, 0);
        assert_eq!(elements[4].max_occurs, MaxOccurs::Unbounded);
    }

    #[test]
    fn parse_anonymous_complex_type() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="ParentType">
    <xs:sequence>
      <xs:element name="Nested">
        <xs:complexType>
          <xs:sequence>
            <xs:element name="InnerField" type="xs:string"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>
    </xs:sequence>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("ParentType").unwrap();
        let ComplexContent::Sequence { elements } = &ct.content else {
            panic!("expected Sequence");
        };
        assert_eq!(elements[0].name, "Nested");
        let TypeRef::AnonymousComplex(inner_ct) = &elements[0].type_ref else {
            panic!("expected AnonymousComplex, got {:?}", elements[0].type_ref);
        };
        let ComplexContent::Sequence {
            elements: inner_elems,
        } = &inner_ct.content
        else {
            panic!("expected inner Sequence");
        };
        assert_eq!(inner_elems[0].name, "InnerField");
    }

    #[test]
    fn parse_anonymous_simple_type() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="ParentType">
    <xs:sequence>
      <xs:element name="Status">
        <xs:simpleType>
          <xs:restriction base="xs:string">
            <xs:enumeration value="On"/>
            <xs:enumeration value="Off"/>
          </xs:restriction>
        </xs:simpleType>
      </xs:element>
    </xs:sequence>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let ct = schema.complex_types.get("ParentType").unwrap();
        let ComplexContent::Sequence { elements } = &ct.content else {
            panic!("expected Sequence");
        };
        let TypeRef::AnonymousSimple(st) = &elements[0].type_ref else {
            panic!("expected AnonymousSimple, got {:?}", elements[0].type_ref);
        };
        assert_eq!(st.restriction.base, "xs:string");
        assert_eq!(st.restriction.facets.len(), 2);
    }

    #[test]
    fn parse_include() {
        let dir = tempfile::tempdir().unwrap();

        fs::write(
            dir.path().join("types.xsd"),
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="IncludedType">
    <xs:sequence>
      <xs:element name="Value" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        let main = dir.path().join("main.xsd");
        fs::write(
            &main,
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="types.xsd"/>
  <xs:element name="TestMsg" type="IncludedType"/>
</xs:schema>"#,
        )
        .unwrap();

        let schema = parse(&main).unwrap();
        assert!(schema.complex_types.contains_key("IncludedType"));
        assert!(schema.is_global_element("TestMsg"));
        assert_eq!(
            schema.global_elements.get("TestMsg").unwrap().type_name,
            "IncludedType"
        );
    }

    #[test]
    fn parse_circular_include_errors() {
        let dir = tempfile::tempdir().unwrap();

        fs::write(
            dir.path().join("a.xsd"),
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="b.xsd"/>
</xs:schema>"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("b.xsd"),
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="a.xsd"/>
</xs:schema>"#,
        )
        .unwrap();

        let result = parse(&dir.path().join("a.xsd"));
        assert!(matches!(result, Err(SchemaError::CircularInclude { .. })));
    }

    #[test]
    fn parse_reused_include_is_not_circular() {
        let dir = tempfile::tempdir().unwrap();

        fs::write(
            dir.path().join("common.xsd"),
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="SharedType">
    <xs:sequence>
      <xs:element name="Value" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("a.xsd"),
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="common.xsd"/>
  <xs:element name="MsgA" type="SharedType"/>
</xs:schema>"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("b.xsd"),
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="common.xsd"/>
  <xs:element name="MsgB" type="SharedType"/>
</xs:schema>"#,
        )
        .unwrap();

        let main = dir.path().join("main.xsd");
        fs::write(
            &main,
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="a.xsd"/>
  <xs:include schemaLocation="b.xsd"/>
</xs:schema>"#,
        )
        .unwrap();

        let schema = parse(&main).unwrap();
        assert!(schema.complex_types.contains_key("SharedType"));
        assert!(schema.is_global_element("MsgA"));
        assert!(schema.is_global_element("MsgB"));
    }

    #[test]
    fn parse_abstract_type() {
        let schema = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="AbstractBase" abstract="true">
    <xs:sequence>
      <xs:element name="Id" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
  <xs:complexType name="ConcreteType">
    <xs:sequence>
      <xs:element name="Name" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
</xs:schema>"#,
        )
        .unwrap();

        assert!(schema.complex_types.get("AbstractBase").unwrap().abstract_);
        assert!(!schema.complex_types.get("ConcreteType").unwrap().abstract_);
    }

    #[test]
    fn parse_error_malformed_xml() {
        let result = parse_xsd(
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="BrokenType">
    <xs:sequence>
      <xs:element name="Field" type="xs:string"/>
  </xs:complexType>
</xs:schema>"#,
        );

        assert!(matches!(result, Err(SchemaError::Xml { .. })));
    }

    #[test]
    fn parse_error_missing_file() {
        let result = parse(Path::new("/nonexistent/schema.xsd"));
        assert!(matches!(result, Err(SchemaError::Io { .. })));
    }

    // -- Verification test (real UCI v2.5 XSD) --

    #[test]
    fn verify_uci_v25_schema() {
        let Ok(schema_path) = std::env::var("SLEET_SCHEMA_PATH") else {
            eprintln!("SLEET_SCHEMA_PATH not set, skipping UCI verification test");
            return;
        };

        let start = std::time::Instant::now();
        let schema = parse(Path::new(&schema_path)).unwrap();
        let elapsed = start.elapsed();

        println!("Parse time: {elapsed:?}");
        println!("Global elements: {}", schema.global_elements.len());

        let named_complex = schema
            .complex_types
            .values()
            .filter(|ct| !ct.name.is_empty())
            .count();
        println!("Complex types: {named_complex}");

        let user_simple = schema
            .simple_types
            .values()
            .filter(|st| st.name.as_ref().is_some_and(|n| !n.starts_with("xs:")))
            .count();
        println!("Simple types (user-defined): {user_simple}");

        let abstract_count = schema
            .complex_types
            .values()
            .filter(|ct| ct.abstract_)
            .count();
        println!("Abstract types: {abstract_count}");

        assert_eq!(
            schema.global_elements.len(),
            722,
            "expected 722 global elements"
        );
        assert_eq!(named_complex, 4612, "expected 4612 complex types");
        assert_eq!(user_simple, 945, "expected 945 simple types");
        assert_eq!(abstract_count, 70, "expected 70 abstract types");

        let facet_count = schema.facet_count();
        println!("Facets: {facet_count}");
        assert!(facet_count > 0, "expected at least one facet in UCI schema");

        println!("Compiled facets: {}", schema.compiled_facets.len());
        assert_eq!(
            schema.compiled_facets.len(),
            926,
            "expected 926 user-defined simple types with effective facets"
        );

        let mut facet_compile_warnings = Vec::new();
        for name in schema
            .simple_types
            .keys()
            .filter(|name| !name.starts_with("xs:"))
        {
            let chain = schema.collect_facet_chain(name);
            let (_, warnings) = crate::facets::CompiledFacets::compile(&chain);
            facet_compile_warnings.extend(warnings);
        }
        assert!(
            facet_compile_warnings.is_empty(),
            "unexpected facet compile warnings: {facet_compile_warnings:?}"
        );

        let uuid_facets = schema
            .compiled_facets
            .get("UniversallyUniqueIdentifierType")
            .expect("UUID type should have compiled facets");
        assert!(
            !uuid_facets.pattern_sets.is_empty(),
            "expected UUID pattern facet"
        );

        let has_enumeration_facets = schema
            .compiled_facets
            .values()
            .any(|facets| !facets.enumerations.is_empty());
        assert!(
            has_enumeration_facets,
            "expected at least one compiled type with enumeration facets"
        );

        assert_eq!(
            schema.target_namespace.as_deref(),
            Some("https://www.vdl.afrl.af.mil/programs/oam")
        );
        assert_eq!(schema.version.as_deref(), Some("002.5.0"));

        assert!(elapsed.as_secs() < 5, "parse took too long: {elapsed:?}");
    }
}

#[cfg(test)]
mod additional_tests {
    use crate::schema::parser::tests::parse_xsd;
    use serde_json::json;

    #[test]
    fn test_inherited_patterns_and_semantics() {
        let schema_xml = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
<xs:simpleType name="BaseType">
<xs:restriction base="xs:string">
  <xs:pattern value="[A-Z]{3}"/>
</xs:restriction>
</xs:simpleType>
<xs:simpleType name="DerivedType">
<xs:restriction base="BaseType">
  <xs:pattern value="AB."/>
</xs:restriction>
</xs:simpleType>
</xs:schema>"#;

        let schema = parse_xsd(schema_xml).unwrap();
        let chain = schema.collect_facet_chain("DerivedType");
        let (compiled, _) = crate::facets::CompiledFacets::compile(&chain);

        assert!(
            compiled
                .validate(&json!("ABC"), "xs:string", "root")
                .is_ok()
        );

        let result = compiled.validate(&json!("XYZ"), "xs:string", "root");
        assert!(
            result.is_err(),
            "XYZ should fail the derived pattern 'AB.' but it incorrectly passed!"
        );
    }
}
