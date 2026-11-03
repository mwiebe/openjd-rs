# ENV_PARAMS prototype plan for openjd-rs

This plan maps the Environment Parameters extension (`ENV_PARAMS`) onto the openjd-rs crates and orders the work
into PR-sized increments. It is a planning document: it changes no code, and each increment below is made later
through the `implement-change` skill.

## Sources

* **Normative spec.** `~/projects/rfc-env-params/openjd-specifications-ep`, branch `rfc-env-params` at `cbc1f27`:
  `rfcs/NNNN-environment-parameters.md` ("RFC") and `git diff mainline -- wiki/`, which changes
  `wiki/2023-09-Template-Schemas.md` ("Schemas"), `wiki/How-Jobs-Are-Run.md` ("HJAR"), and
  `wiki/2026-02-Expression-Language.md` ("ExprLang"). Section numbers below (§3.2, §4.5, …) are Schemas sections.
  Review rounds 1–3 are in `docs/env-params/review-*.md` there. Round 3's verdict is READY with no required changes;
  the RFC's two Open Questions are non-blocking (`review-3.md:128`), and round 3's six wording suggestions were applied (`review-3-response.md`). No reviewer item is
  left unresolved, so the open points below come from planning, not from the reviews.
* **RFC vs wiki.** The RFC's embedded §3.2–§3.2.2 and §4.5 are identical to the wiki once absolute links and
  heading levels are normalized (checked in review round 3 and again in this plan's audit with `diff -w`). The
  remaining wording differences are not normative:
  * §1.2.1: the RFC says "The merge" where the wiki says "This merge".
  * §7.4 Template validation: the RFC says "With `EXPR` it is type-checked using the declared types"; the wiki row
    says "With the `ENV_PARAMS` extension: `Env.Param.*` is type-checked …" without naming `EXPR`. Nothing is
    type-checked without `EXPR`, so both mean the same. The prototype type-checks only with `EXPR`.
  * §7.4 Job creation: the wiki row says "PATH and `LIST[PATH]` Job Parameter defaults" where mainline said
    "parameter defaults"; the RFC's §7.4 bullets do not quote that clause. Wording only.
  * The wiki's §7.4 Unknown and Known Values columns, and the RFC's §7.4 bullets, name `Env.Param.*` but not
    `Env.RawParam.*` (see A15).

  No normative disagreement was found, so there is nothing for the prototype to resolve in favor of the wiki.
* **Code baseline.** This repo at `0ac1d9b`, the head of `env-params-prototype` (equal to `main` at planning time).
  All `file:line` citations below refer to that commit.
* **Analogue.** `~/openjd-rs` branch `service-extension` at `52398e0` (58 commits in `main..service-extension`),
  which threads the `SERVICE` extension (RFC 0009) through openjd-model, openjd-sessions, and openjd-cli. Its
  `<StepDependency>` `service:` form, its `job::Document` provenance type, and its submission-time
  `apply_environment_templates` stage (`crates/openjd-model/src/job/create_job/external.rs:404` there) are the
  closest analogues. This branch does not depend on it. Where the two touch the same type, the plan reuses that
  branch's shape where it can and lists the remaining differences (see [Design decisions](#design-decisions)), so a
  later merge reconciles named differences rather than discovering them.

## Current code that the extension touches

| Area | Today | Where |
|------|-------|-------|
| `<Environment>` schema type | `name`, `description`, `script`, `variables`; `deny_unknown_fields` | `crates/openjd-model/src/template/environment.rs:17` |
| Environment Template schema type | `specificationVersion`, `extensions`, `parameterDefinitions`, `environment`; no `$schema` field, so `$schema` is rejected as unknown | `crates/openjd-model/src/template/environment_template.rs:15` |
| `<StepDependency>` schema type | a struct with one required `depends_on: String` | `crates/openjd-model/src/template/step.rs:292` |
| Job Parameter definitions | enum over the 12 types; each typed `default` is a literal (`FlexInt` for INT rejects a non-numeric string such as `"{{Param.X}}"`) | `crates/openjd-model/src/template/parameters.rs:58`, `:838` |
| Extension registry | `ModelExtension` enum and `ALL`; `ALL` feeds the CLI's default `--extensions` and openjd-for-js. `openjd run <template> --help` instead hardcodes a list that already lacks `WRAP_ACTIONS`; a test asserts `ALL.len() == 5` | `crates/openjd-model/src/types.rs:356`, `:369`; `crates/openjd-cli/src/common.rs:168`; `crates/openjd-for-js/src/model.rs:372`; `crates/openjd-cli/src/help.rs:67-72`; `crates/openjd-model/tests/integration/test_model_profile.rs:186` |
| Dependency structural checks | self-dependency, unknown step, duplicate, empty list | `crates/openjd-model/src/template/validate_v2023_09/structure.rs:216-242` |
| Environment format-string scope | `build_session_scope_symtab`: `Param.*`, `RawParam.*`, `Session.*`, `Env.File.*`, and with `EXPR` `Job.Name` and (Step Environments) `Step.Name`. Wrap hooks validate against a clone of that table plus `WrappedAction.*` | `crates/openjd-model/src/template/validate_v2023_09/format_strings.rs:112`, `:2419-2425` |
| Environment Template format strings | validated in their own document's scope | `format_strings.rs:2258` |
| Validation passes | job template passes 5–10; environment template passes | `crates/openjd-model/src/template/validate_v2023_09/mod.rs:188-205`, `:210-266` |
| Job types | `Job` (manual `Hash`), `Step` (derived `PartialEq`, `Hash`, `Serialize`, `Deserialize`), `Environment` (manual `Hash`), `StepDependency { depends_on: String }`. `JobParameter` derives `Serialize` only, so it cannot be a field of `Step` | `crates/openjd-model/src/job/mod.rs:71`, `:82`, `:94-100`, `:103-105`, `:147`, `:456` |
| A Step's stored symbol table | `Step::resolved_symtab` holds only the symbols that the Step's remaining host-side format strings reference (`filter_symtab_for_step`), although the field's doc comment says it holds the full step scope | `crates/openjd-model/src/job/create_job/instantiate.rs:192-209`, `:909`; `crates/openjd-model/src/job/mod.rs:111-118` |
| Transport form of values | `SerializedSymbolTable` derives `PartialEq`, `Eq`, `Hash` over its JSON form, which keeps a FLOAT's original text, so `1.0` and `1.00` compare unequal there | `crates/openjd-expr/src/symbol_table.rs:398-403` |
| Integration test registration | each `tests/integration/test_*.rs` is compiled only if `tests/integration.rs` includes it with a `#[path]` module line | `crates/openjd-model/tests/integration.rs:5-10`; `crates/openjd-sessions/tests/integration.rs:5-8` |
| Job creation | `create_job(job_template, values, ctx)` sees no Environment Templates; every evaluation uses `PathFormat::Posix` | `crates/openjd-model/src/job/create_job/mod.rs:100`, `:58` |
| Environment Templates at submission | only their Job Parameter definitions are merged, in `preprocess_job_parameters` | `crates/openjd-model/src/job/create_job/parameters.rs:24`, `:853` |
| Dependency conversion and graph | every template dependency becomes a `job::StepDependency`; the graph treats each as a Step edge | `crates/openjd-model/src/job/create_job/instantiate.rs:181-187`; `crates/openjd-model/src/job/step_dependency_graph.rs:71` |
| Entering an Environment | `Session::enter_environment(env, resolved_symtab, identifier, os_env_vars)` | `crates/openjd-sessions/src/session.rs:1133` |
| Exiting an Environment | `exit_environment` takes `resolved_symtab` again from the caller | `crates/openjd-sessions/src/session.rs:1479` |
| Session symbol table | deserializes the base table with the host path format and re-maps `Param.*` PATH and LIST[PATH] using the Session's Job Parameter types | `crates/openjd-sessions/src/session.rs:2461` |
| Wrap-hook scope | a wrapping Environment's hooks read its stored `resolved_symtab` | `crates/openjd-sessions/src/session.rs:2975` |
| `openjd run` | one Session for the whole run: enters every `--environment` Environment, then every Job Environment, then runs each selected Step | `crates/openjd-cli/src/run/execution.rs:9`, `:341`, `:378`; `crates/openjd-cli/src/run/mod.rs:90` |
| `openjd check` | validates one document; takes no `--environment` | `crates/openjd-cli/src/check.rs:21` |
| `openjd summary` | accepts `--environment` for parameter merging | `crates/openjd-cli/src/summary.rs:30`, `:77` |

## Design decisions

These choices shape every increment. Each follows from the spec; where the spec leaves the choice open, the
[ambiguities](#3-rfc-ambiguities-and-the-prototypes-interpretation) section records it.

1. **`EnvironmentParameterDefinition` wraps the Job Parameter definition and owns its `default`.** The RFC says to
   reuse the Job Parameter shapes (RFC Implementation Impact), but a typed `default` cannot hold a format string
   (`FlexInt` rejects `"{{Param.SeatCount}}"`, `parameters.rs:838`). The new type deserializes by removing `default`
   from the object, decoding the rest as a `JobParameterDefinition`, and keeping `default` as an
   `EnvironmentParameterDefault`: `FormatString(FormatString)` when the value is a string containing `{{`, otherwise
   `Literal(serde_json::Value)` (a string without `{{`, a number, a bool, or an array). A literal is checked by
   re-inserting it and decoding the full definition, so its error messages are the Job Parameter ones. That
   re-decode runs in the validation pass, not inside `Deserialize`, because the Job Parameter decoders report serde
   messages with no path (`Cannot parse '{{Param.X}}' as integer`) and the pass attaches the field path. The validator
   then rejects `userInterface`, `objectType`, `dataFlow`, and `RANGE_EXPR` (§4.5 items 1–3). Serde accepts a
   lowercase type name; without `EXPR` only the decode pass in `template/parse.rs:203-260`
   (`collect_written_type_names`, `check_type_name_canonical_case`) rejects one, and it walks only the top-level
   `parameterDefinitions` and the Task parameter definitions. PR 1 extends it to Environment `parameterDefinitions`.
2. **`StepDependency` becomes a struct of optional keys, as on `service-extension`.** Today's required
   `depends_on: String` becomes `depends_on: Option<String>`, plus `environment: Option<String>`,
   `required: Option<bool>` (absent means `true`, §3.2 item 3), and `parameters: Option<EnvironmentParameterValueMap>`.
   `EnvironmentParameterValueMap` is a newtype over `BTreeMap<String, EnvironmentParameterValue>`. Written order is
   not available to keep: the workspace's `serde_json` has no `preserve_order` feature (`Cargo.lock:3315-3325`), so
   a template map reaches the decoder sorted by key. A `BTreeMap` matches that and lets `StepDependency` derive
   `PartialEq`, `Eq`, and `Hash`, as it does on `service-extension`, which needs them for a
   `HashSet<&StepDependency>` (`template/step.rs:338` and `validate_v2023_09/service.rs:368` there). Today's
   template `StepDependency` derives only `Debug`, `Clone`, `Deserialize` (`template/step.rs:290`). *As
   implemented (PRs 1–2):* `StepDependency` derives `PartialEq` and `Eq` but not `Hash`, because
   `EnvironmentParameterValue::Literal` holds a `serde_json::Value`, which has no `Hash`; a later merge with
   `service-extension` must key its `HashSet` on the target instead. A `target()`
   accessor returns a `DependencyTarget` (`Step` or `Environment`), and `None` for a malformed entry, which pass 6
   reports (constraint 1). `service-extension` uses the same struct-of-options shape with a `service` key
   (`template/step.rs:289-348` there), so adding `service` later is one field and one variant. An untagged enum was
   rejected: serde's error for an entry that matches no variant does not name the key at fault.
   `EnvironmentParameterValue` keeps the value as written (`serde_json::Value`) and classifies it the way decision 1
   classifies a `default`: a string containing `{{` is a format string, and anything else is a literal. An object or
   `null` is no parameter type's literal and is a TV error for every entry (A4).
3. **The job's Step dependency list stays Step-only.** `job::StepDependency { depends_on: String }`
   (`job/mod.rs:456`) is unchanged, and instantiation drops `environment:` entries from it, so the dependency graph
   and the CLI's `--run-dependencies` keep working unchanged. A list left empty by the filter becomes `None`, so a
   Step whose `dependencies` hold only `environment:` entries serializes as a Step with no dependencies. Environment entries carry no ordering (§3.2 item 2).
   `service-extension` changes this type to `depends_on: Option<String>, service: Option<String>`
   (`job/mod.rs:1195-1210` there) for its own reason; that change is independent of this one.
4. **Effective values live on `job::Step`, in transport form.** A new field
   `environment_parameters: Option<Vec<EnvironmentParameterValues>>` lists, for each Environment the Step's Sessions
   enter that declares parameters, an `EnvironmentParameterValues { document: Document, environment: String, values:
   SerializedSymbolTable }`. `values` holds one entry per parameter, keyed by the bare parameter name, with the
   parameter's own type (`path`, `list[path]`, …) in the transport `type` field, so a Session can tell which values
   to path-map. `JobParameter` cannot be used because it has no `Deserialize` (`job/mod.rs:94`).
   `SerializedSymbolTable` already derives `PartialEq`, `Eq`, and `Hash`, so the new struct derives them and the
   derived impls of `Step` and the manual `Hash` of `Job` (which hashes `steps`) cover it. `Document` is copied
   from `service-extension` (`job/mod.rs:440-455` there: `JobTemplate` or `EnvironmentTemplate { index, label }`)
   so a runtime keys values by document as well as name, which keeps an attached Environment distinct from a
   same-named Job Environment (§3.2.1 item 4). Job Environment names are unique within the Job Template, so
   `(document, environment)` identifies one Environment. The new `Step` fields (this one and decision 6's) are
   `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a Job without Environment parameters serializes
   exactly as today, including through openjd-for-js's `Job.toJSON()`, which serializes `job::Job` with serde
   (`crates/openjd-for-js/src/model.rs:280-285`).
5. **External resolution is a separate submission stage.** `create_job` does not see Environment Templates
   (`create_job/mod.rs:100`), so it computes effective values for the Job Template's own `jobEnvironments` and
   leaves entries that name no Job Environment unresolved on the Step (decision 6). A new public function
   `apply_environment_templates(job, attached: &[AttachedEnvironmentTemplate], job_parameter_values, ctx)`
   resolves those entries (§3.2.1), computes the attached Environments' effective values per Step, checks
   constraints 5–7 over the combined Job, and returns `AppliedEnvironmentTemplates { environments,
   environment_documents, step_environment_parameters, warnings }` with `into_combined_job(job)`. The function name,
   `AttachedEnvironmentTemplate { template, label }`, the `environments` and `environment_documents` fields, and
   `into_combined_job` match `service-extension`'s `job/create_job/external.rs:70-177`. Two differences remain,
   to reconcile when both land: this function takes `ctx: &ValidationContext` where that one takes
   `caller_limits: &CallerLimits`, because it needs the submission path format (decision 7) as well; and the result
   adds `step_environment_parameters` and `warnings` where that one has `external_services` and
   `requirement_bindings`.
6. **`create_job` freezes what each unresolved entry needs.** An entry for an external Environment cannot be
   converted to a type until the Environment's definition is known, and `Step::resolved_symtab` cannot be reused to
   evaluate it later, because it is filtered to the symbols the Step's host-side format strings reference
   (`instantiate.rs:192-209`). `create_job` therefore stores, for each unresolved entry, an
   `UnresolvedEnvironmentEntry { environment, required, parameters, symtab }` on the Step, where `parameters` is a
   `BTreeMap<String, serde_json::Value>` in key order (so `Step` keeps its derived `Hash`; `IndexMap` has no `Hash`,
   `job/mod.rs:80-81`) and keeps each value as written and `symtab` is a `SerializedSymbolTable` filtered, by the same mechanism as
   `filter_symtab_for_step`, to the symbols those values reference out of the full step scope (`Param.*`,
   `RawParam.*`, `Job.Name`, `Step.Name`, and the Step's `let` bindings). `apply_environment_templates` evaluates
   each value against that table with the parameter's type as the target type, and drops the unresolved entries
   from the combined Job. The doc comment of `Step::resolved_symtab` (`job/mod.rs:113-114`), which overstates what
   it holds, is corrected in the same PR.
7. **The submission path format is an explicit input.** PATH values must be absolute in the submission path format
   (§4.5), but job creation evaluates in `PathFormat::Posix` (`create_job/mod.rs:58`) and has no path-format
   input. `ValidationContext` gains `submission_path_format: PathFormat`, set with `with_submission_path_format` and
   defaulting to `PathFormat::host()`, which is what `openjd run` passes to `preprocess_job_parameters`
   (`run/execution.rs:117`). The absoluteness check uses it; the stored values stay unmapped strings. `ValidationContext` also gains
   `allow_uri_path_values: bool`, with the same meaning and the same default (`false`) as the
   `PathParameterOptions` field (`create_job/parameters.rs:818-833`); `openjd run` sets it to `true`, as it does for
   Job Parameters (`run/execution.rs:119`). See A12. Every caller of `create_job` that has `PathParameterOptions`
   passes the same two values on: `openjd run`, `openjd summary`, and openjd-for-js's `createJob`
   (`crates/openjd-for-js/src/model.rs:649-663`), whose default `PathFormat::host()` would otherwise be POSIX on
   wasm32.
8. **Equality has one home.** `openjd_model::job::environment_values_equal(a: &Step, b: &Step) -> bool` (or the
   equivalent method on `Step`) implements §3.2.2 equality by decoding both tables and comparing typed values, and
   both the CLI and any scheduler call it. It cannot be the derived `PartialEq`, which compares transport JSON and so
   treats FLOAT `1.0` and `1.00` as different (`symbol_table.rs:398-403`; A1). Sessions do not decide which Tasks
   share a Session; they expose the values they entered each Environment with so a caller can check.
9. **`ENV_PARAMS` stays out of `ModelExtension::ALL` until the runtime supports it.** `ALL` is the default allowlist
   of `openjd check`, `openjd run`, and openjd-for-js (`common.rs:168`, `for-js/src/model.rs:372`). If PR 1 put the
   new variant in `ALL`, then between PR 1 and PR 5 the CLI would accept `ENV_PARAMS` templates by default and run
   them without binding `Env.Param.*` or grouping Sessions. PR 1 therefore adds `ModelExtension::EnvParams` with
   `as_str` and `FromStr` but leaves it out of `ALL`, with a doc comment saying why. The doc comment of `ModelProfile::latest()`, which
   promises "every known extension enabled" (`types.rs:522-523`), gets the same caveat until PR 5. PRs 1–4 test it by passing
   `ENV_PARAMS` explicitly to the model and sessions APIs, with a `ValidationContext` built with it (`create_job`
   rejects a context that lacks an extension the template declares, `create_job/mod.rs:123-141`). The CLI's `--extensions` accepts only names in `ALL`
   (`common.rs:206-231`), so the CLI rejects `ENV_PARAMS` until PR 5 adds it to `ALL`.

## 1. Feature inventory

Every normative statement in the RFC and wiki diff, by the spec text that states it, mapped to the crate and module
that implements it and the increment ([PR table](#2-pr-and-commit-order)) that lands it. "TV" is template
validation, "JC" is job creation (`create_job`), "SUB" is `apply_environment_templates`.

### 1.1 Extension gating and document schemas (openjd-model)

| # | Statement | Spec | Stage | Module | PR |
|---|-----------|------|-------|--------|----|
| G1 | `ENV_PARAMS` is a recognized extension name for Job and Environment Templates | Schemas §1.1 item 3 extension list; RFC Job Template | decode | PR 1: `types.rs` `ModelExtension::EnvParams` with `as_str` and `FromStr`, kept out of `ALL` (decision 9). PR 5: add it to `ALL`; switch `crates/openjd-cli/src/help.rs:67-72` to `ALL`; update `test_model_profile.rs:186`, `crates/openjd-for-js/tests/test_supported_extensions.rs`, `js-tests/smoke.test.js`, and the docs that list the default extensions | 1, 5 |
| G2 | A document that uses Environment `parameterDefinitions` or an `environment:` entry must list `ENV_PARAMS`; otherwise each use is rejected | RFC Backward compatibility bullet 1; Schemas §4 item 3, §3.2 item 2 | TV | new `template/validate_v2023_09/env_params.rs`, "validate or reject" pass 11 in `validate_job_template` and `validate_environment_template` (`mod.rs`), as `wrap_actions.rs` does for RFC 0008 | 1 |
| G3 | `ENV_PARAMS` does not require `EXPR`; without `EXPR` an Environment parameter's type is STRING, INT, FLOAT, or PATH | RFC Backward compatibility bullet 2; §4.5 item 1 | TV | `env_params.rs`, reusing the Job Parameter type gate in `structure.rs` | 1 |
| G4 | An extension applies to its own document only: a Job Template without `ENV_PARAMS` runs with an Environment Template that has it (all defaults), and vice versa (entries find no parameters) | RFC Backward compatibility bullet 3; Schemas §1.2 item 3 | TV, SUB | per-document `ValidationContext` in `parse.rs` (already per document); `external.rs` reads each attachment's own extensions | 1, 3 |
| G5 | The Environment Template accepts and ignores `$schema` | Schemas §1.2 item 2 | decode | `template/environment_template.rs`: `#[serde(rename = "$schema")] schema: Option<String>`, the field `service-extension` adds | 1 |
| G6 | A Job with no `environment:` entries and no Environment `parameterDefinitions` behaves exactly as today | RFC Backward compatibility last bullet | all | regression: the existing suites and the conformance suite unchanged | every |

### 1.2 `<Environment>.parameterDefinitions` and `<EnvironmentParameterDefinition>` (openjd-model)

| # | Statement | Spec | Stage | Module | PR |
|---|-----------|------|-------|--------|----|
| D1 | `<Environment>` gains `parameterDefinitions: [<EnvironmentParameterDefinition>]` | §4 item 3 | decode | `template/environment.rs`, new `template/env_parameters.rs` (decision 1) | 1 |
| D2 | 1–50 entries, unique names; a name may equal a Job Parameter's or another Environment's | §4 item 3 constraints 1–3 | TV | `structure.rs` list limits, `env_params.rs` uniqueness | 1 |
| D3 | Permitted on `jobEnvironments` entries and an Environment Template's `environment`; not on `stepEnvironments` | §4 item 3 constraint 4 | TV | `env_params.rs` | 1 |
| D4 | Any Job Parameter type available in the document except `RANGE_EXPR`; `userInterface`, `objectType`, `dataFlow` not permitted | §4.5 items 1–3 | TV | `env_parameters.rs` validator | 1 |
| D5 | `allowedValues`, `minLength`, `maxLength`, `minValue`, `maxValue`, `item` as for the Job Parameter of the same type, and they constrain every effective value | §4.5 item 5 | TV (definition), JC/SUB (values) | definition: reuse each type's `validate_definition` (`parameters.rs:532` etc.); values: reuse `check_constraints` (`parameters.rs:496`, `create_job/parameters.rs:343`) | 1, 3 |
| D6 | `default` is optional and is a format string resolved at job creation | §4.5 item 4 | decode, JC | `EnvironmentParameterDefault` (decision 1) | 1, 3 |
| D7 | A `default` may reference `RawParam.<name>` of any Job Parameter and `Param.<name>` of a non-PATH, non-LIST[PATH] Job Parameter of the same document's top-level `parameterDefinitions`, and nothing else | §4.5 item 4; RFC "Values resolved at job creation" | TV | new default-scope symtab in `format_strings.rs` (`build_param_symtab` minus PATH `Param.*`) | 2 |
| D8 | Those Job Parameters have their merged values, so a Job Template that redefines a Job Parameter's `default` changes the Environment parameter's `default` | §1.2.1 last paragraph | JC, SUB | `build_symbol_table` over the merged `JobParameterValues` (already merged by `preprocess_job_parameters`) | 3 |
| D9 | Environment parameters are never merged with Job Parameters or with each other | §1.2.1 last paragraph | JC | `merge_job_parameter_definitions` (`create_job/parameters.rs:24`) reads top-level definitions only; a test pins that Environment parameters do not enter it | 3 |
| D10 | With `EXPR`, a `default` that is one `{{ }}` expression evaluates with the parameter's type as target; otherwise the resolved string converts as a submitted Job Parameter value would, without joining a relative path to any directory | §4.5 item 4 | JC, SUB | new `job/create_job/env_params.rs`, reusing `coerce_from_str` / `coerce_to_job_parameter_type` (`create_job/parameters.rs:723`, `:662`) with no template-dir join | 3 |
| D11 | A list-typed `default` that is not a literal must be a single `{{ }}` expression | §4.5 item 4 | TV | `env_params.rs` | 2 |
| D12 | A literal `default` is checked at TV against type and constraints, even when every Step supplies the parameter. For PATH, `allowedValues`, `minLength`, and `maxLength`, and for LIST[PATH], `minLength`, `maxLength`, and the per-path constraints under `item`, are checked at TV too, since they do not depend on the path format; job creation checks them again with absoluteness (A16 covers the empty string) | §4.5 "checked at two points" item 1; PATH paragraph | TV | `env_parameters.rs` (decision 1 re-decode) | 1 |
| D13 | For PATH and LIST[PATH], absoluteness and the other path-format checks of a literal `default` happen at job creation; a relative literal `default` is an error there even if no Step uses it | §4.5 point 1, PATH paragraph | JC, SUB | `job/create_job/env_params.rs`, using `submission_path_format` (decision 7) | 3 |
| D14 | A non-literal `default` is resolved at job creation and checked only for each Step that takes it | §4.5 point 2 | JC, SUB | `job/create_job/env_params.rs` | 3 |
| D15 | A parameter without a `default` is required: every Step whose Sessions enter the Environment must supply it | §4.5; §3.2 constraint 7 | TV, SUB | see E15 | 2, 3 |
| D16 | PATH values (and each LIST[PATH] element), supplied or default, are in the submission path format, must be absolute in it or (with `EXPR`) a URI, and have their string constraints checked against the unmapped value | §4.5 PATH paragraph | JC, SUB | `job/create_job/env_params.rs`, with its own absoluteness check rather than `PathResolver` (which passes `""` through, A16); URIs per A12 | 3 |
| D17 | With `EXPR`, a `default` or a Job Environment entry value that is a single `{{ }}` expression is type-checked at TV against the parameter's type, as other typed expressions are; a static type that cannot convert to it is an error (A17) | §7.4 Template validation row ("With the `EXPR` extension: type checking of expressions using declared parameter types"); §4.5 item 4; §3.2 bullet 2 | TV | `format_strings.rs`, with the parameter's expression type as the target type of the default scope and the entry-value scope | 2 |

### 1.3 The `environment:` form of `<StepDependency>` (openjd-model)

| # | Statement | Spec | Stage | Module | PR |
|---|-----------|------|-------|--------|----|
| E1 | A `<StepDependency>` is `dependsOn:` or, with `ENV_PARAMS`, `environment:` with optional `required` (default `true`) and `parameters` | §3.2 | decode | `template/step.rs` (decision 2) | 1 |
| E2 | Exactly one of `dependsOn` / `environment`; `required` and `parameters` only with `environment` | §3.2 constraint 1 | TV | `structure.rs:216-242`, extended | 1 |
| E3 | `environment` must not name a `stepEnvironments` entry of the same Step | constraint 2 | TV | `structure.rs` | 1 |
| E4 | No two `environment` entries of one list name the same Environment; `dependsOn` duplicates stay a separate rule | constraint 3; RFC Composition item 2 | TV | `structure.rs` | 1 |
| E5 | `parameters`, if given, has 1–50 entries; keys are `<Identifier>` | constraint 4; §3.2 `<EnvironmentParameterValues>` | TV | `structure.rs`, `constrained_strings.rs` `Identifier` | 1 |
| E6 | `required: false` is not permitted on an entry that names a `jobEnvironments` entry | constraint 8 | TV | `env_params.rs` | 1 |
| E7 | Name resolution: the Job Template's own `jobEnvironments` first, otherwise an external Environment | §3.2 item 2 | TV, SUB | `env_params.rs` (JT side), `external.rs` (SUB side) | 2, 3 |
| E8 | An entry does not delay scheduling, cause any Environment to be entered, or change entry order | §3.2 item 2 | JC | `instantiate.rs:181-187` drops it from `job::StepDependency` (decision 3); `step_dependency_graph.rs` unchanged | 3 |
| E9 | A parameter the entry omits takes its `default`; an entry with no `parameters` still declares the dependency | §3.2 item 4 | JC, SUB | `job/create_job/env_params.rs` | 3 |
| E10 | A value is a literal of the parameter's type, written as that type's Job Parameter `default` is (string, int or intstring, float or floatstring, and with `EXPR` bool or list) | §3.2 `<EnvironmentParameterValue>` bullet 1 | TV (JT entries), SUB | reuse the per-type literal decoders (decision 1) | 2, 3 |
| E10a | A value for a list-typed parameter has no string form: it must be a list literal or a single `{{ }}` expression | §3.2 `<EnvironmentParameterValue>` bullet 2, last sentence; RFC Validation table row "A format-string value or `default` for a list-typed parameter…" | TV (JT entries), SUB (external entries) | `env_params.rs`, `external.rs` | 2, 3 |
| E10b | The elements of a list literal are literals, not format strings (A14) | §3.2 bullet 1; §4.5 item 4 last sentence | TV (JT entries and every `default`), SUB | `env_parameters.rs`, `env_params.rs`, `external.rs` | 1–3 |
| E11 | A format-string value is resolved at job creation in scope `RawParam.*` (all), `Param.*` (not PATH/LIST[PATH]) of the supplying document's top-level parameters with merged values, and with `EXPR` `Job.Name`, `Step.Name`, and the Step's `let`; not `Task.*`, `Session.*`, `Env.*`. With `EXPR`, a single `{{ }}` expression evaluates with the parameter's type as target; any other format string resolves to a string converted as a submitted Job Parameter value of that type, without joining a relative path (as D10) | §3.2 bullet 2; §3 item 3; §3.6.2 and §7.3.1 rows | TV, JC | new entry-value scope in `format_strings.rs`; evaluation in `job/create_job/env_params.rs` against the full step scope (Job Environment entries) or against the entry's frozen table (external entries, decision 6) | 2, 3 |
| E12 | The supplying document's extensions decide a value's form; without `EXPR` an entry cannot supply a BOOL or LIST parameter | §3.2 paragraph after the bullets; constraint 6 last sentence | TV (syntax: bool or list literal in a non-`EXPR` document), SUB (by type) | `env_params.rs`, `external.rs` | 2, 3 |
| E13 | Keys must name declared parameters; an Environment that declares none accepts an entry with no `parameters` and rejects any key | constraint 5 | TV (JT entries), SUB | `env_params.rs`, `external.rs` | 2, 3 |
| E14 | Every value valid for its parameter: type and constraints | constraint 6 | TV (literal JT values), JC, SUB | `env_params.rs`, `job/create_job/env_params.rs` | 2, 3 |
| E15 | Each Step supplies every parameter without a `default` of every Environment its Sessions enter; every Session enters every `jobEnvironments` entry and every attached Environment | constraint 7 | TV (Job Environments), SUB (attached) | `env_params.rs`, `external.rs` | 2, 3 |
| E16 | Constraints 1–4 and 8 at TV; 5–7 at TV for Job Environment entries and at JC for every entry | §3.2 "Constraints 1–4 and 8 are checked…" | TV, JC, SUB | as above | 1–3 |
| E17 | For an external entry, TV checks only constraints 1–4, 8, and a literal's form; a validator given the Environment Templates can check 5–7 early | §3.2 last paragraph | TV, CLI | the form checks of A4: object, `null`, and nesting in `env_parameters.rs` / `structure.rs` (PR 1), the `EXPR`-gated bool and list forms in `env_params.rs` (PR 2); `openjd check --environment` (PR 5) calls the SUB checks that need no Job Parameter values | 1, 2, 5 |

### 1.4 Resolving external Environments and effective values (openjd-model)

| # | Statement | Spec | Stage | Module | PR |
|---|-----------|------|-------|--------|----|
| X1 | With zero or more attached Environment Templates, each entry naming no Job Environment resolves against them by `environment.name` | §3.2.1 opening | SUB | new `job/create_job/external.rs`, `apply_environment_templates` | 3 |
| X2 | Exactly one match: constraints 5 and 6 apply against it whatever `required` says; supplying any parameter to an Environment that declares none is rejected, naming the entry and parameter | §3.2.1 item 1 | SUB | `external.rs` | 3 |
| X3 | No match: `required: true`, or `required` absent, rejects, naming the entry and the missing Environment; `required: false` ignores the entry | §3.2.1 item 2; §3.2 item 3 | SUB | `external.rs` | 3 |
| X4 | Two or more matches: reject, naming the entry and the ambiguous name | §3.2.1 item 3 | SUB | `external.rs` | 3 |
| X5 | An attached Environment named like a Job Environment is unreachable from the Job Template; implementations should warn | §3.2.1 item 4 | SUB | `external.rs`, `AppliedEnvironmentTemplates::warnings` | 3 |
| X6 | An attached Environment named like a Step's `stepEnvironments` entry is unreachable from that Step only | §3.2.1 item 5 | SUB | `external.rs` (falls out of E3 plus X1; pinned by a test) | 3 |
| X7 | Entries resolve independently; a Step's Sessions enter every attached Environment it names, each with its own values | §3.2.1 paragraph after item 5 | SUB | `external.rs` | 3 |
| X8 | After resolution, constraint 7 over the combined Job for attached Environments | §3.2.1 last paragraph | SUB | `external.rs` | 3 |
| X9 | Effective value: the Step's supplied value if any, else the resolved `default`; fixed at job creation; part of the Job | §3.2.2 items 1–2 | JC (Job Environments), SUB (attached) | `job/create_job/env_params.rs`; `job::Step::environment_parameters` (decision 4) | 3 |
| X10 | Equal values: same type and same value as resolved, before path mapping; INT `1` equals `"1"`; PATH by unmapped string; lists element-wise in order | §3.2.2 | API | `job/mod.rs` equality (decision 8) | 3 |
| X11 | Step `let` and `Step.Name` are available to values (and hence make values per Step) | §3 item 3; §3.6.2; §7.3.1 `Step.Name` row; RFC "`Step.Name` in values" | TV, JC | entry-value scope (E11) | 2, 3 |
| X12 | Job creation adds each Step's effective values to the Known Values; PATH values are not joined and must be absolute | §7.4 Job creation row | JC, SUB | as X9, D13, D16 | 3 |
| X13 | A validator given the queue's Environment Templates should warn when a `required: false` entry resolves to no Environment | RFC "External Environments are named implicitly" item 3 | SUB, CLI | `external.rs` warning; printed by `openjd check` / `run` | 3, 5 |

### 1.5 `Env.Param.*` and `Env.RawParam.*` value references (openjd-model, openjd-expr types)

| # | Statement | Spec | Stage | Module | PR |
|---|-----------|------|-------|--------|----|
| V1 | `Env.Param.<name>` and `Env.RawParam.<name>` are available within the declaring Environment: its `variables`, its Script's `let`, actions (including RFC 0008 wrap hooks), and embedded files | §7.3.1 rows; §4 item `Env.*` scope; §3.6.2 `<EnvironmentScript>.let` row | TV | `build_session_scope_symtab` (`format_strings.rs:112`) adds them from the Environment's own definitions | 2 |
| V2 | Not available in any other Environment or in a Step (script, `let`, step envs of other names) | §4 `Env.*` scope; §7.3.1 | TV | absence from every other scope; tests pin the error message | 2 |
| V3 | A reference that names no parameter of the enclosing Environment is a TV error | RFC Value references paragraph | TV | `format_strings.rs` (unknown-symbol error, as for `Env.File.*`) | 2 |
| V4 | With `EXPR`, `Env.Param.<name>` has the `Param.<name>` type of its parameter type and `Env.RawParam.<name>` the `RawParam.<name>` type (`string` for PATH, `list[string]` for LIST[PATH]) | §4.5 last paragraph; ExprLang §1.2.2 "Environment Parameter Symbols" | TV | `format_strings.rs`, the same type map `build_param_symtab` uses for `Param.*` / `RawParam.*` (`format_strings.rs:40`) | 2 |
| V5 | `Env.Param.*` (and `Env.RawParam.*`, A15) is an Unknown Value at TV; known at task execution, with path mapping for PATH and LIST[PATH] | §7.4 Template validation and Task execution rows | TV, sessions | `format_strings.rs`; at job creation the carried-forward check table for each Environment is built by `build_env_check_symtab` (`job/create_job/instantiate.rs:359`, called at `create_job/mod.rs:244` for Job Environments and `instantiate.rs:139` for Step Environments), which seeds them `Unresolved` before `check_carried_forward_environment` (`format_strings.rs:1493`) reads the table | 2 |
| V6 | A wrapping Environment's hooks read its own parameters; an inner Environment's are not visible to them | RFC Value references paragraph | TV, sessions | TV: wrap hooks are validated against a clone of the session-scope table (`format_strings.rs:2419-2425`), so V1's change covers them; a test pins it. Sessions: S5 | 2, 4 |

### 1.6 Sessions runtime (openjd-sessions)

| # | Statement | Spec | Module | PR |
|---|-----------|------|--------|----|
| S1 | When a Session enters an Environment, `Env.Param.*` and `Env.RawParam.*` are bound from the values it enters with | §4.5; §7.4 Task execution; HJAR Environment Parameters ¶3 | `session.rs`: a new `enter_environment_with_parameters(env, resolved_symtab, params: Option<&EnvironmentParameterValues>, identifier, os_env_vars) -> Result<(String, String), SessionError>` that returns the identifier and the `onEnter` output; `enter_environment` (`session.rs:1133`) and `enter_environment_with_output` (`session.rs:1147`), which `enter_environment` already delegates to, both delegate to it with `None` | 4 |
| S2 | `Env.Param.<name>` for PATH and LIST[PATH] has path mapping applied on the Worker Host; `Env.RawParam.<name>` is unmapped | §4.5; §3.2.2; HJAR ¶3 | `build_symbol_table` (`session.rs:2461`), the same mapping loop used for `Param.*`. Constraints are not re-checked after mapping (§4.5 PATH paragraph: "path mapping on the Worker Host does not re-check them") | 4 |
| S3 | The Session keeps the values until it exits the Environment: `onEnter`, every Task, and `onExit` see the same values | §3.2.2 ¶4; HJAR ¶3; RFC "Values in `onExit`" | `session.rs` stores the values with the entered Environment; `exit_environment` (`session.rs:1479`) uses the stored values, not a caller argument | 4 |
| S4 | Environment `variables` see `Env.Param.*` | §7.3.1 row | `session.rs` environment variable evaluation (`specs/sessions/session.md` "Environment Variable Evaluation") | 4 |
| S5 | Wrap hooks of a wrapping Environment see the wrapping Environment's stored values | RFC Value references; RFC 0008 | `session.rs:2975` layers the stored values onto the wrap scope | 4 |
| S6 | `required: false` with an absent external Environment: the Session simply does not enter it | §3.2 item 3; HJAR ¶2 | no sessions code: SUB drops the entry, and the caller never enters an unattached Environment; a test enters a Job Environment with no values and checks behavior is unchanged | 4 |
| S7 | A caller can read the values an entered Environment was entered with, to enforce the sharing rule | §3.2.2 ¶4 (scheduling rule) | `Session::environment_parameters(identifier) -> Option<&EnvironmentParameterValues>` | 4 |

### 1.7 CLI (openjd-cli)

| # | Statement | Spec | Module | PR |
|---|-----------|------|--------|----|
| C1 | `openjd run` treats each `--environment` template as an attached Environment Template and runs `apply_environment_templates` after `create_job`, printing its warnings to stderr | §3.2.1; RFC "openjd run accepts them" | `run/execution.rs` `prepare_run` (`:70`) | 5 |
| C2 | A Session runs Tasks of a Step only when the Step's effective values equal the values the Session entered each Environment with; Steps with different values never share a Session | §3.2.2 ¶4; HJAR §Running a Session and ¶3–4 | `run/execution.rs` `run_workload` (`:341`): walk the selected Steps in their existing order and start a new Session whenever the next Step's values differ from the current Session's (decision 8). **Superseded:** `openjd run` now runs every selected Step in one Session and, between Steps, exits and re-enters Job Environments whose set or values differ (`specs/cli/run.md`, "One Session, re-entering Environments between Steps"). | 5 |
| C3 | Each Session enters every attached Environment and every Job Environment with that group's values, in the existing order (superseded: `openjd run` now enters each Step's Environments in one Session, exiting and re-entering those whose set or values differ; `specs/cli/run.md`) | §3.2 item 2; `specs/cli/run.md` "Environment Lifecycle" | `run/execution.rs`, `run/mod.rs` `RunContext::enter_environment` (`:90`) | 5 |
| C4 | Job creation and submission errors (missing external Environment, unknown key, bad value, ambiguous name) fail the run before any Session starts, with the entry's path in the message | §3.2.1 items 1–3; RFC Validation table | `run/execution.rs` error mapping, as for `create_job` today (`:132`) | 5 |
| C5 | `openjd check` accepts `--environment` (repeatable) for a Job Template and runs the external checks that need no Job Parameter values (resolution, keys, literal values, constraint 7), printing warnings | §3.2 last paragraph; RFC Tooling-parseable rationale | `check.rs` | 5 |
| C6 | `openjd summary` shows each Step's effective Environment parameter values | RFC "Values fixed per Step" (tooling can display them) | `summary.rs` | 5 |

## 2. PR and commit order

Each increment is one PR-sized branch, made with the `implement-change` skill (implement commit, independent audit
rounds by fresh `deadline-openjd` sub-agents, fix commits, clean round). Each is testable on its own and safe to
merge on its own:

* A document that does not list `ENV_PARAMS` behaves as on `main` after every increment (G6).
* Until PR 5 adds `ENV_PARAMS` to `ModelExtension::ALL`, no default allowlist contains it, so `openjd check`,
  `openjd run`, and openjd-for-js reject an `ENV_PARAMS` template by default (decision 9). A caller that passes
  `ENV_PARAMS` in an explicit allowlist, to the Rust APIs or to openjd-for-js's `supportedExtensions`
  (`crates/openjd-for-js/src/model.rs:382-386`), opts into the partial support of the increments merged so far.

Every increment runs these gates:

* `cargo +nightly fmt --all -- --check`
* `cargo clippy --all-features --all-targets --workspace -- -D warnings`
* `cargo test --workspace`
* `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` (every increment adds public items)
* `scripts/check_copyright_headers.sh` (every increment adds `.rs` files)
* `cargo +1.97.0 check --workspace` (MSRV, CI job `msrv`), since every increment adds Rust code
* `cargo deny check`, only in an increment that changes `Cargo.toml` dependencies or `Cargo.lock` (none is planned)
* the full conformance suite `2023-09/*` against `target/release/openjd` (see `AGENTS.md`). Every increment changes
  template validation or execution code, so the suite shows that G6 holds. There are no `ENV_PARAMS` conformance
  tests yet; see [Follow-ups](#follow-ups-outside-this-epic).

The "Extra gates" column lists the rest. Each new integration test file is registered with a `#[path]` module line
in its crate's `tests/integration.rs`, or it is never compiled; the Files column lists `integration.rs` for that
reason. The audit tier for every increment is "public API change, cross-crate change, or new feature": 2–4 auditors
per round with disjoint angles.

| PR | Title (conventional commit) | Spec sections | Files | Tests | Extra gates |
|----|-----------------------------|---------------|-------|-------|-------------|
| 1 | `feat(model): ENV_PARAMS schema types, extension gating, and structural checks` | §1.1 item 3; §1.2 items 2–3; §4 item 3 constraints 1–4; §4.5 items 1–5 (definition side) and point 1 (literal `default`); §3.2 schema, items 1–4, constraints 1–4 and 8. Inventory G1 (variant only), G2–G5, D1–D4, D5 (definition), D6, D12, E1–E6, E10b (`default` side), E16 (constraints 1–4, 8), E17 (object, `null`, nesting forms) | `crates/openjd-model/src/types.rs` (variant, not in `ALL`); `template/environment.rs`; `template/environment_template.rs`; new `template/env_parameters.rs`; `template/step.rs`; `template/mod.rs` (re-exports); `template/validate_v2023_09/{mod.rs,structure.rs}`; new `template/validate_v2023_09/env_params.rs`; every reader of the template `StepDependency::depends_on` (`job/create_job/instantiate.rs:184`, `structure.rs:225-238`, `structure.rs:1082`, `crates/openjd-model/tests/integration/test_template_public_api.rs:150`); `template/parse.rs:203-260` (type-name case check over Environment `parameterDefinitions`); `crates/openjd-model/tests/integration.rs`; specs: `specs/model/{template-types.md,validation.md,parsing.md,public-api.md,README.md}` | new `crates/openjd-model/tests/integration/test_env_params_schema.rs`: decode and gate (with and without `ENV_PARAMS`, `EXPR`), every structural constraint with full path and message assertions per AGENTS.md; a lowercase Environment parameter type name rejected without `EXPR`; an object, `null`, or over-nested value rejected at its path; error-message cases in `test_error_messages.rs`; the RFC's Basic Examples and the three spec samples as fixtures (valid with the extensions, rejected without) | none |
| 2 | `feat(model): Env.Param scopes and template-validation checks for Job Environment entries` | §4 `Env.*` scope; §7.3.1 `Env.Param`/`Env.RawParam`/`Step.Name` rows; §3.6.2 rows; §7.4 Template validation row; ExprLang §1.2.2; §4.5 item 4 scopes; §3.2 value scopes, list-value form, and constraints 5–7 for Job Environment entries; RFC Value references. Inventory D7, D11, D15 (TV), D17, E7 (JT side), E10, E10a, E11–E15 (TV side), E10b (entry side), E16 (5–7 at TV), E17 (`EXPR`-gated forms), X11 (scope), V1–V6 | `template/validate_v2023_09/format_strings.rs` (new symbols in `build_session_scope_symtab`; new default scope and entry-value scope); `template/validate_v2023_09/env_params.rs`; `job/create_job/instantiate.rs` (`build_env_check_symtab`, `:359`, seeding `Env.Param.*` and `Env.RawParam.*` unresolved in the carried-forward check tables); `crates/openjd-model/tests/integration.rs`; specs: `specs/model/validation.md`, `specs/model/job-creation.md` (carried-forward scope) | new `test_env_params_scope.rs`: `Env.Param` in each permitted field, rejected in Steps and other Environments, unknown name, EXPR typing (INT arithmetic, LIST[PATH] vs `list[string]`), default scope (PATH `Param.*` rejected, `RawParam.*` accepted, another document's parameters rejected), entry-value scope (`Task.*`, `Session.*`, `Env.*` rejected; `Step.Name` and `let` accepted with `EXPR`), a format string for a list parameter that is not one expression, constraints 5–7 against Job Environments, all with full messages; a `create_job` case showing a Job Environment that uses `Env.Param.*` passes the carried-forward checks; with `EXPR`, `default: "{{ [1] }}"` on an INT parameter and a mistyped single-expression entry value rejected at TV (D17); a Step `let` reading PATH `Param.*` still rejected when it feeds an entry (A2) | none |
| 3 | `feat(model): effective Environment parameter values at job creation and submission` | §3.2 items 2–4, constraints 5–7 at job creation; §3.2.1 items 1–5 and closing paragraphs; §3.2.2; §1.2.1 last paragraph; §4.5 item 4 conversion, points 1–2, PATH paragraph; §7.4 Job creation row; RFC Validation table rows marked Job creation. Inventory G4 (per-document extensions at SUB), D5 (values), D6 (resolution), D8–D10, D13–D16, E7–E9, E10, E10a, E10b (external entries), E11–E15 (JC side), E16 (5–7 at JC), X1–X13 | `job/mod.rs` (`Step::environment_parameters`, `EnvironmentParameterValues`, `Document`, `UnresolvedEnvironmentEntry`, equality, the corrected `resolved_symtab` doc comment); new `job/create_job/env_params.rs`; new `job/create_job/external.rs` (`apply_environment_templates`, `AttachedEnvironmentTemplate`, `AppliedEnvironmentTemplates`); `job/create_job/{mod.rs,instantiate.rs}`; `types.rs` (`ValidationContext::submission_path_format` and `allow_uri_path_values`); `lib.rs` re-exports; `crates/openjd-model/tests/integration.rs`; specs: `specs/model/{job-creation.md,job-types.md,step-dependencies.md,parameters.md,public-api.md}` (`parameters.md` documents `ValidationContext`); `crates/openjd-for-js/src/model.rs` `create_job_with_map` (`:638-666`: pass the caller's `path_format` and `allow_uri_path_values` into `ValidationContext`, and call `apply_environment_templates` with no attachments so a returned Job holds no unresolved entries; an Environment Templates input for JS is a follow-up), its serialization tests, and `specs/openjd-for-js/spec.md` (`Job`/`Step` shape, `:192-211`) for the new `Step` fields | new `test_env_params_job_creation.rs` (Job Environments: defaults from merged Job Parameters, INT `1` vs `"1"` equal, FLOAT `1.0` vs `1.00` equal under decision 8 but unequal under derived `PartialEq`, `default` checked and evaluated only where used (A19), relative PATH rejected, URI accepted with `EXPR`, `let`-derived values, environment entries absent from the dependency graph); new `test_env_params_external.rs` (§3.2.1 items 1–5 each with full messages, `required: false` ignored, an absent `required` rejected like `required: true`, pre-ENV_PARAMS Environment rejecting a supplied key, a Job Template without `ENV_PARAMS` run with an `ENV_PARAMS` Environment Template taking every `default` (G4), constraint 7 over the combined Job, warnings for X5 and X13, per-document `EXPR` rule); serde round-trip and `Hash`/`Eq` cases in `test_job_step_serde.rs` and `test_job_eq_hash.rs`; `test_step_dependency_graph.rs:21-50`, which builds `job::Step` literals, updated for the new fields; a for-js test that a Job without Environment parameters serializes as before | `openjd-for-js` wasm build and vitest suite (`job::Step` shape) |
| 4 | `feat(sessions): enter Environments with Environment parameter values` | §3.2.2 ¶4; §4.5 `Env.Param` / `Env.RawParam` paragraph and PATH paragraph (no re-check after mapping); §7.4 Task execution row; HJAR Environment Parameters ¶3; RFC "Values in `onExit`"; RFC Value references (wrap hooks). Inventory S1–S7, V6 (runtime) | `crates/openjd-sessions/src/session.rs` (`enter_environment_with_parameters`, stored values, exit and wrap-hook scope, `environment_parameters` accessor, variables); `src/runner/env_script.rs` if the runner builds the scope; `crates/openjd-sessions/tests/integration.rs`; specs: `specs/sessions/{session.md,public-api.md,architecture.md,README.md}` (`README.md:58-64` lists the implemented extensions) | new `crates/openjd-sessions/tests/integration/test_env_params_session.rs`: `onEnter`, a Task's wrap hook, `variables`, and `onExit` all see the same values; PATH and LIST[PATH] mapped in `Env.Param`, raw in `Env.RawParam`; a mapped path that would violate `maxLength` is not re-checked; two Environments with the same parameter name stay separate; entering with `None` matches today's behavior | none; the cross-user tests do not apply (no cross-user code changes), which the audit inputs state |
| 5 | `feat(cli): openjd run, check, and summary support for ENV_PARAMS` | §3.2 last paragraph; §3.2.1; §3.2.2 ¶4; HJAR §Running a Session and Environment Parameters ¶3–4; RFC Validation table. Inventory G1 (`ALL`), C1–C6, E17, X13 (printing) | `crates/openjd-model/src/types.rs` (`ALL`); `crates/openjd-model/tests/integration/test_model_profile.rs:186`; `crates/openjd-cli/src/run/{execution.rs,mod.rs,result.rs}` (including the `ValidationContext` path policy for `create_job`, decision 7, deferred from PR 3); `src/check.rs`; `src/summary.rs` (the same path policy); `src/help.rs` (`:67-72` switched to `ALL`, and help for the new `check` flag); new templates under `crates/openjd-cli/tests/templates/` (copies of `conda-env-params.yaml`, `docker-env-params.yaml`, and a reduced `video-sfm-composite.yaml` whose actions only `echo`); `crates/openjd-for-js/tests/test_supported_extensions.rs`, `crates/openjd-for-js/js-tests/smoke.test.js`; docs listing the default extensions: `crates/openjd-model/README.md:26-27`, `specs/cli/architecture.md:150`, `specs/cli/check.md:58`, `specs/cli/parameter-parsing.md:181`; specs: `specs/cli/{run.md,check.md,summary.md,README.md}`, `specs/model/public-api.md` (`ALL`); `README.md` if it documents `check` flags | `crates/openjd-cli/tests/cli_tests.rs`: two Steps with different `Packages` run in two Sessions, each `onEnter`/`onExit` printing its own value; two Steps with equal values share one Session; `required: false` with no `--environment` runs unchanged; a Job Template without `ENV_PARAMS` with `conda-env-params.yaml` runs with the defaults (G4); a missing `required: true` Environment and an unknown key fail before any Session with the entry's path; `check --environment` reports the same errors and the warnings; `summary` shows effective values; `run <ENV_PARAMS template> --help` succeeds | `openjd-for-js` wasm build and vitest suite (its default extension list changes) |

*Status:* PRs 1 and 2 were implemented together, as one task branch, since PR 2's checks of Job Environment
entries share PR 1's walk of the dependency list (`template/validate_v2023_09/env_params.rs`). PR 3 is
implemented as planned, with these differences: the equality function is the method
`job::Step::environment_values_equal`, which compares the transport JSON (a value whose type contains `float`
numerically) rather than decoding both tables, with the same results, with `EnvironmentParameterValues::values_equal` for one Environment's
values; `AppliedEnvironmentTemplates` also has `combined_environment_documents` (as on `service-extension`); the
serde round-trip and `Hash`/`Eq` cases are in `test_env_params_job_creation.rs` and `test_env_params_external.rs`
rather than `test_job_step_serde.rs` and `test_job_eq_hash.rs`; the value forms at job creation follow the Job
Template's own extensions, not the context's; `openjd run` and `openjd summary` still build their context with
`default_validation_context()`, so passing `submission_path_format` and `allow_uri_path_values: true` to them (decision 7)
moves to PR 5, which first lets the CLI accept `ENV_PARAMS`; Environment parameter format strings evaluate in the
submission path format (decision 7 named it only for the absoluteness check), with the Step's `let` bindings that entry values
read evaluated again in that format for them when it is not POSIX (A22); `UnresolvedEnvironmentEntry` records the `submission_path_format` it was frozen for, and
`apply_environment_templates` rejects an entry frozen for another; `UnresolvedEnvironmentEntry` also records the entry's
`dependency_index` for error paths; `AppliedEnvironmentTemplates::warnings` holds `SubmissionWarning { path,
message }` values; `EnvironmentTemplate` gains `profile()` and `default_validation_context()` (as on
`service-extension`), so each attachment is evaluated under its own profile; and `apply_environment_templates` does
not re-run the carried-forward checks on the attached Environments' own format strings. PR 4 is implemented as
planned (S1–S7, V6), with these details: S2's symbols are built by a new private
`environment_parameter_symbols` and layered onto the declaring Environment's own scopes (and its wrap hooks'), not
added in `build_symbol_table`, which every scope shares; `Session::environment_parameters(identifier)` returns
`Option<&job::EnvironmentParameterValues>`; a new `SessionError::InvalidEnvironmentParameters` rejects values for
another Environment or a table that is not one entry per bare parameter name, before any state is recorded; path
mapping of `Env.Param.*` uses the Session's rules at each use, as `Param.*` does; and no `src/runner/` change was
needed, since the Session builds every scope the runners receive. PR 5 is implemented as planned (G1, C1–C6, E17,
X13), with these details: `openjd check --environment` calls a new model function `check_environment_templates`,
which shares the attachment index, entry resolution, and missing-parameter sorting with
`apply_environment_templates` (refactored out of it, with one behavior change: an attachment named like a Job
Environment is warned about only when the Job Template lists `ENV_PARAMS`, so a submission without the extension
gets no new warning) so the two report the same messages,
and checks literal entry values (including PATH absoluteness) but not format strings; the CLI's shared
`common::submission_context` gives `create_job` and the submission stage the host path format and
`allow_uri_path_values: true` in `run`, `summary`, and `check`; `openjd run` prints each Environment's values after
its `Entering Environment` banner and prints `Starting a new Session for Step '<name>' ...` before each later
Session (superseded: it now keeps one Session and exits and re-enters Environments between Steps, with no such
message); `openjd summary` lists the attached Environments' values per Step but keeps its environment listings and
counts to the Job Template's own; and the CLI fixtures adapted from the RFC samples are in
`crates/openjd-cli/tests/templates/env_params/`, with Python stubs in place of the application commands.

Order: PR 1 → PR 2 → PR 3 → PR 4 → PR 5, each merged into `env-params-prototype` before the next starts. PR 4
needs PR 3's `job::EnvironmentParameterValues`, and PR 5 needs PRs 3 and 4.

## 3. RFC ambiguities and the prototype's interpretation

Each item is a place where the spec text allows more than one reading, or does not say what an implementation
must do. The interpretation is what the prototype will implement; the last column says whether the RFC should be
clarified.

| # | Ambiguity | Interpretation | Clarify RFC? |
|---|-----------|----------------|--------------|
| A1 | **FLOAT equality versus rendering.** §3.2.2 makes `1.0` and `1.00` equal (same type, same value), but openjd-expr keeps a FLOAT's original text (`ExprValue` float passthrough) and `{{Env.Param.X}}` renders that text. A Session shared by two Steps that wrote `1.0` and `1.00` would render whichever Step's text it entered with. | Compare FLOATs by numeric value, as the text says. The Session renders the text of the values it was entered with (the first Step of the group in `openjd run`). A test pins this. | Yes: either define FLOAT equality on the canonical text, or say the rendering of a shared Session's value is that of any one of the equal values. |
| A2 | **`let` bindings and PATH `Param.*` (resolved while planning; not an ambiguity).** §3.2 allows the Step's `let` names in values but forbids PATH `Param.*`. A Step-level `let` is evaluated in template scope (`job/create_job/instantiate.rs:62-78`; `specs/model/job-creation.md:307-309`), where PATH `Param.*` does not exist, so template validation already rejects `let: ["x = Param.P"]` for a PATH `P` (`steps[0] -> let[0]: … Undefined variable: 'Param.P'. Did you mean: RawParam.P`). A value can therefore never depend on PATH `Param.*` through `let`. | No new check. A test in PR 2 pins that the existing error still fires for a Step whose `let` feeds an `environment:` entry. | No. |
| A3 | **Submission path format in job creation.** §4.5 requires absoluteness in the Job's submission path format, but job creation has no path-format input today. | `ValidationContext::submission_path_format`, defaulting to `PathFormat::host()` (decision 7). `openjd run` uses the host format, matching `preprocess_job_parameters`. | No; an implementation detail. |
| A4 | **What "type of a literal value where its form fixes one" checks at TV for an external entry.** The entry's parameter type is unknown at TV, and the spec's one example ("a list is not a STRING") does not say which forms are rejected outright. | TV rejects every form that no parameter type accepts in the supplying document: an object or `null` value, or an object or `null` element of a list literal, in any document; a bool, a list, or a list element that is itself a list, in a document without `EXPR` (constraint 6, last sentence); and a list nested more than one level deep, or a nested list whose inner elements are neither integers nor integer strings, since `LIST[LIST[INT]]` is the only nested type and its `default` decoder accepts both (`Vec<Vec<FlexInt>>`, `template/expr_parameters.rs:637`). Everything else waits for SUB. | Yes, a sentence giving these as the examples. |
| A5 | **A literal of another JSON type.** `Packages: 7` supplies an integer to a STRING parameter. §3.2 says a literal is written as that type's Job Parameter `default` is written. | Use the Job Parameter literal decoders unchanged: if the Job Parameter `default` of that type would reject it, the value is rejected (constraint 6). A STRING `default` is `Option<String>` (`template/parameters.rs:634`), so `Packages: 7` is rejected and `Packages: "7"` is accepted. INT and FLOAT accept a number or a numeric string (`FlexInt`, `FlexFloat`). | No. |
| A6 | **Sessions in `openjd run`.** HJAR says a Session enters each Environment once. `openjd run` runs every selected Step in one Session today. When selected Steps have different values, the CLI cannot re-enter Job Environments in the same Session. | One Session per run of consecutive selected Steps with equal values, in the order Steps already run (`--run-dependencies` order). The CLI never reorders Steps to merge groups. Each Session has its own working directory; `--preserve` keeps and prints each. Without `ENV_PARAMS` values there is one group, so behavior is unchanged. **Superseded:** `openjd run` now runs every selected Step in one Session and, between Steps, exits and re-enters Job Environments whose set or values differ (`specs/cli/run.md`, "One Session, re-entering Environments between Steps"). | No; CLI behavior is outside the spec. |
| A7 | **Where the sharing rule is enforced.** §3.2.2 states it as a scheduling rule; the RFC's Validation section says it is not a validation rule. | Sessions do not reject a Task (they do not know its Step); they expose the entered values (S7). The CLI groups Steps (C2). | No. |
| A8 | **Which Environments "a Step's Sessions enter" for constraint 7 and effective values.** Without `SERVICE`, every Session enters every Job Environment and every attached Environment, plus the Step's own Step Environments, which cannot declare parameters. | Effective values are computed for every Job Environment and attached Environment that declares parameters, for every Step, whether or not the Step names it. A Step that names no such Environment still gets its `default`s. | No. |
| A9 | **`openjd check` and the Environment Templates.** The RFC says a validator "given the Environment Templates … as `openjd run` accepts them" can check constraints 5–7 early, but no CLI option for `check` exists, and constraint 6 on non-literal values needs Job Parameter values. | Add `--environment` to `check` for Job Templates. It runs resolution, key checks, literal value checks, constraint 7, and the X5/X13 warnings. Format-string values and `default`s are left to `run`, which has Job Parameter values. | No; tooling choice. |
| A10 | **An attached Environment shadowed by a Job Environment.** §3.2.1 item 4 says it is unreachable and implementations should warn. The combined Job then has two Environments of one name. | Keep both (the Session enters both, as today), resolve the entry to the Job Environment, and warn. Effective values are keyed by `(document, environment)`, not name alone (decision 4). | No. |
| A11 | **Two attached Environment Templates with the same `environment.name` that no entry names.** §3.2.1 item 3 rejects only an entry that names an ambiguous name. | No error unless an entry names it; both are entered with their `default`s as today. | No. |
| A12 | **URI path values: whose `EXPR`, and whose policy.** §4.5 allows a PATH value to be "with the `EXPR` extension, a URI" without naming the document, and for Job Parameters URI acceptance is also a caller policy (`PathParameterOptions::allow_uri_path_values`, `create_job/parameters.rs:818-821`, `false` by default, `true` in `openjd run`). | The document that contains the value decides, as §3.2 says for every other form: the Environment's own document for a `default`, the supplying document for an entry value. The caller policy applies as for Job Parameters, through `ValidationContext::allow_uri_path_values` (decision 7). | Yes: name the document. |
| A13 | **Order of error reporting at submission.** The spec does not order the §3.2.1 errors. | Collect every entry's errors into one `ModelError::ModelValidation`, in Step order, then entry order, then parameter-name order (template maps arrive sorted by key, decision 2). *As implemented (PR 3):* `apply_environment_templates` reports in three phases: the attachments' literal `default`s; then every Step's entry resolution (missing, ambiguous), in Step and entry order; then every Step's values, in Step order. So a later Step's resolution error precedes an earlier Step's value error. Within one Environment's values, undeclared keys come first, in key order, then the Environment's definitions in written order, since defaults and supplied values are checked together per definition. This is a new policy for these checks, modeled on how `create_job` already collects its carried-forward re-check failures (`create_job/mod.rs:93-99`); `create_job` still aborts alone on a failure that makes a Step uninstantiable. | No. |
| A14 | **Are the elements of a list literal format strings?** §3.2 annotates every `<EnvironmentParameterValue>` `# @fmtstring` and allows "a list literal or a single `{{ }}` expression" for a list parameter; §4.5 calls a `default` with no `{{ }}` a literal "written as for a `<JobParameterDefinition>`", whose list `default` elements are not format strings. For a `default` the spec decides: §4.5 point 1 defines a literal `default` as one with no `{{ }}`, and item 4 requires a non-literal list `default` to be a single expression, so `default: ["{{RawParam.X}}"]` is already an error. For an entry value it does not say whether `Mounts: ["{{RawParam.WorkDir}}"]` is allowed. The spec samples use only single expressions and `[]` (`docker-env-params.yaml:71`, `video-sfm-composite.yaml:122` in the spec repo). | List literal elements are literals, as for a Job Parameter list `default`. An element containing `{{` is an error (TV for `default`s and Job Environment entries, SUB for external entries) whose message says to write the whole value as one `{{ }}` expression, for example `{{ [RawParam.WorkDir] }}`. Rejecting rather than keeping the text verbatim leaves room to allow format-string elements later without changing what an accepted template means. | Yes, for entry values only: say which. |
| A15 | **`Env.RawParam.*` in §7.4.** The §7.4 rows (wiki and RFC) list `Env.Param.*` in the Unknown Values at template validation and the Known Values at task execution, but not `Env.RawParam.*`, which §4.5 and §7.3.1 define alongside it. | `Env.RawParam.*` is staged exactly as `Env.Param.*`: unknown at TV, known at task execution, never path-mapped. | Yes: add `Env.RawParam.*` to both §7.4 cells. |
| A16 | **An empty PATH value.** §4.5 requires every PATH value to be absolute, and `""` is not. Job Parameters pass an empty PATH through unchanged (`PathResolver::resolve`, `create_job/parameters.rs:1059-1061`), so `default: "{{RawParam.Opt}}"` over an optional, empty PATH Job Parameter resolves to `""`. | Follow §4.5: an empty PATH value, or an empty LIST[PATH] element, is an error wherever it becomes an effective value. A `default` that resolves to `""` is still not an error when every Step that enters the Environment supplies its own value (§4.5 point 2). An Environment Template that wants an optional path declares a STRING, as §4.5 recommends for host-local paths. | Yes: say whether `""` is allowed. |
| A17 | **Type-checking a non-literal `default` at TV.** §7.4's Template validation row type-checks expressions "using declared parameter types" with `EXPR`, but §4.5 point 2 says a non-literal `default` "is checked only where it becomes an effective value", and §3.2 checks Job Environment entry values at TV "against literal values only". For `default: "{{ [1] }}"` on an INT parameter that every Step supplies, the first says error and the second says none. | A static type that can never convert to the parameter's type (a list for an INT, a string expression for a LIST) is a TV error, as a mistyped literal `default` is under point 1. Value checks (constraints, a string that does not parse as an INT, absoluteness) stay where §4.5 point 2 and constraint 6 put them. Rationale: such an expression is wrong for every Job, so reporting it at TV loses nothing and matches the other `EXPR`-typed fields. *As implemented (PRs 1–2):* static evaluation also folds constant sub-expressions, as for every other `EXPR`-typed field, so a constant whose value cannot convert (`{{ 'abc' }}` for an INT) is a TV error too; a value read from `Param.*` is left to job creation. | Yes: say whether point 2 covers static type errors. |
| A18 | **What counts as a literal string.** §4.5 point 1 calls a `default` that "contains no `{{ }}`" a literal. A string such as `"a{{b"` contains `{{` but no `{{ }}` pair. | Classify a string as a format string when it contains `{{`, as every other `@fmtstring` field does, so `"a{{b"` is a format-string error (`FormatString::new` reports "Braces mismatch"), not a STRING literal. | No; the `@fmtstring` annotation decides it. |
| A19 | **Evaluation errors in an unused non-literal `default`.** §4.5 point 2 checks a non-literal `default` "only where it becomes an effective value" but does not say whether a runtime evaluation error (an `EXPR` index out of range, a failing function call) rejects the Job when every Step supplies the parameter. | Evaluate a non-literal `default` only for the Steps that take it, and report an evaluation error there, at the `default`'s path with the Step named. An unused `default` is never evaluated, which follows the RFC rationale "When a `default` is checked" (a job-wide fallback that this Job never uses must not reject it). | Yes: one sentence in point 2. |
| A20 | **`Env.Param.*` in an Environment action's `timeout` and `notifyPeriodInSeconds`.** §7.3.1 makes `Env.Param.*` available in the declaring Environment's "Actions", but those two fields are plain `@fmtstring`, resolved at job creation, before any Session enters the Environment, and §7.4 lists `Env.Param.*` as unknown until task execution. | *As implemented (PRs 1–2):* not available in them; a reference is an "Undefined variable" error, as `Session.*` and `Env.File.*` are. | Yes: say whether "Actions" includes the job-creation fields. |
| A21 | **`Job.Name` in an Environment parameter `default`.** §4.5 item 4 limits a `default` to the same document's `RawParam.*` and non-PATH `Param.*` "and no other scope", but the §7.3.1 `Job.Name` row (unchanged by the RFC) makes `Job.Name` available "in every Format String in the Job Template, except the `name` field". | *As implemented (PRs 1–2):* §4.5 governs: `Job.Name` in a `default` is an "Undefined variable" error, for Job and Environment Templates alike (an Environment Template's `default` cannot know the Job's name before it is attached). Entry values do get `Job.Name` with `EXPR` (§3.2). | Yes: add the exception to the §7.3.1 `Job.Name` row. |
| A22 | **Path format of PATH values, and of a Step's `let` bindings.** §4.5 says PATH values are "in the Job's submission path format" and §3.2.2 compares them "by their unmapped strings", but a WINDOWS path has two separators, so `C:/data\x` and `C:\data\x` name one path with two strings. A single `{{ }}` expression evaluates to a `path`, which WINDOWS rules normalize; a literal or a multi-segment format string is a string. Separately, job creation evaluates a Step's `let` bindings once, in POSIX, for every field of the Step. | *As implemented (PR 3):* Environment parameter format strings evaluate in the submission path format, and the Step `let` bindings that entry values read are evaluated again in that format for them (other uses keep their POSIX values). A PATH value is stored as resolved, with no separator rewriting, and its constraints and §3.2.2 equality see that stored string, so every effective value satisfies its definition; in a WINDOWS submission, `C:/data\x` written as a literal and as `{{ Param.Dir }}` are unequal values. In POSIX only a leading `/` is absolute. | Yes: say whether equality, and `allowedValues`, compare paths after separator normalization. |

## Composition with other extensions

* **`SERVICE`.** Not implemented on this branch (it branches from `main`). The plan keeps the `StepDependency` and
  `apply_environment_templates` shapes compatible with `service-extension` (decisions 2 and 5), and RFC "Composition
  with SERVICE" items 1–9 become a follow-up once both are on one branch.
* **`WRAP_ACTIONS`.** In scope: a wrapping Environment's hooks see its own `Env.Param.*` (V6, S5). No
  `WrappedEnv.Param.*` (RFC Future Work).
* **`EXPR`.** Gates BOOL and LIST types, single-expression target-type evaluation, URI paths, and `Step.Name` /
  `let` in values (G3, D10, D16, E11, E12).

## Follow-ups outside this epic

* `ENV_PARAMS` conformance tests in `openjd-specifications/conformance-tests/2023-09/ENV_PARAMS/` (template
  validation and job execution), which that repository owns and these tasks may not edit.
* Unifying `apply_environment_templates` with the `service-extension` function of the same name, and the RFC's
  Composition with SERVICE items.
* Python parity (`openjd-model-for-python`, `openjd-sessions-for-python`): no Python implementation exists yet, so Q7
  has nothing to compare; record the divergence in the specs as "not yet implemented in Python".
* RFC clarifications A1, A4, A12, A14 (entry values only), A15, A16, A17, A19, A20, A21, and A22 for the RFC author.

## Pre-existing drift found while planning (for the user)

`WRAP_ACTIONS` is in `ModelExtension::ALL` but missing from the hardcoded default list in
`crates/openjd-cli/src/help.rs:67-72`, so `openjd run <WRAP_ACTIONS template> --help` fails with "Unsupported
extension names: WRAP_ACTIONS" while `openjd check` accepts the template. The same four-name list appears in
`crates/openjd-model/README.md:26-27`, `specs/cli/architecture.md:150`, `specs/cli/check.md:58`, and
`specs/cli/parameter-parsing.md:181`. `specs/sessions/README.md:58-64` omitted `WRAP_ACTIONS` from its list of implemented extensions as well (PR 4 added it, with `ENV_PARAMS`). PR 5 must
touch all of these lists (PR 4 the sessions one) to add `ENV_PARAMS`, and switching `help.rs` to
`ALL` there fixes `WRAP_ACTIONS` as a side effect. If the user prefers that fix on `main` first, it is a separate,
small change.

`specs/model/parameters.md:167-175` shows `ValidationContext` with fields `revision` and `extensions`; the real
fields are `profile` and `caller_limits` (`crates/openjd-model/src/types.rs:613-616`). PR 3 rewrites that section
when it adds `submission_path_format` and `allow_uri_path_values`, which corrects it.
