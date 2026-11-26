//! Third-party plugin wiring for the app (PLUGIN_API.md §3): catalog location and
//! persistence, background scans through the isolated scanner, crash marker, live
//! CLAP instances as main-thread mirrors, and the engine's externals provider.

use rawdawg_audio_engine::externals::{ExternalPlugins, PluginRole};
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_plugin_host::clap::{ClapInstance, ModuleCache};
use rawdawg_plugin_host::hosted_mut;
use rawdawg_plugin_host::marker::Marker;
use rawdawg_plugin_host::scan::{ScanOptions, ScanReport};
use rawdawg_plugin_host::vst3::{Vst3Instance, Vst3ModuleCache};
use rawdawg_plugin_host::{Catalog, Format};
use rawdawg_project::{Instance, PluginFormat};
use rawdawg_types::InstanceId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

/// Per-user catalog file (preferences directory), or the override given on the CLI.
pub fn catalog_path(override_path: Option<PathBuf>) -> Option<PathBuf> {
    override_path.or_else(|| directories::ProjectDirs::from("", "", rawdawg_types::PACKAGE_NAME).map(|d| d.config_dir().join("plugins.json")))
}

/// Crash marker next to the catalog.
pub fn marker_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", rawdawg_types::PACKAGE_NAME).map(|d| d.config_dir().join("plugins-live.json"))
}

/// Scanner children run this very executable with `--scan-plugin`.
pub fn scan_options(timeout_secs: u64) -> anyhow::Result<ScanOptions> {
    let exe = std::env::current_exe()?;
    Ok(ScanOptions::new(exe, timeout_secs))
}

pub enum ScanMsg {
    Progress { path: PathBuf, index: usize, total: usize },
    Done { catalog: Catalog, report: ScanReport },
}

pub struct ScanJob {
    rx: Receiver<ScanMsg>,
    pub current: String,
}

/// The app's plugin host: catalog, loaded modules, scan state and crash marker.
pub struct PluginHost {
    pub catalog: Catalog,
    pub catalog_path: Option<PathBuf>,
    pub modules: ModuleCache,
    pub vst3_modules: Vst3ModuleCache,
    pub safe_mode: bool,
    /// Persist the catalog and marker (false for headless tests).
    pub persist: bool,
    pub scan: Option<ScanJob>,
    pub last_report: Option<ScanReport>,
    pub last_error: Option<String>,
    /// Plugins that were live when the previous session ended without a clean shutdown.
    pub previous_crash: Option<Vec<String>>,
    marker_written: bool,
}

impl PluginHost {
    pub fn load(safe_mode: bool, persist: bool, catalog_override: Option<PathBuf>) -> Self {
        let catalog_path = if persist || catalog_override.is_some() { catalog_path(catalog_override) } else { None };
        let catalog = catalog_path.as_deref().map(Catalog::load).unwrap_or_default();
        let previous_crash = if persist { marker_path().and_then(|p| Marker::read(&p)).map(|m| m.plugins) } else { None };
        Self {
            catalog,
            catalog_path,
            modules: ModuleCache::default(),
            vst3_modules: Vst3ModuleCache::default(),
            safe_mode,
            persist,
            scan: None,
            last_report: None,
            last_error: None,
            previous_crash,
            marker_written: false,
        }
    }

    pub fn save(&self) {
        if let Some(p) = &self.catalog_path
            && let Err(e) = self.catalog.save(p)
        {
            log::warn!("could not save the plugin catalog: {e}");
        }
    }

    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Start a catalog refresh on a worker thread (each module in its own child process).
    pub fn start_scan(&mut self) {
        if self.scan.is_some() {
            return;
        }
        let opts = match scan_options(self.catalog.timeout_secs) {
            Ok(o) => o,
            Err(e) => {
                self.last_error = Some(format!("cannot start the scanner: {e}"));
                return;
            }
        };
        let mut catalog = self.catalog.clone();
        let (tx, rx) = channel();
        let progress_tx = tx.clone();
        std::thread::Builder::new()
            .name("plugin-scan".into())
            .spawn(move || {
                let mut report = ScanReport::default();
                for format in [Format::Clap, Format::Vst3] {
                    let r = rawdawg_plugin_host::scan::refresh(&mut catalog, format, &opts, |p, i, n| {
                        let _ = progress_tx.send(ScanMsg::Progress { path: p.to_path_buf(), index: i, total: n });
                    });
                    report.candidates += r.candidates;
                    report.scanned += r.scanned;
                    report.quarantined += r.quarantined;
                    report.unchanged += r.unchanged;
                    report.pruned += r.pruned;
                }
                let _ = tx.send(ScanMsg::Done { catalog, report });
            })
            .ok();
        self.scan = Some(ScanJob { rx, current: "starting…".into() });
        self.last_error = None;
    }

