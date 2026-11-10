// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CreatedJob::environment_statuses`: for each Step, every Environment it
//! considered, in the Job's Environment order (attached Environments, then
//! `jobEnvironments`, then the Step's `stepEnvironments`), with whether its
//! Sessions enter it or which Template Schemas §7.4.1 decision left it out,
//! and the Step's `required: false` entries that named no Environment.
//!
//! - `Declined`: a `BY_DEFAULT` Environment a disabled `environment:` entry
//!   of the Step declines (§3.2 item 3.2, step 4); its `enableIf` is not
//!   evaluated.
//! - `Disabled`: its own `enableIf` is `false` for the Step (§4 item 6,
//!   step 6).
//! - `Superseded`: an earlier Environment of its `exclusiveGroup` remains
//!   in the Step's set (§4 item 7, step 6); its `enableIf` is not
//!   evaluated, so a `false` one still gives `Superseded`.
//!
//! An `ON_REQUEST` Environment the Step does not request is not listed.
//! Each item carries its `exclusiveGroup`, and a `Superseded` one the
//! group member the Step enters (`superseded_by`).

use openjd_model::job::Document;
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    create_job, create_job_with_environment_templates, AttachedEnvironmentTemplate, CreatedJob,
    EnvironmentScope, EnvironmentStatus, OptionalEntryNotFound, StepDisabledEnvironments,
    StepEnvironmentStatus, SupersedingEnvironment,
};

use crate::env_params_common::{env_ok, job_ok, job_params_with, template_ctx};

const HEAD: &str = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                    name: J\n";

/// An Environment Template whose Environment is `body`, a YAML flow
/// mapping's contents.
fn env(body: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {{{body}, variables: {{V: x}}}}\n"
    )
}

/// The `envParameterDefinitions` and `enableIf` of an Environment that is
/// enabled for a Step unless the Step's entry supplies `On: 0`.
const TOGGLE: &str = "envParameterDefinitions: [{name: On, type: INT, default: 1}], \
                      enableIf: \"Env.Param.On == 1\"";

/// Create the Job from `jt_src` with `env_srcs` attached, in order, each
/// labelled by the matching `labels` entry when there is one.
fn create_labelled(jt_src: &str, env_srcs: &[&str], labels: &[&str]) -> CreatedJob {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, &[], &envs);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = envs
        .iter()
        .enumerate()
        .map(|(i, e)| match labels.get(i) {
            Some(l) => AttachedEnvironmentTemplate::new(e).with_label(l),
            None => AttachedEnvironmentTemplate::new(e),
        })
        .collect();
    let created =
        create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
            .unwrap_or_else(|e| panic!("job creation failed:\n{e}"));
    check_invariants(&created);
    created
}

fn create(jt_src: &str, env_srcs: &[&str]) -> CreatedJob {
    create_labelled(jt_src, env_srcs, &[])
}

fn scope_name(scope: EnvironmentScope) -> &'static str {
    match scope {
        EnvironmentScope::Attached => "attached",
        EnvironmentScope::Job => "job",
        EnvironmentScope::Step => "step",
    }
}

/// Step `step`'s statuses, one `scope name@document: status` string each,
/// with ` [group]` after the document for one with an `exclusiveGroup`
/// and ` by scope name@document` after a superseded one's status.
fn statuses(created: &CreatedJob, step: usize) -> Vec<String> {
    created.environment_statuses[step]
        .environments
        .iter()
        .map(|s| {
            let status = match s.status {
                EnvironmentStatus::Entered => "entered",
                EnvironmentStatus::Declined => "declined",
                EnvironmentStatus::Disabled => "disabled",
                EnvironmentStatus::Superseded => "superseded",
            };
            let group = s
                .exclusive_group
                .as_ref()
                .map_or(String::new(), |g| format!(" [{g}]"));
            let by = s.superseded_by.as_ref().map_or(String::new(), |w| {
                format!(" by {} {}@{}", scope_name(w.scope), w.name, w.document)
            });
            format!(
                "{} {}@{}{group}: {status}{by}",
                scope_name(s.scope),
                s.name,
                s.document
            )
        })
        .collect()
}

