import { describe, it, expect, beforeAll } from "vitest";
import { getModule } from "./helpers.js";

let mod;

beforeAll(async () => {
  mod = await getModule();
});

// ── Template Decode & Validate ─────────────────────────────────────

describe("decodeJobTemplate", () => {
  const VALID_TEMPLATE = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "TestJob",
    steps: [
      {
        name: "Render",
        script: {
          actions: { onRun: { command: "echo", args: ["hello"] } },
        },
      },
    ],
  });

  it("decodes a valid job template", () => {
    const t = mod.decodeJobTemplate(VALID_TEMPLATE);
    expect(t.name).toBe("TestJob");
    expect(t.specificationVersion).toBe("jobtemplate-2023-09");
    expect(t.stepCount).toBe(1);
    t.free();
  });

  it("throws on invalid template", () => {
    expect(() => mod.decodeJobTemplate("{}")).toThrow();
  });

  it("throws on invalid YAML", () => {
    expect(() => mod.decodeJobTemplate("{{{{")).toThrow();
  });
});

describe("decodeEnvironmentTemplate", () => {
  const VALID_ENV = JSON.stringify({
    specificationVersion: "environment-2023-09",
    environment: {
      name: "TestEnv",
      variables: { FOO: "bar" },
    },
  });

  it("decodes a valid environment template", () => {
    const t = mod.decodeEnvironmentTemplate(VALID_ENV);
    expect(t.specificationVersion).toBe("environment-2023-09");
    t.free();
  });
});

describe("DocumentType", () => {
  it("is exported as an enum with Yaml and Json variants", () => {
    expect(mod.DocumentType.Yaml).toBeDefined();
    expect(mod.DocumentType.Json).toBeDefined();
    expect(mod.DocumentType.Yaml).not.toBe(mod.DocumentType.Json);
  });
});

describe("decodeJobTemplate with explicit format", () => {
  const VALID_JOB_YAML = `
specificationVersion: jobtemplate-2023-09
name: FromYaml
steps:
  - name: S
    script:
      actions:
        onRun:
          command: x
`;

  it("defaults to YAML (matches Python default)", () => {
    const t = mod.decodeJobTemplate(VALID_JOB_YAML);
    expect(t.name).toBe("FromYaml");
    t.free();
  });

  it("DocumentType.Yaml accepts JSON (superset)", () => {
    const json = JSON.stringify({
      specificationVersion: "jobtemplate-2023-09",
      name: "FromJson",
      steps: [
        { name: "S", script: { actions: { onRun: { command: "x" } } } },
      ],
    });
    const t = mod.decodeJobTemplate(json, mod.DocumentType.Yaml);
    expect(t.name).toBe("FromJson");
    t.free();
  });

  it("DocumentType.Json rejects YAML-only syntax", () => {
    expect(() =>
      mod.decodeJobTemplate(VALID_JOB_YAML, mod.DocumentType.Json)
    ).toThrow(/valid JSON/);
  });
});

// F2 regression guard: the YAML parser must enforce the project-wide
// MAX_DOCUMENT_DEPTH budget, not run unbounded into stack exhaustion.
describe("decodeJobTemplate depth budget (F2)", () => {
  it("rejects pathologically deep YAML without crashing the host", () => {
    let doc = "";
    for (let i = 0; i < 200; i++) {
      doc += "  ".repeat(i) + "a:\n";
    }
    for (let i = 0; i < 200; i++) {
      doc += "  ".repeat(200 - 1 - i) + "  b: 1\n";
    }
    // The exact error wording isn't what matters here — either a depth
    // budget error or a missing-specificationVersion error is fine.
    // What matters is that the call returns synchronously with an
    // error, rather than taking down the WASM instance.
    expect(() => mod.decodeJobTemplate(doc, mod.DocumentType.Yaml)).toThrow();
  });
});

describe("decodeJobTemplateFromObject", () => {
  it("accepts a pre-parsed JS object (parity with Python *_dict)", () => {
    const obj = {
      specificationVersion: "jobtemplate-2023-09",
      name: "FromObject",
      steps: [
        { name: "S", script: { actions: { onRun: { command: "x" } } } },
      ],
    };
    const t = mod.decodeJobTemplateFromObject(obj);
    expect(t.name).toBe("FromObject");
    t.free();
  });

  it("rejects a non-object input", () => {
    expect(() => mod.decodeJobTemplateFromObject([])).toThrow();
  });
});

describe("decodeEnvironmentTemplateFromObject", () => {
  it("accepts a pre-parsed JS object", () => {
    const obj = {
      specificationVersion: "environment-2023-09",
      environment: { name: "E", variables: { FOO: "bar" } },
    };
    const t = mod.decodeEnvironmentTemplateFromObject(obj);
    expect(t.specificationVersion).toBe("environment-2023-09");
    t.free();
  });
});

// ── Expression Engine ──────────────────────────────────────────────

describe("ExprValue", () => {
  it("creates string value", () => {
    const v = mod.ExprValue.string("hello");
    expect(v.toString()).toBe("hello");
    expect(v.type).toBe("string");
    v.free();
  });

  it("creates int value", () => {
    const v = mod.ExprValue.int(42n);
    expect(v.toString()).toBe("42");
    expect(v.type).toBe("int");
    v.free();
  });

  it("creates float value", () => {
    const v = mod.ExprValue.float(3.14);
    expect(v.type).toBe("float");
    v.free();
  });

  it("creates bool value", () => {
    const v = mod.ExprValue.bool(true);
    expect(v.toString()).toBe("true");
    expect(v.type).toBe("bool");
    v.free();
  });

  it("creates path value", () => {
    const v = mod.ExprValue.path("/tmp/test", mod.PathFormat.Posix);
    expect(v.type).toBe("path");
    v.free();
  });
});

