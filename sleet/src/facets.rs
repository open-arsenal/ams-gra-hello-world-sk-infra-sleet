//! XSD facet compilation and runtime evaluation.

use std::str::FromStr;

use regex::Regex;
use serde_json::Value;

use crate::schema::model::Facet;

/// A compiled alternative within a pattern set.
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    /// The original XSD pattern string.
    pub original: String,
    /// The translated and compiled regex.
    pub regex: Regex,
}

/// A set of sibling `xs:pattern` facets that are evaluated as a logical OR.
#[derive(Debug, Clone)]
pub struct PatternSet {
    /// The sibling patterns.
    pub alternatives: Vec<CompiledPattern>,
}

impl PatternSet {
    /// Check if a value matches at least one pattern in this set.
    pub fn is_match(&self, value: &str) -> bool {
        self.alternatives.iter().any(|p| p.regex.is_match(value))
    }
}

/// A compiled representation of XSD simple-type facets.
#[derive(Debug, Clone)]
pub struct CompiledFacets {
    /// Allowed lexical values from `xs:enumeration` facets.
    pub enumerations: Vec<String>,
    /// Compiled regular expressions from `xs:pattern` facets grouped by restriction level.
    pub pattern_sets: Vec<PatternSet>,
    /// Lower inclusive numeric bound from `xs:minInclusive`.
    pub min_inclusive: Option<f64>,
    /// Upper inclusive numeric bound from `xs:maxInclusive`.
    pub max_inclusive: Option<f64>,
    /// Lower exclusive numeric bound from `xs:minExclusive`.
    pub min_exclusive: Option<f64>,
    /// Upper exclusive numeric bound from `xs:maxExclusive`.
    pub max_exclusive: Option<f64>,
    /// Minimum lexical length from `xs:minLength`.
    pub min_length: Option<u32>,
    /// Maximum lexical length from `xs:maxLength`.
    pub max_length: Option<u32>,
    /// Exact lexical length from `xs:length`.
    pub length: Option<u32>,
    /// Maximum total digit count from `xs:totalDigits`.
    pub total_digits: Option<u32>,
    /// Maximum fraction digit count from `xs:fractionDigits`.
    pub fraction_digits: Option<u32>,
}

impl CompiledFacets {
    /// Compile parsed XSD facets into lookup-friendly runtime structures.
    ///
    /// Returns the compiled facets and a list of non-fatal warnings. Facets
    /// that fail compilation or parsing are skipped.
    #[allow(clippy::too_many_lines)]
    pub fn compile(facets: &[Facet]) -> (Self, Vec<String>) {
        let mut compiled = Self {
            enumerations: Vec::new(),
            pattern_sets: Vec::new(),
            min_inclusive: None,
            max_inclusive: None,
            min_exclusive: None,
            max_exclusive: None,
            min_length: None,
            max_length: None,
            length: None,
            total_digits: None,
            fraction_digits: None,
        };
        let mut warnings = Vec::new();

        for facet in facets {
            match facet {
                Facet::Enumeration(value) => compiled.enumerations.push(value.clone()),
                Facet::Pattern(xsd_pattern) => {
                    if has_char_class_subtraction(xsd_pattern) {
                        warnings.push(format!(
                            "unsupported XSD character class subtraction pattern skipped: {xsd_pattern}"
                        ));
                        continue;
                    }

                    let translated = translate_xsd_pattern(xsd_pattern);
                    match Regex::new(&translated) {
                        Ok(regex) => {
                            compiled.pattern_sets.push(PatternSet {
                                alternatives: vec![CompiledPattern {
                                    original: xsd_pattern.clone(),
                                    regex,
                                }],
                            });
                        }
                        Err(error) => warnings.push(format!(
                            "pattern compile failed for '{xsd_pattern}': {error}"
                        )),
                    }
                }
                Facet::PatternGroup(patterns) => {
                    let mut alternatives = Vec::new();
                    for xsd_pattern in patterns {
                        if has_char_class_subtraction(xsd_pattern) {
                            warnings.push(format!(
                                "unsupported XSD character class subtraction pattern skipped: {xsd_pattern}"
                            ));
                            continue;
                        }

                        let translated = translate_xsd_pattern(xsd_pattern);
                        match Regex::new(&translated) {
                            Ok(regex) => alternatives.push(CompiledPattern {
                                original: xsd_pattern.clone(),
                                regex,
                            }),
                            Err(error) => warnings.push(format!(
                                "pattern compile failed for '{xsd_pattern}': {error}"
                            )),
                        }
                    }
                    if !alternatives.is_empty() {
                        compiled.pattern_sets.push(PatternSet { alternatives });
                    }
                }
                Facet::MinInclusive(value) => {
                    parse_f64(
                        "minInclusive",
                        value,
                        &mut compiled.min_inclusive,
                        &mut warnings,
                    );
                }
                Facet::MaxInclusive(value) => {
                    parse_f64(
                        "maxInclusive",
                        value,
                        &mut compiled.max_inclusive,
                        &mut warnings,
                    );
                }
                Facet::MinExclusive(value) => {
                    parse_f64(
                        "minExclusive",
                        value,
                        &mut compiled.min_exclusive,
                        &mut warnings,
                    );
                }
                Facet::MaxExclusive(value) => {
                    parse_f64(
                        "maxExclusive",
                        value,
                        &mut compiled.max_exclusive,
                        &mut warnings,
                    );
                }
                Facet::MinLength(length) => compiled.min_length = Some(*length),
                Facet::MaxLength(length) => compiled.max_length = Some(*length),
                Facet::Length(length) => compiled.length = Some(*length),
                Facet::TotalDigits(digits) => compiled.total_digits = Some(*digits),
                Facet::FractionDigits(digits) => compiled.fraction_digits = Some(*digits),
            }
        }

        (compiled, warnings)
    }

