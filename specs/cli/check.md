# check Command

## Purpose

`openjd check <path>` validates a job template or environment template file against the
OpenJD 2023-09 specification. It reports whether the template passes all validation checks
or prints the first error encountered.

This is the simplest command — it parses, detects the template type, validates, and prints
a success or error message (with `ENV_PARAMS`, after a line per Step for the Environments a
scalar `enableIf: false` disables). No job instantiation or parameter processing occurs. With
`--environment` (`ENV_PARAMS`), a Job Template is also checked against the Environment
Templates a queue would attach; see [Checking against Environment
Templates](#checking-against-environment-templates).

## Interface

```
openjd check <PATH> [--extensions <EXTENSIONS>] [--environment <PATH>]...
```

### Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `path` | `PathBuf` | Yes | Path to the template file (YAML or JSON) |
| `--extensions` | `Option<String>` | No | Comma-separated extension names. Defaults to all supported. Empty string disables all. |
| `--environment` (alias `--env`) | `Vec<PathBuf>` | No | Environment Template files to check a Job Template against, in attachment order (repeatable). Rejected when `path` is an Environment Template. |

### CheckArgs Struct

```rust
#[derive(Args)]
pub struct CheckArgs {
    pub path: PathBuf,
    #[arg(long = "extensions")]
    pub extensions: Option<String>,
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,
}
```

The struct uses clap's `Args` derive. `path` is a positional argument. `--extensions` is
optional with no default value at the clap level — the default is applied in `execute()`.

## Execution Pipeline

```
execute(args)
  │
  ├── Read file content via common::read_input_file()
  │   (single read that maps errors to:
  │    "does not exist" / "is not a file" / "Cannot read ...")
  │
  ├── Detect document type from file extension
  │   └── .json → DocumentType::Json, anything else → DocumentType::Yaml
  │
  ├── Parse to serde_json::Value via parse::document_string_to_object()
  │
  ├── Extract specificationVersion string from the parsed value
  │
  ├── Resolve extensions list
  │   ├── --extensions "" → empty vec (no extensions)
  │   ├── --extensions "A,B" → vec!["A", "B"]
  │   └── (not provided) → every name in ModelExtension::ALL
  │
  ├── Parse specificationVersion into TemplateSpecificationVersion
  │
  ├── Dispatch on template type
  │   ├── Job template → parse::decode_job_template(value, extensions, caller_limits)
  │   │   └── With --environment: decode each Environment Template (an error
  │   │       names the file), openjd_model::merge_job_parameter_definitions,
  │   │       then openjd_model::check_environment_templates(job_template, attached, ctx);
  │   │       print its warnings to stderr as "WARNING: <path>: <message>"
  │   │   └── Print one line per Step that a scalar `enableIf: false` disables
  │   │       Environments for (see [Disabled Environments](#disabled-environments))
  │   └── Environment template → reject --environment, then
  │       parse::decode_environment_template(value, extensions, caller_limits)
  │
  └── Print "Template at '<path>' passes validation checks."
```

## Disabled Environments

With `ENV_PARAMS`, an Environment's own `enableIf` (Template Schemas §4 item 6) can keep a
Step's Sessions out of it. For a valid Job Template, `check` prints to stdout, before the
success line, one line for each Step whose Sessions would otherwise enter an Environment that
the boolean scalar `enableIf: false` disables (`disabled_environment_lines`):

```
Step 'S': enableIf: false disables Environments 'Off' (jobEnvironments), 'Scratch' (stepEnvironments) for its Sessions.
```

The Environments are named in Session order: attached ones (with `--environment`, labeled
with their path), then the Job Template's `jobEnvironments`, then the Step's own
`stepEnvironments`. A Step would enter an Environment with `activation: ALWAYS`, an
`ON_REQUEST` one requested by an entry whose `enableIf` is absent or `true` (an attached one
only when exactly one attachment has the name and no Job Environment shadows it), and each of
its own Step Environments. An Environment or entry whose `enableIf` is an expression (even a
constant one) is decided when the Job is created, so it is not listed; `openjd summary`
shows each Step's Environments with the Job Parameter values (see
[summary.md](summary.md)). A Job Template with no such Environment prints only the success
line, as before. A listed Environment is disabled for the Step, so it is not held to
constraint 7 and the Step's values for it are not checked against their type and
constraints (Template Schemas §3.2.2): a Step that supplies none of its parameters without a
`default`, or a value outside its constraints, still passes. A key the Environment does not
declare and a value no type has (`null`, an object) are still errors.

