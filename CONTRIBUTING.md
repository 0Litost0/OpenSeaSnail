# Contributing to SeaSnail

Thank you for helping improve local voice dictation. Read the [development guide](doc/development.md), [testing conventions](README.md#testing-and-agent-conventions), and [code of conduct](CODE_OF_CONDUCT.md) before making changes.

For a bug, include the app version, macOS version, architecture, steps, and expected/actual behavior. Use fictional examples and redacted diagnostics. Never attach personal audio, transcripts, clipboard data, tokens, or provider credentials. Report vulnerabilities privately using [SECURITY.md](SECURITY.md).

Discuss substantial behavior or architecture changes in an issue first. Keep pull requests focused, explain the problem and resulting behavior, and list checks actually run and remaining limitations. Include source and license evidence for any new dependency, model, image, or other asset. Do not edit generated OpenAPI types directly.

## Developer Certificate of Origin

Contributions use the [Developer Certificate of Origin 1.1](https://developercertificate.org/). By adding a `Signed-off-by` trailer, you certify the contribution under the DCO, including your right to submit it under the project's license. You retain copyright in your contribution; signing off does not transfer copyright.

Sign each contribution commit with your name and email:

```sh
git commit -s
```

To add a missing sign-off to your latest, unpublished commit:

```sh
git commit --amend --no-edit -s
```

For a larger series, use interactive rebase carefully and coordinate with collaborators before rewriting shared commits. A GitHub PR check verifies sign-off trailers on the submitted commit range. Do not sign someone else's certification on their behalf.

## Local verification

Use the [scope-based matrix](README.md#testing-and-agent-conventions). Frontend changes require typecheck, tests, and build. Rust changes require workspace check and tests. API behavior changes also require affected scenarios and quick regression. Full acceptance is a separate run. Record failures honestly; never skip assertions to make a change appear ready.
