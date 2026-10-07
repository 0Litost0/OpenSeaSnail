# Source snapshot review — 2026-10-07

This records local source preparation, not approval to publish a repository or
binary release. The new local repository has independent Git metadata, branch
`main`, no imported commits and no remote. No initial commit or push was made.

## Cleanup and migration

- Removed the entire historical `feature/` tree (103 previously tracked files),
  including the known personal screenshots. Removed the old first-run session
  audit and account-document implementation logs.
- Wrote [architecture](architecture.md) around current code boundaries instead
  of copying historical roadmaps.
- Preserved the 23-row regression matrix and stable membership in
  [test design](../tests/api_regression/docs/design.md), with focused requirements
  and the previous quality/budget decision. No numeric thresholds were changed.
- Initially preserved the six historical system-voice observations for rescoring.
  The audio follow-up below replaces that obsolete baseline with new licensed
  speech observations. Analysis output goes to ignored `artifacts/baseline/`.
- Updated README, development docs, catalog, examples, script paths and code
  comments. No references to the removed directory remain in candidate source.
- Ignored build outputs, local diagnostics, dependency directories and pnpm cache
  are excluded from the source candidates. This review does not clear them for
  sharing as a full working-directory archive.

## Validation

- Architecture boundary checks passed.
- Development setup/license tests: 15 passed.
- API TypeScript check passed after reinstalling the exact locked dependencies;
  copied `node_modules` had lost pnpm dependency links. No lock changed.
- Catalog: 23 cases, 22 required, 17 quick; all document/asset references valid.
- Contract controls: 7 valid examples accepted, 13 invalid examples rejected and
  4 invalid catalog mutations rejected.
- Audio/reference hashes and six quality rejection controls passed.
- Diagnostics, result adaptation and mutation/replay self-checks: 9 passed.
- Baseline cleanup tests: 3 passed. Historical six-output rescoring passed;
  this was not a new ASR run.
- Candidate-source scan found no known owner home path or private email and no
  personal screenshots. Gitleaks found two synthetic token strings in existing
  logging-redaction unit tests; review confirmed they are test literals.
- Project-owned Markdown file links resolved. Verbatim upstream model cards keep
  their original relative links; those are snapshots, not local navigation.
- Third-party copyright contacts and full upstream material bytes were retained.
- The original development repository remained unchanged and clean.

## Remaining work before publication

1. The audio follow-up replaces the three macOS system-voice WAVs with
   procedural business audio and CC-BY-4.0 FLEURS speech, using new identities
   and unchanged numeric quality limits. See
   [audio fixture evidence](../tests/api_regression/assets/AUDIO-LICENSE.md) and
   [follow-up validation record](audio-fixture-replacement-2026-10-07.md).
2. Before distributing default ONNX model binaries, resolve the source-weight
   provenance limitation in [model evidence](../scripts/licenses/MODEL-LICENSE.md).
3. Choose the public repository URL and initial-commit identity. Existing GitHub
   links still point to the development project's public handle; no new remote
   URL or private email has been inserted.
4. Review the exact initial commit and complete the relevant
   [release checklist](releasing.md). This turn did not run full workspace tests,
   full API acceptance, a fresh App build, macOS UI acceptance or signing checks.

Pattern/secret scans are supporting evidence, not proof that arbitrary future
files or new commit metadata are safe to publish.
