// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Host requirements at job creation (Template Schemas §3.3).
//!
//! [`resolve_host_requirements`] resolves one `hostRequirements` object and
//! re-checks, on the resolved values, the §3.3.1 / §3.3.2 constraints that
//! template validation could not apply to format strings. It serves a
//! Step's own requirements and, with `ENV_PARAMS`, an Environment's (§4
//! item 5), which differ only in where their errors are reported
//! ([`Site`]).
//!
//! [`combine_step_host_requirements`] is the last stage of §7.4.1 Job
//! creation with `ENV_PARAMS`: it resolves each Environment's requirements
//! once for each Step whose Sessions enter it, with that Step's effective
//! values, and combines them with the Step's own into the Step's combined
//! host requirements (§3.3.3), which replace `job::Step::host_requirements`.

use std::collections::HashMap;

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::ExprValue;

use crate::error::{
    path_field, path_index, path_to_string, ModelError, PathElement, ValidationErrors,
};
use crate::job;
use crate::template;
use crate::template::validate_v2023_09::env_params::capped_name_list;
use crate::template::validate_v2023_09::helpers::{check_capability_name, CapabilityKind};
use crate::types::{JobParameterType, ValidationContext};

use super::ranges;

/// Where one `hostRequirements` object being resolved is, which decides
/// how its errors are reported.
pub(super) struct Site {
    /// The path of the `hostRequirements` object.
    path: Vec<PathElement>,
    /// What the requirements belong to, for messages that name it:
    /// `Step 'A'` or `Environment 'E'`.
    owner: String,
    /// `None` for a Step's own requirements, whose errors keep their
    /// historical forms (a `JobTemplate` validation error, or the error of
    /// the failing resolution, which for an `anyOf` or `allOf` is prefixed
    /// with the field's path by [`Site::attribute_values_resolution`]). `Some` for an
    /// Environment's, resolved for a Step: every error becomes an error of
    /// the submission at its path, filed under §7.4.1 step 8, with this
    /// sentence (naming the Steps) appended.
    resolved_for: Option<String>,
}

impl Site {
    /// The own `hostRequirements` of Step `step_name`, at `step_index`.
    pub(super) fn step(step_index: usize, step_name: &str) -> Self {
        Self {
            path: vec![
                PathElement::Field("steps".to_string()),
                PathElement::Index(step_index),
                PathElement::Field("hostRequirements".to_string()),
            ],
            owner: format!("Step '{step_name}'"),
            resolved_for: None,
        }
    }

    /// The own `hostRequirements` of the object at `owner_path` (a Step,
    /// `steps[i]`, or a Service under RFC 0009, `services[k]`), named
    /// `owner` in messages (`Step 'A'`, `Service 'C'`), with the error
    /// forms of [`step`](Self::step).
    pub(super) fn owner(owner_path: &[PathElement], owner: &str) -> Self {
        Self {
            path: path_field(owner_path, "hostRequirements"),
            owner: owner.to_string(),
            resolved_for: None,
        }
    }

    /// The `hostRequirements` of Environment `env_name` at `path`
    /// (starting with its document), resolved for the entities named in
    /// `resolved_for`.
    fn environment(path: Vec<PathElement>, env_name: &str, resolved_for: String) -> Self {
        Self {
            path,
            owner: format!("Environment '{env_name}'"),
            resolved_for: Some(resolved_for),
        }
    }

    /// The path of `field[index]` in the object.
    fn item(&self, field: &str, index: usize) -> Vec<PathElement> {
        path_index(&path_field(&self.path, field), index)
    }

    /// The model name a check's errors are reported for.
    fn model(&self) -> &'static str {
        if self.resolved_for.is_some() {
            "Submission"
        } else {
            "JobTemplate"
        }
    }

    /// `errors` as the result of one check.
    fn finish(&self, mut errors: ValidationErrors) -> Result<(), ModelError> {
        if let Some(note) = &self.resolved_for {
            for e in &mut errors.errors {
                e.message = with_note(&e.message, note);
            }
        }
        errors.into_result(self.model())
    }

    /// A rule violated after resolution at `path`.
    fn violation(&self, path: &[PathElement], message: &str) -> ModelError {
        match &self.resolved_for {
            None => ModelError::DecodeValidation(format!("{}: {message}", path_to_string(path))),
            Some(_) => {
                let mut errors = ValidationErrors::default();
                errors.add(path, format!("{message}."));
                self.finish(errors)
                    .expect_err("one error was added, so the result is an error")
            }
        }
    }

    /// A resolution failure `e` of the field at `path`.
    fn resolution(&self, path: &[PathElement], e: ModelError) -> ModelError {
        match &self.resolved_for {
            None => e,
            Some(_) => {
                let mut errors = ValidationErrors::default();
                errors.add(path, e.to_string());
                self.finish(errors)
                    .expect_err("one error was added, so the result is an error")
            }
        }
    }

    /// A resolution failure `e` of an attribute's `anyOf` or `allOf` at
    /// `path`. A Step's own names the field in the message, since a
    /// `FormatStringError` has no path of its own; an Environment's is at
    /// its path, as every resolution failure is.
    fn attribute_values_resolution(&self, path: &[PathElement], e: ModelError) -> ModelError {
        match (&self.resolved_for, e) {
            (
                None,
                ModelError::FormatStringError {
                    message,
                    input,
                    start,
                    end,
                    source,
                },
            ) => ModelError::FormatStringError {
                message: format!("{}: {message}", path_to_string(path)),
                input,
                start,
                end,
                source,
            },
            (_, e) => self.resolution(path, e),
        }
    }
}

/// `message` with the sentence `note` (such as "Resolved for Step 'A'.")
/// appended to its first line, so that an expression error's source and
/// caret lines stay last.
pub(super) fn with_note(message: &str, note: &str) -> String {
    let (first, rest) = match message.split_once('\n') {
        Some((first, rest)) => (first, Some(rest)),
        None => (message, None),
    };
    let sep = if first.ends_with('.') { " " } else { ". " };
    match rest {
        Some(rest) => format!("{first}{sep}{note}\n{rest}"),
        None => format!("{first}{sep}{note}"),
    }
}

/// One requirement that resolved to nothing and was discarded
/// (`ENV_PARAMS`, §3.3): its kind and resolved name.
pub(super) type Discarded = (CapabilityKind, String);

/// One `hostRequirements` object, resolved.
pub(super) struct Resolved {
    /// The requirements left. `None` when the document lists `ENV_PARAMS`
    /// and none are left, which means no requirements (§3.3 item 4).
    pub(super) requirements: Option<job::HostRequirements>,
    /// The requirements discarded, in written order, amounts first.
    pub(super) discarded: Vec<Discarded>,
}

