//! The receipt printer an operator configured, and where that is kept.
//!
//! Device configuration, not business record: which printer is on this desk
//! says nothing about what was sold, so it does not belong in the sales
//! database. It lives beside the lock and the token in the runtime directory,
//! which is already where this installation's operational state is kept, and
//! it is owned by the **daemon** — the composition root is still the only
//! thing that knows what a driver or a queue is (ADR 039).
//!
//! ```text
//! <db-dir>/.pi-edge/printer.json
//! ```
//!
//! Absent means "nobody has configured a printer here", which falls back to
//! the environment exactly as before.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use edge_hardware::{EscPosSpoolerPrinter, PrinterPort, UnconfiguredPrinter};
use serde::{Deserialize, Serialize};

/// A receipt printer model the kiosk offers as a preset.
///
/// A preset is paper and a name, not a driver: all three are ESC/POS over the
/// host spooler, which is the whole point of having one print engine. Adding a
/// model is adding a row here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterModel {
    /// Stable identifier the UI sends back.
    pub id: &'static str,
    /// What an operator sees.
    pub label: &'static str,
    /// Paper width in millimetres, for the UI.
    pub paper_mm: u32,
    /// Characters per line, which is what the document composer needs.
    pub columns: usize,
}

/// Every model this build offers. All ESC/POS, all through the spooler.
pub const MODELS: [PrinterModel; 3] = [
    PrinterModel {
        id: "RPT004",
        label: "3nStar RPT004",
        paper_mm: 80,
        columns: 48,
    },
    PrinterModel {
        id: "RPT008",
        label: "3nStar RPT008",
        paper_mm: 80,
        columns: 48,
    },
    PrinterModel {
        id: "RPT015",
        label: "3nStar RPT015",
        paper_mm: 80,
        columns: 48,
    },
];

pub fn model(id: &str) -> Option<PrinterModel> {
    MODELS.into_iter().find(|entry| entry.id == id)
}

/// What an operator chose on the admin screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterSettings {
    /// Off means this kiosk drives no printer, whatever else is stored.
    #[serde(default)]
    pub enabled: bool,
    /// A model id from [`MODELS`], or empty while nothing is chosen.
    #[serde(default)]
    pub model: String,
    /// The host print queue. Opaque to everything above the composition root.
    #[serde(default)]
    pub queue: String,
    /// Characters per line. Defaults from the model, overridable.
    #[serde(default)]
    pub columns: usize,
    /// Put a receipt out by itself when a sale finalizes.
    #[serde(default)]
    pub auto_print: bool,
}

impl Default for PrinterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            queue: String::new(),
            columns: 48,
            auto_print: false,
        }
    }
}

impl PrinterSettings {
    /// Whether this is enough to actually print.
    pub fn is_configured(&self) -> bool {
        self.enabled && !self.queue.trim().is_empty() && model(&self.model).is_some()
    }

    /// Reject what cannot be stored, before it is stored. A queue name is the
    /// operator's own and goes to `lp -d`, so it may not be an option flag or
    /// carry a separator.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if model(&self.model).is_none() {
            return Err(format!(
                "`{}` is not a printer model this build offers",
                self.model
            ));
        }
        let queue = self.queue.trim();
        if queue.is_empty() {
            return Err("choose a printer".to_owned());
        }
        // A real queue is called "Microsoft Print to PDF" or "3nStar RPT008
        // (Copy 1)". Spaces are ordinary and rejecting them rejects most of
        // the printers people actually have.
        //
        // What is still refused is a name that could be read as an *option*
        // rather than a name. On Windows the question does not arise — the
        // name is handed to `OpenPrinterW` as a string, never parsed — and on
        // Unix `Command::arg` passes it to `lp` without a shell, so the only
        // real hazard is a leading `-` landing in `lp`'s own flag parsing.
        if queue.starts_with('-') {
            return Err(format!(
                "`{queue}` starts with `-`, which a spooler would read as an option"
            ));
        }
        // Control characters are never part of a queue name and would corrupt
        // any log or argument list they reached.
        if queue.chars().any(char::is_control) {
            return Err("a printer name cannot contain control characters".to_owned());
        }
        if !(24..=96).contains(&self.columns) {
            return Err(format!(
                "{} characters per line is outside 24-96",
                self.columns
            ));
        }
        Ok(())
    }

    /// The adapter these settings select. Unconfigured when they do not
    /// select one — never a simulator, which stays opt-in through the
    /// environment (ADR 038).
    pub fn printer(&self) -> Arc<dyn PrinterPort> {
        if self.is_configured() {
            Arc::new(EscPosSpoolerPrinter::new(self.queue.trim()))
        } else {
            Arc::new(UnconfiguredPrinter)
        }
    }
}

fn file(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("printer.json")
}

