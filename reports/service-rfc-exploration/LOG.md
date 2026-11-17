# Exploration log: RFC 0009 SERVICE extension + openjd-rs `openjd run`

Conventions: each entry = intent / first draft / what check/run said / changes / attempts.
Stumble classes: (a) spec UX, (b) implementation UX, (c) my own error, (d) bug (impl vs spec).

Setup: read RFC 0009 (Summary, Overview, Basic Examples, then schema sections as needed),
skimmed `How-Jobs-Are-Run.md` Services section. CLI: `~/openjd-rs/target/release/openjd`.
`openjd run --help` shows: `--step`, `-p/--job-param`, `--environment`, `--extensions`,
`--verbose`, `--preserve`, `--timestamp-format`, `--output`. No service-specific flags.

## 01 — HTTP file server Job Service (01-http-job-service.yaml)
Intent: Job Service running `python3 -m http.server` on an allocated port; 3 Tasks fetch with urllib via `join_host_port`.
First draft: copied the shape of the RFC Valkey example; TCP_CONNECT readiness; onEnter creates `www/` in Session.WorkingDirectory.
check: pass. run: pass on attempt 1. All 3 Tasks got their files.
Attempts: 1.
Went well: `Service.Files.http.port/bindAddress/connectAddress` + `join_host_port` were exactly what I guessed; no port needed; `Session.WorkingDirectory` works in a Service. Log line `Service 'Files' (Job scope) endpoints: http -> 127.0.0.1:42171` is excellent.
Observations (log, class b):
  - http.server's access log (`127.0.0.1 - - [..] "GET /file1.txt" 200 -`) appears INSIDE the Task's `Output:` block, unattributed. An operator can't tell it came from the Service. RFC §Concurrency rule 3 only requires attribution within a Service Session; in a single-host runner the Service's stream is interleaved with Task streams in the same log. Suggest a `[Service Files]` tag on service output lines once Tasks begin (or always).
  - `Output:` appears at 0.003 (onEnter) with no banner saying "onEnter"; then `onRun launched...` then another bare `Output:`. Suggest `Running onEnter` / `Running onRun` labels like Environments get.
  - READY took exactly 1.0s: TCP_CONNECT first probe appears to wait a full interval before the first attempt. Minor; a probe at t=0 would save a second.

## 02 — Step Service, STDOUT readiness, 5 Tasks; second Step must not see it (02-step-service-stdout.yaml, 02b-...-ok.yaml)
Intent: Step `Count` has Step Service `Counter` (python socket server printing `openjd_service_ready:`), 5 Tasks `nc` to it; Step `After` depends on Count and tries to reference `Service.Counter.*`.
First draft (02): wrote the Service referencing its own bind address via embedded file `Service.File.Server` — worked without lookup.
check on 02: FAIL as intended:
    steps[1] -> script -> actions -> onRun -> args[1]:
        Failed to parse interpolation expression at [31, 71]. Undefined variable: 'Service.Counter.api.connectAddress'.
  Classification (b): correct rejection, but the message is a generic "Undefined variable". The author *did* define Counter, just in another Step. A better message: "Service 'Counter' is a Step Service of step 'Count' and is not in scope in step 'After' (Step Services are only available to their own Step)." The RFC itself anticipates this exact mistake ("A Step Service is available only to the Step that declares it; it is not available to other Steps, including Steps that depend on this one").
02b (ref removed): check pass; run pass once I fixed my own bash-quoting error (class c, unrelated to Services).
Attempts: 2 for the valid variant (1 self-inflicted).
Went well: `openjd_service_ready: counter listening` -> log `Service 'Counter' is READY: counter listening` (the message is surfaced, nice). Service stopped *before* `Running step 'After'`, and `onExit` ran and its output is shown under `Stopping Service: Counter`. `(Step 'Count' scope)` in the endpoint line makes scope obvious.
Observation (b, cosmetic): the Service's own `served N` lines appear inside each Task's Output block, unattributed (same as 01).

