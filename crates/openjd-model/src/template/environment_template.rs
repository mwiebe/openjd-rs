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
    /// Job Template's `jobServices`.
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
}
