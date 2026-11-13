// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 11: validate or reject `CONTROL_FLOW` features (RFC 0011, RFC 0012,
//! RFC 0013).
//!
//! Five fields are gated by the `CONTROL_FLOW` extension:
//! - `jobOutputDefinitions` on the Job Template
//! - `enableIf` on `<StepTemplate>`
//! - `outputVariables` on `<StepTemplate>`
//! - `enableIf` on `<Environment>` (Job and Step Environments)
//! - `loop` on a `steps` entry, which makes it a `<StepLoop>` (RFC 0013)
//!
//! Without the extension, each of them is an error at its own path. With it,
//! this pass enforces the RFCs' static rules: RFC 0011 §Validation, "At
//! template validation", items 1, 5's Environment Template part, and the
//! structural part of 9, and RFC 0012 §Validation item 1:
//!
//! - **EXPR prerequisite.** A template that lists `CONTROL_FLOW` must also
//!   list `EXPR`.
//! - **`outputVariables` structure.** 1 to 32 elements with unique names; no
//!   `default`, `userInterface`, or `dataFlow`; constraint properties valid
//!   for the type, with the job-parameter rules; `collect: APPEND` only with
//!   a `LIST[...]` type; a `unit` of 1 to 32 characters with no Unicode Cc
//!   characters; no `LIST[LIST[INT]]` on a Step with a `parameterSpace`; and,
//!   in a Step with a `CHUNK[INT]` Task parameter whose `range` is a literal
//!   list, distinct values in that list.
//! - **`jobOutputDefinitions` structure.** 1 to 50 elements with unique
//!   names; no `collect`, `default`, `userInterface`, or `dataFlow`;
//!   constraint properties valid for the type; a `unit` as for an output
//!   variable. `LIST[LIST[INT]]` is allowed.
//! - **Environment Templates.** `CONTROL_FLOW` applies to Job Templates only:
//!   an Environment Template may neither list it nor use `enableIf`.
//! - **Loops** (RFC 0013 §Validation items 1, 5, 6, and 7's literal form).
//!   Exactly one of `while` and `until`; a literal `maxIterations` from 1 to
//!   1,000; `variables` of 1 to 32 elements with unique names other than
//!   `Iteration`, no `collect`, `unit`, `default`, `userInterface`, or
//!   `dataFlow`, and constraint properties valid for the type; and loop
//!   `outputVariables` of 1 to 32 elements with unique names, `collect`
//!   present and `LAST` or `APPEND` (`APPEND` only with a `LIST[...]` type),
//!   and a `unit` as for an output variable. Pass 6 checks loop names,
//!   dependencies, the body's shape, and cycles; pass 8 the expressions.
//!
//! The `Steps.*` scope rules, the form and typing of `enableIf`, `while`,
//! and `until` (a Step's and a loop's `enableIf` and a loop's `while` and
//! `until` are expression fields, RFC 0010's Template Schemas §7.6), and the
//! scope and typing of a job output's `value` belong to pass 8.

use std::collections::{BTreeSet, HashSet};

use super::helpers::check_expr_prerequisite;
use super::EffectiveLimits;
use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::{
    Environment, EnvironmentTemplate, IntOrFormatString, IntRange, JobOutputDefinition,
    JobParameterDefinition, JobTemplate, LoopOutputVariableDefinition, LoopVariableDefinition,
    OutputVariableDefinition, StepEntry, StepLoop, StepTemplate, TaskParameterDefinition,
};
use crate::types::{JobParameterType, ModelExtension, OutputCollectMode, ValidationContext};

/// RFC 0012 `outputVariables` constraint 2: at most 32 elements.
const MAX_OUTPUT_VARIABLES: usize = 32;
/// RFC 0012 `<OutputVariableDefinition>` item 5: a `unit` of 1 to 32 characters.
const MAX_UNIT_LEN: usize = 32;
/// RFC 0011 `jobOutputDefinitions` constraint 2: at most 50 elements.
const MAX_JOB_OUTPUTS: usize = 50;
/// RFC 0013 `<LoopDefinition>` `variables` and `<StepLoop>`
/// `outputVariables` constraint 2: at most 32 elements.
const MAX_LOOP_DECLARATIONS: usize = 32;
/// RFC 0013 `<LoopDefinition>` item 4 and §Limits item 1: `maxIterations`
/// is from 1 to 1,000.
pub(super) const MAX_ITERATIONS: i64 = 1000;

