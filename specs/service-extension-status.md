# SERVICE extension (RFC 0009) — implementation status

This is the record of the final alignment pass between RFC 0009 (the `rfc-service`
branch of openjd-specifications: `rfcs/0009-service.md`, the wiki *Template Schemas*
§1.1, §1.2, §1.2.2, §3, §3.3.2.1, §4, §4.3, §4.3.1, §7.3.1, §7.4, §9; *How Jobs Are
Run* § Services; *Expression Language* Service Symbols and §2.2.4) and what openjd-rs
implements on the `service-extension` branch. It lists every normative statement
(MUST / MUST NOT / SHOULD / "must" in the wiki) with its status and the file that
implements it, the gaps, and every place the implementation made a choice the spec
leaves open.

Status key: **Implemented**; **Partially** (part of the statement, with the missing
part named); **Not implemented**; **N/A locally** (the statement is about a
distributed scheduler, a worker fleet, or a wrapper/template author — nothing a
single-host runner can do or observe).

File key: `model/…` = `crates/openjd-model/src/…`; `sessions/…` =
`crates/openjd-sessions/src/…`; `cli/…` = `crates/openjd-cli/src/run/…`;
`expr/…` = `crates/openjd-expr/src/…`.

## Schema and validation (RFC "Schema modifications", wiki §1–§9.7)