    /// Drain scan messages; returns true when the catalog changed.
    pub fn poll(&mut self) -> bool {
        let Some(job) = self.scan.as_mut() else { return false };
        let mut changed = false;
        let mut done = false;
        while let Ok(msg) = job.rx.try_recv() {
            match msg {
                ScanMsg::Progress { path, index, total } => {
                    job.current = format!("{}/{} {}", index + 1, total, path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                }
                ScanMsg::Done { catalog, report } => {
                    // keep user edits made meanwhile (paths, retries) by re-applying them on top
                    let user_paths = self.catalog.user_paths.clone();
                    let timeout = self.catalog.timeout_secs;
                    self.catalog = catalog;
                    self.catalog.user_paths = user_paths;
                    self.catalog.timeout_secs = timeout;
                    self.last_report = Some(report);
                    changed = true;
                    done = true;
                }
            }
        }
        if done {
            self.scan = None;
            self.save();
        }
        changed
    }

    /// Catalog entry for a project instance (format + stable id, path hint preferred).
    pub fn lookup(&self, inst: &Instance) -> Option<(&rawdawg_plugin_host::ScannedLibrary, &rawdawg_plugin_host::PluginDescription)> {
        let format = match inst.format {
            PluginFormat::Clap => Format::Clap,
            PluginFormat::Vst3 => Format::Vst3,
            PluginFormat::Native => return None,
        };
        self.catalog.find(format, inst.type_id.as_str(), inst.path_hint.as_deref().map(std::path::Path::new))
    }

    /// Why an instance cannot be hosted right now, or `None` when it can.
    pub fn unavailable_reason(&self, inst: &Instance) -> Option<String> {
        if self.safe_mode {
            return Some("safe mode: third-party plugins are not loaded".into());
        }
        match inst.format {
            PluginFormat::Native => None,
            PluginFormat::Clap | PluginFormat::Vst3 => {
                // a saved state that could not be read back from plugin-state/ keeps the instance
                // a stand-in: hosting it with defaults would overwrite the saved state on the next edit
                if let Some(file) = rawdawg_project::plugin_state::unresolved_reference(&inst.state) {
                    return Some(format!("its saved state file {file} is missing or damaged; the stand-in keeps the reference until the file is back"));
                }
                if self.lookup(inst).is_some() {
                    None
                } else if let Some(q) = inst.path_hint.as_deref().and_then(|p| self.catalog.quarantined(std::path::Path::new(p))) {
                    Some(format!("quarantined: {}", q.reason.summary()))
                } else {
                    Some("not in the plugin catalog (scan your plugin folders in Settings › Plugins)".into())
                }
            }
        }
    }

    /// Create the main-thread instance for a project instance and load its saved state.
    pub fn create_instance(&mut self, inst: &Instance) -> Result<Box<dyn Processor>, String> {
        if let Some(reason) = self.unavailable_reason(inst) {
            return Err(reason);
        }
        let (path, desc) = self.lookup(inst).map(|(l, d)| (l.path.clone(), d.clone())).ok_or_else(|| "not in the plugin catalog".to_owned())?;
        // SAFETY: the module passed the isolated scan and the user placed it in the project.
        let mut live: Box<dyn Processor> = match desc.format {
            Format::Clap => Box::new(ClapInstance::new(unsafe { self.modules.load(&path) }?, &desc)?),
            Format::Vst3 => Box::new(Vst3Instance::new(unsafe { self.vst3_modules.load(&path) }?, &desc)?),
        };
        if !inst.state.is_null() {
            let bytes = serde_json::to_vec(&inst.state).map_err(|e| e.to_string())?;
            // PLUGIN_API.md §3: a state that fails to restore keeps its bytes and shows a
            // placeholder, instead of a live plugin with defaults that a later edit would save
            live.load_state(&bytes).map_err(|e| format!("its saved state could not be restored ({e}); the stand-in keeps the state"))?;
        }
        Ok(live)
    }

    /// Write/refresh the crash marker with the live plugin ids (clean exit clears it).
    pub fn mark_live(&mut self, live: Vec<String>) {
        if !self.persist {
            return;
        }
        let Some(p) = marker_path() else { return };
        if live.is_empty() {
            if self.marker_written {
                Marker::clear(&p);
                self.marker_written = false;
            }
            return;
        }
        let marker = Marker { plugins: live, written_at_unix: rawdawg_plugin_host::catalog::now_unix() };
        if marker.write(&p).is_ok() {
            self.marker_written = true;
        }
    }

    /// Clean shutdown: no crash to report next time.
    pub fn clear_marker(&mut self) {
        if self.persist
            && let Some(p) = marker_path()
        {
            Marker::clear(&p);
            self.marker_written = false;
        }
    }

    pub fn acknowledge_previous_crash(&mut self) {
        self.previous_crash = None;
        self.clear_marker();
    }
}

/// Make sure `mirrors` holds the live main-thread instance for an external `inst`.
pub fn ensure_external_mirror(host: &mut PluginHost, mirrors: &mut HashMap<InstanceId, Box<dyn Processor>>, inst: &Instance) -> Result<(), String> {
    if mirrors.get(&inst.id).is_some_and(|p| rawdawg_plugin_host::is_hosted(p.as_ref())) {
        return Ok(());
    }
    let live = host.create_instance(inst)?;
    mirrors.insert(inst.id, live);
    Ok(())
}

/// Engine provider backed by the app's host and mirror map (borrowed for one compile).
pub struct AppExternals<'a> {
    pub host: &'a mut PluginHost,
    pub mirrors: &'a mut HashMap<InstanceId, Box<dyn Processor>>,
    pub live_fingerprints: &'a HashMap<InstanceId, u64>,
    /// Instances that could not be hosted this time, with the reason.
    pub unavailable: Vec<String>,
    /// Plugin ids that are live after this compile.
    pub live_ids: Vec<String>,
}

