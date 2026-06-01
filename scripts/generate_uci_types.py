#!/usr/bin/env python3
"""Discover and generate Rust helpers for the Sleet UCI v2.5 subset."""

from __future__ import annotations

import argparse
import json
import re
import sys
import unittest
import xml.etree.ElementTree as ET
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Literal

XS_NS = "http://www.w3.org/2001/XMLSchema"
XS = f"{{{XS_NS}}}"
DEFAULT_SCHEMA = Path("spec/002.5.0/UCI_MessageDefinitions_v2_5_0.xsd")

TARGET_MESSAGES: tuple[str, ...] = (
    "PositionReport",
    "SystemStatus",
    "ServiceStatus",
    "SubsystemStatus",
    "OperatorNotification",
    "ESM_Activity",
    "SignalReport",
    "PO_Activity",
    "ObservationMeasurementReport",
    "PositionReportDetailed",
    "NavigationReport",
    "RoutePlan",
    "RouteActivityPlan",
    "RoutePlanExecutionStatus",
)

OUT_OF_SCOPE_GLOBAL_MESSAGES: frozenset[str] = frozenset(
    {
        "OperatorNotificationAck",
        "OperatorNotificationOverride",
        "SubsystemStatusDetailed",
    }
)

XSD_BUILTIN_TYPES: frozenset[str] = frozenset(
    {
        "xs:anySimpleType",
        "xs:anyType",
        "xs:anyURI",
        "xs:base64Binary",
        "xs:boolean",
        "xs:byte",
        "xs:date",
        "xs:dateTime",
        "xs:decimal",
        "xs:double",
        "xs:duration",
        "xs:float",
        "xs:gDay",
        "xs:gMonth",
        "xs:gMonthDay",
        "xs:gYear",
        "xs:gYearMonth",
        "xs:hexBinary",
        "xs:ID",
        "xs:IDREF",
        "xs:int",
        "xs:integer",
        "xs:language",
        "xs:long",
        "xs:Name",
        "xs:NCName",
        "xs:negativeInteger",
        "xs:NMTOKEN",
        "xs:nonNegativeInteger",
        "xs:nonPositiveInteger",
        "xs:normalizedString",
        "xs:positiveInteger",
        "xs:QName",
        "xs:short",
        "xs:string",
        "xs:time",
        "xs:token",
        "xs:unsignedByte",
        "xs:unsignedInt",
        "xs:unsignedLong",
        "xs:unsignedShort",
    }
)

RUST_KEYWORDS: frozenset[str] = frozenset(
    {
        "as",
        "async",
        "await",
        "break",
        "const",
        "continue",
        "crate",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "fn",
        "for",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "match",
        "mod",
        "move",
        "mut",
        "pub",
        "ref",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "type",
        "unsafe",
        "use",
        "where",
        "while",
        "yield",
    }
)

RUST_PRIMITIVES: dict[str, str] = {
    "xs:anySimpleType": "serde_json::Value",
    "xs:anyType": "serde_json::Value",
    "xs:anyURI": "String",
    "xs:base64Binary": "String",
    "xs:boolean": "bool",
    "xs:byte": "i8",
    "xs:date": "String",
    "xs:dateTime": "String",
    "xs:decimal": "f64",
    "xs:double": "f64",
    "xs:duration": "String",
    "xs:float": "f64",
    "xs:gDay": "String",
    "xs:gMonth": "String",
    "xs:gMonthDay": "String",
    "xs:gYear": "String",
    "xs:gYearMonth": "String",
    "xs:hexBinary": "String",
    "xs:ID": "String",
    "xs:IDREF": "String",
    "xs:int": "i32",
    "xs:integer": "i64",
    "xs:language": "String",
    "xs:long": "i64",
    "xs:Name": "String",
    "xs:NCName": "String",
    "xs:negativeInteger": "i64",
    "xs:NMTOKEN": "String",
    "xs:nonNegativeInteger": "u64",
    "xs:nonPositiveInteger": "i64",
    "xs:normalizedString": "String",
    "xs:positiveInteger": "u64",
    "xs:QName": "String",
    "xs:short": "i16",
    "xs:string": "String",
    "xs:time": "String",
    "xs:token": "String",
    "xs:unsignedByte": "u8",
    "xs:unsignedInt": "u32",
    "xs:unsignedLong": "u64",
    "xs:unsignedShort": "u16",
}

ContentKind = Literal[
    "empty", "sequence", "choice", "extension_sequence", "extension_choice"
]


@dataclass(frozen=True)
class GlobalElementDecl:
    """Describe a top-level XSD element declaration."""

    name: str
    type_name: str
    source: Path
    documentation: str | None


@dataclass(frozen=True)
class ElementDecl:
    """Describe an XSD element inside a complex type model group."""

    name: str
    type_name: str | None
    min_occurs: int
    max_occurs: int | None
    documentation: str | None

    @property
    def is_optional(self) -> bool:
        """Return whether the element may be absent."""
        return self.min_occurs == 0

    @property
    def is_repeated(self) -> bool:
        """Return whether the element serializes as an array."""
        return self.max_occurs is None or self.max_occurs > 1


