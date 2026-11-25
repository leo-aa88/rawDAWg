//! Native packaging (PLAT-04, ROADMAP M8): Ubuntu `.deb`, Windows MSI (WiX) and a
//! portable Windows directory/zip. No AppImage, Snap or Flatpak.
//!
//! Every package carries the `rawdawg` binary, the CLAP module and the VST3 bundle,
//! the user guide, the license, a third-party license report and a CycloneDX SBOM
//! generated from `cargo metadata`; `SHA256SUMS` covers the produced artifacts.
//! Legal publisher identity, signing and name clearance remain owner decisions: the
//! packages name the display brand only and are unsigned.

use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DISPLAY_NAME: &str = "rawDAWg";
pub const TAGLINE: &str = "Less thinking. More noise.";
pub const PACKAGE: &str = "rawdawg";
pub const HOMEPAGE: &str = "https://github.com/leonardo-aa/rawDAWg";
/// Maintainer field: the display brand until the owner records a legal publisher identity.
pub const MAINTAINER: &str = "rawDAWg <noreply@rawdawg.invalid>";
pub const MIME_PROJECT: &str = "application/x-rawdawg-project";
pub const MIME_PRESET: &str = "application/x-rawdawg-preset";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Deb,
    Msi,
    Portable,
}

/// What a package run produced.
#[derive(Debug, Default)]
pub struct Produced {
    pub artifacts: Vec<PathBuf>,
    pub staging: PathBuf,
    pub warnings: Vec<String>,
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let status = cmd.status().with_context(|| format!("running {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} failed with {status}");
    }
    Ok(())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in [bin.to_owned(), format!("{bin}.exe")] {
            let p = dir.join(&name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub fn version() -> &'static str {
    rawdawg_types::VERSION
}

/// Release build of the whole workspace (binary + plugin modules), locked.
pub fn build_release(root: &Path) -> anyhow::Result<PathBuf> {
    run(Command::new("cargo").current_dir(root).args(["build", "--workspace", "--release", "--locked"]))?;
    Ok(root.join("target").join("release"))
}

// ---------------------------------------------------------------------------------------------
// shared payload: icon, licenses, SBOM

/// Render the application icon (dark rounded square, bright waveform) at `size` pixels.
pub fn icon_rgba(size: u32) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(size, size);
    let s = size as f32;
    let radius = s * 0.22;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // rounded square mask
            let cx = fx.clamp(radius, s - radius);
            let cy = fy.clamp(radius, s - radius);
            let inside = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() <= radius;
            let mut px = if inside { [0x14, 0x16, 0x1a, 0xff] } else { [0, 0, 0, 0] };
            if inside {
                // waveform: a bright saw-like trace across the middle
                let t = fx / s;
                let wave = 0.5 + 0.28 * ((t * 3.0).fract() * 2.0 - 1.0);
                let dist = (fy / s - wave).abs();
                let thick = 0.045;
                if dist < thick {
                    let a = (1.0 - dist / thick).clamp(0.0, 1.0);
                    let accent: [u8; 3] = [0xff, 0x7a, 0x1a];
                    for (c, &acc) in px.iter_mut().take(3).zip(accent.iter()) {
                        *c = (f32::from(*c) * (1.0 - a) + f32::from(acc) * a) as u8;
                    }
                }
            }
            img.put_pixel(x, y, image::Rgba(px));
        }
    }
    img
}