    /// Return whether this instance has at least one active facet.
    pub const fn has_constraints(&self) -> bool {
        !self.enumerations.is_empty()
            || !self.pattern_sets.is_empty()
            || self.min_inclusive.is_some()
            || self.max_inclusive.is_some()
            || self.min_exclusive.is_some()
            || self.max_exclusive.is_some()
            || self.min_length.is_some()
            || self.max_length.is_some()
            || self.length.is_some()
            || self.total_digits.is_some()
            || self.fraction_digits.is_some()
    }

    /// Validate a JSON value against all compiled facets.
    #[allow(clippy::too_many_lines)]
    pub fn validate(
        &self,
        value: &Value,
        _primitive: &str,
        path: &str,
    ) -> Result<(), FacetViolation> {
        let Some(lexical) = lexical_value(value) else {
            return Ok(());
        };

        if !self.enumerations.is_empty()
            && !self.enumerations.iter().any(|allowed| allowed == &lexical)
        {
            return Err(FacetViolation {
                facet: "enumeration".to_owned(),
                detail: format!(
                    "value '{lexical}' not in [{}]",
                    self.enumerations.join(", ")
                ),
                path: path.to_owned(),
            });
        }

        for pattern_set in &self.pattern_sets {
            if !pattern_set.is_match(&lexical) {
                let expected = pattern_set
                    .alternatives
                    .iter()
                    .map(|p| p.original.as_str())
                    .collect::<Vec<_>>()
                    .join(" OR ");
                return Err(FacetViolation {
                    facet: "pattern".to_owned(),
                    detail: format!("value '{lexical}' does not match pattern(s): {expected}"),
                    path: path.to_owned(),
                });
            }
        }

        let Ok(length) = u32::try_from(lexical.chars().count()) else {
            return Err(FacetViolation {
                facet: "length".to_owned(),
                detail: "lexical length exceeds u32".to_owned(),
                path: path.to_owned(),
            });
        };
        if let Some(expected) = self.length
            && length != expected
        {
            return Err(FacetViolation {
                facet: "length".to_owned(),
                detail: format!("length {length} != {expected}"),
                path: path.to_owned(),
            });
        }
        if let Some(min) = self.min_length
            && length < min
        {
            return Err(FacetViolation {
                facet: "minLength".to_owned(),
                detail: format!("length {length} < {min}"),
                path: path.to_owned(),
            });
        }
        if let Some(max) = self.max_length
            && length > max
        {
            return Err(FacetViolation {
                facet: "maxLength".to_owned(),
                detail: format!("length {length} > {max}"),
                path: path.to_owned(),
            });
        }

        let is_numeric_value = value.as_f64().is_some();
        if is_numeric_value && !is_special_float_literal(&lexical) {
            if let Some(number) = value.as_f64() {
                if let Some(min) = self.min_inclusive
                    && number < min
                {
                    return Err(FacetViolation {
                        facet: "minInclusive".to_owned(),
                        detail: format!("value {number} < {min}"),
                        path: path.to_owned(),
                    });
                }
                if let Some(max) = self.max_inclusive
                    && number > max
                {
                    return Err(FacetViolation {
                        facet: "maxInclusive".to_owned(),
                        detail: format!("value {number} > {max}"),
                        path: path.to_owned(),
                    });
                }
                if let Some(min) = self.min_exclusive
                    && number <= min
                {
                    return Err(FacetViolation {
                        facet: "minExclusive".to_owned(),
                        detail: format!("value {number} <= {min}"),
                        path: path.to_owned(),
                    });
                }
                if let Some(max) = self.max_exclusive
                    && number >= max
                {
                    return Err(FacetViolation {
                        facet: "maxExclusive".to_owned(),
                        detail: format!("value {number} >= {max}"),
                        path: path.to_owned(),
                    });
                }
            }

            if let Some((total_digits, fraction_digits)) = parse_digit_counts(&lexical) {
                if let Some(max_total) = self.total_digits
                    && total_digits > max_total
                {
                    return Err(FacetViolation {
                        facet: "totalDigits".to_owned(),
                        detail: format!("total digits {total_digits} > {max_total}"),
                        path: path.to_owned(),
                    });
                }
                if let Some(max_fraction) = self.fraction_digits
                    && fraction_digits > max_fraction
                {
                    return Err(FacetViolation {
                        facet: "fractionDigits".to_owned(),
                        detail: format!("fraction digits {fraction_digits} > {max_fraction}"),
                        path: path.to_owned(),
                    });
                }
            }
        }

        Ok(())
    }
}

