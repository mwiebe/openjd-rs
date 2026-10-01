# Exploratory test report: RFC 0009 (SERVICE extension) and `openjd run` in openjd-rs

Tested: `~/openjd-specifications` branch `rfc-service` (RFC 0009 + wiki changes) against
`~/openjd-rs/target/release/openjd` built from branch `service-extension`. Method and raw notes are
in `LOG.md` alongside; every run log is in `runs/`. 44 templates written, 24 logged runs plus
roughly a dozen ad-hoc checks and runs.

## 1. Executive summary

The spec is usable as written: all eleven *valid* templates I wrote from the RFC's examples alone
passed `openjd check` and ran correctly on the first attempt, including the harder cases (COMMAND
readiness with interleaved output, a Service-to-Service chain, `KEEP`/`RERUN` crash recovery, a
start failure relaunching in a new Service Session, `serviceEnvironments` isolation, and
same-named Services from two documents). The implementation's runtime logging of the Service
lifecycle is excellent: UNREADY/relaunch/READY/FAILED lines state what happened, why, and which
policy knob governs it. The two weak spots are (1) one real bug that blocks the RFC's headline
"queue-supplied Service, plain Job Template" use case, and (2) scope-violation errors that fall
through to a generic `Undefined variable` message, sometimes with a misleading "Did you mean".

Top 5 recommended changes, by impact:
1. **Bug (impl):** an attached Environment Template's `join_host_port` fails at runtime unless the
   *Job* Template declares `SERVICE` (`Unknown function: 'join_host_port'`). Contradicts the RFC;
   blocks the queue use case. Root cause: `openjd-sessions` builds one function library from the
   Job's `ModelProfile`.
2. **Impl UX:** replace generic `Undefined variable: 'Service.X.y.z'` with a scope-specific
   explanation whenever `X` is a declared Service anywhere in the document (5 of 16 wrong templates).
3. **Spec:** state next to `openjd_redacted_env` in `<ServiceActions>` that it requires the
   `REDACTED_ENV_VARS` extension; **Impl:** warn, don't silently drop, when it is used without it.
4. **Impl UX:** attribute Service output once Tasks are running (e.g. `[Service Files]` prefix) and
   show `Entering Environment` banners inside Service Sessions; fix the post-FAILED
   "Returning every completed Task..." line under `RERUN`.
5. **Spec:** add a short "How Tasks obtain a credential a Service generated" paragraph; add the
   forward-only ordering rule to the `jobServices`/`stepServices` property constraints themselves.

## 2. What worked well

| Design choice | Evidence (template) |
|---|---|
| Three scalar endpoint values + `join_host_port` | `01-http-job-service.yaml`: guessed `Service.Files.http.{port,bindAddress,connectAddress}` and the URL composition correctly from the Summary alone; worked first try. |
| Allocated ports, no `port:` needed | Every template. Two same-named `Kv` Services (job + queue) got distinct ports with zero configuration (`05z`). |
| `Service.File.<name>` embedded files in Services | `02b`, `03`, `04`, `06`, `08`: whole servers as embedded Python; the probe in `03` and the server share the `embeddedFiles` list naturally. |
| `readinessCheck` as policy + `onReadinessCheck` as action | `03-command-readiness.yaml`: `/healthz` probe with `intervalSeconds: 1`; five 503s then READY at the right second. `[onReadinessCheck]` tagging (RFC Concurrency rule 3) implemented exactly. |
| STDOUT readiness surfaces the message | `02b`: `openjd_service_ready: counter listening` → log `Service 'Counter' is READY: counter listening`. |
| Ordered list, forward-only references | `04-proxy-chain.yaml`: Proxy's `variables: UPSTREAM: "http://{{ join_host_port(Service.Backend...) }}"` — the declarative config path the RFC recommends is genuinely the nicest. Start Backend→Proxy, stop Proxy→Backend observed. |
| `variables` on a Service + `openjd_env` from `onEnter` with the stated precedence | `08b-onenter-env-probe.yaml`: `variables` reach onRun/onReadinessCheck/onExit; `onEnter`'s `openjd_env` overrides `variables`; nothing reaches Tasks. |
| `onEnter` once per Session, survives `onRun` relaunch | `06-crash-restart.yaml`: `instances.txt` written by onEnter persisted across two relaunches; onExit reported `instances launched in this Session = 2`. |
| `KEEP` vs `RERUN` | `06` / `06b`: KEEP kept Tasks 1-3 and continued 4-6 against instance 2; RERUN canceled the running Task "(not a Task failure)", requeued all, re-ran against each new instance, then FAILED with `2 of 2 relaunch(es) used`. |
| Start failure → new Service Session | `11-start-failure-env.yaml`: Env onEnter exit 3 → `Relaunching Service 'Svc' in a new Service Session (relaunch 1 of 1)`, new port, onExit correctly skipped (nothing had run). |
| `serviceEnvironments` + default `runScope` | `07-service-environments.yaml`: fake `echod` installed onto PATH via `openjd_env` is seen by the bare-binary `command: echod` and invisible to Tasks; the Job Env `Shared` was entered in both Session kinds. |
| Same-document reference rule / document-scoped names | `05z`: job's `Kv` and queue's `Kv` coexisted; log suffix `(from 05-queue-kv.env.yaml)` on every line made provenance obvious. |
| Step Service lifetime | `02b`: `Stopping Service: Counter` (with onExit output) appears before `Running step 'After'`. |
| Structural validation messages cite the RFC | `w02b`: `SERVICE requires EXPR; both must be listed in the template's extensions (RFC 0009).`; `w04`, `w07`, `w07b`, `w09`, `w12` similar. |
| Failure summary | `03b`: `Failed Service: Api (Job scope): readiness check timed out; 0 of 0 relaunch(es) used (restartPolicy.maxAttempts)`; `--output json` has `failed_services[{name,reason,scope}]`. |

