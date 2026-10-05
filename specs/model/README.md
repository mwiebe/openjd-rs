# openjd-model Crate Specifications

Design specifications for the `openjd-model` crate — the Rust implementation of the
Open Job Description template model.

This crate implements parsing, validation, and instantiation of OpenJD job and environment
templates per the [2023-09 Template Schemas](https://github.com/OpenJobDescription/openjd-specifications/wiki/2023-09-Template-Schemas)
specification. It was inspired by the Python reference implementation
([openjd-model-for-python](https://github.com/OpenJobDescription/openjd-model-for-python))
but redesigned for Rust's type system and performance characteristics.

## Document Index

| Document | Description |
|----------|-------------|
| [architecture.md](architecture.md) | Crate structure, module layout, dependency graph, and key design decisions |
| [public-api.md](public-api.md) | Authoritative reference for every public type, function, and re-export |
| [template-types.md](template-types.md) | Unresolved template types (deserialized from YAML, format strings unevaluated) |
| [job-types.md](job-types.md) | Instantiated job types (fully resolved, output of `create_job`) |
| [parameters.md](parameters.md) | Job and task parameter type systems, value coercion, constraint checking |
| [parsing.md](parsing.md) | Template decoding pipeline: YAML/JSON → version dispatch → serde → validation |
| [validation.md](validation.md) | Multi-pass validation pipeline: limits, structure, extensions, format strings |
| [job-creation.md](job-creation.md) | Job creation pipeline: parameter merging, preprocessing, symbol table, instantiation |
| [parameter-space.md](parameter-space.md) | Lazy parameter space iteration: node tree, combination expressions, chunking |
| [step-dependencies.md](step-dependencies.md) | Step dependency graph: construction, topological sort, cycle detection |
| [capabilities.md](capabilities.md) | Standard capability constants, name validation functions, regex patterns |
| [error-handling.md](error-handling.md) | Error types, structured error paths, validation error accumulation |

## Two-Phase Type System

The crate's central architectural pattern is a two-phase type system that mirrors the two
stages of the job lifecycle:

1. **`template::*` types** — A template file (JSON or YAML) is parsed and validated according
   to the specification revision and extensions it declares. The result is a typed template
   struct with `FormatString` fields still unevaluated. This phase catches structural errors,
   constraint violations, and invalid variable references before any parameter values are involved.

2. **`job::*` types** — A validated template is combined with specific job parameter values
   via `create_job()` to produce a runnable job. Template-scope format strings are resolved
   to concrete values; session/task-scope strings remain as `FormatString` for evaluation
   at runtime when the execution environment is known.

This separation reflects the job lifecycle: a template is a reusable artifact that is validated
once, then instantiated into many jobs with different parameter values. The two type phases
exist because a template definition and a concrete job instance are fundamentally different
things.

## Relationship to the Python Library

The Rust crate mirrors the Python library's public API surface but diverges in implementation:

- **Validation**: Python uses Pydantic model validators; Rust uses a multi-pass pipeline with
  explicit `ValidationErrors` accumulation after serde deserialization.
- **Type dispatch**: Python uses Union types over version-specific Pydantic models; Rust uses
  enums and direct struct types with `#[serde(deny_unknown_fields)]`.
- **Parameter space**: Python's `StepParameterSpaceIterator` mutates a passed-in dict to avoid
  allocation; Rust's version uses index arithmetic on a node tree for zero-allocation random access.
- **Error formatting**: Both produce Pydantic-compatible error paths for consistency with existing
  tooling and error message expectations.

## Specification Version Coverage

Currently implements `2023-09` with extensions:
- `TASK_CHUNKING` (RFC 0001)
- `REDACTED_ENV_VARS` (RFC 0003)
- `FEATURE_BUNDLE_1` (RFC 0004)
- `EXPR` (RFC 0005)
- `WRAP_ACTIONS` (RFC 0008) — full schema and runtime support. Wrap-action
  routing tests live in `crates/openjd-sessions/tests/integration/test_wrap_actions.rs`.
  Re-materialization of the wrap environment's embedded files on each task
  run (needed to resolve `Env.File.*` inside `onWrapTaskRun` scripts) is a
  follow-up.
- `SERVICE` (RFC 0009) — implemented. The `<Service>` schema (one `services`
  list per document, `requiresServices` on a Job Template, the §9 sub-objects
  incl. `dependencies` and `<ServiceRequirement>`), the extension gating and
  EXPR prerequisite, the §9.9 structural checks, Service scope computed from
  `Service.*` references (`template::service_scope`: the four §9.1 rules,
  reference-cycle detection, `dependencies` against the computed scope),
  `<Environment>.runScope` with its reference-driven default and the
  `Environment::runs_in` accessor, the four `onWrapService*` hooks and the
  hooks-follow-`runScope` rule, the Environment Template root changes
  (`$schema`, optional `environment`, "at least one of"); the `Service.*` /
  `Service.File.*` format-string scope with the §9 scope rules (inline Services
  in scope wherever a reference may appear, required Services' declared ports,
  the `runScope` exclusion, no `Service.*` in `hostRequirements` or
  `<Service>.let`, §9.9 items 1–2) and their diagnostics
  (`service_diagnostics.rs`), the `WrappedService.*` wrap-hook variables,
  pass-8 validation of every format string and `let` inside a Service, job
  creation of Services (`job::Service` with its computed `scope` and
  `references`, `Job::services`, `Job::requires_services`, resolved
  `<Service>.let` and numeric fields, job-side `runScope` / Service hooks), the
  runtime-facing `job::service_symbols` builders, and the submission stage
  (`apply_environment_templates`: §1.2.2 external Services merged before the
  Job's `services` with `AllSteps` scope and stamped with their
  `job::Document`, requirement matching with `RequirementBinding`s, the
  wrapping-Environment check, per-document profiles via
  `EnvironmentTemplate::profile`); see [template-types.md](template-types.md),
  pass 8 "Service scopes", passes 10–11, and "Submission-time checks" in
  [validation.md](validation.md), "Services" and
  "apply_environment_templates" in [job-creation.md](job-creation.md), and
  [job-types.md](job-types.md). Execution lives in `openjd-sessions`
  (`ServiceSession`) and orchestration in `openjd-cli` (`openjd run`, which
  calls `apply_environment_templates`).