/// A runtime facet validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetViolation {
    /// JSON path of the failing value.
    pub path: String,
    /// Facet discriminator such as `enumeration` or `minInclusive`.
    pub facet: String,
    /// Human-readable detail for logs and diagnostics.
    pub detail: String,
}

fn parse_f64(facet_name: &str, value: &str, slot: &mut Option<f64>, warnings: &mut Vec<String>) {
    match f64::from_str(value) {
        Ok(parsed) => *slot = Some(parsed),
        Err(error) => warnings.push(format!("failed to parse {facet_name}='{value}': {error}")),
    }
}

fn parse_digit_counts(value: &str) -> Option<(u32, u32)> {
    let unsigned = value
        .strip_prefix(['+', '-'])
        .unwrap_or(value)
        .replace('_', "");

    if unsigned.contains(['e', 'E']) {
        return None;
    }

    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return None;
    }

    if !whole.chars().all(|ch| ch.is_ascii_digit())
        || !fraction.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }

    let whole_len = u32::try_from(whole.len()).ok()?;
    let fraction_len = u32::try_from(fraction.len()).ok()?;
    let total = whole_len.checked_add(fraction_len)?;
    Some((total, fraction_len))
}

fn is_special_float_literal(value: &str) -> bool {
    matches!(value, "NaN" | "Infinity" | "-Infinity")
}

/// Translate an XSD regex pattern to Rust's `regex` syntax.
fn translate_xsd_pattern(xsd: &str) -> String {
    let decoded = decode_xml_char_refs(xsd);
    let translated = decoded
        .replace(r"\i", r"[\p{L}_:]")
        .replace(r"\I", r"[^\p{L}_:]")
        .replace(r"\c", r"[\p{L}\p{N}._:\-]")
        .replace(r"\C", r"[^\p{L}\p{N}._:\-]");

    format!(r"\A(?:{translated})\z")
}