fn not_found(created: &CreatedJob, step: usize) -> Vec<(usize, &str)> {
    created.environment_statuses[step]
        .not_found
        .iter()
        .map(|n| (n.dependency_index, n.environment.as_str()))
        .collect()
}

/// What every result must satisfy: one entry per Step; the `Entered`
/// Job-level ones are exactly the ones each Step's Sessions enter, and its
/// `Entered` Step-level ones its `step_environments`; and
/// `disabled_environments` is the `Disabled` and `Superseded` ones.
fn check_invariants(created: &CreatedJob) {
    let job = &created.job;
    assert_eq!(created.environment_statuses.len(), job.steps.len());
    for (step, s) in job.steps.iter().zip(&created.environment_statuses) {
        let entered: Vec<(&Document, &str)> = job
            .job_environments
            .iter()
            .flatten()
            .zip(&created.environment_documents)
            .filter(|(e, d)| step.enters_environment(d, &e.name))
            .map(|(e, d)| (d, e.name.as_str()))
            .collect();
        let entered_by_status: Vec<(&Document, &str)> = s
            .environments
            .iter()
            .filter(|s| s.scope != EnvironmentScope::Step)
            .filter(|s| s.status == EnvironmentStatus::Entered)
            .map(|s| (&s.document, s.name.as_str()))
            .collect();
        assert_eq!(entered, entered_by_status, "Step {}", step.name);
        let step_envs: Vec<&str> = step
            .step_environments
            .iter()
            .flatten()
            .map(|e| e.name.as_str())
            .collect();
        let step_entered: Vec<&str> = s
            .environments
            .iter()
            .filter(|s| s.scope == EnvironmentScope::Step)
            .filter(|s| s.status == EnvironmentStatus::Entered)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(step_envs, step_entered, "Step {}", step.name);
        for (k, st) in s.environments.iter().enumerate() {
            assert_eq!(
                st.scope == EnvironmentScope::Attached,
                !st.document.is_job_template(),
                "{st:?}"
            );
            // A Step Environment has no group; only a superseded one names
            // the Environment that superseded it.
            if st.scope == EnvironmentScope::Step {
                assert_eq!(st.exclusive_group, None, "{st:?}");
            }
            assert_eq!(
                st.superseded_by.is_some(),
                st.status == EnvironmentStatus::Superseded,
                "{st:?}"
            );
            // The winner is the first Entered member of the group, before
            // the superseded one in the Job's Environment order.
            if let Some(w) = &st.superseded_by {
                let group = st
                    .exclusive_group
                    .as_ref()
                    .expect("a superseded one has a group");
                let first = s.environments[..k]
                    .iter()
                    .find(|e| {
                        e.status == EnvironmentStatus::Entered
                            && e.exclusive_group.as_ref() == Some(group)
                    })
                    .unwrap_or_else(|| panic!("no entered member before {st:?}"));
                assert_eq!(
                    (&first.name, &first.document, first.scope),
                    (&w.name, &w.document, w.scope),
                    "Step {}: {st:?}",
                    step.name
                );
            }
        }
    }
    let derived: Vec<StepDisabledEnvironments> = created
        .environment_statuses
        .iter()
        .map(|s| {
            let left_out = |st: &&StepEnvironmentStatus| {
                matches!(
                    st.status,
                    EnvironmentStatus::Disabled | EnvironmentStatus::Superseded
                )
            };
            StepDisabledEnvironments {
                environments: s
                    .environments
                    .iter()
                    .filter(|st| st.scope != EnvironmentScope::Step)
                    .filter(left_out)
                    .map(|st| openjd_model::job::EnvironmentRef {
                        document: st.document.clone(),
                        environment: st.name.clone(),
                    })
                    .collect(),
                step_environments: s
                    .environments
                    .iter()
                    .filter(|st| st.scope == EnvironmentScope::Step)
                    .filter(left_out)
                    .map(|st| st.name.clone())
                    .collect(),
            }
        })
        .collect();
    assert_eq!(created.disabled_environments, derived);
}

// ── Without ENV_PARAMS ─────────────────────────────────────────────