describe("SymbolTable", () => {
  it("set and get string values", () => {
    const st = new mod.SymbolTable();
    st.setString("Param.Frames", "1-10");
    expect(st.has("Param.Frames")).toBe(true);
    expect(st.has("Param.Missing")).toBe(false);

    const v = st.get("Param.Frames");
    expect(v.toString()).toBe("1-10");
    v.free();
    st.free();
  });

  it("set ExprValue", () => {
    const st = new mod.SymbolTable();
    const val = mod.ExprValue.int(42n);
    st.set("Param.Count", val);
    const got = st.get("Param.Count");
    expect(got.toString()).toBe("42");
    got.free();
    val.free();
    st.free();
  });

  it("allPaths returns all symbol paths", () => {
    const st = new mod.SymbolTable();
    st.setString("Param.A", "1");
    st.setString("Param.B", "2");
    const paths = st.allPaths();
    expect(paths).toContain("Param.A");
    expect(paths).toContain("Param.B");
    st.free();
  });
});

describe("FormatString", () => {
  it("parses and resolves a format string", () => {
    const fs = new mod.FormatString("{{Param.Dir}}/output.exr");
    expect(fs.isLiteral).toBe(false);
    expect(fs.references).toContain("Param.Dir");

    const st = new mod.SymbolTable();
    st.setString("Param.Dir", "/renders");
    const resolved = fs.resolve(st);
    expect(resolved).toBe("/renders/output.exr");

    fs.free();
    st.free();
  });

  it("literal format string", () => {
    const fs = new mod.FormatString("no interpolation");
    expect(fs.isLiteral).toBe(true);
    expect(fs.references).toEqual([]);
    fs.free();
  });
});

describe("ParsedExpression", () => {
  it("parses and evaluates an expression", () => {
    const expr = mod.parseExpression("Param.X");
    expect(expr.expression).toBe("Param.X");
    expect(expr.accessedSymbols).toContain("Param.X");

    const st = new mod.SymbolTable();
    st.setString("Param.X", "hello");
    const result = expr.evaluate(st);
    expect(result.toString()).toBe("hello");

    result.free();
    st.free();
    expr.free();
  });
});

describe("evaluateExpression", () => {
  it("evaluates a simple expression", () => {
    const st = new mod.SymbolTable();
    st.setString("Param.Name", "world");
    const result = mod.evaluateExpression("Param.Name", st);
    expect(result.toString()).toBe("world");
    result.free();
    st.free();
  });
});

describe("escapeFormatString", () => {
  it("escapes double braces", () => {
    // Single braces are not special in format strings, only {{ and }} are
    expect(mod.escapeFormatString("hello")).toBe("hello");
    // Test that it's callable and returns a string
    const result = mod.escapeFormatString("test {{ value }}");
    expect(typeof result).toBe("string");
  });

  it("leaves plain strings alone", () => {
    expect(mod.escapeFormatString("hello")).toBe("hello");
  });
});

describe("parseRangeExpr", () => {
  it("parses a simple range", () => {
    const result = mod.parseRangeExpr("1-5");
    expect(Array.from(result)).toEqual([1n, 2n, 3n, 4n, 5n]);
  });

  it("parses a range with step", () => {
    const result = mod.parseRangeExpr("1-10:3");
    expect(Array.from(result)).toEqual([1n, 4n, 7n, 10n]);
  });
});

describe("range_expr values", () => {
  // Template Schemas §3.4.1.1.1: a range_expr is written in its canonical
  // form, which depends only on its values.
  it("writes the canonical form through toString and the JSON transport", () => {
    const st = new mod.SymbolTable();
    const r = mod.evaluateExpression("range_expr('19-18:-5,13-18')", st);
    expect(r.type).toBe("range_expr");
    expect(r.toString()).toBe("13-19");
    // The JSON transport form carries the same string.
    const json = r.toJSON();
    expect(json.get("type")).toBe("range_expr");
    expect(json.get("value")).toBe("13-19");
    r.free();
    const s = mod.evaluateExpression("string(range_expr([1, 3, 5, 7, 9]))", st);
    expect(s.toString()).toBe("1-9:2");
    s.free();
    st.free();
  });

  it("compares range_expr values by their values", () => {
    const st = new mod.SymbolTable();
    const r = mod.evaluateExpression(
      "range_expr('1,3,5,7,9') == range_expr('1-9:2')",
      st
    );
    expect(r.toString()).toBe("true");
    r.free();
    st.free();
  });
});

describe("getDefaultLibrary", () => {
  it("returns a FunctionLibrary", () => {
    const lib = mod.getDefaultLibrary();
    expect(lib).toBeDefined();
    lib.free();
  });
});

describe("getDefaultMemoryLimit / getDefaultOperationLimit", () => {
  it("returns positive numbers", () => {
    expect(mod.getDefaultMemoryLimit()).toBeGreaterThan(0);
    expect(mod.getDefaultOperationLimit()).toBeGreaterThan(0);
  });
});

// ── Job Creation ───────────────────────────────────────────────────

// A Job Template with an ALWAYS and an ON_REQUEST Job Environment, the
// second with hostRequirements read from its parameter, one Step that
// requests it (entry enableIf), and one that does not.
const ENV_PARAMS_JOB = {
  specificationVersion: "jobtemplate-2023-09",
  extensions: ["ENV_PARAMS", "EXPR", "FEATURE_BUNDLE_1"],
  name: "J",
  jobEnvironments: [
    { name: "Always", variables: { A: "a" } },
    {
      name: "Gpu",
      activation: "ON_REQUEST",
      envParameterDefinitions: [{ name: "Count", type: "INT", default: 1 }],
      hostRequirements: {
        amounts: [{ name: "amount.worker.gpu", min: "{{ Env.Param.Count }}" }],
      },
      variables: { G: "{{ Env.Param.Count }}" },
    },
  ],
  steps: [
    {
      name: "Requester",
      hostRequirements: { amounts: [{ name: "amount.worker.gpu", max: 4 }] },
      dependencies: [
        {
          environment: "Gpu",
          enableIf: "'Gpu' in Job.EnvNames",
          parameters: { Count: "2" },
        },
      ],
      script: { actions: { onRun: { command: "x", args: ["{{ Job.EnvNames }}"] } } },
    },
    {
      name: "Other",
      hostRequirements: { amounts: [{ name: "amount.worker.gpu", max: 4 }] },
      script: { actions: { onRun: { command: "x" } } },
    },
  ],
};

