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
    /// Services that became FAILED (RFC 0009), failing their scope.
    pub failed_services: Vec<FailedService>,
}

/// One FAILED Service in the result output.
pub(crate) struct FailedService {
    pub name: String,
    /// The attached Environment Template that declares an external Service
    /// (its path as given on the command line); `None` for a Service of the
    /// Job Template. Service names are scoped to their document, so this is
    /// what tells two same-named Services apart.
    pub document: Option<String>,
    /// The Service's scope as the run log names it: `every Step`, `Step A`,
    /// or `Steps A, B`.
    pub scope: String,
    pub reason: String,
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
        if !self.failed_services.is_empty() {
            value["failed_services"] = self
                .failed_services
                .iter()
                .map(|f| {
                    let mut entry = serde_json::json!({
                        "name": f.name,
                        "scope": f.scope,
                        "reason": f.reason,
                    });
                    if let Some(document) = &f.document {
                        entry["document"] = serde_json::Value::String(document.clone());
                    }
                    entry
                })
                .collect();
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
        for failed in &self.failed_services {
            let origin = failed
                .document
                .as_deref()
                .map(|d| format!(" (from {d})"))
                .unwrap_or_default();
            write!(
                f,
                "\nFailed Service: {}{origin} (scope: {}): {}",
                failed.name, failed.scope, failed.reason
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::CliResult;

    fn result(failed_services: Vec<FailedService>) -> RunResult {
        RunResult {
            status: "error".into(),
            message: "m".into(),
            job_name: "J".into(),
            step_name: None,
            duration: 1.0,
            chunks_run: 0,
            failed_services,
        }
    }

    #[test]
    fn failed_service_names_its_document_only_when_external() {
        let own = FailedService {
            name: "Cache".into(),
            document: None,
            scope: "every Step".into(),
            reason: "boom".into(),
        };
        let external = FailedService {
            name: "Cache".into(),
            document: Some("queue-cache.yaml".into()),
            scope: "Steps A, B".into(),
            reason: "bang".into(),
        };
        let r = result(vec![own, external]);
        let text = r.to_string();
        assert!(
            text.contains("\nFailed Service: Cache (scope: every Step): boom\n"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "\nFailed Service: Cache (from queue-cache.yaml) (scope: Steps A, B): bang"
            ),
            "{text}"
        );
        let json = r.to_json_value();
        assert_eq!(
            json["failed_services"],
            serde_json::json!([
                {"name": "Cache", "scope": "every Step", "reason": "boom"},
                {"name": "Cache", "scope": "Steps A, B", "reason": "bang", "document": "queue-cache.yaml"},
            ])
        );
    }
}