/// Validate RFC 0011 and RFC 0012 constraints for a job template.
///
/// Runs whether or not `CONTROL_FLOW` is enabled: without it, it rejects
/// the fields the extension adds; with it, it enforces the EXPR
/// prerequisite and the structural rules.
pub fn validate_control_flow_job_template(
    jt: &JobTemplate,
    limits: &EffectiveLimits,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::ControlFlow);
    let chunking = ctx.profile.has_extension(ModelExtension::TaskChunking);
    check_expr_prerequisite(ctx, ModelExtension::ControlFlow, "RFC 0011", errors);

    if let Some(outputs) = &jt.job_output_definitions {
        let path = path_field(&[], "jobOutputDefinitions");
        if active {
            check_job_output_definitions(outputs, &path, limits, errors);
        } else {
            errors.add(
                &path,
                "jobOutputDefinitions requires the CONTROL_FLOW extension.",
            );
        }
    }

    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            check_environment_gate(env, &path_index(&envs_path, i), active, errors);
        }
    }

    for (i, entry) in jt.steps.iter().enumerate() {
        if let StepEntry::Loop(l) = entry {
            let entry_path = path_index(&path_field(&[], "steps"), i);
            if active {
                check_loop(l, &entry_path, limits, errors);
            } else {
                errors.add(
                    &path_field(&entry_path, "loop"),
                    "loop requires the CONTROL_FLOW extension (RFC 0013).",
                );
            }
        }
    }

    for st in jt.step_templates() {
        let step = st.step;
        let step_path = st.path();
        if !active && step.enable_if.is_some() {
            errors.add(
                &path_field(&step_path, "enableIf"),
                "enableIf requires the CONTROL_FLOW extension.",
            );
        }
        if let Some(envs) = &step.step_environments {
            let envs_path = path_field(&step_path, "stepEnvironments");
            for (j, env) in envs.iter().enumerate() {
                check_environment_gate(env, &path_index(&envs_path, j), active, errors);
            }
        }
        if let Some(outputs) = &step.output_variables {
            let ov_path = path_field(&step_path, "outputVariables");
            if active {
                check_output_variables(step, outputs, &ov_path, limits, errors);
                // Without TASK_CHUNKING, pass 9 already rejects the
                // CHUNK[INT] parameter itself.
                if chunking {
                    check_chunk_values_distinct(step, &step_path, errors);
                }
            } else {
                errors.add(
                    &ov_path,
                    "outputVariables requires the CONTROL_FLOW extension.",
                );
            }
        }
    }
}

/// Validate RFC 0011 constraints for an environment template: `CONTROL_FLOW`
/// applies to Job Templates only, and `enableIf` is permitted only in a Job
/// Template, so both are rejected whatever the template's extensions.
pub fn validate_control_flow_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    if ctx.profile.has_extension(ModelExtension::ControlFlow) {
        errors.add(
            &path_field(&[], "extensions"),
            "CONTROL_FLOW is not allowed in an Environment Template; it applies to Job Templates only (RFC 0011).",
        );
    }
    if et.environment.enable_if.is_some() {
        errors.add(
            &path_field(&path_field(&[], "environment"), "enableIf"),
            "enableIf is not allowed in an Environment Template (RFC 0011).",
        );
    }
}

/// Reject an Environment's `enableIf` when the extension is off.
fn check_environment_gate(
    env: &Environment,
    env_path: &[PathElement],
    active: bool,
    errors: &mut ValidationErrors,
) {
    if !active && env.enable_if.is_some() {
        errors.add(
            &path_field(env_path, "enableIf"),
            "enableIf requires the CONTROL_FLOW extension.",
        );
    }
}

