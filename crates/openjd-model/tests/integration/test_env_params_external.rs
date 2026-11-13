// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` at submission: `create_job_with_environment_templates`
//! resolving `environment:` entries against attached Environment Templates
//! (Template Schemas §3.2.1), computing their effective values (§3.2.2),
//! and checking constraints 5–7 over the combined Job.

use openjd_model::job::{Document, Job};
use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{
    create_job_with_environment_templates, AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_ok, job_params_with, rendered_values, template_ctx,
};

const CONDA: &str = include_str!("../fixtures/env_params/conda-env-params.yaml");

/// A decoded submission: the Job Template, the attached templates, and
/// the merged Job Parameter values.
struct Submission {
    jt: JobTemplate,
    envs: Vec<EnvironmentTemplate>,
    params: openjd_model::JobParameterValues,
}

fn submission(jt_src: &str, env_srcs: &[&str], params: &[(&str, &str)]) -> Submission {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let params = job_params_with(&jt, params, &envs);
    Submission { jt, envs, params }
}

impl Submission {
    /// Create the Job with the attachments, labelled `labels` (by index),
    /// under `ctx`.
    fn create_with(
        &self,
        labels: &[Option<&str>],
        ctx: &openjd_model::ValidationContext,
    ) -> Result<CreatedJob, ModelError> {
        let attached: Vec<AttachedEnvironmentTemplate<'_>> = self
            .envs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let a = AttachedEnvironmentTemplate::new(t);
                match labels.get(i).copied().flatten() {
                    Some(l) => a.with_label(l),
                    None => a,
                }
            })
            .collect();
        create_job_with_environment_templates(&self.jt, &attached, &self.params, ctx)
    }

    fn create(&self) -> Result<CreatedJob, ModelError> {
        self.create_with(&[], &template_ctx(&self.jt))
    }

    fn combined(&self) -> (Job, Vec<String>) {
        let created = self
            .create()
            .unwrap_or_else(|e| panic!("create_job_with_environment_templates failed:\n{e}"));
        let warnings = created.warnings.iter().map(ToString::to_string).collect();
        (created.job, warnings)
    }

    fn apply_err(&self, expected: &[&str]) {
        let err = self
            .create()
            .expect_err("expected the submission to be rejected");
        // Shaped by the inputs: `Submission` with attachments, `JobTemplate`
        // without.
        let model = if self.envs.is_empty() {
            "JobTemplate"
        } else {
            "Submission"
        };
        assert_errors(&err.to_string(), model, expected);
    }
}

const STEPS_FOR_CONDA: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: Solve
  dependencies:
  - environment: Conda
    required: false
    parameters: {Packages: "colmap=3.11"}
  script: {actions: {onRun: {command: x}}}
- name: Plain
  script: {actions: {onRun: {command: x}}}
"#;

// ── §3.2.1 item 1: exactly one match ────────────────────────────────

#[test]
fn an_entry_resolves_to_the_attached_environment_of_its_name() {
    let s = submission(STEPS_FOR_CONDA, &[CONDA], &[("CondaChannels", "bioconda")]);
    let applied = s.create().unwrap();
    assert!(applied.warnings.is_empty());
    assert_eq!(
        applied.environment_documents,
        [Document::environment_template(0, None)]
    );
    assert_eq!(
        applied.job.job_environments.as_ref().unwrap()[0].name,
        "Conda"
    );
    let job = applied.job.clone();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Channels=string:bioconda, Packages=string:colmap=3.11"]
    );
    // A Step that names the Environment in no entry takes the defaults,
    // and with the empty default Packages the sample's enableIf disables
    // the Environment for it (§4 item 6), so it has no values for it.
    assert_eq!(rendered_values(&job.steps[1]), Vec::<String>::new());
    assert!(!job.steps[1].enters_environment(&Document::environment_template(0, None), "Conda"));
    assert_eq!(
        applied.disabled_environments[1].environments,
        [openjd_model::job::EnvironmentRef {
            document: Document::environment_template(0, None),
            environment: "Conda".to_string(),
        }]
    );
    let names: Vec<&str> = job
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["Conda"]);
}

#[test]
fn attached_values_precede_the_job_environments_values() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Own
  envParameterDefinitions: [{name: X, type: STRING, default: own}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: Conda, parameters: {Packages: p}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[CONDA], &[]);
    let applied = s.create().unwrap();
    assert_eq!(
        applied.environment_documents,
        [
            Document::environment_template(0, None),
            Document::JobTemplate
        ]
    );
    let (job, _) = s.combined();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            "EnvironmentTemplate[0]/Conda: Channels=string:conda-forge, Packages=string:p",
            "JobTemplate/Own: X=string:own",
        ]
    );
}

#[test]
fn a_job_template_without_env_params_takes_every_default() {
    // RFC Backward compatibility bullet 3: the extension applies to its own
    // document.
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[CONDA], &[("CondaPackages", "numpy")]);
    let (job, warnings) = s.combined();
    assert!(warnings.is_empty());
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Channels=string:conda-forge, Packages=string:numpy"]
    );
}