/// Resolve one `hostRequirements` object with `symtab` and check the
/// resolved values (§3.3, §3.3.1, §3.3.2), reporting at `site`.
///
/// `env_params` says the document lists `ENV_PARAMS`. Then an amount whose
/// `min` and `max` both resolve to `null`, and an attribute whose `anyOf`
/// and `allOf` written as one expression both resolve to `null`, are
/// discarded rather than errors, the uniqueness of names applies to the
/// requirements left, and empty lists become `None` (§3.3). Without it
/// the result keeps the base form: an amount with no bound left is an
/// error, and nothing is discarded.
pub(super) fn resolve_host_requirements(
    hr: &template::HostRequirements,
    symtab: &SymbolTable,
    ctx: &ValidationContext,
    site: &Site,
    budgets: super::EvalBudgets,
    env_params: bool,
) -> Result<Resolved, ModelError> {
    let mut discarded = Vec::new();
    let amounts = hr
        .amounts
        .as_ref()
        .map(|amts| {
            let standard = crate::capabilities::standard_amount_capability_names(
                ctx.profile.revision(),
                ctx.profile.extensions(),
            )?;
            let names = amts
                .iter()
                .enumerate()
                .map(|(amount_index, a)| {
                    resolve_capability_name(
                        &a.name,
                        CapabilityKind::Amount,
                        standard,
                        symtab,
                        budgets,
                        site,
                        amount_index,
                    )
                })
                .collect::<Result<Vec<_>, ModelError>>()?;
            let mut kept = Vec::with_capacity(amts.len());
            for (amount_index, (a, name)) in amts.iter().zip(names).enumerate() {
                let amount_path = site.item("amounts", amount_index);
                let bound = |field: &str, fs: &Option<openjd_expr::FormatString>| {
                    fs.as_ref()
                        .map(|fs| {
                            ranges::resolve_to_f64(
                                fs,
                                symtab,
                                &format!("hostRequirements amount {field}"),
                                budgets,
                            )
                            .map_err(|e| site.resolution(&path_field(&amount_path, field), e))
                        })
                        .transpose()
                        .map(Option::flatten)
                };
                let min = bound("min", &a.min)?;
                let max = bound("max", &a.max)?;
                if env_params && min.is_none() && max.is_none() {
                    discarded.push((CapabilityKind::Amount, name));
                    continue;
                }
                check_resolved_amount_bounds(min, max, site, amount_index)?;
                kept.push((amount_index, job::AmountRequirement { name, min, max }));
            }
            check_resolved_names_unique(
                kept.iter().map(|(i, a)| (*i, a.name.as_str())),
                CapabilityKind::Amount,
                site,
            )?;
            Ok::<_, ModelError>(kept.into_iter().map(|(_, a)| a).collect::<Vec<_>>())
        })
        .transpose()?;

    let attributes = hr
        .attributes
        .as_ref()
        .map(|attrs| {
            let standard = crate::capabilities::standard_attribute_capabilities(
                ctx.profile.revision(),
                ctx.profile.extensions(),
            )?;
            let standard_names: Vec<&str> = standard.iter().map(|(name, _)| *name).collect();
            let names = attrs
                .iter()
                .enumerate()
                .map(|(attr_index, a)| {
                    resolve_capability_name(
                        &a.name,
                        CapabilityKind::Attribute,
                        &standard_names,
                        symtab,
                        budgets,
                        site,
                        attr_index,
                    )
                })
                .collect::<Result<Vec<_>, ModelError>>()?;
            let mut kept = Vec::with_capacity(attrs.len());
            for (attr_index, (a, name)) in attrs.iter().zip(names).enumerate() {
                let attr_path = site.item("attributes", attr_index);
                let resolve = |field: &str, vals: &Option<template::AttributeValues>| {
                    let field_path = path_field(&attr_path, field);
                    let Some(vals) = vals else { return Ok(None) };
                    match resolve_attribute_values(vals, symtab, budgets)
                        .map_err(|e| site.attribute_values_resolution(&field_path, e))?
                    {
                        AttributeValuesResolved::Omitted => Ok(None),
                        AttributeValuesResolved::Values(values) => Ok(Some(values)),
                        AttributeValuesResolved::TooMany => Err(site.violation(
                            &field_path,
                            &format!(
                                "attribute '{name}' of {} has more than \
                                 {MAX_ATTRIBUTE_VALUES} {field} values after resolution, \
                                 exceeding the maximum of {MAX_ATTRIBUTE_VALUES} (Template \
                                 Schemas §3.3.2)",
                                site.owner
                            ),
                        )),
                    }
                };
                let any_of = resolve("anyOf", &a.any_of)?;
                let all_of = resolve("allOf", &a.all_of)?;
                // Only the expression form resolves to `null`, which needs
                // ENV_PARAMS; an attribute left with neither is discarded.
                if any_of.is_none() && all_of.is_none() {
                    discarded.push((CapabilityKind::Attribute, name));
                    continue;
                }
                let is_single_valued = is_single_valued(&name);
                for (field, values, written) in
                    [("anyOf", &any_of, &a.any_of), ("allOf", &all_of, &a.all_of)]
                {
                    if let Some(values) = values {
                        let field_path = path_field(&attr_path, field);
                        // Decode checks these counts on the template's
                        // element list, but a whole-field expression
                        // flattens a list inline (Expression Language
                        // §1.3.2), and the expression form is a list of
                        // any length, so the resolved count is unrelated
                        // to the count decode saw. Re-apply the rules on
                        // the resolved list. The list form stops at the
                        // 50-value maximum while resolving (above); this
                        // check is for the expression form.
                        if values.is_empty() {
                            let hint = match written {
                                Some(template::AttributeValues::Expression(_)) => {
                                    "; an expression that resolves to null omits the field \
                                     instead"
                                }
                                _ if env_params => {
                                    "; to have no requirement, write the field as one \
                                     expression that resolves to null"
                                }
                                _ => "",
                            };
                            return Err(site.violation(
                                &field_path,
                                &format!(
                                    "attribute '{name}' of {} has no {field} values after \
                                     resolution. An empty list is not allowed (Template \
                                     Schemas §3.3.2){hint}",
                                    site.owner
                                ),
                            ));
                        }
                        if values.len() > MAX_ATTRIBUTE_VALUES {
                            return Err(site.violation(
                                &field_path,
                                &format!(
                                    "attribute '{name}' of {} has {} {field} values after \
                                     resolution, exceeding the maximum of \
                                     {MAX_ATTRIBUTE_VALUES} (Template Schemas §3.3.2)",
                                    site.owner,
                                    values.len()
                                ),
                            ));
                        }
                        if is_single_valued && field == "allOf" && values.len() > 1 {
                            return Err(site.violation(
                                &field_path,
                                "single-valued attribute cannot have more than 1 element after resolution",
                            ));
                        }
                        let expression =
                            matches!(written, Some(template::AttributeValues::Expression(_)));
                        check_resolved_attribute_values(
                            &name, field, values, expression, standard, site, attr_index,
                        )?;
                    }
                }
                kept.push((
                    attr_index,
                    job::AttributeRequirement {
                        name,
                        any_of,
                        all_of,
                    },
                ));
            }
            check_resolved_names_unique(
                kept.iter().map(|(i, a)| (*i, a.name.as_str())),
                CapabilityKind::Attribute,
                site,
            )?;
            Ok::<_, ModelError>(kept.into_iter().map(|(_, a)| a).collect::<Vec<_>>())
        })
        .transpose()?;

    let requirements = if env_params {
        // §3.3 item 4: what is left of an empty list, or of an object with
        // no requirements, is no requirements.
        let amounts = amounts.filter(|a| !a.is_empty());
        let attributes = attributes.filter(|a| !a.is_empty());
        (amounts.is_some() || attributes.is_some()).then_some(job::HostRequirements {
            amounts,
            attributes,
        })
    } else {
        Some(job::HostRequirements {
            amounts,
            attributes,
        })
    };
    Ok(Resolved {
        requirements,
        discarded,
    })
}