@dataclass(frozen=True)
class ComplexTypeDecl:
    """Describe a named XSD complex type declaration."""

    name: str
    source: Path
    abstract: bool
    content_kind: ContentKind
    base_type: str | None
    elements: tuple[ElementDecl, ...]
    documentation: str | None

    @property
    def base_types(self) -> tuple[str, ...]:
        """Return extension base types referenced by this type."""
        return () if self.base_type is None else (self.base_type,)

    @property
    def element_types(self) -> tuple[str, ...]:
        """Return element type references contained directly by this type."""
        return tuple(
            sorted(
                {element.type_name for element in self.elements if element.type_name}
            )
        )

    @property
    def choice_elements(self) -> tuple[str, ...]:
        """Return choice element names when this type contains a choice group."""
        if self.content_kind not in {"choice", "extension_choice"}:
            return ()
        return tuple(element.name for element in self.elements)

    @property
    def has_choice(self) -> bool:
        """Return whether this type contains an XSD choice group."""
        return bool(self.choice_elements)


@dataclass(frozen=True)
class EnumValueDecl:
    """Describe a single XSD enumeration facet."""

    value: str
    documentation: str | None


@dataclass(frozen=True)
class SimpleTypeDecl:
    """Describe a named XSD simple type declaration."""

    name: str
    source: Path
    base_types: tuple[str, ...]
    enumerations: tuple[EnumValueDecl, ...]
    documentation: str | None

    @property
    def is_enum(self) -> bool:
        """Return whether this type restricts values with enumerations."""
        return bool(self.enumerations)


@dataclass
class SchemaIndex:
    """Index the XSD declarations needed for generator discovery."""

    global_elements: dict[str, GlobalElementDecl] = field(default_factory=dict)
    complex_types: dict[str, ComplexTypeDecl] = field(default_factory=dict)
    simple_types: dict[str, SimpleTypeDecl] = field(default_factory=dict)
    parsed_files: list[Path] = field(default_factory=list)


@dataclass(frozen=True)
class DiscoveryResult:
    """Capture the deterministic transitive closure for target messages."""

    targets: tuple[GlobalElementDecl, ...]
    complex_types: tuple[str, ...]
    simple_types: tuple[str, ...]
    enum_types: tuple[str, ...]
    choice_types: tuple[str, ...]
    inherited_base_types: tuple[str, ...]
    builtin_types: tuple[str, ...]
    unresolved_types: tuple[str, ...]
    parsed_files: tuple[Path, ...]


@dataclass(frozen=True)
class RustField:
    """Describe a generated Rust struct field."""

    source_name: str
    rust_name: str
    rust_type: str
    optional: bool
    repeated: bool
    documentation: str | None


@dataclass(frozen=True)
class RustChoiceVariant:
    """Describe a generated Rust choice enum variant."""

    source_name: str
    variant_name: str
    field_name: str
    rust_type: str
    documentation: str | None


@dataclass(frozen=True)
class RustEnumVariant:
    """Describe a generated Rust simple enumeration variant."""

    source_name: str
    variant_name: str
    documentation: str | None