pub fn write_icon_png(path: &Path, size: u32) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    icon_rgba(size).save_with_format(path, image::ImageFormat::Png).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Windows `.ico` holding PNG-compressed images (supported since Windows Vista).
pub fn write_icon_ico(path: &Path) -> anyhow::Result<()> {
    let sizes = [16u32, 32, 48, 256];
    let mut pngs = Vec::new();
    for s in sizes {
        let mut bytes = Vec::new();
        icon_rgba(s).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
        pngs.push(bytes);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, png) in sizes.iter().zip(&pngs) {
        let dim = if *s >= 256 { 0u8 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        out.extend_from_slice(png);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
}

#[derive(serde::Deserialize, Clone)]
struct MetaPackage {
    name: String,
    version: String,
    license: Option<String>,
    repository: Option<String>,
    source: Option<String>,
    description: Option<String>,
}

fn external_packages(root: &Path) -> anyhow::Result<Vec<MetaPackage>> {
    let out = Command::new("cargo").current_dir(root).args(["metadata", "--format-version", "1", "--locked"]).output().context("cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let meta: Metadata = serde_json::from_slice(&out.stdout)?;
    let mut pk: Vec<MetaPackage> = meta.packages.into_iter().filter(|p| p.source.is_some()).collect();
    pk.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    Ok(pk)
}

/// `THIRD-PARTY-LICENSES.txt`: every external crate with its declared license expression.
pub fn third_party_report(root: &Path) -> anyhow::Result<String> {
    let pk = external_packages(root)?;
    let mut s = format!(
        "{DISPLAY_NAME} {} — third-party components (from Cargo.lock)\n\nDeclared license expressions as published by each crate; the full license texts ship with the crates' sources.\n\n",
        version()
    );
    for p in &pk {
        s.push_str(&format!("{} {}  {}  {}\n", p.name, p.version, p.license.as_deref().unwrap_or("UNKNOWN"), p.repository.as_deref().unwrap_or("")));
    }
    Ok(s)
}

/// Minimal CycloneDX 1.5 JSON SBOM of the dependency set.
pub fn sbom_json(root: &Path) -> anyhow::Result<String> {
    let pk = external_packages(root)?;
    let components: Vec<serde_json::Value> = pk
        .iter()
        .map(|p| {
            serde_json::json!({
                "type": "library",
                "name": p.name,
                "version": p.version,
                "purl": format!("pkg:cargo/{}@{}", p.name, p.version),
                "description": p.description.clone().unwrap_or_default(),
                "licenses": [{ "expression": p.license.clone().unwrap_or_else(|| "UNKNOWN".into()) }],
            })
        })
        .collect();
    let doc = serde_json::json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": { "component": { "type": "application", "name": PACKAGE, "version": version() } },
        "components": components,
    });
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Files shared by every package: docs, licenses, SBOM.
fn write_common_docs(root: &Path, docs: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(docs)?;
    std::fs::copy(root.join("LICENSE"), docs.join("LICENSE"))?;
    std::fs::copy(root.join("docs/USER_GUIDE.md"), docs.join("USER_GUIDE.md"))?;
    std::fs::copy(root.join("README.md"), docs.join("README.md"))?;
    std::fs::write(docs.join("THIRD-PARTY-LICENSES.txt"), third_party_report(root)?)?;
    std::fs::write(docs.join("sbom.cdx.json"), sbom_json(root)?)?;
    Ok(())
}

/// Lay out the VST3 bundle and the CLAP module from the release build into `plugins`.
fn write_plugin_bundles(release: &Path, clap_dir: &Path, vst3_dir: &Path) -> anyhow::Result<()> {
    let (clap_name, vst3_name) = if cfg!(target_os = "windows") { ("rawdawg_clap.dll", "rawdawg_vst3.dll") } else { ("librawdawg_clap.so", "librawdawg_vst3.so") };
    let clap = release.join(clap_name);
    let vst3 = release.join(vst3_name);
    if !clap.is_file() || !vst3.is_file() {
        bail!("plugin modules missing in {} (run cargo build --workspace --release)", release.display());
    }
    std::fs::create_dir_all(clap_dir)?;
    std::fs::copy(&clap, clap_dir.join("rawDAWg.clap"))?;
    let arch = if cfg!(target_os = "windows") { "x86_64-win" } else { "x86_64-linux" };
    let bundle = vst3_dir.join("rawDAWg.vst3").join("Contents");
    std::fs::create_dir_all(bundle.join(arch))?;
    std::fs::create_dir_all(bundle.join("Resources"))?;
    std::fs::copy(&vst3, bundle.join(arch).join(if cfg!(target_os = "windows") { "rawDAWg.vst3" } else { "rawDAWg.so" }))?;
    std::fs::write(bundle.join("Resources").join("moduleinfo.json"), moduleinfo_json()?)?;
    Ok(())
}

/// `moduleinfo.json` for the VST3 bundle (class list from the shared catalogue).
pub fn moduleinfo_json() -> anyhow::Result<String> {
    let plugins = rawdawg_plugin_api::exported_plugins();
    let classes: Vec<serde_json::Value> = plugins
        .iter()
        .map(|p| {
            let (pc, _) = rawdawg_vst3::class_ids(p);
            serde_json::json!({
                "CID": pc.iter().map(|b| format!("{:02X}", *b as u8)).collect::<String>(),
                "Category": "Audio Module Class",
                "Name": p.name,
                "Vendor": DISPLAY_NAME,
                "Version": version(),
                "SDKVersion": rawdawg_vst3::SDK_VERSION,
                "Sub Categories": p.vst3_subcategories().split('|').collect::<Vec<_>>(),
                "Class Flags": 0,
                "Cardinality": 2147483647
            })
        })
        .collect();
    let doc = serde_json::json!({
        "Name": DISPLAY_NAME,
        "Version": version(),
        "Factory Info": { "Vendor": DISPLAY_NAME, "URL": HOMEPAGE, "E-Mail": "", "Flags": { "Unicode": true, "Classes Discardable": false, "Component Non Discardable": false } },
        "Compatibility": [],
        "Classes": classes
    });
    Ok(serde_json::to_string_pretty(&doc)?)
}

pub fn desktop_entry() -> String {
    // Projects associate through the `project.rawdawg` marker each saved project carries.
    format!(
        "[Desktop Entry]\nType=Application\nName={DISPLAY_NAME}\nGenericName=Digital Audio Workstation\nComment={TAGLINE}\nExec={PACKAGE} --open %f\nTryExec={PACKAGE}\nIcon={PACKAGE}\nTerminal=false\nCategories=AudioVideo;Audio;Music;\nMimeType={MIME_PROJECT};{MIME_PRESET};\nKeywords=DAW;synth;music;audio;\nStartupWMClass={PACKAGE}\n"
    )
}

pub fn mime_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="{MIME_PROJECT}">
    <comment>{DISPLAY_NAME} project</comment>
    <glob pattern="project.rawdawg" weight="80"/>
    <glob pattern="*.rawdawg"/>
  </mime-type>
  <mime-type type="{MIME_PRESET}">
    <comment>{DISPLAY_NAME} preset</comment>
    <sub-class-of type="application/zip"/>
    <glob pattern="*.rawdawgpreset"/>
  </mime-type>
</mime-info>
"#
    )
}