| # | Normative statement | Status | Where |
|---|---|---|---|
| S1 | `jobServices`, `stepServices`, `services`, `runScope` available only with `SERVICE` (§1.1 item 8, §1.2 item 6, §3 item 6, §4 item 3) | Implemented | `model/template/validate_v2023_09/service.rs` (`validate_services_*`, `validate_run_scope`) |
| S2 | A template that lists `SERVICE` must also list `EXPR` (§9, §9.7 item 7) — job and environment templates | Implemented | `service.rs` `check_expr_prerequisite` |
| S3 | Service lists: at least one element, at most 10, unique names (§1.1 item 8.1–8.3, §1.2 item 6.1–6.3, §3 item 6.1–6.3, §9.7 item 5) | Implemented | `service.rs` `validate_service_list` |
| S4 | A Step Service must not share a `name` with a Job Service; different Steps may reuse a name (§1.1 item 8.4, §3 item 6.4–6.5) | Implemented | `service.rs` (`outer_names`) |
| S5 | Environment Template: `$schema` ignored, `extensions` per document, `environment` optional, at least one of `environment` / `services` (§1.2) | Implemented | `model/template/environment_template.rs`, `validate_v2023_09/structure.rs` |
| S6 | An extension listed in an Environment Template applies to that document only (§1.2 item 3) | Implemented | `model/job/create_job/external.rs` (per-attachment profile) at submission; at run time `sessions/session.rs` `Session::enter_environment_with_profile` / `ServiceSessionConfig::environment_profiles` evaluate an attached Environment's strings under its own profile and an external Service's Session runs under its document's profile (`cli/run/mod.rs` `PreparedRun::attached_profiles`, `cli/run/services.rs` `ServiceRunConfig::profile_for`) — exploratory report bug B1 (`join_host_port` in an attached Environment under a Job Template without `SERVICE`); tests `sessions/tests/integration/test_environment_profile.rs`, CLI `test_attached_environment_uses_its_own_extensions_not_the_jobs`, `test_plain_environment_template_attached_to_a_service_job`. The same per-document gate decides whether `openjd_redacted_env` is honored (`REDACTED_ENV_VARS`), and — exploratory report stumble S6 (`08-secret.yaml`: the Service `onEnter`'s redacted token showed as `********` in the log yet `onRun` found no variable; `08c` for a plain Environment) — when it is not, `sessions/action_filter.rs` logs a WARN `COMMAND_OUTPUT` line, `Received openjd_redacted_env for '<NAME>' but the REDACTED_ENV_VARS extension is not declared; the variable is not set.` (name only, never the value; tagged `[Service <name>]` in a Service Session; once per directive), as the Python reference does; tests `action_filter.rs` `test_redacted_env_with_warning_no_extension`, `sessions/tests/integration/test_environment_profile.rs` `redacted_env_without_the_extension_warns_in_the_environments_output`, CLI `test_service_on_enter_redacted_env_without_the_extension_warns` |
| S7 | `<Service>` structure: identifier names not `File`; 1–10 uniquely named ports; `onRun` required; closed property set (§9, §9.1, §9.2 item 1, §9.6) | Implemented | `service.rs` `validate_service`, `validate_service_identifier`; serde `deny_unknown_fields` in `model/template/service.rs` |
| S8 | `port` in 1–65535; `timeoutSeconds` / `intervalSeconds` > 0; `maxAttempts` ≥ 0 — literal forms at validation, format-string forms at job creation (§9.2–§9.4) | Implemented | `service.rs` `check_literal_int`; `format_strings.rs` (`Int` constraint, nullable) |
| S9 | Numeric `@fmtstring` fields resolve at job creation in the `<Service>.let` scope, never `Session.*` / `Service.*`; whole-field target `int?`, `null` = not provided, non-null must satisfy the range (§9.2) | Implemented | `format_strings.rs` `validate_service_format_strings` (job-creation scope); `model/job/create_job/instantiate.rs` |
| S10 | `readinessCheck` discriminated by `type` ∈ {TCP_CONNECT, COMMAND, STDOUT}; `ports` only on TCP_CONNECT, `intervalSeconds` only on COMMAND (§9.3) | Implemented | `model/template/service.rs` (`ServiceReadinessCheck`, tagged enum) |
| S11 | `onReadinessCheck` defined iff type is COMMAND; every TCP_CONNECT `ports` entry declared; `ports` if provided non-empty (§9.3 items 1–2, §9.6 item 3, §9.7 item 4) | Implemented | `service.rs` |
| S11a | `<ServicePort>.protocol` ∈ {TCP, UDP}, default TCP, a literal (not `@fmtstring`); TCP and UDP numbers are separate spaces (§9.2 item 3) | Implemented | `model/template/service.rs` `ServicePortProtocol` (serde enum; `udp` / `SCTP` / a format string are `unknown variant`); `job::ServicePort.protocol` (omitted from JSON when TCP); conformance `9.2--port-protocol-*` |
| S11b | Every port a `TCP_CONNECT` check names has `protocol: TCP` (§9.3 item 2, §9.7 item 4) | Implemented | `service.rs` — `readinessCheck -> ports[k]`: `port '<name>' has protocol UDP and cannot be probed by a TCP_CONNECT readiness check; only TCP ports may be named.`; `sessions/service_session.rs` `with_config` refuses the same at run time |
| S11c | A Service none of whose ports is TCP has a `readinessCheck` of type `STDOUT` or `COMMAND`; omitting it, or giving `TCP_CONNECT`, is a validation error (§9 item 6, §9.7 item 4) | Implemented | `service.rs` — `readinessCheck`: `the default TCP_CONNECT readiness check has no TCP port to probe: none of the Service's ports has protocol TCP, so a readinessCheck of type STDOUT or COMMAND is required.` / `a TCP_CONNECT readiness check has no TCP port to probe: …` |
| S11d | No two ports with the same `protocol` have the same `port` number; the same number across protocols is allowed; a format-string `port` is checked at job creation when resolved (§9 item 5.4, §9.7 item 8) | Implemented | `service.rs` (literal numbers; `ports[i] -> port`: `<TCP\|UDP> port <n> is also used by port '<earlier>'; two ports with the same protocol must not have the same port number.`); `model/job/create_job/instantiate.rs` `check_duplicate_port_numbers` (resolved numbers, same message) |
| S12 | `completedTasks` ∈ {KEEP, RERUN}, default RERUN; `maxAttempts` default 0 (§9.4) | Implemented | `model/template/service.rs` (`ServiceRestartPolicy`, defaults) |
| S13 | Default readiness check `{ type: TCP_CONNECT }` on every **TCP** port (a UDP port of a mixed Service is not probed); default `timeoutSeconds` 300; default `intervalSeconds` 5 (§9 item 6, §9.3) | Implemented | `model/template/service.rs` (`readiness_check()`, `tcp_port_names()`, `DEFAULT_TIMEOUT_SECONDS`, `DEFAULT_INTERVAL_SECONDS`); `instantiate.rs` expands `ports: None` to `tcp_port_names()`; CLI test `test_job_service_mixed_tcp_udp_default_readiness_probes_tcp_only` |
| S14 | `embeddedFiles` if defined non-empty; `<ServiceScript>.let` with EXPR (§9.5) | Implemented | `service.rs`; `format_strings.rs` |
| S15 | `runScope`: ≥ 1 element, recognized names only (`TASK`, `SERVICE`), no duplicates; implementations must reject an unrecognized name (§4 item 3, §9.7 item 3) | Implemented | `service.rs` `validate_run_scope`; `model/template/environment.rs` `RunScope` |
| S16 | Default `runScope` = every kind of Session; explicit list exhaustive (§4 item 3) | Implemented | `Environment::runs_in` |
| S17 | An Environment whose `runScope` includes `SERVICE` must not reference any `Service.*` value (§4 item 3.2, §9.7 item 2) | Implemented | `format_strings.rs` `build_session_scope_symtab` (`!env.runs_in(Service)` gate) |
| S18 | `Service.*` scope rules: declaring Service (all three values); later Services in the same list, and for a Job Service every Step Service; `jobEnvironments` / `stepEnvironments` with `runScope` excluding `SERVICE`; `stepServices` and `script` of the Step (§7.3.1, §9 scope list 1–4, §9.7 item 1) | Implemented | `format_strings.rs` (see `specs/model/validation.md` "Service scopes" table) |
| S19 | `bindAddress` only within the declaring Service (§7.3.1) | Implemented | `model/job/service_symbols.rs` (`build_service_symbol_table`, own vs in-scope) |
| S20 | `Service.*` never in any `hostRequirements`, a `<Service>.let`, a `<StepTemplate>.let`, or any job-creation-stage field (§9, §9.7 item 2, §7.4) | Implemented | `format_strings.rs` (`validate_host_requirements_fs`, job-creation scope) |
| S21 | A reference to an undeclared Service or port, or outside the scopes, is rejected before the Job is created (§9, §9.7 item 1) | Implemented | `format_strings.rs` (undefined-variable error); `validate_v2023_09/service_scope.rs` then rewrites the message into the scope rule broken when the Service is declared in the document (Step Service out of scope, later in its list, `bindAddress` outside the Service, Environment entered in Service Sessions, job-creation field, undeclared port, `Task.*` in a Service — see `specs/model/validation.md` "Scope-rule diagnostics"); an undeclared name keeps the generic message with its suggestion |
| S22 | `Task.*` never available within a Service (§9) | Implemented | `format_strings.rs` `validate_service_format_strings` |
| S23 | `Service.File.<name>` for the declaring Service's embedded files, in its actions, embedded files, and `<ServiceScript>.let` (§7.3.1, §9.5) | Implemented | `format_strings.rs`; `sessions/service_session.rs` `enter()` |
| S24 | `<Service>.let`: `Param.*`, `RawParam.*`, `Job.Name`, `Step.Name` + Step `let` for a Step Service; not `Session.*` / `Service.*`; names available in `hostRequirements`, `variables`, `script` (§9 item 3, §3) | Implemented | `format_strings.rs`; `model/job/create_job/instantiate.rs` |
| S25 | `<ServiceScript>.let` on the service host: `Session.*`, `Service.File.*`, in-scope `Service.*` (§9.5 item 1) | Implemented | `sessions/service_session.rs` `enter()` step 2 |
| S26 | PATH `Param.*` in `variables` and `script` with the service host's path mapping; not in `<Service>.let` / `hostRequirements` (§9 scope item 1) | Implemented | `format_strings.rs` (two scopes); `sessions/session.rs` `build_symbol_table` |
| S27 | `Service.*` type-checks as `unresolved[int]` / `unresolved[string]` / `unresolved[path]` before execution (§7.4) | Implemented | `model/job/service_symbols.rs` seeders |
| S28 | `Service.*` and `Service.File.*` are never in scope for a wrapping Environment entered in Service Sessions, even in its `onWrapService*` hooks (§4 item 3.2) | Implemented | `format_strings.rs` (wrapping env sees no `Service.*`); test `wrapping_environment_does_not_see_services_in_service_sessions` |
| S29 | `onWrapService*` hooks require both `WRAP_ACTIONS` and `SERVICE` (§4.3 item 5) | Implemented | `validate_v2023_09/wrap_actions.rs` |
| S30 | Hooks follow `runScope`: `onWrapEnvEnter`/`onWrapEnvExit` always; `onWrapTaskRun` iff `TASK`; all four `onWrapService*` iff `SERVICE`; a hook not called for, or one omitted, is rejected (§4.3 rule 6, §9.7 item 6) | Implemented | `wrap_actions.rs` `check_hooks_follow_run_scope` |
| S31 | `WrappedService.*` (Name, PortNames, Ports, BindAddresses, Protocols; parallel lists) available in the four `onWrapService*` hooks only; `WrappedEnv.Name` / `WrappedStep.Name` not in them (§4 scope list 6–9, §4.3.1) | Implemented | `format_strings.rs` `add_wrapped_service_scope`; this pass extended the hooks' `timeout` / `cancelation` fields to the same per-hook scope |
| S32 | `attr.worker.preemptible` is a standard attribute capability, not gated by `SERVICE` (§3.3.2.1) | Implemented | `model/capabilities.rs` `STANDARD_ATTRIBUTE_CAPABILITIES` |
| S33 | Workers SHOULD advertise `attr.worker.preemptible` | N/A locally | — (no worker fleet) |
| S34 | Submission: external Services ordered by attachment then `services` order, before the Job Template's `jobServices`; one combined start/stop list; the 10 cap is per document (§1.2.2 item 1) | Implemented | `model/job/create_job/external.rs` `apply_environment_templates`; `cli/services.rs` |
| S35 | Submission: Service names are scoped to their document — an external Service MAY share its name with a Service of the Job Template or of another attachment, the submission is not rejected for it, and a scheduler MUST keep same-named Services from different documents distinct (§1.2.2 item 2; spec commit 044d692, replacing the former cross-document collision rule) | Implemented | `model/job/mod.rs` `job::Document`, `job::Service::document` (stamped by `external.rs`; `JobTemplate` for `create_job`'s own); `cli/services.rs` keys every Service on `(document, name)` (`ServiceKey`), seeds a Service Session's in-scope `Service.*` from its own document only, and labels external Services `Service 'X' (from <doc>)` in the log and in `failed_services`; `cli/mod.rs` `task_symtab` seeds a Task or Job Template Environment with the Job Template's Services only and an attached Environment with its own document's (via `AppliedEnvironmentTemplates::environment_documents`). Tests: `test_service_external.rs` §2, CLI `test_same_named_services_in_two_documents_stay_distinct`, `test_step_service_named_like_an_external_service`; conformance `jobs/service-external-same-name-*` |
| S36 | Submission: a wrapping Environment from a document without `SERVICE` with any Service in its scope must be rejected, naming the document (§1.2.2 item 3, §4.3 rule 6) — the only submission-time check | Implemented | `external.rs` |
| S37 | A Job Template never references an external Service; `Service.*` in an Environment Template resolves within the same document (§1.2.2) | Implemented | per-document validation; test `job_template_cannot_reference_an_external_service`; at run time the per-document seeding above makes a cross-document lookup impossible as well |
| S38 | ~~Queue operators SHOULD give external Services unlikely-to-collide names~~ — withdrawn by 044d692 (names cannot make a Job Template unsubmittable) | N/A | — |
| S39 | Authors SHOULD omit `port` | N/A locally | template author advice |
| S40 | ~~`<Service>.serviceEnvironments`: an ordered list of `<Environment>`s; names unique within the list and distinct from the Job Environments and, for a Step Service, the declaring Step's Step Environments (§9 item 5.1, §9.7 item 5); each entry an ordinary `<Environment>` structurally~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service-scoped Environment list offers nothing `onEnter` does not — RFC Rejected Ideas) | N/A | the `serviceEnvironments` key is rejected as an unknown `<Service>` property (`deny_unknown_fields`; tests `template::service::tests::service_environments_is_not_a_property`, `test_service::service_environments_is_not_a_property`; conformance `job_templates/9--service-environments-not-a-property.invalid.yaml`) |
| S41 | ~~`runScope` MUST NOT be provided on a Service Environment; its scope is fixed to the declaring Service's Session (§9 item 5.2, §9.7 item 3)~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service-scoped Environment list offers nothing `onEnter` does not — RFC Rejected Ideas) | N/A | the `serviceEnvironments` key is rejected as an unknown `<Service>` property (`deny_unknown_fields`; tests `template::service::tests::service_environments_is_not_a_property`, `test_service::service_environments_is_not_a_property`; conformance `job_templates/9--service-environments-not-a-property.invalid.yaml`) |
| S42 | ~~A Service Environment has an effective `runScope` of `[SERVICE]` for the hooks-follow-`runScope` rule: a wrapping one defines `onWrapEnvEnter`, `onWrapEnvExit`, and the four `onWrapService*` hooks, not `onWrapTaskRun` (§4.3 rule 6, §9 item 5.2, §9.7 item 6)~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service-scoped Environment list offers nothing `onEnter` does not — RFC Rejected Ideas) | N/A | the `serviceEnvironments` key is rejected as an unknown `<Service>` property (`deny_unknown_fields`; tests `template::service::tests::service_environments_is_not_a_property`, `test_service::service_environments_is_not_a_property`; conformance `job_templates/9--service-environments-not-a-property.invalid.yaml`) |
| S43 | ~~A Service Environment's format strings have the declaring Service's own scope — its ports including `bindAddress`, earlier Services' `port` / `connectAddress` — unlike a Job or Step Environment with `SERVICE` in `runScope` (§9 item 5, §7.3.1 scope rule 1, §4 item 3.2 exception, §9.7 items 1–2)~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service-scoped Environment list offers nothing `onEnter` does not — RFC Rejected Ideas) | N/A | the `serviceEnvironments` key is rejected as an unknown `<Service>` property (`deny_unknown_fields`; tests `template::service::tests::service_environments_is_not_a_property`, `test_service::service_environments_is_not_a_property`; conformance `job_templates/9--service-environments-not-a-property.invalid.yaml`) |
| S44 | ~~Other Services see a Service's `serviceEnvironments` only through that Service's ports (§7.3.1 scope rule 2)~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service-scoped Environment list offers nothing `onEnter` does not — RFC Rejected Ideas) | N/A | the `serviceEnvironments` key is rejected as an unknown `<Service>` property (`deny_unknown_fields`; tests `template::service::tests::service_environments_is_not_a_property`, `test_service::service_environments_is_not_a_property`; conformance `job_templates/9--service-environments-not-a-property.invalid.yaml`) |
| S45 | ~~External Services' `serviceEnvironments` are carried through submission unchanged; the §1.2.2 item 3 wrapper check does not apply to them (they share their Service's document, which declares `SERVICE`)~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service-scoped Environment list offers nothing `onEnter` does not — RFC Rejected Ideas) | N/A | the `serviceEnvironments` key is rejected as an unknown `<Service>` property (`deny_unknown_fields`; tests `template::service::tests::service_environments_is_not_a_property`, `test_service::service_environments_is_not_a_property`; conformance `job_templates/9--service-environments-not-a-property.invalid.yaml`) |

