# openjd-sessions Architecture

## Crate Purpose

The `openjd-sessions` crate provides the runtime for executing OpenJD sessions — the
sequence of environment enter/exit and task run actions that constitute a worker's
execution of a job. It is the Rust equivalent of the Python `openjd-sessions-for-python`
library, designed as a drop-in replacement for use by the Deadline Cloud worker agent.

## Dependencies

```
openjd-sessions
├── openjd-model    # Template/job types, format string resolution, let bindings
├── openjd-expr     # ExprValue, SymbolTable, FunctionLibrary, PathMappingRule
├── tokio           # Async runtime (rt-multi-thread, process, io-util, time, sync)
├── tokio-util      # CancellationToken for cooperative cancelation
├── nix             # POSIX signals (killpg), process groups (setsid), user IDs
├── serde/json/yaml # Serialization for path mapping rules, cancel_info.json
├── regex           # openjd_* directive parsing in ActionFilter
├── thiserror       # SessionError enum derivation
├── shlex           # Shell-safe argument quoting for cross-user scripts
├── log             # Logging facade with kv feature for structured metadata
├── bitflags        # LogContent flag type
└── uuid            # Session ID generation in tests
```

## Module Layout

```
src/
├── lib.rs                  # Public API re-exports
├── session.rs              # Session struct, state machine, lifecycle
├── service_session.rs      # ServiceSession: RFC 0009 Service Session runtime (composes Session)
├── action.rs               # ActionState, ActionMessage, ActionResult types
├── action_status.rs        # ActionStatus struct (progress, status, fail, exit_code)
├── action_filter.rs        # Directive parsing from stdout lines, redaction
├── subprocess.rs           # Async subprocess execution via tokio::process
├── runner/
│   ├── mod.rs              # CancelMethod, ScriptRunnerState, resolve_action_args()
│   ├── env_script.rs       # EnvironmentScriptRunner (enter/exit)
│   └── step_script.rs      # StepScriptRunner (run)
├── embedded_files.rs       # Two-phase file materialization
├── let_bindings.rs         # Re-exports evaluate_let_bindings from openjd_model
├── session_user.rs         # SessionUser trait, PosixSessionUser
├── tempdir.rs              # Secure temp directory creation
├── logging.rs              # LogContent bitflags, session_log! macro, banners
└── error.rs                # SessionError enum
```

```
build.rs                    # Compiles embedded cross-user helper binary
helper/                     # Standalone helper binary crate
├── Cargo.toml              # Independent workspace ([workspace] = {})
└── src/
    ├── main.rs             # Shared stdin reader, command dispatch loop
    ├── protocol.rs         # Command/Response JSON serde types
    ├── runner.rs           # poll(2) loop, child management, cancel handling
    └── runner_win.rs       # Windows runner (placeholder)
```

## Public API Surface

Re-exported from `lib.rs`:

```rust
// Core session
pub use session::{Session, SessionState, SessionConfig, EnvironmentIdentifier};

// Service Session (RFC 0009)
pub use service_session::{
    ServiceSession, ServiceSessionConfig, ServiceSessionState, ServiceHealth, ServiceUnhealthy, ServiceRunExit,
};
pub use action::{ActionState, ActionResult, ActionMessage};
pub use action_status::ActionStatus;
pub use error::SessionError;

// Subprocess
pub use subprocess::SubprocessResult;
pub use runner::{CancelMethod, ScriptRunnerState};

// Environment and path mapping
pub use openjd_expr::{PathFormat, PathMappingRule};  // re-export

// Logging
pub use logging::LogContent;

// Cross-user (POSIX)
pub use session_user::{SessionUser, PosixSessionUser};
pub use tempdir::TempDir;

// Cross-user (Windows)
#[cfg(windows)]
pub use session_user::{WindowsSessionUser, BadCredentialsError};
```

### External Cancellation

`SessionConfig.cancel_token` accepts an optional `tokio_util::sync::CancellationToken`.
When provided, all action cancel tokens are created as children of this token via
`parent.child_token()`. Canceling the parent cascades to all current and future actions
in the session. This enables the worker agent to cancel an entire session from outside
the session's async context.

