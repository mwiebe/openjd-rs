# Template Parsing

The `parse` module handles decoding YAML/JSON template documents into typed Rust structs.
Parsing is the entry point to the crate — all other operations work on the parsed types.

## Public API

```rust
pub fn decode_job_template(
    template: serde_yaml::Value,
    supported_extensions: Option<&[&str]>,
    caller_limits: &CallerLimits,
) -> Result<JobTemplate, ModelError>

pub fn decode_environment_template(
    template: serde_yaml::Value,
    supported_extensions: Option<&[&str]>,
    caller_limits: &CallerLimits,
) -> Result<EnvironmentTemplate, ModelError>

pub fn decode_template(
    template: serde_yaml::Value,
    supported_extensions: Option<&[&str]>,
    caller_limits: &CallerLimits,
) -> Result<DecodedTemplate, ModelError>

pub fn document_string_to_object(
    document: &str,
    doc_type: DocumentType,
    caller_limits: &CallerLimits,
) -> Result<serde_yaml::Value, ModelError>
```

### Types

```rust
pub enum DocumentType {
    Json,
    Yaml,
}

pub enum DecodedTemplate {
    Job(JobTemplate),
    Environment(EnvironmentTemplate),
}
```

## Decode Pipeline

The `decode_*` functions run passes 1–11 of the template processing pipeline. Passes 1–4
live in the `parse` module; passes 5–11 live in the `validate_v2023_09` module (see
[validation.md](validation.md)).

### Pass 1: Raw Parsing

`document_string_to_object` parses a raw string into a `serde_yaml::Value` tree. The
`DocumentType` parameter selects JSON or YAML parsing. This pass catches syntax errors
(malformed YAML, invalid JSON).

Booleans, nulls and non-finite floats in YAML resolve by the YAML 1.2 Core schema, with
strict booleans:

- `true`, `True`, `TRUE`, `false`, `False` and `FALSE` are booleans.
- `null`, `Null`, `NULL`, `~` and an empty value are null.
- Every Core spelling of a non-finite float (for example `.nan`, `.NaN`, `+.Inf`, `-.INF`)
  becomes a string in lowercase form (`.nan`, `.inf` or `-.inf`), so that field validation
  can report it.
- Any other spelling, such as `yes`, `on`, `tRUE` or `.nAn`, is a string.
- A `!!bool` or `!!float` tag on a value that isn't one of that type's Core spellings, such as
  `!!bool tRUE` or `!!float .NAn`, is a syntax error.

Integers don't strictly follow the Core schema: `1_000` and `0b101` are read as integers, and
`0755` as the float 755.0. Job parameter files and `--tasks` files in the CLI resolve scalars
the same way.

Callers typically handle this pass themselves — the `decode_*` functions accept a
pre-parsed `serde_yaml::Value`.

### Pass 2: Version Dispatch

The `specificationVersion` field is read from the value tree and mapped to a
`TemplateSpecificationVersion` enum:

| String | Enum Variant |
|--------|-------------|
| `"jobtemplate-2023-09"` | `JobTemplate2023_09` |
| `"environment-2023-09"` | `Environment2023_09` |

Unrecognized versions produce `ModelError::UnsupportedSchema`.

`decode_template` auto-detects the template type from this field.

### Pass 3: Serde Deserialization

The value tree is deserialized into the appropriate struct through
`serde_path_to_error::deserialize` (`parse::deserialize_typed`), which tracks the path the
deserializer reaches. All template types use `#[serde(deny_unknown_fields)]`, so unexpected
fields produce errors.

**A serde failure is reported as one `ModelError::ModelValidation` error at its model path**
(`template::decode_errors`), not as a bare `DecodeValidation` string: `steps[0] ->
stepServices:\n\tunknown field \`stepServices\`, expected one of …`, `services[0] ->
healthCheck:\n\tunknown field \`timeoutSeconds\`, …`, `services[0]:\n\tmissing field
\`ports\`…`, `steps[0] -> script -> actions -> onRun -> command:\n\tinvalid type: map, expected a
string or number`. A serde `Map { key }` segment is a `PathElement::Field`, a `Seq { index }` an
`Index`; an `unknown field` of a struct is reported at `… -> <field>`, of an internally tagged
enum (`healthCheck`) at the enum's own path, and a `missing field` at the struct's. serde's
message is kept verbatim; a **rename hint** follows it on the same line for a property name an
earlier draft of RFC 0009 used (exploratory report S9), or one RFC 0009 removed:

| written | hint |
|---|---|
| `jobServices` (job template root) | `'jobServices' is not a property; declare Services in 'services' and put each Step in a Service's scope with 'service: <name>' in the Step's dependencies.` |
| `stepServices` (a Step) | `'stepServices' is not a property; move the Service to the top-level 'services' list and add 'service: <name>' to this Step's dependencies.` |
| `serviceEnvironments` (root) | `'serviceEnvironments' is not a property; a Service sets up its own host in 'onEnter', or requests a Job Environment with an 'environment:' entry in its dependencies, as a Step does.` |
| `runScope` (any `<Environment>`: `jobEnvironments[i]`, `stepEnvironments[j]`, an Environment Template's `environment`; with or without `SERVICE`) | `'runScope' is not a property; an Environment's 'activation' (ALWAYS, BY_DEFAULT, or ON_REQUEST) and 'enableIf' (ENV_PARAMS), and the 'environment:' entries of the Steps and Services that request it, decide which Sessions enter it (RFC 0009).` |
| `externalServices` (environment template root) | `'externalServices' is a Job Template property; an Environment Template declares the Services it provides in 'services'.` |
| `readinessCheck` (a Service) | `'readinessCheck' is not a property; the health check is 'healthCheck'.` |
| `onReadinessCheck` (under `actions`) | `'onReadinessCheck' is not a property; the health check action is 'onHealthCheck'.` |
| `onWrapServiceReadinessCheck` (under `actions`) | `'onWrapServiceReadinessCheck' is not a property; the hook is 'onWrapServiceHealthCheck'.` |
| `timeoutSeconds` / `readyTimeoutSeconds` (a health check) | `'<field>' is not a property of a health check; the time allowed to become READY is 'readinessTimeoutSeconds'.` |
| `intervalSeconds` (a health check) | `'intervalSeconds' is not a property of a health check; use 'readinessIntervalSeconds' for probes before READY and 'healthIntervalSeconds' for probes after.` |
| `readinessIntervalSeconds` on a `STDOUT` check | `'readinessIntervalSeconds' does not apply to a STDOUT health check: the ready line arrives when it arrives.` |
| `missing field \`ports\`` on a Service | `A Service declares at least one port in 'ports'; a port-less background process is not a Service.` |

A hint applies only at its site (`readinessCheck` on a Step gets none). `maxAttempts` in a
`restartPolicy` (renamed `maxRestarts`) is an unknown property with no hint. The pre-pass-3
failures — a missing or unknown `specificationVersion`, the wrong template kind — remain
`DecodeValidation` strings.

Custom deserializers handle:
- **`ExtensionName`** — Validates regex pattern during deserialization
- **`FormatString`** — Parses `{{...}}` interpolation syntax
- **`JobParameterDefinition`** — Case-insensitive `type` field matching, strips `type` before
  delegating to variant-specific deserialization
- **`TaskParameterDefinition`** — Uses serde's `#[serde(tag = "type")]`
- **`IntRange`/`StringRange`/`FloatRange`** — Distinguishes list vs expression string
- **`IntOrFormatString`/`FloatRangeItem`** — A list element that is a number or a
  format string (`IntRange::List`, `FloatRange::List`, and the CHUNK[INT] chunks counts)
- **`FlexInt`/`FlexFloat`** — Accepts multiple YAML value representations
- **`BoolValue`** — Accepts boolean, numeric, and string representations
- **`EnvironmentParameterDefinition`** (ENV_PARAMS) — Removes `default`, decodes the rest
  as a `JobParameterDefinition`, and keeps `default` as an `EnvironmentParameterValue`,
  since a format-string `default` (`"{{Param.Seats}}"` on an INT) does not fit the Job
  Parameter type's typed `default` field
- **`EnvironmentParameterValue`** (ENV_PARAMS) — A string containing `{{` is parsed as a
  `FormatString` (a brace mismatch fails decode); anything else is kept as the written JSON
  value, which pass 11 checks against the parameter's type

### Pass 4: Extension Resolution

Each extension the template requests (via its `extensions` field) must be present in the
caller's `supported_extensions` list. If a requested extension is not supported, decoding
fails with `ModelError::DecodeValidation`. When `supported_extensions` is `None`, it
defaults to an empty set — no extensions are supported.

The resulting extension set is stored in a `ValidationContext`.

Once the extension set is known, pass 4 also applies §2's casing rule: without EXPR,
parameter type names are case-sensitive, so `string` is not a spelling of `STRING`.
Deserialization matches the `type` tag case-blind, so the names as written are collected
from the raw document before pass 3 (`collect_written_type_names`) and checked here
(`check_type_name_canonical_case`). The collected names are the top-level
`parameterDefinitions` and each Step's `taskParameterDefinitions`. An Environment's
`envParameterDefinitions` (ENV_PARAMS) are not collected: a document that lists ENV_PARAMS
lists EXPR too (Template Schemas §1.1 item 3), under which names are case-insensitive, and
template validation rejects any other document that has them (see
[validation.md](validation.md#entry-points)). A name that matches no type is left to the
deserializer's "unknown parameter type" error. These errors are returned before passes
5–11 run.

An empty `extensions: []` list is rejected for both job and environment templates during
pass 4 with a `DecodeValidation` error. If a template does not use any extensions, the
`extensions` field should be omitted entirely.

### Passes 5–11: Validation

The deserialized template is passed through the multi-pass validation pipeline
(see [validation.md](validation.md)). Validation errors are accumulated and returned
as a single `ModelError::ModelValidation`.

## Design Decisions

### serde_yaml::Value as Input Type

The decode functions accept `serde_yaml::Value` rather than `&str` because:

1. Callers may need to inspect the raw value tree before decoding (e.g., to read
   `specificationVersion` for routing)
2. The same value tree can be used for both JSON and YAML sources
3. It separates syntax parsing (pass 1) from semantic decoding (passes 2–9)

### Comparison with Python

The Python library uses `parse_model()` which calls `model_validate()` (Pydantic v2) for
combined deserialization + validation. The Rust crate separates these because serde
deserialization is stateless and can't accumulate multiple validation errors. The multi-pass
pipeline runs after deserialization to provide comprehensive error reporting.
