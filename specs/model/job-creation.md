# Job Creation Pipeline

The `create_job` module transforms parsed templates into instantiated jobs. This is the core
workflow: templates + user-provided parameter values → a `job::Job` ready for session execution.

## Public API

### merge_job_parameter_definitions

```rust
pub fn merge_job_parameter_definitions(
    job_template: &JobTemplate,
    environment_templates: &[EnvironmentTemplate],
) -> Result<Vec<MergedParameterDefinition>, ModelError>
```

Merges parameter definitions from environment templates (processed in order) then the job
template (last), per §1.2.1.

Environment templates and job templates share the same parameter namespace. This allows an
environment template to accept a parameter that defines, for example, which software
installation to provide, while the job template defines that parameter's default value for
the software the job needs. The merge rules accommodate this and similar use cases where
multiple templates collaborate on the same parameters.

Returns a list of `MergedParameterDefinition` entries, each containing the merged definition
and its source template.

**Merge rules:**
- All definitions of the same parameter must have the same type
- `allowedValues`: intersection (must be non-empty after intersection)
- `minLength`/`minValue`: takes the maximum (most restrictive)
- `maxLength`/`maxValue`: takes the minimum (most restrictive)
- PATH-specific: `objectType` and `dataFlow` must be identical across definitions
- Default value: last template to define one wins
- RANGE_EXPR `minLength`/`maxLength`: each definition's lengths keep the rule of the document that
  contains it (Template Schemas §1.2.1 and §2.10, which state it since spec `c12a76e`; before, it
  followed from §1.2 item 3). Every definition's lengths constrain the value as written; only
  the definitions whose document lists `ENV_PARAMS` constrain its canonical form (§2.10). The
  merge keeps these as two pairs of bounds, so a value is accepted exactly when it satisfies every
  definition under its own document's rule. With an EXPR-only Job Template declaring
  `minLength: 4` and an `ENV_PARAMS` Environment Template supplying the `default` `1-3:2`, the
  default is accepted; with the extensions swapped, it is rejected for its canonical form `1,3`.

Conflicts produce `ModelError::Compatibility` with details about which templates conflict.

### preprocess_job_parameters

```rust
pub fn preprocess_job_parameters(
    job_template: &JobTemplate,
    input_values: &JobParameterInputValues,
    environment_templates: &[EnvironmentTemplate],
    path_options: &PathParameterOptions<'_>,
) -> Result<JobParameterValues, ModelError>
```

Validates and coerces user-provided parameter values against merged definitions.

`PathParameterOptions` consolidates path-related options:

```rust
pub struct PathParameterOptions<'a> {
    pub job_template_dir: &'a Path,
    pub current_working_dir: &'a Path,
    pub allow_template_dir_walk_up: bool,
    pub path_format: PathFormat,
    pub allow_uri_path_values: bool,
}
```

**Pipeline:**
1. Merge parameter definitions from all templates
2. Check for extra (undefined) parameters in input
3. Fill defaults for missing parameters
4. Coerce values to target types
5. Validate PATH parameters (relative path resolution, URI handling)
6. Check constraints (allowedValues, min/max, length bounds)
7. Validate merged constraints across multiple definitions
8. Error on still-missing required parameters

**PATH handling** (Template Schemas §2.2; §2.12 applies the same rules to each `LIST[PATH]` element):
- User-provided relative paths joined to `current_working_dir` and lexically normalized (`.` removed,
  `..` applied, no symlink resolution). Absolute user-provided paths are kept as written, and a
  user-provided path may walk up out of `current_working_dir`.
- Default relative paths joined to `job_template_dir` and lexically normalized. An absolute default is
  an error.
- URI paths (`s3://`, `https://`) preserved as-is when EXPR extension is enabled and
  `allow_uri_path_values` is set; rejected otherwise. URIs are never joined.
- `allow_template_dir_walk_up` controls whether paths can traverse above `job_template_dir`. When `false`, a default is rejected unless its normalized form is `job_template_dir` itself or a descendant. Containment is checked per path component, not by raw string prefix. When `true`, absolute and escaping defaults are accepted.
- `LIST[PATH]` errors name the element in the same `item[i]` form `check_constraints` uses
  (`The default value of LIST[PATH] parameter P at item[1] is an absolute path. ...`), and every bad
  element is reported, not just the first.