## Service Session Data Flow (RFC 0009)

`ServiceSession` wraps a `Session` and reuses its environment stack, symbol
table construction, path mapping, embedded files, cross-user helper, and
cleanup; the Service-specific parts are the `Service.*` scope, Service
`variables`, `onEnter`'s retained env changes, and a background `onRun`
driver. See [service-session.md](service-session.md).

```
ServiceSessionConfig ──► ServiceSession::with_config()  ──► Session::with_config()
        │
        ▼
   enter()      ── for env in scope, if env.runs_in(Service): Session::enter_environment()
                ── build_symbol_table() + build_service_symbol_table() + materialize_path_mapping()
                ── EmbeddedFiles(Service) allocate → <ServiceScript>.let → write contents
                ── resolve Service `variables`
                ── onEnter (foreground; openjd_env/unset_env/redacted_env retained)
        │
        ▼
   launch()     ── ScriptRunnerBase::run_action(onRun) inside tokio::spawn(drive_run)
                       ├── stdout ──► ActionFilter ──► ActionMessage (status/progress/fail; service_ready)
                       ├── probes: TCP_CONNECT round / COMMAND onHealthCheck invocation / STDOUT openjd_service_ready
                       │     every readinessIntervalSeconds until READY, then every healthIntervalSeconds;
                       │     failureThreshold consecutive failures ──► UNHEALTHY ──► cancel onRun
                       ├── ready timeout (from launch)
                       ├── health ──► watch<ServiceHealth>
                       └── exit ──► watch<Option<ServiceRunExit>>
        │
        ├── wait_ready() / wait_exit() / cancel_run()
        ├── launch() again (relaunch in the same Session)
        ▼
   end()        ── cancel onRun → await exit → onExit (300 s default) → exit envs (reverse) → cleanup()
```

The `onRun` driver runs on a `tokio::spawn`ed task, which requires the
`run_subprocess` future to be `Send`. It is: the formerly non-`Send`
`Option<HANDLE>` the Windows path carried across an await was dead code and
has been removed. One consequence is that `ServiceSession` methods must be
called from within a tokio runtime (as every async method of this crate
already is).

The futures of `Session::enter_environment` / `exit_environment` — and so of
`ServiceSession::enter` / `end`, which call them — are `Send` as well: the
boxed runner future those methods pin (to keep the state machine off the
stack on Windows) carries a `+ Send` bound. A scheduler can therefore drive a
whole `ServiceSession` on a spawned task, which is how `openjd run` starts
Services concurrently and relaunches a `KEEP` Service while a Task runs.

## Data Flow

A typical session lifecycle flows through these modules:

```
SessionConfig ──► Session::with_config()
                      │
                      ├── TempDir::new() ──► working_directory, files_directory
                      ├── build_symbol_table() ──► SymbolTable with Param.*, Session.*
                      └── materialize_path_mapping() ──► JSON file + Session.HasPathMappingRules
                      │
                      ▼
                 enter_environment()
                      │
                      ├── evaluate_env_vars() ──► cumulative env var map
                      ├── EnvironmentScriptRunner::enter()
                      │       ├── EmbeddedFiles::allocate_file_paths()
                      │       ├── evaluate_let_bindings()
                      │       ├── EmbeddedFiles::write_file_contents()
                      │       └── resolve_action_args() ──► SubprocessConfig
                      │
                      └── run_subprocess()
                              ├── tokio::process::Command (setsid, sudo for cross-user)
                              ├── stdout ──► ActionFilter ──► ActionMessage via mpsc
                              └── Session::drive_action() receives messages, invokes callback
                      │
                      ▼
                 run_task()  [same pattern: StepScriptRunner → subprocess]
                      │
                      ▼
                 exit_environment()  [reverse order, EnvironmentScriptRunner::exit()]
                      │
                      ▼
                 cleanup()  ──► TempDir::cleanup() + cross-user sudo rm
```

## Key Design Decisions

### Async-first with tokio