#[test]
fn an_attached_environment_without_parameters_gets_no_values() {
    let plain = r#"
specificationVersion: environment-2023-09
environment:
  name: Plain
  script: {actions: {onEnter: {command: x}}}
"#;
    let s = submission(
        STEPS_FOR_CONDA
            .replace("Conda", "Plain")
            .replace("    parameters: {Packages: \"colmap=3.11\"}\n", "")
            .as_str(),
        &[plain],
        &[],
    );
    let (job, warnings) = s.combined();
    assert!(warnings.is_empty());
    assert_eq!(job.steps[0].environment_parameters, None);
}

#[test]
fn undeclared_keys_are_rejected_whatever_required_says() {
    // Constraint 5 (§3.2.1 item 1).
    let jt = STEPS_FOR_CONDA.replace(
        "{Packages: \"colmap=3.11\"}",
        "{Packages: a, Pkgs: b, Zzz: c}",
    );
    submission(&jt, &[CONDA], &[]).apply_err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Pkgs:\n\tEnvironment 'Conda' (EnvironmentTemplate[0]) declares no parameter 'Pkgs'. Declared: Packages, Channels.",
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Zzz:\n\tEnvironment 'Conda' (EnvironmentTemplate[0]) declares no parameter 'Zzz'.",
    ]);
}

/// An Environment Template written before ENV_PARAMS: an Environment
/// named `Conda` that declares no parameters, configured by a Job
/// Parameter.
const OLD_CONDA: &str = r#"
specificationVersion: environment-2023-09
parameterDefinitions:
- {name: CondaPackages, type: STRING, default: ""}
environment:
  name: Conda
  script: {actions: {onEnter: {command: x, args: ["{{Param.CondaPackages}}"]}}}
"#;

/// The warning for `STEPS_FOR_CONDA`'s entry when `OLD_CONDA` ignores its
/// parameters, listing `keys`.
fn ignored_warning(keys: &str) -> String {
    format!(
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters: Environment 'Conda' \
         (EnvironmentTemplate[0]) declares no parameters, and this entry has 'required: false', \
         so its parameters are ignored: {keys}. The Step's Sessions get no values from it \
         (Template Schemas §3.2.1 item 1)."
    )
}

#[test]
fn a_required_entry_supplying_parameters_to_an_environment_that_declares_none_is_rejected() {
    // §3.2.1 item 1: only a `required: false` entry's parameters are
    // ignored.
    let jt = STEPS_FOR_CONDA
        .replace("required: false", "required: true")
        .replace("{Packages: \"colmap=3.11\"}", "{Packages: a, Channels: b}");
    submission(&jt, &[OLD_CONDA], &[]).apply_err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters:\n\tEnvironment 'Conda' (EnvironmentTemplate[0]) declares no parameters, so this entry must not supply any; it supplies Channels, Packages.",
    ]);
    // `required` absent means `true`.
    let jt = jt.replace("    required: true\n", "");
    submission(&jt, &[OLD_CONDA], &[]).apply_err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters:\n\tEnvironment 'Conda' (EnvironmentTemplate[0]) declares no parameters, so this entry must not supply any; it supplies Channels, Packages.",
    ]);
}

#[test]
fn an_optional_entrys_parameters_are_ignored_by_an_environment_that_declares_none() {
    // §3.2.1 item 1: the entry supplies no values, with one warning naming
    // the entry, the Environment and the keys. Its values are not resolved
    // and not checked (constraints 5 and 6), so an index out of range and
    // a list for what would be a STRING are not errors. Every Step's
    // Sessions still enter the Environment.
    let jt = STEPS_FOR_CONDA.replace(
        "name: J\n",
        "name: J\nparameterDefinitions:\n- {name: L, type: \"LIST[INT]\", default: [1]}\n",
    );
    let jt = jt.replace(
        "{Packages: \"colmap=3.11\"}",
        "{Packages: \"{{ Param.L[5] }}\", Channels: [a, b], Zzz: 3}",
    );
    let s = submission(&jt, &[OLD_CONDA], &[]);
    let created = s
        .create()
        .unwrap_or_else(|e| panic!("job creation failed:\n{e}"));
    let warnings: Vec<String> = created.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(warnings, [ignored_warning("Channels, Packages, Zzz")]);
    let job = &created.job;
    assert_eq!(job.steps[0].environment_parameters, None);
    // Both Steps' Sessions enter it; the entry is not "not found".
    for i in 0..job.steps.len() {
        assert_eq!(
            created.environment_statuses[i].environments[0].status,
            openjd_model::EnvironmentStatus::Entered,
            "step {i}"
        );
        assert!(created.environment_statuses[i].not_found.is_empty());
    }
}

#[test]
fn an_optional_entry_without_parameters_gets_no_warning() {
    let jt = STEPS_FOR_CONDA.replace("    parameters: {Packages: \"colmap=3.11\"}\n", "");
    let (job, warnings) = submission(&jt, &[OLD_CONDA], &[]).combined();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(job.steps[0].environment_parameters, None);
}

