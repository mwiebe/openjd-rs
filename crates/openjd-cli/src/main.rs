// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! OpenJD CLI — validate, summarize, and run job templates.

mod check;
mod common;
mod help;
pub mod run;
mod summary;

use clap::{Parser, Subcommand};
use log::{LevelFilter, Log, Metadata, Record};
use openjd_sessions::LogContent;
use std::sync::OnceLock;
use std::time::Instant;

/// Global session start time and timestamp format for the logger.
static SESSION_START: OnceLock<Instant> = OnceLock::new();
static TIMESTAMP_FORMAT: OnceLock<String> = OnceLock::new();

fn format_log_timestamp() -> String {
    let fmt = TIMESTAMP_FORMAT
        .get()
        .map(|s| s.as_str())
        .unwrap_or("relative");
    match fmt {
        "local" => chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%.3f")
            .to_string(),
        "utc" => chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
        _ => {
            let start = SESSION_START.get().copied().unwrap_or_else(Instant::now);
            let d = start.elapsed();
            let h = d.as_secs() / 3600;
            let m = (d.as_secs() % 3600) / 60;
            let s = d.as_secs() % 60;
            let ms = d.subsec_millis();
            format!("{h}:{m:02}:{s:02}.{ms:03}")
        }
    }
}

/// Logger that prints subprocess command output, and openjd's warnings and
/// errors about the session, to stdout with timestamps.
struct SessionLogger;

/// Whether `SessionLogger` prints a record with this `openjd_log_content` and
/// level: command output, plus warnings and errors openjd reports about the
/// session itself, such as a working directory it could not delete.
fn prints_session_record(bits: u64, level: log::Level) -> bool {
    let has = |flag: LogContent| bits & u64::from(flag.bits()) != 0;
    has(LogContent::COMMAND_OUTPUT)
        || (has(LogContent::EXCEPTION_INFO) && level <= log::Level::Warn)
}

impl Log for SessionLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        struct Visitor {
            bits: Option<u64>,
        }
        impl<'kvs> log::kv::VisitSource<'kvs> for Visitor {
            fn visit_pair(
                &mut self,
                key: log::kv::Key<'kvs>,
                value: log::kv::Value<'kvs>,
            ) -> Result<(), log::kv::Error> {
                if key.as_str() == "openjd_log_content" {
                    self.bits = value.to_u64();
                }
                Ok(())
            }
        }
        let mut v = Visitor { bits: None };
        let _ = record.key_values().visit(&mut v);
        if let Some(bits) = v.bits {
            if prints_session_record(bits, record.level()) {
                let ts = format_log_timestamp();
                println!("{ts}\t{}", record.args());
            }
        }
    }

    fn flush(&self) {}
}

#[derive(Parser)]
#[command(name = "openjd", about = "Open Job Description CLI (Rust)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check a template for validation errors
    Check(check::CheckArgs),
    /// Print summary information about a Job Template
    Summary(summary::SummaryArgs),
    /// Run a job template locally
    Run(run::RunArgs),
}

#[tokio::main]
async fn main() {
    log::set_boxed_logger(Box::new(SessionLogger)).expect("failed to set logger");
    log::set_max_level(LevelFilter::Info);

    // Intercept `run <path> -h/--help` for context-aware help
    let args: Vec<String> = std::env::args().collect();
    if help::try_context_aware_help(&args) {
        return;
    }

    // Rewrite -tp to --task-param for Python CLI compatibility
    // (clap doesn't support multi-char short flags)
    let rewritten: Vec<String> = args
        .iter()
        .map(|a| {
            if a == "-tp" {
                "--task-param".to_string()
            } else {
                a.clone()
            }
        })
        .collect();
    let cli = Cli::parse_from(rewritten);
    let result = match cli.command {
        Commands::Check(args) => check::execute(args),
        Commands::Summary(args) => summary::execute(args),
        Commands::Run(args) => run::execute(args).await,
    };
    if let Err(e) = result {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod session_logger_tests {
    use super::*;
    use log::Level;

    #[test]
    fn prints_command_output_at_any_level() {
        let output = u64::from(LogContent::COMMAND_OUTPUT.bits());
        for level in [Level::Error, Level::Warn, Level::Info, Level::Debug] {
            assert!(prints_session_record(output, level), "{level}");
        }
    }

    #[test]
    fn prints_exception_info_only_at_warn_or_error() {
        let problem = u64::from((LogContent::EXCEPTION_INFO | LogContent::FILE_PATH).bits());
        assert!(prints_session_record(problem, Level::Error));
        assert!(prints_session_record(problem, Level::Warn));
        assert!(!prints_session_record(problem, Level::Info));
        assert!(!prints_session_record(problem, Level::Debug));
    }

    #[test]
    fn does_not_print_other_content() {
        let other = u64::from((LogContent::FILE_PATH | LogContent::PROCESS_CONTROL).bits());
        assert!(!prints_session_record(other, Level::Error));
        assert!(!prints_session_record(0, Level::Error));
    }
}
