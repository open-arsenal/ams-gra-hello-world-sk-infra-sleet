//! Regression coverage for binary value-space lengths.

use serde_json::{Value, json};

use crate::facets::CompiledFacets;
use crate::schema::{model::Facet, parser};
use crate::validator::{ValidationError, validate_oms_json};

#[test]
fn direct_hex_lengths_and_lexical_forms() {
    let (exact, _) = CompiledFacets::compile(&[Facet::Length(32)]);
    for byte in ["00", "ff", "FF", "aB"] {
        for value in [byte.repeat(32), format!(" \t{}\r\n", byte.repeat(32))] {
            assert!(
                exact
                    .validate(&json!(value), "xs:hexBinary", "root.Hash")
                    .is_ok()
            );
        }
    }
    for size in [31, 33] {
        let err = exact
            .validate(&json!("00".repeat(size)), "xs:hexBinary", "root.Hash")
            .unwrap_err();
        assert_eq!(err.facet, "length");
        assert_eq!(err.path, "root.Hash");
        assert_eq!(err.detail, format!("length {size} != 32"));
    }
    let (empty, _) = CompiledFacets::compile(&[Facet::Length(0)]);
    assert!(empty.validate(&json!(""), "xs:hexBinary", "root").is_ok());
    let (unrestricted, _) = CompiledFacets::compile(&[]);
    for bad in [
        json!("0"),
        json!("GG"),
        json!("00 00"),
        json!("00\t00"),
        json!("\u{a0}00"),
        json!("ＦＦ"),
        json!(null),
        json!(0),
        json!(true),
        json!([]),
        json!({}),
    ] {
        let err = unrestricted
            .validate(&bad, "xs:hexBinary", "root.Hash")
            .unwrap_err();
        assert_eq!(err.facet, "hexBinary");
        assert_eq!(err.path, "root.Hash");
    }
}

#[test]
fn direct_min_max_and_string_semantics() {
    for facets in [
        vec![Facet::MinLength(2)],
        vec![Facet::MaxLength(4)],
        vec![Facet::MinLength(2), Facet::MaxLength(4)],
    ] {
        let (compiled, _) = CompiledFacets::compile(&facets);
        for size in 0..=5 {
            let result = compiled.validate(&json!("ff".repeat(size)), "xs:hexBinary", "root.Hash");
            let expected = if facets.iter().any(|f| matches!(f, Facet::MinLength(2))) && size < 2 {
                Some("minLength")
            } else if facets.iter().any(|f| matches!(f, Facet::MaxLength(4))) && size > 4 {
                Some("maxLength")
            } else {
                None
            };
            match expected {
                Some(facet) => assert_eq!(result.unwrap_err().facet, facet),
                None => assert!(result.is_ok()),
            }
        }
    }
    let (text, _) = CompiledFacets::compile(&[Facet::Length(2)]);
    assert!(text.validate(&json!("é水"), "xs:string", "root").is_ok());
    assert!(text.validate(&json!("é水x"), "xs:string", "root").is_err());
    assert!(text.validate(&json!(" AA "), "xs:string", "root").is_err());
    // Existing non-hex primitive semantics, including base64, are unchanged.
    assert!(
        text.validate(&json!("AA"), "xs:base64Binary", "root")
            .is_ok()
    );
    let (precedence, _) = CompiledFacets::compile(&[
        Facet::Enumeration("00".to_owned()),
        Facet::Pattern("[0-9]+".to_owned()),
        Facet::Length(32),
    ]);
    assert_eq!(
        precedence
            .validate(&json!("GG"), "xs:hexBinary", "root")
            .unwrap_err()
            .facet,
        "enumeration"
    );
    assert_eq!(
        precedence
            .validate(&json!("00"), "xs:hexBinary", "root")
            .unwrap_err()
            .facet,
        "length"
    );
}

