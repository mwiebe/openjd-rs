// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! v2023-09 schema model types.

pub mod parse;

mod actions;
mod constrained_strings;
mod environment;
mod environment_template;
mod expr_parameters;
mod host_requirements;
mod job_template;
mod parameters;
mod step;
mod task_parameters;
pub(crate) mod validate_v2023_09;
pub(crate) mod validation;

// job_template
pub use job_template::JobTemplate;
// environment_template
pub use environment_template::EnvironmentTemplate;
// parameters
pub(crate) use parameters::check_value_char_len;
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
    ListIntUserInterface, ListPathUserInterface, ListSimpleUserInterface, RangeExprUserInterface,
};
#[cfg(test)]
pub use expr_parameters::{
    ListFloatItemConstraints, ListIntItemConstraints, ListListIntItemConstraints,
    ListStringItemConstraints,
};
// step
pub use step::{SimpleAction, SimpleActionKind, StepDependency, StepScript, StepTemplate};
// environment
pub use environment::{EmbeddedFile, Environment, EnvironmentScript};
// actions
pub(crate) use actions::impl_environment_actions_helpers;
pub use actions::{Action, CancelationMode, EnvironmentActions, StepActions, WrapHookScope};
// host_requirements
pub use host_requirements::{AmountRequirement, AttributeRequirement, HostRequirements};
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

#[cfg(test)]
pub(crate) mod test_support {
    use crate::template::JobTemplate;

    /// A job template whose one Step has a task parameter of every type, named
    /// for its type (`I`, `F`, `S`, `P`, `C` for CHUNK[INT]).
    pub(crate) fn template_with_every_task_parameter_type() -> JobTemplate {
        let v: serde_json::Value = serde_json::json!({
            "specificationVersion": "jobtemplate-2023-09",
            "extensions": ["EXPR", "TASK_CHUNKING"],
            "name": "every-task-parameter-type",
            "steps": [{
                "name": "S",
                "parameterSpace": {"taskParameterDefinitions": [
                    {"name": "I", "type": "INT", "range": [1]},
                    {"name": "F", "type": "FLOAT", "range": [1.5]},
                    {"name": "S", "type": "STRING", "range": ["s"]},
                    {"name": "P", "type": "PATH", "range": ["/p"]},
                    {"name": "C", "type": "CHUNK[INT]", "range": "1-3",
                     "chunks": {"defaultTaskCount": 2, "rangeConstraint": "CONTIGUOUS"}}
                ]},
                "script": {"actions": {"onRun": {"command": "echo"}}}
            }]
        });
        crate::decode_job_template(
            v,
            Some(&["EXPR", "TASK_CHUNKING"]),
            &crate::CallerLimits::default(),
        )
        .unwrap()
    }
}