/// Debian control file.
pub fn deb_control(installed_kib: u64) -> String {
    format!(
        "Package: {PACKAGE}\nVersion: {ver}\nSection: sound\nPriority: optional\nArchitecture: amd64\nMaintainer: {MAINTAINER}\nInstalled-Size: {installed_kib}\nDepends: libc6 (>= 2.34), libgcc-s1, libasound2t64 (>= 1.2) | libasound2 (>= 1.2)\nRecommends: pipewire-alsa | pulseaudio\nSuggests: jackd2\nHomepage: {HOMEPAGE}\nDescription: {DISPLAY_NAME} — native experimental DAW\n {TAGLINE} A native Rust digital audio workstation with its own\n synthesizer and effect suite, CLAP/VST3 hosting and CLAP/VST3 exports\n of its instruments and effects. Offline, dark only.\n",
        ver = version()
    )
}

fn dir_size(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let p = entry?.path();
        total += if p.is_dir() { dir_size(&p)? } else { p.metadata()?.len() };
    }
    Ok(total)
}

fn checksums(dir: &Path, files: &[PathBuf]) -> anyhow::Result<PathBuf> {
    use std::io::Read;
    let mut text = String::new();
    for f in files {
        let mut h = Sha256::new();
        let mut file = std::fs::File::open(f)?;
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        text.push_str(&format!("{}  {}\n", h.hex(), f.file_name().unwrap().to_string_lossy()));
    }
    let path = dir.join("SHA256SUMS");
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Build the Ubuntu `.deb` with `dpkg-deb` (no cargo-deb dependency). Returns the artifacts.
pub fn package_deb(root: &Path) -> anyhow::Result<Produced> {
    if !cfg!(target_os = "linux") {
        bail!("deb packaging runs on Ubuntu only");
    }
    let dpkg = which("dpkg-deb").ok_or_else(|| anyhow::anyhow!("dpkg-deb not found (install dpkg)"))?;
    let release = build_release(root)?;
    let out_dir = root.join("target").join("package");
    let name = format!("{PACKAGE}-{}-x86_64", version());
    let staging = out_dir.join(&name);
    let _ = std::fs::remove_dir_all(&staging);
    let usr = staging.join("usr");
    std::fs::create_dir_all(usr.join("bin"))?;
    std::fs::copy(release.join(PACKAGE), usr.join("bin").join(PACKAGE))?;
    write_plugin_bundles(&release, &usr.join("lib").join("clap"), &usr.join("lib").join("vst3"))?;
    let apps = usr.join("share").join("applications");
    std::fs::create_dir_all(&apps)?;
    std::fs::write(apps.join(format!("{PACKAGE}.desktop")), desktop_entry())?;
    let mime = usr.join("share").join("mime").join("packages");
    std::fs::create_dir_all(&mime)?;
    std::fs::write(mime.join(format!("{PACKAGE}.xml")), mime_xml())?;
    for size in [16u32, 32, 48, 64, 128, 256] {
        write_icon_png(&usr.join("share").join("icons").join("hicolor").join(format!("{size}x{size}")).join("apps").join(format!("{PACKAGE}.png")), size)?;
    }
    let docs = usr.join("share").join("doc").join(PACKAGE);
    write_common_docs(root, &docs)?;
    std::fs::write(
        docs.join("copyright"),
        format!(
            "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\nUpstream-Name: {DISPLAY_NAME}\nSource: {HOMEPAGE}\n\nFiles: *\nCopyright: rawDAWg contributors\nLicense: AGPL-3.0-only\n See /usr/share/doc/{PACKAGE}/LICENSE (as placed by the repository owner; legal publisher identity is an owner decision).\n"
        ),
    )?;
    // the pinned fixtures double as demo projects
    let demos = usr.join("share").join(PACKAGE).join("demos");
    copy_tree(&root.join("fixtures").join("demo-song.rawdawg"), &demos.join("demo-song.rawdawg"))?;
    let elf = vec![
        usr.join("bin").join(PACKAGE),
        usr.join("lib").join("clap").join("rawDAWg.clap"),
        usr.join("lib").join("vst3").join("rawDAWg.vst3").join("Contents").join("x86_64-linux").join("rawDAWg.so"),
    ];
    let stripped = strip_debug(&elf)?;
    let debian = staging.join("DEBIAN");
    std::fs::create_dir_all(&debian)?;
    let installed_kib = dir_size(&usr)?.div_ceil(1024);
    std::fs::write(debian.join("control"), deb_control(installed_kib))?;
    // executables and directories get conventional modes
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(usr.join("bin").join(PACKAGE), std::fs::Permissions::from_mode(0o755))?;
    }
    let deb = out_dir.join(format!("{name}.deb"));
    let _ = std::fs::remove_file(&deb);
    run(Command::new(&dpkg).args(["--build", "--root-owner-group"]).arg(&staging).arg(&deb))?;
    let mut produced = Produced { artifacts: vec![deb.clone()], staging: staging.clone(), warnings: Vec::new() };
    if !stripped {
        produced.warnings.push("strip not found: binaries ship with debug line tables".into());
    }
    if let Some(v) = which("desktop-file-validate") {
        let st = Command::new(v).arg(apps.join(format!("{PACKAGE}.desktop"))).status()?;
        if !st.success() {
            bail!("desktop entry failed validation");
        }
    } else {
        produced.warnings.push("desktop-file-validate not installed: desktop entry not validated".into());
    }
    match which("lintian") {
        Some(l) => {
            let out = Command::new(l)
                .args(["--no-tag-display-limit", "--suppress-tags", "no-manual-page,changelog-file-missing,extended-description-is-probably-too-short"])
                .arg(&deb)
                .output()?;
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            std::fs::write(out_dir.join("lintian.txt"), &text)?;
            if text.lines().any(|l| l.starts_with("E:")) {
                bail!("lintian reported errors:\n{text}");
            }
            if !text.trim().is_empty() {
                produced.warnings.push(format!("lintian warnings recorded in {}", out_dir.join("lintian.txt").display()));
            }
        }
        None => produced.warnings.push("lintian not installed: package not linted".into()),
    }
    produced.artifacts.push(checksums(&out_dir, &[deb])?);
    Ok(produced)
}

