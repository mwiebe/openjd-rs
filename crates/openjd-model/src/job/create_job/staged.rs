// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The errors of one job creation, in the order Template Schemas §7.4.1
//! takes its steps, for every Job Template, with or without `ENV_PARAMS`.
//!
//! Every error is filed under a key: the §7.4.1 step it belongs to, then
//! the Step it is about (by index), then a position within the Step (the
//! Environment's place in the Job's Environment order, or an entry's place
//! in the Step's `dependencies`). [`StagedErrors::finish`] reports them in
//! key order, and in the order they were filed within a key. An error that
//! is not about one Step, such as an unreachable attachment's, is filed
//! after the Steps' errors of its step ([`ANY`]); an error that names
//! several Steps, such as a failed `enableIf`, is filed under the first.
//!
//! The checks job creation makes in the template-validation style before
//! §7.4.1's steps (the carried-forward resolved-value checks and the
//! literal `default`s) are filed under [`Stage::Validation`], first, in the
//! order they were made.
//!
//! A Step with an error from steps 1–4 has no fixed set of Environments,
//! so steps 5–8 are not taken for it ([`StagedErrors::stopped`]), and
//! [`StagedErrors::finish`] leaves out anything filed for it under them
//! before it was known to be stopped; the other Steps go on.

use std::collections::BTreeMap;

use crate::error::{PathElement, ValidationError, ValidationErrors};

/// `e` with `prefix` put before its path.
pub(crate) fn prefixed(prefix: &[PathElement], mut e: ValidationError) -> ValidationError {
    e.path = prefix.iter().cloned().chain(e.path).collect();
    e
}

/// Where an error falls in Template Schemas §7.4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage {
    /// The template-validation-style checks job creation makes before
    /// step 1.
    Validation,
    /// Step 2: each entry's `enableIf`.
    Step2,
    /// Step 3: the entries that name no Job Environment, resolved against
    /// the attached Environments.
    Step3,
    /// Step 4: the Step's set of Environments from `activation` and its
    /// entries (a disabled entry for an `ALWAYS` Environment).
    Step4,
    /// Step 5: the Step's effective values (the entries' keys).
    Step5,
    /// Step 6: each Environment's own `enableIf`, and the groups.
    Step6,
    /// Step 7: constraint 7, the other checks of the values, and the
    /// checks over the Step's set, such as the single-layer rule.
    Step7,
    /// Step 8: the Environments' `hostRequirements` and their combination.
    Step8,
}

/// The Step or position of an error that is about no single one.
pub(crate) const ANY: usize = usize::MAX;

/// A job creation's errors, filed by [`Stage`], Step, and position.
#[derive(Default)]
pub(crate) struct StagedErrors {
    buckets: BTreeMap<(Stage, usize, usize), ValidationErrors>,
}

impl StagedErrors {
    /// The errors filed under `stage`, Step `step`, and position `at`, to
    /// add to.
    pub(crate) fn at(&mut self, stage: Stage, step: usize, at: usize) -> &mut ValidationErrors {
        self.buckets.entry((stage, step, at)).or_default()
    }

    /// Whether Step `step` has an error from §7.4.1 steps 1–4, so that
    /// steps 5–8 are not taken for it.
    pub(crate) fn stopped(&self, step: usize) -> bool {
        self.buckets.iter().any(|((stage, s, _), errors)| {
            *s == step && (Stage::Step2..=Stage::Step4).contains(stage) && !errors.is_empty()
        })
    }

    /// File `other`'s errors after this one's, key by key, with `prefix`
    /// put before each path.
    pub(crate) fn absorb(&mut self, other: StagedErrors, prefix: &[PathElement]) {
        for (key, errors) in other.buckets {
            let target = self.buckets.entry(key).or_default();
            target
                .errors
                .extend(errors.errors.into_iter().map(|e| prefixed(prefix, e)));
        }
    }