describe("createJob", () => {
  const TEMPLATE_WITH_PARAMS = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "{{Param.JobName}}",
    parameterDefinitions: [
      { name: "JobName", type: "STRING", default: "DefaultJob" },
    ],
    steps: [
      {
        name: "Render",
        script: {
          actions: {
            onRun: { command: "echo", args: ["{{Param.JobName}}"] },
          },
        },
      },
    ],
  });

  it("creates a job with default parameters", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_PARAMS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    expect(job.name).toBe("DefaultJob");
    expect(job.stepCount).toBe(1);
    expect(job.stepNames).toEqual(["Render"]);
    job.free();
    opts.free();
    template.free();
  });

  it("creates a job with custom parameters", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_PARAMS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, { JobName: "MyJob" }, opts);
    expect(job.name).toBe("MyJob");
    job.free();
    opts.free();
    template.free();
  });

  it("a Job without Environment parameters has no new Step keys", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_PARAMS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    const step = job.toJSON().steps[0];
    expect("environmentParameters" in step).toBe(false);
    expect("environments" in step).toBe(false);
    job.free();
    opts.free();
    template.free();
  });

  it("stores a RANGE_EXPR effective value in canonical form", () => {
    // Template Schemas §3.2.2: RANGE_EXPR values are equal when they have the
    // same canonical form, so `1-9:2` and `1,3,5,7,9`, which denote the same
    // integers, are both stored as `1-9:2`.
    const doc = JSON.stringify({
      specificationVersion: "jobtemplate-2023-09",
      extensions: ["ENV_PARAMS", "EXPR"],
      name: "J",
      jobEnvironments: [
        {
          name: "E",
          envParameterDefinitions: [{ name: "R", type: "RANGE_EXPR", default: "1-3" }],
          variables: { V: "{{Env.Param.R}}" },
        },
      ],
      steps: [
        {
          name: "Run",
          dependencies: [{ environment: "E", parameters: { R: "1-9:2" } }],
          script: { actions: { onRun: { command: "x" } } },
        },
        {
          name: "Odd",
          dependencies: [{ environment: "E", parameters: { R: "1,3,5,7,9" } }],
          script: { actions: { onRun: { command: "x" } } },
        },
      ],
    });
    const template = mod.decodeJobTemplate(doc, mod.DocumentType.Json, undefined, [
      "ENV_PARAMS",
      "EXPR",
    ]);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    const values = job
      .toJSON()
      .steps.map((s) => Object.fromEntries(s.environmentParameters[0].values[0]).value);
    expect(values).toEqual(["1-9:2", "1-9:2"]);
    job.free();
    opts.free();
    template.free();
  });

  it("carries ENV_PARAMS effective values on each Step", () => {
    const doc = JSON.stringify({
      specificationVersion: "jobtemplate-2023-09",
      extensions: ["ENV_PARAMS", "EXPR"],
      name: "J",
      jobEnvironments: [
        {
          name: "E",
          envParameterDefinitions: [{ name: "N", type: "INT", default: 1 }],
          variables: { V: "{{Env.Param.N}}" },
        },
      ],
      steps: [
        {
          name: "S",
          dependencies: [
            { environment: "E", parameters: { N: "2" } },
            { environment: "Absent", required: false },
          ],
          script: { actions: { onRun: { command: "x" } } },
        },
      ],
    });
    const template = mod.decodeJobTemplate(doc, mod.DocumentType.Json, undefined, [
      "ENV_PARAMS",
      "EXPR",
    ]);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    const step = job.toJSON().steps[0];
    expect(step.environmentParameters).toHaveLength(1);
    const [entry] = step.environmentParameters;
    expect(entry.document).toEqual({ kind: "JobTemplate" });
    expect(entry.environment).toBe("E");
    // A symbol-table entry is a serde_json object, which serde-wasm-bindgen
    // delivers as a Map, as it does for `resolvedSymTab`.
    expect(entry.values.map((e) => Object.fromEntries(e))).toEqual([
      { name: "N", type: "int", value: "2" },
    ]);
    // createJob resolves against no Environment Templates, so the
    // optional external entry is dropped, and the Step enters every Job
    // Environment, so it lists none.
    expect("environments" in step).toBe(false);
    job.free();
    opts.free();
    template.free();
  });

  it("decodes ENV_PARAMS activation, hostRequirements, and enableIf", () => {
    const doc = JSON.stringify(ENV_PARAMS_JOB);
    const template = mod.decodeJobTemplate(doc, mod.DocumentType.Json);
    expect(template.stepCount).toBe(2);
    template.free();
    // ENV_PARAMS is in the default supported extensions; without it the
    // template is rejected.
    expect(mod.getSupportedExtensions()).toContain("ENV_PARAMS");
    expect(() =>
      mod.decodeJobTemplate(doc, mod.DocumentType.Json, undefined, [
        "EXPR",
        "FEATURE_BUNDLE_1",
      ]),
    ).toThrow(
      "Model validation error: 1 validation error for JobTemplate\n" +
        "extensions:\n\tUnsupported extension names: ENV_PARAMS",
    );
  });

  it("rejects ENV_PARAMS without EXPR", () => {
    // Template Schemas §1.1 item 3 (§1.2 item 3 for an Environment
    // Template): a document that lists ENV_PARAMS must also list EXPR.
    const job = JSON.stringify({
      specificationVersion: "jobtemplate-2023-09",
      extensions: ["ENV_PARAMS"],
      name: "J",
      steps: [{ name: "S", script: { actions: { onRun: { command: "x" } } } }],
    });
    expect(() => mod.decodeJobTemplate(job, mod.DocumentType.Json)).toThrow(
      "Model validation error: 1 validation error for JobTemplate\n" +
        "extensions:\n\tENV_PARAMS requires EXPR; both must be listed in the template's " +
        "`extensions` (Template Schemas §1.1 item 3).",
    );
    const env = JSON.stringify({
      specificationVersion: "environment-2023-09",
      extensions: ["ENV_PARAMS"],
      environment: { name: "E", variables: { V: "v" } },
    });
    expect(() => mod.decodeEnvironmentTemplate(env, mod.DocumentType.Json)).toThrow(
      "Model validation error: 1 validation error for EnvironmentTemplate\n" +
        "extensions:\n\tENV_PARAMS requires EXPR; both must be listed in the template's " +
        "`extensions` (Template Schemas §1.2 item 3).",
    );
  });

  it("rejects the Environment field names before the ENV_PARAMS renames", () => {
    for (const [field, value] of [
      ["runScope", "ALL"],
      ["parameterDefinitions", [{ name: "N", type: "INT", default: 1 }]],
    ]) {
      const doc = structuredClone(ENV_PARAMS_JOB);
      doc.jobEnvironments[0][field] = value;
      expect(() => mod.decodeJobTemplate(JSON.stringify(doc))).toThrow(
        `Validation error: 'jobtemplate-2023-09' failed checks: unknown field \`${field}\`, ` +
          "expected one of `name`, `description`, `envParameterDefinitions`, `activation`, " +
          "`hostRequirements`, `enableIf`, `script`, `variables`",
      );
    }
    // The renamed value is case-sensitive, and the old values are unknown.
    for (const value of ["ALL", "ONLY_REQUESTED", "always"]) {
      const doc = structuredClone(ENV_PARAMS_JOB);
      doc.jobEnvironments[0].activation = value;
      expect(() => mod.decodeJobTemplate(JSON.stringify(doc))).toThrow(
        `Validation error: 'jobtemplate-2023-09' failed checks: unknown variant \`${value}\`, ` +
          "expected `ALWAYS` or `ON_REQUEST`",
      );
    }
  });

  it("decodes enableIf as an expression field of type bool", () => {
    // A JSON boolean is a scalar used as written; a string is one
    // expression, never a scalar, so "false" is evaluated at job creation.
    for (const [enableIf, entersGpu] of [
      [false, false],
      ["false", false],
      ["'Gpu' in Job.EnvNames", true],
    ]) {
      const doc = structuredClone(ENV_PARAMS_JOB);
      doc.steps[0].dependencies[0].enableIf = enableIf;
      const template = mod.decodeJobTemplate(JSON.stringify(doc));
      const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
      const requester = mod.createJob(template, {}, opts).toJSON().steps[0];
      // A Step that enters every Job Environment lists none.
      expect("environments" in requester).toBe(!entersGpu);
      opts.free();
      template.free();
    }
    // Template Schemas §7.5: no {{ }}, no conversion to bool, no null.
    for (const [enableIf, message] of [
      [
        "{{ true }}",
        "\tInvalid expression: Set literals are not supported. An expression field takes an " +
          "expression without '{{ }}' (Template Schemas §7.5).\n  {{ true }}\n  ^~~~~~~~~~",
      ],
      [
        "Job.Name",
        "\thas type string, which does not fit type bool; an expression field converts no " +
          "type except int to float (Template Schemas §7.5).",
      ],
      [
        1,
        "\tthe integer scalar 1 does not fit type bool: an expression field of type bool " +
          "takes a boolean scalar or a string holding one expression (Template Schemas §7.5).",
      ],
      [
        null,
        "\tmust not be null: an expression field of type bool takes a boolean scalar or a " +
          "string holding one expression (Template Schemas §7.5).",
      ],
    ]) {
      const doc = structuredClone(ENV_PARAMS_JOB);
      doc.steps[0].dependencies[0].enableIf = enableIf;
      expect(() => mod.decodeJobTemplate(JSON.stringify(doc))).toThrow(
        "Model validation error: 1 validation error for JobTemplate\n" +
          "steps[0] -> dependencies[0] -> enableIf:\n" +
          message,
      );
    }
  });

  it("applies an Environment's own enableIf per Step", () => {
    // Template Schemas §4 item 6: an expression field of type bool on the
    // Environment, decided per Step with its effective values. Gpu becomes
    // ALWAYS, enabled only for a Count other than its default 1.
    const doc = structuredClone(ENV_PARAMS_JOB);
    delete doc.jobEnvironments[1].activation;
    doc.jobEnvironments[1].enableIf = "Env.Param.Count != 1";
    const template = mod.decodeJobTemplate(JSON.stringify(doc));
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const [requester, other] = mod.createJob(template, {}, opts).toJSON().steps;
    // Requester supplies Count 2 and enters both, so it lists none; Other
    // takes the default, for which Gpu is disabled: it enters Always only,
    // has no values for Gpu, and none of its host requirements.
    expect("environments" in requester).toBe(false);
    expect(other.environments.map((r) => r.environment)).toEqual(["Always"]);
    expect("environmentParameters" in other).toBe(false);
    expect(other.hostRequirements.amounts).toEqual([
      { name: "amount.worker.gpu", max: 4 },
    ]);
    opts.free();
    template.free();
    // The §7.5 rules apply to it, with its path.
    doc.jobEnvironments[1].enableIf = 1;
    expect(() => mod.decodeJobTemplate(JSON.stringify(doc))).toThrow(
      "Model validation error: 1 validation error for JobTemplate\n" +
        "jobEnvironments[1] -> enableIf:\n" +
        "\tthe integer scalar 1 does not fit type bool: an expression field of type bool " +
        "takes a boolean scalar or a string holding one expression (Template Schemas §7.5).",
    );
  });

  it("gives each Step its Environment set and combined host requirements", () => {
    const template = mod.decodeJobTemplate(JSON.stringify(ENV_PARAMS_JOB));
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const json = mod.createJob(template, {}, opts).toJSON();
    const [requester, other] = json.steps;
    // Requester enters both Job Environments, so it lists none; Other does
    // not request the ON_REQUEST one.
    expect("environments" in requester).toBe(false);
    expect(other.environments).toEqual([
      { document: { kind: "JobTemplate" }, environment: "Always" },
    ]);
    // Combined host requirements (Template Schemas §3.3.3): the Step's own
    // max with the Environment's min, resolved with the Step's value.
    expect(requester.hostRequirements).toEqual({
      amounts: [{ name: "amount.worker.gpu", min: 2, max: 4 }],
      attributes: undefined,
    });
    // Other does not enter Gpu, so it keeps its own requirements.
    expect(other.hostRequirements).toEqual({
      amounts: [{ name: "amount.worker.gpu", min: undefined, max: 4 }],
      attributes: undefined,
    });
    // Job.EnvNames lists every Job Environment, whatever its activation.
    const envNames = requester.resolvedSymTab
      .map((e) => Object.fromEntries(e))
      .find((e) => e.name === "Job.EnvNames");
    expect(envNames).toEqual({
      name: "Job.EnvNames",
      type: "list[string]",
      value: ["Always", "Gpu"],
    });
    opts.free();
    template.free();
  });

  it("rejects conflicting combined host requirements", () => {
    const doc = structuredClone(ENV_PARAMS_JOB);
    doc.steps[0].hostRequirements.amounts[0].max = 1;
    const template = mod.decodeJobTemplate(JSON.stringify(doc));
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    expect(() => mod.createJob(template, {}, opts)).toThrow(
      "Model validation error: 1 validation error for Submission\n" +
        "JobTemplate -> steps[0]:\n" +
        "\tStep 'Requester': no host can satisfy the combined host requirements for " +
        "amount 'amount.worker.gpu': JobTemplate -> jobEnvironments[1] -> " +
        "hostRequirements (Environment 'Gpu') requires min 2, and JobTemplate -> " +
        "steps[0] -> hostRequirements requires max 1. Template Schemas §3.3.3.",
    );
    opts.free();
    template.free();
  });

  it("job.toJSON() returns a JS object", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_PARAMS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, { JobName: "JsonTest" }, opts);
    const json = job.toJSON();
    expect(json.name).toBe("JsonTest");
    expect(json.steps).toHaveLength(1);
    expect(json.steps[0].name).toBe("Render");
    job.free();
    opts.free();
    template.free();
  });
});

