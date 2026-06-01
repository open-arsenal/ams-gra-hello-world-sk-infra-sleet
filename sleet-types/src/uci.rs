//! Provide typed UCI message helpers for simulation clients.

/// Expose generated UCI v2.5 helper types for the supported UCI message subset.
pub mod v2_5;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::v2_5::{
        ClassificationEnum, EnvironmentEnum, Link16HazardType, Link16HazardYieldEnum,
        PositionSourceIdChoiceType, SystemIdType, UciMessage,
    };

    #[test]
    fn generated_enum_serializes_with_uci_value_name() {
        let encoded =
            serde_json::to_value(ClassificationEnum::Ts).expect("classification should serialize");

        assert_eq!(encoded, json!("TS"));
    }

    #[test]
    fn generated_choice_serializes_with_uci_field_name() {
        let choice = PositionSourceIdChoiceType::SystemId {
            system_id: SystemIdType {
                uuid: "00000000-0000-1000-8000-000000000001".to_owned(),
                descriptive_label: None,
            },
        };

        let encoded = serde_json::to_value(choice).expect("choice should serialize");

        assert_eq!(
            encoded,
            json!({ "SystemID": { "UUID": "00000000-0000-1000-8000-000000000001" } })
        );
    }

    #[test]
    fn generated_struct_escapes_rust_keywords() {
        let hazard = Link16HazardType {
            burst: None,
            r#yield: Some(Link16HazardYieldEnum::Small),
        };

        let encoded = serde_json::to_value(hazard).expect("hazard should serialize");

        assert_eq!(encoded, json!({ "Yield": "SMALL" }));
    }

    #[test]
    fn uci_message_master_enum_deserializes_payload() {
        let payload = r#"{
            "PositionReport": {
                "SecurityInformation": {
                    "Classification": "U",
                    "OwnerProducer": [
                        { "GovernmentIdentifier": "USA" }
                    ]
                },
                "MessageHeader": {
                    "SystemID": { "UUID": "550e8400-e29b-41d4-a716-446655440000" },
                    "Timestamp": "2026-01-01T00:00:00Z",
                    "SchemaVersion": "002.5.0",
                    "Mode": "LIVE"
                },
                "MessageData": {
                    "SystemID": { "UUID": "550e8400-e29b-41d4-a716-446655440000" },
                    "Source": "ACTUAL",
                    "CurrentOperatingDomain": "AIR",
                    "InertialState": {
                        "Position": {
                            "Latitude": 1.0,
                            "Longitude": 1.0,
                            "Altitude": 1.0,
                            "Timestamp": "2026-01-01T00:00:00Z"
                        }
                    }
                }
            }
        }"#;

        let message: UciMessage =
            serde_json::from_str(payload).expect("payload should deserialize");

        match message {
            UciMessage::PositionReport(pr) => {
                assert_eq!(
                    pr.message_data.system_id.uuid,
                    "550e8400-e29b-41d4-a716-446655440000"
                );
                assert_eq!(
                    pr.message_data.current_operating_domain,
                    EnvironmentEnum::Air
                );
            }
            _ => panic!("Expected PositionReport variant"),
        }
    }
}