/// Template Schemas §3.3.2: an attribute's `anyOf` or `allOf` holds at
/// most 50 values.
const MAX_ATTRIBUTE_VALUES: usize = 50;

/// What an attribute's `anyOf` or `allOf` resolved to.
enum AttributeValuesResolved {
    /// The expression form resolved to `null`, which omits the field.
    Omitted,
    /// The resolved values.
    Values(Vec<String>),
    /// The list form resolved to more than [`MAX_ATTRIBUTE_VALUES`]
    /// values. Resolution stopped there, so the count is not known.
    TooMany,
}

/// Resolve an attribute's `anyOf` or `allOf` (§3.3.2) to its values:
/// the list form item by item ([`ranges::resolve_string_list`], so a
/// `null` item is skipped and a list flattens, stopping as soon as the
/// list would hold more than [`MAX_ATTRIBUTE_VALUES`] values), or the
/// expression form (`ENV_PARAMS`) with the target type `list[string]?`,
/// whose `null` result omits the field.
fn resolve_attribute_values(
    vals: &template::AttributeValues,
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
) -> Result<AttributeValuesResolved, ModelError> {
    let fs = match vals {
        template::AttributeValues::List(items) => {
            return Ok(
                match ranges::resolve_string_list(items, symtab, budgets, MAX_ATTRIBUTE_VALUES)? {
                    Some(values) => AttributeValuesResolved::Values(values),
                    None => AttributeValuesResolved::TooMany,
                },
            );
        }
        template::AttributeValues::Expression(fs) => fs,
    };
    let target = openjd_expr::ExprType::union(vec![
        openjd_expr::ExprType::list(openjd_expr::ExprType::STRING),
        openjd_expr::ExprType::NULLTYPE,
    ]);
    let value = fs
        .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
        .map_err(|e| ModelError::FormatStringError {
            message: e.to_string(),
            input: Some(fs.raw().to_string()),
            start: None,
            end: None,
            source: Some(e),
        })?;
    match value {
        ExprValue::Null => Ok(AttributeValuesResolved::Omitted),
        // The target type makes any other value an error above.
        v => Ok(AttributeValuesResolved::Values(
            v.list_elements()
                .unwrap_or_default()
                .iter()
                .map(ExprValue::to_display_string)
                .collect(),
        )),
    }
}

/// Resolve a `hostRequirements` capability name and check the §3.3.1.1 /
/// §3.3.2.1 constraints on the resolved value.
///
/// `name` is `@fmtstring`, and template validation can only check a name
/// whose value it knows: a literal, or one that is fully static. Job
/// creation resolves every name, so the checks run here on the resolved
/// value, through the same [`check_capability_name`] template validation
/// uses, so a violation reads the same way whichever stage caught it.
fn resolve_capability_name(
    name: &openjd_expr::FormatString,
    kind: CapabilityKind,
    standard: &[&str],
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
    site: &Site,
    index: usize,
) -> Result<String, ModelError> {
    let path = site.item(kind.field(), index);
    // Required string field: a single whole-field expression resolves with
    // target type `string` (Expression Language §1.3.2).
    let resolved = name
        .resolve_with(
            symtab,
            &budgets
                .fs_options()
                .with_target_type(&openjd_expr::ExprType::STRING),
        )
        .map(|v| match v {
            ExprValue::String(s) => s,
            other => other.to_display_string(),
        })
        .map_err(|e| ModelError::FormatStringError {
            message: format!("hostRequirements {} name: {e}", kind.noun()),
            input: Some(name.raw().to_string()),
            start: None,
            end: None,
            source: Some(e),
        })
        .map_err(|e| site.resolution(&path_field(&path, "name"), e))?;
    let mut errors = ValidationErrors::default();
    check_capability_name(&resolved, kind, standard, &path, &mut errors);
    site.finish(errors)?;
    Ok(resolved)
}

/// Check the §3.3 constraint that no two amounts, and no two attributes,
/// have the same name after the name format strings have been resolved,
/// among `names`, each with its index in the template's list: with
/// `ENV_PARAMS`, those left after discarding (§3.3).
///
/// Decode compares literal names only, since a name containing expressions
/// is unknown there. Two different templates names can resolve to the same
/// capability, so the resolved names are compared here, case-insensitively
/// as §3.3.1.1 / §3.3.2.1 require. The rule applies to one source's
/// requirements, not to a Step's combined set (§3.3.3).
fn check_resolved_names_unique<'n>(
    names: impl IntoIterator<Item = (usize, &'n str)>,
    kind: CapabilityKind,
    site: &Site,
) -> Result<(), ModelError> {
    let mut seen = std::collections::HashSet::new();
    let mut errors = ValidationErrors::default();
    for (index, name) in names {
        if !seen.insert(name.to_lowercase()) {
            errors.add(
                &site.item(kind.field(), index),
                format!("duplicate {} name '{name}'.", kind.noun()),
            );
        }
    }
    site.finish(errors)
}