fn is_list_type(ty: JobParameterType) -> bool {
    matches!(
        ty,
        JobParameterType::ListString
            | JobParameterType::ListInt
            | JobParameterType::ListFloat
            | JobParameterType::ListPath
            | JobParameterType::ListBool
            | JobParameterType::ListListInt
    )
}

/// The structural rules on one Step's `outputVariables`.
fn check_output_variables(
    step: &StepTemplate,
    outputs: &[OutputVariableDefinition],
    ov_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    check_list_length(outputs.len(), MAX_OUTPUT_VARIABLES, ov_path, errors);
    let mut names = HashSet::new();
    for (k, ov) in outputs.iter().enumerate() {
        let path = path_index(ov_path, k);
        check_definition(
            &Declared {
                kind: Kind::OutputVariable,
                definition: ov.definition(),
                unavailable: ov.unavailable_properties(),
            },
            &path,
            &mut names,
            limits,
            errors,
        );
        let ty = ov.job_param_type();
        if ov.collect() == OutputCollectMode::Append && !is_list_type(ty) {
            errors.add(
                &path_field(&path, "collect"),
                format!("APPEND requires a LIST[...] type; '{ty}' is not a list type."),
            );
        }
        check_unit(ov.unit(), &path, errors);
        if ty == JobParameterType::ListListInt && step.parameter_space.is_some() {
            errors.add(
                &path_field(&path, "type"),
                "LIST[LIST[INT]] is not allowed in a Step with a parameterSpace: each output of a multi-Task Step is collected into a list, and EXPR allows two levels of list nesting.",
            );
        }
    }
}

/// The structural rules on `jobOutputDefinitions` (RFC 0011 §Validation item
/// 9, except the `value` scope and typing, which pass 8 checks). A job
/// output may be `LIST[LIST[INT]]`: there is no `parameterSpace` to nest it
/// in.
fn check_job_output_definitions(
    outputs: &[JobOutputDefinition],
    list_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    check_list_length(outputs.len(), MAX_JOB_OUTPUTS, list_path, errors);
    // Job output names and job parameter names are separate namespaces.
    let mut names = HashSet::new();
    for (k, def) in outputs.iter().enumerate() {
        let path = path_index(list_path, k);
        check_definition(
            &Declared {
                kind: Kind::JobOutput,
                definition: def.definition(),
                unavailable: def.unavailable_properties(),
            },
            &path,
            &mut names,
            limits,
            errors,
        );
        check_unit(def.unit(), &path, errors);
    }
}

/// The structural rules on a loop (RFC 0013 §Validation, "At template
/// validation", items 1, 5, 6, and the literal form of 7). The names, the
/// dependencies, the body's shape, and the body's cycles are checked by
/// structure (pass 6), and the expressions by pass 8.
fn check_loop(
    l: &StepLoop,
    entry_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    let loop_path = path_field(entry_path, "loop");
    let def = &l.loop_definition;
    // Item 1: exactly one of `while` and `until`.
    match (&def.while_condition, &def.until_condition) {
        (Some(_), Some(_)) => errors.add(
            &loop_path,
            "must have exactly one of 'while' and 'until', not both.",
        ),
        (None, None) => errors.add(&loop_path, "must have exactly one of 'while' and 'until'."),
        _ => {}
    }
    // Item 7, literal form; a format string is checked by pass 8.
    if let IntOrFormatString::Int(n) = def.max_iterations {
        if !(1..=MAX_ITERATIONS).contains(&n) {
            errors.add(
                &path_field(&loop_path, "maxIterations"),
                format!("must be from 1 to {MAX_ITERATIONS}; got {n}."),
            );
        }
    }
    if let Some(vars) = &def.variables {
        check_loop_variables(vars, &path_field(&loop_path, "variables"), limits, errors);
    }
    if let Some(outputs) = &l.output_variables {
        check_loop_outputs(
            outputs,
            &path_field(entry_path, "outputVariables"),
            limits,
            errors,
        );
    }
}