#[test]
fn an_optional_entry_enabled_by_an_expression_is_ignored_at_job_creation() {
    // The entry's `enableIf` is decided when the Job is created; once it is
    // `true`, the rule applies as for an entry without one.
    let jt = STEPS_FOR_CONDA.replace(
        "    required: false\n",
        "    required: false\n    enableIf: \"Job.Name == 'J'\"\n",
    );
    let (_, warnings) = submission(&jt, &[OLD_CONDA], &[]).combined();
    assert_eq!(warnings, [ignored_warning("Packages")]);
}

#[test]
fn undeclared_keys_are_rejected_by_an_environment_that_declares_parameters() {
    // §3.2.1 item 1: only an Environment that declares none ignores keys,
    // even when every key of a `required: false` entry is undeclared.
    let jt = STEPS_FOR_CONDA.replace("{Packages: \"colmap=3.11\"}", "{Pkgs: b}");
    submission(&jt, &[CONDA], &[]).apply_err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Pkgs:\n\tEnvironment 'Conda' (EnvironmentTemplate[0]) declares no parameter 'Pkgs'. Declared: Packages, Channels.",
    ]);
}

// ── §3.2.1 item 2: no match ─────────────────────────────────────────

const REQUIRED: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - {environment: Conda}
  - {environment: Docker, required: true}
  - {environment: Gpu, required: false}
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn a_required_entry_with_no_environment_is_rejected() {
    submission(REQUIRED, &[], &[]).apply_err(&[
        "steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Conda', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).",
        "steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).",
    ]);
}

#[test]
fn a_missing_environment_names_the_attached_documents() {
    let s = submission(
        REQUIRED,
        &[
            CONDA,
            CONDA.replace("  name: Conda\n", "  name: Other\n").as_str(),
        ],
        &[],
    );
    let err = s
        .create_with(&[None, Some("queue/other.yaml")], &template_ctx(&s.jt))
        .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &["JobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates (EnvironmentTemplate[0], queue/other.yaml) defines it. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2)."],
    );
}

#[test]
fn an_optional_entry_with_no_environment_is_ignored_with_a_warning() {
    let jt = REQUIRED.replace(
        "  - {environment: Conda}\n  - {environment: Docker, required: true}\n",
        "",
    );
    let s = submission(&jt, &[], &[]);
    let (job, warnings) = s.combined();
    assert_eq!(
        warnings,
        ["steps[0] -> dependencies[0] -> environment: names Environment 'Gpu', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry has 'required: false', so it has no effect."]
    );
    assert_eq!(job.steps[0].environment_parameters, None);
}

// ── §3.2.1 item 3: two or more matches ──────────────────────────────

#[test]
fn an_entry_naming_an_ambiguous_environment_is_rejected() {
    let s = submission(STEPS_FOR_CONDA, &[CONDA, CONDA], &[]);
    let err = s
        .create_with(&[Some("a.yaml"), None], &template_ctx(&s.jt))
        .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &["JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Conda', which 2 attached Environment Templates define (a.yaml, EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 item 3)."],
    );
}

#[test]
fn an_ambiguous_entry_is_reported_once_without_contradicting_errors() {
    // Each copy has a parameter without default and one whose default
    // fails for a Step that takes it; the entry supplies both. Only the
    // ambiguity is reported, not "must supply" or "does not supply"
    // errors for each copy: it stops the Step after §7.4.1 step 4, and no
    // other Step is known to enter the copies, so their unreachable errors
    // are not reported either.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: Word, type: STRING, default: w}]
environment:
  name: Q
  envParameterDefinitions:
  - {name: R, type: INT}
  - {name: Bad, type: INT, default: "{{Param.Word}}"}
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{environment: Q, parameters: {R: 1, Bad: 2}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[env, env, env], &[]);
    s.apply_err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Q', which 3 attached Environment Templates define (EnvironmentTemplate[0], EnvironmentTemplate[1], EnvironmentTemplate[2]); an entry must match exactly one (Template Schemas §3.2.1 item 3).",
    ]);
}

#[test]
fn an_entry_resolves_to_the_attachment_it_names_not_the_first() {
    let env = |name: &str, param: &str, default: i64| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: {name}\n  envParameterDefinitions: [{{name: {param}, type: INT, default: {default}}}]\n  variables: {{V: v}}\n"
        )
    };
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: B, parameters: {Y: "5"}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let (a, b) = (env("A", "X", 1), env("B", "Y", 2));
    let (job, _) = submission(jt, &[&a, &b], &[]).combined();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            "EnvironmentTemplate[0]/A: X=int:1",
            "EnvironmentTemplate[1]/B: Y=int:5"
        ]
    );
}

#[test]
fn two_same_named_attachments_no_entry_names_are_both_kept() {
    // Plan A11: no error unless an entry names the ambiguous name.
    let jt = STEPS_FOR_CONDA.replace(
        "  dependencies:\n  - environment: Conda\n    required: false\n    parameters: {Packages: \"colmap=3.11\"}\n",
        "",
    );
    // Non-empty Packages, so the sample's enableIf enters both. The second
    // copy leaves out the sample's exclusiveGroup, which would supersede it
    // (§4 item 7).
    let ungrouped = CONDA.replace("  exclusiveGroup: SoftwareProvider\n", "");
    assert_ne!(ungrouped, CONDA);
    let s = submission(&jt, &[CONDA, &ungrouped], &[("CondaPackages", "p")]);
    let (job, _) = s.combined();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            "EnvironmentTemplate[0]/Conda: Channels=string:conda-forge, Packages=string:p",
            "EnvironmentTemplate[1]/Conda: Channels=string:conda-forge, Packages=string:p",
        ]
    );
}