@dataclass(frozen=True)
class RustGenerator:
    """Generate deterministic Rust source from a discovered XSD closure."""

    index: SchemaIndex
    discovery: DiscoveryResult

    def render(self) -> str:
        """Render the discovered UCI subset as Rust source."""
        self.validate_supported_closure()
        lines: list[str] = [
            "//! Provide generated UCI v2.5 helper types for the supported UCI message subset.",
            "//!",
            "//! This file is generated from the local UCI v2.5 XSD subset.",
            "",
            "#![allow(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]",
            "",
            "use serde::{Deserialize, Serialize};",
            "",
        ]
        for name in self.discovery.simple_types:
            decl = self.index.simple_types[name]
            if decl.is_enum:
                lines.extend(self.render_simple_enum(decl))
            else:
                lines.extend(self.render_simple_alias(decl))
        for name in self.discovery.complex_types:
            decl = self.index.complex_types[name]
            lines.extend(self.render_complex_type(decl))
        lines.extend(self.render_uci_message_enum())
        return "\n".join(lines).rstrip() + "\n"

    def render_uci_message_enum(self) -> list[str]:
        """Render the top-level UciMessage enum for all target messages."""
        lines: list[str] = []
        lines.extend(
            render_rust_doc(
                None, "Represent the top-level envelope for all supported UCI messages."
            )
        )
        lines.append("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]")
        lines.append("pub enum UciMessage {")
        used: set[str] = set()
        for target in self.discovery.targets:
            variant_name = unique_name(rust_variant_name(target.name), used)
            rust_type = self.rust_type_for_type_name(target.type_name)
            lines.extend(
                indent_lines(
                    render_rust_doc(
                        target.documentation, f"Carry `{target.name}`."
                    )
                )
            )
            if variant_name != target.name:
                lines.append(f"    #[serde(rename = {rust_string(target.name)})]")
            lines.append(f"    {variant_name}({rust_type}),")
        lines.append("}")
        lines.append("")
        return lines

    def validate_supported_closure(self) -> None:
        """Fail when the discovered closure contains unsupported references."""
        if self.discovery.unresolved_types:
            names = ", ".join(self.discovery.unresolved_types)
            raise SystemExit(f"unresolved non-built-in type references: {names}")
        unsupported: list[str] = []
        for type_name in self.discovery.complex_types:
            decl = self.index.complex_types[type_name]
            for element in decl.elements:
                if element.type_name is None:
                    unsupported.append(f"{type_name}.{element.name}")
        if unsupported:
            names = ", ".join(sorted(unsupported))
            raise SystemExit(
                f"unsupported anonymous or missing element type(s): {names}"
            )

    def render_simple_alias(self, decl: SimpleTypeDecl) -> list[str]:
        """Render a non-enum simple type as a Rust type alias."""
        rust_type = self.resolve_simple_base_type(decl)
        lines: list[str] = []
        lines.extend(
            render_rust_doc(
                decl.documentation, f"Alias the UCI {decl.name} simple type."
            )
        )
        lines.append(f"pub type {rust_type_name(decl.name)} = {rust_type};")
        lines.append("")
        return lines

    def render_simple_enum(self, decl: SimpleTypeDecl) -> list[str]:
        """Render an XSD enumeration simple type as a Rust enum."""
        lines: list[str] = []
        lines.extend(
            render_rust_doc(
                decl.documentation, f"Represent the UCI {decl.name} enumeration."
            )
        )
        lines.append("#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]")
        lines.append(f"pub enum {rust_type_name(decl.name)} {{")
        used: set[str] = set()
        for value in decl.enumerations:
            variant_name = unique_name(rust_variant_name(value.value), used)
            lines.extend(
                indent_lines(
                    render_rust_doc(value.documentation, f"Represent `{value.value}`.")
                )
            )
            lines.append(f"    #[serde(rename = {rust_string(value.value)})]")
            lines.append(f"    {variant_name},")
        lines.append("}")
        lines.append("")
        return lines

    def render_complex_type(self, decl: ComplexTypeDecl) -> list[str]:
        """Render a complex type as a Rust struct or choice enum."""
        if decl.content_kind == "choice" and decl.base_type is None:
            return self.render_choice_enum(decl, rust_type_name(decl.name))
        return self.render_complex_struct(decl)

    def render_complex_struct(self, decl: ComplexTypeDecl) -> list[str]:
        """Render a complex type as a Rust struct."""
        fields = self.flatten_fields(decl)
        choice = self.choice_for_struct(decl)
        lines: list[str] = []
        lines.extend(
            render_rust_doc(
                decl.documentation, f"Represent the UCI {decl.name} complex type."
            )
        )
        lines.append("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]")
        if not fields and choice is None:
            lines.append(f"pub struct {rust_type_name(decl.name)} {{}}")
            lines.append("")
            return lines
        lines.append(f"pub struct {rust_type_name(decl.name)} {{")
        for field in fields:
            lines.extend(
                indent_lines(
                    render_rust_doc(
                        field.documentation, f"Carry `{field.source_name}`."
                    )
                )
            )
            lines.append(f"    #[serde(rename = {rust_string(field.source_name)})]")
            if field.optional:
                if field.repeated:
                    lines.append(
                        '    #[serde(default, skip_serializing_if = "Vec::is_empty")]'
                    )
                else:
                    lines.append(
                        '    #[serde(skip_serializing_if = "Option::is_none")]'
                    )
            lines.extend(
                render_rust_field_declaration(
                    f"pub {field.rust_name}", field.rust_type, spaces=4
                )
            )
        if choice is not None:
            enum_name, variants = choice
            lines.extend(
                indent_lines(render_rust_doc(None, "Carry exactly one choice member."))
            )
            lines.append("    #[serde(flatten)]")
            lines.append(f"    pub choice: {enum_name},")
        lines.append("}")
        lines.append("")
        if choice is not None:
            enum_name, variants = choice
            lines.extend(
                self.render_choice_variants(enum_name, variants, decl.documentation)
            )
        return lines

    def render_choice_enum(self, decl: ComplexTypeDecl, enum_name: str) -> list[str]:
        """Render a choice-only complex type as a Rust enum."""
        variants = tuple(self.choice_variant(element) for element in decl.elements)
        return self.render_choice_variants(enum_name, variants, decl.documentation)

    def render_choice_variants(
        self,
        enum_name: str,
        variants: tuple[RustChoiceVariant, ...],
        documentation: str | None,
    ) -> list[str]:
        """Render choice variants as an untagged serde enum."""
        lines: list[str] = []
        lines.extend(
            render_rust_doc(
                documentation, f"Represent one member of the UCI {enum_name} choice."
            )
        )
        lines.append("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]")
        lines.append("#[serde(untagged, deny_unknown_fields)]")
        lines.append(f"pub enum {enum_name} {{")
        used: set[str] = set()
        for variant in variants:
            variant_name = unique_name(variant.variant_name, used)
            lines.extend(
                indent_lines(
                    render_rust_doc(
                        variant.documentation, f"Select `{variant.source_name}`."
                    )
                )
            )
            lines.append(f"    {variant_name} {{")
            lines.extend(
                indent_lines(
                    render_rust_doc(
                        variant.documentation, f"Carry `{variant.source_name}`."
                    ),
                    spaces=8,
                )
            )
            lines.append(
                f"        #[serde(rename = {rust_string(variant.source_name)})]"
            )
            lines.extend(
                render_rust_field_declaration(
                    variant.field_name, variant.rust_type, spaces=8
                )
            )
            lines.append("    },")
        lines.append("}")
        lines.append("")
        return lines

    def flatten_fields(self, decl: ComplexTypeDecl) -> tuple[RustField, ...]:
        """Collect inherited and direct sequence fields for a generated struct."""
        fields: list[RustField] = []
        if decl.base_type is not None:
            base = self.index.complex_types.get(decl.base_type)
            if base is None:
                raise SystemExit(f"unknown complex base type: {decl.base_type}")
            fields.extend(self.flatten_fields(base))
        if decl.content_kind in {"sequence", "extension_sequence"}:
            fields.extend(self.rust_field(element) for element in decl.elements)
        return tuple(fields)

    def choice_for_struct(
        self,
        decl: ComplexTypeDecl,
    ) -> tuple[str, tuple[RustChoiceVariant, ...]] | None:
        """Return the flattened choice enum used by a struct when needed."""
        if decl.content_kind not in {"extension_choice"}:
            return None
        enum_name = f"{rust_type_name(decl.name)}Choice"
        return enum_name, tuple(
            self.choice_variant(element) for element in decl.elements
        )

    def rust_field(self, element: ElementDecl) -> RustField:
        """Convert an XSD element declaration into a Rust field declaration."""
        rust_type = self.rust_type_for_element(element)
        return RustField(
            source_name=element.name,
            rust_name=rust_field_name(element.name),
            rust_type=rust_type,
            optional=element.is_optional,
            repeated=element.is_repeated,
            documentation=element.documentation,
        )

    def choice_variant(self, element: ElementDecl) -> RustChoiceVariant:
        """Convert a choice element declaration into a Rust enum variant."""
        return RustChoiceVariant(
            source_name=element.name,
            variant_name=rust_variant_name(element.name),
            field_name=rust_field_name(element.name),
            rust_type=self.rust_type_for_element(element),
            documentation=element.documentation,
        )

    def rust_type_for_element(self, element: ElementDecl) -> str:
        """Return the Rust type used for an XSD element occurrence."""
        base_type = self.rust_type_for_type_name(element.type_name)
        if element.is_repeated:
            return f"Vec<{base_type}>"
        if element.is_optional:
            return f"Option<{base_type}>"
        return base_type

    def rust_type_for_type_name(self, type_name: str | None) -> str:
        """Return the Rust type name for an XSD named type."""
        if type_name is None:
            raise SystemExit("cannot map anonymous XSD element to Rust")
        if type_name in RUST_PRIMITIVES:
            return RUST_PRIMITIVES[type_name]
        if simple := self.index.simple_types.get(type_name):
            return rust_type_name(simple.name)
        if complex_type := self.index.complex_types.get(type_name):
            return rust_type_name(complex_type.name)
        raise SystemExit(f"cannot map unknown XSD type to Rust: {type_name}")

    def resolve_simple_base_type(self, decl: SimpleTypeDecl) -> str:
        """Return the Rust primitive used by a non-enum simple type."""
        for base_type in decl.base_types:
            if base_type in RUST_PRIMITIVES:
                return RUST_PRIMITIVES[base_type]
            if base_type in self.index.simple_types:
                return rust_type_name(base_type)
        return "serde_json::Value"