## Checking against Environment Templates

Template Schemas §3.2 checks an `environment:` dependency that names no `jobEnvironments`
entry only when the Job is created, since the Environment comes from the queue. Its last
paragraph says a validator given the Environment Templates, as `openjd run` accepts them, can
check constraints 5–7 early. `--environment` does that for a Job Template (prototype plan
A9). The templates are attached in the order given, each labeled with its path as given. The
Job Parameter definitions of the Job Template and the Environment Templates are merged first
(`openjd_model::merge_job_parameter_definitions`, Template Schemas §1.2.1), so a conflicting
definition is reported as `openjd run` reports it. Then
`openjd_model::check_environment_templates` runs every check of job creation's submission
stage that needs no Job Parameter values:

- each entry whose `enableIf` is absent or the boolean scalar `true` resolved by name: a
  missing `required` Environment and an ambiguous name are errors, and a `required: false`
  entry that resolves to nothing is a warning. An entry whose `enableIf` is `false` is as if
  absent, and one whose `enableIf` is an expression (an expression field, Template Schemas
  §7.5, so even a constant one such as `"true"`) is checked only by `openjd run`, as is
  every attachment it may name, for its Step;
- for a resolved entry, every key names a declared parameter (constraint 5); and, when the
  attached Environment's own `enableIf` is absent or `true`, or is certainly unresolved for the
  Step (it reads only values that stay unknown, see [validation.md](../model/validation.md)),
  every literal value converts and satisfies its definition, including PATH absoluteness in the
  host path format (constraint 6), and a
  value for a list-typed parameter that is a format string is one `{{ }}` expression;
- every Step supplies every parameter without a `default` of every reachable attached
  Environment its Sessions enter whose own `enableIf` is absent or `true`, or certainly
  unresolved: each one with
  `activation: ALWAYS`, and each one with `activation: ON_REQUEST` that one of its entries
  refers to (constraint 7). An Environment whose `enableIf` is `false` is disabled, so its
  values are not checked and it is not held to constraint 7 (Template Schemas §3.2.2); for
  one whose `enableIf` is an expression a violation is not certain without Job Parameter
  values (§7.5 rule 5), so `openjd run` makes those checks;
- an attached Environment named like a `jobEnvironments` entry is a warning when the Job
  Template lists `ENV_PARAMS` (§3.2.1 item 4). When such an attachment, or one of two or more
  attachments with the same name (item 3), has `activation: ALWAYS`, parameters without a
  `default`, and an own `enableIf` that is absent or `true` (or certainly unresolved), every
  Session enters it and no
  entry can supply them, so that is an error at its `envParameterDefinitions`. One whose
  `enableIf` is `false` is entered by no Session; for one whose `enableIf` is an expression
  `openjd run` decides. With `activation: ON_REQUEST` no Session enters it, so it is not an
  error;
- every attached literal `default` is checked (§4.5 point 1);
- the RFC 0008 single-layer rule, for each Step whose Sessions enter an attached Environment
  that defines wrap hooks: the wrapping Environments its Sessions certainly enter (attached
  ones with `activation: ALWAYS` or requested by a resolved entry, Job Environments with `ALWAYS` or
  requested by an entry whose `enableIf` is absent or the scalar `true`, and its Step
  Environments, each only when its own `enableIf` is absent or the scalar `true`) must
  be at most one;
