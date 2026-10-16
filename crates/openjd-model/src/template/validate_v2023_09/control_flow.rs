// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 11: validate or reject `CONTROL_FLOW` features (RFC 0010).
//!
//! Three fields are gated by the `CONTROL_FLOW` extension:
//! - `enableIf` on `<StepTemplate>`
//! - `outputVariables` on `<StepTemplate>`
//! - `enableIf` on `<Environment>` (Job and Step Environments)
//!
//! Without the extension, each of them is an error at its own path. With it,
//! this pass enforces the RFC's static rules (§Validation, "At template
//! validation", items 1, 2, and 6's Environment Template part):
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
//! - **Environment Templates.** `CONTROL_FLOW` applies to Job Templates only:
//!   an Environment Template may neither list it nor use `enableIf`.
//!
//! The `Steps.*` scope rules and the typing of `enableIf` belong to pass 8.

use std::collections::{BTreeSet, HashSet};

use super::helpers::check_expr_prerequisite;
use super::EffectiveLimits;
use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::{
    Environment, EnvironmentTemplate, IntRange, JobTemplate, OutputVariableDefinition,
    StepTemplate, TaskParameterDefinition,
};
use crate::types::{JobParameterType, ModelExtension, OutputCollectMode, ValidationContext};

/// RFC 0010 `outputVariables` constraint 2: at most 32 elements.
const MAX_OUTPUT_VARIABLES: usize = 32;
/// RFC 0010 `<OutputVariableDefinition>` item 5: a `unit` of 1 to 32 characters.
const MAX_UNIT_LEN: usize = 32;

/// Validate RFC 0010 constraints for a job template.
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
    check_expr_prerequisite(ctx, ModelExtension::ControlFlow, "RFC 0010", errors);

    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            check_environment_gate(env, &path_index(&envs_path, i), active, errors);
        }
    }

    for (i, step) in jt.steps.iter().enumerate() {
        let step_path = path_index(&path_field(&[], "steps"), i);
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

/// Validate RFC 0010 constraints for an environment template: `CONTROL_FLOW`
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
            "CONTROL_FLOW is not allowed in an Environment Template; it applies to Job Templates only (RFC 0010).",
        );
    }
    if et.environment.enable_if.is_some() {
        errors.add(
            &path_field(&path_field(&[], "environment"), "enableIf"),
            "enableIf is not allowed in an Environment Template (RFC 0010).",
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
    if outputs.is_empty() {
        errors.add(ov_path, "if provided, must contain at least one element.");
    }
    if outputs.len() > MAX_OUTPUT_VARIABLES {
        errors.add(
            ov_path,
            format!("must not contain more than {MAX_OUTPUT_VARIABLES} elements."),
        );
    }

    let mut names = HashSet::new();
    for (k, ov) in outputs.iter().enumerate() {
        let path = path_index(ov_path, k);
        let name = ov.name();
        if name.chars().count() > limits.max_identifier_len {
            errors.add(
                &path,
                format!("name exceeds {} characters.", limits.max_identifier_len),
            );
        }
        if !names.insert(name) {
            errors.add(&path, format!("duplicate output variable name: '{name}'"));
        }
        for key in ov.unavailable_properties() {
            errors.add(
                &path_field(&path, key),
                "is not allowed in an output variable definition.",
            );
        }
        // Constraint properties must be valid for the type, by the same
        // rules as the job parameter definition of that type. Those
        // messages name a "Parameter"; relabel them for an output.
        if let Err(msgs) = ov.definition().validate_definition(limits) {
            let param_prefix = format!("Parameter '{name}': ");
            for m in msgs {
                let detail = m.strip_prefix(&param_prefix).unwrap_or(&m);
                errors.add(&path, format!("Output variable '{name}': {detail}"));
            }
        }
        let ty = ov.job_param_type();
        if ov.collect() == OutputCollectMode::Append && !is_list_type(ty) {
            errors.add(
                &path_field(&path, "collect"),
                format!("APPEND requires a LIST[...] type; '{ty}' is not a list type."),
            );
        }
        if let Some(unit) = ov.unit() {
            let unit_path = path_field(&path, "unit");
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
        if ty == JobParameterType::ListListInt && step.parameter_space.is_some() {
            errors.add(
                &path_field(&path, "type"),
                "LIST[LIST[INT]] is not allowed in a Step with a parameterSpace: each output of a multi-Task Step is collected into a list, and EXPR allows two levels of list nesting.",
            );
        }
    }
}

/// RFC 0010 §Chunked Steps item 1: in a Step that declares `outputVariables`,
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
        for item in items {
            if !seen.insert(item.0) {
                repeated.insert(item.0);
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