#[test]
fn message_validator_nested_restrictions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary.xsd");
    std::fs::write(&path, r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:simpleType name="Bytes"><xs:restriction base="xs:hexBinary"><xs:minLength value="2"/></xs:restriction></xs:simpleType>
      <xs:simpleType name="Bounded"><xs:restriction base="Bytes"><xs:maxLength value="4"/></xs:restriction></xs:simpleType>
      <xs:simpleType name="Alias"><xs:restriction base="Bounded"/></xs:simpleType>
      <xs:complexType name="Payload"><xs:sequence><xs:element name="Hash" type="Alias"/><xs:element name="Raw" type="xs:hexBinary"/></xs:sequence></xs:complexType>
      <xs:element name="Message" type="Payload"/>
    </xs:schema>"#).unwrap();
    let schema = parser::parse(&path).unwrap();
    for size in 1..=5 {
        let result = validate_oms_json(
            &schema,
            &json!({"Message":{"Hash":"FF".repeat(size),"Raw":""}}),
        );
        if (2..=4).contains(&size) {
            assert!(result.is_ok());
        } else {
            assert!(
                matches!(result.unwrap_err(), ValidationError::FacetViolation {path, facet, ..}
                if path == "Message.Hash" && facet == if size < 2 {"minLength"} else {"maxLength"})
            );
        }
    }
    for bad in [
        json!("0"),
        json!("GG"),
        json!("00 00"),
        json!(null),
        json!(0),
        json!(false),
        json!([]),
        json!({}),
    ] {
        for field in ["Hash", "Raw"] {
            let mut doc = json!({"Message":{"Hash":"00ff", "Raw":""}});
            doc["Message"][field] = bad.clone();
            let err = validate_oms_json(&schema, &doc).unwrap_err();
            let expected = format!("Message.{field}");
            match err {
                ValidationError::FacetViolation { path, facet, .. } => {
                    assert_eq!(path, expected);
                    assert_eq!(facet, "hexBinary");
                }
                ValidationError::TypeMismatch {
                    path,
                    expected: kind,
                    ..
                } => {
                    assert_eq!(path, expected);
                    assert_eq!(kind, "string");
                }
                ValidationError::OccurrenceMismatch {
                    path,
                    expected_array: false,
                } => {
                    assert!(bad.is_array());
                    assert_eq!(path, expected);
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }
}

fn authentic_file_metadata(root_var: &str, fixture: &str) {
    let root = std::env::var(root_var).expect("set the authenticated UCI schema root");
    let schema = parser::parse(std::path::Path::new(&root)).unwrap();
    let seed: Value = serde_json::from_str(fixture).unwrap();
    assert!(validate_oms_json(&schema, &seed).is_ok());
    for digits in ["00", "ff", "FF", "aB"] {
        let mut doc = seed.clone();
        doc["FileMetadata"]["MessageData"]["SHA_2_Hash"] = json!(digits.repeat(32));
        assert!(validate_oms_json(&schema, &doc).is_ok());
    }
    for (bad, facet) in [
        (json!("00".repeat(31)), "length"),
        (json!("00".repeat(33)), "length"),
        (json!("0".repeat(63)), "hexBinary"),
        (json!("G".repeat(64)), "hexBinary"),
    ] {
        let mut doc = seed.clone();
        doc["FileMetadata"]["MessageData"]["SHA_2_Hash"] = bad;
        assert!(matches!(validate_oms_json(&schema, &doc).unwrap_err(),
            ValidationError::FacetViolation {path, facet: actual, ..}
                if path == "FileMetadata.MessageData.SHA_2_Hash" && actual == facet));
    }
    for bad in [json!(null), json!(32), json!(true), json!([]), json!({})] {
        let mut doc = seed.clone();
        doc["FileMetadata"]["MessageData"]["SHA_2_Hash"] = bad;
        match validate_oms_json(&schema, &doc).unwrap_err() {
            ValidationError::TypeMismatch {
                path,
                expected: "string",
                ..
            } => assert_eq!(path, "FileMetadata.MessageData.SHA_2_Hash"),
            ValidationError::OccurrenceMismatch {
                path,
                expected_array: false,
            } => {
                assert!(doc["FileMetadata"]["MessageData"]["SHA_2_Hash"].is_array());
                assert_eq!(path, "FileMetadata.MessageData.SHA_2_Hash");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

#[test]
#[ignore = "requires authenticated UCI 2.5 schema via AMS_GRA_UCI_2_5_ROOT"]
fn authentic_file_metadata_25() {
    authentic_file_metadata(
        "AMS_GRA_UCI_2_5_ROOT",
        include_str!("../tests/fixtures/file-metadata-25.json"),
    );
}

#[test]
#[ignore = "requires authenticated UCI 2.6 schema via AMS_GRA_UCI_2_6_ROOT"]
fn authentic_file_metadata_26() {
    authentic_file_metadata(
        "AMS_GRA_UCI_2_6_ROOT",
        include_str!("../tests/fixtures/file-metadata-26.json"),
    );
}
