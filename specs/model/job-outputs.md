# Job Outputs (`CONTROL_FLOW`)

A Job's outputs are the typed values it reports back to its submitter and other callers
(RFC 0011 §Job outputs, Template Schemas §2.17). Each is declared in the Job Template's
`jobOutputDefinitions` with a `value` expression over the Job's parameters and its Steps'
outputs, and is evaluated as soon as every `Steps.*` symbol it reads is concrete. A Task
never writes a job output. Job outputs are not symbols in any format string of the
template.

This document covers the model's part: the schema, validation, the evaluation that
`create_job` performs, and the two functions a caller (a scheduler, or `openjd run`)
uses to evaluate the rest. Deciding when to call them, failing the Job, and marking an
output stale after a re-run are the caller's job.

## Schema

```yaml
jobOutputDefinitions:
  - name: <Identifier>
    type: <OutputVariableType>
    value: <FormatString>
    description: <Description>   # optional
    unit: <OutputVariableUnit>   # optional
    # optional constraint properties, as on <OutputVariableDefinition>
```

`template::JobTemplate::job_output_definitions` holds the list as
`template::JobOutputDefinition` values. A `JobOutputDefinition` wraps the
`JobParameterDefinition` of the declared type, as `OutputVariableDefinition` does, with
`value` in place of `collect` (see [template-types.md](template-types.md) §
JobOutputDefinition).

## Validation

**Pass 11** ([validation.md](validation.md) § Pass 11) gates the field on `CONTROL_FLOW`
and checks the structure: 1 to 50 elements; unique names, in a namespace separate from the
job parameters'; no `collect`, `default`, `userInterface`, or (`PATH` and `LIST[PATH]`)
`dataFlow`; constraint properties valid for the type; and a `unit` of 1 to 32 characters
with no control characters. `LIST[LIST[INT]]` is allowed.

**Pass 8** ([validation.md](validation.md) § The `Steps.*` Scope and `enableIf`, Job
output values) checks each `value` against the job output scope:

| Available | Not available |
|---|---|
| `Param.*` except `PATH` and `LIST[PATH]` parameters | `Param.*` of `PATH` and `LIST[PATH]` (use `RawParam.*`) |
| `RawParam.*` | `Step.Name`, `Task.*`, `Session.*` |
| `Job.Name` | names bound by `let` |
| `Steps.<P>.RawOutput.*` and `Steps.<P>.TaskRawParam.*` of every Step `P` with an `<Identifier>` name, with no dependency required | `Steps.<P>.Output.*` and `Steps.<P>.TaskParam.*` whose type is or contains `path` (use the `Raw` form) |
| `Steps.<P>.Output.*` and `Steps.<P>.TaskParam.*` of those Steps, for other types | |

The `Steps.*` symbols have the types of the RFC's §The `Steps.*` scope table, as in a
Step's fields. The target type is the declared type's expression type made optional, so
a `null` result is allowed and an unresolved `T?` from a conditional producer type-checks.
A literal value, and a value with text around an expression that is fully static, is
converted to the declared type at validation. A value with text around an expression
that is not static must have a type a string converts to, so it cannot be a `LIST[...]`
type; it is converted when it is evaluated, as is a whole-field expression.

## Evaluation

A job output is evaluated in three steps, at Job creation or through
`evaluate_job_output`, with the same code:

1. **Resolve.** The `value` resolves against the job symbol table (`Param.*`,
   `RawParam.*`, and `Job.Name` with `EXPR`) plus the `Steps.*` symbols it reads, with
   the optional declared type as target type (Expression Language §1.3.2), under the
   caller's evaluation budgets and the POSIX path format, as every job-creation
   evaluation is. A value with text around an expression resolves to a string. A `null`
   result ends here: the output has no value, which is not an error, and its constraint
   properties are not checked.
2. **Convert.** A result whose type is not the declared type is converted with EXPR's
   implicit coercion (§1.2.3), as a Step's output values are
   ([outputs.md](outputs.md)). A string converted to `path` keeps its text as written
   (RFC 0011 §Job outputs item 6). It is labelled with the POSIX path format whatever the
   submission path context, because the model holds `RawParam.*` of a `PATH` parameter
   as a string, so a `PATH` output built from it (`{{RawParam.Out}}`) goes through this
   conversion too: a caller should read the text, not the `path` value's format.
3. **Check.** The value is checked against the declared constraint properties with
   `job::OutputVariable::check_constraints`, the check a Step's outputs use. Only the
   declared properties apply.

A failure at any step is a `ModelError::ModelValidation` with one error at
`jobOutputDefinitions[k] -> value`, whose message starts `Job output '<name>': `:

| Failure | Message after the prefix |
|---|---|
| The expression fails, including `fail(...)` and a whole-field value that does not coerce | EXPR's message (`too few`, `Cannot coerce string to int?`), then the expression and a caret on the lines below it, as EXPR renders them |
| Text that does not convert | `value 'x4' does not convert to INT: Cannot convert 'x4' to int: invalid digit found in string` |
| A constraint violation | the output variable check's message with `Job output` for `Output variable`, such as `value 4 exceeds maximum 3` |

