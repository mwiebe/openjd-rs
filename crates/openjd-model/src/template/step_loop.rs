// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Multi-Step loops (`CONTROL_FLOW`, RFC 0013 §Schema modifications).
//!
//! With RFC 0013, an entry of a Job Template's `steps` list is a
//! [`StepEntry`]: a `<StepLoop>` when the mapping has a `loop` key, and a
//! `<StepTemplate>` otherwise. A loop's body is a list of `<StepTemplate>`s,
//! and a body entry with a `loop` key is rejected because loops do not nest.
//!
//! These are template types. `create_job` turns each loop into a
//! [`job::Loop`](crate::job::Loop), and the loop functions in
//! [`create_job`](crate::create_job) run it.

use serde::Deserialize;

use super::constrained_strings::Description;
use super::expr_field::ExprField;
use super::output_variables::{LoopOutputVariableDefinition, LoopVariableDefinition};
use super::step::{StepDependency, StepTemplate};
use super::task_parameters::IntOrFormatString;
use crate::error::PathElement;

/// One entry of a Job Template's `steps` list (RFC 0013 §Job Template):
/// `steps: [ <StepTemplate> | <StepLoop>, ... ]`.
///
/// An entry whose mapping has a `loop` key is a [`StepLoop`], and every other
/// entry is a [`StepTemplate`]. The choice is made on the key alone, so a
/// malformed entry gets the errors of the shape it chose (an unknown field of
/// a loop, for example `script`), not an error that names neither shape.
/// A loop's error is prefixed `loop '<name>': `, and an unknown field's
/// with `steps entry '<name>' has a 'loop' field, so it is a loop
/// (CONTROL_FLOW, RFC 0013), which has no such field: `.
/// Without `CONTROL_FLOW` a `loop` entry still parses as a `StepLoop`, and
/// template validation reports it at `steps[i] -> loop`.
// A Step is much larger than a loop, and nearly every entry is one; boxing
// it would add an indirection to every Step of every template to save space
// in the few entries that are loops.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum StepEntry {
    /// A Step (`<StepTemplate>`).
    Step(StepTemplate),
    /// A loop (`<StepLoop>`, RFC 0013).
    Loop(StepLoop),
}

impl<'de> Deserialize<'de> for StepEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = serde_json::Value::deserialize(deserializer)?;
        if has_loop_key(&value) {
            // Name the loop, and for an unknown field the `loop` field that
            // decided the shape, so that an error such as an unknown
            // `script` explains itself, with or without CONTROL_FLOW.
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|n| format!(" '{n}'"))
                .unwrap_or_default();
            serde_json::from_value(value).map(Self::Loop).map_err(|e| {
                let e = e.to_string();
                // Only an unknown field of the `<StepLoop>` itself, whose
                // list of expected fields includes `loop`; not one of its
                // `loop` mapping or of a body Step.
                let own_unknown_field =
                    e.starts_with("unknown field") && e.contains(STEP_LOOP_FIELDS);
                D::Error::custom(if own_unknown_field {
                    format!(
                        "steps entry{name} has a 'loop' field, so it is a loop (CONTROL_FLOW, RFC 0013), which has no such field: {e}"
                    )
                } else {
                    format!("loop{name}: {e}")
                })
            })
        } else {
            serde_json::from_value(value)
                .map(Self::Step)
                .map_err(D::Error::custom)
        }
    }
}

/// The end of serde's unknown-field message for a `<StepLoop>`: the fields
/// it expects.
const STEP_LOOP_FIELDS: &str =
    "expected one of `name`, `description`, `enableIf`, `dependencies`, `loop`, `outputVariables`";

/// Whether a `steps` element is a loop: a mapping with a `loop` key.
fn has_loop_key(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.contains_key("loop"))
}

impl StepEntry {
    /// The entry's name: the Step's or the loop's.
    pub fn name(&self) -> &str {
        match self {
            Self::Step(s) => &s.name,
            Self::Loop(l) => &l.name,
        }
    }

    /// The entry's `dependencies`, empty when there are none.
    pub fn dependencies(&self) -> &[StepDependency] {
        match self {
            Self::Step(s) => s.dependencies.as_deref().unwrap_or(&[]),
            Self::Loop(l) => l.dependencies.as_deref().unwrap_or(&[]),
        }
    }

    /// The entry's `enableIf`, if any.
    pub fn enable_if(&self) -> Option<&ExprField> {
        match self {
            Self::Step(s) => s.enable_if.as_ref(),
            Self::Loop(l) => l.enable_if.as_ref(),
        }
    }

    /// The Step, when this entry is one.
    pub fn as_step(&self) -> Option<&StepTemplate> {
        match self {
            Self::Step(s) => Some(s),
            Self::Loop(_) => None,
        }
    }

    /// The loop, when this entry is one.
    pub fn as_loop(&self) -> Option<&StepLoop> {
        match self {
            Self::Step(_) => None,
            Self::Loop(l) => Some(l),
        }
    }
}

