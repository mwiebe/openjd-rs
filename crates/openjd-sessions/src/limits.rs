// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Caller-policy limits enforced by the session runtime.
//!
//! Of the spec's three processing stages (Template Schemas §7.4:
//! template validation, job creation, task execution on the worker
//! host), task execution is the enforcement boundary: a worker can
//! receive a job that never passed through this client's template
//! validation or job creation, so the runtime must enforce these
//! limits on the final resolved values regardless of what earlier
//! stages checked.

use openjd_expr::function_library::FunctionLibrary;
use openjd_expr::FormatStringOptions;

/// Opt-in caps and evaluation budgets enforced by the session runtime,
/// mirroring the corresponding `openjd_model::CallerLimits` fields a
/// submitting service would set. All fields default to `None` — no
/// restriction beyond the OpenJD specification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionLimits {
    /// Maximum character length of the resolved action `command`
    /// (Template Schemas §5.1) and of each final argv entry an `args`
    /// element produces (§5.2, after null-skip and list-flatten). The
    /// spec sets no maximum but notes the operating system imposes its
    /// own; this cap surfaces that failure legibly instead of letting
    /// process spawning fail opaquely.
    pub max_resolved_arg_len: Option<usize>,
    /// Maximum character length of each resolved embedded-file `data`
    /// value (§6.1.2 sets no limit of its own).
    pub max_resolved_data_len: Option<usize>,
    /// Memory budget, in bytes, for each format-string expression
    /// evaluation (the Expression Language spec's "Memory-bounded
    /// evaluation" lever). Also bounds the accumulated
    /// expression-rendered bytes of a multi-segment resolution, so
    /// segments cannot compose per-segment allowances into an unbounded
    /// result. `None` uses the spec-recommended default
    /// ([`openjd_expr::DEFAULT_MEMORY_LIMIT`], 100 MB).
    pub max_eval_memory_bytes: Option<usize>,
    /// Operation budget for evaluating each format-string expression.
    /// `None` uses the spec-recommended default
    /// ([`openjd_expr::DEFAULT_OPERATION_LIMIT`], 10 million).
    pub max_eval_operations: Option<usize>,
    /// Maximum bytes of output values each Task holds (RFC 0010 §Limits
    /// item 2, `CONTROL_FLOW`). `None` uses the RFC's minimum,
    /// [`openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT`] (1 MiB); a smaller
    /// value is raised to it, because a runtime must accept at least that
    /// much. See [`task_output_bytes`](Self::task_output_bytes).
    pub max_task_output_bytes: Option<usize>,
}

impl SessionLimits {
    /// The per-Task output size limit the Session enforces:
    /// [`max_task_output_bytes`](Self::max_task_output_bytes), and never
    /// less than [`openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT`].
    pub fn task_output_bytes(&self) -> usize {
        self.max_task_output_bytes
            .unwrap_or(openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT)
            .max(openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT)
    }
}

/// Mirror the run-time-relevant fields of a submitting service's
/// [`openjd_model::CallerLimits`], so one policy value can drive both
/// validation/job-creation (model) and the session runtime without the
/// two drifting apart. Fields of `CallerLimits` with no run-time
/// counterpart (document-size caps, task counts) are ignored.
impl From<&openjd_model::CallerLimits> for SessionLimits {
    fn from(limits: &openjd_model::CallerLimits) -> Self {
        Self {
            max_resolved_arg_len: limits.max_resolved_arg_len,
            max_resolved_data_len: limits.max_resolved_data_len,
            max_eval_memory_bytes: limits.max_eval_memory_bytes,
            max_eval_operations: limits.max_eval_operations,
            max_task_output_bytes: limits.max_task_output_bytes,
        }
    }
}

/// Format-string evaluation options carrying the session's library and
/// the caller's evaluation budgets. Every format-string resolution in
/// the session runtime must be built through this helper — and let
/// bindings, which evaluate parsed expressions directly rather than
/// resolving a `FormatString`, must pass the same budgets to
/// `openjd_model::evaluate_let_bindings` — so that the budgets bound
/// all expression evaluation uniformly.
pub(crate) fn fs_options<'a>(
    library: Option<&'a FunctionLibrary>,
    limits: &SessionLimits,
) -> FormatStringOptions<'a> {
    let mut opts = FormatStringOptions::new().with_library(library);
    if let Some(m) = limits.max_eval_memory_bytes {
        opts = opts.with_memory_limit(m);
    }
    if let Some(o) = limits.max_eval_operations {
        opts = opts.with_operation_limit(o);
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT;

    #[test]
    fn task_output_bytes_defaults_to_and_never_goes_below_the_minimum() {
        let limits = |m| SessionLimits {
            max_task_output_bytes: m,
            ..Default::default()
        };
        assert_eq!(limits(None).task_output_bytes(), MIN_OUTPUT_BYTES_LIMIT);
        assert_eq!(limits(Some(10)).task_output_bytes(), MIN_OUTPUT_BYTES_LIMIT);
        assert_eq!(
            limits(Some(MIN_OUTPUT_BYTES_LIMIT + 1)).task_output_bytes(),
            MIN_OUTPUT_BYTES_LIMIT + 1
        );
    }
}