def parse_args(argv: list[str]) -> argparse.Namespace:
    """Parse command-line arguments for discovery, generation, and validation."""
    parser = argparse.ArgumentParser(
        description="Discover or generate Rust for the transitive UCI v2.5 XSD type closure used by Sleet helper messages.",
    )
    parser.add_argument(
        "--schema",
        type=Path,
        default=DEFAULT_SCHEMA,
        help=f"Path to UCI_MessageDefinitions XSD (default: {DEFAULT_SCHEMA})",
    )
    parser.add_argument(
        "--generate-rust",
        action="store_true",
        help="Generate deterministic Rust source for the discovered UCI subset instead of printing the discovery report.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="Optional path to write generated Rust source; stdout is used when omitted.",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run script unit tests instead of printing discovery or generated Rust output.",
    )
    return parser.parse_args(argv)


def load_schema_index(root_path: Path) -> SchemaIndex:
    """Load an XSD file and local includes into a declaration index."""
    index = SchemaIndex()
    active: set[Path] = set()
    parsed: set[Path] = set()
    parse_schema_file(root_path, index, parsed, active)
    index.parsed_files.sort(key=lambda path: path.as_posix())
    return index


def parse_schema_file(
    path: Path,
    index: SchemaIndex,
    parsed: set[Path],
    active: set[Path],
) -> None:
    """Parse a single XSD file and recursively parse local includes."""
    try:
        canonical = path.resolve(strict=True)
    except OSError as error:
        raise SystemExit(f"failed to read schema '{path}': {error}") from error

    if canonical in active:
        raise SystemExit(f"circular xs:include detected at '{canonical}'")
    if canonical in parsed:
        return

    active.add(canonical)
    try:
        try:
            tree = ET.parse(canonical)
        except ET.ParseError as error:
            raise SystemExit(f"failed to parse XML '{canonical}': {error}") from error
        except OSError as error:
            raise SystemExit(f"failed to read schema '{canonical}': {error}") from error

        root = tree.getroot()
        for include in root.findall(f"{XS}include"):
            location = include.get("schemaLocation")
            if location:
                parse_schema_file(canonical.parent / location, index, parsed, active)

        index_top_level_declarations(root, canonical, index)
        index.parsed_files.append(canonical)
        parsed.add(canonical)
    finally:
        active.remove(canonical)


def index_top_level_declarations(
    root: ET.Element, source: Path, index: SchemaIndex
) -> None:
    """Index global elements and named types from an XSD schema root."""
    for child in root:
        if child.tag == f"{XS}element":
            decl = parse_global_element(child, source)
            if decl is not None:
                index.global_elements[decl.name] = decl
        elif child.tag == f"{XS}complexType":
            decl = parse_complex_type(child, source)
            if decl is not None:
                index.complex_types[decl.name] = decl
        elif child.tag == f"{XS}simpleType":
            decl = parse_simple_type(child, source)
            if decl is not None:
                index.simple_types[decl.name] = decl