// ── §3.2.1 items 4–5: shadowed names ────────────────────────────────

#[test]
fn an_attached_environment_named_like_a_job_environment_is_unreachable() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Conda
  envParameterDefinitions: [{name: Mine, type: STRING, default: m}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: Conda, parameters: {Mine: s}}]
  script: {actions: {onRun: {command: x}}}
"#;
    // Non-empty Packages, so the sample's enableIf enters the attachment.
    let s = submission(jt, &[CONDA], &[("CondaPackages", "p")]);
    let (job, warnings) = s.combined();
    assert_eq!(
        warnings,
        ["EnvironmentTemplate[0] -> environment -> name: Environment 'Conda' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency of the Job Template can reach it (Template Schemas §3.2.1 item 4)."]
    );
    // The entry goes to the Job Environment; the attachment keeps its
    // defaults (plan A10).
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            "EnvironmentTemplate[0]/Conda: Channels=string:conda-forge, Packages=string:p",
            "JobTemplate/Conda: Mine=string:s",
        ]
    );
    // Equality keys values on (document, environment), so the two
    // same-named Environments stay apart.
    let step = &job.steps[0];
    assert!(step.environment_values_equal(&step.clone()));
    let mut other = step.clone();
    other.environment_parameters.as_mut().unwrap().swap(0, 1);
    assert!(step.environment_values_equal(&other));
}

#[test]
fn an_attached_environment_named_like_a_step_environment_is_reached_by_other_steps() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: Own
  stepEnvironments:
  - {name: Conda, variables: {V: v}}
  script: {actions: {onRun: {command: x}}}
- name: Other
  dependencies: [{environment: Conda, parameters: {Packages: other}}]
  script: {actions: {onRun: {command: x}}}
"#;
    // Non-empty Packages, so the sample's enableIf enters the attachment.
    let (job, warnings) = submission(jt, &[CONDA], &[("CondaPackages", "p")]).combined();
    assert!(warnings.is_empty());
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Channels=string:conda-forge, Packages=string:p"]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        ["EnvironmentTemplate[0]/Conda: Channels=string:conda-forge, Packages=string:other"]
    );
}

// ── Constraint 7 over the combined Job (§3.2.1 last paragraph) ──────

const SEATS_ENV: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: License
  envParameterDefinitions:
  - {name: Seats, type: INT}
  - {name: Server, type: STRING}
  - {name: Region, type: STRING, default: us}
  variables: {V: v}
"#;

#[test]
fn every_step_must_supply_an_attached_parameter_without_default() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{environment: License, parameters: {Seats: 2, Server: s}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: License, parameters: {Seats: 2}}]
  script: {actions: {onRun: {command: x}}}
- name: C
  script: {actions: {onRun: {command: x}}}
"#;
    submission(jt, &[SEATS_ENV], &[]).apply_err(&[
        "JobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'Server' of Environment 'License' (EnvironmentTemplate[0]).",
        "JobTemplate -> steps[2]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'Seats', 'Server' of Environment 'License' (EnvironmentTemplate[0]).",
    ]);
}

// ── Values of external entries (constraint 6; §3.2 bullets) ─────────

const TYPED_ENV: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Typed
  envParameterDefinitions:
  - {name: N, type: INT, minValue: 1, default: 1}
  - {name: On, type: BOOL, default: false}
  - {name: Mounts, type: "LIST[PATH]", default: []}
  - {name: Dir, type: PATH, default: /opt/tool}
  variables: {V: v}
"#;

#[test]
fn external_values_resolve_against_the_frozen_step_scope() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Work, type: PATH, default: work}
- {name: Count, type: INT, default: 3}
steps:
- name: S
  let: ["n = Param.Count * 2"]
  dependencies:
  - environment: Typed
    parameters: {N: "{{ n }}", On: "yes", Mounts: "{{ [RawParam.Work, '/scratch'] }}", Dir: "{{RawParam.Work}}/bin"}
  script: {actions: {onRun: {command: x}}}
"#;
    let (job, _) = submission(jt, &[TYPED_ENV], &[]).combined();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Typed: Dir=path:/templates/work/bin, Mounts=list[path]:[\"/templates/work\",\"/scratch\"], N=int:6, On=bool:true"]
    );
}

#[test]
fn external_values_are_checked_against_the_definitions() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - environment: Typed
    parameters: {N: 0, On: maybe, Mounts: "x{{ 1 }}", Dir: relative}
  script: {actions: {onRun: {command: x}}}
- name: T
  dependencies:
  - environment: Typed
    parameters: {Mounts: ["{{RawParam.X}}"], N: "{{ 'abc' }}"}
  script: {actions: {onRun: {command: x}}}
