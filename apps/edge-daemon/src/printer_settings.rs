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

use edge_hardware::{
    EscPosConfig, EscPosSpoolerPrinter, EscPosTcpPrinter, EscPosTcpStatus, PrinterPort,
    PrinterStatus, PrinterStatusPort, SpoolerStatus, StatusSource, UnconfiguredPrinter,
    UsbDleEotStatus,
};
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
    /// What this model's firmware will tell us about itself.
    ///
    /// It matters because the answer differs by model, and a status screen
    /// that claims more than the hardware reports is worse than one that says
    /// `UNKNOWN`. Nothing here reads a sensor yet — the field declares what
    /// *could* be read once the transport does, so the demo can show the
    /// association between a configured model and the states it supports
    /// (ADR 035).
    pub reports: Reports,
}

/// What a printer's firmware can say back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reports {
    /// Paper present / near-end / out.
    pub paper: bool,
    /// A cover open or a jam.
    pub jam: bool,
    /// Whether the device answers a status query at all (ASB or equivalent).
    pub online: bool,
}

impl Reports {
    /// A device that answers nothing but "the queue took it".
    const SILENT: Self = Self {
        paper: false,
        jam: false,
        online: false,
    };
    /// ESC/POS Automatic Status Back: paper, cover and online.
    const ASB: Self = Self {
        paper: true,
        jam: true,
        online: true,
    };
    /// Paper and online, but no cover/jam line.
    const PAPER_ONLY: Self = Self {
        paper: true,
        jam: false,
        online: true,
    };

    /// The states a status screen may show for this model, in order.
    pub fn paper_states(self) -> &'static [&'static str] {
        if self.paper {
            &["OK", "LOW", "OUT", "UNKNOWN"]
        } else {
            &["UNKNOWN"]
        }
    }
}