/// Item 5: 1 to 32 elements with unique `<Identifier>` names other than
/// `Iteration`, an `<OutputVariableType>` each, and constraint properties
/// valid for the type. `LIST[LIST[INT]]` is allowed.
fn check_loop_variables(
    vars: &[LoopVariableDefinition],
    list_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    check_list_length(vars.len(), MAX_LOOP_DECLARATIONS, list_path, errors);
    let mut names = HashSet::new();
    for (k, var) in vars.iter().enumerate() {
        let path = path_index(list_path, k);
        if var.name() == "Iteration" {
            errors.add(
                &path_field(&path, "name"),
                "'Iteration' is reserved: Loop.Iteration is the index of the current iteration.",
            );
        }
        check_definition(
            &Declared {
                kind: Kind::LoopVariable,
                definition: var.definition(),
                unavailable: var.unavailable_properties(),
            },
            &path,
            &mut names,
            limits,
            errors,
        );
    }
}

/// Item 6: 1 to 32 elements with unique `<Identifier>` names; `collect`
/// present and `LAST` or `APPEND`, and `APPEND` only with a `LIST[...]`
/// type; types, constraint properties, and `unit` as for RFC 0012's
/// `outputVariables`. `LIST[LIST[INT]]` is allowed: a loop has no
/// `parameterSpace`. (A missing `value` fails to parse.)
fn check_loop_outputs(
    outputs: &[LoopOutputVariableDefinition],
    list_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    check_list_length(outputs.len(), MAX_LOOP_DECLARATIONS, list_path, errors);
    let mut names = HashSet::new();
    for (k, ov) in outputs.iter().enumerate() {
        let path = path_index(list_path, k);
        check_definition(
            &Declared {
                kind: Kind::LoopOutput,
                definition: ov.definition(),
                unavailable: ov.unavailable_properties(),
            },
            &path,
            &mut names,
            limits,
            errors,
        );
        let collect_path = path_field(&path, "collect");
        let ty = ov.job_param_type();
        match ov.collect() {
            None => errors.add(
                &collect_path,
                "is required in a loop output definition: LAST or APPEND.",
            ),
            Some(OutputCollectMode::Once) => errors.add(
                &collect_path,
                "ONCE is not allowed for a loop output, whose value is evaluated at every iteration boundary; use LAST or APPEND.",
            ),
            Some(OutputCollectMode::Append) if !is_list_type(ty) => errors.add(
                &collect_path,
                format!("APPEND requires a LIST[...] type; '{ty}' is not a list type."),
            ),
            Some(_) => {}
        }
        check_unit(ov.unit(), &path, errors);
    }
}

/// Which definition [`check_definition`] is checking, for its messages.
#[derive(Clone, Copy)]
enum Kind {
    OutputVariable,
    JobOutput,
    LoopVariable,
    LoopOutput,
}

impl Kind {
    /// The lowercase noun: `output variable`, `job output`, and so on.
    fn noun(self) -> &'static str {
        match self {
            Self::OutputVariable => "output variable",
            Self::JobOutput => "job output",
            Self::LoopVariable => "loop variable",
            Self::LoopOutput => "loop output",
        }
    }

    /// The capitalized noun, at the start of a message.
    fn label(self) -> &'static str {
        match self {
            Self::OutputVariable => "Output variable",
            Self::JobOutput => "Job output",
            Self::LoopVariable => "Loop variable",
            Self::LoopOutput => "Loop output",
        }
    }

    /// `an output variable definition`, `a job output definition`, and so on.
    fn definition_phrase(self) -> &'static str {
        match self {
            Self::OutputVariable => "an output variable definition",
            Self::JobOutput => "a job output definition",
            Self::LoopVariable => "a loop variable definition",
            Self::LoopOutput => "a loop output definition",
        }
    }
}

/// What an output variable and a job output definition have in common.
struct Declared<'a> {
    kind: Kind,
    definition: &'a JobParameterDefinition,
    unavailable: &'a [&'static str],
}

