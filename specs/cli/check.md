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
`stepEnvironments`. A Step would enter an Environment with `activation: BY_DEFAULT` for
which it has no entry or one whose `enableIf` is absent or `true`, an `ON_REQUEST` one requested
by such an entry (an attached one only when exactly one attachment has the name and no Job
Environment shadows it), and each of its own Step Environments. A Step whose entry for a
`BY_DEFAULT` Environment has `enableIf: false` declines it (Template Schemas §3.2 item 3.2): the
Environment's own `enableIf` is not evaluated for that Step, so it is not listed for it, as
`openjd summary` does not list it under "Disabled environments" (it lists it under "Declined
environments"). An Environment with `activation: ALWAYS` cannot be disabled:
template validation rejects one with `enableIf: false` before this list is made (Template
Schemas §3.2.2 item 6). These lines name only what the boolean scalar `enableIf: false`
disables: an Environment or entry whose `enableIf` is an expression is not listed, even one
whose value template validation knows, such as `"false"` (§7.6 rule 5); `openjd summary`
shows each Step's Environments with the Job Parameter values (see
[summary.md](summary.md)). An Environment superseded by an earlier one of its
`exclusiveGroup` (Template Schemas §4 item 7) is disabled too, but these lines list only what
the scalar `enableIf: false` disables; `openjd summary` lists the superseded ones. A Job
Template with no such Environment prints only the success line, as before. The lines are
informational. A listed Environment is disabled for the Step, so the Step's values for it are
not checked against their type and constraints (Template Schemas §3.2.2): a value outside
its constraints still passes. It still applies to the Step, so the Step must supply every
parameter of it without a `default` (§3.2 constraint 7), and a key the Environment does not
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

- each entry whose `enableIf` is known to be `true` (absent, the boolean scalar, or an
  expression whose value template validation knows, such as `"true"`, Template Schemas §7.6
  rule 5) resolved by name: a missing `required` Environment and an ambiguous name are errors,
  and a `required: false` entry that resolves to nothing is a warning. An entry known to be
  disabled supplies nothing; it declines each `BY_DEFAULT` attachment it names, is an error
  for each `ALWAYS` one, and is not an error when several attachments share its name
  (Template Schemas §3.2.1 item 3). One whose `enableIf` is not known is checked only by
  `openjd run`, as is every attachment it may name, for its Step;
- a `required: false` entry that resolves to an attached Environment that declares no
  parameters, and that supplies some, is a warning: its parameters are ignored (Template
  Schemas §3.2.1 item 1), and it still requests an `ON_REQUEST` Environment;
- for any other resolved entry, every key names a declared parameter (constraint 5); and, when the
  attached Environment is known not to be disabled for the Step (its `activation` is
  `ALWAYS`, or its own `enableIf` is known to be `true`, or reads a literal value that does
  not convert, so it is not evaluated, §3.2.2 item 2; see
  [validation.md](../model/validation.md)) and no earlier one of its `exclusiveGroup`
  supersedes or may supersede it (Template Schemas §4 item 7, and the bullets after item 10),
  every literal value converts and satisfies its definition (constraint 6; a PATH value of
  any form, relative ones included, is used as given, Template Schemas §4.5 item 3), and a
  value for a list-typed parameter that is a format string is one `{{ }}` expression. For an
  Environment whose `enableIf` is known to be `false` the values are not checked (§3.2.2); for
  one whose `enableIf` is not known a violation is not certain without Job Parameter values
  (§7.6 rule 5), so `openjd run` makes those checks;
- every Step supplies every parameter without a `default` of every reachable attached
  Environment that applies to it: each one with `activation: ALWAYS`, each one with
  `BY_DEFAULT` it does not decline, and each one with `activation: ON_REQUEST` that one of its
  entries refers to (constraint 7). This holds whatever the Environment's own `enableIf` or
  `exclusiveGroup`: a disabled or superseded Environment still applies to the Step (Template
  Schemas §3.2 constraint 7). An attachment that an entry whose `enableIf` is not known may
  name is left to `openjd run` for that Step;
