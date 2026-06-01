# Troubleshooting

This guide provides solutions to common issues encountered when building, integrating, or running sleet.

## OMS / UCI XSD issues

- Sleet currently supports OMS / UCI v2.5 which is sources directly from [Open Arsenal](https://gitlab.com/open-arsenal/uci/standard).
- The Containerfile build process pulls the UCI XSD spec in to `/build/spec/<version>/*.xsd` (e.g., `/build/spec/002.5.0/UCI_MessageDefinitions_v2_5_0.xsd`) and any mounted overrides will need to respect that hierarchy.
- Any attempt to use another XSD schema is unsupported and may result in errors.

## Getting Help

If you encounter an issue not listed here:
1. Gather the relevant logs, build output, and configuration files.
2. Note the host environment and version being used.
3. Open an issue on the issue tracker with the collected details.