/// Every model this build offers. All ESC/POS, all through the spooler.
///
/// The 3nStar units are driven write-only: the demo sends a document and the
/// spooler says whether it took it. The Epson units speak ASB, so a later
/// build can read paper and cover from them — which is why the model is
/// recorded rather than just its column count.
pub const MODELS: [PrinterModel; 6] = [
    PrinterModel {
        id: "RPT004",
        label: "3nStar RPT004",
        paper_mm: 80,
        columns: 48,
        reports: Reports::SILENT,
    },
    PrinterModel {
        id: "RPT008",
        label: "3nStar RPT008",
        paper_mm: 80,
        columns: 48,
        reports: Reports::SILENT,
    },
    PrinterModel {
        id: "RPT015",
        label: "3nStar RPT015",
        paper_mm: 80,
        columns: 48,
        reports: Reports::SILENT,
    },
    PrinterModel {
        id: "EPSON-EU30",
        label: "Epson EU-m30 (EU30)",
        paper_mm: 58,
        columns: 32,
        // A kiosk-mount printer: paper status, no cover line.
        reports: Reports::PAPER_ONLY,
    },
    PrinterModel {
        id: "EPSON-TM-T88VI",
        label: "Epson TM-T88VI",
        paper_mm: 80,
        columns: 48,
        reports: Reports::ASB,
    },
    PrinterModel {
        id: "EPSON-TM-T82III",
        label: "Epson TM-T82III",
        paper_mm: 80,
        columns: 48,
        reports: Reports::ASB,
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
    /// Where to ask this printer how it is, as `host[:port]`. Empty for none.
    ///
    /// A **separate** address from the queue that prints, because the two are
    /// separate capabilities on this hardware: the USB receipt printers on a
    /// stand take their work through a spooler queue, and a spooler queue is
    /// write-only — there is no way to get a `DLE EOT` reply back out of one.
    /// A printer reachable over TCP answers status on the same socket it
    /// prints on, and this is where that socket is named.
    ///
    /// Empty does not mean "no status": it falls back to what the host
    /// spooler reports about the queue, which is less but is not nothing.
    #[serde(default)]
    pub status_channel: String,
}

impl Default for PrinterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            queue: String::new(),
            columns: 48,
            auto_print: false,
            status_channel: String::new(),
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
        // Empty is a valid answer — it selects the spooler fallback — but a
        // channel that is set has to be one this build can actually open,
        // because a malformed one would read as `NO_RESPONSE` and quietly
        // stop every sale on the kiosk.
        StatusChannel::parse(&self.status_channel)?;
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

    /// How to ask this printer how it is, or `None` when nothing can.
    ///
    /// Three sources, in the order of how much they know:
    ///
    /// ```text
    /// status_channel = "usb"              USBPRINT device  -> DLE EOT
    ///                  "usb:<fragment>"   a named one of several
    ///                  "host[:port]"      a network printer -> DLE EOT
    ///                  ""                 the host spooler's queue flags
    /// ```
    ///
    /// The first two are the real bidirectional channels: the firmware's own
    /// answer, including paper *near* its end, which nothing else can report.
    /// The last is the fallback for hardware that offers nothing better, and
    /// it is labelled `SPOOLER` on the wire so a floor is never told a queue
    /// flag is a sensor (ADR 048).
    ///
    /// Nothing configured is `None`, which reports `UNKNOWN` and refuses no
    /// sale: an installation that cannot ask the question does not get to
    /// close the lane over it.
    pub fn status_port(&self) -> Option<Arc<dyn PrinterStatusPort>> {
        if !self.is_configured() {
            return None;
        }
        match StatusChannel::parse(&self.status_channel) {
            // Parsed again rather than trusted: `validate` refuses a bad one
            // on the way in, and a file edited by hand never went through it.
            Err(detail) => {
                tracing::warn!(%detail, "the stored printer status channel is unusable; \
                     falling back to the host spooler");
                Some(Arc::new(SpoolerStatus::new(self.queue.trim())))
            }
            Ok(StatusChannel::Spooler) => Some(Arc::new(SpoolerStatus::new(self.queue.trim()))),
            Ok(StatusChannel::Usb(hint)) => Some(Arc::new(UsbDleEotStatus::new(hint))),
            Ok(StatusChannel::Network { host, port }) => {
                let config = EscPosConfig {
                    width: self.columns,
                    ..EscPosConfig::new(host, port)
                };
                Some(Arc::new(EscPosTcpStatus::new(EscPosTcpPrinter::new(
                    config,
                ))))
            }
        }
    }

    /// Who would answer, for the admin screen.
    pub fn status_source(&self) -> StatusSource {
        self.status_port()
            .map(|port| port.source())
            .unwrap_or(StatusSource::None)
    }
}

/// What an operator typed into the status channel field, understood.
///
/// A tiny grammar, and it exists rather than being parsed inline because two
/// places read it — building the port and validating a save — and they must
/// agree. A channel that one accepts and the other cannot build would read as
/// a printer that is not answering, which stops every sale on the kiosk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusChannel {
    /// Nothing named: the host spooler's flags for the queue that prints.
    Spooler,
    /// The printer's own USB device interface. The fragment picks one of
    /// several; empty means "the only one there is".
    Usb(String),
    /// A printer on the network, asked on the socket it prints on.
    Network { host: String, port: u16 },
}

