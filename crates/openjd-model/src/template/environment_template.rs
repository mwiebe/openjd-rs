// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Environment template per spec §1.2.

use super::constrained_strings::ExtensionName;
use super::environment::Environment;
use super::parameters::JobParameterDefinition;
use super::service::Service;
use serde::Deserialize;

/// §1.2 EnvironmentTemplate
///
/// Defines an Environment, a list of Services (`SERVICE` extension, RFC
/// 0009), or both; template validation rejects a document that defines
/// neither.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTemplate {
    pub specification_version: String,
    /// §1.2 item 2 — ignored, as on the Job Template; allowed for
    /// compatibility with JSON-editing IDEs.
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    /// §1.2 item 3 — the extensions this document uses. Applies to this
    /// document only; a Job Template need not list the extensions used by
    /// the Environment Templates a scheduler applies to it, or vice versa.
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    /// §1.2 item 5 — the Environment this template defines. Optional since
    /// RFC 0009; at least one of `environment` or `services` is required.
    pub environment: Option<Environment>,
    /// §1.2 item 6 (RFC 0009) — the Services this template defines, which a
    /// scheduler applies per Job with Job scope ("external Services", see
    /// §1.2.2). Requires the `SERVICE` extension; same list constraints as a
    /// Job Template's `services`; a Service here gives no `dependencies`.
    pub services: Option<Vec<Service>>,
}

impl EnvironmentTemplate {
    /// The Environment this template defines, if any.
    pub fn environment(&self) -> Option<&Environment> {
        self.environment.as_ref()
    }

    /// The Services this template defines, in declaration order; empty when
    /// `services` is absent.
    pub fn services(&self) -> &[Service] {
        self.services.as_deref().unwrap_or_default()
    }

    /// The [`ModelProfile`](crate::ModelProfile) this document declares:
    /// its specification revision plus the extensions in its `extensions`
    /// list (§1.2 item 3). An extension listed here applies to this
    /// document only, so this is the profile under which the document's
    /// own Environment and Services are evaluated when a submission
    /// applies the template to a Job — the counterpart of
    /// [`JobTemplate::profile`](crate::template::JobTemplate::profile).
    ///
    /// Entries that don't parse as a known
    /// [`ModelExtension`](crate::types::ModelExtension) are silently
    /// skipped, as on the Job Template.
    pub fn profile(&self) -> crate::ModelProfile {
        use std::str::FromStr;
        let revision =
            crate::types::TemplateSpecificationVersion::from_str(&self.specification_version)
                .map(|v| v.revision())
                // Unknown spec versions shouldn't reach this point (the
                // template was validated). Fall back to the first revision.
                .unwrap_or(crate::types::SpecificationRevision::V2023_09);
        let mut exts = crate::types::Extensions::new();
        if let Some(list) = &self.extensions {
            for e in list {
                if let Ok(known) = crate::types::ModelExtension::from_str(e.as_str()) {
                    exts.insert(known);
                }
            }
        }
        crate::ModelProfile::new(revision).with_extensions(exts)
    }

    /// Convenience: wrap [`profile`](Self::profile) in a
    /// [`ValidationContext`](crate::types::ValidationContext) with default
    /// caller limits — the "do what the document says" context. Callers
    /// that set caller limits at decode should carry them here with
    /// [`with_caller_limits`](crate::types::ValidationContext::with_caller_limits).
    pub fn default_validation_context(&self) -> crate::types::ValidationContext {
        crate::types::ValidationContext::from_profile(self.profile())
    }
}