def parse_global_element(element: ET.Element, source: Path) -> GlobalElementDecl | None:
    """Parse a top-level XSD element declaration."""
    name = element.get("name")
    type_name = normalize_type_name(element.get("type"))
    if not name or not type_name:
        return None
    return GlobalElementDecl(
        name=name,
        type_name=type_name,
        source=source,
        documentation=extract_documentation(element),
    )


def parse_complex_type(element: ET.Element, source: Path) -> ComplexTypeDecl | None:
    """Parse a named XSD complex type declaration."""
    name = element.get("name")
    if not name:
        return None

    abstract = element.get("abstract") == "true"
    documentation = extract_documentation(element)
    complex_content = first_child(element, "complexContent")
    if complex_content is not None:
        extension = first_child(complex_content, "extension")
        if extension is None:
            return ComplexTypeDecl(
                name, source, abstract, "empty", None, (), documentation
            )
        base_type = normalize_type_name(extension.get("base"))
        content_kind, elements = parse_model_group(extension, extension=True)
        return ComplexTypeDecl(
            name,
            source,
            abstract,
            content_kind,
            base_type,
            tuple(elements),
            documentation,
        )

    content_kind, elements = parse_model_group(element, extension=False)
    return ComplexTypeDecl(
        name,
        source,
        abstract,
        content_kind,
        None,
        tuple(elements),
        documentation,
    )


def parse_model_group(
    parent: ET.Element,
    *,
    extension: bool,
) -> tuple[ContentKind, list[ElementDecl]]:
    """Parse direct XSD sequence or choice content from a parent element."""
    sequence = first_child(parent, "sequence")
    if sequence is not None:
        kind: ContentKind = "extension_sequence" if extension else "sequence"
        return kind, collect_model_elements(sequence)
    choice = first_child(parent, "choice")
    if choice is not None:
        kind = "extension_choice" if extension else "choice"
        return kind, collect_model_elements(choice)
    return "empty", []


def collect_model_elements(group: ET.Element) -> list[ElementDecl]:
    """Collect element declarations from an XSD model group in schema order."""
    elements: list[ElementDecl] = []
    for child in group:
        if child.tag == f"{XS}element":
            decl = parse_nested_element(child)
            if decl is not None:
                elements.append(decl)
        elif child.tag in {f"{XS}sequence", f"{XS}choice", f"{XS}all"}:
            elements.extend(collect_model_elements(child))
    return elements


def parse_nested_element(element: ET.Element) -> ElementDecl | None:
    """Parse an element declaration nested inside a complex type."""
    name = element.get("name")
    type_name = normalize_type_name(element.get("type"))
    if not name:
        return None
    return ElementDecl(
        name=name,
        type_name=type_name,
        min_occurs=parse_occurs(element.get("minOccurs"), default=1),
        max_occurs=parse_max_occurs(element.get("maxOccurs")),
        documentation=extract_documentation(element),
    )


def parse_occurs(value: str | None, *, default: int) -> int:
    """Parse an XSD occurrence count."""
    if value is None:
        return default
    try:
        return int(value)
    except ValueError as error:
        raise SystemExit(f"invalid xs:occurs value '{value}'") from error


def parse_max_occurs(value: str | None) -> int | None:
    """Parse an XSD maxOccurs value."""
    if value == "unbounded":
        return None
    return parse_occurs(value, default=1)


def parse_simple_type(element: ET.Element, source: Path) -> SimpleTypeDecl | None:
    """Parse a named XSD simple type declaration."""
    name = element.get("name")
    if not name:
        return None

    base_types = tuple(
        filter(
            None,
            (
                normalize_type_name(restriction.get("base"))
                for restriction in element.findall(f".//{XS}restriction")
            ),
        )
    )
    enumerations = tuple(
        EnumValueDecl(
            value=enumeration.get("value") or "",
            documentation=extract_documentation(enumeration),
        )
        for enumeration in element.findall(f".//{XS}enumeration")
    )
    return SimpleTypeDecl(
        name=name,
        source=source,
        base_types=base_types,
        enumerations=enumerations,
        documentation=extract_documentation(element),
    )


def discover_type_closure(
    index: SchemaIndex,
    target_messages: Iterable[str] = TARGET_MESSAGES,
) -> DiscoveryResult:
    """Discover all named types reachable from the configured target messages."""
    targets = resolve_target_messages(index, target_messages)
    queue: deque[str] = deque(target.type_name for target in targets)
    complex_types: set[str] = set()
    simple_types: set[str] = set()
    inherited_base_types: set[str] = set()
    builtin_types: set[str] = set()
    unresolved_types: set[str] = set()

    while queue:
        type_name = queue.popleft()
        if type_name in XSD_BUILTIN_TYPES:
            builtin_types.add(type_name)
            continue
        if type_name in complex_types or type_name in simple_types:
            continue

        complex_decl = index.complex_types.get(type_name)
        if complex_decl is not None:
            complex_types.add(type_name)
            inherited_base_types.update(
                base
                for base in complex_decl.base_types
                if base not in XSD_BUILTIN_TYPES
            )
            queue.extend(complex_decl.base_types)
            queue.extend(complex_decl.element_types)
            continue

        simple_decl = index.simple_types.get(type_name)
        if simple_decl is not None:
            simple_types.add(type_name)
            inherited_base_types.update(
                base for base in simple_decl.base_types if base not in XSD_BUILTIN_TYPES
            )
            queue.extend(simple_decl.base_types)
            continue

        unresolved_types.add(type_name)

    enum_types = sorted(
        name
        for name in simple_types
        if (decl := index.simple_types.get(name)) is not None and decl.is_enum
    )
    choice_types = sorted(
        name
        for name in complex_types
        if (decl := index.complex_types.get(name)) is not None and decl.has_choice
    )

    return DiscoveryResult(
        targets=targets,
        complex_types=tuple(sorted(complex_types)),
        simple_types=tuple(sorted(simple_types)),
        enum_types=tuple(enum_types),
        choice_types=tuple(choice_types),
        inherited_base_types=tuple(sorted(inherited_base_types)),
        builtin_types=tuple(sorted(builtin_types)),
        unresolved_types=tuple(sorted(unresolved_types)),
        parsed_files=tuple(index.parsed_files),
    )


