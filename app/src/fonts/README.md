# Bundled font: JetBrains Mono

The terminal and the chrome use JetBrains Mono (decision D-17), bundled so cell metrics do not depend on what the
machine has installed.

| | |
|---|---|
| Project | https://github.com/JetBrains/JetBrainsMono |
| Release tag | `v2.304` (published 2023-01-14, the latest release when this was fetched on 2026-10-10) |
| Download URL | https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip |
| Zip SHA-256 | `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf` |
| Published digest | none: GitHub reports `digest: null` for this asset, so the checksum above is the one that was measured |
| Licence | SIL Open Font License 1.1, see `OFL.txt` (copyright 2020 The JetBrains Mono Project Authors) |
| Modification | none: the files are byte-identical to the entries of the zip |

## Files

Extracted from `fonts/webfonts/` of the zip (and `OFL.txt` from its root); nothing else was taken from the archive.

| File | SHA-256 |
|---|---|
| `JetBrainsMono-Regular.woff2` | `a9cb1cd82332b23a47e3a1239d25d13c86d16c4220695e34b243effa999f45f2` |
| `JetBrainsMono-Bold.woff2` | `c503cc5ec5f8b2c7666b7ecda1adf44bd45f2e6579b2eba0fc292150416588a2` |
| `JetBrainsMono-Italic.woff2` | `cb6a1b246318ed3885d7dffa14a2609297fe80e9b8e500bea33b52fa312a36a4` |
| `OFL.txt` | `30f0c136e3c88e422d0791acd97238870f9054a9729bc34cf2ff0d4ed8cac4ad` |

## Updating

Fetch the newest release, unpack it into a new empty directory outside the repository (the archive is untrusted
data: nothing from it is executed), extract only the three woff2 files and `OFL.txt`, compare the zip with the
digest GitHub publishes when there is one, and update the tag, URL and checksums above in the same commit.
`app/tests/browser/fonts.test.mjs` checks that the files are served with a font content type and load.