// ── Step Dependency Graph ──────────────────────────────────────────

describe("StepDependencyGraph", () => {
  const MULTI_STEP = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "MultiStep",
    steps: [
      {
        name: "Composite",
        script: { actions: { onRun: { command: "composite" } } },
        dependencies: [{ dependsOn: "Render" }],
      },
      {
        name: "Render",
        script: { actions: { onRun: { command: "render" } } },
      },
    ],
  });

  it("returns topological order", () => {
    const template = mod.decodeJobTemplate(MULTI_STEP);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    const graph = new mod.StepDependencyGraph(job);
    const order = graph.topologicalOrder();
    expect(order.indexOf("Render")).toBeLessThan(order.indexOf("Composite"));
    graph.free();
    job.free();
    opts.free();
    template.free();
  });

  it("orders a step: entry (ENV_PARAMS) as a dependsOn entry", () => {
    const doc = JSON.parse(MULTI_STEP);
    doc.extensions = ["ENV_PARAMS", "EXPR"];
    doc.steps[0].dependencies = [{ step: "Render" }];
    const template = mod.decodeJobTemplate(JSON.stringify(doc));
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    // The Job has no `step` key: the entry is a `dependsOn` there.
    expect(job.toJSON().steps[0].dependencies).toEqual([{ dependsOn: "Render" }]);
    const graph = new mod.StepDependencyGraph(job);
    const order = graph.topologicalOrder();
    expect(order.indexOf("Render")).toBeLessThan(order.indexOf("Composite"));
    graph.free();
    job.free();
    opts.free();
    template.free();
  });

  it("rejects a step: entry without ENV_PARAMS", () => {
    const doc = JSON.parse(MULTI_STEP);
    doc.steps[0].dependencies = [{ step: "Render" }];
    expect(() => mod.decodeJobTemplate(JSON.stringify(doc))).toThrow(
      "Model validation error: 1 validation error for JobTemplate\n" +
        "steps[0] -> dependencies[0]:\n" +
        "\tthe 'step' form of a dependency requires the ENV_PARAMS extension.",
    );
  });
});