impl StatusChannel {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Self::Spooler);
        }
        if let Some(rest) = raw.strip_prefix("usb") {
            let hint = rest.strip_prefix(':').unwrap_or("").trim();
            if !rest.is_empty() && !rest.starts_with(':') {
                return Err(format!(
                    "`{raw}` is neither `usb`, `usb:<fragment>` nor an address"
                ));
            }
            if hint.chars().any(char::is_control) {
                return Err("a USB device fragment cannot contain control characters".to_owned());
            }
            return Ok(Self::Usb(hint.to_owned()));
        }
        // The conventional 9100 is the default port, as it is for the print
        // path. No default host: a channel with no address is a configuration
        // error, not a guess at which machine on the network to interrogate
        // (ADR 039).
        let (host, port) = match raw.rsplit_once(':') {
            Some((host, port)) => (
                host.trim(),
                port.trim()
                    .parse::<u16>()
                    .map_err(|_| format!("`{port}` is not a TCP port"))?,
            ),
            None => (raw, EscPosConfig::DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err("a printer status channel needs a host, as `host[:port]`".to_owned());
        }
        if host.chars().any(char::is_control) || host.chars().any(char::is_whitespace) {
            return Err(format!("`{host}` is not a host name"));
        }
        Ok(Self::Network {
            host: host.to_owned(),
            port,
        })
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
            status_channel: "printer.local:9100".to_owned(),
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
                status_channel: String::new(),
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
                status_channel: String::new(),
            };
            settings
                .validate()
                .unwrap_or_else(|error| panic!("`{queue}` was refused: {error}"));
            assert!(settings.is_configured());
        }
    }

    /// The status channel grammar, which two places have to read the same
    /// way: the one that builds the port and the one that validates a save.
    #[test]
    fn the_status_channel_says_which_of_the_three_sources_answers() {
        assert_eq!(StatusChannel::parse(""), Ok(StatusChannel::Spooler));
        assert_eq!(StatusChannel::parse("   "), Ok(StatusChannel::Spooler));

        // The real bidirectional channels.
        assert_eq!(
            StatusChannel::parse("usb"),
            Ok(StatusChannel::Usb(String::new()))
        );
        assert_eq!(
            StatusChannel::parse("usb:vid_0519"),
            Ok(StatusChannel::Usb("vid_0519".to_owned()))
        );
        assert_eq!(
            StatusChannel::parse("usb: vid_0519 "),
            Ok(StatusChannel::Usb("vid_0519".to_owned())),
            "a fragment typed with spaces around it"
        );
        assert_eq!(
            StatusChannel::parse("192.168.88.50:9100"),
            Ok(StatusChannel::Network {
                host: "192.168.88.50".to_owned(),
                port: 9_100,
            })
        );
        assert_eq!(
            StatusChannel::parse("printer.local"),
            Ok(StatusChannel::Network {
                host: "printer.local".to_owned(),
                port: 9_100,
            }),
            "9100 is the default port, not a default host"
        );
    }

    /// A channel this build could not open is refused on the way in. Letting
    /// one through would read as a printer that is not answering, and that
    /// stops every sale on the kiosk.
    #[test]
    fn a_status_channel_that_cannot_be_opened_is_refused() {
        for bad in [
            "printer.local:notaport",
            ":9100",
            "has a space:9100",
            "usbprint",
            "usb2",
            ":",
        ] {
            assert!(
                StatusChannel::parse(bad).is_err(),
                "`{bad}` was accepted as a status channel"
            );
        }
    }

    /// Each channel selects the source it promises, and nothing configured
    /// selects none.
    #[test]
    fn each_channel_selects_the_source_it_names() {
        let printer = |channel: &str| PrinterSettings {
            enabled: true,
            model: "RPT008".to_owned(),
            queue: "RPT008_USB".to_owned(),
            columns: 48,
            auto_print: false,
            status_channel: channel.to_owned(),
        };
        // The two real `DLE EOT` channels.
        assert_eq!(printer("usb").status_source(), StatusSource::DleEot);
        assert_eq!(
            printer("usb:vid_0519").status_source(),
            StatusSource::DleEot
        );
        assert_eq!(
            printer("192.168.88.50:9100").status_source(),
            StatusSource::DleEot
        );
        // The fallback, named as what it is.
        assert_eq!(printer("").status_source(), StatusSource::Spooler);
        // And a printer that is switched off asks nothing at all.
        assert_eq!(
            PrinterSettings::default().status_source(),
            StatusSource::None
        );
    }

    /// A stored file edited by hand can carry a channel `validate` never saw.
    /// It falls back to the spooler rather than reporting a printer that is
    /// not answering — which would stop every sale over a typo.
    #[test]
    fn a_stored_channel_that_is_nonsense_falls_back_rather_than_closing_the_lane() {
        let settings = PrinterSettings {
            enabled: true,
            model: "RPT008".to_owned(),
            queue: "RPT008_USB".to_owned(),
            columns: 48,
            auto_print: false,
            status_channel: "usb2".to_owned(),
        };
        assert_eq!(settings.status_source(), StatusSource::Spooler);
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
    /// The last answer, and when it was taken.
    ///
    /// Cached because the question is asked on the path to taking money, and
    /// a payment must not wait on four socket round trips — and because a
    /// kiosk polls it every fifteen seconds anyway, so the answer is nearly
    /// always already here.
    status: Arc<std::sync::RwLock<CachedPrinterStatus>>,
}