def resolve_target_messages(
    index: SchemaIndex,
    target_messages: Iterable[str],
) -> tuple[GlobalElementDecl, ...]:
    """Resolve configured target message names to global XSD declarations."""
    targets: list[GlobalElementDecl] = []
    missing: list[str] = []
    for message_name in target_messages:
        decl = index.global_elements.get(message_name)
        if decl is None:
            missing.append(message_name)
        else:
            targets.append(decl)

    if missing:
        names = ", ".join(sorted(missing))
        raise SystemExit(f"target global element(s) not found: {names}")
    return tuple(targets)


def render_discovery_report(result: DiscoveryResult) -> str:
    """Render a deterministic text report for review."""
    lines: list[str] = [
        "# UCI v2.5 Type Discovery",
        "",
        "## Parsed Files",
        *[f"- {display_path(path)}" for path in result.parsed_files],
        "",
        "## Target Global Elements",
        *[f"- {target.name}: {target.type_name}" for target in result.targets],
        "",
        "## Closure Summary",
        f"- Complex types: {len(result.complex_types)}",
        f"- Simple types: {len(result.simple_types)}",
        f"- Enum simple types: {len(result.enum_types)}",
        f"- Choice complex types: {len(result.choice_types)}",
        f"- Inherited base types: {len(result.inherited_base_types)}",
        f"- XSD built-in leaves: {len(result.builtin_types)}",
        f"- Unresolved references: {len(result.unresolved_types)}",
        "",
    ]
    lines.extend(render_name_section("Complex Types", result.complex_types))
    lines.extend(render_name_section("Simple Types", result.simple_types))
    lines.extend(render_name_section("Enum Simple Types", result.enum_types))
    lines.extend(render_name_section("Choice Complex Types", result.choice_types))
    lines.extend(
        render_name_section("Inherited Base Types", result.inherited_base_types)
    )
    lines.extend(render_name_section("XSD Built-in Leaves", result.builtin_types))
    lines.extend(render_name_section("Unresolved References", result.unresolved_types))
    return "\n".join(lines).rstrip() + "\n"


def render_name_section(title: str, names: tuple[str, ...]) -> list[str]:
    """Render a sorted name section for the discovery report."""
    lines = [f"## {title}"]
    if names:
        lines.extend(f"- {name}" for name in names)
    else:
        lines.append("- none")
    lines.append("")
    return lines


def render_rust_doc(documentation: str | None, fallback: str) -> list[str]:
    """Render a Rust doc comment from XSD documentation."""
    text = escape_rustdoc_text(documentation or fallback)
    sentences = wrap_doc_text(text)
    return [f"/// {line}" if line else "///" for line in sentences]


def escape_rustdoc_text(text: str) -> str:
    """Escape XSD prose that rustdoc would otherwise parse as links."""
    return text.replace("[", r"\[").replace("]", r"\]")


def render_rust_field_declaration(
    field_name: str, rust_type: str, *, spaces: int
) -> list[str]:
    """Render a Rust field declaration using stable rustfmt-compatible wrapping."""
    prefix = " " * spaces
    one_line = f"{prefix}{field_name}: {rust_type},"
    if len(one_line) <= 100:
        return [one_line]
    continuation = " " * (spaces + 4)
    return [f"{prefix}{field_name}:", f"{continuation}{rust_type},"]


def wrap_doc_text(text: str, *, width: int = 96) -> list[str]:
    """Wrap documentation text to stable Rust doc comment lines."""
    words = text.split()
    if not words:
        return [""]
    lines: list[str] = []
    current = words[0]
    for word in words[1:]:
        if len(current) + 1 + len(word) > width:
            lines.append(current)
            current = word
        else:
            current = f"{current} {word}"
    lines.append(current)
    return lines


def indent_lines(lines: list[str], spaces: int = 4) -> list[str]:
    """Indent rendered lines by a fixed number of spaces."""
    prefix = " " * spaces
    return [f"{prefix}{line}" if line else "" for line in lines]


def display_path(path: Path) -> str:
    """Return a stable display path when the parsed file is under the repository."""
    try:
        return path.relative_to(Path.cwd()).as_posix()
    except ValueError:
        return path.as_posix()


def normalize_type_name(value: str | None) -> str | None:
    """Normalize an XML QName value to the names used by the Rust schema parser."""
    if value is None:
        return None
    if value.startswith("xs:") or value.startswith("xsd:"):
        _, local = value.split(":", maxsplit=1)
        return f"xs:{local}"
    if ":" in value:
        _, local = value.split(":", maxsplit=1)
        return local
    return value