/// Read what was stored. A missing or unreadable file is "nothing configured"
/// rather than a reason not to start: a kiosk must come up so its admin screen
/// can fix it.
pub fn load(runtime_dir: &Path) -> PrinterSettings {
    let path = file(runtime_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return PrinterSettings::default();
    };
    match serde_json::from_str(&raw) {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "the stored printer settings are unreadable; starting with none"
            );
            PrinterSettings::default()
        }
    }
}

/// Write them, atomically, so a crash mid-write cannot leave half a file.
pub fn save(runtime_dir: &Path, settings: &PrinterSettings) -> Result<(), String> {
    settings.validate()?;
    let path = file(runtime_dir);
    let temporary = path.with_extension("json.part");
    let body = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, &body)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_write_and_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let settings = PrinterSettings {
            enabled: true,
            model: "RPT008".to_owned(),
            queue: "RPT008_USB".to_owned(),
            columns: 48,
            auto_print: true,
        };
        save(dir.path(), &settings).unwrap();
        assert_eq!(
            load(dir.path()),
            settings,
            "what was saved is what comes back"
        );
        assert!(settings.is_configured());
        assert_eq!(settings.printer().driver(), "escpos_spooler");
    }

    #[test]
    fn nothing_stored_is_nothing_configured_rather_than_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let settings = load(dir.path());
        assert_eq!(settings, PrinterSettings::default());
        assert!(!settings.is_configured());
        // Not a simulator. An unconfigured printer answers
        // HARDWARE_NOT_CONFIGURED and never pretends to have printed.
        assert_eq!(settings.printer().driver(), edge_hardware::UNCONFIGURED);
    }

    /// Spaces are ordinary in a printer name; options and control characters
    /// are not.
    #[test]
    fn a_queue_name_cannot_smuggle_an_option_into_the_spooler() {
        for queue in ["-o", "-d other", "", "name\twith\ttabs", "line\nbreak"] {
            let settings = PrinterSettings {
                enabled: true,
                model: "RPT004".to_owned(),
                queue: queue.to_owned(),
                columns: 48,
                auto_print: false,
            };
            assert!(settings.validate().is_err(), "`{queue}` must be refused");
        }
    }

    /// The names Windows actually reports have spaces and punctuation in
    /// them, and every one has to be storable.
    #[test]
    fn a_printer_name_with_spaces_is_an_ordinary_printer_name() {
        for queue in [
            "Microsoft Print to PDF",
            "3nStar RPT008",
            "RPT015 (Copy 1)",
            "Receipt Printer #2",
            "\\\\server\\queue",
        ] {
            let settings = PrinterSettings {
                enabled: true,
                model: "RPT008".to_owned(),
                queue: queue.to_owned(),
                columns: 48,
                auto_print: false,
            };
            settings
                .validate()
                .unwrap_or_else(|error| panic!("`{queue}` was refused: {error}"));
            assert!(settings.is_configured());
        }
    }

    #[test]
    fn every_offered_model_is_a_real_preset() {
        for entry in MODELS {
            assert_eq!(model(entry.id), Some(entry));
            assert!(entry.columns >= 24 && entry.paper_mm > 0);
        }
        assert!(model("RPT999").is_none());
    }
}

/// The composition root's handle on the configured printer.
///
/// Holds the only things that know what a queue or a driver is: the stored
/// settings, the port whose adapter can be replaced, and the paper width the
/// document composer reads. Nothing downstream is given one, so a use case
/// still cannot name a device (ADR 039).
#[derive(Debug, Clone)]
pub struct PrinterControl {
    runtime_dir: PathBuf,
    port: Arc<edge_hardware::SwappablePrinter>,
    columns: Arc<std::sync::atomic::AtomicUsize>,
}

impl PrinterControl {
    pub fn new(
        runtime_dir: PathBuf,
        port: Arc<edge_hardware::SwappablePrinter>,
        columns: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            runtime_dir,
            port,
            columns,
        }
    }

    pub fn settings(&self) -> PrinterSettings {
        load(&self.runtime_dir)
    }

    /// What the installed adapter calls itself.
    pub fn driver(&self) -> &'static str {
        self.port.driver()
    }

    /// Can the configured printer be reached? `None` when none is configured,
    /// because "unreachable" and "not set up" are different answers.
    pub fn reachable(&self) -> Option<bool> {
        let settings = self.settings();
        settings
            .is_configured()
            .then(|| settings.printer().probe().is_ok())
    }

    /// Store a choice and make it the one in force, in that order: a setting
    /// that could not be written must not be the one printing.
    pub fn apply(&self, settings: &PrinterSettings) -> Result<(), String> {
        save(&self.runtime_dir, settings)?;
        self.install(settings);
        Ok(())
    }

    /// Swap the adapter and the paper without touching the stored file. Used
    /// at startup, where the file is already the source.
    pub fn install(&self, settings: &PrinterSettings) {
        self.port.replace(settings.printer());
        let columns = model(&settings.model)
            .map(|chosen| chosen.columns)
            .unwrap_or(settings.columns)
            .clamp(24, 96);
        self.columns
            .store(columns, std::sync::atomic::Ordering::Relaxed);
    }
}