## 03 — COMMAND readiness, slow /healthz (03-command-readiness.yaml, 03b-...-timeout.yaml)
Intent: server binds immediately but returns 503 on /healthz for WarmupSeconds=4; probe is a python urllib script in an embedded file; intervalSeconds: 1.
First draft: guessed `onReadinessCheck` goes under `script.actions` next to onRun, probe uses `Service.File.Probe`. check pass, run pass, attempt 1.
Went well: `[onReadinessCheck]` tag on every probe line, exactly RFC rule 3; 5 probes at 1s cadence then `Service 'Api' is READY`. Probe script could reference `Service.Api.http.connectAddress` (self-reference permitted), and `Param.WarmupSeconds` in the service embedded file.
03b (timeoutSeconds: 3 < warmup): fails as expected with
    Service 'Api' (Job scope) is UNREADY: readiness check timed out
    Service 'Api' (Job scope) is FAILED: readiness check timed out; 0 of 0 relaunch(es) used (restartPolicy.maxAttempts)
    ...
    Failed Service: Api (Job scope): readiness check timed out; 0 of 0 relaunch(es) used (restartPolicy.maxAttempts)
  Went well: the summary names the Service, the cause, and the policy knob. Great operator UX.
Observations (b, cosmetic):
  - `[onReadinessCheck] Output:` banner is printed per invocation but the probe's own output is only shown when it prints something; fine. The server's stderr access log lines (`"GET /healthz" 503`) are *untagged*, which is correct per the RFC (onRun's stream is untagged) but visually they sit between tagged probe lines, so an operator might think the probe printed them.
  - Timeout fired at 3.003s while an invocation was mid-flight (started at 2.097); it was canceled silently. A line like `[onReadinessCheck] canceled: readiness timeout` would help.
  - The final ERROR line is printed 3 times (ERROR:, in-log, and Results). Repetitive but harmless.

## 04 — Proxy -> Backend chain (04-proxy-chain.yaml, 04b wrong order, 04c bindAddress of other service)
Intent: Backend http server; Proxy service forwards GETs to it, learning the upstream via its own `variables: UPSTREAM: http://{{ join_host_port(Service.Backend...) }}`; Tasks call Proxy.
First draft: check pass, run pass, attempt 1. Start order Backend→Proxy, stop order Proxy→Backend, both as spec says.
Went well: `variables` on a Service with a `Service.*` format string worked and is the nicest way to configure a process (the RFC says so and it's true). Stop order reverse of start. Sequential READY gating visible in the log.
04b (Proxy listed first, references Backend later): check FAIL
    jobServices[0] -> variables -> UPSTREAM:
        Failed to parse interpolation expression at [7, 91]. Undefined variable: 'Service.Backend.http.connectAddress'.
  Classification (b): correct rejection but same generic message as 02. Author sees "Undefined variable" for a Service that IS defined 10 lines below. Message should say: "Service 'Backend' is declared later in jobServices; a Service may only reference Services earlier in its list (move 'Backend' above 'Proxy')." This is the single most likely mistake with the ordered-list design and the error should teach the rule.
  Classification (a, minor): the RFC's ordering rule is stated in the Service.* scope section and in Design Rationale, but the `jobServices` property description in §1.1 only says "after any Service it references is READY" — a reader of the schema section alone would not learn the list must be ordered. Suggest adding "A Service may reference only Services earlier in this list" to the `jobServices` and `stepServices` property constraints (it IS in the Env Template `services` description).
04c (Proxy references Backend.bindAddress): see below.
04c result: check FAIL `jobServices[1] -> variables -> UPSTREAM: ... Undefined variable: 'Service.Backend.http.bindAddress'`. Correct per spec (bindAddress only within declaring Service), again generic message (b). Should say "'bindAddress' is only available within Service 'Backend' itself; use connectAddress".

## 05 — Queue-style Environment Template with services + runScope [TASK] client (05-queue-kv.env.yaml, 05-plain-job.yaml, 05y, 05z, 05w, 05v)
Intent: env template defines Service `Kv` (python line-protocol kv store) + Environment `KvClient` (runScope [TASK]) exporting KV_HOST/KV_PORT/KV_ADDR; plain Job Template (no extensions) uses env vars.
First draft: env template written from the RFC's queue example; check pass. Plain job: check pass.
run attempt 1: FAIL at environment entry:
    --------- Entering Environment: KvClient
    ERROR: Environment setup failed: Failed to resolve env var 'KV_ADDR': Unknown function: 'join_host_port'
  The Service started fine; the Environment's `variables` referencing `join_host_port` failed at run time even though `openjd check 05-queue-kv.env.yaml` passed.
  Attempt 2: added `extensions: [EXPR]` to the *job* template -> same error. Attempt 3: `extensions: [EXPR, SERVICE]` on the job -> works.
  Classification (d) BUG: RFC §Environment Template: "An extension listed here applies to the content of this document only; a Job Template does not need to list the extensions used by the Environment Templates a scheduler applies to it, and vice versa." And the RFC's own example says "This Job Template does not use the SERVICE extension at all." The runtime evaluates the attached Environment's format strings with the Job Template's extension set, so `join_host_port` is unknown unless the job declares SERVICE. Severity: blocking for the headline queue use case (a Job Template written before the queue Service existed cannot run). Note `check` of the env template alone is correct; the bug is in the runtime's expression-function set for attached environments (the Service.* *symbols* resolved fine; it's only the function table).
  (Also a self-inflicted stumble: `nc -q1` is not valid for ncat; switched to a python client. class c.)
05y (job declares SERVICE, EXPR): run pass; Tasks see KV_ADDR=127.0.0.1:<port>, SET/GET round-trip works, Read step sees keys=3. Went well: `Service 'Kv' (from 05-queue-kv.env.yaml)` in every log line is excellent provenance; `Param.KvGreeting` default from the env template's parameterDefinitions reached the service's embedded file.
05z (job declares its own `Kv` + queue supplies `Kv`): run pass, both start, Task reaches queue Kv via env vars and its own via Service.Kv.* — distinct ports, distinct answers. Exactly as spec: "a scheduler MUST keep same-named Services from different documents distinct". Stop order: job's Kv first, then queue's (external services placed before jobServices, reverse stop). Correct.
05w (env template references Service.* without runScope): check FAIL with 3x "Undefined variable" (correct rejection per spec constraint 2; message doesn't say *why*: should say "Environment 'KvClient' is entered in Service Sessions (runScope includes SERVICE by default) and may not reference Service.*; add runScope: [TASK]"). (b)
05v (env template with services: only, no environment:): check pass, run starts the service; the job's Tasks naturally can't find KV_HOST (KeyError, expected). Good: `environment` optional works.
Observations:
  - (a) The RFC says `openjd check` can validate every reference "without knowledge of the queue". True. But there is no way for a Job Template to *document* that it expects KV_HOST from the queue; nothing to do here, just noting that `05v` silently gave a KeyError deep in the task.
  - (b) When running with `--environment`, the run log never says which Environments/Services came from which file *at the start*; the `(from file)` suffix covers it per line. Fine.

## 06 — Crashing Service: KEEP / RERUN with maxAttempts 2, then maxAttempts 0 (06-crash-restart.yaml, 06b-crash-restart-rerun.yaml)
Intent: STDOUT-ready socket server that `os._exit(1)`s after serving 3 requests; 6 Tasks each make one request. onEnter writes an instance counter so onExit can report how many instances ran in the Session.
First draft: parameterized both `maxAttempts: "{{ Param.MaxAttempts }}"` and `completedTasks: "{{ Param.Policy }}"`.
check attempt 1: FAIL `unknown variant `{{ Param.Policy }}`, expected `KEEP` or `RERUN``.
  Classification (c)/(a-minor): the RFC marks maxAttempts/port/timeoutSeconds @fmtstring but not completedTasks; I didn't check. Error message is clear enough. A reasonable author *might* expect symmetry; low priority. Split into two files instead. (`maxAttempts: "{{ Param.MaxAttempts }}"` DID work — nice.)
KEEP run (06-keep): pass. Observed exactly the spec'd behavior: at crash, `Service 'Flaky' (Job scope) is UNREADY: onRun exited while the scope still had work (exit code: 1) (completedTasks: KEEP)` → `Relaunching Service 'Flaky' onRun in its Service Session (relaunch 1 of 2)` → `launch 2 in this Session` → READY → Tasks 4-6 continue; Tasks 1-3 kept. onEnter NOT re-run (instances.txt persisted; onExit reported 2 instances). Task 3, which triggered the crash, had already received its reply and completed (exit 0).
  Went well: the UNREADY/Relaunching/READY lines are *excellent* — they say what, why, which policy and attempt count. `launch N in this Session` and `relaunch N of M` is exactly the vocabulary the RFC uses.
  Observation (b, cosmetic): after Task 6 (last) triggered the crash the runtime relaunched (relaunch 2 of 2) only to stop it 3ms later because the scope completed. Spec permits it ("An exit observed after the scope has completed is not a failure"); here Task 6 was technically still running. Harmless.
RERUN run (06-rerun): FAIL as designed (service crashes every 3 requests, so with RERUN it can never finish: 1,2,[3 canceled] ×3). Log shows: `Canceling the running Task of Step 'Work': a Service with completedTasks: RERUN is UNREADY; the Task returns to the queue` → `Task canceled; it returns to the queue (not a Task failure)` → `Returning every completed Task of the Job to the queue: a Job Service with completedTasks: RERUN was relaunched; every Step returns to pending` → `New Task Session for the requeued Tasks: /tmp/...`. Tasks 1,2,3 rerun against instance 2, then 3. After relaunch budget exhausted: FAILED with `2 of 2 relaunch(es) used`.
  Went well: every step of RFC §Failure and restart is narrated in plain words. The "(not a Task failure)" parenthetical is exactly what an operator wants to know.
  Observation (b): after `is FAILED` the log STILL prints `Returning every completed Task of the Job to the queue: ... was relaunched` and `New Task Session for the requeued Tasks` even though no relaunch happens. Misleading: an operator would think tasks were requeued and a new session started. Should be suppressed when the Service is FAILED.
  Observation (b): `Chunks run: 6` for 9 Task executions (6 completed, 3 canceled); fine but "Tasks rerun: 6, canceled: 3" would be more honest for RERUN scenarios.
maxAttempts 0 (06-max0, `-p MaxAttempts=0`): FAILED immediately on first crash with `0 of 0 relaunch(es) used`. Tasks 4-6 never ran; `Chunks run: 3`. onExit ran. Correct.
Spec observation (a): Task 3 triggered the crash *after* receiving its reply and completed successfully under KEEP; under RERUN it was canceled even though it had the result in hand. The spec says "running Tasks are canceled" — correct by the letter, and the right call since the Task's result could depend on lost state. No change needed, just confirming the design is understandable in practice.

## 07 — serviceEnvironments installs a fake binary only the Service sees (07-service-environments.yaml)
Intent: Job Env `Shared` (default runScope → entered everywhere, exports SHARED_MARK). Service `Echo` has serviceEnvironment `FakeTool` whose onEnter writes `tools/echod` and `openjd_env: PATH=...`; onRun is the bare `echod` command. Task checks it does NOT see echod/FAKETOOL_INSTALLED but DOES see SHARED_MARK.
First draft: check pass, run pass, attempt 1.
Went well: `Env.File.Install` works in a serviceEnvironment (it is an ordinary Environment). `openjd_env: PATH=` from a serviceEnvironment onEnter made the bare-binary `command: echod` resolvable — the Valkey-from-conda story in the RFC works as described. `Shared` was entered once for the Service Session and once for the Task Session (two `Shared.onEnter running` lines), as the RFC warns ("runs its onEnter once per Service Session ... in addition to once per Task Session"). SHARED_MARK propagated to the service process (echod replied with it). Tasks saw neither FAKETOOL_INSTALLED nor echod. All correct.
Observations (b):
  - Inside `--------- Starting Service: Echo`, the Environment entries are invisible: two bare `Output:` blocks (one is Shared.onEnter, one is FakeTool.onEnter) with no `Entering Environment: Shared` / `Entering Environment: FakeTool` banner, whereas the Task Session gets a proper `--------- Entering Environment: Shared` banner. An operator can't tell which environment failed if one does. Also no `Exiting Environment` lines during `Stopping Service`. Suggest reusing the same banners inside the Service Session (indented or prefixed with the service name).
  - The Service Session working dir is `/tmp/OpenJD/cli-48737-svc-Echo-<hash>` — the `svc-Echo` infix is a nice touch, but the path is never printed by the runtime (I only saw it because PATH leaked it). Printing the Service Session dir on start would help debugging (Task Sessions get `New Task Session for the requeued Tasks: <dir>` only on RERUN).
  - Full PATH echo on `openjd_env:` lines is noisy, but that's my script's fault, not the runtime's.

## 08 — Secret from onEnter → onRun via openjd_env / openjd_redacted_env; Tasks get it another way (08-secret.yaml, 08b probe, 08c control)
Intent: Vault service: onEnter generates an admin token and emits `openjd_redacted_env: VAULT_ADMIN_TOKEN=...` plus an `openjd_env` "leak test"; onRun reads it; Tasks must prove they can't see either and obtain a *client* token over the wire.
First draft: check pass. run attempt 1: FAIL — onRun crashed `KeyError: 'VAULT_ADMIN_TOKEN'`. The log showed `openjd_redacted_env: VAULT_ADMIN_TOKEN=********` (redacted!) but the variable was not set.
  08b probe: `openjd_env: PLAIN` reached onRun/onReadinessCheck/onExit; `openjd_redacted_env: SECRET` did not; Tasks saw neither (correct). `variables` reached all three actions and onEnter's `openjd_env` overrode `variables` (`OVERRIDE_ME=from-onEnter`) — matches spec precedence.
  08c control (plain Environment, no SERVICE): same — `openjd_redacted_env` not set. So not Service-specific.
  Root cause: `openjd_redacted_env` requires `extensions: [REDACTED_ENV_VARS]` (How-Jobs-Are-Run). Attempt 2 with the extension: everything works, and the redaction even masks the value when onRun echoes it (`SECRET=********`). 
  Classification (a): RFC 0009 says "Implementations MUST additionally watch the stdout of onEnter for openjd_env, openjd_redacted_env, and openjd_unset_env, with the same syntax and redaction rules as for an Environment's onEnter" — without mentioning REDACTED_ENV_VARS. A reader following the RFC would hit exactly this. Suggest: "(openjd_redacted_env additionally requires the REDACTED_ENV_VARS extension)".
  Classification (b): the runtime redacts the line (as How-Jobs-Are-Run requires even without the extension) but silently drops the assignment. It should log a warning: "openjd_redacted_env ignored: template does not declare the REDACTED_ENV_VARS extension; no variable set". Silent-drop + visible redaction is the worst combination: it *looks* like it worked.
Attempts: 2 (plus 2 probes).
How the design wants Tasks to get a secret: the RFC is explicit that "nothing set within a Service is ever propagated to the entities in its scope", and Tasks have only `Service.*` endpoint values. So the options are (1) Tasks fetch a credential from the Service over the wire (what 08 does — the service mints a client token; fine when the network is trusted), (2) the secret is a Job Parameter known to both sides (visible in the job, so not really secret), or (3) a Job Environment with runScope [TASK] generates/distributes it — but it runs per Task Session on a different host, so it can't read the Service's working dir. There's no first-class channel. (a) Suggest an RFC paragraph under Design Rationale / Use cases naming these patterns, since "my service generates a password, how do Tasks get it" is a question every reader of the Valkey example will ask (Valkey's `requirepass` is the obvious next step).
Went well: `onExit still sees admin token? yes` — onEnter's vars persist to onExit as spec'd; `openjd_redacted_env` masking persists into onRun's output.

## 09 — Things the RFC does not obviously support
### 09a Task wants its own address (09a-task-own-address.yaml)
Tried `{{ Session.HostAddress }}` then `{{ Session.Hostname }}`: both `Undefined variable`. There is no `Session.*`/`Worker.*` address symbol anywhere in the spec. Workaround: the Task asks the OS (`hostname -I`) — fragile on multi-homed hosts (here two IPs) and doesn't know which interface the Service can reach back on. Verdict: genuinely out of scope for this RFC (RFC says reachability is guaranteed only Task→Service). Belongs in the "co-scheduled Steps" future work (peer connectivity), which the RFC already names. Would be worth one sentence in Future Work: "Tasks have no address of their own; callbacks from a Service to a Task are not supported."
### 09b Service shared by two Steps but not the whole Job (09b-shared-by-two-steps.yaml)
Only option: Job Service. Worked, but `Unrelated` ran while `Coord` was still up, and nothing stops `Unrelated` referencing `Service.Coord.*` (it did, by design). RFC's Future Work "Custom named scopes" covers exactly this and I agree it's future work; the current RFC is honest about the gap. Worth noting the practical cost: a stateful coordinator (RERUN) held for the whole Job means a late crash during `Unrelated` reruns Scatter and Gather for nothing. One mitigation available today is nothing; even `completedTasks: KEEP` wouldn't help here.
### 09c UDP port (09c-udp-port.yaml, 09c2-udp-protocol-field.yaml)
Allocated a "TCP" port and bound UDP on the same number with STDOUT readiness. **It works** (udp-echo round trip) in the single-host runner, because the runner only reserves the number in its bookkeeping. This is a loophole the RFC's "All ports are TCP" doesn't close and can't; fine. `protocol: UDP` → `unknown field `protocol`, expected `name` or `port`` — a clear error (good), and Future Work already names `protocol:` as the extension. Suggest the RFC's `<ServicePort>` text say "the runtime's TCP_CONNECT check requires a TCP listener; a Service that speaks UDP must use STDOUT or COMMAND readiness" — authors will do this regardless.
### 09d "I'm full, back off" (09d-backpressure.yaml)
No channel from a Service to the scheduler exists. In-band 503 + Retry-After works fine (Tasks back off and succeed). The Service's `openjd_status:`/`openjd_progress:` lines on onRun are *printed verbatim* in the log rather than being interpreted (no "Status:"/"Progress:" rendering as a Task's would get). RFC says implementations MUST watch onRun stdout for `openjd_status`/`openjd_progress`, so either (b) they are honored but not surfaced in the human log, or (d) they are not honored; can't tell from outside. Either way, a Service's status message would be a reasonable place to surface "FULL" to an operator. Verdict: Service→scheduler backpressure is "nowhere" for this RFC (it's a scheduler concurrency concern: the natural knob would be a Step `hostRequirements`/max-concurrency, out of scope).

## 10 — Deliberately wrong templates (wrong/*.yaml, runs/wrong-check.log)
Grading: A = tells you what's wrong AND what to do; B = correct location, wrong-ish/incomplete reason; C = misleading.
| # | Mistake | Message (abridged) | Grade |
|---|---|---|---|
| w01 | `Service.Cache.mian.port` typo | `Undefined variable: 'Service.Cache.mian.port'. Did you mean: Service.Cache.main.port` | **A** — "Did you mean" nails it. |
| w02 | no `extensions:` at all | 2 errors: `Undefined variable: 'Service.Cache.main.connectAddress'` AND `jobServices: jobServices requires the SERVICE extension.` | **B** — the second error is perfect; the first is noise that appears *before* it. Suppress Service.* undefined-variable errors when SERVICE isn't declared, or make the first error say "Service.* requires the SERVICE extension". |
| w02b | `[SERVICE]` without EXPR | `SERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).` | **A** |
| w03 | `bindAddress` in a Task | `Undefined variable: 'Service.Cache.main.bindAddress'` | **C** — it IS a defined value; the issue is scope. Should say "bindAddress is only available within Service 'Cache'; Tasks use connectAddress". |
| w04 | `runScope` on serviceEnvironment | `must not be provided on a Service Environment: its scope is fixed to the declaring Service's Session (RFC 0009).` | **A** |
| w05 | reference a *later* Service | `Undefined variable: 'Service.Later.main.port'. Did you mean: Service.Cache.main.port` | **C** — actively misleading: suggests a different Service when the author's Service exists and the fix is reordering. Should say "'Later' is declared after 'Cache' in jobServices; a Service may reference only earlier Services". |
| w06 | `port: 80000` | `jobServices[0] -> ports[0] -> port: must be between 1 and 65535.` | **A** |
| w07 | COMMAND without onReadinessCheck | `onReadinessCheck must be defined when readinessCheck.type is COMMAND.` | **A** |
| w07b | onReadinessCheck with TCP_CONNECT | `onReadinessCheck must not be defined when readinessCheck.type is TCP_CONNECT.` | **A** |
| w08 | `Task.Param.Frame` in a Service | `Undefined variable: 'Task.Param.Frame'` at `jobServices[0] -> ... args[5]` | **B** — location makes it obvious enough, but "Task.* is never available in a Service" would be better. |
| w09 | Service named `File` | `must not be 'File'; it is reserved for Service.File.* references.` | **A** |
| w10 | `Service.*` in hostRequirements | `Undefined variable` (+ an unrelated reserved-scope error from my `attr.worker.*` choice) | **B** — would be A with "Service.* is never available in hostRequirements (resolved before the Service is placed)". |
| w11 | env `runScope: [TASK, SERVICE]` + Service.* | `Undefined variable: 'Service.Kv.main.port'` | **C** — the author *tried* to set runScope and got it wrong; the message doesn't mention runScope. Should say "Environment 'KvClient' has SERVICE in its runScope and may not reference Service.*; use runScope: [TASK]". |
| w12 | `runScope: [TASKS]` | `unknown run scope name 'TASKS'; expected one of TASK, SERVICE.` | **A** |
| w13 | `Service.Cash` typo | `Did you mean: Service.Cache.main.connectAddress` | **A** |
| w14 | Service with no onRun | `missing field `onRun`` (no path!) | **B** — serde-level error loses the `jobServices[0] -> script -> actions` path that every other error has. |
| w15 | `http://{{ addr }}:{{ port }}` manual join | passes | — correct (legal), but a lint/warning "use join_host_port for IPv6 safety" would be in the spirit of the RFC's MUST. Noted as (a)/(b) nice-to-have. |
Pattern: every *structural* check the RFC lists in §Validation has a tailored, RFC-citing message (A). Every *scope* violation (items 1-2 of §Validation) falls through to the generic expression-engine "Undefined variable", sometimes with a wrong "Did you mean". That's 5 of 16 cases, and they're the subtle ones. One fix: when an undefined `Service.<x>.<y>.<z>` is encountered and `<x>` IS a declared Service somewhere in the document, emit a scope-specific explanation instead.

## 11 — Runtime edge cases + log review (11-start-failure-env.yaml; --step, --verbose, --output json, summary)
- 11 start failure (Job Env `Provision` onEnter fails on first entry only): log shows `Service 'Svc' (Job scope) is UNREADY: failed to start: Environment 'Provision' onEnter failed: exit code: 3` → `Relaunching Service 'Svc' in a new Service Session (relaunch 1 of 1)` → Stopping (Svc.onExit correctly NOT run since Svc.onEnter never ran) → new Session with a NEW port (34061→42115), Environment re-entered, Svc.onEnter, READY. Exactly RFC "start failure ... MUST begin a new Service Session (constraint 9)". Went well: "in a new Service Session" vs "in its Service Session" wording distinguishes the two relaunch modes.
- `--step After` on 02b: Step Service `Counter` of the other Step is not started (correct). `--step Count --maximum-tasks 1`: service started and stopped around one Task.
- `--verbose`: no additional Service-related lines; same output. (b, minor) verbose could print the Service Session working dir and the per-instance PID.
- `openjd summary`: reports `Total environments: 0` and lists no Services at all. (b) Should list jobServices / stepServices (name, ports, readiness type, restart policy) — this is the "tooling parseable" promise made concrete.
- `--output json`: `failed_services: [{name, reason, scope}]` is present and clean; `status: error`. Good. Note the JSON is preceded by the human log on stdout (needs `grep -v '^0:'`) — but that's pre-existing CLI behavior.
- Operator read of a mixed log (07, 11): the Service Session's Environment entries have no banners; a failing Environment onEnter *is* named in the UNREADY line, which saves it. Service output interleaving with Task output remains the main readability problem on multi-request services (01, 03).