## 3. Stumbles

| # | What I tried | What happened | Attempts | Class | Recommended change | Severity |
|---|---|---|---|---|---|---|
| S1 | Plain Job Template (no `extensions`) + `--environment 05-queue-kv.env.yaml` whose `KvClient` env has `KV_ADDR: "{{ join_host_port(Service.Kv.main.connectAddress, Service.Kv.main.port) }}"` | Service started; then `ERROR: Environment setup failed: Failed to resolve env var 'KV_ADDR': Unknown function: 'join_host_port'`. Adding `extensions: [EXPR]` to the job: same. Adding `[EXPR, SERVICE]` to the job: works. `upper()` in the same place works without any job extensions, so only the SERVICE-gated functions are affected. | 3 | **d** | Runtime must evaluate each attached document's format strings with that document's own extension set. See §5 bug B1. | **blocking** |
| S2 | `Service.Counter.*` from a *later* Step that depends on the Step declaring Counter (`02-step-service-stdout.yaml`) | `Undefined variable: 'Service.Counter.api.connectAddress'` | 1 (intended) | b | Message should say the Service is a Step Service of step `Count` and is not in scope for `After`. | annoying |
| S3 | Proxy listed before Backend (`04b`); `w05` | `Undefined variable: 'Service.Backend...'`; in `w05` additionally `Did you mean: Service.Cache.main.port` (a *different* Service) | 1 (intended) | b (+ a) | Say "declared later in jobServices; a Service may reference only earlier Services". Spec: put the ordering rule in the `jobServices` property constraints (§4 R3). | annoying |
| S4 | `bindAddress` of another Service (`04c`) / from a Task (`w03`) | `Undefined variable: 'Service.Backend.http.bindAddress'` | 1 | b | "bindAddress is only available within Service 'Backend'; use connectAddress". | annoying |
| S5 | `completedTasks: "{{ Param.Policy }}"` (`06`) | `unknown variant `{{ Param.Policy }}`, expected `KEEP` or `RERUN`` | 1 | c (a-minor) | RFC is explicit that only four numeric fields are `@fmtstring`; the message is clear. Optionally make `completedTasks` `@fmtstring` for symmetry. | cosmetic |
| S6 | `openjd_redacted_env: VAULT_ADMIN_TOKEN=...` from Service `onEnter` (`08`) | Log shows `openjd_redacted_env: VAULT_ADMIN_TOKEN=********` but onRun got `KeyError`. Same in a plain Environment (`08c`). Works after adding `REDACTED_ENV_VARS` to `extensions`. | 2 (+2 probes) | **a + b** | Spec: mention the extension in `<ServiceActions>`. Impl: log a warning when the directive is dropped. | annoying (looks like it worked) |
| S7 | Env template references `Service.*` without `runScope` (`05w`) or with `runScope: [TASK, SERVICE]` (`w11`) | 3× `Undefined variable` | 1 (intended) | b | "Environment 'KvClient' is entered in Service Sessions (runScope includes SERVICE) and may not reference Service.*; add runScope: [TASK]". | annoying |
| S8 | Reading mixed logs (`01`, `03`, `09d`) | `http.server` access-log lines appear *inside* Task `Output:` blocks with no attribution; in `03` the server's untagged lines sit between tagged `[onReadinessCheck]` lines. | — | b | Prefix Service stream lines with `[Service <name>]` once Tasks run (or always). | annoying |
| S9 | Reading the Service Session start (`07`, `11`) | No `Entering Environment:` banners inside `Starting Service:`; two bare `Output:` blocks. Task Sessions get banners. | — | b | Reuse the Environment banners inside Service Sessions. | annoying |
| S10 | `RERUN` after relaunch budget exhausted (`06b`) | After `is FAILED`, log still prints `Returning every completed Task of the Job to the queue: ... was relaunched` and `New Task Session for the requeued Tasks: /tmp/...`. | — | b | Suppress those two lines when the Service is FAILED. | annoying (misleading) |
| S11 | `openjd summary 02b-...yaml` | `Total environments: 0`, no Services listed. | — | b | List Job/Step Services with ports, readiness type, restart policy. | cosmetic |
| S12 | `w02` (no `extensions:` at all) | Two errors; the generic `Undefined variable` precedes the perfect `jobServices requires the SERVICE extension.` | 1 | b | Emit only the extension error (or make the first say "Service.* requires SERVICE"). | cosmetic |
| S13 | `w14` Service with no `onRun` | `missing field `onRun`` with no `jobServices[0] -> script -> actions` path | 1 | b | Surface the path like every other error. | cosmetic |
| S14 | COMMAND readiness timeout mid-invocation (`03b`) | Invocation started at 2.097s was silently canceled at 3.003s. | — | b | `[onReadinessCheck] canceled: readiness timeout`. | cosmetic |
| S15 | `nc -q1`, bash `;` inside YAML args | My own shell mistakes | 2 | c | — | — |