// ── Merge Parameter Definitions ────────────────────────────────────

describe("mergeJobParameterDefinitions", () => {
  it("returns parameter definitions", () => {
    const template = mod.decodeJobTemplate(
      JSON.stringify({
        specificationVersion: "jobtemplate-2023-09",
        name: "Test",
        parameterDefinitions: [
          { name: "Frames", type: "STRING", default: "1-10" },
          { name: "Quality", type: "INT", default: 5 },
        ],
        steps: [
          {
            name: "S1",
            script: { actions: { onRun: { command: "echo" } } },
          },
        ],
      })
    );
    const merged = mod.mergeJobParameterDefinitions(template);
    expect(merged).toHaveLength(2);
    // Verify it's an array of objects (structure may vary)
    expect(merged[0]).toBeDefined();
    expect(merged[1]).toBeDefined();
    template.free();
  });
});

// ── Evaluate Let Bindings ──────────────────────────────────────────

describe("evaluateLetBindings", () => {
  it("evaluates let bindings with expression references", () => {
    const st = new mod.SymbolTable();
    st.setString("Param.X", "hello");
    // RHS of let binding is an expression, result stored at top level
    const result = mod.evaluateLetBindings(["Y=Param.X"], st);
    // allPaths should include the new binding
    const paths = result.allPaths();
    expect(paths.some((p) => p.includes("Y"))).toBe(true);
    result.free();
    st.free();
  });
});