/// RFC 0013 `<StepLoop>`: a loop in the Job Template's `steps` list.
///
/// A loop is a node of the step graph, with a name, `dependencies`, and an
/// optional `enableIf`, and has no Tasks of its own: its work is the Tasks of
/// its body's Steps, run once per iteration. It has no `script`,
/// `parameterSpace`, `hostRequirements`, `stepEnvironments`, or `let`, which
/// `deny_unknown_fields` rejects.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepLoop {
    /// The loop's name, unique among all Steps of the template, by which other
    /// Steps depend on it and read its outputs as `Steps.<name>.*`.
    pub name: String,
    /// For display, as on `<StepTemplate>`.
    pub description: Option<Description>,
    /// Whether the loop runs, with the meaning and scope of a Step's: an
    /// expression field of type `bool` (RFC 0010's Template Schemas §7.6).
    #[serde(default, deserialize_with = "super::expr_field::deserialize_enable_if")]
    pub enable_if: Option<ExprField>,
    /// The top-level Steps and loops the loop depends on.
    pub dependencies: Option<Vec<StepDependency>>,
    /// The loop's variables, condition, bound, and body.
    #[serde(rename = "loop")]
    pub loop_definition: LoopDefinition,
    /// The loop's outputs, read after it as `Steps.<name>.Output.<VarName>`.
    pub output_variables: Option<Vec<LoopOutputVariableDefinition>>,
}

impl StepLoop {
    /// The loop variables, empty when there are none.
    pub fn variables(&self) -> &[LoopVariableDefinition] {
        self.loop_definition.variables.as_deref().unwrap_or(&[])
    }

    /// The loop outputs, empty when there are none.
    pub fn output_variables_list(&self) -> &[LoopOutputVariableDefinition] {
        self.output_variables.as_deref().unwrap_or(&[])
    }

    /// The body Steps, in body order.
    pub fn body(&self) -> &[StepTemplate] {
        &self.loop_definition.steps
    }
}

/// RFC 0013 `<LoopDefinition>`: a loop's variables, condition, bound, and
/// body.
///
/// Exactly one of `while` and `until` is present in a valid template; both
/// are optional here so that validation can report the rule at
/// `steps[i] -> loop`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopDefinition {
    /// The loop variables, read as `Loop.<name>` and `RawLoop.<name>`.
    pub variables: Option<Vec<LoopVariableDefinition>>,
    /// Whether to run another iteration, checked before each iteration,
    /// including the first: an expression field of type `bool` (RFC 0010's
    /// Template Schemas §7.6).
    #[serde(
        rename = "while",
        default,
        deserialize_with = "super::expr_field::deserialize_while"
    )]
    pub while_condition: Option<ExprField>,
    /// Whether to stop, checked after each iteration: an expression field of
    /// type `bool`, as `while` is.
    #[serde(
        rename = "until",
        default,
        deserialize_with = "super::expr_field::deserialize_until"
    )]
    pub until_condition: Option<ExprField>,
    /// The most iterations the loop may run, from 1 to 1,000.
    pub max_iterations: IntOrFormatString,
    /// The body: the Steps that run in each iteration.
    #[serde(deserialize_with = "deserialize_body")]
    pub steps: Vec<StepTemplate>,
}

/// Deserialize a loop body, rejecting an entry with a `loop` key with a
/// message that says loops do not nest, rather than as an unknown field of a
/// `<StepTemplate>` (RFC 0013 §`<LoopDefinition>` item 5.2). Any other error
/// of an entry is prefixed `loop body entry <j> '<name>': `.
fn deserialize_body<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<StepTemplate>, D::Error> {
    use serde::de::Error as _;
    let entries = Vec::<serde_json::Value>::deserialize(deserializer)?;
    entries
        .into_iter()
        .enumerate()
        .map(|(j, entry)| {
            let name = entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|n| format!(" '{n}'"))
                .unwrap_or_default();
            if has_loop_key(&entry) {
                return Err(D::Error::custom(format!(
                    "loop body entry {j}{name} has a 'loop' field, but loops do not nest: a loop's body may contain only Steps (RFC 0013)."
                )));
            }
            serde_json::from_value(entry)
                .map_err(|e| D::Error::custom(format!("loop body entry {j}{name}: {e}")))
        })
        .collect()
}

/// A `<StepTemplate>` of a Job Template, top-level or in a loop's body, with
/// where it is. See [`JobTemplate::step_templates`](super::JobTemplate::step_templates).
#[derive(Debug, Clone, Copy)]
pub struct StepTemplateRef<'a> {
    /// The Step.
    pub step: &'a StepTemplate,
    /// The index of its entry in the template's `steps`: its own for a
    /// top-level Step, its loop's for a body Step.
    pub entry_index: usize,
    /// Its index in its loop's body, `None` for a top-level Step.
    pub body_index: Option<usize>,
    /// Its loop, `None` for a top-level Step.
    pub step_loop: Option<&'a StepLoop>,
}

impl StepTemplateRef<'_> {
    /// The Step's validation-error path: `steps[i]` for a top-level Step,
    /// `steps[i] -> loop -> steps[j]` for a body Step.
    pub fn path(&self) -> Vec<PathElement> {
        let mut path = vec![
            PathElement::Field("steps".into()),
            PathElement::Index(self.entry_index),
        ];
        if let Some(j) = self.body_index {
            path.extend([
                PathElement::Field("loop".into()),
                PathElement::Field("steps".into()),
                PathElement::Index(j),
            ]);
        }
        path
    }
}