/// Re-check the `amounts[].min` / `amounts[].max` bounds (§3.3.1) on resolved
/// values.
///
/// Under FEATURE_BUNDLE_1 both fields may be format strings, so the bounds
/// cannot be applied at decode — `validate_v2023_09::structure`'s
/// `parse_literal_amount` returns `None` for a non-literal for exactly that
/// reason, which skips all three checks. Job creation resolves the values, so
/// the deferred checks resume here.
///
/// `min` is `<nonnegativefloat>` and `max` is `<positivefloat>`, so `0` is
/// legal for one and not the other. Paths and wording match the decode-time
/// checks, so a violation reads the same way whichever path caught it.
fn check_resolved_amount_bounds(
    min: Option<f64>,
    max: Option<f64>,
    site: &Site,
    amount_index: usize,
) -> Result<(), ModelError> {
    let amount_path = site.item("amounts", amount_index);
    let mut errors = ValidationErrors::default();
    // Decode enforces "at least one of min or max" on field *presence*,
    // but a whole-field expression resolving to null under the `float?`
    // target means "bound unset" — so a present field can still resolve
    // away. With ENV_PARAMS such an amount is discarded before this check
    // (§3.3). Without it, the rule is re-applied on the resolved values;
    // otherwise the job would carry a boundless amount requirement that
    // matches every worker. The "after resolution" suffix distinguishes this from the
    // decode-time message: the author *did* provide the field.
    if min.is_none() && max.is_none() {
        errors.add(
            &amount_path,
            "must have at least one of min or max after resolution.",
        );
    }
    if let Some(min) = min {
        if min < 0.0 {
            errors.add(&path_field(&amount_path, "min"), "must be non-negative.");
        }
    }
    if let Some(max) = max {
        if max <= 0.0 {
            errors.add(&path_field(&amount_path, "max"), "must be positive.");
        }
    }
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            errors.add(&amount_path, format!("min ({min}) > max ({max})."));
        }
    }
    site.finish(errors)
}

/// Re-check `<AttributeCapabilityValue>` constraints (§3.3.2.2) on resolved
/// `attributes[].anyOf` / `attributes[].allOf` values.
///
/// Both fields are `@fmtstring` in base 2023-09, so a value written as a
/// format string is unknown at decode and its constraints cannot be applied
/// there — `validate_v2023_09::structure` gates the decode-time check on
/// `FormatString::is_literal` for exactly that reason. Job creation resolves
/// the value, so the deferred check resumes here.
///
/// Errors are reported at the path and with the wording the decode-time check
/// uses for the same violation, so a violation reads the same way whichever
/// path caught it. Two differences are deliberate: the path always carries the
/// failing element's index, which decode omits for standard capabilities, and
/// the length message names the resolved value. For a field written as one
/// expression (`expression`, ENV_PARAMS), which has no elements in the
/// template, the error is at the field with a `list element i:` prefix, as
/// template validation reports a static result.
///
/// Only the first failing `anyOf`/`allOf` group on the first failing attribute
/// is reported, because the caller collects through `?`. Decode accumulates
/// every violation instead. Closing that gap means threading one
/// `ValidationErrors` through the whole of `resolve_host_requirements`.
fn check_resolved_attribute_values(
    capability_name: &str,
    field: &str,
    values: &[String],
    expression: bool,
    standard: &[(&str, &[&str])],
    site: &Site,
    attr_index: usize,
) -> Result<(), ModelError> {
    let mut errors = ValidationErrors::default();
    let field_path = path_field(&site.item("attributes", attr_index), field);
    for (value_index, value) in values.iter().enumerate() {
        if let Err(message) = crate::capabilities::validate_attribute_capability_value(
            capability_name,
            value,
            standard,
        ) {
            if expression {
                errors.add(
                    &field_path,
                    format!("list element {value_index}: {message}"),
                );
            } else {
                errors.add(&path_index(&field_path, value_index), message);
            }
        }
    }
    site.finish(errors)
}

/// One Environment of a combined Job, as the combination needs it.
pub(super) struct EnvironmentSource<'a> {
    /// The template Environment.
    pub(super) environment: &'a template::Environment,
    /// Its document, as `job::Step::environments` and
    /// `job::Step::environment_parameters` name it.
    pub(super) document: &'a job::Document,
    /// The path of the Environment, starting with its document
    /// (`JobTemplate -> jobEnvironments[i]`, or the attachment's
    /// `... -> environment`).
    pub(super) path: Vec<PathElement>,
    /// The validation context of its document: the profile the
    /// standard capability tables come from.
    pub(super) ctx: &'a ValidationContext,
}

/// One source of a Step's combined host requirements, for messages.
struct Source {
    /// `<path> -> hostRequirements`, with the Environment's name for an
    /// Environment.
    label: String,
    requirements: job::HostRequirements,
}

/// One entity whose combined host requirements
/// [`combine_step_host_requirements`] computes: a Step, or (RFC 0009
/// `<Service>` item 5) a Service.
struct Target<'j> {
    /// `Step 'X'` or `Service 'X'` (with its document for an external one).
    who: String,
    /// The index its errors are filed under in the staged errors.
    index: usize,
    /// Its full path, starting with its document.
    path: Vec<PathElement>,
    /// Its effective Environment parameter values.
    values: Option<&'j [job::EnvironmentParameterValues]>,
    /// Whether its Sessions enter each Environment, index for index.
    entered: &'j [bool],
}

