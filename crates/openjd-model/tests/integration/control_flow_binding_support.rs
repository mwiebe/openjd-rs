// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Shared helpers for the `CONTROL_FLOW` (RFC 0010) step input binding tests.

use openjd_expr::ExprValue;
use openjd_model::job::{BindingError, Job, ProducerState, Step, StepInputs};
use openjd_model::outputs::TaskOutputs;
use openjd_model::template::JobTemplate;
use openjd_model::{
    advance_step_binding, create_job, decode_job_template, preprocess_job_parameters, CallerLimits,
    JobParameterValues, ModelError, PathParameterOptions, ValidationContext,
};

pub const SUPPORTED: &[&str] = &["EXPR", "CONTROL_FLOW", "TASK_CHUNKING", "FEATURE_BUNDLE_1"];

/// A template header: `specificationVersion`, `extensions`, and `name: J`.
pub fn header(extensions: &str) -> String {
    format!("specificationVersion: jobtemplate-2023-09\nextensions: {extensions}\nname: J\n")
}

pub fn decode(template: &str) -> JobTemplate {
    let value: serde_json::Value = serde_saphyr::from_str(template).unwrap();
    match decode_job_template(value, Some(SUPPORTED), &CallerLimits::default()) {
        Ok(jt) => jt,
        Err(e) => panic!("expected the template to decode, got:\n{e}\n\n{template}"),
    }
}

/// The job parameter values for `inputs` (name, value text), with defaults.
pub fn params(jt: &JobTemplate, inputs: &[(&str, &str)]) -> JobParameterValues {
    let dir = std::env::temp_dir();
    let dir = dir.to_str().unwrap();
    let inputs = inputs
        .iter()
        .map(|(k, v)| (k.to_string(), ExprValue::String(v.to_string())))
        .collect();
    preprocess_job_parameters(jt, &inputs, &[], &PathParameterOptions::new(dir, dir)).unwrap()
}

/// The template's own context, with `limits`.
pub fn ctx(jt: &JobTemplate, limits: CallerLimits) -> ValidationContext {
    jt.default_validation_context().with_caller_limits(limits)
}

/// A job binding test: the template, its parameter values, and its context.
pub struct Fixture {
    pub jt: JobTemplate,
    pub params: JobParameterValues,
    pub ctx: ValidationContext,
}

impl Fixture {
    pub fn new(template: &str, inputs: &[(&str, &str)]) -> Self {
        Self::with_limits(template, inputs, CallerLimits::default())
    }

    pub fn with_limits(template: &str, inputs: &[(&str, &str)], limits: CallerLimits) -> Self {
        let jt = decode(template);
        let params = params(&jt, inputs);
        let ctx = ctx(&jt, limits);
        Self { jt, params, ctx }
    }

    pub fn create(&self) -> Result<Job, ModelError> {
        create_job(&self.jt, &self.params, &self.ctx)
    }

    pub fn job(&self) -> Job {
        match self.create() {
            Ok(job) => job,
            Err(e) => panic!("expected create_job to succeed, got:\n{e}"),
        }
    }

    /// Advance Step `step` (by name) of `job` with the inputs `inputs` builds
    /// from it, and swap the result in.
    pub fn advance(
        &self,
        job: &mut Job,
        step: &str,
        inputs: impl FnOnce(&Job) -> StepInputs,
        committed: u64,
    ) -> Result<Step, BindingError> {
        let i = index(job, step);
        let inputs = inputs(job);
        let advanced = advance_step_binding(
            &self.jt,
            &self.params,
            &self.ctx,
            job,
            i,
            &inputs,
            committed,
        )?;
        job.steps[i] = advanced.clone();
        Ok(advanced)
    }
}

pub fn index(job: &Job, step: &str) -> usize {
    job.steps
        .iter()
        .position(|s| s.name == step)
        .unwrap_or_else(|| panic!("no Step {step}"))
}

pub fn step<'a>(job: &'a Job, name: &str) -> &'a Step {
    &job.steps[index(job, name)]
}

/// One Task's outputs.
pub fn outputs(pairs: &[(&str, ExprValue)]) -> TaskOutputs {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// Add producer `name` of `job` to `inputs` as succeeded with `task_outputs`.
pub fn succeeded(inputs: &mut StepInputs, job: &Job, name: &str, task_outputs: &[TaskOutputs]) {
    inputs
        .add_producer(step(job, name), ProducerState::Succeeded { task_outputs })
        .unwrap();
}

/// Add producer `name` of `job` to `inputs` with its Tasks known.
pub fn tasks_known(inputs: &mut StepInputs, job: &Job, name: &str) {
    inputs
        .add_producer(step(job, name), ProducerState::TasksKnown)
        .unwrap();
}

/// The `Model validation error` text for `errors` (path, message).
pub fn expected(errors: &[(&str, &str)]) -> String {
    let n = errors.len();
    let word = if n == 1 { "error" } else { "errors" };
    let mut out = format!("Model validation error: {n} validation {word} for JobTemplate");
    for (path, msg) in errors {
        out.push_str(&format!("\n{path}:\n\t{msg}"));
    }
    out
}
