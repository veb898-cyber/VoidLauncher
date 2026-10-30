# Test fixtures — provenance

These files are **verbatim copies of public metadata** downloaded on 2026-10-29,
used only by `src-tauri/tests/`. They are not read by the launcher at runtime.

## `versions/`

| File | Source | SHA-256 |
|---|---|---|
| `1.12.2.json` | `https://piston-meta.mojang.com/v1/packages/832d95b9f40699d4961394dcf6cf549e65f15dc5/1.12.2.json` | `56dac2e1caa872d7e37a7b5523056cd812dca94f912ad2bd652817bee60edca4` |
| `1.20.1.json` | `https://piston-meta.mojang.com/v1/packages/fff64d6821a97cf12ce5f16df58dd5db0ed210b3/1.20.1.json` | `5a60bf164ed8c8af0d7547ec2e619b91a37bca2b566bdaa06acf796e85e95f1e` |
| `1.21.4.json` | `https://piston-meta.mojang.com/v1/packages/64843a304d7747da779448f853323962233f39ff/1.21.4.json` | `71bc842aefb41c9c33e58f333b1164a396547188cb459fdc4608cdec67909bee` |
| `26.1.2.json` | `https://piston-meta.mojang.com/v1/packages/8f9f3c548c892ce41b78561138aeba112cba3490/26.1.2.json` | `7ac4a97220ae169b57a3897e4e00bcdbcdffe11476e6f20c1b89eaf4dcd0ef0c` |

Obtained from the official version manifest
(`https://piston-meta.mojang.com/mc/game/version_manifest_v2.json`).
No fields were removed: tests that materialise the library tree parse the
`downloads` paths straight from these files.

## `loader_profiles/`

These are **not** verbatim downloads. They are written in VoidLauncher's
persisted `LoaderProfile` format (`main_class`, `libraries`, `jvm_args`,
`game_args`, `legacy_args`) and reproduce the shape of the real loader
metadata:

| File | Basis |
|---|---|
| `fabric-0.16.9-1.20.1.json` | Fabric loader 0.16.9 for MC 1.20.1 (`KnotClient`, intermediary + loader + ASM + Mixin). |
| `neoforge-21.1.172-1.21.4.json` | NeoForge 21.1.172 for MC 1.21.4 (`BootstrapLauncher`, `-p` module path, `--launchTarget forgeclient`). ASM 9.8 intentionally overlaps vanilla 1.21.4 ASM 9.6 so the loader-wins rule is exercised. |
| `forge-14.23.5.2860-1.12.2.json` | Forge 14.23.5.2860 for MC 1.12.2, `launchwrapper` + legacy `minecraftArguments` (`legacy_args: true`). |

The network endpoints for Fabric Meta and the NeoForge Maven were not reachable
from the machine that produced these files, so the profiles are hand-written
from the documented format. They are **shape-faithful, value-synthetic**: tests
assert composition rules (ordering, substitution, dedup), not the real
artefact bytes. When network access is available, they should be replaced with
real profiles captured from `instance.json` of a real install.

## Why full version JSONs are committed

Trimming `arguments`/`libraries` hides exactly the quirks these tests exist to
catch (OS-specific rules, native classifiers, `inheritsFrom`-less legacy
format). The four files together are ~140 KB, which is acceptable for a repo
that ships a launcher.
