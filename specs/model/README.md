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
| [public-api.md](public-api.md) | Reference for every public type, function, and re-export |
| [public-api.txt](public-api.txt) | Generated listing of every public item and signature, checked by CI (`scripts/check_public_api.sh`) |
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
- `ENV_PARAMS` (Environment Parameters RFC, draft) — prototype, implemented to the spec at
  `2491d6e`. A document that lists `ENV_PARAMS` must also list `EXPR` (Template Schemas §1.1
  item 3, §1.2 item 3; see [validation.md](validation.md#entry-points)). Decode and
  template validation are implemented: Environment `envParameterDefinitions`, `activation`,
  `hostRequirements`, and `enableIf` (an expression field, §4 item 6),
  the `step:` and `environment:` forms of `<StepDependency>`, the latter with `enableIf` (an
  expression field, Template Schemas §7.5; see
  [template-types.md](template-types.md#expression-fields-75)), `Job.EnvNames`, and the
  `Env.Param.*` / `Env.RawParam.*` scopes (see [validation.md](validation.md) § Pass 11 and
  § Environment parameter scopes), and so are, at job creation in the Template Schemas §7.4
  order, entry `enableIf`, each Step's Environment set, effective values, and external
  Environment resolution against the attached Environment Templates
  (`create_job_with_environment_templates`; see
  [job-creation.md](job-creation.md#environment-parameter-values-env_params)), each
  Environment's own `enableIf` per Step with the values that are known, before the checks
  of the values and constraint 7, which a disabled Environment escapes (see
  [job-creation.md](job-creation.md#environment-enableif-env_params)), and last each
  Environment's `hostRequirements` resolved per entering Step and combined with the Step's
  own (see [job-creation.md](job-creation.md#combined-host-requirements-env_params)). The
  openjd-sessions runtime binds the values (see
  [session.md](../sessions/session.md#enter-with-environment-parameter-values-env_params)), and
  the CLI runs, checks, and summarizes such Jobs (see [../cli/run.md](../cli/run.md)), so
  `ModelExtension::EnvParams` is in `ModelExtension::ALL`. `check_environment_templates` runs
  the submission checks that need no Job Parameter values. Plans:
  [`docs/env-params-prototype-plan.md`](../../docs/env-params-prototype-plan.md),
  [`docs/env-params-prototype-plan-2.md`](../../docs/env-params-prototype-plan-2.md), and
  [`docs/env-params-prototype-plan-3.md`](../../docs/env-params-prototype-plan-3.md) for the
  update to the spec at `2a5a930` (renames, `step:`, expression fields, and an
  Environment's own `enableIf`), and
  [`docs/env-params-prototype-plan-4.md`](../../docs/env-params-prototype-plan-4.md) for the
  update to `bf81141` (disabled Environments, RANGE_EXPR parameters, and the full
  `<JobParameterDefinition>` shape), and
  [`docs/env-params-prototype-plan-5.md`](../../docs/env-params-prototype-plan-5.md) for the
  update to `d335619` (the exact attribute conjunction, and RANGE_EXPR equality by spelling,
  since replaced by equality by canonical form and length checks of both the written value and
  its canonical form, spec `80a2018`),
  and [`docs/env-params-prototype-plan-6.md`](../../docs/env-params-prototype-plan-6.md) for
  the update to `2491d6e` (`ENV_PARAMS` requires `EXPR`). Not yet implemented
  in the Python reference (`openjd-model-for-python`), so there is no Python behavior to
  match.