/// Strip debug sections from staged ELF files (the release profile keeps line tables for
/// profiling; packages ship without them). Returns false when no `strip` is available.
fn strip_debug(files: &[PathBuf]) -> anyhow::Result<bool> {
    if !cfg!(target_os = "linux") {
        return Ok(true); // Windows debug info lives in separate PDBs, not in the binaries
    }
    let Some(strip) = which("strip") else { return Ok(false) };
    for f in files {
        run(Command::new(&strip).arg("--strip-debug").arg(f))?;
    }
    Ok(true)
}

fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let p = entry?.path();
        let to = dst.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_tree(&p, &to)?;
        } else {
            std::fs::copy(&p, &to)?;
        }
    }
    Ok(())
}

/// Windows payload directory shared by the MSI and the portable zip.
fn windows_payload(root: &Path, release: &Path, dir: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    std::fs::copy(release.join(format!("{PACKAGE}.exe")), dir.join(format!("{PACKAGE}.exe")))?;
    write_plugin_bundles(release, &dir.join("plugins").join("CLAP"), &dir.join("plugins").join("VST3"))?;
    write_common_docs(root, &dir.join("docs"))?;
    write_icon_png(&dir.join(format!("{PACKAGE}.png")), 256)?;
    write_icon_ico(&dir.join(format!("{PACKAGE}.ico")))?;
    copy_tree(&root.join("fixtures").join("demo-song.rawdawg"), &dir.join("demos").join("demo-song.rawdawg"))?;
    Ok(())
}