// ── PathParameterOptions ───────────────────────────────────────────

describe("PathParameterOptions", () => {
  it("constructor sets required fields and safe defaults", () => {
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    expect(opts.jobTemplateDir).toBe("/tmpl");
    expect(opts.currentWorkingDir).toBe("/cwd");
    // PathFormat.Posix matches openjd_expr::PathFormat::host() on wasm32.
    expect(opts.pathFormat).toBe(mod.PathFormat.Posix);
    expect(opts.allowTemplateDirWalkUp).toBe(false);
    expect(opts.allowUriPathValues).toBe(false);
    opts.free();
  });

  it("setters update fields", () => {
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    opts.jobTemplateDir = "/other";
    opts.currentWorkingDir = "/wd";
    opts.pathFormat = mod.PathFormat.Windows;
    opts.allowTemplateDirWalkUp = true;
    opts.allowUriPathValues = true;

    expect(opts.jobTemplateDir).toBe("/other");
    expect(opts.currentWorkingDir).toBe("/wd");
    expect(opts.pathFormat).toBe(mod.PathFormat.Windows);
    expect(opts.allowTemplateDirWalkUp).toBe(true);
    expect(opts.allowUriPathValues).toBe(true);
    opts.free();
  });

  it("PathFormat exposes Uri variant (parity with Rust)", () => {
    expect(mod.PathFormat.Uri).toBeDefined();
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    opts.pathFormat = mod.PathFormat.Uri;
    expect(opts.pathFormat).toBe(mod.PathFormat.Uri);
    opts.free();
  });
});

// ── F1 regression guards: PATH-default walk-up protection ─────────

describe("createJob PATH default walk-up protection", () => {
  const TEMPLATE_ABS_PATH_DEFAULT = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "T",
    parameterDefinitions: [
      { name: "Out", type: "PATH", default: "/etc/passwd" },
    ],
    steps: [
      { name: "S", script: { actions: { onRun: { command: "x" } } } },
    ],
  });

  it("rejects absolute PATH default with default options (F1)", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_ABS_PATH_DEFAULT);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    expect(() => mod.createJob(template, {}, opts)).toThrow(/absolute path/);
    opts.free();
    template.free();
  });

  it("accepts absolute PATH default when allowTemplateDirWalkUp=true", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_ABS_PATH_DEFAULT);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    opts.allowTemplateDirWalkUp = true;
    const job = mod.createJob(template, {}, opts);
    expect(job.name).toBe("T");
    job.free();
    opts.free();
    template.free();
  });
});

describe("createJob URI path value protection", () => {
  const TEMPLATE_URI_PATH_DEFAULT = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "T",
    extensions: ["EXPR"],
    parameterDefinitions: [
      { name: "Out", type: "PATH", default: "s3://bucket/key" },
    ],
    steps: [
      { name: "S", script: { actions: { onRun: { command: "x" } } } },
    ],
  });

  it("rejects URI PATH default with default options (F3)", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_URI_PATH_DEFAULT);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    expect(() => mod.createJob(template, {}, opts)).toThrow(
      /URI path values are not permitted/
    );
    opts.free();
    template.free();
  });

  it("accepts URI PATH default when allowUriPathValues=true", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_URI_PATH_DEFAULT);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    opts.allowUriPathValues = true;
    const job = mod.createJob(template, {}, opts);
    expect(job.name).toBe("T");
    job.free();
    opts.free();
    template.free();
  });
});

describe("preprocessJobParameters", () => {
  it("applies the same options as createJob (F1 via preprocess)", () => {
    const template = mod.decodeJobTemplate(
      JSON.stringify({
        specificationVersion: "jobtemplate-2023-09",
        name: "T",
        parameterDefinitions: [
          { name: "Out", type: "PATH", default: "/etc/passwd" },
        ],
        steps: [
          { name: "S", script: { actions: { onRun: { command: "x" } } } },
        ],
      })
    );
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    expect(() => mod.preprocessJobParameters(template, {}, opts)).toThrow(
      /absolute path/
    );
    opts.free();
    template.free();
  });
});

// ── CallerLimits ────────────────────────────────────────────────────
//
// CallerLimits is exposed as a plain structural type
// ({ maxTemplateSize: N, maxTaskCount: Nn, ... }) rather than a
// wasm_bindgen class. Callers construct object literals and reuse
// them freely across calls; there is no .free() ceremony.