## 4. Spec recommendations (RFC 0009)

R1 (S6). In `<ServiceActions>`, "Environment variables within a Service": after "with the same
syntax and redaction rules as for an Environment's `onEnter`", add: "`openjd_redacted_env`
additionally requires the `REDACTED_ENV_VARS` extension, as it does for an Environment; a template
that omits it has its redacted directive ignored." Also add `REDACTED_ENV_VARS` to the Valkey
example's `extensions` if a `requirepass` variant is ever shown.

R2 (S6, item 8). Add a short paragraph (Use case 4 or Design Rationale "`variables` and
`openjd_env`") on credentials: "A Service's `onEnter` may generate a credential and hand it to
`onRun` with `openjd_redacted_env`, but nothing set within a Service reaches its Tasks. Tasks
obtain such a credential either (a) from the Service itself over the published endpoint, (b) as
a Job Parameter known to both sides, or (c) not at all, relying on network scope. A
scheduler-mediated secret channel is out of scope." Every reader of the Valkey example will ask
this on the way to `--requirepass`.

R3 (S3). In §1.1 `jobServices` and §3 `stepServices` constraints, add: "5. A Service in this
list may reference, through `Service.*`, only Services earlier in the list (and, for a Step
Service, any Job Service)." The rule currently lives only in the `Service.*` scope section and
the Environment Template's `services` text; the schema-section reader never sees it.

R4 (09c). In `<ServicePort>` after "All ports are TCP": "The default `TCP_CONNECT` readiness
check requires a TCP listener on every declared port. A Service whose process does not listen on
TCP (for example one that is reached over UDP on the allocated number) MUST use a `STDOUT` or
`COMMAND` readiness check." Authors will do this; the spec should say what happens.

R5 (09a). Future Work, co-scheduled Steps bullet: add one sentence: "This RFC gives a Task no
address of its own; a Service cannot call back to a Task, and `Session.*` has no host address
value." (`Session.HostAddress` and `Session.Hostname` were my first two guesses.)

R6 (w15, minor). In "Address forms", the MUST ("templates MUST compose such strings with
`join_host_port`") is unenforceable by validation and `w15` passes `check`. Either soften to
SHOULD or add "implementations MAY warn when `Service.*.connectAddress` is immediately followed by
`:` in a format string".

R7 (S5, optional). Consider marking `completedTasks` `@fmtstring` (enum-valued) for symmetry
with `maxAttempts`, since a Job Parameter choosing the policy is a plausible use.

## 5. Implementation recommendations (openjd-rs)

### Bugs

B1 (S1, blocking). **Attached Environment Template evaluated with the Job's extension set.**
Spec: "An extension listed here applies to the content of this document only; a Job Template does
not need to list the extensions used by the Environment Templates a scheduler applies to it, and
vice versa" and, of the queue example, "This Job Template does not use the `SERVICE` extension
at all." Observed with `openjd run 05-plain-job.yaml --environment 05-queue-kv.env.yaml`:

```
--------- Entering Environment: KvClient
ERROR: Environment setup failed: Failed to resolve env var 'KV_ADDR': Unknown function: 'join_host_port'
  join_host_port(Service.Kv.main.connectAddress, Service.Kv.main.port)
```

The Service.* *symbols* resolve (the `KV_HOST`/`KV_PORT` variants work), and `openjd check` on the
env template passes; only the SERVICE-gated functions are missing at runtime. Root cause in
`crates/openjd-sessions/src/session.rs`: `derive_library(profile, rules)` builds one
`FunctionLibrary` from the session's single `ModelProfile`, and `default_library.rs`
`register_service_functions` runs only when that profile has `ExprExtension::Service`. Fix:
evaluate each Environment's format strings under its own document's profile (or union the profiles
of every attached document into the session's library; the latter is simpler and safe since the
functions are pure). Add a test: plain job + env template using `join_host_port`.

### Error messages (validation)

E1 (S2, S3, S4, S7, w08, w10). When `Service.<svc>.<port>.<field>` is undefined and `<svc>` is
declared *somewhere* in the document, replace `Undefined variable` with the scope rule that
applies, in priority order:
- `<svc>` is a Step Service of another Step → "Service 'Counter' is a Step Service of step 'Count'
  and is not available in step 'After'."
- `<svc>` is later in the same list → "Service 'Later' is declared after 'Cache' in jobServices;
  a Service may reference only Services earlier in its list."
- `<field>` is `bindAddress` outside the declaring Service → "bindAddress is available only within
  Service 'Backend' itself; use connectAddress."
- Reference is inside an Environment whose runScope includes SERVICE → "Environment 'KvClient' is
  entered in Service Sessions (its runScope includes SERVICE) and may not reference Service.*; set
  runScope: [TASK]."
- Reference is inside `hostRequirements` or `<Service>.let` → "Service.* is never available in
  hostRequirements (resolved before the Service is placed)."
- `Task.*` inside a Service → "Task.* is never available within a Service."
Also suppress the "Did you mean" suggestion when it would point to a *different* Service than
the one named (w05 suggests `Service.Cache.main.port` for a typo-free reference to `Later`).

E2 (S12). When `SERVICE` is not declared, report only `jobServices requires the SERVICE extension`
(or prefix the Service.* errors with that reason).

E3 (S13). Wrap the serde `missing field `onRun`` error with the model path
(`jobServices[0] -> script -> actions`).

### Log output

L1 (S8). Tag Service `onRun` stdout/stderr with `[Service <name>]` at least once Tasks begin
(in a single-host log, the RFC's "onRun is the only stream present" assumption does not hold).

L2 (S9). Inside `Starting Service:` / `Stopping Service:`, print `Entering Environment: <name>` /
`Exiting Environment:` banners (indented or prefixed) and `onEnter` / `onRun` / `onExit` labels
instead of bare `Output:`. Print the Service Session working directory (`/tmp/OpenJD/cli-...-svc-
<name>-...`) once at start; it is never shown today.

L3 (S10). Under `RERUN`, do not print `Returning every completed Task of the Job to the queue` or
`New Task Session for the requeued Tasks` after the Service has become FAILED.

L4 (S14). Log cancellation of an in-flight `onReadinessCheck` on readiness timeout.

L5 (03b, 06b). The terminal error is printed three times (`ERROR:` line, in-log line, Results
section). Once in the log and once in Results is enough.

### Other

O1 (S11). `openjd summary` should list Services (name, scope, ports, readiness type, restart
policy) and count them; today a template with Services reports `Total environments: 0` and nothing
else.

O2 (01). `TCP_CONNECT` waits a full interval before its first probe; every TCP-readiness Service
costs a flat second. Probe immediately, then on the interval.

O3 (06, cosmetic). When the last Task's completion coincides with an `onRun` exit, the runtime
relaunches and stops the Service milliseconds later. Spec-permitted; could be skipped if no Task
remains to be scheduled.

## 6. Unsupported but wanted (item 9)

| Want | How far it got | Belongs |
|---|---|---|
| Task knows its own address, to register a callback with a Service (`09a`) | No symbol exists (`Session.HostAddress`, `Session.Hostname` both undefined). Workaround `hostname -I` returned two IPs and can't know which one the Service can reach. | Future work alongside co-scheduled Steps (peer reachability). Add the one-sentence note (R5). |
| Service shared by Scatter+Gather but not by `Unrelated` (`09b`) | Only Job scope; works but the Service outlives Gather and `Unrelated` can still reference it. A late crash under RERUN would rerun Scatter/Gather for nothing. | Future work "custom named scopes", which the RFC already names; honest gap. |
| UDP port (`09c`, `09c2`) | Allocating a nominal TCP port and binding UDP on the number with STDOUT readiness *works* on the single-host runner (udp-echo round trip). `protocol: UDP` is rejected clearly. | Future work `protocol:`; add R4 so authors know to avoid TCP_CONNECT. |
| Service tells the scheduler "I'm full, back off" (`09d`) | No Service→scheduler channel. In-band 503 + `Retry-After` works fine. `openjd_status:` from onRun is printed verbatim (same as for Tasks, so not Service-specific). | Nowhere in this RFC; it is a Step concurrency concern. |

## 7. Appendix: templates written

Top level (`~/service-rfc-exploration/`):
- `01-http-job-service.yaml` — Job Service `python -m http.server`, 3 Tasks fetch via `join_host_port`.
- `02-step-service-stdout.yaml` — Step Service with STDOUT readiness; second Step references it (rejected).
- `02b-step-service-stdout-ok.yaml` — same, reference removed; valid.
- `03-command-readiness.yaml` — COMMAND readiness, `/healthz` 503→200 after 4s.
- `03b-command-readiness-timeout.yaml` — same with `timeoutSeconds: 3` (fails as designed).
- `04-proxy-chain.yaml` — Backend + Proxy referencing it via `variables`.
- `04b-proxy-chain-wrong-order.yaml` — Proxy before Backend (rejected).
- `04c-proxy-uses-bindaddress.yaml` — Proxy references Backend's bindAddress (rejected).
- `05-queue-kv.env.yaml` — Environment Template: `services: [Kv]` + `runScope: [TASK]` client env.
- `05-plain-job.yaml` — Job Template without SERVICE consuming KV_HOST/KV_PORT (exposes bug B1).
- `05y-plain-job-service-ext.yaml` — same job with `extensions: [EXPR, SERVICE]` (works).
- `05z-job-own-kv-same-name.yaml` — job declares its own `Kv` alongside the queue's.
- `05w-env-missing-runscope.env.yaml` — env references Service.* without runScope (rejected).
- `05v-env-services-only.env.yaml` — env template with `services:` only.
- `05u-env-no-join.env.yaml` — env template without `join_host_port` (B1 workaround control).
- `06-crash-restart.yaml` — crashing service, `completedTasks: KEEP`, `maxAttempts` parameterized (run with 2 and 0).
- `06b-crash-restart-rerun.yaml` — same with `RERUN`.
- `07-service-environments.yaml` — serviceEnvironment installs `echod` onto PATH; Tasks can't see it.
- `08-secret.yaml` — onEnter generates a token via `openjd_redacted_env`; Tasks fetch a client token over the wire.
- `08b-onenter-env-probe.yaml` — which env directives reach onRun/onReadinessCheck/onExit/Tasks.
- `08c-redacted-env-in-environment.yaml` — control: `openjd_redacted_env` in a plain Environment.
- `09a-task-own-address.yaml` — Task's own address (unsupported).
- `09b-shared-by-two-steps.yaml` — coordinator for two Steps via Job scope.
- `09c-udp-port.yaml` — UDP on an allocated port with STDOUT readiness.
- `09c2-udp-protocol-field.yaml` — `protocol: UDP` (rejected).
- `09d-backpressure.yaml` — 503 + Retry-After backpressure.
- `11-start-failure-env.yaml` — Job Environment onEnter fails once inside the Service Session.

`wrong/` (item 10, graded in LOG.md §10): `w01` port-name typo, `w02` no extensions, `w02b` SERVICE
without EXPR, `w03` bindAddress in Task, `w04` runScope on serviceEnvironment, `w05` reference to a
later Service, `w06` `port: 80000`, `w07` COMMAND without onReadinessCheck, `w07b` onReadinessCheck
with TCP_CONNECT, `w08` `Task.*` in a Service, `w09` Service named `File`, `w10` Service.* in
hostRequirements, `w11` env `runScope: [TASK, SERVICE]` with Service.*, `w12` unknown runScope name,
`w13` Service-name typo, `w14` Service without onRun, `w15` manual `addr:port` join (passes).

Helpers: `run.sh` (check + run → `runs/<name>.log`), `runs/wrong-check.log` (all `check` output for
`wrong/`), `LOG.md` (chronological notes, including first drafts and attempt counts).

Totals: 44 templates (27 top-level, 17 in `wrong/`); 24 logged `openjd run` executions in `runs/`
plus about 12 unlogged ad-hoc `check`/`run` invocations.
