# Template Types

The `template` module contains types deserialized directly from YAML/JSON templates. These are
"unresolved" — `FormatString` fields have not been evaluated, parameter values have not been
substituted, and syntax sugar (like `SimpleAction`) has not been expanded.

All types use `#[serde(rename_all = "camelCase", deny_unknown_fields)]` for strict deserialization.
Section references (§) refer to the
[2023-09 Template Schemas](https://github.com/OpenJobDescription/openjd-specifications/wiki/2023-09-Template-Schemas).

## Constrained String Types (§7)

Three string types enforce spec constraints at deserialization time via custom `Deserialize` impls:

| Type | Pattern | Length | Usage |
|------|---------|--------|-------|
| `Identifier` | `[A-Za-z_][A-Za-z0-9_]*` | 1–512 | Parameter names, embedded file names |
| `Description` | Unicode except Cc control chars (allows `\n`, `\r`, `\t`) | 0–2048 | Description fields |
| `ExtensionName` | `[A-Z_0-9]{3,128}` | 3–128 | Extension names in `extensions` list |

These types implement `Deserialize`, `Serialize`, and `Display`. Validation happens during
deserialization — invalid values produce serde errors before the validation pipeline runs.

## Root Templates

### JobTemplate (§1.1)

```rust
pub struct JobTemplate {
    pub specification_version: String,
    pub schema: Option<String>,                              // $schema
    pub extensions: Option<Vec<ExtensionName>>,
    pub name: FormatString,
    pub description: Option<Description>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub job_environments: Option<Vec<Environment>>,
    pub services: Option<Vec<Service>>,                        // SERVICE extension (RFC 0009), §1.1 item 8
    pub external_services: Option<Vec<ServiceRequirement>>,    // SERVICE extension (RFC 0009), §1.1 item 9
    pub steps: Vec<StepTemplate>,
}

impl JobTemplate {
    pub fn services(&self) -> &[Service];                      // empty when absent
    pub fn external_services(&self) -> &[ServiceRequirement];  // empty when absent
}
```

Helper: `parameter_definitions_list()` returns `&[JobParameterDefinition]`, defaulting to
an empty slice when `parameter_definitions` is `None`.

`services` is the Job Template's one Service list; each Service's *scope* (the Steps whose Tasks
depend on it) is computed from the `dependencies` of the template's Steps, Services, and Job
Environments — a Step lists `service: <name>` to depend on a Service (§3.2, §9.1, see
[Service scope](#service-scope-91) below) — so a `<StepTemplate>` has no separate Service list and
the order of `services` carries no meaning. `externalServices` declares the external Services whose
endpoints the template reads, with their ports (§9.8). The pre-RFC-0009 keys `jobServices` and
`stepServices` are not properties of any type and are rejected as unknown fields at decode.

### EnvironmentTemplate (§1.2)

```rust
pub struct EnvironmentTemplate {
    pub specification_version: String,
    pub schema: Option<String>,                              // $schema, ignored (§1.2 item 2)
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub environment: Option<Environment>,                    // optional since RFC 0009
    pub services: Option<Vec<Service>>,                      // SERVICE extension (RFC 0009)
}

impl EnvironmentTemplate {
    pub fn environment(&self) -> Option<&Environment>;
    pub fn services(&self) -> &[Service];   // empty when `services` is absent
}
```

An Environment Template defines an Environment, a list of Services (§1.2 item 6, §1.2.2
"external Services"), or both; validation rejects a document that defines neither. `services`
has the same list constraints as a Job Template's `services` and is validated by the same code
(pass 12), except that a Service's `dependencies` here may list only other Services of the same
document, with the `service` key, and disabled `environment:` entries (§1.2 *services*
constraint 5, see [validation.md](validation.md)) (the document has no Steps; a `dependsOn` entry, an unknown
Service, and a cycle among them are pass 12 errors), a Service here is not checked for an empty scope
(its scope is computed over the combined Job at submission, and one nothing lists is never started),
and `externalServices` is not a property of this type (rejected
as an unknown field). `environment`
is `Option` on the wire too: an explicit `environment: null` is "not provided". The `$schema`
property is accepted and ignored, as on the Job Template.

## StepTemplate (§3)

> **Note:** Step names are plain `String`, not `Identifier` or `FormatString`.
> They accept any Unicode except Cc control characters — unlike parameter names
> and environment names which are constrained to `[A-Za-z_][A-Za-z0-9_]*` via
> the `Identifier` type. This is per the OpenJD specification §3.1 `<StepName>`. No character is
> reserved under `SERVICE` either: a dependency names a Step with `dependsOn` and a Service with
> `service` (§3.2), so `Layer: Beauty` is an ordinary Step name in every template.

```rust
pub struct StepTemplate {
    pub name: FormatString,
    pub description: Option<Description>,
    pub let_bindings: Option<Vec<String>>,           // "let" field in YAML
    pub dependencies: Option<Vec<StepDependency>>,
    pub step_environments: Option<Vec<Environment>>,
    pub host_requirements: Option<HostRequirements>,
    pub parameter_space: Option<StepParameterSpaceDefinition>,
    pub script: Option<StepScript>,
    // SimpleAction syntax sugar (FEATURE_BUNDLE_1)
    pub bash: Option<SimpleAction>,
    pub python: Option<SimpleAction>,
    pub cmd: Option<SimpleAction>,
    pub powershell: Option<SimpleAction>,
    pub node: Option<SimpleAction>,
}
```

### SimpleAction (FEATURE_BUNDLE_1)

Syntax sugar that expands into a `StepScript` with an embedded file and `onRun` action
(Template Schemas §8). A step must have either `script` or exactly one simple action
field — never both.

```rust
pub struct SimpleAction {
    pub let_bindings: Option<Vec<String>>,
    /// `<DataString>`, `@fmtstring[host]` — parsed at deserialization, so
    /// malformed `{{ ... }}` fails at parse time like any other format string.
    pub script: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub timeout: Option<FormatString>,
    pub cancelation: Option<CancelationMode>,
}

/// Which interpreter key the SimpleAction was written under.
pub enum SimpleActionKind { Python, Bash, Cmd, Powershell, Node }

impl SimpleActionKind {
    pub const ALL: [SimpleActionKind; 5];          // the order resolve_syntax_sugar considers them
    pub fn field_name(self) -> &'static str;       // "bash", … — also the desugared `command`
    pub fn file_extension(self) -> &'static str;   // ".sh", ".py", ".bat", ".ps1", ".js"
    pub fn arg_prefix(self) -> &'static [&'static str]; // ["/C"] for cmd, ["-File"] for powershell, else []
    pub fn synthetic_arg_count(self) -> usize;     // arg_prefix().len() + 1 (the Task.File reference)
    pub fn desugar(self, step_name: &str, sa: &SimpleAction) -> StepScript;
    pub fn remap_desugared_path(self, rel: &[PathElement]) -> Vec<PathElement>;
}

impl StepTemplate {
    pub fn simple_actions(&self) -> impl Iterator<Item = (SimpleActionKind, &SimpleAction)>;
    pub fn simple_action(&self, kind: SimpleActionKind) -> Option<&SimpleAction>;
    /// `script.clone()` if present, else the first SimpleAction desugared; `None` if neither.
    pub fn resolve_syntax_sugar(&self) -> Option<StepScript>;
}
```

`desugar` builds the generated embedded file's name from the step name
(every character outside `[A-Za-z0-9]` → `_`, as in `openjd-model-for-python`;
at most 200 chars; `_`-prefixed if it would start with a digit; suffixed
`_script` — always a valid identifier, so the generated reference always
parses), sets `command` to the interpreter name,
and produces `args = [<arg_prefix>..., "{{Task.File.<name>}}", <authored args>...]`.

`remap_desugared_path` maps a validation-error path on the desugared
`StepScript` (relative to its `script` node) back onto the sugar field the
author wrote, so a diagnostic never names a node that does not exist in the
template — see `specs/model/validation.md` § Reporting on desugared forms
for the table. Validation (passes 6 and 8) and job creation all validate the
desugared form and report through this remap.

### StepDependency (§3.2)

One entry of a Step's, a Service's, or a Job Environment's `dependencies`. It is one of
`dependsOn: <StepName>` or, with `ENV_PARAMS`, its synonym `step: <StepName>`; with `SERVICE`,
`service: <ServiceName>`; or, with `ENV_PARAMS` and in a Step's list or (with `SERVICE`) the list
of a Service in a Job Template's `services` only, `environment: <EnvironmentName>`. Exactly one
of the four keys must be present:

```rust
pub struct StepDependency {                                  // Debug, Clone, PartialEq, Eq, Deserialize
    pub depends_on: Option<String>,                          // dependsOn
    pub step: Option<String>,                                // ENV_PARAMS; synonym of dependsOn
    pub service: Option<String>,                             // SERVICE extension
    pub environment: Option<String>,                         // ENV_PARAMS
    pub required: Option<bool>,                              // ENV_PARAMS; absent = true
    pub enable_if: Option<ExprField>,                        // ENV_PARAMS; @expr bool; absent = enabled
    pub parameters: Option<EnvironmentParameterValueMap>,    // ENV_PARAMS
}

pub enum DependencyTarget<'a> {       // Debug, Clone, Copy, PartialEq, Eq, Hash
    Step(&'a str, StepKey),           // satisfied when the Step has completed; the key as written
    Service(&'a str),                 // satisfied when the Service is READY
    Environment(&'a str),             // ENV_PARAMS; does not delay scheduling
}

pub enum StepKey {
    DependsOn,  // dependsOn
    Step,       // step (ENV_PARAMS)
}
impl<'a> DependencyTarget<'a> {
    pub fn step(self) -> Option<&'a str>;
    pub fn service(self) -> Option<&'a str>;
    pub fn environment(self) -> Option<&'a str>;
}
impl Display for DependencyTarget<'_>; // `dependsOn: X` / `step: X` / `service: X` / `environment: X`

impl StepDependency {
    pub fn on_step(name: impl Into<String>) -> Self;
    pub fn on_service(name: impl Into<String>) -> Self;
    pub fn on_environment(name: impl Into<String>) -> Self;
    pub fn target(&self) -> Option<DependencyTarget<'_>>;  // None unless exactly one naming key
    pub fn step(&self) -> Option<&str>;                    // a dependsOn or step entry
    pub fn service(&self) -> Option<&str>;
    pub fn environment(&self) -> Option<&str>;
    pub fn is_well_formed(&self) -> bool;                  // exactly one naming key
    pub fn describe(&self) -> String;  // `dependsOn: X`, `service: X`, `dependsOn: X, service: Y`, `{}`
    pub fn literal_enabled(&self) -> Option<bool>;
    pub fn is_required(&self) -> bool;
}

pub fn lists_service(dependencies: Option<&[StepDependency]>, name: &str) -> bool;
pub fn listed_service_names(dependencies: Option<&[StepDependency]>) -> impl Iterator<Item = &str>;
pub fn listed_step_names(dependencies: Option<&[StepDependency]>) -> impl Iterator<Item = &str>; // dependsOn and step
```

An entry is `dependsOn: <StepName>`, or with ENV_PARAMS its synonym `step: <StepName>` (§3.2
item 2), or with ENV_PARAMS `environment: <EnvironmentName>`
with optional `required`, `enableIf` (§3.2 item 3.2), and `parameters`. Every key is optional in
the struct, so an entry
with more than one form or none decodes and pass 6 reports it at the entry's path (§3.2
constraint 1). `target()` returns the form, or `None` for such an entry (`is_well_formed()`
is `target().is_some()`). A `step` and a `dependsOn` entry are both `DependencyTarget::Step`,
differing only in the `StepKey`, which no rule reads: `step()` returns the Step name for
either, so self-dependency, unknown-Step, duplicate, cycle, and job-creation checks treat them
alike. `environment()` reads the other form; `is_required()` applies
the default; `literal_enabled()` is the `enableIf` as written when it is a literal:
`Some(true)` for an absent `enableIf` or the boolean scalar `true`, `Some(false)` for `false`,
and `None` for an expression. Template validation knows more than this: an expression whose
value does not depend on the values unknown there, such as `"true"` or `"Param.F or
true"`, is known too (§3.2 checked-at paragraph, §7.6 rule 5), and every check before job
creation reads that value (see [validation.md](validation.md#known-enableif-values-env_params)),
not `literal_enabled()`. `enableIf` is an expression field of type `bool`; see
[Expression fields](#expression-fields-75). `on_step(name)` and `on_environment(name)` build an entry, and
`DependencyTarget`'s `Display` writes it as a template does (`dependsOn: X`, `step: X`,
`service: X`, `environment: X`); `StepKey::as_str()` is `dependsOn` or `step`.

The struct derives `PartialEq` and `Eq` but not `Hash`, because a literal value is kept as a
`serde_json::Value`, which has no `Hash` (nor has `ExprField`, which holds an `f64`), so the
duplicate checks key a set on `target()` (with `step` and `dependsOn` folded into one key)
rather than on the entry. `describe()` writes the entry as written for messages, including a
malformed one (every naming key it gives, `{}` for none).

Job creation converts the Step entries (`dependsOn` and `step`) and the `service` entries to
`job::StepDependency`, whose keys are `dependsOn` and `service`, so a `step: X` entry is
`dependsOn: X` in the Job; an `environment:` entry does not delay scheduling (§3.2 item 3), so
it is not part of the Step (or Step/Service) dependency graph, and a list left empty by the
filter becomes `None`.

With `SERVICE` and without `ENV_PARAMS`, an entry with both `dependsOn` and `service` or with no
key is reported at the entry's path with the `SERVICE` prototype's wording (`a dependency names
a Step with dependsOn or a Service with service, not both: {dependsOn: A, service: X} (Template
Schemas §3.2).` / `a dependency must name a Step with dependsOn or a Service with service; this
entry gives neither (Template Schemas §3.2).`); with `ENV_PARAMS` the message names every form
the document's extensions make available (`must have exactly one of 'dependsOn', 'step',
'service', or 'environment', not both 'step' and 'service'.`). `target` is `None` for such an
entry, and every reader (`step`, `service`, the three free functions, the scope graph) treats it
as no dependency. A Service's and an Environment's `dependencies` (SERVICE lists) accept `step:`
as `dependsOn:`. A Job Template Service's list also accepts the `environment:` form with its
`required`, `enableIf`, and `parameters` keys (RFC 0009 `<StepDependency>` item 3), checked by
pass 11 as a Step's are, except that `parameters` and `enableIf` are in the Service's own scope
(`RawParam.*`, `Param.*` other than PATH and LIST[PATH], `Job.Name`, `Job.EnvNames`, the Service's
`let` names; not `Step.Name`). An Environment Template Service's list accepts an `environment:`
entry only when its `enableIf` is the literal `false` and it has no `parameters`, so that it can
only decline an Environment (RFC 0009 §1.2 *services* constraint 5; `dependency 'environment: E'
must be disabled: …` otherwise, see [validation.md](validation.md)). An Environment's list
rejects an `environment:` entry (`dependency 'environment: E' names an Environment; an
Environment does not request another Environment (RFC 0009 <StepDependency> item 3).`), and
both reject the three keys on any other entry (`'required' is permitted only in an
'environment' entry of the dependencies of a Step or of a Service.`). Without `SERVICE` the `service` key is gated like every other `SERVICE`
property (`steps[i] -> dependencies[j] -> service: service requires the SERVICE extension.`), by
the gating pass that runs ahead of every other; a plain template keeps `dependsOn` only. The three
free functions accept the `Option` field directly (`step.dependencies.as_deref()`). `job` re-exports
`DependencyTarget`, and `job::StepDependency` has the same shape and accessors (see
[job-types.md](job-types.md)).

A Step's Tasks are scheduled once every listed Step has completed and every listed Service is
READY. Listing an `externalServices` name is valid and is satisfied when that external Service is
READY; it does not affect any scope, but it is what lets the Step (or Service) reference that
required Service's `Service.<name>.*` values, exactly as for an inline Service (§9.8 item 2;
`listed_requirements`). Pass 6 checks a Step's list — the one-of rule, self and duplicate entries,
and the `dependsOn` and `step` entries (`dependency 'X' not found.`, or, when a Service named `X` exists,
`dependency 'X' names no Step; did you mean 'service: X'?`); pass 12 resolves the `service` entries
(`dependency 'service: X' not found: no Service of that name in services or externalServices.`,
or, when a Step named `X` exists, `dependency 'X' names no Service; did you mean 'dependsOn: X'?`;
see [validation.md](validation.md)).

### Expression fields (§7.6)

```rust
pub enum ExprField {
    Bool(bool),          // a boolean scalar, used as written
    Int(i64),            // an integer scalar
    Float(f64),          // a number with a fraction or exponent
    Expression(String),  // a string: one expression, without {{ }}
    Null,                // a present null
}
```

An expression field (`@expr <type>`) holds one Expression Language expression written
without `{{ }}`, or a boolean, integer, or float scalar used as written. `ExprField` is the
value as written; it knows nothing of the field's target type, which each field that uses it
supplies to the checker, so a later field (a Step `enableIf`, RFC 0011's loop fields) reuses
both unchanged. The fields that use it today are an `environment:` entry's `enableIf` and an
Environment's own `enableIf` (below), both of type `bool`.

Decoding is by hand and accepts every scalar, so that template validation, which knows the
target type, reports a scalar that does not fit (`enableIf: 1`) at the field's path with the
§7.6 rule, rather than serde reporting it without one. A string is always `Expression`, never
read as a scalar: `"true"` is the expression `true`. A list or a map is a decode error that
names the field ("enableIf must be a boolean, integer, float, or string (an expression field,
Template Schemas §7.6)"), as is an integer above `i64::MAX` that fits `u64` ("enableIf: the
integer N is out of range for an expression field …"). A larger integer, or one below
`i64::MIN`, arrives from JSON number parsing as a float and decodes as `Float`. Decode errors
carry no path, which is why the message names the field. A present `null` is an error, not an
omitted field (§7.6), so an optional `enableIf` is declared `#[serde(default, deserialize_with =
"expr_field::deserialize_enable_if")]`: an absent key is `None`, a present `null` is
`Some(ExprField::Null)`, and template validation rejects it at the field's path. (A later
expression field adds a `deserialize_with` that names it; `ExprField`'s own `Deserialize`,
which names no field, says "an expression field must be …" and, for a `u64`, "the integer N is
out of range for an expression field …". A plain `Option<ExprField>` reads a present `null` as
`None`, so every optional expression field needs such a `deserialize_with`.) `PartialEq` and
`Eq` are written by hand, comparing a float's bits (a decoded value is never NaN; `-0.0` and
`0.0` are unequal); `as_bool()` returns the boolean scalar, `None` otherwise.

The crate-private checker in `template/expr_field.rs` applies §7.6 for a target type `G`,
given the field's scope (a symbol table) and an `ExprFieldEval` (profile, library, budgets;
paths are always POSIX, since an expression field is evaluated in template scope, not a host
context: Template Schemas §7.6, the paragraph after its rules; spec `5535230`):

- `validate` (template validation, rules 1, 2, 4): a scalar must fit `G` (only `int` widens to
  `float`), a `null` is an error, and an expression must parse under the document's profile,
  evaluate against the scope (unknown names are errors), and have a static type that fits
  `G`. A parse error of a value that contains `{{` adds "An expression field takes an
  expression without '{{ }}' (Template Schemas §7.6)." after the message, before the
  expression and caret lines (". " before it, or " " after a summary that ends in `.`, `?`,
  or `!`). A float scalar is quoted in Rust's shortest round-trip form, which keeps a whole
  number's fraction (`3.0`; `1e3` is quoted `1000.0`), and a non-finite one (only a template
  built in code can hold one) is an error.
- `check_static_type` is the fit rule: `unresolved[U]` is read as `U`; where `G` admits
  `nulltype` (`nulltype`, `any`, or a choice with one) the type must satisfy `G`; otherwise
  `nulltype` is an error and `nulltype` members of a union are dropped (`U?` is accepted
  when `U` fits). Fitting is `ExprType::satisfies` (Expression Language §1.2.3 step 1), or
  `satisfies` after every `int` becomes `float`, element-wise.
- `resolve` (the field's stage) evaluates and returns the value: a `null` result is an error
  unless `G` admits `nulltype`, even on an optional field; an `int` result is widened to
  `float` where only that fits; any other result that does not fit is an error.
- `known_value` is the field's value before its stage, when it is *known* there (Template
  Schemas §7.6 rule 5): the scope binds each value not yet known to `unresolved[T]` of its
  type, a scalar that fits is known, and an expression is known when it parses, evaluates,
  and gives a value that is not unresolved and fits, by the Expression Language's rules for
  unknown operands (Expression Language §2.1.6 "Unknown operands" and "Conditional
  Expressions with Unknown Conditions"). A parse or evaluation error gives `None`.
  `known_bool` applies it to an optional `bool` field (an `enableIf`), with an absent field
  `Some(true)`. Template validation uses them for every `enableIf` (see
  [validation.md](validation.md#known-enableif-values-env_params)); job creation evaluates an
  Environment's own `enableIf` with `resolve` and concrete values only (Template Schemas
  §3.2.2 item 2).

The checker lives in openjd-model because it is template-field policy built on
openjd-expr's public type API (`satisfies`, `code`, `params`, `ExprValue::coerce`);
openjd-expr is unchanged.

### Environment parameters (§4.5, ENV_PARAMS)

```rust
pub struct EnvironmentParameterDefinition {
    pub definition: JobParameterDefinition,      // its `default` is always None
    pub default: Option<EnvironmentParameterValue>,
}

pub enum EnvironmentParameterValue {
    FormatString(FormatString),                  // a string containing `{{`
    Literal(serde_json::Value),                  // anything else, as written
}

pub struct EnvironmentParameterValueMap(pub BTreeMap<String, EnvironmentParameterValue>);
```

An `<EnvironmentParameterDefinition>` has the shape of a `<JobParameterDefinition>` of the
same type, but its `default` is a format string resolved at job creation, which the Job
Parameter types' typed `default` fields cannot hold. The deserializer removes `default`,
decodes the rest as a `JobParameterDefinition`, and keeps `default` as an
`EnvironmentParameterValue`. An explicit `default: null` is no default, as it is for a Job
Parameter. Reusing the Job Parameter definition keeps `allowedValues`,
`minLength`, `item`, and the other constraints, their decoders, and `check_constraints`
in one place, and with them every field of that shape, which §4.5 permits: any type the
document allows, `RANGE_EXPR` included, and `userInterface`, `objectType`, and `dataFlow`,
with their decoders and the definition checks of `validate_definition`. Those three are
informational; nothing reads them after validation.

The same `EnvironmentParameterValue` holds a `default` and an `environment:` entry value,
since both are `@fmtstring` and the spec splits them the same way. A literal's validity
depends on the parameter's type, which is not known at decode, so it is kept as written and
checked in pass 11 (or, for an external Environment, at job creation against the attached
Environment Templates). The `parameters` map
is a `BTreeMap` keyed by plain strings: the workspace's `serde_json` has no
`preserve_order` feature, so a template map reaches the decoder sorted by key anyway, and a
plain string key lets pass 11 report a key that is not an `<Identifier>` at its own path.

## Environment (§4)

```rust
pub struct Environment {
    pub name: String,
    pub description: Option<Description>,
    pub env_parameter_definitions: Option<Vec<EnvironmentParameterDefinition>>,  // ENV_PARAMS; envParameterDefinitions
    pub activation: Option<EnvironmentActivation>,                              // ENV_PARAMS
    pub host_requirements: Option<HostRequirements>,                            // ENV_PARAMS
    pub enable_if: Option<ExprField>,                                           // ENV_PARAMS; @expr bool; absent = enabled
    pub exclusive_group: Option<String>,                                        // ENV_PARAMS; an <Identifier>
    pub dependencies: Option<Vec<StepDependency>>, // SERVICE extension (RFC 0009), §4 item 3
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

pub enum EnvironmentActivation {  // serde: SCREAMING_SNAKE_CASE
    Always,     // ALWAYS: cannot be disabled
    ByDefault,  // BY_DEFAULT, the default
    OnRequest,  // ON_REQUEST
}

impl Environment {
    /// `activation`, or `BY_DEFAULT` when absent (§4 item 4).
    pub fn activation(&self) -> EnvironmentActivation;
    /// `Some(true)` for an absent or `true` `enableIf`, `Some(false)` for
    /// `false`, `None` for any other value (decided at job creation).
    pub fn literal_enabled(&self) -> Option<bool>;
    /// The Service names `dependencies` lists with the `service` key, in
    /// list order (a `dependsOn` entry, a validation error, yields nothing).
    pub fn listed_services(&self) -> impl Iterator<Item = &str> + '_;
    /// `dependencies` lists at least one Service.
    pub fn depends_on_service(&self) -> bool;
    /// Any format string (variables, actions, embedded files, script `let`)
    /// references a `Service.*` value.
    pub fn references_service(&self) -> bool;
}
```

`activation()` applies the default (§4 item 4: absent means `BY_DEFAULT` in a document that
lists ENV_PARAMS). An Environment from a document that does not list ENV_PARAMS behaves as
`ALWAYS` with no `enableIf`, except that a Step's disabled entry for it has no effect and is
not an error; `activation()` does not know the document, so job creation applies this to each
attached Environment Template that does not list the extension (see
[job-creation.md](job-creation.md#environment-parameter-values-env_params)). In a Job
Template without ENV_PARAMS the readings agree, since it has no entries and no `enableIf`.
`BY_DEFAULT` and
`ALWAYS` are both entered by every Step's Sessions; under `BY_DEFAULT` a Step may decline it
with a disabled `environment:` entry (§3.2 item 3.2), the Environment's own
`enableIf` may leave a Step out, and an earlier Environment of its `exclusiveGroup` may
supersede it (§4 item 7), and under `ALWAYS` none of these can: a Step's disabled entry for
it, or its `enableIf` being `false` for a Step, is an error (§4 item 4, §3.2.2 item 6; see
[validation.md](validation.md#environment-enableif-env_params) and
[job-creation.md](job-creation.md#environment-enableif-env_params)). An unrecognized value,
including one in another case (`always`, `by_default`) or a value from before the rename
(`ALL`, `ONLY_REQUESTED`), is a decode error. Pass 11 rejects the field without
ENV_PARAMS and on a `stepEnvironments` entry. `EnvironmentActivation::as_str` gives the value as written.

`host_requirements` (§4 item 5) is the Step's [`HostRequirements`](#host-requirements-33)
type, with the same per-source checks (pass 6) and its own format-string scope (pass 8; see
[validation.md](validation.md#environment-hostrequirements-env_params)). Pass 11 rejects it
without ENV_PARAMS and on a `stepEnvironments` entry. Job creation resolves it once per
entering Step and combines it with the Step's own (see
[job-creation.md](job-creation.md#combined-host-requirements-env_params)).

`enable_if` (§4 item 6) is an [expression field](#expression-fields-75) of type `bool`,
decoded as an entry's `enableIf` is, with a present `null` kept as `ExprField::Null`. It
decides whether a Step's Sessions enter the Environment, and unlike `activation`,
`hostRequirements`, and `envParameterDefinitions` it is permitted on every position, a
`stepEnvironments` entry included. Pass 11 requires ENV_PARAMS in the document (which
requires EXPR; see [validation.md](validation.md#entry-points)), and pass 8 checks it in its
scope (see [validation.md](validation.md#environment-enableif-env_params)). `literal_enabled()` is
the value as written when it is a literal: `Some(true)` for an absent `enableIf` or the
boolean scalar `true`, `Some(false)` for the scalar `false`, and `None` for an expression.
What template validation counts as entered or not reads the known value instead (§4, after
item 10; §7.6 rule 5), so a constant expression such as `"true"` is known there (see
[validation.md](validation.md#known-enableif-values-env_params)). It never decides
constraint 7, which holds for every Step the Environment applies to whatever its `enableIf`
(§3.2 constraint 7). Job creation resolves it once per Step that the Environment applies to and
for which it is not superseded, with the Step's concrete values, and not at all when it reads
one that is missing or does not convert (§3.2.2 item 2; see
[job-creation.md](job-creation.md#environment-enableif-env_params)); no job type records it,
since a disabled Environment is simply left out of the Step's sets (and, under `SERVICE`, it is
resolved the same way for each Service, with the Service's values).

**`dependencies`** (§4 item 3, RFC 0009). The Services an Environment depends on, as
`<StepDependency>` entries with the `service` key, held as written (like a Step's). The
field is permitted only on a `jobEnvironments` entry and on an Environment Template's
`environment`; a `stepEnvironments` entry follows its Step's `dependencies` and may not give a
list of its own. Listing a Service is what makes `Service.<name>.<port>.port` /
`.connectAddress` available to the Environment's format strings (pass 8 seeds exactly the
listed Services — `listed_services` / `listed_requirements` over `env.dependencies` — the same
rule as for a Step or a Service). Every Step whose Sessions enter a Job Environment that lists
an inline Service is in that Service's scope (§9.1 rule 3, below), and each Step and each Service
whose Sessions enter it depends on each Service it lists (a dependency-graph edge). A Service
whose Sessions enter an Environment that lists it, by its `activation` or through its own
`environment:` entry, is the cycle `X -> X`: there is no exception for it.
Validation (pass 12) gates the field on `SERVICE`, rejects it on a Step Environment, and checks
each entry (see [validation.md](validation.md)). Job creation carries the list onto
`job::Environment`.

**Which Sessions enter a Job Environment** (RFC 0009 "Services run inside Environments"). There
is no `runScope` property: `'runScope' is not a property; …` is the decode hint for the unknown
field on an Environment, with or without `SERVICE`. A Job Environment is entered by every Task
Session and every Service Session when its `activation` is `ALWAYS` or `BY_DEFAULT`, except, for
`BY_DEFAULT`, the Sessions of a Step or Service that declines it with a disabled `environment:` entry (RFC 0010 §3.2 item 3.2); when `ON_REQUEST`, by the Sessions of the Steps and Services that request
it with an enabled `environment:` entry of their own `dependencies`; in both cases less any
entity for which its `enableIf` is `false`. A Step Environment is entered only by the Task
Sessions of its Step. `template::Entrances::of(&JobTemplate)` answers
this for every Step and Service as known at template validation, after the `exclusiveGroup` rule (`Entrance::Enters`, `DoesNotEnter`, or
`Undetermined` when an `enableIf` not known before the Job is created (§7.6 rule 5) decides; see [Service scope](#service-scope-91)), and
job creation decides it for each Step (`job::Step::environments`) and each Service
(`job::Service::environments`).

`exclusive_group` (§4 item 7) names a group of Environments of which a Step's Sessions enter
at most one. For each Step, of the Environments its Sessions would enter that share a group
and whose `enableIf` is not `false` for it, only the first in the Job's Environment order
(attached ones first, then `jobEnvironments`; §1.1 item 7) is entered, and the others are
*superseded*, which disables them for the Step (§3.2.2); they still apply to the Step, so
constraint 7 holds for them (§3.2 constraint 7). Job creation takes them in that order and
does not evaluate a superseded one's `enableIf` (§7.4.1 step 6). It is a plain `String`, so pass 11
reports a value that is not an `<Identifier>` (§7.1, at most 64 characters, or 512 with
`FEATURE_BUNDLE_1`) at its own path. Pass 11 also rejects it without ENV_PARAMS, on a
`stepEnvironments` entry, and on an Environment whose `activation` is `ALWAYS` (constraint
1). `group()` (crate-private) is the group the rule applies with: none for an `ALWAYS`
Environment, so a template that breaks constraint 1 and is created anyway never has its
`ALWAYS` Environment superseded. Before the Job is created, `known_state(enabled, entry)`
gives what is known about a Step entering the Environment from its `activation`, what its
`enableIf` is known to be (`enabled`), and what the Step's entry is known to be (`entry`) (`Known::Entered`, `NotEntered`, or `Undetermined`; the first three bullets
after §4 item 10), and `apply_exclusive_groups` applies the group bullet to one Step's
Environments in Job order: a grouped one is entered only when it is entered by its own state
and every earlier one of its group is known not to be, and is not entered when an earlier one
would, by its own state, be known to be entered (that one, or one before it, is then the
group's first). No job type records the group: `Job.EnvNames` lists every Environment (§7.3.1), and a superseded one is
left out of the Step's sets like any disabled one (see
[job-creation.md](job-creation.md#exclusive-groups-env_params)). Job creation reports the group
beside the Job instead, in each `StepEnvironmentStatus::exclusive_group` of
`CreatedJob::environment_statuses`, with `superseded_by` naming the member a superseded one
lost to (see [public-api.md](public-api.md)).

### EnvironmentScript (§4.1)

```rust
pub struct EnvironmentScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}
```

### EmbeddedFile (§6)

```rust
pub struct EmbeddedFile {
    pub name: String,
    pub file_type: String,                    // "type" field; must be "TEXT"
    pub filename: Option<String>,             // Plain string (not @fmtstring)
    pub data: Option<FormatString>,
    pub runnable: Option<bool>,
    pub end_of_line: Option<String>,          // FEATURE_BUNDLE_1: "LF", "CRLF", "AUTO"
}
```

## Service (§9, `SERVICE` extension, RFC 0009)

A Service is a long-lived process with named TCP or UDP ports that a scheduler starts before any
Task of a Step in its scope is scheduled and keeps running until no such Task remains. Its
*scope* is the set of Steps whose Tasks depend on it, computed from the `dependencies` of the
template's Steps, Services, and Job Environments (§9.1, [below](#service-scope-91)). The types below are the unresolved template
shapes; the `Service.*` format-string scope and the `WrappedService.*` wrap-hook variables are
validated by pass 8 (see [validation.md](validation.md), "Service scopes"), the runtime-facing
symbol builders live in `job::service_symbols`, and job creation produces `job::Service` (see
[job-types.md](job-types.md) and [job-creation.md](job-creation.md)). A template that lists
`SERVICE` must also list `EXPR` and `ENV_PARAMS` (`SERVICE requires EXPR and ENV_PARAMS; …`, see
[validation.md](validation.md)).

```rust
pub struct Service {
    pub name: String,                                    // <ServiceName> §9.2: identifier, not "File"
    pub description: Option<Description>,
    pub let_bindings: Option<Vec<String>>,               // "let" field in YAML (EXPR); may read Job.EnvNames
    pub dependencies: Option<Vec<StepDependency>>,       // §9 item 4: Steps (dependsOn), Services (service), Job Environments (environment, Job Template only)
    pub host_requirements: Option<HostRequirements>,     // same type as StepTemplate's; combined at job creation with entered Environments'
    pub ports: Vec<ServicePort>,                         // 1–10, unique names; no two of one protocol share a number
    pub health_check: Option<ServiceHealthCheck>,        // None = { type: TCP_CONNECT } on every TCP port
    pub restart_policy: Option<ServiceRestartPolicy>,    // None = { maxRestarts: 0 }
    pub variables: Option<HashMap<String, FormatString>>, // same schema as Environment.variables
    pub script: ServiceScript,
}

impl Service {
    pub fn health_check(&self) -> ServiceHealthCheck;        // declared, or the §9 default
    pub fn restart_policy(&self) -> ServiceRestartPolicy;    // declared, or the §9 default
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
    pub fn tcp_port_names(&self) -> impl Iterator<Item = &str>;  // the ports a defaulted TCP_CONNECT probes
}
```

`name` (and `ServicePort::name`) is a plain `String` rather than `Identifier` so that the
identifier, length, and `File` constraints are reported with a field path by the validation
pipeline instead of as a serde error.

`dependencies` has the shape of a Step's ([StepDependency](#stepdependency-32)) and lists Steps
and Services in one list: the Service starts only after every listed Step (`dependsOn`) has
completed and every listed Service (`service`, an inline Service or an `externalServices` name) is
READY, in addition to its other start conditions, and it is stopped before any Service it lists.
The Service sees `Service.X.<port>.port` and `.connectAddress` of another inline Service `X` only
when it lists `service: X`. On a Job Template, pass 12 rejects at `services[k] -> dependencies` an
empty list (`must not be empty.`), and at `services[k] -> dependencies[j]` a malformed entry (both
keys or neither), an unknown Step (`dependency 'Nope' not found.`, with the `did you mean 'service:
Nope'?` hint when a Service of that name exists), an unknown Service (`dependency 'service: Nope'
not found: no Service of that name in services or externalServices.`, or the mirror hint when a
Step of that name exists), the Service itself (`cannot depend on itself.`), and a duplicate entry
(`duplicate dependency '…'.`); a cycle in the combined graph — which includes an edge from each
Step and each Service whose Sessions enter a Job Environment to each Service it lists — is
reported at the root (`dependencies contain a cycle: <path> (…).`, naming the Environment whose
entering closed the cycle). On an Environment
Template only `service` entries naming a Service of the same document are valid (§9 item 4, §3.2
constraints 1–3, §9.1 rule 4).

In a Job Template, an `environment:` entry requests a Job Environment for the Service's Sessions
exactly as a Step's does (RFC 0009 `<Service>` items 3–4, RFC 0010 §3.2): `required`, `enableIf`,
and `parameters` have the same meaning, the Service's effective values are computed by RFC 0010
§3.2.2 applied to the Service, and RFC 0010 constraints 5–7 apply to it, constraint 7 (required
parameters) for every Environment its Sessions enter. The entry does not delay the Service's
start and is not part of the Step/Service graph except through the Services that Environment
lists (above).

A `<Service>` has no `serviceEnvironments` property: the RFC rejected a Service-scoped
Environment list (Rejected Ideas, "`serviceEnvironments`, a Service-scoped Environment list"), so
the key is rejected as an unknown field (with the hint that a Service sets up its host in
`onEnter` or requests a Job Environment with an `environment:` entry). A Service belongs to no
Step: it is provisioned by the Job Environments its Sessions enter (see [Which Sessions enter a
Job Environment](#environment-4)) and by its `onEnter`, never by a Step's `stepEnvironments`.

### Service scope (§9.1)

`template::service_scope` computes the scope of every Service of a Job Template from the
`dependencies` of its Steps, Services, and Job Environments, and which Job Environments each
entity's Sessions enter, as far as both are known at template validation:

```rust
pub enum ServiceScope {               // Serialize/Deserialize: {"kind":"allSteps"} | {"kind":"steps","steps":[..]}
    AllSteps,
    Steps { steps: BTreeSet<String> }, // empty = no Step known to be in scope
}
impl ServiceScope {
    pub fn steps(names) -> Self;  pub fn all_steps_default() -> Self;
    pub fn is_all_steps(&self) -> bool;  pub fn contains(&self, step: &str) -> bool;
    pub fn step_names(&self) -> Option<&BTreeSet<String>>;
}
impl Display for ServiceScope;        // "every Step" | "Step A" | "Steps A, B" | "no Step" (empty)

pub enum Entrance { Enters, DoesNotEnter, Undetermined }   // Copy; may_enter() = != DoesNotEnter
pub struct Entrances {                // Entrances::of(&JobTemplate)
    pub steps: Vec<Vec<Entrance>>,    // [step i][job environment e]
    pub services: Vec<Vec<Entrance>>, // [service k][job environment e]
}

pub struct ComputedServiceScope {
    pub name: String,
    pub scope: ServiceScope,                   // Steps known at template validation to be in scope
    pub possibly_used: bool,                   // false: no Step can be in scope, whatever any enableIf is
    pub depends_on_services: BTreeSet<String>, // inline Services it lists with `service`; sorted,
                                               // never itself, a required Service, or an undeclared name
    pub depends_on_steps: Vec<String>,         // Steps it lists, list order (undeclared names kept)
    pub dependent_steps: Vec<String>,          // Steps listing it (rule 1), template order
    pub dependent_services: Vec<String>,       // Services listing it (rule 2), template order
    pub listing_environments: Vec<String>,     // Job Environments listing it (rule 3), template order
    pub listed_by_job_environment: bool,       // rule 3: one every Step is known to enter lists it
}
impl ComputedServiceScope {
    pub fn is_unused(&self) -> bool;           // rule 4: !possibly_used
}
pub struct ServiceScopes { .. }           // get(name), iter(), scope_of(name) (empty for an undeclared name),
                                          // required_service_scopes(), requirement_possibly_used(name)
pub struct CycleViaEnvironment {
    pub from: String,         // "Step X" | "Service X": the node whose Sessions enter the Environment
    pub environment: String,  // "Job Environment 'E'", or "Environment 'E'" (with document when attached)
    pub service: String,      // the Service the Environment lists
}
pub struct ServiceDependencyCycle {
    pub path: Vec<String>,                          // "Step Use", "Service Indexer", "Step Use"
    pub via_environment: Option<CycleViaEnvironment>, // the first edge that entering an Environment added
}
// Display: "dependencies contain a cycle: Step Use -> Service Indexer -> Step Use." — or, via an
// Environment, "dependencies contain a cycle: Step Prep -> Service X -> Step Prep (the Sessions of
// Step Prep enter Job Environment 'E', which lists Service 'X', so Step Prep depends on it; Template
// Schemas §3.2 constraint 3)." — or, for the edge X -> X, "dependencies contain a cycle: Service X ->
// Service X (the Sessions of Service X enter Job Environment 'E', which lists Service 'X', so it
// would wait for itself to be READY. There is no exception for an Environment that lists the
// Service: keep the Service's Sessions out of it: drop any 'environment:' dependency of the Service
// that requests it, and decline a BY_DEFAULT one with a disabled 'environment:' dependency of the
// Service, or make it ON_REQUEST and request it only from the Steps that need it; Template Schemas
// §3.2 constraint 3)."

pub fn compute_service_scopes(jt: &JobTemplate) -> Result<ServiceScopes, ServiceDependencyCycle>;
pub fn environment_template_service_cycle(et: &EnvironmentTemplate) -> Option<ServiceDependencyCycle>;
pub fn listed_services<'a>(dependencies: Option<&'a [StepDependency]>, services: &'a [Service])
    -> impl Iterator<Item = &'a Service> + Clone + 'a;
pub fn listed_requirements<'a>(dependencies: Option<&'a [StepDependency]>, requirements: &'a [ServiceRequirement])
    -> impl Iterator<Item = &'a ServiceRequirement> + Clone + 'a;

// Reference extraction (`template::service_scope::…`, not re-exported from `template`):
pub fn step_references(step: &StepTemplate) -> BTreeSet<String>;
pub fn environment_references(env: &Environment) -> BTreeSet<String>;
pub fn environment_references_service(env: &Environment) -> bool;
pub fn service_references(svc: &Service) -> BTreeSet<String>;
```

`Entrances::of` decides whether each Step and Service enters each `jobEnvironments` entry (see
[Which Sessions enter a Job Environment](#environment-4)), by one rule for both. An entry whose
`enableIf` is `false` requests nothing and declines a `BY_DEFAULT` Environment (RFC 0010 §3.2
item 3.2), so the entity does not enter it; an `enableIf` on the entry or on the Environment that
is not known (§7.6 rule 5; an expression whose value is known counts as that value) makes a
possible entrance `Undetermined`, and so does an `exclusiveGroup`, since an attached
Environment of the group may come first.

The four rules: (1) a Step that lists Service `X` in its `dependencies` is in `X`'s scope;
(2) when Service `Y` depends on Service `X` — lists it, or `Y`'s Sessions enter a Job Environment
that lists it — every Step in `Y`'s scope is in `X`'s scope, transitively through any chain of
Services; (3) when a `jobEnvironments` entry lists Service `X` in its `dependencies`, every Step
whose Sessions enter it is in `X`'s scope — every Step for an `ALWAYS` Environment, every Step that does not
decline it for a `BY_DEFAULT` one, the requesting Steps for an `ON_REQUEST` one, less any for which its
`enableIf` is `false`; (4) a Service in whose
scope no Step can fall is unused, and pass 12 rejects the template at `services[k]` naming it
(`Service 'Cache' is unused: no Step, Service, or Job Environment lists it.`; for one listed only
by an `ON_REQUEST` Environment no Step requests, `Service 'X' is unused: no Step is in its scope
(Job Environment 'E' lists it, but no Step's Sessions enter it).`). Where an `enableIf` not known (§7.6
rule 5) decides a scope member, `scope` holds only the Steps known to be in it, `possibly_used` stays
true, and job creation computes the final scope. A `service` entry
naming an `externalServices` name is not an edge among the inline Services and places nothing in
an inline Service's scope; it contributes to the scope the entry confers on the attached Service it
is matched to (`ServiceScopes::required_service_scopes`, §9.1 over the required name), and an entry
that nothing lists is rejected like an unused inline Service (rule 4). An attached Service's scope
is computed over the combined Job at submission; one nothing lists is never started.

The `dependencies` of the template's Steps, Services, and Job Environments form one graph —
Step-to-Step, Step-to-Service, Service-to-Step and Service-to-Service edges, plus an edge from
each Step and each Service whose Sessions enter a Job Environment to each Service that
Environment lists — that must be acyclic (§3.2 constraint 3). So a Service an `ALWAYS` or `BY_DEFAULT` Job
Environment lists, or any Service it depends on transitively, cannot depend on a Step whose
Sessions enter that Environment (for `BY_DEFAULT`, a Step that does not decline it), and a
Service whose Sessions enter an Environment that lists it, by its `activation` or through its
own `environment:` entry, is the cycle `X -> X`.
The entering edges are not counted among the Steps that list a Service (`dependent_steps`).
`compute_service_scopes` reports the first cycle whose edges are all known (every `enableIf` they
depend on known by §7.6 rule 5, and no Environment in an `exclusiveGroup`) as a
`ServiceDependencyCycle`, whose `path` spells each node
as `Step X` or `Service X` and starts and ends with the same node, and whose `via_environment`
names the first entering edge; pass 12 reports it at the root path. A cycle that depends on an
`enableIf` not known or an `exclusiveGroup`, or that runs through an attached Environment
Template, is reported at job creation (`job/create_job/composition.rs`). `environment_template_service_cycle` finds a cycle
among the Service dependencies of an Environment Template's Services (which have no Steps); pass
12 reports it at `services`.

`listed_services` yields, in declaration order, the Services of `services` that `dependencies`
lists with the `service` key — those whose `Service.<name>.<port>.*` the listing Step, Service, or
Environment may reference; a required name or a typo yields nothing. `listed_requirements` is
its counterpart over a Job Template's `externalServices`: the required Services the entity lists,
whose `port` / `connectAddress` it may reference (§9.8 item 2); an inline name or a typo yields
nothing. Pass 8 seeds a Step's script and `stepEnvironments` (over the Step's list), a Service's
own fields (over its list), a Job Environment (over its own list, both helpers), and an
Environment Template's `environment` (over its own list, `listed_services` only) with them, and
job creation passes the same sets as the entity's in-scope sets. Listing a required Service
makes its values visible and puts the lister in the matched Service's scope.

`Service.*` values are available exactly to the entities that list the Service, so a reference is
never an implicit edge, and never an implicit dependency. The reference extraction helpers serve
the pass 8 diagnostic (`service_diagnostics.rs`) that names the missing `service: <name>` entry
when a Step, Service, or Environment references a Service it does not list;
`environment_references_service` backs `Environment::references_service`. Only
the fields that may legally
reference `Service.*` are read: a reference in a job-creation field (a `<StepTemplate>`'s or
`<Service>`'s `let`, `hostRequirements`, a numeric `@fmtstring`) is a pass 8 error and places
nothing in a scope. Job creation records each Service's scope on `job::Service` (see
[job-types.md](job-types.md)).

### ServiceRequirement (§9.8) and ServiceRequirementPort (§9.8.1)

```rust
pub struct ServiceRequirement {
    pub name: String,                            // <ServiceName>; not an inline Service's name
    pub ports: Vec<ServiceRequirementPort>,      // 1–10, unique names
}
impl ServiceRequirement { pub fn port_names(&self) -> impl Iterator<Item = &str>; }

pub struct ServiceRequirementPort {
    pub name: String,                            // identifier, not "File"
    #[serde(default)]
    pub protocol: ServicePortProtocol,           // TCP (default) | UDP
}
```

A requirement makes `Service.<name>.<port>.port` and `.connectAddress` available, for each
declared port, under the same rule as an inline Service's ports (§9 scope rules 2–4, §9.8 item 2):
to the `script` and `stepEnvironments` of a Step that lists `service: <name>` in its
`dependencies`, to an inline Service that lists it, and to a `jobEnvironments` entry that lists
it. A Step, Service, or Job Environment that references a required Service without listing it is
rejected with the same message as for an inline Service (see [validation.md](validation.md),
"Scope-rule diagnostics"); `bindAddress` is never in scope. Listing a required Service makes
its values visible and puts the lister in the matched Service's scope. An entry that no Step,
Service, or Job Environment lists is rejected at template validation
(`external Service '<name>' is unused: no Step, Service, or Job Environment lists it.`), exactly
as an unused inline Service is. At submission `create_job_with_environment_templates` matches it to
exactly one attached Service of that name declaring every listed port with the same protocol (see
[job-creation.md](job-creation.md), "Services from Environment Templates"). Permitted only in a Job
Template.

### ServicePort (§9.3)

```rust
pub struct ServicePort {
    pub name: String,                 // identifier, not "File", unique within the Service
    pub port: Option<FormatString>,   // <posinteger> | <posintstring>, 1–65535 in the protocol's space; None = runtime allocates
    #[serde(default)]
    pub protocol: ServicePortProtocol, // §9.3 item 3: TCP (default) | UDP; a literal, not @fmtstring
}

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServicePortProtocol { #[default] Tcp, Udp }   // Copy, Ord, Hash, Serialize, Display

impl ServicePortProtocol {
    pub fn as_str(&self) -> &'static str;   // "TCP" | "UDP" — also the WrappedService.Protocols spelling
    pub fn is_default(&self) -> bool;       // for skip_serializing_if on job types
}
```

`protocol` is an enum literal, so `udp`, `SCTP`, or a format string such as `"{{ Param.Proto
}}"` is a serde `unknown variant` error, as a bad `completedTasks` is. TCP and UDP numbers are
separate spaces: two ports may give the same `port` when their protocols differ, and a number
is requested or allocated in the space of its protocol (see [validation.md](validation.md),
pass 12, and [job-creation.md](job-creation.md), "Services"). A UDP port cannot be probed by
`TCP_CONNECT`.

### Numeric `@fmtstring` fields

`ServicePort::port`, `ServiceHealthCheck`'s four numeric fields (`readinessIntervalSeconds`,
`readinessTimeoutSeconds`, `healthIntervalSeconds`, `failureThreshold`), and
`ServiceRestartPolicy::max_restarts` are `<posinteger> | <posintstring>` (or `<integer> |
<intstring>`) marked `@fmtstring`. They are modeled exactly like `<Action>.timeout`: the field
is an `Option<FormatString>`, a YAML integer is accepted and held as its decimal text, and a
format string is kept unevaluated. Unlike `<Action>.timeout`, a format string here is admitted
by `SERVICE` itself (the spec marks the fields `@fmtstring`), not by `FEATURE_BUNDLE_1`.
Validation range-checks a value that carries no expression (see [validation.md](validation.md),
pass 12) and statically checks one that does (pass 8, target type `int?`); job creation
resolves every value in the `<Service>.let` scope with the same target — a `null` result means
the field was not provided and the §9 default applies — and range-checks the result (see
[job-creation.md](job-creation.md), "Services").

### ServiceHealthCheck (§9.4)

One probe mechanism applied in two phases. Before the instance is READY the probe decides
readiness: the first probe runs as soon as `onRun` is launched, one every
`readinessIntervalSeconds` after it, and the first success makes the instance READY;
`readinessTimeoutSeconds`, measured from the launch of `onRun`, bounds the phase. After READY the
probe decides health: one every `healthIntervalSeconds`, and `failureThreshold` consecutive
failures make the instance UNHEALTHY (an instance failure — the runtime's concern; see
`specs/sessions/service-session.md`). Intervals are measured from the end of the previous probe.

A discriminated union on `type`, derived with `#[serde(tag = "type", deny_unknown_fields)]`, so
a field belonging to another variant is a deserialization error: `ports` on anything but
`TCP_CONNECT`, and `readinessIntervalSeconds` on `STDOUT`, whose ready line "arrives when it
arrives" (§9.4 item 3 requires a `STDOUT` check that gives it to be rejected). The pre-revision
spellings `timeoutSeconds` and `intervalSeconds` are unknown fields on every variant, as
`readinessCheck` is on `<Service>` and `onReadinessCheck` on `<ServiceActions>`.

```rust
pub enum ServiceHealthCheck {
    TcpConnect {                                          // "TCP_CONNECT"
        ports: Option<Vec<String>>,                       // TCP ports only; None = every TCP port
        readiness_interval_seconds: Option<FormatString>, // default 1
        readiness_timeout_seconds: Option<FormatString>,      // default 300
        health_interval_seconds: Option<FormatString>,    // default 30
        failure_threshold: Option<FormatString>,          // default 3
    },
    Command {                                             // "COMMAND"
        readiness_interval_seconds: Option<FormatString>, // default 5
        readiness_timeout_seconds: Option<FormatString>,      // default 300
        health_interval_seconds: Option<FormatString>,    // default 30
        failure_threshold: Option<FormatString>,          // default 3
    },
    Stdout {                                              // "STDOUT"
        readiness_timeout_seconds: Option<FormatString>,      // default 300
        health_interval_seconds: Option<FormatString>,    // no default: None = no heartbeat expected
        failure_threshold: Option<FormatString>,          // default 3; only with health_interval_seconds
    },
}

pub const SERVICE_HEALTH_CHECK_NUMERIC_FIELDS: [&str; 4] =
    ["readinessIntervalSeconds", "readinessTimeoutSeconds", "healthIntervalSeconds", "failureThreshold"];

impl ServiceHealthCheck {
    pub const DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS: u64 = 1;
    pub const DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS: u64 = 5;
    pub const DEFAULT_READY_TIMEOUT_SECONDS: u64 = 300;
    pub const DEFAULT_HEALTH_INTERVAL_SECONDS: u64 = 30;   // TCP_CONNECT and COMMAND only
    pub const DEFAULT_FAILURE_THRESHOLD: u64 = 3;
    pub fn type_name(&self) -> &'static str;                 // "TCP_CONNECT" | "COMMAND" | "STDOUT"
    pub fn readiness_interval_seconds(&self) -> Option<&FormatString>;  // always None for STDOUT
    pub fn readiness_timeout_seconds(&self) -> Option<&FormatString>;
    pub fn health_interval_seconds(&self) -> Option<&FormatString>;
    pub fn failure_threshold(&self) -> Option<&FormatString>;
    /// The four fields in SERVICE_HEALTH_CHECK_NUMERIC_FIELDS order, so validators
    /// and job creation treat them alike (every one is a <posinteger>).
    pub fn numeric_fields(&self) -> [(&'static str, Option<&FormatString>); 4];
}

impl Default for ServiceHealthCheck;  // TcpConnect with every field None
```

### ServiceRestartPolicy (§9.5)

```rust
pub struct ServiceRestartPolicy {
    pub max_restarts: Option<FormatString>,              // <integer> | <intstring>, >= 0
    pub completed_tasks: Option<CompletedTasksPolicy>,   // no default; required when maxRestarts > 0
}

impl ServiceRestartPolicy {
    pub const DEFAULT_MAX_RESTARTS: i64 = 0;
}

impl Default for ServiceRestartPolicy;  // both fields None: `{ maxRestarts: 0 }`

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompletedTasksPolicy {
    Keep,
    Rerun,
}

impl CompletedTasksPolicy {
    pub fn as_str(&self) -> &'static str;  // "KEEP" | "RERUN"
}
```

`completedTasks` has no default (§9.5 item 2): when `maxRestarts` is greater than 0
it must be provided — a template that allows relaunch must say what a relaunch means for completed
Tasks. Pass 12 checks a literal `maxRestarts` at `services[k] -> restartPolicy`
(`completedTasks must be provided when maxRestarts is greater than 0 (maxRestarts is 2): a
template that allows relaunch must say what a relaunch means for completed Tasks, KEEP or RERUN
(Template Schemas §9.5 item 2).`); a format-string `maxRestarts` is checked when it is resolved at
job creation, with the same message. When `maxRestarts` is 0 the field may be omitted: no relaunch
happens, and the value matters only if the Service is stopped and started again because a Service
it lists began a new Service Session, where an omitted value is read as `RERUN`
(`job::ServiceRestartPolicy::completed_tasks_on_dependent_restart`, see
[job-types.md](job-types.md)). `maxRestarts` counts relaunches, not launches; the earlier name
`maxAttempts` is an unknown property (`deny_unknown_fields`). When the Service becomes FAILED
(its restarts exhausted), every Task in its scope that has not completed successfully fails
without a Task retry, and completed Tasks keep their result; a `RERUN` requeue never consumes a
Task retry (RFC 0009 "Failure and restart" step 4) — the runtime's rule, which no model type
records.

### ServiceScript (§9.6) and ServiceActions (§9.7)

```rust
pub struct ServiceScript {
    pub let_bindings: Option<Vec<String>>,       // "let" field in YAML (EXPR)
    pub actions: ServiceActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct ServiceActions {
    pub on_enter: Option<Action>,
    pub on_run: Action,                          // required: the process that is the service
    pub on_health_check: Option<Action>,         // iff healthCheck.type is COMMAND
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    pub const ON_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    /// RFC 0009 `<Action>` default-timeout table: onEnter None, onRun None,
    /// onHealthCheck Some(30), onExit Some(300); None for any other name.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64>;
    pub fn named_slots(&self) -> [(&'static str, Option<&Action>); 4];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
}
```

The model crate does not apply action default timeouts itself — the sessions runtime does, as
it does for an Environment's `onExit` — so the Service defaults are recorded here as constants
for the runtime to consume.

## Actions (§5)

```rust
pub struct Action {
    pub command: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub cancelation: Option<CancelationMode>,
    pub timeout: Option<FormatString>,
}

pub struct StepActions {
    pub on_run: Action,
}

pub struct EnvironmentActions {
    pub on_enter: Option<Action>,
    pub on_wrap_env_enter: Option<Action>,             // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_task_run: Option<Action>,              // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_env_exit: Option<Action>,              // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_service_enter: Option<Action>,         // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_run: Option<Action>,           // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_health_check: Option<Action>, // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_exit: Option<Action>,          // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_exit: Option<Action>,
}

impl EnvironmentActions {
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    pub const ON_WRAP_SERVICE_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    /// §5 timeout table: onExit/onWrapEnvExit/onWrapServiceExit → Some(300),
    /// onWrapServiceHealthCheck → Some(30), every other slot → None.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64>;
    /// The four RFC 0009 hooks, in lifecycle order.
    pub fn service_wrap_hooks(&self) -> [(&'static str, &Option<Action>); 4];
    pub fn has_any_service_wrap_hook(&self) -> bool;

    // Generated by `impl_environment_actions_helpers!` (shared with job::EnvironmentActions):
    pub fn named_slots(&self) -> [(&'static str, &Option<Action>); 9];   // declaration order
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
    pub fn wrap_hooks(&self) -> [(&'static str, &Option<Action>, WrapHookScope); 7];
    pub fn has_any_action(&self) -> bool;
    pub fn has_any_wrap_hook(&self) -> bool;   // the definition of a *wrapping* Environment
}

pub enum WrapHookScope {
    EnvName,   // `WrappedEnv.Name`  — onWrapEnvEnter, onWrapEnvExit
    StepName,  // `WrappedStep.Name` — onWrapTaskRun
    Service,   // `WrappedService.*` — the four onWrapService* hooks
}
```

The slots are enumerated once per struct in the `impl_environment_actions_helpers!` invocation
(`slots: [...]`, `wrap_hooks: [...]`); the array lengths above are derived from those lists.
The job-side `job::EnvironmentActions` is invoked with the same nine slots and seven hooks, and
`convert_environment` carries the RFC 0009 hooks and `dependencies` across (see
[job-types.md](job-types.md)), for a Job Environment and a Step Environment alike.

Which wrap hooks a wrapping Environment must define follows from which kinds of Session enter it
(RFC 0009 wrap hook rule 1): `onWrapEnvEnter`, `onWrapTaskRun`, and `onWrapEnvExit` if any Task
Session enters it, and `onWrapEnvEnter`, `onWrapEnvExit`, and the four `onWrapService*` hooks if
any Service Session enters it; extra hooks are permitted, so a `stepEnvironments` wrapper needs
the three Task hooks and may define Service hooks. Pass 10 checks each kind of Session where an
entity of that kind is known to enter (`Entrances`), and job creation checks the rest, including an
attached wrapping Environment that a Service with a non-empty scope enters (see
[validation.md](validation.md) and [job-creation.md](job-creation.md)).

The wrap-hook default timeouts follow the wrapped action: `onWrapEnvExit` takes `onExit`'s 300
seconds (sessions `env_script.rs`), and by the same rule `onWrapServiceExit` takes
`<ServiceActions>.onExit`'s 300 seconds and `onWrapServiceHealthCheck` takes
`onHealthCheck`'s 30 seconds. The spec's §5 table lists the `<ServiceActions>` defaults but
has no rows for the `onWrapService*` hooks; the values here are the analogy, recorded as
constants for the runtime to consume.

### CancelationMode

Discriminated union on the `mode` field, implemented as a Rust enum with a custom
`Deserialize` impl:

```rust
pub enum CancelationMode {
    Terminate,
    NotifyThenTerminate {
        notify_period_in_seconds: Option<FormatString>,
    },
    DeferredMode {
        mode: FormatString,
        notify_period_in_seconds: Option<FormatString>,
    },
}
```

The `Terminate` variant rejects any extra fields. The `NotifyThenTerminate` variant
accepts an optional `notifyPeriodInSeconds` field. An explicit JSON/YAML `null` for
`notifyPeriodInSeconds` is treated the same as omitting the field, matching the
Python implementation (pydantic `Optional`).

#### DeferredMode: why the mode decision can be deferred

Format strings in general are already delay-processed: when a template says
`args: ["{{WrappedAction.Command}}"]`, the parser just stores "this is a format
string" and the value gets resolved much later, inside a running session, right
before the action launches — that's when the runtime seeds the `WrappedAction.*`
variables from the action being wrapped. "Resolve later" is the normal pipeline
for every other field.

`mode` is different because it isn't a normal value field — it's the *schema
selector*. The parser needs to know TERMINATE vs NOTIFY_THEN_TERMINATE at parse
time to decide what shape of object it's even reading (only one of them allows
`notifyPeriodInSeconds`). So the "which shape?" decision happens at parse time,
but a forwarded value like `mode: "{{WrappedAction.Cancelation.Mode}}"` only
exists at run time — that mismatch made round-trip cancelation forwarding in
RFC 0008 wrap hooks impossible (the parser rejected the template with "unknown
variant").

`DeferredMode` resolves the mismatch: the parser accepts a format string in
`mode` as a third, "decided later" state (gated on the FEATURE_BUNDLE_1
extension), and the shape decision moves to resolution time, right before the
action runs:

1. The runtime seeds `WrappedAction.Cancelation.Mode` from the wrapped action
   (`"TERMINATE"`, `"NOTIFY_THEN_TERMINATE"`, or null).
2. It resolves the `mode:` expression against that.
3. `"TERMINATE"`/`"NOTIFY_THEN_TERMINATE"` — the cancelation block now acts as
   that method, and its sibling fields are validated against that shape. Null
   (whole-field expressions only) — the whole `cancelation:` block is treated
   as never written. Anything else — the action fails.

Static validation is *not* deferred: at parse time the validator still checks
the expression is well-formed and that `WrappedAction.*` is only referenced
inside wrap hooks. A wrap hook's `timeout` and `cancelation` fields validate
against the same scope as its `command` and `args` — `WrappedAction.*` plus the
hook's companion group (`WrappedEnv.Name`, `WrappedStep.Name`, or the RFC 0009
`WrappedService.*`) — because the runtime resolves all of a hook's fields
against one symbol table. Any format string is accepted — normal interpolation like
`"{{Prefix}}_THEN_TERMINATE"` is permitted; only the resolved value is
constrained. You just can't know *which* of the two modes it'll be until the
wrapped action is in front of you — which is inherent to forwarding: the same
wrap environment gets reused across many steps whose cancelation settings
differ.

The run-time resolution lives in `openjd-sessions`
(`resolve_effective_cancelation` in `runner/mod.rs`). See openjd-specifications
Template Schemas §5.3 and RFC 0008 "Cancelation behavior" for the normative
rules.

## StepScript (§3.5)

```rust
pub struct StepScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}
```

## Host Requirements (§3.3)

```rust
pub struct HostRequirements {
    pub amounts: Option<Vec<AmountRequirement>>,
    pub attributes: Option<Vec<AttributeRequirement>>,
}

pub struct AmountRequirement {
    pub name: FormatString,
    pub min: Option<FormatString>,
    pub max: Option<FormatString>,
}

pub struct AttributeRequirement {
    pub name: FormatString,
    pub any_of: Option<AttributeValues>,
    pub all_of: Option<AttributeValues>,
}

pub enum AttributeValues {
    List(Vec<FormatString>),   // the list form
    Expression(FormatString),  // ENV_PARAMS: one {{ }} expression, list[string]?
}
```

`name` is `@fmtstring` (§3.3.1 / §3.3.2): it is resolved at job creation, and
the §3.3.1.1 / §3.3.2.1 constraints apply to the resolved name. The job types
carry the resolved name as a `String`.

`anyOf` and `allOf` deserialize as `AttributeValues`: a YAML/JSON list is `List`,
and a string is `Expression`, which template validation accepts only in a
document that lists ENV_PARAMS and only when it is exactly one `{{ }}` expression
with no other text (a `<ListExpressionString>`, §3.3.2). Any other JSON type is a
decode error ("expected a list of values, or (with ENV_PARAMS) a string holding one
{{ }} expression"). `AttributeValues::as_list` gives the list form's items,
and `is_literal` says whether the value is known without resolution (a list of
literals).

`AmountRequirement::may_be_discarded` and `AttributeRequirement::may_be_discarded`
say whether job creation may discard the requirement in a document that lists
ENV_PARAMS (§3.3), judged from its text: an amount whose every bound is a format
string with expressions, and an attribute whose every field is the expression
form. The answer is conservative (`{{ 2 }}` counts as a bound that may be `null`)
and means nothing without ENV_PARAMS, where nothing is discarded. Structural
validation uses them to compare only requirements that are certainly kept when it
checks for duplicate names; pass 8 refines this with the expressions' static
types. In such a document an empty `HostRequirements`, or
empty lists, are valid and mean no requirements.

## Task Parameter Space (§3.4)

### StepParameterSpaceDefinition

```rust
pub struct StepParameterSpaceDefinition {
    pub task_parameter_definitions: Vec<TaskParameterDefinition>,
    pub combination: Option<String>,
}
```

### TaskParameterDefinition

Discriminated union via `#[serde(tag = "type")]`. Variant names use SCREAMING_CASE to
match the serde tag values directly, with `#[serde(rename = "CHUNK[INT]")]` on `CHUNK_INT`
since brackets aren't valid in Rust identifiers:

| Variant | Type Field | Range Type | Extra Fields |
|---------|-----------|------------|-------------|
| `INT` | `"INT"` | `IntRange` | — |
| `FLOAT` | `"FLOAT"` | `FloatRange` | — |
| `STRING` | `"STRING"` | `StringRange` | — |
| `PATH` | `"PATH"` | `StringRange` | — |
| `CHUNK_INT` | `"CHUNK[INT]"` | `IntRange` | `chunks: ChunksDefinition` |

### Range Types

Ranges accept either a list of values or a range expression string:

```rust
pub enum IntRange {
    List(Vec<IntOrFormatString>),
    Expression(FormatString),
}

pub enum StringRange {
    List(Vec<FormatString>),
    Expression(FormatString),
}

pub enum FloatRange {
    List(Vec<FloatRangeItem>),
    Expression(FormatString),
}
```

`IntRange::List` holds `IntOrFormatString` elements (Template Schemas §3.4.1.1
`<IntRangeList>`, shared by `CHUNK[INT]`): a YAML integer, an integer-valued float, or a
string that parses as an integer becomes `Int`; a string containing `{{` becomes a
`FormatString` (a `<TaskParameterStringValue>`), resolved at job creation, and a
malformed one is a decode error. Any other string is a decode error
(`Cannot parse 'abc' as integer`).

`FloatRange::List` uses `FloatRangeItem` — an enum that accepts either a plain `f64` or
a `FormatString` — to handle YAML float edge cases and format string interpolation in
float ranges:

```rust
pub enum FloatRangeItem {
    Float(f64),
    FormatString(FormatString),
}
```

### ChunksDefinition

```rust
pub struct ChunksDefinition {
    pub default_task_count: IntOrFormatString,
    pub target_runtime_seconds: Option<IntOrFormatString>,
    pub range_constraint: RangeConstraint,  // Required field
}

pub enum IntOrFormatString {
    Int(i64),
    FormatString(FormatString),
}

pub enum RangeConstraint {
    Contiguous,
    Noncontiguous,
}
```

## Flexible Deserialization Types

Several wrapper types handle YAML's flexible value representations:

| Type | Accepts | Rejects | Purpose |
|------|---------|---------|--------|
| `FlexInt(i64)` | Integers, floats with `.0`, strings of integers | Bools, nulls | INT parameter defaults/constraints |
| `FlexFloat(f64, Option<String>)` | Numbers, string representations | Bools, nulls | FLOAT parameter defaults/constraints |
| `FlexUint(u64)` | Non-negative integers, string representations | Negatives, bools | Timeout values |
| `BoolValue(bool)` | `true`/`false`, `0`/`1`, `"yes"`/`"no"`, `"on"`/`"off"` | Other strings | BOOL parameter defaults |
| `NullableVec<T>` | Absent field, list of T | Explicit `null` | INT/FLOAT `allowedValues` |

`FlexFloat` preserves the original string representation when parsed from a string, which
is needed for round-trip fidelity in constraint checking.

`NullableVec` exists because the spec distinguishes between an absent `allowedValues` field
(no constraint) and an explicit `null` (invalid). Serde's `Option<Vec<T>>` would accept both.
