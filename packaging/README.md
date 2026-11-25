# Packaging

`cargo xtask package --format <deb|msi|portable>` builds the native packages (PLAT-04):

| Format | OS | Tool | Output |
|---|---|---|---|
| `deb` | Ubuntu 24.04 | `dpkg-deb` (+ `desktop-file-validate`, `lintian` when installed) | `target/package/rawdawg-<version>-x86_64.deb` |
| `msi` | Windows 11 | WiX v5 CLI (`dotnet tool install --global wix --version 5.0.2`), manifest `windows/rawdawg.wxs` | `target/package/rawdawg-<version>-x86_64.msi` |
| `portable` | Windows 11 | PowerShell `Compress-Archive` | `target/package/rawdawg-<version>-x86_64.zip` |

Every package contains the `rawdawg` binary, the CLAP module (`rawDAWg.clap`), the VST3 bundle
(`rawDAWg.vst3/Contents/<arch>/…` + `moduleinfo.json`), the user guide, `LICENSE`, a third-party
license report and a CycloneDX SBOM generated from `Cargo.lock`; `SHA256SUMS` lists the artifacts.
The deb installs the plugins into `/usr/lib/clap` and `/usr/lib/vst3`, a desktop entry
(`rawdawg --open %f`), MIME types `application/x-rawdawg-project` (the `project.rawdawg` marker
file every saved project folder carries, since folders cannot be associated by suffix) and
`application/x-rawdawg-preset` (`*.rawdawgpreset`), and hicolor icons rendered at package time. The
MSI installs the plugins into the common CLAP/VST3 folders and registers `.rawdawg` (the marker)
and `.rawdawgpreset`. Artifacts follow the `rawdawg-<version>-<arch>` basename. Uninstall never touches user projects or preferences.

Not produced on purpose: AppImage, Snap, Flatpak (CLAUDE.md). Packages are unsigned; signing,
legal publisher identity and name clearance are owner decisions. The `Maintainer` field carries the
display brand only until the owner records a legal identity.
