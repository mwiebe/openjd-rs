// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Shared helper for the SERVICE (RFC 0009) submission tests: submits a Job
//! Template with Environment Templates attached through
//! `create_job_with_environment_templates` and splits what the attachments
//! contributed out of the combined Job, the way these tests read it.

use openjd_model::job;
use openjd_model::template::JobTemplate;
use openjd_model::{
    create_job_with_environment_templates, AttachedEnvironmentTemplate, CreatedJob,
    JobParameterValues, ModelError, RequirementBinding,
};

/// What a set of Environment Templates contributed to the combined Job of
/// one submission, in the order the submission combines them (Template
/// Schemas §1.2.2 item 1).
#[derive(Debug)]
pub(crate) struct Applied {
    /// The external Services: the combined Job's Services that an attached
    /// document declares.
    pub(crate) external_services: Vec<job::Service>,
    /// [`CreatedJob::requirement_bindings`].
    pub(crate) requirement_bindings: Vec<RequirementBinding>,
    /// The attached Environments: the combined Job's Job Environments that
    /// an attached document declares.
    pub(crate) environments: Vec<job::Environment>,
    /// The document of each entry of `environments`, index for index.
    pub(crate) environment_documents: Vec<job::Document>,
    created: CreatedJob,
}

impl Applied {
    /// The combined Job. `_job` is the Job created from the Job Template
    /// alone, which the combination folds the attachments into.
    pub(crate) fn into_combined_job(self, _job: job::Job) -> job::Job {
        self.created.job
    }

    /// The document of every entry of the combined `job_environments`.
    pub(crate) fn combined_environment_documents(&self, _job: &job::Job) -> Vec<job::Document> {
        self.created.environment_documents.clone()
    }
}

/// Submit `jt` with `attached`, using the template's own validation
/// context.
pub(crate) fn apply(
    jt: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    processed: &JobParameterValues,
) -> Result<Applied, ModelError> {
    let created = create_job_with_environment_templates(
        jt,
        attached,
        processed,
        &jt.default_validation_context(),
    )?;
    let external_services = created
        .job
        .services
        .iter()
        .flatten()
        .filter(|s| !s.document.is_job_template())
        .cloned()
        .collect();
    let (environments, environment_documents) = created
        .job
        .job_environments
        .iter()
        .flatten()
        .zip(&created.environment_documents)
        .filter(|(_, d)| !d.is_job_template())
        .map(|(e, d)| (e.clone(), d.clone()))
        .unzip();
    Ok(Applied {
        external_services,
        requirement_bindings: created.requirement_bindings.clone(),
        environments,
        environment_documents,
        created,
    })
}