/// Windows MSI through the WiX v4/v5 CLI (`wix`), from `packaging/windows/rawdawg.wxs`.
pub fn package_msi(root: &Path) -> anyhow::Result<Produced> {
    if !cfg!(target_os = "windows") {
        bail!("msi packaging runs on Windows only");
    }
    let wix = which("wix").ok_or_else(|| anyhow::anyhow!("WiX CLI not found: `dotnet tool install --global wix` (pinned version in .github/workflows/ci.yml)"))?;
    let release = build_release(root)?;
    let out_dir = root.join("target").join("package");
    let payload = out_dir.join("windows-payload");
    windows_payload(root, &release, &payload)?;
    let msi = out_dir.join(format!("{PACKAGE}-{}-x86_64.msi", version()));
    run(Command::new(wix)
        .args(["build", "-arch", "x64", "-o"])
        .arg(&msi)
        .arg("-d")
        .arg(format!("Payload={}", payload.display()))
        .arg("-d")
        .arg(format!("Version={}", version()))
        .arg(root.join("packaging").join("windows").join("rawdawg.wxs")))?;
    let mut produced = Produced { artifacts: vec![msi.clone()], staging: payload, warnings: vec!["unsigned MSI (signing is an owner decision)".into()] };
    produced.artifacts.push(checksums(&out_dir, &[msi])?);
    Ok(produced)
}