"#;
    // Reported Step by Step, and within an entry in the Environment's
    // definition order. The relative `Dir` is no error (§4.5 item 3).
    submission(jt, &[TYPED_ENV], &[]).apply_err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 0 is less than minimum 1",
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> On:\n\tis not a valid BOOL value: Invalid bool value: 'maybe'",
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Mounts:\n\ta LIST[PATH] parameter has no string form, so a format-string value for it must be a single '{{ }}' expression.",
        "JobTemplate -> steps[1] -> dependencies[0] -> parameters -> N:\n\tcould not be resolved as a value of type INT: Cannot convert 'abc' to int: invalid digit found in string",
        "JobTemplate -> steps[1] -> dependencies[0] -> parameters -> Mounts[0]:\n\tthe elements of a list literal are literals, not format strings. Write the whole value as one '{{ }}' expression, for example '{{ [RawParam.Dir] }}'.",
    ]);
}

#[test]
fn an_attached_relative_literal_default_is_used_as_given() {
    // §4.5 item 3 (spec af53020): an unused relative literal `default` is
    // no error, and a Step that takes it gets it unchanged.
    let env = TYPED_ENV.replace("default: /opt/tool", "default: opt/tool");
    let unused = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(unused, &[&env], &[]);
    s.create_with(&[Some("typed.yaml")], &template_ctx(&s.jt))
        .unwrap();
    let taken = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Typed}]
  script: {actions: {onRun: {command: x}}}
"#;
    let (job, _) = submission(taken, &[&env], &[]).combined();
    assert!(
        rendered_values(&job.steps[0])[0].contains("Dir=path:opt/tool"),
        "{:?}",
        rendered_values(&job.steps[0])
    );
}

#[test]
fn an_attached_default_uses_its_own_documents_extensions() {
    // The Environment Template lists EXPR; the Job Template does not.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: Base, type: INT, default: 4}]
environment:
  name: E
  envParameterDefinitions:
  - {name: Twice, type: INT, default: "{{ Param.Base * 2 }}"}
  - {name: Uri, type: PATH, default: "s3://b/k"}
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    // A URI PATH value is used as given (§4.5 item 3).
    let job = submission(jt, &[env], &[]).create().unwrap().job;
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/E: Twice=int:8, Uri=path:s3://b/k"]
    );
}

#[test]
fn the_combined_job_round_trips_through_serde() {
    let s = submission(STEPS_FOR_CONDA, &[CONDA], &[]);
    let (job, _) = s.combined();
    let json = serde_json::to_string(&job.steps[0]).unwrap();
    let back: openjd_model::job::Step = serde_json::from_str(&json).unwrap();
    assert_eq!(back, job.steps[0]);
}

// ── The combined Job (CreatedJob) ───────────────────────────────────

#[test]
fn combined_job_environments_are_attachments_then_the_jobs_own() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: J
jobEnvironments:
- {name: Own1, variables: {V: v}}
- {name: Own2, variables: {V: v}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[CONDA], &[]);
    let applied = s.create().unwrap();
    assert_eq!(
        applied.environment_documents,
        [
            Document::environment_template(0, None),
            Document::JobTemplate,
            Document::JobTemplate
        ]
    );
    let job = applied.job;
    let names: Vec<&str> = job
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["Conda", "Own1", "Own2"]);
}

#[test]
fn with_nothing_to_combine_the_job_is_unchanged() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[], &[]);
    let applied = s.create().unwrap();
    assert_eq!(applied.environment_documents, []);
    let job = applied.job;
    assert_eq!(job.job_environments, None);
    assert_eq!(job.steps[0].environment_parameters, None);
    assert_eq!(job.steps[0].environments, None);
    assert_eq!(
        job,
        openjd_model::create_job(&s.jt, &s.params, &template_ctx(&s.jt)).unwrap()
    );
}

