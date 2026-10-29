// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `openjd check` command — validate a template file.

use clap::Args;
use openjd_model::template::parse;
use std::path::PathBuf;

#[derive(Args)]
pub struct CheckArgs {
    /// Path to the template file
    pub path: PathBuf,

    /// Extensions to support (comma-separated or repeated). Empty string disables all.
    #[arg(long = "extensions")]
    pub extensions: Option<String>,

    /// Environment Template files to check a Job Template against, in the
    /// order a queue would attach them (repeatable)
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,
}

pub fn execute(args: CheckArgs) -> Result<(), Box<dyn std::error::Error>> {
    let path = &args.path;
    let content = crate::common::read_input_file(path)?;
    let template_value = parse::document_string_to_object(
        &content,
        crate::common::document_type(path),
        &crate::common::caller_limits(),
    )?;

    let version_str = template_value
        .get("specificationVersion")
        .and_then(|v| v.as_str())
        .ok_or("Missing field 'specificationVersion'")?;

    let exts = crate::common::parse_extensions(&args.extensions)?;
    let supported: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();

    let version = version_str.parse::<openjd_model::TemplateSpecificationVersion>();
    match version {
        Ok(v) if v.is_job_template() => {
            let job_template = parse::decode_job_template(
                template_value.clone(),
                Some(&supported),
                &crate::common::caller_limits(),
            )?;
            let env_templates = if args.environments.is_empty() {
                Vec::new()
            } else {
                check_with_environments(&job_template, &args.environments, &supported)?
            };
            let labels = crate::common::environment_labels(&args.environments);
            for line in disabled_environment_lines(&job_template, &env_templates, &labels) {
                println!("{line}");
            }
        }
        Ok(v) if v.is_environment_template() => {
            if !args.environments.is_empty() {
                return Err(
                    "--environment applies only when checking a Job Template; this is an \
                     Environment Template."
                        .into(),
                );
            }
            parse::decode_environment_template(
                template_value.clone(),
                Some(&supported),
                &crate::common::caller_limits(),
            )?;
        }
        Ok(_) | Err(_) => {
            return Err(format!("Unknown template 'specificationVersion' ({version_str}).").into());
        }
    }

    println!("Template at '{}' passes validation checks.", path.display());
    Ok(())
}

/// Check `job_template` against the Environment Templates at `paths`, in
/// order, with the checks that need no Job Parameter values: the Job
/// Parameter definitions merge (Template Schemas §1.2.1), then
/// `openjd_model::check_environment_templates` (`ENV_PARAMS`, §3.2):
/// `environment:` entries resolved by name, their keys, their literal
/// values, every parameter without a `default` supplied, the `WRAP_ACTIONS`
/// single-layer rule for Steps that enter a wrapping attachment, and the
/// §3.3.3 combination of literal host requirements. The warnings go to
/// stderr. Format-string values, non-literal `default`s and host
/// requirements, and entries and Environments whose `enableIf` is an
/// expression (even a constant one; it is an expression field, Template
/// Schemas §7.5) are left to `openjd run`, which has the Job Parameter
/// values; an Environment whose `enableIf` is the scalar `false` is not
/// entered, and its values and parameters without a `default` are not
/// checked (§3.2.2), so the value checks and constraint 7 are made only
/// for an attached Environment whose `enableIf` is absent or `true`, or
/// certainly unresolved (it reads only values that stay unknown).
/// Returns the Environment Templates read.
fn check_with_environments(
    job_template: &openjd_model::template::JobTemplate,
    paths: &[PathBuf],
    supported: &[&str],
) -> Result<Vec<openjd_model::template::EnvironmentTemplate>, Box<dyn std::error::Error>> {
    let env_templates = crate::common::read_environment_templates(paths, supported)?;
    openjd_model::merge_job_parameter_definitions(job_template, &env_templates)?;
    let labels = crate::common::environment_labels(paths);
    let attached = crate::common::attach(&env_templates, &labels);
    let warnings = openjd_model::check_environment_templates(
        job_template,
        &attached,
        &crate::common::submission_context(job_template),
    )?;
    crate::common::print_submission_warnings(&warnings);
    Ok(env_templates)
}

/// One line for each Step whose Sessions would enter an Environment that
/// the boolean scalar `enableIf: false` disables (`ENV_PARAMS`, Template
/// Schemas §4 item 6), naming those Environments in the order a Session
/// enters them: the attached ones (`env_templates`, named by `labels`),
/// the Job Template's `jobEnvironments`, then the Step's
/// `stepEnvironments`.
///
/// A Step would enter an Environment with `activation: ALWAYS`, an
/// `ON_REQUEST` one that an entry whose `enableIf` is absent or `true`
/// requests (an attached one only when exactly one attachment has the
/// name and no Job Environment shadows it, §3.2.1), and each of its own
/// Step Environments. An Environment or entry whose `enableIf` is an
/// expression is decided when the Job is created, so it is not listed.
fn disabled_environment_lines(
    jt: &openjd_model::template::JobTemplate,
    env_templates: &[openjd_model::template::EnvironmentTemplate],
    labels: &[String],
) -> Vec<String> {
    use openjd_model::template::EnvironmentActivation;
    use std::collections::HashSet;
    let job_envs = jt.job_environments.as_deref().unwrap_or(&[]);
    let job_env_names: HashSet<&str> = job_envs.iter().map(|e| e.name.as_str()).collect();
    // How many attachments define each name, once, so each Step's check is
    // linear in the attachments.
    let mut attached_count: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for t in env_templates {
        *attached_count
            .entry(t.environment().name.as_str())
            .or_default() += 1;
    }
    // An `ON_REQUEST` attachment an entry can reach: exactly one attachment
    // has the name and no Job Environment shadows it (§3.2.1).
    let reachable = |name: &str| !job_env_names.contains(name) && attached_count[name] == 1;
    let mut lines = Vec::new();
    for step in &jt.steps {
        let requested: HashSet<&str> = step
            .dependencies
            .iter()
            .flatten()
            .filter(|d| d.literal_enabled() == Some(true))
            .filter_map(|d| d.environment())
            .collect();
        let enters = |env: &openjd_model::template::Environment| {
            env.activation() == EnvironmentActivation::Always
                || requested.contains(env.name.as_str())
        };
        let mut names: Vec<String> = Vec::new();
        for (t, label) in env_templates.iter().zip(labels) {
            let env = t.environment();
            let entered = env.activation() == EnvironmentActivation::Always
                || (reachable(&env.name) && requested.contains(env.name.as_str()));
            if entered && env.literal_enabled() == Some(false) {
                names.push(format!("'{}' ({label})", env.name));
            }
        }
        names.extend(
            job_envs
                .iter()
                .filter(|env| enters(env) && env.literal_enabled() == Some(false))
                .map(|env| format!("'{}' (jobEnvironments)", env.name)),
        );
        names.extend(
            step.step_environments
                .iter()
                .flatten()
                .filter(|env| env.literal_enabled() == Some(false))
                .map(|env| format!("'{}' (stepEnvironments)", env.name)),
        );
        if !names.is_empty() {
            let noun = if names.len() == 1 {
                "Environment"
            } else {
                "Environments"
            };
            lines.push(format!(
                "Step '{}': enableIf: false disables {noun} {} for its Sessions.",
                step.name,
                names.join(", ")
            ));
        }
    }
    lines
}