fn decode_xml_char_refs(pattern: &str) -> String {
    let mut decoded = String::new();
    let mut cursor = pattern;

    while let Some(start) = cursor.find("&#") {
        decoded.push_str(&cursor[..start]);
        let rest = &cursor[start + 2..];
        let Some(end) = rest.find(';') else {
            decoded.push_str(&cursor[start..]);
            return decoded;
        };

        let entity = &rest[..end];
        let parsed = entity.strip_prefix(['x', 'X']).map_or_else(
            || entity.parse::<u32>().ok(),
            |hex| u32::from_str_radix(hex, 16).ok(),
        );

        if let Some(ch) = parsed.and_then(char::from_u32) {
            decoded.push(ch);
        } else {
            decoded.push_str("&#");
            decoded.push_str(entity);
            decoded.push(';');
        }

        cursor = &rest[end + 1..];
    }

    decoded.push_str(cursor);
    decoded
}

fn has_char_class_subtraction(xsd: &str) -> bool {
    let bytes = xsd.as_bytes();
    let mut idx = 0;

    while idx < bytes.len() {
        if bytes[idx] != b'[' {
            idx += 1;
            continue;
        }

        let mut inner = idx + 1;
        while inner < bytes.len() {
            if bytes[inner] == b'\\' {
                inner += 2;
                continue;
            }
            if bytes[inner] == b']' {
                break;
            }
            inner += 1;
        }

        if inner >= bytes.len() {
            return false;
        }

        if xsd[idx + 1..inner].contains("-[") {
            return true;
        }

        idx = inner + 1;
    }

    false
}