/// The last stage of §7.4.1 Job creation with `ENV_PARAMS`: give every Step
/// of `job`, and every Service whose scope is not empty (`used`, index for
/// index with `job.services`; RFC 0009 `<Service>` item 5), its combined
/// host requirements (§3.3.3).
///
/// `environments` describes `job.job_environments`, index for index.
/// `entered[s][e]` says whether Step `s`'s Sessions enter Environment `e`,
/// and `service_entered[k][e]` whether Service `k`'s do; `service_paths`
/// is each Service's path. `default_symtab` holds the Job Parameters
/// (merged), and may hold `Job.Name`; `job_name` and `env_names` are
/// `Job.Name` and `Job.EnvNames`.
///
/// Each Environment with `hostRequirements` is resolved once for each
/// distinct set of effective values among the entities that enter it
/// (entities whose values have the same transport form get equal
/// requirements), skipping one whose values for an Environment that
/// declares parameters were rejected (already reported), in the scope §7.3.2
/// gives, and checked as a Step's own are (§3.3.1, §3.3.2, and
/// unique names within the source). The combined set of an entity is its
/// own requirements followed by each entered Environment's, in Job order,
/// normalized by [`combine`]. An entity none of whose entered Environments
/// has requirements keeps its own, unchanged.
///
/// A requirement of an Environment that resolves to nothing for a Step is
/// discarded (§3.3) and added, with the Environment, to `discarded[s]` of
/// each such Step `s`, in Job order. An Environment left with no
/// requirements for a Step takes no part in its combination.
///
/// Every failure is filed in `staged` under §7.4.1 step 8, with paths
/// starting with the document when Environment Templates are attached: a
/// resolution failure at the Environment's field, naming the entities it
/// was resolved for, filed under the first of them and the Environment's
/// place in the Job's order; and a conflict at the entity (`steps[i]`, or
/// `JobTemplate -> steps[i]` with attachments, or the Service's path),
/// filed under the entity after its Environments' failures. Each entity is
/// filed under its index: a Step's own, and for the Service at
/// `job.services[k]`, `service_indices[k]`, which comes after every Step's
/// (D1: a Service's errors follow the Steps' in each step). An entity that
/// enters no Environment, as `entered` says of a Step stopped after §7.4.1
/// step 4, is left alone.
#[allow(clippy::too_many_arguments)]
pub(super) fn combine_step_host_requirements(
    job: &mut job::Job,
    environments: &[EnvironmentSource<'_>],
    entered: &[Vec<bool>],
    service_entered: &[Vec<bool>],
    service_paths: &[Vec<PathElement>],
    service_indices: &[usize],
    used: &[bool],
    job_path: &[PathElement],
    default_symtab: &SymbolTable,
    env_names: &ExprValue,
    budgets: super::EvalBudgets,
    discarded: &mut [Vec<super::DiscardedHostRequirement>],
    staged: &mut super::staged::StagedErrors,
) {
    use super::staged::{Stage, ANY};
    let no_env: &[bool] = &[];
    let mut targets: Vec<Target<'_>> = job
        .steps
        .iter()
        .enumerate()
        .map(|(s, step)| Target {
            who: format!("Step '{}'", step.name),
            index: s,
            path: path_index(&path_field(job_path, "steps"), s),
            values: step.environment_parameters.as_deref(),
            entered: entered.get(s).map_or(no_env, Vec::as_slice),
        })
        .collect();
    let n_steps = targets.len();
    for (k, svc) in job.services.iter().flatten().enumerate() {
        targets.push(Target {
            who: if svc.document.is_job_template() {
                format!("Service '{}'", svc.name)
            } else {
                format!("Service '{}' ({})", svc.name, svc.document)
            },
            index: service_indices.get(k).copied().unwrap_or(ANY),
            path: service_paths.get(k).cloned().unwrap_or_default(),
            values: svc.environment_parameters.as_deref(),
            // A Service nothing uses is never started, so its Sessions
            // enter nothing.
            entered: if used.get(k).copied().unwrap_or(false) {
                service_entered.get(k).map_or(no_env, Vec::as_slice)
            } else {
                no_env
            },
        });
    }
    // resolved[t][e]: Environment e's requirements for target t, when t
    // enters it, it has requirements, and they resolved.
    let mut resolved: Vec<Vec<Option<job::HostRequirements>>> =
        vec![vec![None; environments.len()]; targets.len()];
    for (e, source) in environments.iter().enumerate() {
        let Some(hr) = &source.environment.host_requirements else {
            continue;
        };
        let declares = source
            .environment
            .env_parameter_definitions
            .as_ref()
            .is_some_and(|d| !d.is_empty());
        // The entering entities, grouped by their values for this
        // Environment, in order. Values group by their exact transport
        // form. For the values job creation makes, that is §3.2.2 equality:
        // a FLOAT keeps its text and a RANGE_EXPR is stored in canonical
        // form, so equal values have one transport form.
        let mut groups: Vec<(Option<&job::EnvironmentParameterValues>, Vec<usize>)> = Vec::new();
        let mut group_of: HashMap<Option<String>, usize> = HashMap::new();
        for (t, target) in targets.iter().enumerate() {
            if !target.entered.get(e).copied().unwrap_or(false) {
                continue;
            }
            let values = target.values.into_iter().flatten().find(|v| {
                &v.document == source.document && v.environment == source.environment.name
            });
            // An Environment that declares parameters but has no values for
            // this entity had them rejected (a failed or missing value, or
            // an ambiguous entry), which is already reported; resolving
            // without them would only add an "Undefined variable" error.
            if declares && values.is_none() {
                continue;
            }
            let key = values.map(|v| v.values.as_value().to_string());
            match group_of.get(&key) {
                Some(g) => groups[*g].1.push(t),
                None => {
                    group_of.insert(key, groups.len());
                    groups.push((values, vec![t]));
                }
            }
        }
        let hr_path = path_field(&source.path, "hostRequirements");
        for (values, members) in groups {
            let labels: Vec<&str> = members.iter().map(|t| targets[*t].who.as_str()).collect();
            let site = Site::environment(
                hr_path.clone(),
                &source.environment.name,
                format!("Resolved for {}.", entity_list(&labels)),
            );
            let result = environment_symtab(source, values, default_symtab, &job.name, env_names)
                .and_then(|symtab| {
                    resolve_host_requirements(hr, &symtab, source.ctx, &site, budgets, true)
                });
            // §7.4.1 step 8, filed under the first entity it was resolved
            // for, at the Environment's place in the Job's order.
            let first = targets[members[0]].index;
            match result {
                Ok(result) => {
                    for t in members {
                        resolved[t][e] = result.requirements.clone();
                        if let Some(d) = discarded.get_mut(t) {
                            d.extend(result.discarded.iter().map(|(kind, name)| {
                                super::DiscardedHostRequirement {
                                    environment: Some(job::EnvironmentRef {
                                        document: source.document.clone(),
                                        environment: source.environment.name.clone(),
                                    }),
                                    kind: (*kind).into(),
                                    name: name.clone(),
                                }
                            }));
                        }
                    }
                }
                Err(ModelError::ModelValidation(ve)) => {
                    staged.at(Stage::Step8, first, e).errors.extend(ve.errors)
                }
                Err(other) => staged
                    .at(Stage::Step8, first, e)
                    .add(&hr_path, format!("{other} {}", site_note(&site))),
            }
        }
    }

    let combined: Vec<Option<job::HostRequirements>> = targets
        .iter()
        .enumerate()
        .map(|(t, target)| {
            let mut sources = Vec::new();
            for (e, requirements) in resolved[t].iter_mut().enumerate() {
                if let Some(requirements) = requirements.take() {
                    sources.push(environment_source(&environments[e], requirements));
                }
            }
            if sources.is_empty() {
                return None;
            }
            let own = if t < n_steps {
                job.steps[t].host_requirements.clone()
            } else {
                job.services
                    .as_ref()
                    .and_then(|s| s[t - n_steps].host_requirements.clone())
            };
            if let Some(own) = own {
                sources.insert(0, step_source(&target.path, own));
            }
            Some(combine(
                &target.who,
                &target.path,
                &sources,
                staged.at(Stage::Step8, target.index, ANY),
            ))
        })
        .collect();
    drop(targets);
    for (t, hr) in combined.into_iter().enumerate() {
        let Some(hr) = hr else {
            continue;
        };
        if t < n_steps {
            job.steps[t].host_requirements = Some(hr);
        } else if let Some(services) = job.services.as_mut() {
            services[t - n_steps].host_requirements = Some(hr);
        }
    }
}

/// An Environment's resolved requirements as a source of a combination.
fn environment_source(env: &EnvironmentSource<'_>, requirements: job::HostRequirements) -> Source {
    Source {
        label: format!(
            "{} -> hostRequirements (Environment '{}')",
            path_to_string(&env.path),
            env.environment.name
        ),
        requirements,
    }
}

/// An entity's own resolved requirements as a source of a combination.
fn step_source(step_path: &[PathElement], requirements: job::HostRequirements) -> Source {
    Source {
        label: format!("{} -> hostRequirements", path_to_string(step_path)),
        requirements,
    }
}

/// The requirements of `hr` whose format strings are all literals, so they
/// resolve the same whatever the Job Parameter and Environment parameter
/// values are; `None` when there are none.
fn literal_part(hr: &template::HostRequirements) -> Option<template::HostRequirements> {
    let amounts: Vec<template::AmountRequirement> = hr
        .amounts
        .iter()
        .flatten()
        .filter(|a| {
            a.name.is_literal()
                && a.min.as_ref().is_none_or(|f| f.is_literal())
                && a.max.as_ref().is_none_or(|f| f.is_literal())
        })
        .cloned()
        .collect();
    let attributes: Vec<template::AttributeRequirement> = hr
        .attributes
        .iter()
        .flatten()
        .filter(|a| {
            a.name.is_literal()
                && a.any_of
                    .as_ref()
                    .is_none_or(template::AttributeValues::is_literal)
                && a.all_of
                    .as_ref()
                    .is_none_or(template::AttributeValues::is_literal)
        })
        .cloned()
        .collect();
    (!amounts.is_empty() || !attributes.is_empty()).then(|| template::HostRequirements {
        amounts: (!amounts.is_empty()).then_some(amounts),
        attributes: (!attributes.is_empty()).then_some(attributes),
    })
}

/// One entity whose literal combination [`check_literal_combinations`]
/// checks: a Step, or a Service (RFC 0009 `<Service>` item 5).
pub(super) struct LiteralTarget<'t> {
    /// `Step 'X'` or `Service 'X'`.
    pub(super) who: &'t str,
    /// Its full path, starting with `JobTemplate`.
    pub(super) path: &'t [PathElement],
    /// Its own `hostRequirements`.
    pub(super) own: Option<&'t template::HostRequirements>,
    /// Whether its Sessions certainly enter each Environment, index for
    /// index with the `environments` of [`check_literal_combinations`].
    pub(super) entered: &'t [bool],
}