const PLAIN: &str = "specificationVersion: jobtemplate-2023-09\nname: J\n\
                     jobEnvironments:\n- {name: A, variables: {V: a}}\n- {name: B, variables: {V: b}}\n\
                     steps:\n- name: S\n  stepEnvironments:\n  - {name: L, variables: {V: l}}\n  \
                     script: {actions: {onRun: {command: x}}}\n\
                     - name: T\n  script: {actions: {onRun: {command: x}}}\n";

#[test]
fn without_env_params_every_environment_is_entered() {
    // A Job Template without ENV_PARAMS has no activation, enableIf or
    // exclusiveGroup, so every Environment of each Step is entered.
    let created = create(PLAIN, &[]);
    assert_eq!(
        statuses(&created, 0),
        [
            "job A@JobTemplate: entered",
            "job B@JobTemplate: entered",
            "step L@JobTemplate: entered",
        ]
    );
    assert_eq!(
        statuses(&created, 1),
        ["job A@JobTemplate: entered", "job B@JobTemplate: entered"]
    );
    assert!(not_found(&created, 0).is_empty());
    // `create_job` is this route with nothing attached: the same Job.
    let jt = job_ok(PLAIN);
    let job = create_job(&jt, &job_params_with(&jt, &[], &[]), &template_ctx(&jt)).unwrap();
    assert_eq!(job, created.job);
}

#[test]
fn without_env_params_an_attached_environment_can_still_be_left_out() {
    // The attachments list ENV_PARAMS, so their own enableIf and group
    // apply even though the Job Template does not list it. (Without
    // ENV_PARAMS the Job Template has no `environment:` entries, so each
    // Step gets the same decisions.)
    let created = create(
        PLAIN,
        &[
            &env("name: Q1, enableIf: false"),
            &env("name: Q2, exclusiveGroup: G"),
            &env("name: Q3, exclusiveGroup: G"),
        ],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Q1@EnvironmentTemplate[0]: disabled",
            "attached Q2@EnvironmentTemplate[1] [G]: entered",
            "attached Q3@EnvironmentTemplate[2] [G]: superseded by attached Q2@EnvironmentTemplate[1]",
            "job A@JobTemplate: entered",
            "job B@JobTemplate: entered",
            "step L@JobTemplate: entered",
        ]
    );
    assert_eq!(
        statuses(&created, 1),
        [
            "attached Q1@EnvironmentTemplate[0]: disabled",
            "attached Q2@EnvironmentTemplate[1] [G]: entered",
            "attached Q3@EnvironmentTemplate[2] [G]: superseded by attached Q2@EnvironmentTemplate[1]",
            "job A@JobTemplate: entered",
            "job B@JobTemplate: entered",
        ]
    );
}

// ── Job Environments and Step Environments, nothing attached ───────

