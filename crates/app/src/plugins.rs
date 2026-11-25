//! Third-party plugin wiring for the app: catalog location, scanner options,
//! crash marker and the `--scan-plugins` CLI (PLUGIN_API.md §3).

use rawdawg_plugin_host::scan::ScanOptions;
use rawdawg_plugin_host::{Catalog, Format};
use std::path::PathBuf;

/// Per-user catalog file (preferences directory), or the override given on the CLI.
pub fn catalog_path(override_path: Option<PathBuf>) -> Option<PathBuf> {
    override_path.or_else(|| directories::ProjectDirs::from("", "", rawdawg_types::PACKAGE_NAME).map(|d| d.config_dir().join("plugins.json")))
}

/// Scanner children run this very executable with `--scan-plugin`.
pub fn scan_options(timeout_secs: u64) -> anyhow::Result<ScanOptions> {
    let exe = std::env::current_exe()?;
    Ok(ScanOptions::new(exe, timeout_secs))
}

#[derive(serde::Serialize)]
struct CliReport<'a> {
    catalog: &'a std::path::Path,
    candidates: usize,
    scanned: usize,
    quarantined: usize,
    unchanged: usize,
    pruned: usize,
    plugins: Vec<CliPlugin>,
    quarantine: Vec<CliQuarantine>,
}

#[derive(serde::Serialize)]
struct CliPlugin {
    format: Format,
    id: String,
    name: String,
    vendor: String,
    version: String,
    path: PathBuf,
    instrument: bool,
    effect: bool,
    gui: bool,
}

#[derive(serde::Serialize)]
struct CliQuarantine {
    path: PathBuf,
    reason: String,
}

/// `rawdawg --scan-plugins [--catalog FILE]`: refresh the CLAP catalog and print a JSON report.
pub fn scan_cli(catalog_override: Option<PathBuf>) -> anyhow::Result<i32> {
    let path = catalog_path(catalog_override).ok_or_else(|| anyhow::anyhow!("no configuration directory available"))?;
    let mut catalog = Catalog::load(&path);
    let opts = scan_options(catalog.timeout_secs)?;
    let report = rawdawg_plugin_host::scan::refresh(&mut catalog, Format::Clap, &opts, |p, i, n| eprintln!("scanning {}/{}: {}", i + 1, n, p.display()));
    catalog.save(&path).map_err(|e| anyhow::anyhow!("saving catalog {}: {e}", path.display()))?;
    let out = CliReport {
        catalog: &path,
        candidates: report.candidates,
        scanned: report.scanned,
        quarantined: report.quarantined,
        unchanged: report.unchanged,
        pruned: report.pruned,
        plugins: catalog
            .plugins()
            .map(|(l, p)| CliPlugin {
                format: p.format,
                id: p.plugin_id.clone(),
                name: p.name.clone(),
                vendor: p.vendor.clone(),
                version: p.version.clone(),
                path: l.path.clone(),
                instrument: p.is_instrument,
                effect: p.is_effect,
                gui: p.has_gui,
            })
            .collect(),
        quarantine: catalog.quarantine.iter().map(|q| CliQuarantine { path: q.path.clone(), reason: q.reason.summary() }).collect(),
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(0)
}