/// The combination check of [`combine_step_host_requirements`] before the
/// Job Parameter values are known, for `check_environment_templates`:
/// for each Step and Service of `targets`, the literal requirements
/// ([`literal_part`]) of its own `hostRequirements` and of each
/// Environment in `environments` that its `entered` says its Sessions
/// enter are combined (§3.3.3) and each
/// conflict is added to `errors` in the form job creation gives (the same
/// `combine`). Since only the literal requirements are combined, the
/// sources a message names, or the bounds it quotes, may differ from job
/// creation's when a non-literal requirement also takes part, and a
/// conflict that needs a non-literal requirement is reported only at job
/// creation. `entered` leaves out the attachments job creation may compute
/// no values for: one an ambiguous entry names, or an entry whose `enableIf`
/// is an expression when its values are not certain both with the entry's
/// values and with the `default`s; one whose values for the entity were rejected
/// here (constraint 6 or 7, or a failed literal `default`); and one whose
/// values are not all literals (a format-string value or a non-literal
/// `default`), which only job creation resolves. A
/// Step with no literal Environment requirement is skipped, as job
/// creation keeps such a Step's own requirements.
///
/// Combining only some of a Step's requirements finds only conflicts the
/// full combination also has: more sources can only raise the `min`,
/// lower the `max`, or narrow the values common to every list of an item
/// 2.1 attribute. Template validation has checked
/// each literal requirement, so resolving one cannot fail; a source that
/// did is skipped.
/// `step_ctx` is the Job Template's context, for the entities' own
/// requirements.
pub(super) fn check_literal_combinations(
    targets: &[LiteralTarget<'_>],
    environments: &[EnvironmentSource<'_>],
    step_ctx: &ValidationContext,
    budgets: super::EvalBudgets,
    errors: &mut ValidationErrors,
) {
    let no_symbols = SymbolTable::new();
    // Each literal Environment's requirements, resolved once.
    let env_requirements: Vec<Option<job::HostRequirements>> = environments
        .iter()
        .map(|source| {
            let hr = literal_part(source.environment.host_requirements.as_ref()?)?;
            let site = Site::environment(
                path_field(&source.path, "hostRequirements"),
                &source.environment.name,
                String::new(),
            );
            resolve_host_requirements(&hr, &no_symbols, source.ctx, &site, budgets, true)
                .ok()
                .and_then(|r| r.requirements)
        })
        .collect();
    for target in targets {
        let mut sources: Vec<Source> = environments
            .iter()
            .zip(&env_requirements)
            .enumerate()
            .filter(|(e, _)| target.entered.get(*e).copied() == Some(true))
            .filter_map(|(_, (env, hr))| hr.clone().map(|hr| environment_source(env, hr)))
            .collect();
        if sources.is_empty() {
            continue;
        }
        if let Some(own) = target.own.and_then(literal_part) {
            // The site's path is relative to the Job Template.
            let site = Site::owner(target.path.get(1..).unwrap_or_default(), target.who);
            let lists_env_params = step_ctx
                .profile
                .has_extension(crate::types::ModelExtension::EnvParams);
            if let Some(own) = resolve_host_requirements(
                &own,
                &no_symbols,
                step_ctx,
                &site,
                budgets,
                lists_env_params,
            )
            .ok()
            .and_then(|r| r.requirements)
            {
                sources.insert(0, step_source(target.path, own));
            }
        }
        combine(target.who, target.path, &sources, errors);
    }
}

/// The sentence a [`Site`] appends to its messages.
fn site_note(site: &Site) -> &str {
    site.resolved_for.as_deref().unwrap_or("")
}

/// The scope of Environment `source`'s `hostRequirements` for a Step with
/// effective values `values` (§7.3.2): the Job Parameters, `Job.Name`
/// and `Job.EnvNames`, and the Environment's `Env.RawParam.*` of any type (a path as its
/// unmapped string) and `Env.Param.*` of types other than PATH and LIST[PATH]. Template
/// validation has rejected every other
/// reference, so binding more of the Job Parameters than the document's
/// non-PATH ones changes nothing.
pub(super) fn environment_symtab(
    source: &EnvironmentSource<'_>,
    values: Option<&job::EnvironmentParameterValues>,
    default_symtab: &SymbolTable,
    job_name: &str,
    env_names: &ExprValue,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = default_symtab.clone();
    symtab.set("Job.Name", ExprValue::String(job_name.to_string()))?;
    symtab.set("Job.EnvNames", env_names.clone())?;
    let Some(values) = values else {
        return Ok(symtab);
    };
    let table = values
        .values
        .to_symtab(PathFormat::Posix)
        .map_err(ModelError::DecodeValidation)?;
    for def in source
        .environment
        .env_parameter_definitions
        .iter()
        .flatten()
    {
        if let Some(value) = table.get_value(def.name()) {
            if matches!(
                def.job_param_type(),
                JobParameterType::Path | JobParameterType::ListPath
            ) {
                // The raw value is the unmapped string; `Env.Param.*` of a
                // path type is path-mapped, which needs a host.
                symtab.set(
                    &format!("Env.RawParam.{}", def.name()),
                    super::env_params::paths_to_strings(value.clone()),
                )?;
                continue;
            }
            let value = value.clone();
            symtab.set(&format!("Env.Param.{}", def.name()), value.clone())?;
            symtab.set(&format!("Env.RawParam.{}", def.name()), value)?;
        }
    }
    Ok(symtab)
}

/// Entity labels (`Step 'A'`, `Service 'C'`) for a message, grouped by
/// kind in order of first appearance: `Step 'A'`, `Steps 'A' and 'B'`, or
/// `Steps 'A' and 'B' and Service 'C'`, each group's names listed by
/// [`name_list`]. A label without a kind word is listed as it is.
pub(super) fn entity_list(labels: &[&str]) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for label in labels {
        let (noun, name) = label.split_once(' ').unwrap_or(("", label));
        match groups.iter_mut().find(|(n, _)| *n == noun) {
            Some((_, names)) => names.push(name),
            None => groups.push((noun, vec![name])),
        }
    }
    groups
        .iter()
        .map(|(noun, names)| {
            let plural = if names.len() == 1 { "" } else { "s" };
            if noun.is_empty() {
                name_list(names)
            } else {
                format!("{noun}{plural} {}", name_list(names))
            }
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// `labels` for a message: `a and b` for two, else a capped
/// comma-separated list.
pub(super) fn name_list(labels: &[&str]) -> String {
    match labels {
        [a, b] => format!("{a} and {b}"),
        _ => capped_name_list(labels.iter().copied(), labels.len()),
    }
}

/// One capability's amount requirement while combining: the bounds so far
/// and the source of each.
struct AmountAcc {
    name: String,
    min: Option<(f64, usize)>,
    max: Option<(f64, usize)>,
}

/// Every source's requirement on one attribute while combining: the name
/// as the first source spells it, and each requirement with its source.
struct AttributeAcc<'s> {
    name: String,
    requirements: Vec<(usize, &'s job::AttributeRequirement)>,
}

/// Whether a host has exactly one value of attribute `name` (§3.3.3 item
/// 2.1).
fn is_single_valued(name: &str) -> bool {
    crate::template::validate_v2023_09::helpers::is_single_valued_attribute(name)
}

/// The one value of `values`, compared ignoring case (§3.3.2), so `[q, Q]`
/// has one value; `None` for an empty list or two or more values.
fn one_value(values: &[String]) -> Option<&String> {
    let (first, rest) = values.split_first()?;
    let lower = first.to_lowercase();
    rest.iter()
        .all(|v| v.to_lowercase() == lower)
        .then_some(first)
}

/// §3.3.3 item 2.1: the values in every `anyOf` and every `allOf` list of
/// `acc`, compared ignoring case, in the order and spelling of their first
/// appearance (a source's `anyOf` before its `allOf`); `None` when there
/// are none.
fn common_values(acc: &AttributeAcc<'_>) -> Option<Vec<String>> {
    use std::collections::HashSet;
    let lists: Vec<&[String]> = acc
        .requirements
        .iter()
        .flat_map(|(_, a)| [a.any_of.as_deref(), a.all_of.as_deref()])
        .flatten()
        .collect();
    let sets: Vec<HashSet<String>> = lists
        .iter()
        .map(|l| l.iter().map(|v| v.to_lowercase()).collect())
        .collect();
    let mut seen = HashSet::new();
    let common: Vec<String> = lists
        .iter()
        .flat_map(|l| l.iter())
        .filter(|v| {
            let lower = v.to_lowercase();
            seen.insert(lower.clone()) && sets.iter().all(|s| s.contains(&lower))
        })
        .cloned()
        .collect();
    (!common.is_empty()).then_some(common)
}

/// §3.3.3 item 2.2: the requirements of `acc` on an attribute of which a
/// host may have any set of values. *B*, the values a host must have, is
/// the union of every `allOf` list and the value of every one-value
/// `anyOf`, in the order and spelling of first appearance (a source's
/// `anyOf` before its `allOf`); the result is the `allOf` of *B*, when *B*
/// is not empty, followed by each `anyOf` of two or more values that has
/// no value in *B*, as written and in source order. An `anyOf` that has a
/// value in *B* is implied by the `allOf`, so it is dropped.
fn multi_valued_conjunction(acc: &AttributeAcc<'_>) -> Vec<job::AttributeRequirement> {
    let mut must = Vec::new();
    let mut must_lower = std::collections::HashSet::new();
    for (_, a) in &acc.requirements {
        let one = a.any_of.as_deref().and_then(one_value);
        for v in one.into_iter().chain(a.all_of.iter().flatten()) {
            if must_lower.insert(v.to_lowercase()) {
                must.push(v.clone());
            }
        }
    }
    let mut combined = Vec::new();
    if !must.is_empty() {
        combined.push(job::AttributeRequirement {
            name: acc.name.clone(),
            any_of: None,
            all_of: Some(must),
        });
    }
    for (_, a) in &acc.requirements {
        let Some(values) = a.any_of.as_ref() else {
            continue;
        };
        // A one-value `anyOf`'s value is in B, so this drops it too.
        if !values
            .iter()
            .any(|v| must_lower.contains(&v.to_lowercase()))
        {
            combined.push(job::AttributeRequirement {
                name: acc.name.clone(),
                any_of: Some(values.clone()),
                all_of: None,
            });
        }
    }
    combined
}

/// The combined host requirements of `who` (§3.3.3; `Step 'X'` or `Service
/// 'X'`) from
/// `sources`, in order: the conjunction of every source's requirements,
/// normalized per capability (names compared ignoring case) over all the
/// sources at once, so what a host must satisfy does not depend on their
/// order (only the stored spelling and value order follow first appearance).
///
/// - The amounts on one capability become one with the greatest given
///   `min` and the least given `max`, left out when no source gives one, so
///   an omitted bound means what §3.3.1 says for a Step's own requirement;
///   amounts do not add (item 1). A conflict is found only between a given
///   `min` and a given `max`: a capability's own minimum (1 for
///   `amount.worker.vcpu`) is not checked against a `max`, as it is not for
///   a Step's own requirements.
/// - For `attr.worker.os.family` and `attr.worker.cpu.arch`, the
///   requirements become one `anyOf` of the values in every `anyOf` and
///   every `allOf` list, values compared ignoring case ([`common_values`],
///   item 2.1).
/// - For any other attribute, they become the `allOf` of every `allOf`
///   value and every one-value `anyOf`'s value, then each `anyOf` of two or
///   more values that has none of those values
///   ([`multi_valued_conjunction`], item 2.2).
///
/// A capability that only one source names keeps that source's
/// requirement as written. A combined requirement keeps the name spelling
/// of the first source that names the capability, and requirements keep
/// the order in which their capability first appears.
///
/// An amount whose combined `min` exceeds its `max`, and an item 2.1
/// attribute with no common value, are conflicts, added to `errors` at
/// `step_path` naming the Step, the capability, and the sources (for an
/// amount, the first with the greatest `min` and the first with the least
/// `max`; for an attribute, every source that names it, the list capped by
/// [`name_list`] as the other job-creation lists are). Combining adds no other
/// error.
fn combine(
    who: &str,
    step_path: &[PathElement],
    sources: &[Source],
    errors: &mut ValidationErrors,
) -> job::HostRequirements {
    let mut amounts: Vec<AmountAcc> = Vec::new();
    let mut amount_index: HashMap<String, usize> = HashMap::new();
    for (k, source) in sources.iter().enumerate() {
        for a in source.requirements.amounts.iter().flatten() {
            let slot = *amount_index
                .entry(a.name.to_lowercase())
                .or_insert_with(|| {
                    amounts.push(AmountAcc {
                        name: a.name.clone(),
                        min: None,
                        max: None,
                    });
                    amounts.len() - 1
                });
            let acc = &mut amounts[slot];
            if let Some(min) = a.min {
                if acc.min.is_none_or(|(m, _)| min > m) {
                    acc.min = Some((min, k));
                }
            }
            if let Some(max) = a.max {
                if acc.max.is_none_or(|(m, _)| max < m) {
                    acc.max = Some((max, k));
                }
            }
        }
    }
    for acc in &amounts {
        if let (Some((min, min_k)), Some((max, max_k))) = (acc.min, acc.max) {
            if min > max {
                // Each source has `min` <= `max` (checked per source), so
                // the two bounds come from different sources.
                let detail = format!(
                    "{} requires min {min}, and {} requires max {max}",
                    sources[min_k].label, sources[max_k].label
                );
                errors.add(
                    step_path,
                    format!(
                        "{who}: no host can satisfy the combined host requirements for amount \
                         '{}': {detail}. Template Schemas §3.3.3.",
                        acc.name
                    ),
                );
            }
        }
    }

    // Attributes: every source's requirement on a capability, collected
    // first and then combined at once (§3.3.3 item 2), so the result does
    // not depend on the order of the sources.
    let mut attrs: Vec<AttributeAcc<'_>> = Vec::new();
    let mut attr_index: HashMap<String, usize> = HashMap::new();
    for (k, source) in sources.iter().enumerate() {
        for a in source.requirements.attributes.iter().flatten() {
            let slot = *attr_index.entry(a.name.to_lowercase()).or_insert_with(|| {
                attrs.push(AttributeAcc {
                    name: a.name.clone(),
                    requirements: Vec::new(),
                });
                attrs.len() - 1
            });
            attrs[slot].requirements.push((k, a));
        }
    }
    let mut attributes = Vec::new();
    for acc in &attrs {
        // Named by one source only: kept as written.
        if let [(_, a)] = acc.requirements.as_slice() {
            attributes.push((*a).clone());
            continue;
        }
        if is_single_valued(&acc.name) {
            match common_values(acc) {
                Some(values) => attributes.push(job::AttributeRequirement {
                    name: acc.name.clone(),
                    any_of: Some(values),
                    all_of: None,
                }),
                None => {
                    let labels: Vec<&str> = acc
                        .requirements
                        .iter()
                        .map(|(k, _)| sources[*k].label.as_str())
                        .collect();
                    errors.add(
                        step_path,
                        format!(
                            "{who}: no host can satisfy the combined host requirements for \
                             attribute '{}', of which a host has exactly one \
                             value: the anyOf and allOf values of {} have no value in common. \
                             Template Schemas §3.3.3.",
                            acc.name,
                            name_list(&labels)
                        ),
                    );
                }
            }
        } else {
            attributes.extend(multi_valued_conjunction(acc));
        }
    }

    let amounts: Vec<job::AmountRequirement> = amounts
        .into_iter()
        .map(|acc| job::AmountRequirement {
            name: acc.name,
            min: acc.min.map(|(v, _)| v),
            max: acc.max.map(|(v, _)| v),
        })
        .collect();
    job::HostRequirements {
        amounts: (!amounts.is_empty()).then_some(amounts),
        attributes: (!attributes.is_empty()).then_some(attributes),
    }
}