    /// Every error, in key order, as one `ModelValidation` error for
    /// `model`, or `Ok` when there are none. The errors filed under steps
    /// 5–8 for a [`stopped`](Self::stopped) Step are left out.
    pub(crate) fn finish(self, model: &str) -> Result<(), crate::error::ModelError> {
        let stopped: std::collections::HashSet<usize> = self
            .buckets
            .iter()
            .filter(|((stage, _, _), errors)| {
                (Stage::Step2..=Stage::Step4).contains(stage) && !errors.is_empty()
            })
            .map(|((_, step, _), _)| *step)
            .collect();
        let mut all = ValidationErrors::default();
        for ((stage, step, _), errors) in self.buckets {
            if stage >= Stage::Step5 && stopped.contains(&step) {
                continue;
            }
            all.errors.extend(errors.errors);
        }
        all.into_result(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(staged: StagedErrors, model: &str) -> String {
        staged.finish(model).unwrap_err().to_string()
    }

    #[test]
    fn errors_are_reported_by_stage_then_step_then_position_then_filing_order() {
        let mut staged = StagedErrors::default();
        staged.at(Stage::Step7, 0, 1).add(&[], "7/0/1 a");
        staged.at(Stage::Step3, 1, 0).add(&[], "3/1/0");
        staged.at(Stage::Step7, 0, 0).add(&[], "7/0/0");
        staged.at(Stage::Step7, ANY, 0).add(&[], "7/any");
        staged.at(Stage::Step7, 0, 1).add(&[], "7/0/1 b");
        staged.at(Stage::Step7, 2, 0).add(&[], "7/2/0");
        staged.at(Stage::Validation, 0, 0).add(&[], "validation");
        assert_eq!(
            messages(staged, "M"),
            "Model validation error: 7 validation errors for M\nM: validation\nM: 3/1/0\n\
             M: 7/0/0\nM: 7/0/1 a\nM: 7/0/1 b\nM: 7/2/0\nM: 7/any"
        );
    }

    #[test]
    fn a_step_is_stopped_by_an_error_of_steps_2_to_4_only() {
        let mut staged = StagedErrors::default();
        staged.at(Stage::Validation, 0, 0).add(&[], "v");
        staged.at(Stage::Step5, 0, 0).add(&[], "5");
        // An empty bucket stops nothing.
        staged.at(Stage::Step2, 0, 0);
        assert!(!staged.stopped(0));
        for (i, stage) in [Stage::Step2, Stage::Step3, Stage::Step4]
            .into_iter()
            .enumerate()
        {
            staged.at(stage, i + 1, 0).add(&[], "x");
            assert!(staged.stopped(i + 1), "{stage:?}");
        }
        assert!(!staged.stopped(0));
        assert!(!staged.stopped(5));
    }

    #[test]
    fn a_stopped_steps_errors_from_steps_5_to_8_are_left_out() {
        let mut staged = StagedErrors::default();
        staged
            .at(Stage::Step6, 0, 0)
            .add(&[], "6/0 filed before the stop");
        staged.at(Stage::Step2, 0, 0).add(&[], "2/0");
        staged.at(Stage::Step8, 0, ANY).add(&[], "8/0");
        staged.at(Stage::Step5, 1, 0).add(&[], "5/1");
        staged.at(Stage::Step7, ANY, 0).add(&[], "7/any");
        assert_eq!(
            messages(staged, "M"),
            "Model validation error: 3 validation errors for M\nM: 2/0\nM: 5/1\nM: 7/any"
        );
    }

    #[test]
    fn absorb_files_after_and_prefixes() {
        let mut staged = StagedErrors::default();
        staged.at(Stage::Step3, 0, 0).add(&[], "mine");
        let mut other = StagedErrors::default();
        other
            .at(Stage::Step3, 0, 0)
            .add(&[PathElement::Field("f".to_string())], "theirs");
        other.at(Stage::Step2, 0, 0).add(&[], "earlier");
        staged.absorb(other, &[PathElement::Field("Doc".to_string())]);
        assert_eq!(
            messages(staged, "M"),
            "Model validation error: 3 validation errors for M\nDoc:\n\tearlier\nM: mine\n\
             Doc -> f:\n\ttheirs"
        );
    }

    #[test]
    fn empty_buckets_are_no_error() {
        let mut staged = StagedErrors::default();
        assert!(StagedErrors::default().finish("M").is_ok());
        staged.at(Stage::Step8, 0, 0);
        staged.at(Stage::Step2, 1, 0);
        assert!(staged.finish("M").is_ok());
    }
}