#[test]
fn an_attached_environment_freezes_the_job_parameters_and_job_name_with_expr() {
    let env = |exts: &str, vars: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [{exts}]\n\
             parameterDefinitions: [{{name: Q, type: STRING, default: q}}]\n\
             environment:\n  name: E\n  variables: {{{vars}}}\n"
        )
    };
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: TheJob
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let names = |e: &str| -> Vec<String> {
        let s = submission(jt, &[e], &[]);
        let applied = s.create().unwrap();
        applied.job.job_environments.as_ref().unwrap()[0]
            .resolved_symtab
            .as_ref()
            .unwrap()
            .as_value()
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                format!(
                    "{}={}",
                    e["name"].as_str().unwrap(),
                    e["value"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(
        names(&env("EXPR", r#"A: "{{Param.Q}}", B: "{{Job.Name}}""#)),
        ["Job.Name=TheJob", "Param.Q=q"]
    );
    // Only the symbols the Environment references are frozen, so one that
    // reads no `Job.*` freezes none, whatever its document lists.
    assert_eq!(
        names(&env("ENV_PARAMS, EXPR", r#"A: "{{Param.Q}}""#)),
        ["Param.Q=q"]
    );
}

#[test]
fn an_attachments_label_names_its_document_everywhere() {
    let jt = STEPS_FOR_CONDA.replace("{Packages: \"colmap=3.11\"}", "{Nope: x}");
    let s = submission(&jt, &[CONDA], &[]);
    let err = s
        .create_with(&[Some("q.yaml")], &template_ctx(&s.jt))
        .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &["JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Nope:\n\tEnvironment 'Conda' (q.yaml) declares no parameter 'Nope'. Declared: Packages, Channels."],
    );
    let s = submission(STEPS_FOR_CONDA, &[CONDA], &[]);
    let applied = s
        .create_with(&[Some("q.yaml")], &template_ctx(&s.jt))
        .unwrap();
    let labelled = Document::environment_template(0, Some("q.yaml"));
    assert_eq!(
        applied.environment_documents,
        std::slice::from_ref(&labelled)
    );
    assert_eq!(
        applied.job.steps[0]
            .environment_parameters
            .as_ref()
            .unwrap()[0]
            .document,
        labelled
    );
    assert_eq!(labelled.to_string(), "q.yaml");
    assert_eq!(
        Document::environment_template(3, None).to_string(),
        "EnvironmentTemplate[3]"
    );
    assert_eq!(Document::JobTemplate.to_string(), "JobTemplate");
    assert_eq!(
        serde_json::to_value(&labelled).unwrap(),
        serde_json::json!({"kind": "EnvironmentTemplate", "index": 0, "label": "q.yaml"})
    );
}

#[test]
fn an_attachment_is_evaluated_with_posix_paths_under_the_callers_limits() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: Big, type: STRING, default: "{{ 'ab' * 50 }}"}
  - {name: P, type: PATH, default: "{{ path('C:/tools') / 'bin' }}"}
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[env], &[]);
    // The attachment's format strings are TEMPLATE-scope fields, so they
    // evaluate with POSIX semantics (Expression Language §1.2.1): `/` joins.
    let job = s.create().unwrap().job;
    assert_eq!(
        rendered_values(&job.steps[0]),
        [format!(
            "EnvironmentTemplate[0]/E: Big=string:{}, P=path:C:/tools/bin",
            "ab".repeat(50)
        )]
    );
    // The caller's evaluation budget applies to the attachment.
    let limits = openjd_model::CallerLimits {
        max_eval_memory_bytes: Some(10),
        ..Default::default()
    };
    let err = s
        .create_with(&[], &template_ctx(&s.jt).with_caller_limits(limits))
        .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tfor Step 'S', which does not supply this parameter: could not be resolved as a value of type STRING: Expression memory usage (72 bytes) exceeded limit (10 bytes)\n  'ab' * 50\n  ^~~~",
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[1] -> default:\n\tfor Step 'S', which does not supply this parameter: could not be resolved as a value of type PATH: Expression memory usage (72 bytes) exceeded limit (10 bytes)\n  path('C:/tools') / 'bin'\n       ^~~~~~~~~~",
        ],
    );
}

/// §4.5 item 3 (spec af53020): an attached Environment's PATH parameter
/// takes a value of any form, POSIX, Windows or relative, unchanged.
#[test]
fn an_attached_path_value_of_any_form_is_used_as_given() {
    let env = TYPED_ENV.replace("default: /opt/tool", "default: 'C:\\tool'");
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Typed, parameters: {Dir: VALUE}}]
  script: {actions: {onRun: {command: x}}}
"#;
    for (value, text) in [
        ("/posix", "/posix"),
        (r"'rel\dir'", r"rel\dir"),
        ("'C:x'", "C:x"),
    ] {
        let (job, _) = submission(&jt.replace("VALUE", value), &[&env], &[]).combined();
        let rendered = rendered_values(&job.steps[0]);
        assert!(
            rendered[0].contains(&format!("Dir=path:{text},")),
            "{rendered:?}"
        );
    }
}

#[test]
fn literal_values_of_the_string_int_float_and_path_types_convert() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: F, type: FLOAT, default: 1}
  - {name: N, type: INT, default: 1}
  - {name: S, type: STRING, default: s}
  - {name: P, type: PATH, default: /p}
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: E, parameters: {F: 1.5, N: 2, S: t, P: /q}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let (job, _) = submission(jt, &[env], &[]).combined();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/E: F=float:1.5, N=int:2, P=path:/q, S=string:t"]
    );
}

// ── Message sizes at submission (constraints 5 and 7) ───────────────

/// An attached Environment with `n` STRING parameters without default,
/// each name `len` characters (FEATURE_BUNDLE_1 allows up to 512).
fn many_params_env(name: &str, prefix: char, n: usize, len: usize) -> (String, Vec<String>) {
    let names: Vec<String> = (0..n)
        .map(|i| format!("{prefix}{i:0>w$}", w = len - 1))
        .collect();
    let defs: String = names
        .iter()
        .map(|n| format!("  - {{name: {n}, type: STRING}}\n"))
        .collect();
    (
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]\n\
             environment:\n  name: {name}\n  envParameterDefinitions:\n{defs}  variables: {{V: v}}\n"
        ),
        names,
    )
}

const NO_ENTRIES: &str = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;

fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn the_missing_parameters_list_spans_attachments_and_stops_at_50_names() {
    let (a, an) = many_params_env("A", 'a', 30, 3);
    let (b, bn) = many_params_env("B", 'b', 30, 3);
    submission(NO_ENTRIES, &[&a, &b], &[]).apply_err(&[&format!(
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: {} of Environment 'A' (EnvironmentTemplate[0]); {} of Environment 'B' (EnvironmentTemplate[1]); and 10 more.",
        quoted(&an),
        quoted(&bn[..20])
    )]);
    let (b, bn) = many_params_env("B", 'b', 21, 3);
    submission(NO_ENTRIES, &[&a, &b], &[]).apply_err(&[&format!(
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: {} of Environment 'A' (EnvironmentTemplate[0]); {} of Environment 'B' (EnvironmentTemplate[1]); and 1 more.",
        quoted(&an),
        quoted(&bn[..20])
    )]);
}

#[test]
fn long_names_do_not_make_submission_messages_long() {
    // 50 parameters with 500-character names. One name fills most of the
    // 512-character list, so each list shows one name and "and N more".
    let (env, names) = many_params_env("E", 'n', 50, 500);
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
name: J
steps:
- name: S
  dependencies: [{environment: E, parameters: {Zbad1: x, Zbad2: y}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let label = "'E' (EnvironmentTemplate[0])";
    submission(jt, &[&env], &[]).apply_err(&[
        &format!(
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Zbad1:\n\tEnvironment {label} declares no parameter 'Zbad1'. Declared: {}, and 49 more.",
            names[0]
        ),
        &format!(
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Zbad2:\n\tEnvironment {label} declares no parameter 'Zbad2'."
        ),
        &format!(
            "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: '{}' of Environment {label}; and 49 more.",
            names[0]
        ),
    ]);
}

#[test]
fn lists_of_attachments_in_messages_are_capped() {
    // Labels are caller-supplied; three 300-character labels do not fit in
    // a 512-character list.
    let labels: Vec<String> = (0..3).map(|i| format!("{i}{}", "x".repeat(299))).collect();
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Missing}, {environment: Conda}]
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[CONDA, CONDA, CONDA], &[]);
    let refs: Vec<Option<&str>> = labels.iter().map(|l| Some(l.as_str())).collect();
    let err = s.create_with(&refs, &template_ctx(&s.jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            &format!(
                "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates ({}, and 2 more) defines it. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).",
                labels[0]
            ),
            &format!(
                "JobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Conda', which 3 attached Environment Templates define ({}, and 2 more); an entry must match exactly one (Template Schemas §3.2.1 item 3).",
                labels[0]
            ),
        ],
    );
}

#[test]
fn attaching_by_from_has_no_label() {
    let env = env_ok(CONDA);
    let attached: AttachedEnvironmentTemplate<'_> = (&env).into();
    assert_eq!(attached.label, None);
    assert_eq!(attached.with_label("l").label, Some("l"));
}

#[test]
fn path_operations_use_posix_semantics() {
    // Environment parameter format strings are TEMPLATE-scope fields, so
    // `path(...) / 'sub'` and `.parent` use POSIX rules (Expression
    // Language §1.2.1), in a default and in an external entry's value
    // alike: a Windows drive path is one POSIX path name.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: Root, type: STRING, default: 'C:\data\proj'}]
environment:
  name: Ext
  envParameterDefinitions:
  - {name: Sub, type: PATH, default: "{{ path(Param.Root) / 'sub' }}"}
  - {name: Up, type: PATH, default: "{{ path(Param.Root).parent }}"}
  - {name: Given, type: PATH, default: 'C:\x'}
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Base, type: STRING, default: 'D:\out'}]
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {Given: "{{ path(Param.Base) / 'run' }}"}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let job = submission(jt, &[env], &[]).create().unwrap().job;
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            r"EnvironmentTemplate[0]/Ext: Given=path:D:\out/run, Sub=path:C:\data\proj/sub, Up=path:."
        ]
    );
}

#[test]
fn an_external_entry_reads_a_path_let_binding_with_posix_semantics() {
    // The frozen Step scope keeps the `let` values job creation computed
    // once, with POSIX semantics.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Ext
  envParameterDefinitions: [{name: A, type: PATH, default: 'C:\a'}]
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Root, type: STRING, default: 'D:\out'}]
steps:
- name: S
  let: ["p = path(Param.Root) / 'run'"]
  dependencies: [{environment: Ext, parameters: {A: "{{ p.parent / 'y' }}"}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let jt_dec = job_ok(jt);
    let envs = vec![env_ok(env)];
    let params = job_params_with(&jt_dec, &[], &envs);
    let attached = [AttachedEnvironmentTemplate::new(&envs[0])];
    let job =
        create_job_with_environment_templates(&jt_dec, &attached, &params, &template_ctx(&jt_dec))
            .unwrap()
            .job;
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"EnvironmentTemplate[0]/Ext: A=path:D:\out/y"]
    );
}