/// What the printer last said, and when it said it.
#[derive(Debug, Clone)]
pub struct CachedPrinterStatus {
    pub status: PrinterStatus,
    /// `None` before the first read — which is the honest reading of
    /// `UNKNOWN`, not a zero timestamp.
    pub checked_at: Option<edge_domain::prelude::Timestamp>,
}

impl Default for CachedPrinterStatus {
    fn default() -> Self {
        Self {
            status: PrinterStatus::unknown(),
            checked_at: None,
        }
    }
}

/// How often an idle kiosk asks. Fifteen seconds: fast enough that somebody
/// opening the cover sees the screen change while they are still standing
/// there, slow enough to be invisible on the wire.
pub const STATUS_POLL: std::time::Duration = std::time::Duration::from_secs(15);

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
            status: Arc::new(std::sync::RwLock::new(CachedPrinterStatus::default())),
        }
    }

    /// What the printer last said. Never touches the device.
    pub fn cached_status(&self) -> CachedPrinterStatus {
        self.status
            .read()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    /// Ask the device now, and remember the answer.
    ///
    /// Called on the three occasions the rule needs: every fifteen seconds
    /// while idle, immediately before a payment is allowed to start, and
    /// after a document has been printed.
    pub fn refresh_status(&self) -> CachedPrinterStatus {
        let fresh = CachedPrinterStatus {
            status: self.port.read_status(),
            // The composition root may read a clock; the domain may not.
            checked_at: Some(edge_application::ports::Clock::now(
                &edge_application::SystemClock,
            )),
        };
        if let Ok(mut held) = self.status.write() {
            *held = fresh.clone();
        }
        fresh
    }

    /// Ask the printer how it is, every fifteen seconds, for the life of the
    /// runtime.
    ///
    /// Spawned and never joined: the daemon's life is the poller's. It reads
    /// through the same mutex a print takes, so a poll in the middle of a
    /// busy lunch waits its turn rather than interleaving with a ticket.
    pub fn watch(&self) {
        let control = self.clone();
        std::thread::spawn(move || loop {
            control.refresh_status();
            std::thread::sleep(STATUS_POLL);
        });
    }

    pub fn settings(&self) -> PrinterSettings {
        load(&self.runtime_dir)
    }

    /// What the installed adapter calls itself.
    pub fn driver(&self) -> &'static str {
        self.port.driver()
    }

    /// Who **would** answer a status read, from the channel installed.
    ///
    /// Deliberately not "who answered last": before the first read nobody
    /// has, and reporting `NONE` then would tell a floor monitor that a
    /// perfectly configured printer cannot say anything about itself. The
    /// two questions are separated in the published payload — this field says
    /// who can answer, and `status_checked_at` says whether they have.
    pub fn status_source(&self) -> StatusSource {
        self.port.status_source()
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
        // An operator who just pressed Save is looking at the screen; the
        // first answer from the new device belongs on it now rather than in
        // fifteen seconds.
        self.refresh_status();
        Ok(())
    }

    /// Swap the adapter, the status channel and the paper without touching
    /// the stored file. Used at startup, where the file is already the source.
    pub fn install(&self, settings: &PrinterSettings) {
        self.port.replace(settings.printer());
        // The channel goes with the printer. A kiosk reconfigured onto a
        // device that cannot answer has to go back to `UNKNOWN` rather than
        // keep reporting the previous one's paper, so the cache is cleared
        // here too.
        self.port.replace_status(settings.status_port());
        if let Ok(mut held) = self.status.write() {
            *held = CachedPrinterStatus::default();
        }
        let columns = model(&settings.model)
            .map(|chosen| chosen.columns)
            .unwrap_or(settings.columns)
            .clamp(24, 96);
        self.columns
            .store(columns, std::sync::atomic::Ordering::Relaxed);
    }
}
