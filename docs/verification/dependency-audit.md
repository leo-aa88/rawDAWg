# Dependency audit (QUAL-03, PLAT-05/06)

Policy: `deny.toml`. CI runs the same check on every push (`.github/workflows/ci.yml`, job
"Dependency audit", `cargo-deny-action` pinned by commit). Release packages carry the per-crate
license list (`THIRD-PARTY-LICENSES.txt`) and a CycloneDX SBOM (`sbom.cdx.json`), both generated
from `Cargo.lock` by `cargo xtask package`.

## Run of 2026-11-25

```sh
cargo deny --offline check
```

| Item | Value |
|---|---|
| cargo-deny | 0.20.2 |
| Advisory database | RustSec advisory-db at commit `10db699` (2026-11-24 22:47 +01:00), local copy, not refreshed for this run |
| Targets | x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc |
| Result | **advisories ok, bans ok, licenses ok, sources ok** |

Notes:

- **Advisories.** Three RustSec advisories are ignored with recorded reasons:
  RUSTSEC-2026-0370, -0371 and -0372. They flag `zvariant`, `zbus_names` and `zvariant_derive`
  as *unmaintained*, not vulnerable; the crates were merged into zbus 6. They come in only on Linux,
  through eframe 0.37's accessibility bridge (`accesskit_unix`). Revisit on the next eframe update.
  No vulnerability advisory matches.
- **Bans.** JUCE, webview stacks (wry, tauri, webview2-com) and `openssl-sys` are banned
  (PLAT-05/06). None is present.
- **Sources.** Only crates.io; no git dependencies.
- **Licenses.** Every crate's license is on the permissive allowlist plus MPL-2.0. Three allowed
  licenses are not used by any current dependency (BSL-1.0, CDLA-Permissive-2.0, Unicode-DFS-2016);
  cargo-deny warns about them and they are harmless.

## CI (2026-11-26)

The "Dependency audit" job (`EmbarkStudios/cargo-deny-action` v2.1.1, cargo-deny 0.20.2,
`--all-features`, current advisory database fetched by the action) passes since run
45731123590; the workflow had not run before that (see PROGRESS.md, "CI made to run").

## License distribution

From `cargo deny --offline list -l license`. A crate under a choice of licenses counts once per
license, so the counts overlap.

| License | Crates | Notes |
|---|---|---|
| MIT | 317 | |
| Apache-2.0 | 274 | |
| Zlib | 10 | |
| MPL-2.0 | 9 | Symphonia (audio decoding, 8 crates) and `option-ext`; file-level weak copyleft, unmodified |
| Unlicense | 7 | dual with MIT (`midly`, `memchr`, …) |
| Apache-2.0 WITH LLVM-exception | 4 | `rustix`, `linux-raw-sys` |
| BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0 | 2 each | |
| 0BSD, CC0-1.0, MIT-0, OFL-1.1, Ubuntu-font-1.0 | 1 each | the two font licenses cover egui's bundled default fonts |
| GPL-2.0-only | 1 | `self_cell` is "Apache-2.0 OR GPL-2.0-only" and is used under Apache-2.0 |
| AGPL-3.0-only | 18 | rawDAWg's own crates, per the repository LICENSE placed by the owner |

The application license and the legal publisher identity remain owner decisions (CLAUDE.md);
this audit only checks that dependencies are compatible with the license the owner placed.

## Not covered

- The advisory database was not refreshed online for this run. CI fetches the current database on
  every run.
- No dependency-review or supply-chain signing checks (for example cargo-vet); not requested by
  the specification.
- Third-party plugins loaded at run time are user-installed native code and outside this audit
  (PLUGIN_API.md §3).