#[test]
fn an_unreachable_attachment_with_a_required_parameter_is_reported_once_at_the_attachment() {
    // §3.2.1 item 4: the name resolves to the Job Environment, so no entry
    // can supply the attachment's parameter; one error says why, instead
    // of a "must supply" error per Step that no entry could satisfy.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: X
  envParameterDefinitions: [{name: Need, type: STRING}, {name: Opt, type: STRING, default: o}]
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: X, variables: {V: v}}
steps:
- name: A
  dependencies: [{environment: X}]
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#;
    submission(jt, &[env], &[]).apply_err(&["EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment 'X' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has parameters without a default that no Step can supply: Need. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1)."]);
}

#[test]
fn a_step_that_cannot_reach_an_attachment_is_told_why() {
    // §3.2.1 item 5: Step A's own Step Environment X hides the attachment
    // X from it, and constraint 2 forbids an entry naming X, so the error
    // says it cannot reach it. Step B can, and gets the usual message.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: X
  envParameterDefinitions: [{name: S, type: STRING}]
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  stepEnvironments: [{name: X, variables: {Q: r}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#;
    submission(jt, &[env], &[]).apply_err(&[
        "JobTemplate -> steps[0]:\n\tcannot supply the parameters without a default of Environment 'X' (EnvironmentTemplate[0]): S. It has the same name as a stepEnvironments entry of this Step, so no 'environment:' dependency of this Step can reach it (Template Schemas §3.2.1 item 5), and its activation is BY_DEFAULT, so it applies to this Step (§3.2 constraint 7).",
        "JobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'S' of Environment 'X' (EnvironmentTemplate[0]).",
    ]);
}

#[test]
fn an_attached_default_cannot_read_job_name() {
    // Plan A21, for an Environment Template edited after decoding:
    // attachments' defaults resolve without `Job.Name` too.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: T, type: STRING, default: t}]
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let mut s = submission(jt, &[env], &[]);
    s.envs[0]
        .environment
        .env_parameter_definitions
        .as_mut()
        .unwrap()[0]
        .default = Some(
        openjd_model::template::EnvironmentParameterValue::from_json(serde_json::json!(
            "{{ Job.Name }}"
        ))
        .unwrap(),
    );
    s.apply_err(&["EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tfor Step 'S', which does not supply this parameter: could not be resolved as a value of type STRING: Undefined variable: 'Job.Name'."]);
}

#[test]
fn ambiguous_attachments_with_a_required_parameter_are_reported_at_the_attachments() {
    // §3.2.1 item 3: no entry can name X, so its parameter without a
    // default is reported once per attachment, not as "must supply" advice.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: X
  envParameterDefinitions: [{name: P, type: STRING}]
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
"#;
    let s = submission(jt, &[env, env], &[]);
    let err = s
        .create_with(&[Some("a.yaml"), Some("b.yaml")], &template_ctx(&s.jt))
        .unwrap_err();
    let msg = |doc: &str| {
        format!(
            "{doc} -> environment -> envParameterDefinitions:\n\tEnvironment 'X' is defined by 2 attached Environment Templates (a.yaml, b.yaml), so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 3), and it has parameters without a default that no Step can supply: P. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1)."
        )
    };
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&msg("a.yaml"), &msg("b.yaml")],
    );
}

#[test]
fn an_external_entry_reads_a_posix_path_let_binding() {
    // The frozen table keeps the POSIX `let` values.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Ext
  envParameterDefinitions: [{name: A, type: PATH, default: /a}]
  variables: {V: v}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Root, type: STRING, default: /out}]
steps:
- name: S
  let: ["p = path(Param.Root) / 'run'"]
  dependencies: [{environment: Ext, parameters: {A: "{{ p.parent / 'y' }}"}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let (job, _) = submission(jt, &[env], &[]).combined();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Ext: A=path:/out/y"]
    );
}

#[test]
fn serde_form_of_documents() {
    assert_eq!(
        serde_json::to_string(&Document::environment_template(0, None)).unwrap(),
        r#"{"kind":"EnvironmentTemplate","index":0}"#
    );
}

/// §4.5 item 3 (spec af53020): an attached Environment's PATH and
/// LIST[PATH] values written as format strings are used as given,
/// evaluated with POSIX semantics, whatever their form.
#[test]
fn an_attached_format_string_value_of_any_form_is_used_as_given() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: P, type: PATH}
  - {name: L, type: "LIST[PATH]", default: "{{ [path('C:/x')] }}"}
  - {name: M, type: "LIST[PATH]"}
  variables: {V: v}
"#;
    for (value, stored) in [
        (r#""{{ 'C:' }}/x""#, "C:/x"),
        (r#""{{ '/' }}x""#, "/x"),
        (r#""{{ 'rel' }}\\x""#, r"rel\x"),
        (r#""{{ 'C:' }}""#, "C:"),
    ] {
        let jt = format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{{environment: E, parameters: {{P: {value}, M: "{{{{ [string(path('D:\\m') / 'n'), 'rel'] }}}}"}}}}]
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        );
        let job = submission(&jt, &[env], &[])
            .create()
            .unwrap_or_else(|e| panic!("{value}: {e}"))
            .job;
        assert_eq!(
            rendered_values(&job.steps[0]),
            [format!(
                r#"EnvironmentTemplate[0]/E: L=list[path]:["C:/x"], M=list[path]:["D:\\m/n","rel"], P=path:{stored}"#
            )],
            "{value}"
        );
    }
}
