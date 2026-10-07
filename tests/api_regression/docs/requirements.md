# API regression requirements

The API regression project exercises public business behavior through the
production daemon composition. Test-only credentials and deterministic ASR
fixtures remain confined to the separate host; they are not production modes.

- Stable case IDs map to explicit business promises, preconditions, assertions
  and versioned assets in `case-catalog.json` and `contracts/`.
- The fixed default catalog has 22 required cases; 17 deterministic cases form
  quick regression. The real provider case is optional and needs explicit setup.
- Each attempt uses an isolated home, account state, credentials and loopback
  endpoint. Tests must not access the developer's production Keychain or data.
- Success includes business assertions and confirmed teardown. Missing cases,
  environment errors, interrupted attempts and unknown teardown are incomplete;
  business/cleanup failures and flaky retries cannot pass the gate.
- A real ASR case must use the pinned model/artifact and audio/reference inputs.
  Completing a task without passing content checks is not quality acceptance.
- Reports preserve failed attempts, selected/actual cases and replay identities.
  Published diagnostics must exclude credentials, user data and private paths.
- Replay validates the exact build, registered assets, rules and fault identity;
  a report from an earlier source snapshot is not evidence for the new snapshot.
- Recovery only stops resources whose ownership and process identity are known.
  Unknown/live resources are retained for explicit recovery rather than erased.
- Extensions cannot shadow default cases, replace required membership or change
  registered assertions. Demonstration faults must be detected by the original
  business assertions in a fresh environment.

See [design and coverage](design.md), [quality decision](quality-approval.v3.md),
[execution instructions](../README.md) and [extension guide](../examples/README.md).