The Python library uses `ThreadPoolExecutor` + daemon threads + `Queue` + `Lock` for
non-blocking execution — a complex arrangement driven by Python's lack of native async
subprocess I/O. Rust with tokio eliminates this complexity:

- `tokio::process::Command` provides async stdout streaming natively
- `tokio::select!` replaces the entire `LoggingSubprocess` + `Timer` + `Lock` + `Queue`
  apparatus
- `CancellationToken` replaces `threading.Event` — no lock coordination needed
- `tokio::time::sleep` replaces `Timer` threads

The public API is async. A blocking wrapper for PyO3 bindings is planned but not yet
implemented.

### Channel-based message streaming

The Python library uses a `logging.Filter` attached to the module logger to intercept
`openjd_*` directives mid-stream. This couples directive processing to Python's logging
infrastructure.

The Rust crate uses `tokio::sync::mpsc::unbounded_channel` to stream `ActionMessage`
values from the subprocess stdout loop to the session. This decouples parsing (in
`ActionFilter`) from processing (in `Session::drive_action`), and avoids the need for
shared mutable state between the subprocess and session.

### Ownership-driven API

The Python library stores the current runner as `self._runner` and mutates session state
from callbacks. The Rust crate avoids interior mutability by having `Session` own the
action lifecycle through `&mut self` methods. The `drive_action` method holds `&mut self`
while concurrently processing messages from the channel, which is safe because the
subprocess runs in a separate future joined via `tokio::select!`.

### Windows support

The Python library supports both POSIX and Windows with extensive platform-specific code
(ACLs, `CreateProcessWithLogonW`, `PopenWindowsAsUser`, etc.). The Rust crate implements
both platforms:

- Same-user subprocess execution: implemented (`subprocess.rs` Windows platform module),
  with pre-spawn executable resolution (`win32_locate.rs` — PATHEXT-aware,
  working-directory-first, no fallback to the worker's own PATH)
- Cross-user subprocess execution: implemented end-to-end — `WindowsSessionUser`
  (password and logon-token modes), the embedded helper binary spawned via
  `CreateProcessWithLogonW`/`CreateProcessAsUserW` (`win32.rs`), CTRL_BREAK notify and
  Job-Object/`CreateToolhelp32Snapshot` process-tree termination inside the helper
  (`helper/src/runner_win.rs`)
- Win32 helpers: `win32.rs` (logon, user lookup, environment blocks),
  `win32_permissions.rs` (ACL management), `win32_locate.rs` (executable resolution,
  called from both spawn paths)
- Temp directory and embedded file permissions: Windows ACL paths implemented
- Integration testing on Windows: dedicated CI job (`cross-user-windows`) runs the
  cross-user and permissions suites on `windows-latest` with a temporary test user,
  and the standard test matrix includes Windows

## Python-vs-Rust Design Comparison

This section consolidates the key design differences between the Python
`openjd-sessions-for-python` library and this Rust crate. Other spec documents
reference this section rather than repeating the comparison.

| Aspect | Python | Rust |
|--------|--------|------|
| Concurrency | `ThreadPoolExecutor` + daemon threads + `Queue` + `Lock` | `tokio::select!` + `mpsc::unbounded_channel` + `CancellationToken` |
| Subprocess I/O | `logging.Filter` on module logger intercepts stdout | `ActionFilter` struct parses lines, sends `ActionMessage` via channel |
| State mutation | `logging.Filter` callback mutates session state (GIL-safe) | `Session::drive_action` processes messages with `&mut self` (no locks) |
| Cancelation | `threading.Event` + lock coordination | `CancellationToken` (child tokens cascade from parent) |
| Cross-user launch | `sudo -u <user> -i` per action (~1s overhead each) | Embedded helper binary, `sudo -i` once per session (~1ms subsequent) |
| Error types | Exceptions (`RuntimeError`, `OSError`) | `SessionError` enum with `thiserror` (`#[non_exhaustive]`) |
| Callback | `Callable[[str, ActionStatus], None]` | `Box<dyn Fn(&str, &ActionStatus) + Send + Sync>` |
| Temp directory cleanup | Explicit `cleanup()`, no `__del__` | Explicit `cleanup()` + `Drop` safety net |