/// 1 to `max` elements.
fn check_list_length(len: usize, max: usize, path: &[PathElement], errors: &mut ValidationErrors) {
    if len == 0 {
        errors.add(path, "if provided, must contain at least one element.");
    }
    if len > max {
        errors.add(path, format!("must not contain more than {max} elements."));
    }
}

/// The rules an output variable and a job output definition share, at
/// `path`: the name's length and uniqueness among `names`, the properties
/// the definition may not have, and constraint properties valid for the
/// type. The `unit` is [`check_unit`].
fn check_definition<'a>(
    d: &Declared<'a>,
    path: &[PathElement],
    names: &mut HashSet<&'a str>,
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    let name = d.definition.name();
    if name.chars().count() > limits.max_identifier_len {
        errors.add(
            path,
            format!("name exceeds {} characters.", limits.max_identifier_len),
        );
    }
    if !names.insert(name) {
        errors.add(path, format!("duplicate {} name: '{name}'", d.kind.noun()));
    }
    for key in d.unavailable {
        errors.add(
            &path_field(path, key),
            format!("is not allowed in {}.", d.kind.definition_phrase()),
        );
    }
    // Constraint properties must be valid for the type, by the same rules
    // as the job parameter definition of that type. Those messages name a
    // "Parameter"; relabel them.
    if let Err(msgs) = d.definition.validate_definition(limits) {
        let param_prefix = format!("Parameter '{name}': ");
        for m in msgs {
            let detail = m.strip_prefix(&param_prefix).unwrap_or(&m);
            errors.add(path, format!("{} '{name}': {detail}", d.kind.label()));
        }
    }
}

/// A `unit` of 1 to 32 characters with no Unicode Cc characters.
fn check_unit(unit: Option<&str>, path: &[PathElement], errors: &mut ValidationErrors) {
    let Some(unit) = unit else {
        return;
    };
    let unit_path = path_field(path, "unit");
    let len = unit.chars().count();
    if len == 0 {
        errors.add(&unit_path, "must not be empty.");
    }
    if len > MAX_UNIT_LEN {
        errors.add(&unit_path, format!("exceeds {MAX_UNIT_LEN} characters."));
    }
    if unit.chars().any(char::is_control) {
        errors.add(&unit_path, "contains control characters.");
    }
}

/// RFC 0012 §Chunked Steps item 1: in a Step that declares `outputVariables`,
/// the `CHUNK[INT]` parameter's values must be distinct, so that a value
/// names exactly one Task. A literal-list `range` is checked here; a
/// computed one when it is materialized. A range string cannot repeat a
/// value: its sub-ranges may not overlap.
fn check_chunk_values_distinct(
    step: &StepTemplate,
    step_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    let Some(ps) = &step.parameter_space else {
        return;
    };
    let tpd_path = path_field(
        &path_field(step_path, "parameterSpace"),
        "taskParameterDefinitions",
    );
    for (k, tp) in ps.task_parameter_definitions.iter().enumerate() {
        let TaskParameterDefinition::CHUNK_INT(chunk) = tp else {
            continue;
        };
        let IntRange::List(items) = &chunk.range else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut repeated = BTreeSet::new();
        // A format-string item is checked once it is evaluated, with the
        // computed ranges (job creation or step input binding).
        for value in items.iter().filter_map(|item| item.as_i64()) {
            if !seen.insert(value) {
                repeated.insert(value);
            }
        }
        if !repeated.is_empty() {
            let values: Vec<String> = repeated.iter().map(i64::to_string).collect();
            let (noun, verb) = if values.len() == 1 {
                ("value", "appears")
            } else {
                ("values", "appear")
            };
            errors.add(
                &path_field(&path_index(&tpd_path, k), "range"),
                format!(
                    "must have distinct values in a Step with outputVariables, so that each value names one Task; {noun} {} {verb} more than once.",
                    values.join(", ")
                ),
            );
        }
    }
}