/// Job Environments: `Def` (BY_DEFAULT), `Req` (ON_REQUEST), `Off`, `G1`
/// and `G2` in group `G`, the last three each disabled for a Step whose
/// entry gives it `On: 0` ([`TOGGLE`]). Step S declines `Def`, requests
/// `Req`, and turns off `G2`; Step T turns off `Off` and `G1`, disables its
/// Step Environment `L`, and requests nothing.
fn job() -> String {
    format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: Def, variables: {{V: d}}}}\n\
         - {{name: Req, activation: ON_REQUEST, variables: {{V: r}}}}\n\
         - {{name: Off, {TOGGLE}, variables: {{V: o}}}}\n\
         - {{name: G1, exclusiveGroup: G, {TOGGLE}, variables: {{V: g}}}}\n\
         - {{name: G2, exclusiveGroup: G, {TOGGLE}, variables: {{V: g}}}}\n\
         steps:\n\
         - name: S\n  dependencies:\n  - {{environment: Def, enableIf: false}}\n  \
         - {{environment: Req}}\n  - {{environment: G2, parameters: {{On: 0}}}}\n  \
         stepEnvironments:\n  - {{name: L, variables: {{V: l}}}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: T\n  dependencies:\n  - {{environment: Off, parameters: {{On: 0}}}}\n  \
         - {{environment: G1, parameters: {{On: 0}}}}\n  stepEnvironments:\n  \
         - {{name: L, enableIf: \"Step.Name != 'T'\", variables: {{V: l}}}}\n  \
         - {{name: M, variables: {{V: m}}}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn job_and_step_environments_get_each_status() {
    let created = create(&job(), &[]);
    // S: Def declined, Req requested, Off enabled, G1 enabled and so
    // supersedes G2, whose enableIf (false for S) is not evaluated.
    assert_eq!(
        statuses(&created, 0),
        [
            "job Def@JobTemplate: declined",
            "job Req@JobTemplate: entered",
            "job Off@JobTemplate: entered",
            "job G1@JobTemplate [G]: entered",
            "job G2@JobTemplate [G]: superseded by job G1@JobTemplate",
            "step L@JobTemplate: entered",
        ]
    );
    // T: Req is ON_REQUEST and not requested, so it is not listed; Off and
    // G1 are disabled by their enableIf, so G2 is the group's first; the
    // Step Environment L is disabled, M entered.
    assert_eq!(
        statuses(&created, 1),
        [
            "job Def@JobTemplate: entered",
            "job Off@JobTemplate: disabled",
            "job G1@JobTemplate [G]: disabled",
            "job G2@JobTemplate [G]: entered",
            "step L@JobTemplate: disabled",
            "step M@JobTemplate: entered",
        ]
    );
}

#[test]
fn a_superseded_environment_with_a_false_enable_if_is_superseded() {
    // §7.4.1 step 6: once G1 remains in the set, G2's enableIf is not
    // evaluated, so its reason is the group, not the enableIf.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: G1, exclusiveGroup: G, variables: {{V: a}}}}\n\
         - {{name: G2, exclusiveGroup: G, enableIf: false, variables: {{V: b}}}}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(
        statuses(&create(&jt, &[]), 0),
        [
            "job G1@JobTemplate [G]: entered",
            "job G2@JobTemplate [G]: superseded by job G1@JobTemplate"
        ]
    );
}

#[test]
fn a_declined_environment_with_a_false_enable_if_is_declined() {
    // §3.2 item 3.2: a declined BY_DEFAULT Environment's own enableIf is
    // not evaluated for the Step, and it takes no group, so the next one of
    // its group is entered.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: G1, exclusiveGroup: G, enableIf: false, variables: {{V: a}}}}\n\
         - {{name: G2, exclusiveGroup: G, variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies:\n  - {{environment: G1, enableIf: false}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(
        statuses(&create(&jt, &[]), 0),
        [
            "job G1@JobTemplate [G]: declined",
            "job G2@JobTemplate [G]: entered"
        ]
    );
}

#[test]
fn an_on_request_environment_is_listed_only_when_requested() {
    // A disabled entry for an ON_REQUEST Environment has no effect: it is
    // neither requested nor declined, so it is not listed.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: R, activation: ON_REQUEST, variables: {{V: a}}}}\n\
         - {{name: A, activation: ALWAYS, variables: {{V: b}}}}\n\
         steps:\n\
         - name: S\n  dependencies:\n  - {{environment: R, enableIf: false}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: T\n  dependencies:\n  - {{environment: R}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: U\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(&jt, &[]);
    assert_eq!(statuses(&created, 0), ["job A@JobTemplate: entered"]);
    assert_eq!(
        statuses(&created, 1),
        ["job R@JobTemplate: entered", "job A@JobTemplate: entered"]
    );
    assert_eq!(statuses(&created, 2), ["job A@JobTemplate: entered"]);
}

// ── Attached Environment Templates ─────────────────────────────────

#[test]
fn a_group_spans_an_attachment_and_a_job_environment() {
    // §4 item 7: attached Environments come first in the Job's Environment
    // order, so an entered attachment supersedes a Job Environment of its
    // group; where the attachment's enableIf is false, the Job Environment
    // is the group's first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: Conda, exclusiveGroup: Soft, variables: {{V: c}}}}\n\
         steps:\n\
         - name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: T\n  dependencies:\n  - {{environment: Docker, parameters: {{On: 0}}}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create_labelled(
        &jt,
        &[&env(&format!(
            "name: Docker, exclusiveGroup: Soft, {TOGGLE}"
        ))],
        &["queue/docker.yaml"],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Docker@queue/docker.yaml [Soft]: entered",
            "job Conda@JobTemplate [Soft]: superseded by attached Docker@queue/docker.yaml",
        ]
    );
    assert_eq!(
        statuses(&created, 1),
        [
            "attached Docker@queue/docker.yaml [Soft]: disabled",
            "job Conda@JobTemplate [Soft]: entered",
        ]
    );
    assert_eq!(
        created.environment_statuses[0].environments[0].document,
        Document::environment_template(0, Some("queue/docker.yaml"))
    );
}

#[test]
fn attached_environments_get_each_status() {
    // Q1 ALWAYS; Q2 BY_DEFAULT declined by S; Q3 ON_REQUEST requested by
    // S only; Q4 disabled for T by its enableIf; Q5 and Q6 in one group,
    // Q6 superseded although its enableIf is false.
    let jt = format!(
        "{HEAD}steps:\n\
         - name: S\n  dependencies:\n  - {{environment: Q2, enableIf: false}}\n  \
         - {{environment: Q3}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: T\n  dependencies:\n  - {{environment: Q4, parameters: {{On: 0}}}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(
        &jt,
        &[
            &env("name: Q1, activation: ALWAYS"),
            &env("name: Q2"),
            &env("name: Q3, activation: ON_REQUEST"),
            &env(&format!("name: Q4, {TOGGLE}")),
            &env("name: Q5, exclusiveGroup: G"),
            &env("name: Q6, exclusiveGroup: G, enableIf: false"),
        ],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Q1@EnvironmentTemplate[0]: entered",
            "attached Q2@EnvironmentTemplate[1]: declined",
            "attached Q3@EnvironmentTemplate[2]: entered",
            "attached Q4@EnvironmentTemplate[3]: entered",
            "attached Q5@EnvironmentTemplate[4] [G]: entered",
            "attached Q6@EnvironmentTemplate[5] [G]: superseded by attached Q5@EnvironmentTemplate[4]",
        ]
    );
    assert_eq!(
        statuses(&created, 1),
        [
            "attached Q1@EnvironmentTemplate[0]: entered",
            "attached Q2@EnvironmentTemplate[1]: entered",
            "attached Q4@EnvironmentTemplate[3]: disabled",
            "attached Q5@EnvironmentTemplate[4] [G]: entered",
            "attached Q6@EnvironmentTemplate[5] [G]: superseded by attached Q5@EnvironmentTemplate[4]",
        ]
    );
}

#[test]
fn job_and_step_environments_are_disabled_with_attachments_present() {
    // The Job and Step Environments' statuses do not depend on whether
    // Environment Templates are attached: Step T turns off Off and G1 and
    // disables L with an attachment in front of them, which takes no part
    // in their groups.
    let created = create_labelled(&job(), &[&env("name: Q")], &["queue/q.yaml"]);
    assert_eq!(
        statuses(&created, 1),
        [
            "attached Q@queue/q.yaml: entered",
            "job Def@JobTemplate: entered",
            "job Off@JobTemplate: disabled",
            "job G1@JobTemplate [G]: disabled",
            "job G2@JobTemplate [G]: entered",
            "step L@JobTemplate: disabled",
            "step M@JobTemplate: entered",
        ]
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Q@queue/q.yaml: entered",
            "job Def@JobTemplate: declined",
            "job Req@JobTemplate: entered",
            "job Off@JobTemplate: entered",
            "job G1@JobTemplate [G]: entered",
            "job G2@JobTemplate [G]: superseded by job G1@JobTemplate",
            "step L@JobTemplate: entered",
        ]
    );
}

#[test]
fn a_disabled_attachment_does_not_supersede_the_next() {
    let jt = format!("{HEAD}steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n");
    let created = create(
        &jt,
        &[
            &env("name: Q1, exclusiveGroup: G, enableIf: false"),
            &env("name: Q2, exclusiveGroup: G"),
            &env("name: Q3, exclusiveGroup: G"),
        ],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Q1@EnvironmentTemplate[0] [G]: disabled",
            "attached Q2@EnvironmentTemplate[1] [G]: entered",
            "attached Q3@EnvironmentTemplate[2] [G]: superseded by attached Q2@EnvironmentTemplate[1]",
        ]
    );
}

#[test]
fn every_part_of_the_job_environment_order_is_in_order() {
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: J2, variables: {{V: a}}}}\n- {{name: J1, variables: {{V: b}}}}\n\
         steps:\n- name: S\n  stepEnvironments:\n  - {{name: S2, variables: {{V: c}}}}\n  \
         - {{name: S1, variables: {{V: d}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(&jt, &[&env("name: Q2"), &env("name: Q1")]);
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Q2@EnvironmentTemplate[0]: entered",
            "attached Q1@EnvironmentTemplate[1]: entered",
            "job J2@JobTemplate: entered",
            "job J1@JobTemplate: entered",
            "step S2@JobTemplate: entered",
            "step S1@JobTemplate: entered",
        ]
    );
}

// ── exclusiveGroup and superseded_by ────────────────────────────────

#[test]
fn each_group_in_the_job_template_names_its_own_winner() {
    // Two interleaved groups and an ungrouped Environment. Step S enters
    // the first of each group, A1 and B1, and every later member names the
    // first of its own group. Step T turns off A1, so A2 wins G and A3
    // names A2, while H is unchanged.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: A1, exclusiveGroup: G, {TOGGLE}, variables: {{V: a}}}}\n\
         - {{name: B1, exclusiveGroup: H, variables: {{V: b}}}}\n\
         - {{name: U, variables: {{V: u}}}}\n\
         - {{name: A2, exclusiveGroup: G, variables: {{V: a}}}}\n\
         - {{name: B2, exclusiveGroup: H, variables: {{V: b}}}}\n\
         - {{name: A3, exclusiveGroup: G, variables: {{V: a}}}}\n\
         steps:\n\
         - name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: T\n  dependencies:\n  - {{environment: A1, parameters: {{On: 0}}}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(&jt, &[]);
    assert_eq!(
        statuses(&created, 0),
        [
            "job A1@JobTemplate [G]: entered",
            "job B1@JobTemplate [H]: entered",
            "job U@JobTemplate: entered",
            "job A2@JobTemplate [G]: superseded by job A1@JobTemplate",
            "job B2@JobTemplate [H]: superseded by job B1@JobTemplate",
            "job A3@JobTemplate [G]: superseded by job A1@JobTemplate",
        ]
    );
    assert_eq!(
        statuses(&created, 1),
        [
            "job A1@JobTemplate [G]: disabled",
            "job B1@JobTemplate [H]: entered",
            "job U@JobTemplate: entered",
            "job A2@JobTemplate [G]: entered",
            "job B2@JobTemplate [H]: superseded by job B1@JobTemplate",
            "job A3@JobTemplate [G]: superseded by job A2@JobTemplate",
        ]
    );
    // The fields themselves, for an ungrouped one, a disabled one, and a
    // superseded one.
    let t = &created.environment_statuses[1].environments;
    assert_eq!((&t[2].exclusive_group, &t[2].superseded_by), (&None, &None));
    assert_eq!(
        (&t[0].exclusive_group, &t[0].superseded_by),
        (&Some("G".to_string()), &None)
    );
    assert_eq!(t[5].exclusive_group.as_deref(), Some("G"));
    assert_eq!(
        t[5].superseded_by,
        Some(SupersedingEnvironment {
            name: "A2".into(),
            document: Document::JobTemplate,
            scope: EnvironmentScope::Job,
        })
    );
}

#[test]
fn a_group_whose_first_member_is_disabled_supersedes_nothing() {
    // §4 item 7: a member its own enableIf disables takes no group, so the
    // next one is entered and no member is superseded.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: G1, exclusiveGroup: G, enableIf: false, variables: {{V: a}}}}\n\
         - {{name: G2, exclusiveGroup: G, variables: {{V: b}}}}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(&jt, &[]);
    assert_eq!(
        statuses(&created, 0),
        [
            "job G1@JobTemplate [G]: disabled",
            "job G2@JobTemplate [G]: entered"
        ]
    );
    assert!(created.environment_statuses[0]
        .environments
        .iter()
        .all(|s| s.superseded_by.is_none()));
}

#[test]
fn the_winner_is_told_apart_from_a_job_environment_of_the_same_name() {
    // An attachment named like a Job Environment is unreachable by entries
    // (§3.2.1 item 4, a warning), but it is still an Environment of the
    // Job. Here it shares the Job Environment's group and comes first, so
    // the Job Template's Conda is superseded by the attached Conda, and
    // `superseded_by` says which by document and scope. A third member,
    // in the Job Template after an ungrouped one, names the same winner.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: Conda, exclusiveGroup: Soft, variables: {{V: c}}}}\n\
         - {{name: U, variables: {{V: u}}}}\n\
         - {{name: Pip, exclusiveGroup: Soft, variables: {{V: p}}}}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create_labelled(
        &jt,
        &[&env("name: Conda, exclusiveGroup: Soft")],
        &["queue/conda.yaml"],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Conda@queue/conda.yaml [Soft]: entered",
            "job Conda@JobTemplate [Soft]: superseded by attached Conda@queue/conda.yaml",
            "job U@JobTemplate: entered",
            "job Pip@JobTemplate [Soft]: superseded by attached Conda@queue/conda.yaml",
        ]
    );
    let winner = SupersedingEnvironment {
        name: "Conda".into(),
        document: Document::environment_template(0, Some("queue/conda.yaml")),
        scope: EnvironmentScope::Attached,
    };
    let s = &created.environment_statuses[0].environments;
    assert_eq!(s[1].superseded_by.as_ref(), Some(&winner));
    assert_ne!(
        (&s[1].document, s[1].scope),
        (&winner.document, winner.scope)
    );
    assert_eq!(s[3].superseded_by.as_ref(), Some(&winner));
}

#[test]
fn a_job_environment_supersedes_a_later_one_after_a_disabled_attachment() {
    // The group spans two attachments and two Job Environments. The first
    // attachment is disabled and the second declined, so the first Job
    // Environment wins and supersedes the second, at its position in the
    // Job's Environment order after the attachments.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: J1, exclusiveGroup: G, variables: {{V: a}}}}\n\
         - {{name: J2, exclusiveGroup: G, variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies:\n  - {{environment: Q2, enableIf: false}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(
        &jt,
        &[
            &env("name: Q1, exclusiveGroup: G, enableIf: false"),
            &env("name: Q2, exclusiveGroup: G"),
        ],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Q1@EnvironmentTemplate[0] [G]: disabled",
            "attached Q2@EnvironmentTemplate[1] [G]: declined",
            "job J1@JobTemplate [G]: entered",
            "job J2@JobTemplate [G]: superseded by job J1@JobTemplate",
        ]
    );
}

#[test]
fn every_later_member_names_the_first_attachment_of_its_group() {
    // A group of three attachments and one Job Environment: the first
    // attachment takes the group, and each later member, attached or Job,
    // names it, not the member just before it.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: J, exclusiveGroup: G, variables: {{V: j}}}}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(
        &jt,
        &[
            &env("name: A1, exclusiveGroup: G"),
            &env("name: A2, exclusiveGroup: G"),
            &env("name: A3, exclusiveGroup: G"),
        ],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached A1@EnvironmentTemplate[0] [G]: entered",
            "attached A2@EnvironmentTemplate[1] [G]: superseded by attached A1@EnvironmentTemplate[0]",
            "attached A3@EnvironmentTemplate[2] [G]: superseded by attached A1@EnvironmentTemplate[0]",
            "job J@JobTemplate [G]: superseded by attached A1@EnvironmentTemplate[0]",
        ]
    );
}

#[test]
fn two_attached_groups_each_name_their_own_winner() {
    // The second group is taken after the first, by a later attachment, so
    // its superseded member must name that attachment, not the first
    // group's winner.
    let jt = format!("{HEAD}steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n");
    let created = create(
        &jt,
        &[
            &env("name: G1, exclusiveGroup: G"),
            &env("name: H1, exclusiveGroup: H"),
            &env("name: G2, exclusiveGroup: G"),
            &env("name: H2, exclusiveGroup: H"),
        ],
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached G1@EnvironmentTemplate[0] [G]: entered",
            "attached H1@EnvironmentTemplate[1] [H]: entered",
            "attached G2@EnvironmentTemplate[2] [G]: superseded by attached G1@EnvironmentTemplate[0]",
            "attached H2@EnvironmentTemplate[3] [H]: superseded by attached H1@EnvironmentTemplate[1]",
        ]
    );
}

#[test]
fn an_on_request_member_takes_its_group_only_when_requested() {
    // An ON_REQUEST member the Step does not request is not considered, so
    // it takes no group and the next member wins; one it requests wins.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: J, exclusiveGroup: G, variables: {{V: j}}}}\n\
         steps:\n\
         - name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: T\n  dependencies:\n  - {{environment: R}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(
        &jt,
        &[&env("name: R, activation: ON_REQUEST, exclusiveGroup: G")],
    );
    assert_eq!(statuses(&created, 0), ["job J@JobTemplate [G]: entered"]);
    assert_eq!(
        statuses(&created, 1),
        [
            "attached R@EnvironmentTemplate[0] [G]: entered",
            "job J@JobTemplate [G]: superseded by attached R@EnvironmentTemplate[0]",
        ]
    );
}

// ── Optional entries that name nothing ─────────────────────────────

/// Step S: an enabled optional entry for `Gpu` (index 0), a required one
/// for `Docker` (index 1), a disabled optional one for `Licenses` (index
/// 2), and an enabled optional one for `Cache` (index 3).
const OPTIONAL: &str =
    "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                        name: J\nsteps:\n\
                        - name: S\n  dependencies:\n  \
                        - {environment: Gpu, required: false}\n  \
                        - {environment: Docker}\n  \
                        - {environment: Licenses, required: false, enableIf: false}\n  \
                        - {environment: Cache, required: false}\n  \
                        script: {actions: {onRun: {command: x}}}\n";

#[test]
fn an_optional_entry_that_names_nothing_is_not_found() {
    // Docker is attached; Gpu and Cache name nothing. A disabled entry that
    // names nothing has no effect at all (§3.2.1), so Licenses is not
    // listed.
    let created = create(OPTIONAL, &[&env("name: Docker, activation: ON_REQUEST")]);
    assert_eq!(not_found(&created, 0), [(0, "Gpu"), (3, "Cache")]);
    assert_eq!(
        statuses(&created, 0),
        ["attached Docker@EnvironmentTemplate[0]: entered"]
    );
    // Each one also produced a warning.
    let warning = |i: usize, name: &str| {
        format!(
            "JobTemplate -> steps[0] -> dependencies[{i}] -> environment: names Environment \
             '{name}', which is not a jobEnvironments entry of the Job Template, and none of the \
             attached Environment Templates (EnvironmentTemplate[0]) defines it. The entry has \
             'required: false', so it has no effect."
        )
    };
    let warnings: Vec<String> = created.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(warnings, [warning(0, "Gpu"), warning(3, "Cache")]);
    // An optional entry that names an attachment is not in the list, even
    // when the attachment is then disabled for the Step.
    let created = create(
        OPTIONAL,
        &[
            &env("name: Docker, activation: ON_REQUEST"),
            &env("name: Gpu, activation: ON_REQUEST, enableIf: false"),
        ],
    );
    assert_eq!(
        created.environment_statuses[0].not_found,
        [OptionalEntryNotFound {
            dependency_index: 3,
            environment: "Cache".into(),
        }]
    );
    assert_eq!(
        statuses(&created, 0),
        [
            "attached Docker@EnvironmentTemplate[0]: entered",
            "attached Gpu@EnvironmentTemplate[1]: disabled",
        ]
    );
}

#[test]
fn an_optional_entry_names_nothing_with_no_attachment() {
    // The `create_job` route: nothing attached, so every optional entry
    // naming no Job Environment is not found.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: A, activation: ON_REQUEST, variables: {{V: a}}}}\n\
         steps:\n- name: S\n  dependencies:\n  - {{environment: A}}\n  \
         - {{environment: Gpu, required: false}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create(&jt, &[]);
    assert_eq!(not_found(&created, 0), [(1, "Gpu")]);
    let warnings: Vec<String> = created.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(
        warnings,
        [
            "steps[0] -> dependencies[1] -> environment: names Environment 'Gpu', which is not a \
          jobEnvironments entry of the Job Template, and no Environment Template is attached. The \
          entry has 'required: false', so it has no effect."
        ]
    );
    assert_eq!(statuses(&created, 0), ["job A@JobTemplate: entered"]);
}
