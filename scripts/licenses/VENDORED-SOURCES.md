# Copied third-party source and development resources

Copyright (c) 2023 shadcn. All material below retains the upstream MIT license
in `upstream/SHADCN-MIT.txt`; SeaSnail's Apache-2.0 license does not replace it.

| Local scope | Upstream source | Verification / changes |
| --- | --- | --- |
| `.agents/skills/shadcn/` (15 files, including icons) | `shadcn-ui/ui`, `skills/shadcn`, revision `6cd3f4c65c361ab6554e06a77e6a0af9cf8b6e37` | Every copied file matches the official Git blob byte for byte |
| `.agents/skills/migrate-radix-to-base/` (10 files) | `shadcn-ui/ui`, `skills/migrate-radix-to-base`, revision `f3e7de11752b087b1c4bf61f4035a866f3a4f9ed` | Every copied file matches the official Git blob byte for byte |
| `apps/desktop/src/components/ui/` | shadcn/ui registry templates, `radix-nova`; CLI version locked in `apps/desktop/pnpm-lock.yaml` | Templates adapted by SeaSnail contributors; imports, behavior and styling may differ. Original registry revision was not recorded; no byte-identity claim. Local `LICENSE` retains MIT |

`VENDORED-SOURCES.json` records each skill file's pinned source URL, upstream Git
blob and SHA-256. The MIT license was checked at both exact skill revisions;
both copies match `upstream/SHADCN-MIT.txt`. No skill instructions or icons were
modified to add this attribution; the root `NOTICE` and these materials accompany
the complete source distribution.

Modified UI templates retain upstream copyright and permission notices. The
SeaSnail brand policy does not claim ownership of shadcn's skill icons.
