// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! v2023-09 schema model types.

pub mod parse;

mod decode_errors;

mod actions;
mod constrained_strings;
mod env_parameters;
mod environment;
mod environment_template;
pub(crate) mod expr_field;
mod expr_parameters;
mod host_requirements;
mod job_template;
mod parameters;
mod service;
pub mod service_scope;
mod step;
mod task_parameters;
pub(crate) mod validate_v2023_09;
pub(crate) mod validation;

// job_template
pub use job_template::JobTemplate;
// environment_template
pub use environment_template::EnvironmentTemplate;
// parameters
pub use parameters::{
    FileFilter, FloatUserInterface, IntUserInterface, JobFloatParameterDefinition,
    JobIntParameterDefinition, JobParameterDefinition, JobPathParameterDefinition,
    JobStringParameterDefinition, PathUserInterface, StringUserInterface,
};
pub use parameters::{FlexFloat, FlexInt};
// expr_parameters (EXPR-extension job parameter types)
pub use expr_parameters::{
    BoolUserInterface, HiddenOnlyUserInterface, JobBoolParameterDefinition,
    JobListBoolParameterDefinition, JobListFloatParameterDefinition, JobListIntParameterDefinition,
    JobListListIntParameterDefinition, JobListPathParameterDefinition,
    JobListStringParameterDefinition, JobRangeExprParameterDefinition, ListFloatUserInterface,
    ListIntUserInterface, ListPathUserInterface, ListSimpleUserInterface, RangeExprLengthCheck,
    RangeExprUserInterface,
};
#[cfg(test)]
pub use expr_parameters::{
    ListFloatItemConstraints, ListIntItemConstraints, ListListIntItemConstraints,
    ListStringItemConstraints,
};
// step
pub use step::{
    listed_service_names, listed_step_names, lists_service, DependencyTarget, SimpleAction,
    SimpleActionKind, StepDependency, StepKey, StepScript, StepTemplate,
};
// env_parameters (ENV_PARAMS extension)
pub use env_parameters::{
    EnvironmentParameterDefinition, EnvironmentParameterValue, EnvironmentParameterValueMap,
};
// expr_field (Template Schemas §7.6)
pub use expr_field::ExprField;
// environment
pub(crate) use environment::{apply_exclusive_groups, Known, KnownEntry};
pub use environment::{EmbeddedFile, Environment, EnvironmentActivation, EnvironmentScript};
// service (SERVICE extension, RFC 0009)
pub use service::{
    CompletedTasksPolicy, Service, ServiceActions, ServiceHealthCheck, ServicePort,
    ServicePortProtocol, ServiceRequirement, ServiceRequirementPort, ServiceRestartPolicy,
    ServiceScript, SERVICE_HEALTH_CHECK_NUMERIC_FIELDS,
};
pub use service_scope::{
    compute_service_scopes, environment_template_service_cycle, listed_requirements,
    listed_services, ComputedServiceScope, CycleViaEnvironment, Entrance, Entrances,
    ServiceDependencyCycle, ServiceScope, ServiceScopes,
};
// actions
pub(crate) use actions::impl_environment_actions_helpers;
pub use actions::{Action, CancelationMode, EnvironmentActions, StepActions, WrapHookScope};
// host_requirements
pub use host_requirements::{
    AmountRequirement, AttributeRequirement, AttributeValues, HostRequirements,
};
// task_parameters
pub use task_parameters::{
    ChunkIntTaskParameterDefinition, ChunksDefinition, FloatRange, FloatRangeItem,
    FloatTaskParameterDefinition, IntOrFormatString, IntRange, IntTaskParameterDefinition,
    PathTaskParameterDefinition, RangeConstraint, StepParameterSpaceDefinition, StringRange,
    StringTaskParameterDefinition, TaskParameterDefinition,
};
// constrained_strings
#[cfg(test)]
pub use constrained_strings::Identifier;
pub use constrained_strings::{Description, ExtensionName};