The conversion and constraint messages have no caret, so they end with a line holding the
`value` (`\n  {{Param.N}}`), which names the expression as RFC 0011 §Job outputs item 3
asks.

A message can repeat a value, which may be a job parameter's or one a Task reported:
EXPR's message for `fail(s)` is `s`, and for `int(s)` it quotes `s`; the conversion
message quotes the value and repeats it in the coercion error. Each such part (EXPR's
message line, the quoted value, the coercion error, and the trailing `value` line) is
treated as a Step output value's messages are (`outputs::quote` / `outputs::shorten`):
cut to its first 200 characters, before escaping, with `...` marking a cut, and with each
Unicode Cc control character escaped (`\n`, `\u{1b}`). So every part stays on its line and
a reported value cannot drive the terminal, though an escaped part can be longer than 200
characters. The expression and caret lines EXPR renders after its message come from the
template and are kept as written.

The size limit of RFC 0011 §Job outputs item 7 (at least 1 MiB of job output values per
Job) is not enforced by the model: a scheduler applies its own.

### At Job creation

`create_job` stores every definition in `job::Job::outputs` as a `job::JobOutput`
(`JobOutputDefinition::to_job_output`), after the Steps are built. It builds one
`StepInputs` for all of them (so a Step's `TaskParam` lists are built once, however many
outputs read them), and evaluates each output whose `Steps.*` symbols are all concrete
then, given the Steps it built:

- one that reads no `Steps.*` symbol;
- a symbol of a Step that `create_job` skipped (`null`);
- a `TaskParam` or `TaskRawParam` symbol of a Step whose Tasks are known
  (`job::Step::tasks_known()` is `Some`), whether it is bound or still pending.

Any `Output` or `RawOutput` symbol of a Step that is not skipped is not concrete at
creation, so an output that reads one stays pending with `evaluated: None`. An evaluated
output's `evaluated` is `Some(JobOutputValue::Value(v))` or `Some(JobOutputValue::Null)`.

Every job output that fails at creation fails `create_job`, as an invalid job parameter
value does, and all of them are reported together, in template order. They are evaluated
only after the Steps, so a Step error that fails `create_job` is reported without them.

### Later: `job_output_inputs` and `evaluate_job_output`

```rust
pub fn job_output_inputs(job: &job::Job, output_index: usize) -> Vec<String>;

pub fn evaluate_job_output(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    output_index: usize,
    inputs: &job::StepInputs,
) -> Result<Option<ExprValue>, ModelError>;
```

`job_output_inputs` lists the `Steps.*` symbols an output reads, as
`Steps.<Step>.<Member>.<Name>` (a property after the name, such as `.name`, is dropped),
sorted and without repeats. It is empty for an output that reads none, and for an
`output_index` past the Job's outputs. A caller evaluates the output once each symbol is
concrete in its `StepInputs` (`StepInputs::is_concrete`):

- an `Output` or `RawOutput` symbol once its Step has succeeded
  (`ProducerState::Succeeded`) or been skipped (`ProducerState::Skipped`);
- a `TaskParam` or `TaskRawParam` symbol once its Step's Tasks are known
  (`ProducerState::TasksKnown`) or it has been skipped.

`StepInputs::add_producer` does not require the producer to be a dependency of anything,
so a caller adds every Step the output reads. `evaluate_job_output` copies each symbol's
value from `inputs`, then resolves, converts, and checks as above. `Ok(None)` is a `null`
result and `Ok(Some(v))` the value; the caller stores it in `JobOutput::evaluated`, for
example with `JobOutputValue::from`. A `ModelError::ModelValidation` fails the Job. A
`ModelError::Compatibility` is a caller error that says nothing about the Job:

- `the Job has N job outputs, so it has no job output K.` (`1 job output` for one)
- `job output K of the Job is 'X', but the template's is 'Y'; pass the template the Job was created from.` (`but the template has none` when it has fewer)
- `job output 'X' reads 'Steps.P.Output.N', which is not concrete in the given inputs.`

An output evaluated at creation can be evaluated again with the same inputs and gives the
same value.

An evaluated output stays committed even if a Step it reads later fails, because the
model does not revisit it. Re-evaluating after a re-run (RFC 0011 §Job outputs item 5)
is calling `evaluate_job_output` again with the new inputs; keeping the earlier value
and marking it stale when that fails is the caller's job.

## The CLI

`openjd run` reports an output that fails at creation, as any `create_job` error. It
prints each output `create_job` evaluated, and evaluates every other one as soon as its
inputs are concrete in its scheduler simulator: after a Step's Tasks become known, or it
succeeds or is skipped. One that fails fails the run, and the end of the run lists the
outputs never evaluated with the Step each waits on ([cli/run.md](../cli/run.md) §
CONTROL_FLOW). `openjd summary` lists every output with its value or what it waits on
([cli/summary.md](../cli/summary.md) § CONTROL_FLOW).