describe("decodeJobTemplate CallerLimits enforcement (F4)", () => {
  const MINIMAL_TEMPLATE = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "T",
    steps: [
      { name: "S1", script: { actions: { onRun: { command: "x" } } } },
      { name: "S2", script: { actions: { onRun: { command: "x" } } } },
    ],
  });

  it("rejects template exceeding maxTemplateSize", () => {
    expect(() =>
      mod.decodeJobTemplate(MINIMAL_TEMPLATE, undefined, {
        maxTemplateSize: 50,
      })
    ).toThrow(/exceeds caller limit/);
  });

  it("accepts template under maxTemplateSize", () => {
    const t = mod.decodeJobTemplate(MINIMAL_TEMPLATE, undefined, {
      maxTemplateSize: 1_000_000,
    });
    expect(t.name).toBe("T");
    t.free();
  });

  it("rejects template exceeding maxStepCount", () => {
    expect(() =>
      mod.decodeJobTemplate(MINIMAL_TEMPLATE, undefined, { maxStepCount: 1 })
    ).toThrow(/[Ss]tep/);
  });

  it("accepts template when no caller limits are passed (default)", () => {
    const t = mod.decodeJobTemplate(MINIMAL_TEMPLATE);
    expect(t.name).toBe("T");
    t.free();
  });

  it("accepts the same limits object reused across multiple calls", () => {
    const limits = { maxTemplateSize: 1_000_000 };
    const t1 = mod.decodeJobTemplate(MINIMAL_TEMPLATE, undefined, limits);
    const t2 = mod.decodeJobTemplate(MINIMAL_TEMPLATE, undefined, limits);
    expect(t1.name).toBe("T");
    expect(t2.name).toBe("T");
    t1.free();
    t2.free();
  });

  it("accepts maxTaskCount as a BigInt (u64)", () => {
    // maxTaskCount isn't checked by decode, but the deserializer
    // must accept it without complaint for forward compatibility
    // with createJob — same options object often gets reused.
    const t = mod.decodeJobTemplate(MINIMAL_TEMPLATE, undefined, {
      maxTaskCount: 1_000_000n,
    });
    expect(t.name).toBe("T");
    t.free();
  });
});

describe("decodeJobTemplateFromObject CallerLimits enforcement", () => {
  it("rejects object with too many steps", () => {
    const obj = {
      specificationVersion: "jobtemplate-2023-09",
      name: "T",
      steps: [
        { name: "S1", script: { actions: { onRun: { command: "x" } } } },
        { name: "S2", script: { actions: { onRun: { command: "x" } } } },
      ],
    };
    expect(() =>
      mod.decodeJobTemplateFromObject(obj, { maxStepCount: 1 })
    ).toThrow(/[Ss]tep/);
  });
});

describe("createJob CallerLimits enforcement (F4)", () => {
  const TEMPLATE_WITH_TASKS = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "T",
    steps: [
      {
        name: "S",
        parameterSpace: {
          taskParameterDefinitions: [
            { name: "F", type: "INT", range: "1-10" },
          ],
        },
        script: { actions: { onRun: { command: "x" } } },
      },
    ],
  });

  it("rejects job exceeding maxTaskCount", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_TASKS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    expect(() =>
      mod.createJob(template, {}, opts, { maxTaskCount: 5n })
    ).toThrow(/[Tt]ask/);
    opts.free();
    template.free();
  });

  it("accepts job under maxTaskCount", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_TASKS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts, { maxTaskCount: 100n });
    expect(job.name).toBe("T");
    job.free();
    opts.free();
    template.free();
  });

  it("accepts job when no caller limits are passed (default)", () => {
    const template = mod.decodeJobTemplate(TEMPLATE_WITH_TASKS);
    const opts = new mod.PathParameterOptions("/tmpl", "/cwd");
    const job = mod.createJob(template, {}, opts);
    expect(job.name).toBe("T");
    job.free();
    opts.free();
    template.free();
  });
});

// ── EvalOptions ─────────────────────────────────────────────────────
//
// EvalOptions is a plain structural type ({ memoryLimit?, operationLimit? })
// passed as the optional trailing argument to evaluateExpression and
// ParsedExpression.evaluate. Omitting it, or passing undefined / {},
// keeps the built-in defaults. Callers can reuse the same options
// literal across calls — there is no .free() ceremony, matching the
// CallerLimits pattern.

describe("evaluateExpression EvalOptions enforcement (F5)", () => {
  it("respects memoryLimit override — rejects memory-blowing expression", () => {
    const st = new mod.SymbolTable();
    expect(() =>
      mod.evaluateExpression("'x' * 1000000", st, undefined, {
        memoryLimit: 1_000,
      })
    ).toThrow(/memory/i);
    st.free();
  });

  it("respects operationLimit override — rejects op-exhausting expression", () => {
    const st = new mod.SymbolTable();
    expect(() =>
      mod.evaluateExpression(
        "sum([i for i in range(10000)])",
        st,
        undefined,
        { operationLimit: 10 }
      )
    ).toThrow(/operation/i);
    st.free();
  });

  it("accepts the same options literal reused across calls", () => {
    const st = new mod.SymbolTable();
    const opts = { memoryLimit: 50_000_000, operationLimit: 1_000_000 };
    const r1 = mod.evaluateExpression("1 + 1", st, undefined, opts);
    const r2 = mod.evaluateExpression("2 + 2", st, undefined, opts);
    expect(r1.toString()).toBe("2");
    expect(r2.toString()).toBe("4");
    r1.free();
    r2.free();
    st.free();
  });

  it("defaults stay in effect when options are omitted", () => {
    const st = new mod.SymbolTable();
    const r = mod.evaluateExpression("sum([i for i in range(100)])", st);
    expect(r.toString()).toBe("4950");
    r.free();
    st.free();
  });

  it("accepts empty options literal as equivalent to undefined", () => {
    const st = new mod.SymbolTable();
    const r = mod.evaluateExpression("1 + 1", st, undefined, {});
    expect(r.toString()).toBe("2");
    r.free();
    st.free();
  });
});

describe("ParsedExpression.evaluate EvalOptions enforcement (F5)", () => {
  it("respects memoryLimit override", () => {
    const parsed = mod.parseExpression("'x' * 1000000");
    const st = new mod.SymbolTable();
    expect(() =>
      parsed.evaluate(st, undefined, { memoryLimit: 1_000 })
    ).toThrow(/memory/i);
    st.free();
    parsed.free();
  });

  it("accepts default when options are omitted", () => {
    const parsed = mod.parseExpression("2 + 3");
    const st = new mod.SymbolTable();
    const r = parsed.evaluate(st);
    expect(r.toString()).toBe("5");
    r.free();
    st.free();
    parsed.free();
  });

  it("works with a library argument and options together", () => {
    const parsed = mod.parseExpression("1 + 1");
    const st = new mod.SymbolTable();
    // Note: passing `lib` by value to evaluate() consumes the JS
    // handle (wasm-bindgen moves the internal pointer), so we must
    // NOT call `.free()` on it after. The library is freed by
    // wasm-bindgen when it's consumed.
    const lib = mod.getDefaultLibrary();
    const r = parsed.evaluate(st, lib, {
      memoryLimit: 10_000_000,
      operationLimit: 100_000,
    });
    expect(r.toString()).toBe("2");
    r.free();
    st.free();
    parsed.free();
  });
});