/// Portable Windows directory + zip (no installer, no registry).
pub fn package_portable(root: &Path) -> anyhow::Result<Produced> {
    if !cfg!(target_os = "windows") {
        bail!("portable packaging runs on Windows only");
    }
    let release = build_release(root)?;
    let out_dir = root.join("target").join("package");
    let dir = out_dir.join(format!("{PACKAGE}-{}-x86_64", version()));
    windows_payload(root, &release, &dir)?;
    std::fs::write(
        dir.join("PORTABLE.txt"),
        format!(
            "{DISPLAY_NAME} portable build. Run {PACKAGE}.exe from this folder; preferences live in %APPDATA%\\{PACKAGE}. Copy plugins\\CLAP and plugins\\VST3 into your plugin folders to use the exports in other hosts.\n"
        ),
    )?;
    let zip = out_dir.join(format!("{PACKAGE}-{}-x86_64.zip", version()));
    let _ = std::fs::remove_file(&zip);
    run(Command::new("powershell").args(["-NoProfile", "-Command"]).arg(format!("Compress-Archive -Path '{}' -DestinationPath '{}' -Force", dir.display(), zip.display())))?;
    let mut produced = Produced { artifacts: vec![zip.clone()], staging: dir, warnings: Vec::new() };
    produced.artifacts.push(checksums(&out_dir, &[zip])?);
    Ok(produced)
}

pub fn package(root: &Path, format: Format) -> anyhow::Result<Produced> {
    match format {
        Format::Deb => package_deb(root),
        Format::Msi => package_msi(root),
        Format::Portable => package_portable(root),
    }
}

// ---------------------------------------------------------------------------------------------
// SHA-256 (self-contained; keeps xtask free of an extra dependency)

struct Sha256 {
    state: [u32; 8],
    buf: Vec<u8>,
    len: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Self { state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19], buf: Vec::new(), len: 0 }
    }
    fn update(&mut self, data: &[u8]) {
        self.len += data.len() as u64;
        self.buf.extend_from_slice(data);
        while self.buf.len() >= 64 {
            let block: [u8; 64] = self.buf[..64].try_into().unwrap();
            self.compress(&block);
            self.buf.drain(..64);
        }
    }
    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
    fn hex(mut self) -> String {
        let bits = self.len * 8;
        let mut pad = vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_be_bytes());
        let total = self.len;
        self.update(&pad);
        self.len = total;
        self.state.iter().map(|w| format!("{w:08x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(h.hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let mut h = Sha256::new();
        h.update(b"");
        assert_eq!(h.hex(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let mut h = Sha256::new();
        let data = vec![b'a'; 1_000_000];
        h.update(&data[..500_000]);
        h.update(&data[500_000..]);
        assert_eq!(h.hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn desktop_entry_and_mime_name_the_product() {
        let d = desktop_entry();
        assert!(d.contains("Name=rawDAWg") && d.contains("Exec=rawdawg --open %f") && d.contains(MIME_PROJECT) && d.contains(MIME_PRESET));
        let m = mime_xml();
        assert!(m.contains("project.rawdawg") && m.contains("*.rawdawgpreset"));
        let c = deb_control(1234);
        assert!(c.starts_with("Package: rawdawg\n") && c.contains("Architecture: amd64") && c.contains("Installed-Size: 1234"));
        assert!(!m.contains("project.json"), "never claim generic project.json files");
        let icon = icon_rgba(32);
        assert_eq!(icon.get_pixel(0, 0).0[3], 0, "corners are transparent");
        assert_eq!(icon.get_pixel(16, 16).0[3], 255, "centre is opaque");
        let dir = std::env::temp_dir().join(format!("rawdawg-ico-{}", std::process::id()));
        let ico = dir.join("rawdawg.ico");
        write_icon_ico(&ico).unwrap();
        let bytes = std::fs::read(&ico).unwrap();
        assert_eq!(&bytes[..6], &[0, 0, 1, 0, 4, 0], "ICO header with four images");
        let first_offset = u32::from_le_bytes(bytes[18..22].try_into().unwrap()) as usize;
        assert_eq!(&bytes[first_offset..first_offset + 8], b"\x89PNG\r\n\x1a\n", "PNG-compressed entries");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
