# Packaging

`cargo xtask package --format <deb|msi|portable>` builds the native packages (PLAT-04):

| Format | OS | Tool | Output |
|---|---|---|---|
| `deb` | Ubuntu 24.04 | `dpkg-deb`, `strip`, `gzip`, `md5sum` (+ `desktop-file-validate`; `lintian` when installed, which must report no errors) | `target/package/rawdawg-<version>-x86_64.deb` |
| `msi` | Windows 11 | WiX v5 CLI (`dotnet tool install --global wix --version 5.0.2`), manifest `windows/rawdawg.wxs` | `target/package/rawdawg-<version>-x86_64.msi` |
| `portable` | Windows 11 | PowerShell `Compress-Archive` | `target/package/rawdawg-<version>-x86_64.zip` |

Every package contains the `rawdawg` binary, the CLAP module (`rawDAWg.clap`), the VST3 bundle
(`rawDAWg.vst3/Contents/<arch>/…` + `moduleinfo.json`), the user guide, `LICENSE`, a third-party
license report and a CycloneDX SBOM generated from `Cargo.lock`; `SHA256SUMS` lists the artifacts.
The deb installs the plugins into `/usr/lib/clap` and `/usr/lib/vst3`, a desktop entry
(`rawdawg --open %f`), MIME types `application/x-rawdawg-project` (the `project.rawdawg` marker
file every saved project folder carries, since folders cannot be associated by suffix) and
`application/x-rawdawg-preset` (`*.rawdawgpreset`), and hicolor icons rendered at package time,
a `rawdawg(1)` man page generated from `rawdawg --help`, a Debian changelog, a machine-readable
`copyright` carrying the full AGPL text, and `md5sums`. Binaries are stripped like `dh_strip`. The
MSI installs the plugins into the common CLAP/VST3 folders and registers `.rawdawg` (the marker)
and `.rawdawgpreset`. Artifacts follow the `rawdawg-<version>-<arch>` basename. Uninstall never touches user projects or preferences.

Not produced on purpose: AppImage, Snap, Flatpak (CLAUDE.md). Packages are unsigned; signing,
legal publisher identity and name clearance are owner decisions. The `Maintainer` field carries the
display brand only until the owner records a legal identity.

## Releases

`.github/workflows/release.yml` builds these packages for a release: run it from the Actions tab
(choosing a semver bump) or push a `vX.Y.Z` tag. It only releases commits with a green CI run,
stamps the tag's version into the workspace (`rawdawg --version`, package metadata and file names
match the tag), smoke-tests each package and publishes them with `SHA256SUMS.txt` on a GitHub
Release. Pre-release tags (`v0.2.0-rc.1`) become Debian `0.2.0~rc.1` and MSI version `0.2.0`.