// ── SymbolTable: dotted-key surface (F6) ────────────────────────────

describe("SymbolTable dotted-key surface (F6)", () => {
  it("rejects nesting under an existing scalar", () => {
    const st = new mod.SymbolTable();
    st.setString("A", "leaf");
    expect(() => st.setString("A.B", "child")).toThrow(/A/);
    st.free();
  });

  it("rejects scalar assignment over an existing subtable", () => {
    const st = new mod.SymbolTable();
    st.setString("Param.Frames", "1-10");
    const v = mod.ExprValue.int(99n);
    expect(() => st.set("Param", v)).toThrow(/Param/);
    v.free();
    st.free();
  });

  it("supports deep nesting round-trip", () => {
    const st = new mod.SymbolTable();
    st.setString("A.B.C", "deep");
    const got = st.get("A.B.C");
    expect(got.toString()).toBe("deep");
    expect(st.has("A.B.C")).toBe(true);
    expect(st.has("A.B")).toBe(false);   // intermediate is a table
    expect(st.has("A")).toBe(false);     // intermediate is a table
    got.free();
    st.free();
  });

  it("overwrites a scalar at the same key", () => {
    const st = new mod.SymbolTable();
    st.setString("Param.X", "old");
    st.setString("Param.X", "new");
    const got = st.get("Param.X");
    expect(got.toString()).toBe("new");
    got.free();
    st.free();
  });

  it("get returns undefined for an unset key", () => {
    const st = new mod.SymbolTable();
    expect(st.get("Missing.Key")).toBeUndefined();
    st.free();
  });
});

// ── getSupportedExtensions + supportedExtensions passthrough (F8) ───

describe("getSupportedExtensions (F8)", () => {
  it("returns the full default allowlist", () => {
    const exts = mod.getSupportedExtensions();
    expect(exts.length).toBe(6);
    expect(exts).toContain("TASK_CHUNKING");
    expect(exts).toContain("REDACTED_ENV_VARS");
    expect(exts).toContain("FEATURE_BUNDLE_1");
    expect(exts).toContain("EXPR");
    expect(exts).toContain("WRAP_ACTIONS");
    expect(exts).toContain("ENV_PARAMS");
  });
});

describe("supportedExtensions enforcement on decode (F8)", () => {
  const TEMPLATE_USING_EXPR = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "T",
    extensions: ["EXPR"],
    steps: [
      { name: "S", script: { actions: { onRun: { command: "x" } } } },
    ],
  });

  const TEMPLATE_NO_EXTENSIONS = JSON.stringify({
    specificationVersion: "jobtemplate-2023-09",
    name: "T",
    steps: [
      { name: "S", script: { actions: { onRun: { command: "x" } } } },
    ],
  });

  it("default (omitted) accepts EXPR template", () => {
    const t = mod.decodeJobTemplate(TEMPLATE_USING_EXPR);
    expect(t.name).toBe("T");
    t.free();
  });

  it("rejects EXPR template when EXPR is not in the allowlist", () => {
    expect(() =>
      mod.decodeJobTemplate(TEMPLATE_USING_EXPR, undefined, undefined, [
        "TASK_CHUNKING",
      ])
    ).toThrow(/EXPR|extension/i);
  });

  it("empty allowlist accepts a template that uses no extensions", () => {
    const t = mod.decodeJobTemplate(
      TEMPLATE_NO_EXTENSIONS,
      undefined,
      undefined,
      []
    );
    expect(t.name).toBe("T");
    t.free();
  });

  it("empty allowlist rejects an EXPR template", () => {
    expect(() =>
      mod.decodeJobTemplate(TEMPLATE_USING_EXPR, undefined, undefined, [])
    ).toThrow(/EXPR|extension/i);
  });

  it("allowlist containing EXPR accepts EXPR template", () => {
    const t = mod.decodeJobTemplate(
      TEMPLATE_USING_EXPR,
      undefined,
      undefined,
      ["EXPR"]
    );
    expect(t.name).toBe("T");
    t.free();
  });

  it("allowlist derived from getSupportedExtensions accepts all templates", () => {
    const defaults = mod.getSupportedExtensions();
    const t = mod.decodeJobTemplate(
      TEMPLATE_USING_EXPR,
      undefined,
      undefined,
      defaults
    );
    expect(t.name).toBe("T");
    t.free();
  });

  it("accepts the same allowlist array reused across multiple calls", () => {
    const allowlist = mod.getSupportedExtensions();
    const t1 = mod.decodeJobTemplate(TEMPLATE_NO_EXTENSIONS, undefined, undefined, allowlist);
    const t2 = mod.decodeJobTemplate(TEMPLATE_USING_EXPR, undefined, undefined, allowlist);
    expect(t1.name).toBe("T");
    expect(t2.name).toBe("T");
    t1.free();
    t2.free();
  });
});

describe("supportedExtensions on decodeJobTemplateFromObject (F8)", () => {
  it("rejects EXPR template under restricted allowlist", () => {
    const obj = {
      specificationVersion: "jobtemplate-2023-09",
      name: "T",
      extensions: ["EXPR"],
      steps: [
        { name: "S", script: { actions: { onRun: { command: "x" } } } },
      ],
    };
    expect(() =>
      mod.decodeJobTemplateFromObject(obj, undefined, ["TASK_CHUNKING"])
    ).toThrow(/EXPR|extension/i);
  });
});
