# API regression contracts v1

These versioned contracts are used by the implemented Playwright API regression project. The checks below validate contracts and examples; they do not execute business cases. For business execution and environment setup, see the [test README](../README.md); development and Agent testing conventions are maintained in the [root README](../../../README.md#测试与-agent-开发约定).

From the repository root:

```sh
python3 -m pip install -r tests/api_regression/contracts/requirements.txt
python3 tests/api_regression/scripts/check_contracts.py all
python3 tests/api_regression/scripts/check_contracts.py result /path/to/result.json
python3 tests/api_regression/scripts/check_contracts.py replay /path/to/replay.json
```

Python 3.10+ and the pinned JSON Schema validator are needed only for this development check. `all` verifies the catalog against the design matrix and independent required-ID snapshot, resolves document/asset/precondition/assertion references, accepts legal examples, and rejects illegal examples and catalog mutations. Exit 0 means contract validation passed; exit 2 means invalid/unavailable input. Diagnostic messages never print rejected values.

## Catalog

`case-catalog.json` is the business identity registry. `acceptance-required.v1.json` is a separately versioned snapshot: the required set must never be derived from discovered or executed tests. Preconditions and explicit business promises are in `preconditions.v1.json` and `assertions.v1.json`. These describe obligations; executable assertions live in the registered specs. Discovery checks connect the registry to the implemented tests. Catalog references to the asset manifest alone do not prove model validation; actual run results provide that evidence.

## Result

The JSON Schema validates field types and forbids undeclared fields. The semantic checker also verifies unique case/attempt/step IDs, selection, preserved retry chronology, strict flaky outcomes and gate/exit consistency. For acceptance, requested IDs must equal the independent required set; `missing_case_ids` contains required IDs with no executed attempt (an explicit `not_run` is unexecuted). An environment error or interruption is an attempted but incomplete execution. For a subset, each requested case needs a record, including explicit `not_run` attempts; missing required cases outside that subset are not presented as verified.

`execution_status` and `teardown_status` are independent. Business and teardown errors coexist in `errors[]`; never replace the original failure. A business pass followed by failed teardown has gate `failed`. An unknown teardown is incomplete. Report status `not_published` is incomplete; a failed publication is a failure. Mixed outcomes aggregate as `2 > 1 > 0`:

- 0 / passed: declared scope completed, all attempts/teardown/report passed.
- 1 / failed: business failure, earlier failed attempt even if retried successfully, flaky, teardown failure or report failure.
- 2 / incomplete: invalid configuration, missing dependency/case, not_run, environment error, interruption or unknown resource cleanup.

Framework final success never erases the first failed attempt. The examples include business+teardown failure, flaky, not_run, environment error and interrupted execution. Build fields on individual attempts describe the actual build used; manifest/asset hashes must also be verified by the runtime tooling, not merely accepted by the schema.

## Replay

Replay shares the executable configuration shape with each attempt: build/source identity, assets, assertion version, runtime scenario or real model/artifacts, provider mode, Keychain mode, budget, optional mutation and secure credential references. `source_patch` is a local, non-secret source snapshot/patch identity that must include any required untracked sources. A dirty build without that asset may be reported, but exact replay is refused. Source reconstruction additionally requires the recorded toolchain, target, profile, features, RUSTFLAGS and lockfile. Matching source names alone is insufficient; the binary hash identifies the original artifact. A rebuilt different binary is not silently treated as identical.

Synthetic account credentials are generated in memory. External credentials use environment references (`reference` names a variable, never its value). No password, token, credential value, raw header or unrestricted configuration object is permitted. Known bearer/canary patterns in free text are rejected. This check does not replace the runtime secret registry, allowlisted diagnostics, HTML/ZIP checks and atomic publication. Fault fields must identify the same base commit and binary as the attempt. Remote model output is explicitly nondeterministic.

## Version changes

v1 schemas use JSON Schema draft 2020-12, no network `$ref`, and `additionalProperties: false`. Adding/removing fields or changing meaning requires a new schema version and examples; readers reject unsupported versions. Stable case IDs are never repurposed. Renames/splits need an explicit historical mapping. Changes to the required set update both its snapshot and the design matrix with a coverage rationale.