/// Extract the lexical value used for facet comparison.
fn lexical_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn enumeration_in_set_passes() {
        let (compiled, warnings) = CompiledFacets::compile(&[Facet::Enumeration("A".to_owned())]);
        assert!(warnings.is_empty());
        assert!(compiled.validate(&json!("A"), "xs:string", "root").is_ok());
    }

    #[test]
    fn enumeration_out_of_set_fails() {
        let (compiled, _) = CompiledFacets::compile(&[Facet::Enumeration("A".to_owned())]);
        let error = compiled
            .validate(&json!("B"), "xs:string", "root")
            .expect_err("out-of-set enumeration must fail");
        assert_eq!(error.facet, "enumeration");
    }

    #[test]
    fn enumeration_matches_numeric_lexical_value() {
        let (compiled, _) = CompiledFacets::compile(&[Facet::Enumeration("42".to_owned())]);
        assert!(compiled.validate(&json!(42), "xs:int", "root").is_ok());
    }

    #[test]
    fn pattern_matching_passes() {
        let (compiled, _) = CompiledFacets::compile(&[Facet::Pattern("[A-Z]{3}".to_owned())]);
        assert!(
            compiled
                .validate(&json!("ABC"), "xs:string", "root")
                .is_ok()
        );
    }

    #[test]
    fn pattern_non_matching_fails() {
        let (compiled, _) = CompiledFacets::compile(&[Facet::Pattern("[A-Z]{3}".to_owned())]);
        let error = compiled
            .validate(&json!("ABCD"), "xs:string", "root")
            .expect_err("non-matching pattern must fail");
        assert_eq!(error.facet, "pattern");
    }

    #[test]
    fn xsd_translation_handles_i_and_c_classes() {
        let translated = translate_xsd_pattern(r"\i\c*");
        let regex = Regex::new(&translated).expect("translated pattern must compile");
        assert!(regex.is_match("Name_1"));
        assert!(!regex.is_match("1Name"));
    }

    #[test]
    fn xsd_translation_decodes_xml_char_refs() {
        let translated = translate_xsd_pattern("[&#x20;-&#x7E;]{1,3}");
        let regex = Regex::new(&translated).expect("translated pattern must compile");
        assert!(regex.is_match("ABC"));
    }

    #[test]
    fn class_subtraction_detection_ignores_adjacent_classes() {
        assert!(!has_char_class_subtraction("[1-5][8-9]"));
    }

    #[test]
    fn pattern_is_implicitly_anchored() {
        let (compiled, _) = CompiledFacets::compile(&[Facet::Pattern("ABC".to_owned())]);
        assert!(
            compiled
                .validate(&json!("ABC"), "xs:string", "root")
                .is_ok()
        );
        assert!(
            compiled
                .validate(&json!("XABC"), "xs:string", "root")
                .is_err()
        );
    }

    #[test]
    fn multiple_patterns_are_all_enforced() {
        let (compiled, _) =
            CompiledFacets::compile(&[Facet::Pattern("[A-Z]{3}[0-9]{2}".to_owned())]);
        let (compiled2, _) = CompiledFacets::compile(&[Facet::Pattern("ABC[0-9]{2}".to_owned())]);

        assert!(
            compiled
                .validate(&json!("ABC12"), "xs:string", "root")
                .is_ok()
        );
        assert!(
            compiled
                .validate(&json!("XYZ12"), "xs:string", "root")
                .is_ok()
        );
        assert!(
            compiled2
                .validate(&json!("XYZ12"), "xs:string", "root")
                .is_err()
        );
    }

    #[test]
    fn inclusive_numeric_bounds_enforce_boundary_values() {
        let (compiled, _) = CompiledFacets::compile(&[
            Facet::MinInclusive("10".to_owned()),
            Facet::MaxInclusive("20".to_owned()),
        ]);
        assert!(compiled.validate(&json!(10), "xs:int", "root").is_ok());
        assert!(compiled.validate(&json!(20), "xs:int", "root").is_ok());
        assert!(compiled.validate(&json!(9), "xs:int", "root").is_err());
        assert!(compiled.validate(&json!(21), "xs:int", "root").is_err());
    }

    #[test]
    fn exclusive_numeric_bounds_enforce_boundary_values() {
        let (compiled, _) = CompiledFacets::compile(&[
            Facet::MinExclusive("10".to_owned()),
            Facet::MaxExclusive("20".to_owned()),
        ]);
        assert!(compiled.validate(&json!(11), "xs:int", "root").is_ok());
        assert!(compiled.validate(&json!(19), "xs:int", "root").is_ok());
        assert!(compiled.validate(&json!(10), "xs:int", "root").is_err());
        assert!(compiled.validate(&json!(20), "xs:int", "root").is_err());
    }

    #[test]
    fn length_constraints_enforced() {
        let (compiled, _) =
            CompiledFacets::compile(&[Facet::MinLength(2), Facet::MaxLength(4), Facet::Length(3)]);

        assert!(
            compiled
                .validate(&json!("abc"), "xs:string", "root")
                .is_ok()
        );
        assert!(
            compiled
                .validate(&json!("ab"), "xs:string", "root")
                .is_err()
        );
    }

    #[test]
    fn combined_enumeration_and_pattern_enforced() {
        let (compiled, _) = CompiledFacets::compile(&[
            Facet::Enumeration("ABC".to_owned()),
            Facet::Pattern("[A-Z]{3}".to_owned()),
        ]);

        assert!(
            compiled
                .validate(&json!("ABC"), "xs:string", "root")
                .is_ok()
        );
        assert!(
            compiled
                .validate(&json!("ABD"), "xs:string", "root")
                .is_err()
        );
    }

    #[test]
    fn empty_facets_are_no_op() {
        let (compiled, warnings) = CompiledFacets::compile(&[]);
        assert!(warnings.is_empty());
        assert!(
            compiled
                .validate(&json!("anything"), "xs:string", "root")
                .is_ok()
        );
    }

    #[test]
    fn special_float_literals_skip_numeric_range_checks() {
        let (compiled, _) = CompiledFacets::compile(&[
            Facet::MinInclusive("0".to_owned()),
            Facet::MaxInclusive("10".to_owned()),
        ]);

        assert!(compiled.validate(&json!("NaN"), "xs:float", "root").is_ok());
    }
}