def extract_documentation(element: ET.Element) -> str | None:
    """Extract the first XSD documentation string for a declaration."""
    doc = element.find(f"{XS}annotation/{XS}documentation")
    if doc is None:
        return None
    text = " ".join("".join(doc.itertext()).split())
    return text or None


def first_child(element: ET.Element, local_name: str) -> ET.Element | None:
    """Return the first direct child with the requested XSD local name."""
    return element.find(f"{XS}{local_name}")


def rust_type_name(name: str) -> str:
    """Convert an XSD type name to an idiomatic Rust type name."""
    return ensure_rust_ident(to_pascal_case(name), pascal=True)


def rust_variant_name(name: str) -> str:
    """Convert an XSD value or element name to an idiomatic Rust enum variant."""
    return ensure_rust_ident(to_pascal_case(name), pascal=True)


def rust_field_name(name: str) -> str:
    """Convert an XSD element name to an idiomatic Rust field name."""
    return ensure_rust_ident(to_snake_case(name), pascal=False)


def ensure_rust_ident(name: str, *, pascal: bool) -> str:
    """Return a valid Rust identifier, escaping keywords when needed."""
    if not name:
        name = "Value" if pascal else "value"
    if name[0].isdigit():
        name = f"Value{name}" if pascal else f"value_{name}"
    if name in RUST_KEYWORDS:
        return f"r#{name}"
    return name


def to_pascal_case(name: str) -> str:
    """Convert a mixed XSD name to PascalCase."""
    words = split_identifier(name)
    return "".join(word[:1].upper() + word[1:].lower() for word in words)


def to_snake_case(name: str) -> str:
    """Convert a mixed XSD name to snake_case."""
    words = split_identifier(name)
    return "_".join(word.lower() for word in words)


def split_identifier(name: str) -> list[str]:
    """Split an XSD identifier into alphanumeric words."""
    normalized = re.sub(r"[^0-9A-Za-z]+", " ", name)
    normalized = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", normalized)
    normalized = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1 \2", normalized)
    return re.findall(r"[0-9A-Za-z]+", normalized)


def unique_name(name: str, used: set[str]) -> str:
    """Return a unique Rust identifier within one namespace."""
    candidate = name
    suffix = 2
    while candidate in used:
        candidate = f"{name}{suffix}"
        suffix += 1
    used.add(candidate)
    return candidate


def rust_string(value: str) -> str:
    """Render a Rust string literal."""
    return json.dumps(value)


SELF_TEST_SCHEMA = DEFAULT_SCHEMA


