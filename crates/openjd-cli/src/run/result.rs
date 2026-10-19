// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Run command output result type.

pub(crate) struct RunResult {
    pub status: String,
    pub message: String,
    pub job_name: String,
    pub step_name: Option<String>,
    pub duration: f64,
    pub chunks_run: usize,
    /// What the scheduler simulator recorded, for a Job Template that lists
    /// `CONTROL_FLOW`; `None` otherwise, so other templates' output matches
    /// the Python CLI's.
    pub control_flow: Option<ControlFlowResult>,
}

/// The Step states and job outputs of a `CONTROL_FLOW` run.
pub(crate) struct ControlFlowResult {
    /// One entry per Step: `{name, state, task_count, outputs, ...}`.
    pub steps: Vec<serde_json::Value>,
    /// Each evaluated job output's value (`null` for no value).
    pub job_outputs: serde_json::Map<String, serde_json::Value>,
    /// `Job output <name> = <value>` for each evaluated job output.
    pub job_output_lines: Vec<String>,
    /// Each job output not evaluated, with the Steps it waits on.
    pub not_evaluated: Vec<(String, Vec<String>)>,
}

impl crate::common::CliResult for RunResult {
    fn to_json_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "status": self.status,
            "message": self.message,
            "job_name": self.job_name,
            "step_name": self.step_name,
            "duration": self.duration,
            "chunks_run": self.chunks_run,
        });
        if let (Some(cf), Some(obj)) = (&self.control_flow, value.as_object_mut()) {
            obj.insert("steps".into(), cf.steps.clone().into());
            obj.insert("job_outputs".into(), cf.job_outputs.clone().into());
            obj.insert(
                "job_outputs_not_evaluated".into(),
                cf.not_evaluated
                    .iter()
                    .map(|(name, waits)| serde_json::json!({"name": name, "waits_on": waits}))
                    .collect::<Vec<_>>()
                    .into(),
            );
        }
        value
    }
}

impl std::fmt::Display for RunResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f)?;
        writeln!(f, "--- Results of local session ---")?;
        writeln!(f)?;
        writeln!(f, "{}", self.message)?;
        writeln!(f)?;
        writeln!(f, "Job: {}", self.job_name)?;
        if let Some(sn) = &self.step_name {
            writeln!(f, "Step: {sn}")?;
        }
        writeln!(f, "Duration: {:.3} seconds", self.duration)?;
        write!(f, "Chunks run: {}", self.chunks_run)?;
        if let Some(cf) = &self.control_flow {
            if !cf.job_output_lines.is_empty() || !cf.not_evaluated.is_empty() {
                writeln!(f)?;
                write!(f, "Job outputs:")?;
                for line in &cf.job_output_lines {
                    let line = line.strip_prefix("Job output ").unwrap_or(line);
                    write!(f, "\n  {line}")?;
                }
                for (name, waits) in &cf.not_evaluated {
                    if waits.is_empty() {
                        write!(f, "\n  {name}: not evaluated (its evaluation failed)")?;
                    } else {
                        write!(
                            f,
                            "\n  {name}: not evaluated (waits on {})",
                            waits.join(", ")
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}