- Lexical normalization follows the path format: under `PathFormat::Windows` both `/` and `\` separate
  components; under POSIX only `/` does, and a backslash is an ordinary filename character.
- `allowedValues`, `minLength`, and `maxLength` (and `item.*` for `LIST[PATH]`) are checked at two
  stages (Template Schemas §2.2, §2.12). Template validation checks a default as written. Job
  creation checks the joined value, for submitted values and defaults alike, because the join is done
  by the submitting client and the job may be created elsewhere from the joined values. A relative
  default therefore passes both only if `allowedValues` contains both the default as written and its
  joined path. `preprocess_job_parameters` checks every merged parameter, including those contributed
  only by environment templates; `create_job` re-checks the job template's own parameters.

**Value coercion:**
- `coerce_from_str` — Parses string input (CLI): numeric parsing that ignores white space
  around the number (Template Schemas §7.5; see [parameters.md](parameters.md#value-coercion)),
  boolean aliases (`yes`/`no`/`on`/`off`/`1`/`0`), JSON list parsing for list types
- `coerce_to_job_parameter_type` — Validates typed input (library): type compatibility, numeric
  widening (int → float), list element type validation

**Round trip:** a value `preprocess_job_parameters` returns should be a value it accepts as input.
Both its input and output types are public, so a caller can hand its output straight back to it.
`create_job` re-runs `check_constraints` over each value it is given (`create_job/mod.rs`, the
"Validate parameter values against template constraints" loop at the start of `create_job`), which
holds for every caller with one exception. A RANGE_EXPR value reaches it as a `range_expr`, whose
spelling as submitted is gone, so the re-check tests only its canonical form, and only against the
lengths of a template that lists `ENV_PARAMS`; `preprocess_job_parameters` has already checked the
value as written. For a RANGE_EXPR Job Parameter in a template that does not list `ENV_PARAMS`,
`create_job` therefore enforces no length bounds, and a `range_expr` value that did not come from
`preprocess_job_parameters` is accepted whatever its length. Testing the canonical form against an EXPR-only template's lengths would break the round
trip: `1-3:2` passes `minLength: 4` in the first pass, and its canonical form `1,3` would fail it in
the second.

For `PATH` this holds because both passes check constraints against the same resolved value: the
first pass joins a relative default and checks the joined path, and the second pass receives that
absolute path as a submitted value, leaves it as written, and checks it again. Earlier, the first pass
skipped the check for defaults, so a relative default under a `maxLength` passed the first pass and
failed the second.

For lists it holds by construction, because **a job parameter stores a path as a string.** A path is
context-sensitive: the same value does not denote the same file on a Windows submitter and a Linux
worker, and job parameters are settled before there is a host to ask. OpenJD keeps paths POSIX until
template evaluation on the host -- every `with_path_format` call in this crate passes
`PathFormat::Posix`, and `openjd-sessions` passes `PathFormat::host()` -- and stores `PATH` and
`LIST[PATH]` parameter values as strings. `build_symbol_table` does not bind `Param.*` for those two
types at all, for the same reason.

So a `LIST[PATH]` value is an `ExprValue::ListString` at every length, and `ExprValue::ListPath`, which
carries a `PathFormat`, is not a shape a stored parameter value has. It is refused as input at any
length. An empty list is where that could drift, having no elements to infer a variant from:
`coerce_json_to_job_parameter_type` types it from a hint, and the hint must name `STRING`, the variant
the coerced elements produce, rather than the declared `PATH`. Passing `PATH` gave the empty case a
`ListPath` that `openjd-sessions::build_symbol_table` then dropped, leaving `Param.<name>` unbound at
session scope (issue #387). That was issue #389.

### build_symbol_table

```rust
pub fn build_symbol_table(
    params: &JobParameterValues,
) -> Result<SymbolTable, ModelError>
```

Builds a `SymbolTable` with `Param.*` and `RawParam.*` entries from processed parameter
values. Returns `Result` because symbol table insertion can fail.

PATH types are stored as `Unresolved(PATH)` since the source path format may differ
from the host path format — the value must be preserved exactly as a string until path
mapping is applied at session time. `RawParam.*` for PATH types is forced to STRING.

### create_job

```rust
pub fn create_job(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<job::Job, ModelError>

pub fn create_job_with_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError>
```

Full template instantiation pipeline. `create_job` is
`create_job_with_environment_templates` with no attached Environment Templates, returning only
the Job; both run every stage below, including [the submission
stage](#the-submission-stage), so the Job `create_job` returns is complete. Environment templates should
already be merged into `job_parameter_values` via `preprocess_job_parameters` before
calling this function. `ctx` carries the model profile and the `CallerLimits` that apply to
this job instance (it has no path policy: every
TEMPLATE-scope field evaluates with POSIX semantics, Expression Language §1.2.1). Its revision must
match the template's and its extensions must cover every extension the
template declares — enforced with a `Compatibility` error, since a
context enabling fewer extensions would make every downstream
evaluation ambiguous (template defect vs context artifact). Enabling
more extensions is allowed, as is layering caller policy on top (e.g.
stricter queue-level limits via `with_caller_limits`).

Every expression evaluation this stage performs — the job name, host
requirement values, parameter-space ranges, `let` bindings, and the
carried-forward checks below — runs under the caller's evaluation budgets
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`,
defaults: the Expression Language spec's 100 MB / 10 M), the same
budgets template validation and the session runtime apply.

1. Build symbol table from parameter values
2. Resolve template-scope fields:
   - Job name (evaluate FormatString). The resolved name is checked against §1.1.1:
     length vs `max_job_name_len`, non-empty, and no Cc control characters. Decode
     already rejects an empty literal name, control characters in the name's literal
     text (verbatim in every resolution), and a fully static name resolving to an
     empty or control-character-bearing value — what remains only knowable here is
     an emptiness or control character *introduced by an interpolated value*.
   - Step names
   - Host requirement names. Each amount and attribute `name` is `@fmtstring`
     (§3.3.1 / §3.3.2) and resolves with target type `string`
     (`resolve_capability_name`). The resolved name is checked against
     §3.3.1.1 / §3.3.2.1 — at most 100 characters, the capability name pattern,
     and the reserved scopes — through `helpers::check_capability_name`, the check
     decode applies to a literal name and pass 8 to a fully static one, so the
     wording matches. The resolved names are then checked for uniqueness within
     `amounts` and within `attributes`, case-insensitively
     (`check_resolved_names_unique`, §3.3), and the resolved name is what the
     standard-capability value checks below and the job use. Decode can only
     check a name it knows — a literal, or one that is fully static — so a name
     depending on a job parameter is first checked here.
   - Host requirement values (amounts min/max, attribute values). Resolved amount bounds
     must be finite `f64` values; non-numeric, NaN, and infinite results are rejected.
     White space around the number is ignored (Template Schemas §7.5), in a whole-field
     expression's `float?` coercion and in a multi-segment string alike.
     Resolved `attributes[].anyOf` / `.allOf` elements are re-checked against
     `<AttributeCapabilityValue>` (§3.3.2.2) via
     `capabilities::validate_attribute_capability_value`, because decode skips that check
     when the element is a format string. Errors are reported at
     `steps[i] -> hostRequirements -> attributes[j] -> anyOf[k]` so they read like the
     decode-time error for a literal value; for an `anyOf` written as one expression
     (ENV_PARAMS), which has no elements in the template, at `... -> anyOf` with a
     `list element k:` prefix, as template validation reports a static result. Only the first failing `anyOf`/`allOf` group on
     the first failing attribute is reported; decode accumulates every violation instead.
     The amount bound constraints (`min` non-negative, `max` positive, `min <= max`) are
     re-applied on the resolved value by `check_resolved_amount_bounds`, and the
     `chunks.defaultTaskCount` / `targetRuntimeSeconds` minimums are re-applied by
     `resolve_parameter_space` — see [validation.md](validation.md). Chunks fields
     resolve single whole-field expressions with the target type Expression Language
     §1.3.2 derives from the schema: `int` for the required `defaultTaskCount`
     (so `{{ 4.0 }}` coerces to 4) and `int?` for the optional
     `targetRuntimeSeconds` (a whole-field `null` means the field is omitted);
     multi-segment strings concatenate and parse with surrounding whitespace
     tolerated. Both forms ignore the same white space, since string-to-`int`
     coercion trims it too (see `specs/expr/values.md` § from_str_coerce).
   - Parameter space ranges (evaluate range expressions, resolve FormatString ranges).
     A format string element of a range list resolves through one helper
     (`resolve_list_item`) for every task parameter type, with the target from
     `TaskParameterType::range_list_item_target`. Template validation types the
     element with the same target (for STRING and PATH through
     `string_list_item_target`, which that function returns for them).

     STRING and PATH elements are always list items (Expression Language §1.3.2),
     with the target `string? | list[string]`. INT, CHUNK[INT] and FLOAT elements are
     list items when the template enables EXPR, with `int? | list[int]` and
     `float? | int | list[float]`. Whether EXPR is enabled is read from the template,
     as template validation reads it, not from the `create_job` context, which may
     enable extensions the template does not declare. A whole-field list-item expression drops the
     element when it is `null`, and flattens a list inline, one range element per
     member, mixable with literal elements. A range emptied by drops is rejected
     (`Task parameter '<name>' range has no elements after resolution`), since the
     ≥1-element rule was checked on the list as written, at decode. For the same
     reason the 1024-element limit is checked again on the resolved list, so a list
     that flattens past it fails (`Task parameter '<name>' range exceeds 1024
     elements (<n> elements)`). Resolution stops at the first element that takes the
     list past the limit, before a flattened element's members are copied, so `<n>`
     counts the elements resolved up to and including that one, not the whole list.
     The work for a single element is bounded by the evaluation budgets, and a
     `range_expr` that coerces to `list[int]` by the expression engine's cap of 10
     million members. A `range_expr` past that cap fails with the generic coercion
     error (`Cannot coerce range_expr to int | list[int] | nulltype`): coercion to a
     union target reports no member's own diagnostic (see `ExprValue::coerce`), so
     the cap's message is not shown. Attribute `anyOf`/`allOf` values get the same
     list-item treatment in `resolve_host_requirements`
     (`job/create_job/host_requirements.rs`), with their §3.3.2 maximum of 50 values
     applied the same way to the list form: resolution stops at the element that
     takes the list past 50 ("attribute 'X' of Step 'S' has more than 50 anyOf values
     after resolution, exceeding the maximum of 50 (Template Schemas §3.3.2)"). The
     expression form (ENV_PARAMS) resolves as one value and is checked after, with
     the count ("... has 51 anyOf values after resolution ..."). An empty list is
     "attribute 'X' of Step 'S' has no anyOf values after resolution. An empty list is
     not allowed (Template Schemas §3.3.2)".

     Without EXPR an INT, CHUNK[INT] or FLOAT element is the base 2023-09
     `<TaskParameterStringValue>` and resolves to a string. A base format string
     only interpolates value references, so it cannot produce `null` or a list, and
     the string form ignores white space around the number, as an `<intstring>` or
     `<floatstring>` does (Template Schemas §7.5). The same is true with EXPR of any
     format string that is not a single whole-field `{{...}}`: literal text around
     its expressions, two or more expressions (`{{ Param.A }}{{ Param.B }}`), or no
     expressions (a literal FLOAT string such as `"02.50"`). A string is interpreted
     for the element type. In these messages and the ones below, `<k>` is the
     element's index in the list as written. (The STRING and PATH per-element length
     and empty-value checks in `resolve_string_range` count the resolved list
     instead, after flattening.)
     - INT and CHUNK[INT]: trimmed, then parsed as an `i64` (Rust's integer syntax:
       an optional sign and ASCII digits). Otherwise job creation fails with `Task
       parameter '<name>' range[<k>]: value '<resolved>' is not an integer`, or `...
       is out of range for a 64-bit integer` for an integer that does not fit.
     - FLOAT: trimmed, and must parse as a finite `f64`, or job creation fails with
       `Task parameter '<name>' range[<k>]: value '<resolved>' is not a number` or
       `... is not finite`. The text is kept as the `<floatstring>` spelling (§7.5).

     For a whole-field list-item expression the target's coercion applies
     (Expression Language §1.2.3), as for the chunks fields above: `{{ 3.0 }}` is 3
     in an INT range, a STRING job parameter `"7"` or `" 7 "` is 7 (coercion ignores
     white space around the number, so the element gives the value it gives without
     EXPR), and a value that cannot coerce, such as a STRING `"abc"` or `"\u200b7"`
     (U+200B is not white space), or a `list[string]` with a
     non-numeric member (`["a"]`; `["1", "2"]` coerces member by member to `[1, 2]`),
     fails with the coercion error, for example `Task parameter '<name>' range[<k>]: Cannot
     coerce string to int | list[int] | nulltype`, followed by the expression and
     its caret. Evaluation errors in INT, CHUNK[INT] and FLOAT elements are reported
     the same way. CHUNK[INT] then applies its value bound to the resolved list.

     A FLOAT element's target accepts `int` so that an integer renders as written:
     an INT job parameter `10` gives the task parameter value `10`, as the
     `<floatstring>` `"10"` would, not `10.0`. A float keeps the spelling it carries,
     alone or as a list member, when that spelling differs from what `format_float`
     gives (an equal one renders the same, and is not stored): a FLOAT job parameter
     given as `2.50` renders `2.50`, and so does `{{ [Param.F] }}` as an element (the whole-field range
     expression `"{{ [Param.F] }}"` renders `2.5`, because `resolve_float_range`
     drops spellings there). A float with no spelling, such as a computed one or an
     integer list member converted to float, renders through `format_float`:
     `{{ Param.Ints }}` with `[3, 4]` gives `3.0` and `4.0`. A STRING value that is an integer coerces to `int`
     first (Expression Language §1.2.3 prefers `int` for a string), so it renders as
     that integer: `"+5"` renders `5`, where the same text as a `<floatstring>`, or
     as the element without EXPR, keeps its sign and renders `+5`. A decimal string
     keeps its spelling either way (`"+2.5"` renders `+2.5`).

     The §3.4.2 limit of 1024 characters after resolution is not applied to INT or
     FLOAT elements, only to STRING and PATH ones. An INT element only has to parse,
     and a FLOAT element longer than the limit keeps its value but not its text
     (see `float_range_value`). The reference implementation applies no limit to
     an element that parses as a number either. It does apply the limit to one that
     does not, which openjd-rs rejects at any length.

     **Divergence from the Python reference:** openjd-model-for-python 0.14 does
     not apply §1.3.2 to range list elements of any type, with or without EXPR. It
     resolves each element to a string, so a `null` becomes an empty element and a
     list becomes its display text (`["b", "c"]`) instead of being dropped or
     flattened, and there is no coercion: with EXPR, Python renders a STRING
     value `"+5"` in a FLOAT element as `+5` where openjd-rs renders `5`. It converts a
     resolved INT or CHUNK[INT] element with Python's `int()`, which accepts
     underscores (`1_10` is 110), non-ASCII decimal digits (`١٠` and fullwidth
     `７`), and integers of any size, all of which openjd-rs rejects. The same
     applies to a literal `<intstring>` element, which openjd-rs rejects at decode
     with `Cannot parse '1_10' as integer`. When `int()` fails, Python keeps the
     text, if at most 1024 characters, as the value of an INT or CHUNK[INT] task
     parameter (`abc`, `2.5`); iterating a CHUNK[INT] parameter space then raises an
     uncaught `ValueError`. §3.4.1.1 requires a base-10
     integer, so openjd-rs rejects a resolved value like these at job creation and
     a literal one at decode. Python reads a FLOAT element with `float()`, which
     likewise accepts underscores (`1_0.5`) and non-ASCII digits, as well as `inf`,
     `nan` and `1e999`, which Python keeps as task parameter values (with EXPR, the
     task then fails when it references the value). §3.4.1.2 requires a base-10
     floating point value, and openjd-rs requires a finite one (as Expression
     Language §1.3.4 does with EXPR), so it rejects such a FLOAT element, literal or
     resolved, at job creation (a literal `<floatstring>` is not parsed at decode). Python's template validation also does no static
     typing of these elements (see [validation.md](validation.md)).
   - **Empty and discarded host requirements (ENV_PARAMS, Template Schemas §3.3).** When
     the requirements' document lists ENV_PARAMS (for a Step's own, the Job Template;
     an Environment's always does), `resolve_host_requirements` discards a requirement
     that resolves to nothing instead of rejecting it: an amount whose `min` and `max`
     both resolve to `null`, and an attribute whose `anyOf` and `allOf` both resolve to
     `null`, which only the expression form can (`template::AttributeValues::Expression`,
     resolved with the target type `list[string]?`; a list result is checked as the list
     form is, 1 to 50 values each an `<AttributeCapabilityValue>`, so `[]` is an error, and
     at most one for the `allOf` of `attr.worker.os.family` or `attr.worker.cpu.arch`,
     Template Schemas §3.3.2.1).
     A list form whose items all resolve to `null` is left empty, which is an error, not a
     discard. An `anyOf` or `allOf` (either form) that fails to resolve on a Step's own
     requirements is a `FormatStringError` whose message starts with the field's path
     (`steps[i] -> hostRequirements -> attributes[j] -> anyOf: ...`); an Environment's is
     reported at that path. The capability names are resolved and checked first, so a discarded
     requirement's name must still be valid; the duplicate-name check then applies to the
     requirements left. Empty `amounts` and `attributes` lists become `None`, and a
     `hostRequirements` with nothing left becomes `None` on `job::Step`, which means no
     requirements (§3.3 item 4). Each discarded requirement is returned as a
     `DiscardedHostRequirement` in `CreatedJob::discarded_host_requirements`, the Step's own
     first. Without ENV_PARAMS nothing is discarded: an amount with no bound left is
     "must have at least one of min or max after resolution.", and the shape of the
     result is unchanged.
   - Step-level let bindings (template scope; a failure is reported at
     `steps[i] -> let[j]` — see the carried-forward checks below)
3. With EXPR extension: inject `Job.Name` and `Job.EnvNames` (before step instantiation)
   and `Step.Name` (per step) into the symbol table. `Job.EnvNames` (ENV_PARAMS, Template
   Schemas §7.3.1) is a `list[string]` of the attached Environments' names, in attachment
   order, then the `jobEnvironments` names, whatever their `activation`; a shared name repeats,
   and Step Environments are not listed. The attached Environments are known before anything
   else (§7.4.1), so it is concrete before any TEMPLATE-scope expression, including a Step's
   `let`. The order is the order a Session that entered all of them would enter them, since
   the combined Job lists the attached Environments first (Template Schemas §1.1 item 7, the
   Job's Environment order; prototype plan P2). It is bound
   with `EXPR` alone: template validation rejects a reference without ENV_PARAMS, and every
   `resolved_symtab` keeps only the symbols a field references, so a Job that does not use it
   is unchanged. It is not in the scope of the Job name or of an Environment parameter
   `default`, which resolve before it is bound or in their own table.
4. Carry forward session/task-scope fields as FormatString (plus action
   `timeout`/`notifyPeriodInSeconds`, which validate in template scope but
   resolve on the worker). A SimpleAction step (§8) is desugared here via
   `StepTemplate::resolve_syntax_sugar` — infallible, since the sugar's
   `script` is already a parsed `FormatString` — so the job's `Step` always
   carries a full `StepScript`.
5. Run the resolved-value checks on the carried-forward fields
   (next section) — job environments first, then each step as it is
   instantiated — accumulating every failure into one collection that
   is reported once, with the submission stage's errors (see [Error
   order](#error-order))
6. Convert environments from template to job types
7. Build step dependency list from the template's Step entries only: `dependsOn`, and its
   ENV_PARAMS synonym `step` (§3.2 item 2), each a `job::StepDependency` with `dependsOn`.
   An `environment:` entry (ENV_PARAMS) does not delay scheduling (§3.2
   item 3), so it is dropped, and a list left empty by the filter becomes
   `None`.
8. ENV_PARAMS, after the step's `let` bindings (§7.4.1): evaluate each `environment:` entry's
   `enableIf`, fix which Job Environments apply to the step by their
   `activation` and its entries, compute its effective Environment parameter values for them, unchecked, and
   keep them, with its enabled entries for external Environments, for the submission stage,
   which checks each Job Environment's constraint 7, decides its group and own `enableIf`
   after the attachments, and checks its values; see [Environment parameter
   values](#environment-parameter-values-env_params). Each Step Environment's own `enableIf`
   is decided here too, and a disabled one is left out of `job::Step::step_environments`
   (see [Environment enableIf](#environment-enableif-env_params)). Errors are filed by §7.4.1
   step (see [Error order](#error-order))
9. Attach resolved symbol table to each step
10. Run [the submission stage](#the-submission-stage) with the attached Environment Templates
    (possibly none)

#### Environment parameter values (ENV_PARAMS)

**`enableIf`** (§3.2 item 3.2). Each `environment:` entry's `enableIf` is decided after the
Step's `let` bindings and before the entry's name is resolved (§7.4.1). It is an expression
field of type `bool` (§7.6), resolved by the expression-field checker's `resolve` (see
[template-types.md](template-types.md#expression-fields-75)) in the Step's template scope
(below), under the template-scope profile and library pass 8 checked it with, the caller's
evaluation budgets, and POSIX paths: an `enableIf` is
evaluated in template scope, not in a host context (Template Schemas §7.6, the
paragraph after its rules; spec `5535230`), and reads the `let` bindings with the values job
creation computed for the Step, as the entry's values do: a boolean scalar is its
value; an expression must resolve to `true` or `false`. A `null` result ("resolved to null,
which does not fit type bool (Template Schemas §7.6)."), another type ("resolved to a value
of type T, which does not fit type bool …"), or an evaluation failure ("could not be
evaluated: <error>") is reported at `steps[i] -> dependencies[j] -> enableIf`, and that
entry's Environment gets no values for the Step, so the one mistake is reported once. A
scalar or `null` that template validation rejects is rejected here with its message too,
for a template built in code. A **disabled** entry requests nothing and supplies nothing,
and what it means depends on the `activation` of the Environment it names (§3.2 item 3.2):

- `ON_REQUEST`: the Step does not request it, so its Sessions do not enter it.
- `BY_DEFAULT`: the Step **declines** it. Its Sessions do not enter it, exactly as for an
  `ON_REQUEST` Environment the Step does not request: the Environment's own `enableIf` is not
  evaluated for the Step, and neither the value checks, nor constraint 7, nor its
  `hostRequirements` apply to the Step. A declined Environment is not listed in
  `CreatedJob::disabled_environments`, which lists the ones an Environment's own `enableIf`
  disabled or its `exclusiveGroup` superseded; `CreatedJob::environment_statuses` lists it as
  `Declined` (see [Environment statuses](#environment-statuses-env_params)).
- `ALWAYS`: an error at `steps[i] -> dependencies[j] -> enableIf`, "is false, so Step 'S'
  would decline Environment 'E', whose activation is ALWAYS: no Step can leave out an ALWAYS
  Environment (Template Schemas §3.2 item 3.2)." Template validation reports it for a literal
  `false` on an entry that names a Job Environment; job creation for an expression that
  resolves to `false`, and the submission stage for any disabled entry naming an attached
  `ALWAYS` Environment. It is a §7.4.1 step 4 error, so job creation takes no later step for
  the Step (see [Error order](#error-order)). `check_environment_templates`, which
  reports every violation it is certain of, still checks the Environment as entered, with
  every parameter at its `default`, since no Step can leave it out (§3.2.2 item 6): a
  parameter without a `default` is also reported missing there, with the note that the entry
  is disabled.

An attached Environment whose Environment Template does not list ENV_PARAMS behaves as
`ALWAYS` with no `enableIf`, but a disabled entry naming it has no effect and is not an error
(§4 item 4; see the submission stage, step 2). Job Environments are taken as written: a Job
Template that does not list ENV_PARAMS has no entries and no `enableIf`, so every Session
enters its Environments whichever `activation` they are read as.

A disabled entry's name is resolved, as an enabled one's is, to learn the `activation`: against
`jobEnvironments` here, and, for one that names none, against the attached Environments in the
submission stage, which job creation keeps it for, by name and position
(`StepSelection::disabled`). Constraint 3 applies to entries as written, enabled or not.

**Each Step's Environment set** (§4 item 4). Every Job Environment with `activation: ALWAYS`
applies to a Step, and so does every one with `BY_DEFAULT` (the default) that no disabled
entry of the Step declines, and every one with `activation: ON_REQUEST` that an enabled entry
of the Step requests (§3.2 constraint 7); with its own `stepEnvironments`, that is the set its
Sessions would enter. The Environments' own `enableIf`s then take out the ones disabled for
the Step, except an `ALWAYS` one, which cannot be disabled (see
[Environment enableIf](#environment-enableif-env_params)), and each `exclusiveGroup` takes out
all but the first of its Environments left (see
[Exclusive groups](#exclusive-groups-env_params)). Job creation proper records, for each
`jobEnvironments` entry, whether it applies to the Step by its `activation` and entries; the
submission stage takes out the ones an `enableIf` disables or a group supersedes, does the
same for the attached Environments, and stores the result on `job::Step::environments` (see
[job-types.md](job-types.md#step)).

Template Schemas §3.2.2: for each Step and each Environment that applies to it that declares
`envParameterDefinitions`, every parameter's **effective value** is the value the Step's enabled
`environment:` entry for that Environment supplies, else the parameter's resolved `default`.
So every Step gets values for every `ALWAYS` Job Environment that declares parameters, and every
`BY_DEFAULT` one it does not decline, whether or
not it names it (prototype plan A8), and for each `ON_REQUEST` one it requests; an
Environment that does not apply to the Step has no values for it, and its parameters without a
`default` need no value from the Step (constraint 7 over the Step's set, which does not depend
on any Environment's own `enableIf` or `exclusiveGroup`). An entry that
supplies keys to a Job Environment that declares none is rejected (constraint 5).
`create_job` stores them on `job::Step::environment_parameters`, one
`EnvironmentParameterValues { document: JobTemplate, environment, values }` per such Job
Environment in `jobEnvironments` order (see [job-types.md](job-types.md#step)).

The code is `job/create_job/env_params.rs`, shared with [the submission
stage](#the-submission-stage):

- **Literal values** (no `{{`) are checked with the template-validation function
  (`check_literal_value`: the Job Parameter `default` decoder of the type, list elements that
  are not format strings, the definition's constraints), so the messages match pass 11's, then
  converted.
- **Which extensions apply.** A document that holds a value or a `default` lists `ENV_PARAMS`,
  so it lists `EXPR` (Template Schemas §1.1 item 3, checked at template validation), and every
  value is read with `EXPR`, whatever `ctx` enables. The first half of plan 1's A12 (a value's
  forms follow the extensions of the document that holds it) has no case left to decide (plan
  6); its second half, the caller's URI policy, is gone: a URI is one more form of PATH value
  (§4.5 item 3 at spec `af53020`).
- **Format-string values** are TEMPLATE-scope fields resolved at Job creation (§7.3.2, and §7.4's
  Job creation row: "expressions in TEMPLATE scope are fully evaluated"), so they resolve with POSIX
  semantics on every host, as every other job-creation field does (Expression Language §1.2.1,
  "In TEMPLATE scope contexts, POSIX semantics are used for consistency"): `path(RawParam.Dir) /
  'sub'` joins with `/` and reads `\` as a filename character, under the caller's evaluation
  budgets. An entry value's scope, and an `enableIf`'s, is the Step's full template scope
  (`Param.*` other than PATH and LIST[PATH], `RawParam.*`, `Job.Name`, `Job.EnvNames`,
  `Step.Name`, and the Step's `let` bindings; §7.3.2). Job creation evaluates the Step's `let`
  bindings once, with POSIX semantics, and the Step's script, its entries' values and every
  `enableIf` read the same values; a binding that fails is the Step's ordinary `let` error at
  `steps[i] -> let[j]`, which leaves the Step uninstantiable (see [Error order](#error-order)).
  (Before spec `af53020` this prototype evaluated the bindings an entry value read a second
  time, in a caller-given submission path format.) A
  `default`'s scope is the merged Job Parameters alone, without `Job.Name` or `Job.EnvNames`
  (§4.5 item 1, §7.3.1; plan A21). A single `{{ }}` expression evaluates with the
  parameter's expression type as its target type, and the result is coerced to the
  Job Parameter type (a `path` result becomes its string). INT, FLOAT, BOOL, RANGE_EXPR and the
  list types convert as the target type says. For PATH and LIST[PATH] a `string` (or
  `list[string]`) result becomes a POSIX `path`, which keeps its text, so the value is the
  string as written: `{{RawParam.Dir}}` with `C:\x` is `C:\x`, the same value the literal
  `C:\x` and the multi-segment `{{RawParam.Dir}}{{ '' }}` give. Any other format string resolves to
  a string and is converted with `coerce_from_str`, as a submitted Job Parameter value would
  be, with no directory join. A list-typed value that is not a single expression is an error
  (template validation already rejects it for a Job Environment).
- **PATH values**, and each LIST[PATH] element, are used as given (§4.5 item 3 at spec
  `af53020`): never joined with a directory, and not checked against any form. A relative path,
  a drive-relative `C:dir`, a drive-less rooted `\dir`, an empty string and a URI are values
  like any other, and a relative literal `default` is no error. The value is stored as resolved
  (plan A22): a literal or a multi-segment format string as written; a single `{{ }}`
  expression as the string of its result, a `string` result unchanged and a `path` result as
  POSIX path operations made it. Earlier prototypes required the value to be absolute in the
  submission path format (plan A16 and decision 7), then, at spec `2892d96`, in some path
  format, or a URI the caller allowed.
- The definition's **constraints** are checked against that stored value (constraint 6,
  §4.5 item 2), using `EnvironmentParameterDefinition::check_value_constraints`, which applies
  `JobParameterDefinition::check_constraints` with `RangeExprLengthCheck::AsWrittenAndCanonical`
  and then the 1024-character limit of §4.5 item 5 on a STRING or PATH value and on each element
  of a LIST[STRING] or LIST[PATH] one
  (for a literal, by the template-validation check), so every effective value of an
  Environment that is not disabled for its Step satisfies its definition. A RANGE_EXPR's
  `minLength` and `maxLength` check both the value as written and its canonical form, whatever
  extensions the Job Parameters' documents list (§4.5 item 4, §2.10):
  a literal's string, a format string's resolved string, or a single `{{ }}` expression's
  string result, which is read by evaluating the expression again with no target type. A
  single expression whose result is a `range_expr` has only its canonical form, which must be
  a valid `<IntRangeExpr>` (the empty `range_expr` of `{{ Param.R[1:] }}` with `R` `5` is an
  error at the value).
- A **RANGE_EXPR** value is stored as its `range_expr`, whose transport form is its canonical
  form, so two values that denote the same integers are equal (§3.2.2): `1-3` and `1,2,3` have
  one transport form, and `Step::environment_values_equal`, `openjd run`'s choice of Environments to re-enter, and the shared
  resolutions of `enableIf` and `hostRequirements` see them as one. A Session parses it as it
  parses a RANGE_EXPR Job Parameter's value.

**Disabled Environments** (§3.2.2, §7.4.1). For each Step and each Environment that applies
to it, `DeclaringEnvironment::values_for_step` computes the effective values without
reporting the checks that depend on the Environment's own `enableIf` (`PendingValues`):

- reported at once, whatever the `enableIf`: constraint 5 (a key the Environment does not
  declare);
- `PendingValues::missing`: the parameters without a `default` the Step does not supply
  (constraint 7), which the caller reports under §7.4.1 step 5 for every Environment that
  applies to the Step, superseded or disabled included, before any Environment's `enableIf` is
  evaluated (§3.2 constraint 7);
- kept: each value's conversion and checks (type, constraints, the
  single-expression rule for a list type) and a failed resolved `default`;
- the value each definition has for the `enableIf`: the effective value when it converts to
  the parameter's type, whether or not it meets the other constraints, and otherwise
  (missing, failing to resolve, or not converting) none.

`DeclaringEnvironment::decide` then decides the Environment's `enableIf` for the Step
(`EnableIf`), with concrete values only (§3.2.2 item 2): absent or `true` is enabled; the
scalar `false` is disabled; an expression that *reads* (references, from its
`accessed_symbols`, whether or not evaluation reaches the reference) an `Env.Param.<name>` or
`Env.RawParam.<name>` whose value is none is not evaluated (`EnableIf::NotEvaluated`): the
Environment is not disabled, and the value's error (constraint 7, or the value's own) is
reported. Any other expression is evaluated (`expr_field::resolve`) in its scope with each
`Env.RawParam.<name>` it reads (a path as its unmapped string) and each non-PATH
`Env.Param.<name>` it reads bound to its value; a value it does not read is not bound and
need not convert. Steps with the same values of the parameters it reads share one evaluation
(keyed by those values; a RANGE_EXPR value is keyed by its canonical form, so `1-3` and
`1,2,3`, which are equal values, share one evaluation). For an Environment that is not
disabled (`true` or not evaluated), `DeclaringEnvironment::commit` reports what was kept and
returns the values; for a disabled one it is dropped, and the Environment is left out of the
Step's set with no values. An evaluation that fails (or resolves to `null`,
or to a value that does not fit `bool`) is reported once, under §7.4.1 step 6 and the first
Step it names, at `<environment> -> enableIf` with "Resolved for Step 'A'." (or "Steps 'A'
and 'B'") appended (`report_enable_if_errors`). Job creation fails either way, and which other errors
to report is an implementation choice (plan 4 A40, revised): openjd-rs checks the Environment for those
Steps as if it is not disabled, so a Step's other mistakes for it (a value that breaks a
constraint, a `default` that does not convert, a second wrapping layer) are reported in the
same run instead of one edit at a time. A missing parameter is reported whatever the
`enableIf` (constraint 7).

**When a `default` is checked** (§4.5 item 2, plan A19). A literal `default` is checked
when `create_job` sets up the Job Environments, before any Step, so one that fails a
path-format check (a relative PATH) is an error even when every Step supplies the parameter.
A non-literal `default` is resolved the first time a Step takes it and cached, since its value
does not depend on the Step; a failure is reported once, at
`jobEnvironments[k] -> envParameterDefinitions[m] -> default`, prefixed
`for Step '<name>', which does not supply this parameter:`, naming the first Step that takes
it and for which the Environment is not disabled; a Step for which the Environment is
disabled does not report it (§3.2.2). A non-literal `default` that no Step takes is never
evaluated. A literal `default` is checked whatever any `enableIf` decides, since the check
is of the definition, not of a Step's value (§4.5 item 2; plan 4 A42).

`create_job` also rejects a second `environment:` entry of one Step for the same Environment
(§3.2 constraint 3), for a template edited after decoding, so neither entry's values are
silently dropped.

A Step whose step-level `let` binding failed is not instantiated (its host requirements and
parameter space need the `let` values), so its Environment parameter values are not computed
or checked either; job creation already fails on the `let` error, and the values are checked
once it is fixed.

A supplied value that fails is reported at `steps[i] -> dependencies[j] -> parameters -> <name>`.
A parameter with neither a supplied value nor a `default`, of an Environment in the Step's set,
is reported once per Step at `steps[i]` (constraint 7), whatever the Environment's own
`enableIf` or `exclusiveGroup`: "must supply, in an 'environment:' dependency, every parameter
without a default of each Environment that applies to it (<why>), whatever that Environment's
own enableIf is; missing: ...". Template validation already rejects that for a decoded
template where the Step's entries for the Job Environments are known (§7.6 rule 5). (Template validation and
`check_environment_templates` note an entry its `enableIf` disabled; job creation never
needs to, since such an entry for an `ALWAYS` Environment is a step 4 error that stops the
Step, and one for any other declines or does not request it. `StepSelection::disabled`
carries the disabled entries to the submission stage, which resolves them against the
attachments.)
These errors join the carried-forward check failures in the one `ModelValidation` error
`create_job` reports, in §7.4.1 order (see [Error order](#error-order)): the
literal `default`s with the carried-forward checks, after the Job Environments' ones,
constraint 7 under step 5, and the values under step 7, by Step and then the Environment's
place in the Job's Environment order.

**External entries.** An enabled `environment:` entry that names no `jobEnvironments` entry
refers to an external Environment (§3.2 item 3), whose definitions job creation proper does
not see. It is kept for the submission stage as a crate-private `FrozenEntry`: the name, the
entry's index in `dependencies`, `required` with its default applied, the values as written,
and `symtab`, the symbols of the Step's full template scope that the values' format strings
reference (with the `let` bindings as the entry values see them, above), copied with
`FormatString::copy_used_symtab_values`. Its `path` values are POSIX, as every value in it is
computed with POSIX semantics.
`Step::resolved_symtab` cannot serve here, because it holds only what the Step's host-side
format strings reference. No returned Job holds frozen entries.

#### Resolved-value checks on carried-forward fields

Of the spec's three processing stages (Template Schemas §7.4: template
validation, job creation, task execution on the worker host), job
creation is the first at which job parameters have real values. The
session/task-scope format strings it carries forward unresolved —
action `command`/`args`, environment `variables` values, embedded-file
`data` — and the template-scope action fields that also resolve on the
worker — `timeout`, cancelation `notifyPeriodInSeconds` and deferred
`mode` — are statically evaluated against a **check symbol table**,
and exactly the resolved-value checks pass 8 applies to those fields
re-run on the result:

| Field | Constraint |
|---|---|
| environment `variables` values | resolved-length bound vs 2048 (§4.4.2, spec-mandated, always on) |
| action `command`, each `args[*]` entry | bound vs `CallerLimits::max_resolved_arg_len`, if set |
| embedded file `data` | bound vs `CallerLimits::max_resolved_data_len`, if set |
| action `timeout` (FB1) | coerced integer > 0; `null` = unset (spec-mandated) |
| cancelation `notifyPeriodInSeconds` (FB1) | coerced integer > 0, ≤ 600; `null` = unset (spec-mandated) |
| deferred cancelation `mode` (FB1) | `TERMINATE` / `NOTIFY_THEN_TERMINATE`; `null` = unset (spec-mandated) |

The last three rows run through the same helper pass 8 uses
(`validate_action_timing_fs`, with `TIMEOUT_CONSTRAINT`,
`NOTIFY_PERIOD_CONSTRAINT`, and `ResolvedConstraint::CancelationMode`),
so `timeout: "{{ Param.T }}"` submitted with `T = 0` fails here with
pass 8's `timeout must be > 0.` instead of on the worker. They evaluate
against the **template-scope** table pass 8 uses for them, with real
values: concrete non-PATH `Param.*`, `RawParam.*`, `Job.Name`, and — for
a step script or step environment — `Step.Name` and the step-level
`let` bindings. On the RFC 0008 wrap hooks the `WrappedAction.*` scope
is seeded unresolved on top, exactly as at pass 8, so round-trip
forwarding (`timeout: "{{ WrappedAction.Timeout }}"`) passes. Every
action is covered: step `onRun`, and environment `onEnter`/`onExit`
plus the three wrap hooks.

At template validation every `Param.*` is unresolved and contributes 0
to the lower bound; here the parameters are bound to real values, so a
violation that depends only on parameter values —
`args: ["{{ 'A' * Param.N }}"]` submitted with a huge `N` — becomes
decidable and fails at submission instead of on every worker (task
execution remains the enforcement boundary). The
walk covers step scripts (`onRun` + embedded files), step environments,
and job environments — including the RFC 0008 wrap hooks, with their
`WrappedAction.*` scopes seeded unresolved, exactly as in pass 8.

The check symbol tables mirror what the session runtime binds at run
time, with everything only a session can know left `Unresolved`:

- **Task scope** (step scripts): concrete `Param.*` / `RawParam.*` /
  `Job.Name` / `Step.Name` / step-level `let` bindings; `Unresolved`
  `Session.*`, PATH `Param.*`, `Task.Param.*` / `Task.RawParam.*`,
  `Task.File.*`. Script-level `let` bindings are evaluated into the
  table (this subsumes the type check job creation has always run on
  them: a binding that fails with the real parameter values fails
  here, deterministically, rather than in every session).
- **Session scope** (job and step environments): as above minus
  `Task.*`, plus this environment's `Env.File.*` and (ENV_PARAMS) its
  `Env.Param.*` / `Env.RawParam.*` (all `Unresolved`, since a Session
  binds them when it enters the environment), and its script-level `let`
  bindings evaluated in.
  An environment `let` binding that fails to evaluate fails job
  creation, under the same error policy as every other evaluation this
  stage performs (see below): the bindings only evaluate when the
  context profile enables EXPR, their expressions already type-checked
  at pass 8 with everything unresolved, so a failure here comes from
  the real parameter values and would deterministically recur in every
  session that enters the environment.

**`let` bindings.** Every `let` block job creation evaluates —
step-level (template scope, `HostContext::None`), step-script and
environment script-level (`HostContext::Unresolved`) — goes through
`check_carried_forward_let_bindings`, which calls the same per-binding
evaluation helper pass 8 uses (`evaluate_let_binding`). Each binding is
**parsed under the context's profile** — the same profile pass 8
parsed it with, never the latest profile, so syntax the profile does
not enable is refused here exactly as at template validation (and a
crate upgrade cannot make job creation accept what pass 8 refused, or
vice versa) — then evaluated under `PathFormat::Posix` with the
caller's budgets. A failure is a check failure like any other: reported
at the binding's path — `steps[i] -> let[j]`,
`steps[i] -> script -> let[j]`,
`steps[i] -> stepEnvironments[k] -> script -> let[j]`,
`jobEnvironments[k] -> script -> let[j]`, or for a SimpleAction
`steps[i] -> <kind> -> let[j]` — with pass 8's message
`Invalid expression in let binding '<name>': <error>` and the caret
aligned to the full `name = expr` binding string, as pass 8 and the
run-time path align it. **Continuation:** as at pass 8, the failed name
is bound `Unresolved(ANY)` and the scope's remaining bindings and
checks still run — a later binding or format string referencing it
evaluates to `Unresolved` instead of cascading `Undefined variable`
errors, and the scope's unrelated violations are still reported. A
failed **step-level** binding additionally makes the step
uninstantiable (host requirements and the parameter space are resolved
for real and need its value), so those two are skipped for that step —
its carried-forward checks, and every later step's, still run, and job
creation fails with the aggregate. Structural checks (names,
duplicates, shadowing, self-reference, count) are template
validation's job and are not repeated; structurally malformed bindings
— no `=`, empty name, empty expression — are skipped rather than
reported: pass 8 rejects all three at decode, so they cannot reach a
template that came through `decode_job_template`, and a hand-built
`JobTemplate` that bypassed decode still fails on them at run time.
The public `evaluate_let_bindings` (below) is the run-time entry point
used by `openjd-sessions` and `openjd-for-js`; the check symtabs do not
use it.

Failures are `ModelError::ModelValidation` at the same field paths
pass 8 uses, e.g.
`steps[0] -> script -> actions -> onRun -> args[0]:` /
`resolves to at least 100000 characters, exceeding the maximum of 1024.`
For a SimpleAction step (§8) the checks run on the desugared
`StepScript` — the same form pass 8 validated — and the paths are
re-rooted onto the field the author wrote through the same
`SimpleActionKind::remap_desugared_path` table pass 8 uses
(`steps[0] -> bash -> script`, `steps[0] -> cmd -> args[1]`,
`steps[0] -> bash -> timeout`, `steps[0] -> bash -> let[0]`, …; see
`specs/model/validation.md` § Reporting on desugared forms). No
diagnostic from this stage names a synthesized node.

**Aggregation.** Like pass 8, job creation reports every check failure
in the template at once: one `ValidationErrors` collection is threaded
through the job environments and every step (`instantiate_step` takes
it as `&mut`) and converted to a single `ModelError::ModelValidation`
once, together with the submission stage's errors and before them, as the
`Validation` key of [Error order](#error-order) says; the caller-limit
task-count and size checks come after them. The order is pass 8's order — deterministic
except within one environment's `variables`, which is a `HashMap` and
iterates in arbitrary order (shared with pass 8): `jobEnvironments` in template order, then each step in template
order — its step-level `let`, its script (`let`, `onRun`
`command`/`args`, `timeout`/`cancelation`, embedded-file `data`), then
its `stepEnvironments` in order (`let`, `variables`, every action's
`command`/`args`, every action's `timeout`/`cancelation`, embedded-file
`data`). **Non-check failures are reported alone.** The job name and
parameter constraints are checked before any check failure is
collected, and the caller task-count / size caps are reported only when
no error at all was found, after the submission stage, so a cap is never
reported together with a check failure. Resolving a step's host
requirements or parameter space for real (a value that cannot coerce,
a duplicate resolved capability name, an association length mismatch,
…) aborts immediately, discarding any check failures collected from
earlier scopes. These leave the job unconstructible
rather than describing a defect in a carried-forward field, and they
are reported with their own (pre-existing) error kinds and formats.

**Task-count limit.** With `CallerLimits::max_task_count` set, each
step's task count is `StepParameterSpaceIterator::len` with a chunk
override of 1 (so a `CHUNK[INT]` parameter counts values, not chunks),
and a step without a parameter space counts 1. Each count is an exact
`u64` on every target, including wasm32, where a `usize` could not
count `"1-5000000000"`. The counts are summed in `u128`, so the total
cannot overflow, and a total above the limit fails with
`Total task count (<total>) exceeds caller limit of <limit>.`

**Error policy.** Evaluation/parse errors are reported exactly as in
pass 8. `create_job` requires a context whose revision matches the
template's and whose extensions cover everything the template declares
(a `Compatibility` error otherwise — see `create_job`'s docs), so an
evaluation error at this stage cannot be a context artifact: it is
either a template defect pass 8 missed or a deterministic
value-dependent failure that every session resolving the field would
hit. Both are worth failing at submission. That disjunction being
exhaustive additionally depends on the evaluator propagating
`Unresolved` without error through every operator — this stage
evaluates under a symbol state no other stage sees (`Param.*` concrete,
`Task.*`/`Session.*` unresolved), so an operator that hard-errors on a
partially-resolved input turns a valid template into a submission
failure (as `eval_listcomp`'s concrete-iterable filter path once did —
see ListComp in `specs/expr/evaluator.md`). Budget exceedances
(`MemoryLimitExceeded` / `OperationLimitExceeded`) are reported like
any other error; the evaluator guarantees they propagate out of every
construct that absorbs errors under an unresolved operand — an
unresolved-test conditional whose other branch succeeds, and `and`/`or`
operands past an unresolved one that may pass evaluation on (a value error
there is absorbed — run time may select the other branch or short-circuit —
but the budget was spent in this evaluation regardless; operands after an
unresolved one that always decides, such as `Task.Param.I or …`, are not
evaluated, as at run time; see IfExp and BoolOp in
`specs/expr/evaluator.md`). One coarseness caveat:
for an unresolved-test conditional the evaluator charges both branches
against the budget, while a run-time evaluation with the test resolved
charges one, so a budget within a branch-cost of the limit can fail
here and pass there. Callers lowering the budgets accept that
granularity. Resolved-value constraint violations (the table above)
are always reported.

Cost note: job creation previously did not evaluate these fields, so the pass
adds work proportional to what a single worker would do anyway — done
once at submission instead of per-task-per-worker.

### The submission stage

The paths below are written in the form they take when Environment Templates are attached
(`JobTemplate -> steps[i]`); without them they have no document prefix (`steps[i]`), as
[Error order](#error-order) says.

The last stage of `create_job_with_environment_templates` (Template Schemas §3.2.1–§3.2.2, §4
item 4), the crate-private `apply_environment_templates` in `job/create_job/external.rs`. A
submission combines one Job Template with zero or more Environment Templates that the
scheduler attaches, in its order. It takes the Job that job creation proper built from the
Job Template, what job creation decided for each Step (its frozen and disabled entries, and
the `jobEnvironments` entries that apply to it by their `activation`, with its values
for them, undecided), the errors job creation found, the `Job.EnvNames` value, and the merged
values
`preprocess_job_parameters` produced with the same Environment Templates. `ctx` supplies the
caller limits, and its profile is the Job Template's (job creation checked that), which the
Job Environments' host requirements read. Each attached template is
evaluated under its own profile (`EnvironmentTemplate::profile()`), since an extension applies
to the document that lists it (§1.2 item 3): an attachment that lists `ENV_PARAMS` lists `EXPR`
too, so its `default`s may use `EXPR` when the Job Template lists neither, and a Job Template
that has entries lists both.

1. Each attachment's literal `default`s are checked, as job creation checks the Job
   Template's. An attachment named like a Job Environment gets a warning (§3.2.1 item 4) when
   the Job Template lists `ENV_PARAMS` (without it the Job Template has no entries, and the
   submission gets no warning it did not get before the extension, RFC Backward
   compatibility); if its `activation`
   is `ALWAYS` or `BY_DEFAULT` and it has parameters without a `default`, it applies to every
   Step, since no entry can decline it, whatever its own `enableIf` or `exclusiveGroup` (§3.2
   constraint 7), and no entry can supply them, so that is one error at `<attachment> ->
   environment -> envParameterDefinitions`, and those parameters are not reported again per
   Step under constraint 7. The error is reported when the attachment applies to some Step
   that is not stopped after §7.4.1 step 4 (`report_unreachable`), filed under step 5 after
   the Steps' step 5 errors (see [Error order](#error-order)); for a `BY_DEFAULT` one with an `enableIf` or an
   `exclusiveGroup` its message says "Its activation is BY_DEFAULT, so it applies to every
   Step whatever its enableIf and exclusiveGroup (§3.2 constraint 7)". One with `activation:
   ON_REQUEST` never applies, since no entry can request it, so it is not an error. (Warnings are returned only with a successful result.)
2. Each Step's frozen entries are matched to the attachments by `environment.name`:
   - exactly one: the entry refers to it, whatever `required` says (§3.2.1 item 1). When the
     attachment declares no parameters, such as one written before ENV_PARAMS, and the entry
     has `required: false` and supplies parameters, its parameters are ignored
     (`Attachments::ignores_parameters`): the entry still refers to the attachment, so it
     still requests an `ON_REQUEST` one, but it supplies no values, and its values are not
     resolved or checked (constraints 5 and 6). A warning is recorded at
     `JobTemplate -> steps[i] -> dependencies[j] -> parameters`, naming the Environment and the
     ignored keys (capped as other name lists are). The Environment's status stays `Entered`;
     `CreatedJob` does not otherwise record that the values were ignored;
   - none: with `required` true or absent, an error at
     `JobTemplate -> steps[i] -> dependencies[j] -> environment` naming the Environment and the
     attached documents; with `required: false`, the entry is ignored and a warning is recorded
     (§3.2.1 item 2; RFC "External Environments are named implicitly" item 3);
   - two or more: an error naming the attachments (§3.2.1 item 3). Two same-named attachments
     that no entry names are not an error (plan A11), unless their `activation` is `ALWAYS` or
     `BY_DEFAULT` and they have parameters without a `default`: no entry can reach them, so
     each such attachment
     gets one error at its `envParameterDefinitions`, as in step 1, whatever its `enableIf` or
     group, when it applies to some Step that is not stopped, and those parameters are not
     reported per Step.

   A disabled entry (`StepSelection::disabled`) is matched too, before the Step's frozen
   entries, to learn the `activation` that decides what it means (§3.2.1, §3.2 item 3.2). It
   supplies nothing, so constraints 5 and 6 are not checked for it, and one that matches no
   attachment has no effect and no warning, whatever `required` says (item 2 does not apply).
   Two or more matches are the item 3 error, as for an enabled entry; the spec's §3.2.1 lists
   item 3 among the rules a disabled entry follows. One match with `activation: ON_REQUEST`
   has no effect; with `BY_DEFAULT` the Step declines the attachment (step 3); with `ALWAYS`
   it is the error at `JobTemplate -> steps[i] -> dependencies[j] -> enableIf` that job
   creation reports for a Job Environment (see [Environment parameter
   values](#environment-parameter-values-env_params)), a §7.4.1 step 4 error. One match whose
   Environment Template does not list ENV_PARAMS has no effect and is not an error: such an
   attachment behaves as `activation: ALWAYS` with no `enableIf` (§4 item 4,
   `DeclaringEnvironment::pre_extension`), so every Step's Sessions enter it and its status is
   `Entered`, whatever the entry's `required` and `enableIf` say.
   An attachment named like a Step's `stepEnvironments` entry is unreachable from that
   Step only because template validation rejects an entry naming one (constraint 2); other
   Steps reach it (§3.2.1 item 5). If its `activation` is `ALWAYS` or `BY_DEFAULT` and it has parameters without a
   `default`, that Step gets an error saying it cannot reach the attachment, instead of the
   constraint 7 "must supply" message.
3. A Step with an error from §7.4.1 steps 1–4 (here, or in job creation) is stopped: none of
   the rest of this step, or steps 6–8 below, is done for it, and it counts as entering no
   Environment (see [Error order](#error-order)). For every other Step:
   every attachment with `activation: ALWAYS` or from an Environment
   Template that does not list ENV_PARAMS applies to it, and so does every one with
   `BY_DEFAULT` that a disabled entry of the Step does not decline (a declined one does not
   apply: its own `enableIf` is not evaluated for the Step, and no check applies), and
   each one with `activation: ON_REQUEST` that one of its enabled entries refers to (§3.2.1,
   after item 5). A
   Job Template without ENV_PARAMS has no entries, so its Steps never request an
   `ON_REQUEST` attachment (§1.2 item 3). Every Step gets effective values for every
   attachment that applies to it and declares parameters, from its entry if one refers to it
   and from the `default`s otherwise, with the same conversion and checks as for a Job
   Environment. Constraint 7 is checked for every attachment and Job Environment that applies
   to the Step, under §7.4.1 step 5, whatever its own `enableIf` or `exclusiveGroup`. Then the
   attachments and the Job Environments are decided in the Job's Environment order, attached
   ones first: each one's group (see [Exclusive groups](#exclusive-groups-env_params)) and own
   `enableIf`, with the Step's concrete values (see [Disabled
   Environments](#environment-parameter-values-env_params)); a disabled one is not entered and
   its values are not checked.
   An `ALWAYS` attachment whose `enableIf` is `false` for the Step is an error instead (see
   [Environment enableIf](#environment-enableif-env_params)).
   (A Step whose entry names an ambiguous attachment is stopped, so the one mistake is not
   also reported as "must supply" or "does not supply" errors.) The entry's format strings resolve against its frozen `symtab`. A key the
   attachment does not declare is an error (constraint 5), including any key of a
   `required: true` entry for an attachment that declares no parameters, such as one written
   before ENV_PARAMS (a `required: false` entry's are ignored, step 2); and a parameter
   without a `default` of an attachment that applies to the Step that the
   Step does not supply is an error at `JobTemplate -> steps[i]` (constraint 7 over the
   combined Job, §3.2.1 last paragraph). The lists in these messages are capped as template
   validation caps them (at most 50 names and 512 characters, the first name always listed,
   then "and N more"), with the shared functions `capped_name_list` and
   `missing_parameters_message`, and only an entry's first undeclared key lists the declared
   names. The lists of attachments in the "no attached Environment Template defines it" and
   "ambiguous" messages are capped the same way, since labels are caller-supplied. So a
   submission's error output is linear in the number of Steps and entries whatever the name
   and label lengths.
4. The attachments' Environments are converted with `convert_environment_with_symtab`, with the
   merged Job Parameters, plus `Job.Name` and `Job.EnvNames` when the attachment lists `EXPR`,
   as the frozen table.
5. The Job is combined: `job_environments` becomes the attachments followed by the Job's own
   (a Session enters the attachments first), and each Step's `environment_parameters` becomes
   its values for the attachments followed by its own.
6. Each Step's `environments` lists the combined Job Environments its Sessions enter after
   §7.4.1 step 6: each Environment's own `enableIf` and its `exclusiveGroup`, all decided in
   step 3 (see [Environment enableIf](#environment-enableif-env_params) and
   [Exclusive groups](#exclusive-groups-env_params)), or is `None` when it enters every one.
7. The `WRAP_ACTIONS` single-layer rule (RFC 0008) is checked over each Step's final
   Environment set: the combined Job Environments it enters and its Step Environments. More
   than one Environment defining wrap hooks is an error at `JobTemplate -> steps[i]` naming
   them, the attached ones with their document. Template validation checks the Job Template's
   own layers that are certainly entered, by entries and Environments whose `enableIf` is
   known there (§7.6 rule 5; see [validation.md](validation.md)); this check also covers an
   `enableIf` that is not known there, on an entry or an Environment, and the attached
   Environments (prototype plan R8).
8. Each Step gets its combined host requirements (see
   [Combined host requirements](#combined-host-requirements-env_params)), the last stage of
   §7.4.

Every error, with the ones job creation proper found, is reported in one `ModelValidation`
error in §7.4.1 order (see [Error order](#error-order)). The submission stage runs
whenever job creation proper built every Step, with or without `ENV_PARAMS`: an error there
does not stop it. The stage
does not re-run the carried-forward resolved-value checks on the attachments' own format
strings (the `SERVICE` prototype's submission function does).

#### Error order

Job creation takes the steps of Template Schemas §7.4.1 over the Job Template and the
attached Environment Templates together, in one pass, for every Job Template, whether or not
it lists `ENV_PARAMS`. Earlier versions made two passes, and an error of the Job Template
alone stopped job creation before the attachments were applied, so errors were reported out
of §7.4.1 order. A Job Template without `ENV_PARAMS` kept the two passes until now (see
[Without ENV_PARAMS](#without-env_params)). Every error is filed in a crate-private
`StagedErrors` (`job/create_job/staged.rs`) under a key: the §7.4.1 step, then the Step's
index, then a position within the Step. `finish_errors` (`job/create_job/mod.rs`) reports
them in key order, and in the order they were filed within a key:

| Key | Errors | Position |
|-----|--------|----------|
| `Validation` | the template-validation-style checks job creation makes: the Job Environments' carried-forward resolved-value checks, then their literal `default`s, then each Step's carried-forward checks and their `let` bindings, then the attachments' literal `default`s | the order they are made |
| `Step2` | an entry's `enableIf` that fails; a duplicate entry | the entry's index |
| `Step3` | an entry that names no Environment while required, or names several | the entry's index |
| `Step4` | a disabled entry for an `ALWAYS` Environment | the entry's index |
| `Step5` | an entry's undeclared key (constraint 5); constraint 7, for every Environment that applies to the Step, superseded or disabled included (by the first Environment the message names); an unreachable attachment's parameters | the Environment's place in the Job's Environment order (from step 5 on, an error is about an Environment, so it is ordered by the Environment, not by the entry that names it); an unreachable attachment's error, about no Step, by the attachment |
| `Step6` | an Environment's own `enableIf` that fails, or an `ALWAYS` one's that is `false`; a Step Environment's `enableIf` that fails | as for step 5, Step Environments after every Job Environment |
| `Step7` | the value checks (constraint 6); the single-layer rule | as for step 5; the single-layer rule after the Step's Environments |
| `Step8` | an Environment's resolved `hostRequirements`; a combination's conflict | as for step 5; the conflict after the Step's Environments |

An error that names several Steps (a failed `enableIf`, resolved once per distinct set of
values; an Environment's resolved `hostRequirements`) is filed under the first of them. One
about no single Step (an unreachable attachment) is filed after every Step's errors of its
step.

A Step with an error from steps 1–4 has no fixed set of Environments, so steps 5–8 are not
taken for it: the submission stage skips its values, groups and `enableIf`s, counts it as
entering no Environment for the single-layer rule, the unreachable-attachment error and host
requirements, and `finish_errors` drops anything job creation proper filed for it under steps
5–8 (an undeclared key, a Step Environment's `enableIf`). The other Steps are checked in full.
So a Step's value and constraint 7 errors appear only once its step 1–4 errors are fixed.
Resolving a Step's own host requirements or parameter space still aborts job creation at
once. A step-level `let` binding that fails leaves its Step uninstantiable: the other Steps
get only the checks job creation proper makes (template-validation style errors, step 2,
steps 4 and 5 for their Job Environment entries, and step 6 for their Step Environments),
the submission stage does not run, so no entry for an attachment is resolved (step 3) and no
check of steps 6–8 on the Job or attached Environments is made, and the errors found so far
are reported in the same order and shape. The caller limits (task count, script and Environment
sizes, measured on the Job Template's own Environments and Steps) abort job creation too, but
only when no other error was found: they are checked after the submission stage, so a Job
Environment's error, now found there, or an unresolved entry's, is reported in place of a
limit. These aborts keep their own forms (a limit's error is for `Template`, a parameter
space's is not a `ModelValidation`), which the shape rule below does not cover.

The error is shaped by the inputs, with or without `ENV_PARAMS`. Without attached Environment Templates it is for
`JobTemplate`, and its paths are relative to the Job Template, as every other job-creation
error is. With them it is for `Submission`, and every path starts with its document:
`JobTemplate`, `EnvironmentTemplate[i]`, or the attachment's label. One `ModelValidation`
error is used either way: the errors of the two documents are interleaved in step order, so
they cannot be split into a Job Template error and a Submission error that each read in
order, and the shape then depends only on the inputs, never on which of these errors occurred.
The warnings (`CreatedJob::warnings`) have the same paths.

For example, take the Job Environment `Lic`, whose `enableIf` is `Param.UseLic`, with `Seats`
without a `default`, and a Step `S` with the entry `{environment: Lic, enableIf:
"Param.UseLic"}`, which supplies none, and the entry `{environment: Missing}`, required by
default, which no Environment defines. (With the entry `{environment: Lic}`, Lic applies to `S`
whatever its own `enableIf`, so template validation rejects the template for the missing
`Seats`, §3.2 constraint 7; the entry's `enableIf`, not known at template validation, leaves
Lic to job creation.) Before, the Job Template's own pass found the constraint 7 error and
stopped, so `openjd run` reported only (after its `ERROR: ` prefix):

```
Model validation error: 1 validation error for JobTemplate
steps[0]:
	must supply, ... missing: 'Seats' of Environment 'Lic'.
```

Now the unresolved entry, of step 3, is found in the same pass, and it stops `S` after step
4, so `openjd run` reports only (after its `ERROR: ` prefix):

```
Model validation error: 1 validation error for JobTemplate
steps[0] -> dependencies[1] -> environment:
	names Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. ...
```

and the `Seats` error once the `Missing` entry is fixed. With `--environment` naming an
Environment Template, the error is for `Submission`, at `JobTemplate -> steps[0] ->
dependencies[1] -> environment`, and says that none of the attached Environment Templates
defines `Missing`. When the two errors are in different Steps, both are
reported, step 3's first (`test_env_params_error_order.rs`).

##### Without ENV_PARAMS

A Job Template that does not list `ENV_PARAMS` follows the same rule. One that template
validation accepts has no `environment:` entries and no Environment `enableIf`,
`exclusiveGroup` or `hostRequirements` of its own, so §7.4.1 steps 1–5 file nothing for it,
and no Step is stopped. (A template built or edited in code can still carry entries; they are
checked, and a Step with a step 1–4 error is stopped, as with `ENV_PARAMS`.) What does apply is filed under
the same keys: the template-validation-style re-checks (`Validation`), and, from the attached
Environment Templates, their literal `default`s (`Validation`), an attachment's constraint 7
and an unreachable attachment's parameters (`Step5`), its own `enableIf` (`Step6`), its values
and the single-layer rule over each Step's set (`Step7`), and its resolved `hostRequirements`
(`Step8`).
The caller limits are checked after the submission stage, as above. The aborts (a Step's own
host requirements or parameter space, a failed step-level `let`, a caller limit) keep their
forms.

Without attached Environment Templates the output is unchanged for a template that template
validation accepts: the submission stage finds no error for it, so the errors
are the Job Template's own, for `JobTemplate`, in the order they were found, and a caller limit
is reported only when there are none, as before. (A template built or edited in code that
template validation would reject can get different errors than before: more of them, from the
submission stage; one of them in place of a caller limit; the same ones in §7.4.1 order; or,
for a Step that a step 1–4 error now stops, that error in place of the Step's later ones.) With attachments it changes in three ways:

- The Job Template's errors and the attachments' are reported together, for `Submission`, in
  §7.4.1 order. Before, the Job Template's own errors were reported alone, for `JobTemplate`,
  and the attachments were applied only when it had none.
- The attachments' errors are in §7.4.1 order rather than the order they were found (an
  attachment's constraint 7 errors, step 5, before its `enableIf` that fails, step 6).
- A caller limit is reported only when the attachments add no error. Before, it was checked
  before they were applied.

For example, take a Job Template without `ENV_PARAMS` whose Job Environment `W1` and the
attached Environment `W2` both define wrap hooks (`WRAP_ACTIONS`), and whose Step `S` has a
`timeout` that resolves to 0. Before, `create_job_with_environment_templates` reported:

```
Model validation error: 1 validation error for JobTemplate
steps[0] -> script -> actions -> onRun -> timeout:
	timeout must be > 0.
```

and the single-layer error only once the timeout was fixed. Now it reports both:

```
Model validation error: 2 validation errors for Submission
JobTemplate -> steps[0] -> script -> actions -> onRun -> timeout:
	timeout must be > 0.
JobTemplate -> steps[0]:
	its Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W2' (EnvironmentTemplate[0]), 'W1'. A Session may have at most one Environment defining wrap hooks (RFC 0008).
```

(`test_env_params_error_order.rs`,
`a_job_template_without_env_params_reports_its_errors_with_the_attachments_in_one_pass`.)

#### Environment enableIf (ENV_PARAMS)

An Environment's own `enableIf` (Template Schemas §4 item 6) is an expression field of type
`bool`, decided for each Step that the Environment applies to: a Step Environment's by the
expression-field checker's `resolve`, and a Job or attached Environment's by `resolve` too,
with the Step's concrete values, once per distinct set of the values it reads, and not at all
for a Step when it reads a value that is missing, fails to resolve, or does not convert
(§3.2.2 item 2). A boolean scalar is its value. It never decides constraint 7, which holds for
the Step whatever the `enableIf` is (§3.2 constraint 7). Where it is
`false` for a Step, that Step's Sessions do not enter the Environment: it is not in the
Step's sets, so it is not entered or exited, sets no `variables`, is not in the stack, gets
no values, and adds no host requirements (which are not resolved for that Step). Two places
decide it, by what the expression can read:

- **A Step Environment** (§7.3.2) reads no effective values, so its `enableIf` is
  decided by `resolve_step_entries` (`job/create_job/env_params.rs`), with the entries'
  `enableIf`s, in the same scope, profile, library, and budgets, with POSIX paths (the Step's
  full template scope as job creation built it; an `enableIf` is not host context). A disabled Step Environment is
  dropped from `job::Step::step_environments`; when that leaves none the field is `None`. An
  error (a `null` result: "resolved to null, which does not fit type bool (Template Schemas
  §7.6)."; another type; or an evaluation failure) is an error at
  `steps[i] -> stepEnvironments[j] -> enableIf`, filed under §7.4.1 step 6 after the Step's
  Job Environments, and the Step Environment is kept. It is evaluated earlier than §7.4.1
  lists it, which changes no result (plan 3 A38).
- **A Job Environment or an attached Environment** (§7.3.2) reads its own effective
  values, so its `enableIf` is decided with them (see
  [Disabled Environments](#environment-parameter-values-env_params)), by the submission stage,
  in its step 3, for each Step, the attachments first and then the Job Environments, in the
  Job's Environment order. A superseded one's is not evaluated (see
  [Exclusive groups](#exclusive-groups-env_params)). It is decided after the Step's effective
  values are computed and constraint 7 is checked (§7.4.1 step 5), and before the value
  checks, the single-layer rule, and host requirements (§7.4.1 steps 7 and 8). It is evaluated
  under its document's template-scope profile, with the caller's budgets and the POSIX path
  format, in its `hostRequirements` scope (the merged Job Parameters, `Job.Name`,
  `Job.EnvNames`, and, of the parameters it reads, its `Env.RawParam.*` of any type and its
  non-PATH `Env.Param.*`, each bound to its concrete value). One that reads a value that has
  none is `EnableIf::NotEvaluated`: the Environment is not disabled for the Step, and that
  value's error (constraint 7, or the value's own) fails job creation. A `false` leaves the
  Environment out of the Step's entered list and values, so `job::Step::environments` and
  `environment_parameters` leave it out, and its values for the Step are not checked. An error
  is reported at `<environment path> -> enableIf` with "Resolved for Step 'A'." (or "Steps 'A'
  and 'B'") appended, filed under §7.4.1 step 6 and the first Step it names. The Environment
  is then checked for those Steps as if it is not disabled, so their other mistakes are
  reported in the same run (plan 4 A40, revised): its values, the single-layer rule, and its
  combined host requirements. Its constraint 7 and, for an unreachable attachment, the
  unreachable-attachment check do not depend on the `enableIf`, so they are reported either
  way.

An Environment whose `activation` is `ALWAYS` cannot be disabled (§4 item 4, §3.2.2 item 6).
`DeclaringEnvironment::decide` records each Step its `enableIf` is `false` for and treats
the Step as entering it, so the Environment stays in the Step's set and values and is
checked as entered, and the Step's other mistakes for it are reported in the same run.
`report_enable_if_errors` then reports one error for the Environment at `<environment
path> -> enableIf` naming every such Step: "Environment 'Lic' has activation ALWAYS, so it
cannot be disabled, but its enableIf is false for Step 'B' (Template Schemas §4 item 4)."
(or "for Steps 'B' and 'C'"), filed under §7.4.1 step 6. An `enableIf` known to be `false`
at template validation, a constant expression such as `"false"` included (§7.6 rule 5), is
rejected earlier, there. A `true` result is not this error, nor is an `enableIf` that is not
evaluated, which also treats the Step as entering it.

`Job.EnvNames` lists every Job Environment whatever its `enableIf` (§7.3.1). A disabled
Environment is still held to constraint 7 for every Step it applies to (§3.2 constraint 7,
§3.2.2 item 4), but the Step's values for it are not checked (§3.2.2 item 3), which replaces
plan 3's A33. `Step::same_environments`,
which decides whether a Session can go from one Step to the next without exiting and
entering Job Environments, compares the final sets. `CreatedJob::disabled_environments`
records, per Step, the Environments an `enableIf` disabled (Job Environments by document and
name, attached ones first, Step Environments by name), for display; an Environment the Step
would not have entered anyway (an `ON_REQUEST` one it does not request, or a `BY_DEFAULT` one
it declines with a disabled entry, §3.2 item 3.2) is not listed, since
its `enableIf` is never evaluated for that Step, and neither is one whose `enableIf` failed.
A superseded Environment (next section) is listed too. `CreatedJob::environment_statuses`
says which of the two each one is, and lists the declined ones as well (see [Environment
statuses](#environment-statuses-env_params)).

#### Exclusive groups (ENV_PARAMS)

An Environment's `exclusiveGroup` (Template Schemas §4 item 7) is applied for each Step with
the Environments' own `enableIf`, after constraint 7 and before the value checks (§7.4.1
step 6, between steps 5 and 7). The Environments that apply to the Step are taken in the
Job's Environment order: the attached Environments in attachment order, then the
`jobEnvironments` in written order. One whose group an earlier one that remains in the Step's
set already has is superseded, and its `enableIf` is not evaluated for the Step, so an
`enableIf` that would fail there is no error (JB-154). Any other has its `enableIf` decided,
and takes its group when the result does not disable it (`true`, not evaluated, or failed).
Superseding disables an Environment for the Step (§3.2.2): they are left out of
`job::Step::environments` and `environment_parameters`, their values for the Step are not
checked (constraint 6), their `hostRequirements` are not resolved for the Step, the
single-layer rule does not count them, and `CreatedJob::disabled_environments` lists them with
the ones an `enableIf` disabled, in Job order. They still apply to the Step, so constraint 7
holds for them (reported under step 5, before the group is applied). Constraint 5 still
applies to the Step's entry for one (`values_for_step` reports it before the group is
applied), as do the checks of an entry as written at template validation. An `ALWAYS` Environment has no group for the rule
(`Environment::group`), since it cannot be disabled; template validation rejects one with a
group.

The attached Environments come first in the Job's Environment order, so a group that spans the
two documents cannot be settled from the Job Template alone. So job creation proper decides no
group: `resolve_step_entries` computes the Step's values for each Job Environment that
applies to it by its `activation` and leaves them, undecided, in `StepSelection::deferred`.
The submission stage reports each one's constraint 7 under step 5 before it applies any
group.
The submission stage (`apply_environment_templates`) then decides each Step's Environments in
the Job's Environment order with one map of taken groups, each to the position in that order
of the Environment that took it, the one a later member's `superseded_by` names:

- the attachments, in attachment order: one is superseded when its group is taken, without its
  `enableIf` being evaluated; otherwise its `enableIf` is decided, and one it does not disable
  takes its group. A Step whose entry names an ambiguous attachment is stopped after step 4,
  so it decides no group; a Step that does not name it decides the attachment as any other;
- then the Job Environments, in written order: one whose group an attachment or an earlier Job
  Environment took is superseded, without its `enableIf` being evaluated, and its status for
  the Step set to `Superseded`; any other has its `enableIf` decided, and one it disables gets
  the status `Disabled`, while one it does not
  disable takes its group and is committed, its errors at the Job Template's paths, and
  its values placed among the Step's Job Environment values in written order. Every Job
  Environment that applies to the Step, whatever its status, has its constraint 7 names
  reported in an error at `steps[i]` (`JobTemplate -> steps[i]` with attachments) with the
  jobEnvironments wording.

So a group's first is the same whichever documents its members come from, and a mistake of a
grouped Job Environment is reported in the same form whether or not an attachment shares its
group.

#### Environment statuses (ENV_PARAMS)

Each decision above is recorded where it is made, as an `EnvironmentStatus` per Step and per
Environment, with, for a superseded one, the position of the group member that took the group,
so `CreatedJob::environment_statuses` says why a Step's Sessions do not enter an
Environment, not just that they do not:

| Environment | Decided in | Status |
| --- | --- | --- |
| `ALWAYS` (which includes an attachment whose Environment Template does not list ENV_PARAMS, whatever its entry says), or `BY_DEFAULT` that no disabled entry declines, or `ON_REQUEST` that an enabled entry requests | job creation proper (Job Environments, `StepSelection::job_environment_statuses`) or the submission stage (attached) | `Entered`, until step 6 changes it |
| `BY_DEFAULT` that a disabled entry declines (§7.4.1 step 4) | the same | `Declined`; its `enableIf` is not evaluated and it takes no group |
| `ON_REQUEST` that no enabled entry requests | the same | not listed |
| An earlier one of its `exclusiveGroup` remains in the set (step 6) | the submission stage, in the Job's Environment order | `Superseded`, with `superseded_by` naming that earlier one; its `enableIf` is not evaluated |
| Its own `enableIf` is `false` (step 6) | the submission stage; for a Step Environment, job creation proper (`StepSelection::step_environments`) | `Disabled` |

The submission stage lists them per Step in the Job's Environment order, attached, then
`jobEnvironments`, then the Step's `stepEnvironments`, and derives from the same statuses the
entered list on `job::Step::environments`, the entered flags host requirements and the
`WRAP_ACTIONS` rule read, and `CreatedJob::disabled_environments` (the `Disabled` and
`Superseded` ones), so the four cannot disagree. A Step stopped by a §7.4.1 step 1–4 error has
no fixed set and fails job creation, so no status of it is returned. It also lists in
`StepEnvironmentStatuses::not_found` each enabled `required: false` entry that resolved to no
attachment (§3.2.1 item 2), the case that records a warning; a disabled entry that names
nothing has no effect and is not listed, and a required one that names nothing is an error.

Each status also carries the Environment's `exclusive_group`, read from its template
Environment whatever the status (template validation rejects the field on a `stepEnvironments`
entry, so a Step Environment's is `None`), and a `Superseded` one carries `superseded_by`, the
name, document and scope of the Environment at the recorded position. That Environment is
always one the Step enters: a member takes its group only when its `enableIf` does not disable
it, and an Environment whose `enableIf` failed for a Step fails job creation.

#### Combined host requirements (ENV_PARAMS)

`combine_step_host_requirements` in `job/create_job/host_requirements.rs` (Template Schemas
§7.3.2, §3.3.3). It runs over the combined Job, so the Job Template's own
Environments and the attached ones are handled alike.

- **Resolution.** Each Environment with `hostRequirements` is resolved for the Steps whose
  Sessions enter it (step 6's final sets). Steps whose effective values for it have the same
  transport form share one resolution, so an error is reported once. For the values job
  creation makes, that is §3.2.2 equality: FLOAT `1.0` and `1.00` are different values, whose
  text an expression can read, so they resolve separately.
  A Step whose values for an Environment that declares
  parameters were rejected (a failed or missing value, or an ambiguous entry, already
  reported) is skipped for that Environment, so it gets no extra "Undefined variable" error. The table is the merged Job
  Parameters, `Job.Name`, `Job.EnvNames`, and the Environment's `Env.RawParam.*` of any type
  and non-PATH `Env.Param.*` from the Step's effective
  values. Resolution and its checks are the Step's own (`resolve_host_requirements`, shared
  through a `Site` that only decides how errors read): finite numbers, non-negative `min`,
  positive `max`, `min` ≤ `max` and at least one bound within one requirement, unique names
  within one source ignoring case, the capability-name and attribute-value grammar, and the
  resolved `anyOf`/`allOf` lengths. A failure is an error at the Environment's field, for
  example `JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0] -> min`, with
  "Resolved for Step 'A'." (or "Steps 'A' and 'B'", a list capped as the other ENV_PARAMS
  messages cap theirs) appended to the message's first line. The Step's own requirements are
  resolved earlier, by job creation proper, with their errors unchanged. A requirement that
  resolves to nothing for a Step is discarded (Template Schemas §3.3), and recorded with
  the Environment in `CreatedJob::discarded_host_requirements` for each such Step, in Job
  order after the Step's own; an Environment left with no requirements for a Step takes no
  part in its combination.
- **Combination.** The sources are the Step's own requirements, then each entered
  Environment's in Job order. An Environment the Step does not enter (an `ON_REQUEST`
  one it does not request, a `required: false` entry that resolved to nothing, a `BY_DEFAULT`
  one a disabled entry declines, an Environment whose own `enableIf` is `false` for the
  Step, or one its `exclusiveGroup` supersedes) contributes nothing. A disabled entry for an
  `ALWAYS` Environment is a §3.2 item 3.2 error of §7.4.1 step 4, which stops the Step, so its
  requirements are not combined (see [Error order](#error-order)).
  `combine` collects every source's requirements on a capability (names compared ignoring
  case) and combines them at once, so what a host must satisfy does not depend on the order
  of the sources (plan 5 S1); only the stored spelling and value order follow first appearance.
  Amounts merge to the greatest given `min` and the least given `max`, and a bound that no source gives is
  left out, so it means what Template Schemas §3.3.1 says for a Step's own requirement; amounts do not add. A
  conflict is a given `min` above a given `max`; a capability's own minimum (1 for `amount.worker.vcpu`) is not
  checked against a `max`, as it is not for a Step's own requirements. For
  `attr.worker.os.family` and `attr.worker.cpu.arch` (§3.3.3 item 2.1), the requirements
  become one `anyOf` of the values in every `anyOf` and every `allOf` list, values compared
  ignoring case, in the order and spelling of their first appearance (a source's `anyOf`
  before its `allOf`), even when that is one value. For any other attribute (item 2.2), *B* is the union of every `allOf` list and the
  value of every one-value `anyOf` (`[q, Q]` is one value; plan 5 A51), in first-appearance
  order (a source's `anyOf` before its `allOf`) and spelling; the requirements become the
  `allOf` of *B*, when it is not empty, followed by each `anyOf` of two or more values that
  has no value of *B*, as written and in source order (an `anyOf` with a value of *B* is
  implied, so dropped; equal `anyOf`s are each kept, plan 5 A50). So `anyOf [a]` and `anyOf
  [b]` are `allOf [a, b]`. A combined entry keeps the name spelling of the first source that
  names the capability (plan 2 A30), and capabilities keep the order of their first
  appearance. A capability only one source names is kept as written, so a lone `anyOf [a]`
  stays an `anyOf`, and a requirement with both `anyOf` and `allOf` stays one entry.
- **Conflicts.** A merged amount whose `min` exceeds its `max`, and an item 2.1 attribute
  with no value in every list, are errors at `JobTemplate -> steps[i]` naming the Step, the
  capability, and the sources: for an amount, the first with the greatest `min` and the
  first with the least `max`; for an attribute, every source that names it (plan 5 A49), each as
  its `... -> hostRequirements` path with the Environment's name, the list capped as the
  other job-creation lists are (512 characters, then "and N more"). Combining adds no other
  error: requirements on any other attribute can always be satisfied. The §3.3 list-length
  and duplicate-name limits are not applied to the combined set.
- **Result.** The combined set replaces `job::Step::host_requirements`, in the same shape. A
  Step none of whose entered Environments has requirements left keeps its own unchanged
  (`None` when it has none), so a Job without Environment requirements serializes as
  before. Discarded requirements take no part in the combination (§3.3.3).

The result, `CreatedJob { job, environment_documents, warnings, disabled_environments,
environment_statuses, discarded_host_requirements }`, holds the combined Job, the document of
each of its `job_environments`, the warnings as `SubmissionWarning { path, message }`, for
each Step the Environments an `enableIf` disabled or a group superseded
(`StepDisabledEnvironments`; see [Environment enableIf](#environment-enableif-env_params) and
[Exclusive groups](#exclusive-groups-env_params)), for each Step every Environment it
considered with its status and its optional entries that named nothing
(`StepEnvironmentStatuses`; see [Environment statuses](#environment-statuses-env_params)),
and for each Step the host
requirements that resolved to nothing and were discarded (`DiscardedHostRequirement`, the
Step's own first, then each entered Environment's in Job order; Template Schemas §3.3).

`AttachedEnvironmentTemplate` matches the `SERVICE` prototype's (`service-extension`
branch). That branch's public `apply_environment_templates` takes `&CallerLimits` and
returns a delta (`AppliedEnvironmentTemplates` with Services, requirement bindings, and the
attached Environments) that `into_combined_job` folds into the Job; this stage is
crate-private, takes `&ValidationContext` because it needs the caller limits, and
returns the combined Job. Merging the two is a follow-up (prototype plan P1).

### check_environment_templates

```rust
pub fn check_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    ctx: &ValidationContext,
) -> Result<Vec<SubmissionWarning>, ModelError>
```

The checks of [the submission stage](#the-submission-stage) that need no Job Parameter values,
run on a decoded Job Template (Template Schemas §3.2 last paragraph: a validator given the Environment
Templates can check constraints 5–7 early; prototype plan A9). Also in
`job/create_job/external.rs`; `openjd check --environment` calls it. The two functions share
the attachment index (`Attachments`), the name resolution of an entry (`match_entry`), and the
sorting of a Step's missing parameters (`sort_missing`), so they report the same conditions
with the same messages and paths, except as items 4 and 5 say for the
wrap-layer rule and host requirements, as item 3 says for a Step's constraint 7 error (it
leaves out the attachments it cannot know apply to the Step, such as one an entry whose
`enableIf` is not known may name, so job creation's message can list more), and
for the order and number of errors: job creation reports them in §7.4.1 order and takes no
step after step 4 for a Step with an error from steps 1–4 (see
[Error order](#error-order)), while this function reports every violation it is
certain of, in its own order (so it can report a Step's value errors that job creation
reports only once the Step's step 1–4 errors are fixed):

1. step 1 above, with an empty symbol table: a literal `default` reads no symbols; the error
   for an unreachable attachment is reported when it applies to some Step (by its
   `activation`, since no entry decides it), whatever its own `enableIf` or `exclusiveGroup`
   (§3.2 constraint 7). That error is reported after item 3's errors (job creation files it
   under step 5, after every Step's step 5 errors);
2. step 2 above, over each Step's template `dependencies`: every `environment:` entry that
   names no `jobEnvironments` entry (the first entry for a name; a repeat is a
   template-validation error), with `required` defaulting to `true`. Whether an entry is
   enabled is what its `enableIf` is known to be (`KnownEnableIfs`, as template validation
   knows it; §7.6 rule 5). An entry known to be disabled is matched as step 2 matches a
   disabled entry: it declines a `BY_DEFAULT` attachment, which items 3–5 then leave out for
   its Step, is the error for an `ALWAYS` one, and has no effect on one whose Environment
   Template does not list ENV_PARAMS. One whose `enableIf` is not known is enabled or not only
   when the Job is created, so every attachment it may name is skipped for its Step. Its name
   is resolved either way (Template Schemas §3.2.1), so a name that two or more attachments
   define is the §3.2.1 item 3 error here, whatever the `enableIf` resolves to (§7.6 rule 5).
   An enabled `required: false` entry whose parameters an attachment that declares none
   ignores gets step 2's warning, and supplies nothing to items 3–5;
3. step 3 above, without resolving anything that needs Job Parameter values
   (`DeclaringEnvironment::check_step_statically`): constraint 5; for each supplied value, a
   literal converted and checked as at submission (type and constraints; a PATH value of any
   form is used as given, §4.5 item 3), and a format string checked only against the
   single-expression rule for a list-typed parameter; and constraint 7, over the attachments
   that apply to the Step (every `ALWAYS` one, every `BY_DEFAULT` one it does not decline, and
   each `ON_REQUEST` one an enabled entry refers to), whatever each one's own `enableIf` or
   `exclusiveGroup` (§3.2 constraint 7). The value checks are made only for an attachment
   known not to be disabled for the Step (§3.2.2): one with `activation: ALWAYS`, which cannot
   be disabled, or whose own `enableIf` is known to be `true` (§7.6 rule 5,
   `known_environment_template_enable_if`), or reads a literal value of the Step's, supplied
   or the `default`, that does not convert, so it is not evaluated at job creation (§3.2.2
   item 2, `enable_if_reads_unconvertible_literal`; see
   [validation.md](validation.md#pass-11-env_params-gating)). One whose `enableIf` is known
   to be `false` is disabled, and any other is undetermined, so a violation need not hold
   whatever it resolves to. They are also made only for an attachment no earlier attachment
   of its `exclusiveGroup` may supersede: each attachment's state for the Step (entered when it
   applies and its `enableIf` is as above; not entered when it does not apply or its
   `enableIf` is known to be `false`; undetermined otherwise, including an attachment an entry
   whose `enableIf` is not known may name) goes through `apply_exclusive_groups` (§4, after
   item 10), and only a known-entered result is checked. An attachment no entry can reach
   (named like a Job Environment, or ambiguous) applies by its `activation` alone, whatever
   entries name it, and its values are not checked (job creation stops a Step whose entry
   names an ambiguous attachment, so it reports less for that Step). The Job Template's own
   Environments come after the attachments, so they supersede none. Constraint 5 is checked
   for every attachment. The same value checks are made for each Job Environment with an
   `exclusiveGroup` that the Step is known to enter after the attachments (its state from its
   `activation`, its known `enableIf` or one that reads an unconvertible literal, and the
   Step's entry as known), since template validation left them out; they are reported at the
   Job Template's paths, as job creation reports them. Their constraint 7 does not depend on
   the group, so template validation checks it. With no attachments, every such Job
   Environment first in its group is checked;
4. the `WRAP_ACTIONS` single-layer rule (`check_attached_wrap_layers`), for each Step (a
   Step whose only wrapping layers are the Job Template's own ungrouped Environments and its
   Step Environments passed template validation, which counts them the same way, so it adds
   no error; a grouped Job Environment is counted only here), over the wrapping Environments the Step's Sessions
   certainly enter, in Session order: attachments with `activation: ALWAYS` or from an
   Environment Template that does not list ENV_PARAMS, whatever the Step's entry for one says
   (`template::Environment::known_state_in`), those with
   `BY_DEFAULT` for which the Step has no entry or an enabled one (a disabled entry declines
   it, and an entry whose `enableIf` is not known may), and the `ON_REQUEST` ones an enabled
   entry resolved to; Job Environments in the same way, with an entry enabled by an `enableIf`
   known to be `true`; then the Step's Step Environments; each only when its own `enableIf`
   is known to be `true` (one known to be `false` is not entered, and any other is decided at
   job creation), or it is `ALWAYS`, which is entered whatever
   its `enableIf` and the Step's entries (`template::Environment::known_state`, given
   what the Step's entry is known to be, `template::KnownEntry`), and, for one with an
   `exclusiveGroup`, only when every earlier Environment of its group, attached or the Job
   Template's own, is known not to be entered (`certainly_entered_per_step`, which applies
   `apply_exclusive_groups` over the attachments and then the Job Environments). Job
   creation's message is reused (`wrap_layers_message`), but it lists only
   the layers the Step certainly enters, so when an `enableIf` not known here, on an entry or
   an Environment, brings in another one, job creation's message counts and lists more;
5. the §3.3.3 combination over literal requirements
   (`host_requirements::check_literal_combinations`): for each Step, the requirements of its
   own `hostRequirements` and of each Environment its Sessions certainly enter (as in item 4,
   so an Environment whose own `enableIf` is not known to be `true` is left out, unless it is
   `ALWAYS`, and so is one an earlier one of its group may supersede)
   whose format strings are all literals (`literal_part`; an `anyOf` or `allOf` written as
   one expression is never literal), resolved with an empty symbol table
   and combined by the same `combine` job creation uses, so a conflict among literal sources
   has job creation's message. A literal requirement is never discarded (Template Schemas
   §3.3), so leaving out the others finds only conflicts job creation also has. When a non-literal requirement of the same capability also
   takes part, job creation may name other sources or bounds, or report a conflict this
   check cannot see. An Environment whose values for the Step fail here (reported by item 3,
   or, for an attachment no entry can reach, by item 1's attachment-level error for its
   parameters without a `default`) is still combined, while job creation skips it until its values are fixed, so `check` can
   report one more conflict than `openjd run` for such a submission, or name that Environment
   where `openjd run` names another source with the same bound. An attachment that
   declares parameters and that an ambiguous entry of the Step names is left out, since job
   creation computes no values for it and so skips its requirements. An `ALWAYS` attachment that
   an entry whose `enableIf` is not known names is kept, since the Step's Sessions enter it
   either way; a `BY_DEFAULT` one is left out, since the entry may decline it. A conflict that involves
   only the Job Template's own requirements (a Job Environment's with its Step's, no
   attachment) is reported here too, though `openjd check` without `--environment` and
   template validation leave it to job creation.
   Combining a subset of a Step's requirements finds only conflicts the full combination also
   has, since more sources can only raise a `min`, lower a `max`, or narrow the values common
   to every list of an `attr.worker.os.family` or `attr.worker.cpu.arch` requirement. A
   Step with no literal Environment requirement is skipped.

`ctx` supplies the caller limits, as for job creation. A submission that passes can still be
rejected at job creation, for a format-string value or non-literal `default` that does not
resolve to a valid value with the submitted Job Parameters, for an entry or an Environment
whose `enableIf` is an expression (and the Environments it may bring into a Step's set), or
for a host requirement that is not a literal.

### convert_environment

```rust
pub fn convert_environment(env: &template::Environment) -> job::Environment
```

Converts a template environment to a resolved job environment. Takes 1 argument and is
infallible. Environment variables and script fields remain as FormatString (session-scope).

A separate `convert_environment_with_symtab` function accepts an optional `&SymbolTable`
to filter the symbol table to only symbols referenced by the environment's format strings.

### evaluate_let_bindings

```rust
pub fn evaluate_let_bindings(
    bindings: &[String],
    symtab: &SymbolTable,
    library: Option<&FunctionLibrary>,
    path_format: PathFormat,
    memory_limit: Option<usize>,
    operation_limit: Option<usize>,
) -> Result<SymbolTable, ModelError>
```

Evaluates `"name = expression"` bindings sequentially. Each binding sees the results of
all prior bindings in the same block. Returns a new symbol table with the binding results
added.

The `library` parameter is optional (pass `None` for template-scope bindings that don't
need host functions). `path_format` controls path construction behavior.
`memory_limit` / `operation_limit` bound each binding's evaluation
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`); `None`
uses the spec-recommended defaults.

## Design Decisions

### Explicit Instantiation (vs Generic Traversal)

The Python library uses `instantiate_model()` which generically traverses Pydantic models
via metadata to find and resolve FormatString fields. The Rust crate uses explicit conversion
methods on each type instead. This is more verbose but:

- Makes template-scope vs session-scope distinction explicit and compiler-verified
- Avoids runtime reflection or trait object overhead
- Makes it clear which fields are resolved at which phase
- Allows different resolution strategies per field (e.g., host requirements resolve
  FormatStrings to f64, while step names resolve to String)

### Merged Constraint Validation

When multiple templates define the same parameter, constraints are merged (intersection for
allowedValues, most restrictive for ranges). The merged constraints are validated for
consistency — e.g., if the intersection of allowedValues is empty, or if the merged
min > merged max, that's an error. This catches conflicts that individual template
validation wouldn't find.

### Path Normalization Without Filesystem Access

`normalize_path_str` performs pure path normalization (resolving `.` and `..` components)
without filesystem access. This is important because:
- Templates may reference paths that don't exist yet
- The crate should be usable in environments without filesystem access
- Path mapping (for cross-platform execution) happens at session time, not job creation time