def run_self_tests(schema_path: Path) -> int:
    """Run script unit tests against the selected schema and return an exit code."""
    global SELF_TEST_SCHEMA  # noqa: PLW0603 - unittest setup reads module-level configuration.
    SELF_TEST_SCHEMA = schema_path
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(DiscoveryTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


class DiscoveryTests(unittest.TestCase):
    """Validate discovery and generation behavior against the local UCI v2.5 schema."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.index = load_schema_index(SELF_TEST_SCHEMA)
        cls.result = discover_type_closure(cls.index)
        cls.generator = RustGenerator(cls.index, cls.result)
        cls.rust_source = cls.generator.render()

    def test_resolves_all_target_global_elements(self) -> None:
        targets = {target.name: target.type_name for target in self.result.targets}
        self.assertEqual(tuple(targets), TARGET_MESSAGES)
        self.assertEqual(targets["PositionReport"], "PositionReportMT")
        self.assertEqual(targets["SignalReport"], "SignalReportMT")

    def test_closure_contains_expected_message_types(self) -> None:
        complex_types = set(self.result.complex_types)
        for expected in (
            "MessageType",
            "PositionReportMT",
            "SystemStatusMT",
            "ServiceStatusMT",
            "SubsystemStatusMT",
            "OperatorNotificationMT",
            "ESM_ActivityMT",
            "SignalReportMT",
            "PO_ActivityMT",
            "ObservationMeasurementReportMT",
            "PositionReportDetailedMT",
            "NavigationReportMT",
        ):
            self.assertIn(expected, complex_types)

    def test_out_of_scope_messages_are_not_target_roots(self) -> None:
        target_names = {target.name for target in self.result.targets}
        self.assertTrue(OUT_OF_SCOPE_GLOBAL_MESSAGES.isdisjoint(target_names))

    def test_unresolved_references_are_empty(self) -> None:
        self.assertEqual(self.result.unresolved_types, ())
        self.assertIn("xs:string", self.result.builtin_types)
        self.assertIn("xs:dateTime", self.result.builtin_types)

    def test_reports_enums_choices_and_included_security_types(self) -> None:
        self.assertGreater(len(self.result.enum_types), 0)
        self.assertGreater(len(self.result.choice_types), 0)
        self.assertIn("SecurityInformationType", self.result.complex_types)

    def test_report_distinguishes_inherited_base_types(self) -> None:
        self.assertIn("MessageType", self.result.inherited_base_types)
        self.assertIn("## Inherited Base Types", render_discovery_report(self.result))

    def test_report_is_deterministic(self) -> None:
        first = render_discovery_report(self.result)
        second = render_discovery_report(discover_type_closure(self.index))
        self.assertEqual(first, second)
        self.assertIn("## Closure Summary", first)

    def test_parser_retains_order_occurrence_documentation_and_abstract_flags(
        self,
    ) -> None:
        position = self.index.complex_types["PositionReportMDT"]
        self.assertFalse(position.abstract)
        self.assertEqual(position.content_kind, "sequence")
        self.assertEqual(position.elements[0].name, "SystemID")
        self.assertEqual(position.elements[1].name, "DisplayName")
        self.assertFalse(position.elements[0].is_optional)
        self.assertTrue(position.elements[1].is_optional)
        self.assertIn("free-text", position.elements[1].documentation or "")
        self.assertTrue(self.index.complex_types["MessageType"].abstract)

    def test_rust_naming_and_type_mapping_are_idiomatic_and_stable(self) -> None:
        self.assertEqual(rust_type_name("ESM_ActivityMT"), "EsmActivityMt")
        self.assertEqual(rust_field_name("MessageData"), "message_data")
        self.assertEqual(rust_field_name("Yield"), "r#yield")
        self.assertEqual(rust_variant_name("2D"), "Value2D")
        element = ElementDecl("Count", "xs:unsignedInt", 0, None, None)
        self.assertEqual(self.generator.rust_type_for_element(element), "Vec<u32>")

    def test_generated_structs_preserve_serde_names_and_optional_occurrences(
        self,
    ) -> None:
        position = self.index.complex_types["PositionReportMDT"]
        fields = self.generator.flatten_fields(position)
        display_name = next(
            field for field in fields if field.source_name == "DisplayName"
        )
        self.assertEqual(display_name.rust_name, "display_name")
        self.assertEqual(display_name.rust_type, "Option<VisibleString256Type>")
        self.assertTrue(display_name.optional)
        self.assertIn("pub struct PositionReportMdt", self.rust_source)
        self.assertIn('#[serde(rename = "DisplayName")]', self.rust_source)
        self.assertIn(
            "pub display_name: Option<VisibleString256Type>", self.rust_source
        )

    def test_generated_enums_preserve_exact_xsd_values(self) -> None:
        classification = self.index.simple_types["ClassificationEnum"]
        self.assertTrue(classification.is_enum)
        variant_values = [
            variant.source_name
            for variant in self.generated_enum_variants(classification)
        ]
        self.assertIn("U", variant_values)
        self.assertIn("TS", variant_values)
        self.assertIn("pub enum ClassificationEnum", self.rust_source)
        self.assertIn('#[serde(rename = "TS")]', self.rust_source)

    def test_generated_choices_are_strict_one_of_enums(self) -> None:
        choice = self.index.complex_types["PositionSourceID_ChoiceType"]
        variants = tuple(
            self.generator.choice_variant(element) for element in choice.elements
        )
        self.assertEqual(
            [variant.source_name for variant in variants],
            ["SystemID", "SubsystemID", "ServiceID"],
        )
        rendered = "\n".join(
            self.generator.render_choice_enum(choice, rust_type_name(choice.name))
        )
        self.assertIn("#[serde(untagged, deny_unknown_fields)]", rendered)
        self.assertIn("SystemId {", rendered)
        self.assertIn(
            "        /// Indicates the unique ID of the System that produced this report.",
            rendered,
        )
        self.assertIn('#[serde(rename = "SystemID")]', rendered)

    def test_generated_rust_output_is_deterministic(self) -> None:
        second = RustGenerator(self.index, discover_type_closure(self.index)).render()
        self.assertEqual(self.rust_source, second)
        self.assertTrue(self.rust_source.startswith("//! Provide generated UCI v2.5"))
        self.assertIn("#![allow(clippy::all", self.rust_source)

    def test_generated_rustdoc_escapes_literal_brackets(self) -> None:
        rendered = render_rust_doc("Range [0,1] for [Capability]Command.", "unused")

        self.assertEqual(rendered, [r"/// Range \[0,1\] for \[Capability\]Command."])

    def test_generated_uci_message_enum_exists(self) -> None:
        self.assertIn("pub enum UciMessage {", self.rust_source)
        self.assertIn("    PositionReport(PositionReportMt),", self.rust_source)
        self.assertIn("    PositionReportDetailed(PositionReportDetailedMt),", self.rust_source)
        self.assertIn("    NavigationReport(NavigationReportMt),", self.rust_source)

    @staticmethod
    def generated_enum_variants(decl: SimpleTypeDecl) -> tuple[RustEnumVariant, ...]:
        """Return generated Rust enum variant metadata for assertions."""
        used: set[str] = set()
        return tuple(
            RustEnumVariant(
                source_name=value.value,
                variant_name=unique_name(rust_variant_name(value.value), used),
                documentation=value.documentation,
            )
            for value in decl.enumerations
        )


def main(argv: list[str] | None = None) -> int:
    """Run discovery reporting, Rust generation, or self-tests."""
    args = parse_args(list(sys.argv[1:] if argv is None else argv))
    if args.self_test:
        return run_self_tests(args.schema)

    index = load_schema_index(args.schema)
    result = discover_type_closure(index)
    if result.unresolved_types:
        names = ", ".join(result.unresolved_types)
        print(f"unresolved non-built-in type references: {names}", file=sys.stderr)
        return 1

    output = (
        RustGenerator(index, result).render()
        if args.generate_rust
        else render_discovery_report(result)
    )
    if args.output is not None:
        try:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(output, encoding="utf-8")
        except OSError as error:
            print(f"failed to write output '{args.output}': {error}", file=sys.stderr)
            return 1
    else:
        try:
            print(output, end="")
        except BrokenPipeError:
            return 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