## Expression Language (RFC "Modifications to the Expression Language")

| # | Normative statement | Status | Where |
|---|---|---|---|
| V0 | Early validation: every SERVICE value the template alone determines (literal or let-bound numeric fields, Service variable lengths, the static types of `Service.*` symbols, host/port function argument types) is checked at template validation; only Param-dependent values are deferred to job creation, where the same range checks run | Implemented | pass 8 resolved-value constraints (`SERVICE_PORT_CONSTRAINT`, `SERVICE_SECONDS_CONSTRAINT`, `SERVICE_MAX_ATTEMPTS_CONSTRAINT`) and `unresolved[...]` typing in `model/.../format_strings.rs`; `tests/integration/test_service_early_validation.rs`; conformance `9.2--port-*`, `7.3.1--service-*-type-*` |
| E0 | The host and port functions are added by the SERVICE extension and are absent from a profile that enables EXPR alone | Implemented | `ExprExtension::Service` in `expr/profile.rs`; `register_service_functions` in `expr/default_library.rs`; `ModelProfile::to_expr_profile` maps `ModelExtension::Service`; conformance `expr2.2.4--*-requires-service-extension.invalid.yaml` |
| E1 | `join_host_port(host, port) -> string` brackets an IPv6 literal, not an already-bracketed host; zone identifiers carried verbatim | Implemented | `expr/functions/host_port.rs`, `expr/default_library.rs`; `specs/expr/function-library.md` |
| E2 | `split_host_port(s) -> list[string]?`: `[host, port]`, brackets removed, `null` for no port incl. bare IPv6, error on malformed brackets | Implemented | same |
| E3 | `is_ipv4`, `is_ipv6` (bracketed or not, with or without zone) | Implemented | same |
| E4 | Method syntax `addr.join_host_port(port)` | Implemented | same |
| E5 | Templates MUST compose address+port strings with `join_host_port` ("Address forms") | N/A locally | template author rule; the samples and fixtures do |