impl ExternalPlugins for AppExternals<'_> {
    fn instantiate(&mut self, inst: &Instance, _role: PluginRole, spec: &PrepareSpec) -> Result<Option<Box<dyn Processor>>, String> {
        let name = if inst.display_name.is_empty() { inst.type_id.as_str().to_owned() } else { inst.display_name.clone() };
        if let Err(reason) = ensure_external_mirror(self.host, self.mirrors, inst) {
            self.unavailable.push(format!("{name}: {reason}"));
            return Ok(None);
        }
        let Some(live) = self.mirrors.get_mut(&inst.id).and_then(|p| hosted_mut(p.as_mut())) else {
            return Ok(None);
        };
        // the project's stored state changed underneath the live instance (undo, preset load)
        let fingerprint = rawdawg_audio_engine::fingerprint_state(inst.type_id.as_str(), &inst.state);
        if !inst.state.is_null() && self.live_fingerprints.get(&inst.id) != Some(&fingerprint) && !inst.state.as_object().is_some_and(|o| o.is_empty()) {
            let bytes = serde_json::to_vec(&inst.state).unwrap_or_default();
            if let Err(e) = live.load_state(&bytes) {
                log::warn!("{name}: plugin state not applied: {e}");
            }
        }
        match live.activate_for_rt(spec) {
            Ok(p) => {
                self.live_ids.push(live.live_id());
                Ok(Some(p))
            }
            Err(e) => {
                self.unavailable.push(format!("{name}: {e}"));
                Ok(None)
            }
        }
    }
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
    let mut report = ScanReport::default();
    for format in [Format::Clap, Format::Vst3] {
        let r = rawdawg_plugin_host::scan::refresh(&mut catalog, format, &opts, |p, i, n| eprintln!("scanning {} {}/{}: {}", format.label(), i + 1, n, p.display()));
        report.candidates += r.candidates;
        report.scanned += r.scanned;
        report.quarantined += r.quarantined;
        report.unchanged += r.unchanged;
        report.pruned += r.pruned;
    }
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
