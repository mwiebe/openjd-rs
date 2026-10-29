// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Environment template per spec §1.2.

use super::constrained_strings::ExtensionName;
use super::environment::Environment;
use super::parameters::JobParameterDefinition;
use serde::Deserialize;

/// §1.2 EnvironmentTemplate
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct EnvironmentTemplate {
    pub specification_version: String,
    /// §1.2 item 2 `$schema`: accepted and ignored, for JSON-editing IDEs.
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub environment: Environment,
}

impl EnvironmentTemplate {
    #[allow(
        missing_docs,
        reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
    )]
    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    /// The template's own profile: its specification revision and the
    /// extensions it declares, as [`JobTemplate::profile`](super::JobTemplate::profile)
    /// gives a Job Template's. An extension applies to the document that
    /// lists it (Template Schemas §1.2 item 3), so a submission evaluates
    /// each attached Environment Template under its own profile.
    pub fn profile(&self) -> crate::ModelProfile {
        use std::str::FromStr;
        let revision =
            crate::types::TemplateSpecificationVersion::from_str(&self.specification_version)
                .map(|v| v.revision())
                // A decoded template has a known version; fall back to the
                // first revision as `JobTemplate::profile` does.
                .unwrap_or(crate::types::SpecificationRevision::V2023_09);
        let mut exts = crate::types::Extensions::new();
        for e in self.extensions.iter().flatten() {
            if let Ok(known) = crate::types::ModelExtension::from_str(e.as_str()) {
                exts.insert(known);
            }
        }
        crate::ModelProfile::new(revision).with_extensions(exts)
    }

    /// [`profile`](Self::profile) wrapped in a
    /// [`ValidationContext`](crate::types::ValidationContext) with default
    /// caller limits and path policy.
    pub fn default_validation_context(&self) -> crate::types::ValidationContext {
        crate::types::ValidationContext::from_profile(self.profile())
    }
}
