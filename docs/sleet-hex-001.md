# hexBinary facet octet length

Base: `e38f61d8ce0d75c8508434a52f2ed77c69cf6a3b`, tree
`7e705f4eb5f658957bd414b4abc8207311f3c1e4`.

`CompiledFacets::validate` ignored the resolved primitive and counted Unicode
characters. A valid 32-octet UCI SHA2256HashType value therefore failed with
`FileMetadata.MessageData.SHA_2_Hash`, `length 64 != 32`.

For `xs:hexBinary` only, validation now requires a string containing an even
number of ASCII hex digits, after XML edge-whitespace normalization. Internal
whitespace remains invalid. Length facets count digit pairs (octets), without
allocating decoded bytes. Enumeration/pattern/length/minLength/maxLength order,
error paths, string character lengths and base64 behavior remain unchanged.
The validator also evaluates unrestricted hexBinary, so malformed data cannot
bypass lexical validation through an empty facet set. No message-name special
case, dependency, schema edit or validation bypass is introduced.

`sleet/tests/fixtures/file-metadata-{25,26}.json` are hand-authored authentic-schema
FileMetadata envelopes, not copied XSDs. They exercise the production parser and
message validator against the separately supplied, pinned full schemas:

| File | SHA-256 |
|---|---|
| 2.5 MessageDefinitions | `ac9430499e1107371345e04430895c8c9f18578c1a6b022958ca43ae8aa7bf27` |
| 2.5 SecurityMarkings | `4a8056f1503234d423a0c2495844ab65387febced62bea58b87d4f343e9e7a0b` |
| 2.6 MessageDefinitions | `af54ce724c4fe869c8208c86985c0b768d74d581691e21d66c88bb6cfe59955b` |
| 2.6 SecurityMarkings | `ee6e58d5db9fd80d526bc964b8244ec296d106301640d34c3cb2c5c00b710b88` |

Run from the repository root with authenticated schema paths:

```sh
cargo test --locked -p sleet hex_binary_tests
cargo test --locked -p sleet authentic_file_metadata -- --ignored
SLEET_SCHEMA_PATH="$AMS_GRA_UCI_2_5_ROOT" cargo test --locked --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
```

The two authentic-schema tests are explicitly ignored in the offline default
suite; missing schema paths fail when they are explicitly selected. They are
not counted as passes unless executed. No OMS dependency pin is changed by
this correction; independent review and dependency acceptance remain required.