## Lifecycle (RFC "Modifications to How Jobs Are Run", wiki *How Jobs Are Run* § Services)

| # | Normative statement | Status | Where |
|---|---|---|---|
| L1 | Ports allocated and `bindAddress` / `connectAddress` determined before any action of the Service Session (constraint 1) | Implemented | `cli/service_ports.rs` `PortAllocator`; `cli/services.rs` (allocate at Session open) |
| L1a | A port is requested or allocated in the space of its `protocol`; a UDP `port` that cannot be provided is a start failure like a TCP one (§9.2 items 2–3) | Implemented | `cli/service_ports.rs` `bind_loopback(protocol, …)` (`TcpListener` / `UdpSocket`), allocation table keyed `(protocol, number)`; the endpoints log line suffixes a UDP port with `/udp` (`dgram -> 127.0.0.1:50780/udp`); CLI test `test_job_service_udp_echo`; conformance `service-udp-port-echo` |
| L2 | No action of a Service Session begins until every Service it references is READY; non-referencing Services MAY start concurrently (constraint 2) | Implemented | `cli/services.rs` `gate` waves; `model/job/service_symbols.rs` `referenced_service_names` |
| L3 | No Task of a Step scheduled until every Job Service and Step Service of that Step is READY; Sessions formed as today (constraint 3) | Implemented | `cli/services.rs` `gate` before each Task |
| L4 | A Service stopped before any it references; every Step Service before any Job Service; reverse list order (constraint 4) | Implemented | `cli/services.rs` `stop_step_services` / `stop_job_services` (reverse start order) |
| L5 | At most one live Service Session and one running `onRun`; previous `onRun` exited before relaunch (constraint 5) | Implemented | `sessions/service_session.rs` state machine (`launch()` in `Running` is an error) |
| L6 | Service Session ends when the scope completes (all Tasks done, scope failed or canceled), on relocation, or on start failure; ended whatever its state (constraint 6) | Implemented (relocation N/A) | `cli/services.rs` `stop_step_services` / `stop_job_services` / `stop_all`, FAILED path |
| L7 | Before ending: running action canceled with its own method; `onExit` if defined and any action ran; Environments exited in reverse; working directory deleted; ports released (constraint 7) | Implemented | `sessions/service_session.rs` `end()`; `--preserve` keeps the directory |
| L8 | Constraint 7 does not apply to a lost host (constraint 8) | N/A locally | — |
| L9 | A Service started again after its Session ended begins a new Session: new ports, new working directory, Environments re-entered, `onEnter` re-run (constraint 9) | Implemented | `cli/services.rs` `start_or_recover` (new `ServiceSession`) |
| L10 | A scheduler MAY start lazily and MAY decline to start a Service whose scope will schedule no Task (constraint 10) | Implemented | `cli/services.rs` (Job Services eager at job start only if some Task will run; Step Services skipped when `--tasks '[]'`) |
| L11 | A scheduler MAY suspend a `KEEP` Service while no Task can run; a `RERUN` Service MUST NOT be suspended (constraint 10) | N/A locally | a single-process runner never pauses a Job (no suspension implemented, so the MUST NOT holds trivially) |
| L12 | A Service Session enters the scope's Environments whose `runScope` includes `SERVICE`, in Task-Session order, around the Service's actions; Environment `variables` / `openjd_env` < Service `variables` < `onEnter` env ("Services run inside Environments", §9.6) | Implemented | `sessions/service_session.rs` `enter()` step 1, `service_env_vars()` |
| L13 | An Environment's `onEnter` runs once per Service Session | Implemented | same |
| L14 | A wrapping Environment with `SERVICE` in `runScope` wraps the Service's four actions and the inner Environments; one wrap layer per Session | Implemented | `sessions/service_session.rs` "Wrap hooks"; `Session::service_wrap_hooks` |
| L15 | Instance failure: `onRun` exits (any status) while the scope has work, not because of cancelation; readiness timeout; host loss. An exit after the scope completed is not a failure ("Failure and restart") | Implemented (host loss N/A) | `cli/services.rs` `wait_instance_failure`; `sessions/service_session.rs` `ServiceRunExit.canceled` |
| L16 | Start failure: requested port unavailable, Environment `onEnter` fails, Service `onEnter` exits non-zero or times out; Session ends with constraint 7 | Implemented | `cli/service_ports.rs`; `sessions/service_session.rs` `StartFailed` |
| L17 | On failure, `RERUN`: cancel every running Task in scope, return to queue, not a Task failure; `KEEP`: running Tasks continue, fail on their own and are retried under ordinary Task retry | Partially | `cli/mod.rs` `RunContext::run_task` cancels and requeues (`RERUN`); `KEEP` lets the Task continue — but the local runner has no Task retry, so a `KEEP` Task that fails against the UNREADY Service fails the run |
| L18 | Cancel `onRun` if still running (readiness timeout) and wait for it to exit before relaunch | Implemented | `cli/services.rs` (`cancel_run` + `wait_exit`) |
| L19 | Relaunch if relaunches < `maxAttempts`; `RERUN` returns completed Tasks to the queue; MAY relaunch in the same Session; SHOULD begin a new Session when `onRun` exited before READY; MUST after a start failure | Implemented | `cli/services.rs` `start_or_recover`; `cli/execution.rs` `run_workload` resumes from the Step / first Step |
| L20 | Otherwise FAILED; a Job Service fails the Job, a Step Service fails the Step; Session ended | Implemented | `cli/services.rs`; `failed_services` in the result |
| L21 | Relocation counts as one relaunch, changes every `Service.*` value; Tasks re-resolve at execution; constraint 7 on the old Session | N/A locally (one host) | endpoint change on a new Session is handled — see choice C4 |
| L22 | Host loss: MUST NOT wait for `onExit` etc.; SHOULD terminate survivors if the host reappears | N/A locally | — |
| L23 | Authors SHOULD require non-preemptible capacity for stateful Services and prefer `RERUN` | N/A locally | template author advice |
| L24 | `RERUN` on a Step Service requeues only that Step; on a Job Service every Step returns to pending, dependencies re-resolved, stopped Step Services started again in new Sessions | Implemented | `cli/execution.rs` (Job-scope RERUN restarts from the first selected Step and stops the current Step's Services) |
| L25 | A Task failure never fails a Service; a Task failing against a READY Service is an ordinary Task failure | Implemented | `cli/execution.rs` (the Service is stopped with constraint 7 when the run fails) |
| L26 | Authors SHOULD prefer COMMAND / STDOUT when TCP connect is insufficient | N/A locally | template author advice |
| L27 | Placement: single-host runner on loopback with `bindAddress` = `connectAddress` = loopback; `connectAddress`:`port` from any host MUST reach the process | Implemented | `cli/service_ports.rs` (`127.0.0.1`) |
| L28 | Schedulers SHOULD provide a hostname for `connectAddress` when one resolves from every host | N/A locally | loopback IP is what the RFC's Placement paragraph prescribes for a single-host runner |
| L29 | ~~A Service Session enters the Service's `serviceEnvironments`, in order, after the scope's Environments and before `onEnter`, and exits them in reverse after `onExit`; later Environments take precedence for environment variables, the Service's `variables` over all; a Service Environment's format strings have the Service's own scope, `bindAddress` included (§9 item 5, "Services run inside Environments", *How Jobs Are Run* § Services)~~ — withdrawn by a91fc0d (`serviceEnvironments` removed from the RFC; a Service provisions itself in `onEnter`, which the Valkey example and the CLI test `test_valkey_example_on_enter_provisions_the_binary` now do) | N/A | — |

## Service actions, readiness, concurrency (RFC `<ServiceActions>`, `<ServiceReadinessCheck>`, wiki §9.3, §9.6, §9.6.1)

| # | Normative statement | Status | Where |
|---|---|---|---|
| A1 | `onEnter`: once per Service Session before the first `onRun`; ordinary action; non-zero / timeout is a start failure; canceled with its method if the scope ends | Implemented | `sessions/service_session.rs` `enter()` step 4, `cancel_handle()` |
| A2 | `onRun` is the service; any exit before cancelation is an instance failure; stopped by canceling with its `cancelation` method | Implemented | `drive_run`, `cancel_run` |
| A3 | `onReadinessCheck`: exit 0 ready, else not yet; MUST be defined iff type COMMAND; SHOULD be read-only w.r.t. the working directory | Implemented (SHOULD is author advice) | `drive_readiness_check` |
| A4 | `onExit` after the other actions stopped for the last time, whether or not `onRun` launched, if any action ran; runs to completion; non-zero reported but does not change the scope's outcome | Implemented | `end()` step 2; `cli/services.rs` logs the `onExit` failure and continues |
| A5 | All four actions share the Service's format-string scopes; embedded files materialized before each (subject to rule 1) | Implemented | `enter()` step 2 (written once — allowed by rule 1) |
| A6 | TCP_CONNECT: connect from the service host to the port on loopback, or on `bindAddress` when not a wildcard; close immediately; retry at an implementation-defined interval (recommended 1 s) until success or timeout | Implemented | `probe_address`, `drive_run` TCP arm (1 s per connect, 1 s re-arm) |
| A7 | COMMAND: invocations sequential; first once `onRun` launched; each next `intervalSeconds` after the previous ends; until success, timeout, or `onRun` exit; one invocation bounded by the action's `timeout` (default 30 s), overrun canceled and not ready; never a Service failure; not run again after READY | Implemented | `drive_readiness_check` |
| A8 | STDOUT: READY on `openjd_service_ready: <message>` from `onRun`; message MAY be surfaced | Implemented | `drive_run` message arm; CLI logs `is READY: <message>` |
| A9 | `timeoutSeconds` measured from `onRun` start, continuous, default 300; exceeding it fails the instance | Implemented | `drive_run` deadline arm |
| A10 | Readiness applies to every instance (after a restart the new `onRun` must pass the check) | Implemented | `launch()` resets readiness |
| A11 | Rule 1 (embedded files): materializing for one action MUST NOT modify a file a still-running action was given | Implemented | written once per Session (`enter()`); wrapper files once on first hook |
| A12 | Rule 2: `onReadinessCheck` stdout logged, no `openjd_*` message honored; status/progress/fail from `onEnter`/`onRun`/`onExit` only | Implemented | `log_check_message_ignored`; result = exit status |
| A13 | Rule 3: every captured line attributable to its action; plain-text log SHOULD tag `onReadinessCheck` lines and leave `onRun` untagged; tag added by the runtime; MAY collapse successful invocations' output | Implemented (collapse not implemented — MAY) | `sessions/logging.rs` `session_action_log!`, `ScriptRunnerBase::action_tag`. In the CLI's merged single-host log the rule's "`onRun` is the only stream present" premise fails once Tasks run, so `openjd run` additionally gives every Service Session a `SessionConfig::log_tag` (`Service <name>`): all of its lines carry `[Service <name>] ` ahead of any action tag (`[Service Files] [onReadinessCheck] …`; `onRun` lines carry the Service tag alone) and its Environment / action banners appear as single tagged lines inside `Starting Service:` (`sessions/logging.rs` `LogTag`, `session_tagged_log!`; `cli/main.rs` `SessionLogger`) |
| A14 | Rule 4: invocations never overlap, never run during `onEnter`/`onExit`, never after READY | Implemented | structural (one sequential check driver, stops on READY) |
| A15 | Rule 5: READY only if `onRun` still running when success observed; an invocation in progress when `onRun` exits is canceled and discarded; one in progress at Session end canceled before `onExit` | Implemented | `drive_run` (`check_rx` arm disabled after exit; awaits the check driver) |
| A16 | Rule 6: `onWrapServiceReadinessCheck` runs while `onWrapServiceRun` runs; wrap scripts MUST tolerate it | Implemented (tolerance is the wrapper author's) | second action slot |
| A17 | Default timeouts: `onEnter` none, `onRun` none (declared timeout expiry = instance failure), `onReadinessCheck` 30 s, `onExit` 300 s | Implemented | `model/template/service.rs` `default_timeout_seconds`; `sessions/service_session.rs` constants |
| A18 | Watch `onEnter`/`onRun`/`onExit` stdout for `openjd_status`/`openjd_progress`/`openjd_fail`; `onRun` for `openjd_service_ready` when type STDOUT; ignored from `onReadinessCheck` | Implemented | `apply_foreground_message`, `drive_run` |
| A19 | `openjd_fail` "supplies the reason … and does not itself decide success or failure, which the exit status does"; the message accompanies the failure | Partially | the message now accompanies `ServiceScriptFailed` (this pass); but openjd-rs — like the Python reference, and for every action in the crate — marks an action FAILED when it emits `openjd_fail` even if it exits 0. See open question Q1 |
| A20 | `OPENJD_SESSION_WORKING_DIR` set for every action of a Service Session | Implemented | `Session::evaluate_env_vars` |
| A21 | Watch `onEnter` for `openjd_env` / `openjd_redacted_env` / `openjd_unset_env` with the Environment rules; set for every later action of the Session incl. every `onRun` instance, `onReadinessCheck`, `onExit`; retained across relaunches; `onEnter` vars override Service `variables`; ignored from the other actions; nothing propagates to the scope | Implemented | `service_env_vars()`, `apply_foreground_message` (honor_env_messages only for `onEnter`); `openjd_redacted_env` additionally needs the Service's document to declare `REDACTED_ENV_VARS`, else it is dropped with the S6 warning in the Service Session's output |
| A22 | Service `variables` resolved when the Service is started, set for every action incl. `onReadinessCheck`; not propagated to the scope | Implemented | `enter()` step 3 |

## Wrap hooks (RFC `<EnvironmentActions>` modification rules 1–6, wiki §4.3 rule 6, §4.3.1)

| # | Normative statement | Status | Where |
|---|---|---|---|
| W1 | Rule 1 — required hooks follow `runScope`; rejected at template validation; SERVICE-less document rejected at submission when a Service is in scope | Implemented | S30, S36 |
| W2 | Rule 2 — nothing to replace: `onWrapServiceEnter`/`ReadinessCheck`/`Exit` run only for a Service defining the action; `onWrapServiceRun` always | Implemented | `sessions/service_session.rs` `resolve_action` |
| W3 | Rule 3 — single layer per Service Session | Implemented | `Session::enter_environment` (RFC 0008 check); CLI preflight over the combined stacks |
| W4 | Rule 4 — stdout scanned on the wrap script; wrapped `openjd_env` / `openjd_service_ready` honored when forwarded; both wrap scripts' output subject to attribution | Implemented | hook runs through the same runner/filter; check tag = hook name |
| W5 | Rule 5 — failure mapping: `onWrapServiceRun` = instance failure, `onWrapServiceEnter` = start failure, `onWrapServiceExit` = `onExit` failure, check status = check meaning | Implemented | `resolve_action` substitution keeps the Service action's failure path |
| W6 | Rule 6 — networking: a wrapper giving the process its own namespace MUST forward every port with the protocol given by `WrappedService.Protocols` and let it bind `bindAddress` | N/A locally | wrapper author rule; `WrappedService.Ports` / `.BindAddresses` / `.Protocols` are supplied for it |
| W7 | `WrappedService.*` four parallel lists (`PortNames`, `Ports`, `BindAddresses`, `Protocols`) in declaration order | Implemented | `model/job/service_symbols.rs` `add_wrapped_service_symbols` (`ServiceEndpoint.protocol`); endpoints re-ordered at construction; `sessions` test `wrap_hooks_replace_the_service_actions_and_forward_messages` (prints `/tcp` / `/udp` per port) |
| W8 | `WrappedAction.Environment` carries session-defined variables incl. the Service's `variables` and `onEnter` env | Implemented | `ServiceSession::wrapped_env_vars` |
| W9 | The wrapping Environment's own `onEnter` / `onExit` never wrapped | Implemented | `Session` (RFC 0008 behavior) |

## Stdout/Stderr messages (RFC "Stdout/Stderr messages", wiki *How Jobs Are Run*)

| # | Normative statement | Status | Where |
|---|---|---|---|
| M1 | `openjd_service_ready: <message>` interpreted only from `onRun` of a Service whose check type is STDOUT; more than once has no effect; ignored elsewhere and under other types; recognized on the wrap script's stdout | Implemented | `sessions/action_filter.rs` (`ActionMessage::ServiceReady`), `drive_run`, `apply_foreground_message` |
| M2 | `openjd_env` / `openjd_unset_env` from a Service's `onEnter` define variables for that Service's own later actions only | Implemented | A21 |

## Gaps

Small gaps fixed in this pass (with tests):

1. **Wrap-hook timing fields and the per-hook companion scope.** A hook's `timeout` and
   `cancelation` validated against `WrappedAction.*` only, so `WrappedService.*` (and
   `WrappedEnv.Name` / `WrappedStep.Name`) in those fields was rejected at validation
   while the runtime resolves them against the full hook scope. `format_strings.rs`
   (`validate_env_format_strings`) now seeds the companion group too. Tests:
   `test_service_environments::service_hook_timing_fields_see_wrapped_service`,
   `hook_timing_fields_reject_other_hooks_companion_groups`; conformance
   `SERVICE/env_templates/4.3.1--wrapped-service-in-service-hook-timing-fields.yaml`.
2. **`openjd_fail` message on a Service action failure.** `ServiceScriptFailed`'s reason
   now carries the `openjd_fail` message (`exit code: 2; openjd_fail: no license`), per
   RFC 0009 `<ServiceActions>` "the message accompanies it". Tests:
   `test_service_session::on_enter_failure_is_a_start_failure_and_end_still_runs_on_exit`,
   `on_exit_failure_is_reported_after_full_teardown`.

Larger gaps (not implemented; listed for the record):

- **L17 (`KEEP` + ordinary Task retry).** The local runner has no Task retry, so a Task
  that fails against an UNREADY `KEEP` Service fails the run instead of being retried.
  A scheduler with Task retries gets the RFC's behavior from the same runtime.
- **L11 suspension, L21 relocation, L22 host loss.** Not applicable to a one-process,
  one-host runner; the `ServiceSession` runtime supports a new-Session relaunch, which
  is what relocation and resumption would use.
- **A13 (MAY) collapsing the output of successful `onReadinessCheck` invocations.**
  Lines stream as they arrive; collapsing would need per-invocation buffering.

## Choices the specification leaves open

The RFC author may want to specify (or explicitly leave to implementations) each of
these. The implementation's choice and where it is recorded:

| # | Open point | openjd-rs choice | Recorded in |
|---|---|---|---|
| C1 | Order of a Step's Environment exits (Task Session) relative to its Step Services' stop | Environments exit first, then the Step's Services stop, so a TASK-scoped Environment's `onExit` can still reach the Service — mirroring the Job level | `specs/cli/run.md` "Task gating" |
| C2 | Whether an allocated port may be reused after its Service Session ends (constraint 7 says "released") | Never reused within a run, so a Task holding an old endpoint cannot reach the wrong Service | `specs/cli/run.md` "Endpoint allocation" |
| C3 | When to start Services (constraint 10 permits lazy start) | Job Services eagerly at job start (if any Task will run); Step Services when the Step is reached, before its Environments | `specs/cli/run.md` "Start ordering" |
| C4 | What happens to a READY Service that references a Service whose relaunch began a *new* Session (new endpoints); the RFC guarantees the referenced endpoint only at the referencing Session's start | Every READY Service that references the replaced one is restarted, transitively, in reverse start order, without consuming its attempts; the Task Session's Environments that may have captured the old value are exited and re-entered | `specs/cli/run.md` "Failure and restart" |
| C5 | Whether an Environment's `variables` that captured a `Service.*` value are refreshed after the endpoint changes | Yes — the Task Session re-enters all Job Environments (Job Service) or the Step's (Step Service) | same |
| C6 | Whether requeued Tasks (`RERUN`) run in the same Task Session | A new Task Session is formed (a canceled action leaves a Session ending-only); Job Environments re-entered | same |
| C7 | `Service.File.<name>` may resolve to a different path per action (rule 1) | Same path in every action; files written once per Service Session and never rewritten | `specs/sessions/service-session.md` "`enter()`" step 2 |
| C8 | TCP_CONNECT probe details: per-connect bound, retry interval, address for a wildcard `bindAddress` | 1 s per connect, 1 s between rounds, loopback of the same family (`127.0.0.1` / `::1`) for `0.0.0.0` / `::` | `specs/sessions/service-session.md` "Readiness" |
| C9 | Which wrapping-Environment embedded files (`Env.File.*` in hooks) may reference `WrappedAction.*` given rule 1 | Not allowed: wrapper files are written once per Service Session on the first hook; `WrappedService.*` and the wrapper's own scope are allowed | `specs/sessions/service-session.md` "Concurrency" rule 1 |
| C10 | How `onRun`'s exit observed *during* the stop race (scope completed, cancel not yet delivered) is classified | Not observed at all: nothing watches after the last Task, so it is never a failure | `specs/cli/run.md` "Failure and restart" |
| C11 | Whether a readiness `TimedOut` instance's `onRun` is canceled by the runtime or the scheduler | By the scheduler (CLI): the runtime reports `TimedOut` and the CLI calls `cancel_run` then `wait_exit` | `specs/sessions/service-session.md` "Readiness" |
| C12 | `WrappedAction.Cancelation.NotifyPeriodInSeconds` default for a wrapped Service action | 30 s (§5.3.2 "30 otherwise" — a Service action is not a Task's `onRun`) | `specs/sessions/service-session.md` "Wrap hooks" |
| C13 | Whether `onReadinessCheck` sees `onEnter`'s `openjd_env` variables | Yes (RFC: "every instance of onRun, onReadinessCheck, and onExit") | `specs/sessions/service-session.md` "Environment variables" |
| C14 | Log format of Service lifecycle events (banners, READY/UNREADY/FAILED lines, `failed_services` in JSON/YAML) | As listed in `specs/cli/run.md` "Output" | `specs/cli/run.md` |
| C15 | `Session.WorkingDirectory` naming for Service Sessions | `cli-<pid>-svc-<ServiceName>-<hash>` under the session root | `cli/services.rs` |
| C16 | `openjd_service_ready` from `onEnter`, or from `onRun` under TCP_CONNECT/COMMAND | Logged as ignored (one `info` line), never READY | `specs/sessions/service-session.md` message table |

## Open spec questions for the RFC author

- **Q1 — `openjd_fail` vs exit status (A19).** The RFC and wiki say "`openjd_fail`
  supplies the reason … and does not itself decide success or failure, which the exit
  status does." The base specification's *How Jobs Are Run* only says the message
  "indicates why an Action has failed", and both the Python reference and openjd-rs
  mark *any* action FAILED when it emits `openjd_fail`, regardless of exit status (an
  Environment `onEnter` printing `openjd_fail` and exiting 0 fails the Session today).
  For a Service `onRun` the two readings coincide (any exit is an instance failure),
  but for `onEnter` and `onExit` they do not. The RFC should either align with the
  implementations' behavior or state explicitly that Services change it.
- **Q2 — Wrap-hook `timeout`/`cancelation` scope (S31).** §4.3.1 / §4 scope list say
  `WrappedService.*` is "available within the four `onWrapService*` hooks". RFC 0008's
  round-trip forwarding established that a hook's `timeout` and `cancelation` resolve
  at run time with `WrappedAction.*` in scope; openjd-rs now treats the companion
  groups the same way. Worth one sentence in §4.3.1.
- **Q3 — Dependents of a relaunched Service (C4, C5).** The RFC guarantees a referenced
  endpoint only "at the referencing Service's Session start". It does not say whether a
  referencing Service (or a TASK-scoped Environment holding the endpoint in `variables`)
  must be restarted / re-entered when a relaunch begins a new Service Session. openjd-rs
  restarts dependents transitively without consuming their attempts; a scheduler might
  instead leave them running (KEEP-like) and let them fail. The RFC should state the
  expectation, and whether such a restart consumes the dependent's `maxAttempts`.
- **Q4 — Port release vs reuse (C2).** Constraint 7 says ports are "released"; whether
  a later Service Session in the same Job may receive the same number (so a stale
  endpoint could reach a different Service) is unspecified.
- **Q5 — Step Environment exit vs Step Service stop (C1).** The RFC's execution-order
  example implies Environments exit first; the normative text does not say.
- **Q6 — `Service.File.*` path stability (C7).** Rule 1 allows a different path per
  action. Templates that pass `Service.File.<name>` from `onEnter` to `onRun` via
  `openjd_env` rely on the path being stable; the RFC may want to recommend stability
  or warn authors.
- **Q7 — `KEEP` and schedulers without Task retry (L17).** The `KEEP` semantics assume
  "the scheduler's ordinary Task retry rules"; a runner with none (like `openjd run`)
  has no sanctioned behavior for a Task that fails against an UNREADY Service.
- **Q8 — Wrapper embedded files and `WrappedAction.*` (C9).** Rule 1 plus
  RFC 0008's `Env.File.*` in hooks leaves open whether a wrapper's embedded file may
  reference `WrappedAction.*` (which differs per hook) in a Service Session.
- **Q10 — `<Service>.let` names in `serviceEnvironments` (S43). Moot** — a91fc0d removed
  `serviceEnvironments` from the RFC; `<Service>.let` names are available in
  *hostRequirements*, *variables*, and *script* (§9 item 3).
- **Q11 — Cross-document Service Environment names. Moot** — a91fc0d removed
  `serviceEnvironments`; there is no Service-scoped Environment whose name could collide.
- **Q9 — `repr_sh` / `repr_py` of `WrappedService.Ports`.** `WrappedService.Ports` is
  `list[int]`; `repr_sh(list[int])` has no signature in the Expression Language, so
  the RFC's docker example must convert with `string(p)` first (as its `flatten` idiom
  does). A note in §4.3.1 would save wrapper authors a validation error.