- the value checks for each `jobEnvironments` entry with an `exclusiveGroup` that the Step is
  known to enter, given the attachments (template validation, which does not know them,
  leaves these to `openjd run`); the other Job Environments' values, and every Job
  Environment's constraint 7, grouped ones included, were checked by template validation;
- an attached Environment named like a `jobEnvironments` entry is a warning when the Job
  Template lists `ENV_PARAMS` (§3.2.1 item 4). When such an attachment, or one of two or more
  attachments with the same name (item 3), has parameters without a `default` and
  `activation: ALWAYS` or `BY_DEFAULT`, it applies to every Step and no entry can supply them,
  so that is an error at its `envParameterDefinitions`, whatever its own `enableIf` or
  `exclusiveGroup` (§3.2 constraint 7). No entry decides whether such an attachment applies,
  so an entry naming its name, with any `enableIf`, does not change this. With `activation:
  ON_REQUEST` it applies to no Step, so it is not an error;
- every attached literal `default` is checked (§4.5 item 2);
- the RFC 0008 single-layer rule, for each Step (template validation already checked a Step
  whose only wrapping layers are ungrouped `jobEnvironments` entries and its Step
  Environments, and it counts no `jobEnvironments` entry with an `exclusiveGroup`): the wrapping Environments its Sessions certainly enter (attached
  ones with `activation: ALWAYS`, with `BY_DEFAULT` when the Step has no entry for it or an
  enabled one (an entry known to be disabled declines it, and one that is not known may), or
  requested by a resolved entry; Job Environments in the same way, with an entry whose
  `enableIf` is known to be `true` enabled; and its Step Environments; each only
  when its own `enableIf` is known to be `true` or it is `ALWAYS`, and, for one
  with an `exclusiveGroup`, only when every earlier one of its group is known not to be
  entered) must be at most one;
- each Step's combined host requirements (§3.3.3), over the requirements written as literals
  of the Step's own `hostRequirements` and of the Environments its Sessions certainly enter
  (attached and the Job Template's own, as for the single-layer rule, so not one whose own
  `enableIf` is not known to be `true`, unless it is `ALWAYS`, nor one an
  earlier one of its group may supersede): a conflict (a `min` above a `max`, or
  `attr.worker.os.family` or `attr.worker.cpu.arch` lists with no value in common) is an error, with `openjd run`'s message when only literal
  requirements of that capability take part. A conflict between the Job Template's own
  requirements alone is therefore reported with any `--environment`, though `check` without
  it, and template validation, leave it to `openjd run`.

A Step's entry known to be disabled naming an attached Environment declines it when its
`activation` is `BY_DEFAULT`, so the checks above leave it out for that Step, and is an
error naming the Step and the Environment when it is `ALWAYS` (Template Schemas §3.2 item
3.2). It has no effect on an attached Environment whose Environment Template does not list
ENV_PARAMS, which behaves as `ALWAYS` with no `enableIf` (§4 item 4): the checks above count
it as entered by every Step, whatever the Step's entry for it says. An entry whose `enableIf`
is known to be `false`, a constant expression such as `"false"` included (§7.6 rule 5), is
treated the same way, so an entry `enableIf: "false"` naming an attached `ALWAYS`
Environment is reported here. An entry whose `enableIf` is not known is not: §4 item 4 has
that error reported when the Job is created, and `openjd run` rejects the template when the
expression is `false`. When an `ALWAYS` Environment's missing parameters are reported and the
Step's entry for it is known to be disabled, the message says so.

Format-string values, non-literal `default`s and host requirements, and entries and
Environments whose `enableIf` is not known before the Job is created need Job Parameter
values, so `openjd run` checks them. The
messages and paths are those `openjd run` reports for the same
submission, except that a single-layer or host-requirement error names only what `check`
can know without Job Parameter values (the layers a Session certainly enters, the literal
requirements), so `openjd run` may count or name more, and that `check` reports every
violation it is certain of, where `openjd run` reports a Step's value errors only once its
§7.4.1 step 1–4 errors are fixed (Template Schemas §7.4.1; see
[job-creation.md](../model/job-creation.md#error-order)): errors are one
`Submission` validation error whose paths start with the document (`JobTemplate`, or the
Environment Template's path), and warnings go to stderr.
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