- each Step's combined host requirements (§3.3.3), over the requirements written as literals
  of the Step's own `hostRequirements` and of the Environments its Sessions certainly enter
  (attached and the Job Template's own, as for the single-layer rule, so not one whose own
  `enableIf` is the scalar `false` or an expression): a conflict (a `min` above a `max`, or
  `attr.worker.os.family` or `attr.worker.cpu.arch` lists with no value in common) is an error, with `openjd run`'s message when only literal
  requirements of that capability take part. A conflict between the Job Template's own
  requirements alone is therefore reported with any `--environment`, though `check` without
  it, and template validation, leave it to `openjd run`.

When an Environment's missing parameters are reported and the Step's entry for it was
disabled by `enableIf: false`, the message says so. A note marks an Environment whose own
`enableIf` is unresolved for the Step (it depends on a value that is missing or does not
convert): `check` adds it for one whose `enableIf` is certainly unresolved, and `openjd run`
for any whose `enableIf` was unresolved.

Format-string values, non-literal `default`s and host requirements, and entries and
Environments whose `enableIf` is an expression need Job Parameter values, so `openjd run`
checks them. The
messages and paths are those `openjd run` reports for the same
submission, except that a single-layer or host-requirement error names only what `check`
can know without Job Parameter values (the layers a Session certainly enters, the literal
requirements), so `openjd run` may count or name more: errors are one `Submission` validation error whose paths start with the
document (`JobTemplate`, or the Environment Template's path), and warnings go to stderr.
Without `--environment`, `check` makes no assumption about which Environments a queue
attaches and reports none of these.

## Caller-Limits Policy

Every decode call passes `common::caller_limits()` — the library defaults
plus `max_resolved_arg_len` set to `common::DEFAULT_MAX_ARG_LEN` (32K
characters, uniform across platforms). Template Schemas §5.1/§5.2 set no
maximum of their own but note the OS imposes one; the CLI surfaces
hopeless values at `check` time whenever an action `command`/`args` value
is *guaranteed* to exceed the cap (the lower bound of every possible
resolution is over it). 32K is an opinionated *reasonable default*, not
an encoding of any particular OS limit — the real OS limits are measured
in different units (Linux `MAX_ARG_STRLEN` is bytes; the Windows command
line is UTF-16 code units), so no character count maps exactly onto
them. 32K characters is at most 128 KiB of UTF-8 (within Linux's
per-string limit) and approximately the Windows command-line capacity;
the OS itself and the worker's run-time enforcement remain
authoritative. The cap bounds each single argument — nothing bounds the
argv element count or the argv total (`ARG_MAX`-style budgets are
possible future work). Library users of `openjd-model` get no cap by
default — this is CLI policy.

## Template Type Detection

The command determines whether the file is a job template or environment template by
parsing the `specificationVersion` field and calling `is_job_template()` or
`is_environment_template()` on the parsed version enum. This mirrors the Python CLI's
approach of inspecting the version string prefix (`jobtemplate-` vs `environment-`).

If the version string doesn't match any known pattern, the command reports an error with
the unrecognized version string.

## Error Handling

The function returns `Result<(), Box<dyn std::error::Error>>`. Errors propagate to `main()`
which prints them to stderr and exits with code 1. Error sources:

- File not found or not a regular file (checked explicitly before reading)
- File read failure (I/O error from `read_to_string`)
- Parse failure (malformed YAML/JSON)
- Missing `specificationVersion` field
- Unknown specification version
- Validation errors from `decode_job_template` or `decode_environment_template`

Validation errors from the model crate include structured paths (e.g.,
`steps[0] -> script -> actions -> onRun -> command`) that help users locate the problem
in their template file.

## Differences from Python CLI

| Aspect | Python | Rust |
|--------|--------|------|
| Output formatting | `@print_cli_result` decorator, supports `--output` (human/json/yaml) | Plain `println!`, no `--output` flag |
| Return type | `OpenJDCliResult` dataclass with status/message | `Result<(), Box<dyn Error>>` |
| Common args | Uses `CommonArgument.PATH` for shared path argument | Defines `path` directly in `CheckArgs` |

The Rust `check` command does not support `--output` for structured output. The Python CLI
wraps the result in an `OpenJDCliResult` dataclass and formats it according to `--output`,
but for a simple pass/fail validation command, the structured output adds little value.
Adding `--output` support would be straightforward if needed for tooling integration.
