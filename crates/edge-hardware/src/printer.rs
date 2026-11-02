//! What a printer is asked to print, and the port that prints it.
//!
//! The split that keeps ESC/POS out of the core: the application expresses a
//! **canonical document** — lines, alignment, emphasis, a cut — and the
//! device's adapter renders that into whatever the physical protocol is
//! (ADR 005 as amended in milestone 10).
//!
//! So nothing upstream of the adapter knows an escape sequence, and nothing
//! can send one: the protocol carries intent, never bytes (ADR 039).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::HardwareError;

/// Where a line sits on the paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrintAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// One line of a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintLine {
    pub text: String,
    pub align: PrintAlign,
    /// Bold. Kept as one flag rather than a style system: a receipt needs
    /// emphasis, not typography.
    pub emphasis: bool,
    /// Double height and width, for a total.
    pub double_size: bool,
}

impl PrintLine {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            align: PrintAlign::Left,
            emphasis: false,
            double_size: false,
        }
    }

    pub fn centered(text: impl Into<String>) -> Self {
        Self {
            align: PrintAlign::Center,
            ..Self::plain(text)
        }
    }

    pub fn bold(mut self) -> Self {
        self.emphasis = true;
        self
    }

    pub fn large(mut self) -> Self {
        self.double_size = true;
        self
    }

    /// A left-aligned label and a right-aligned value on one line, padded to
    /// the paper width. Composed here rather than in the renderer so every
    /// adapter lays a receipt out the same way.
    pub fn columns(left: &str, right: &str, width: usize) -> Self {
        let gap = width.saturating_sub(left.chars().count() + right.chars().count());
        Self::plain(format!("{left}{}{right}", " ".repeat(gap.max(1))))
    }
}

/// Why something is being printed. Decides nothing physical; it is what the
/// action log and the renderer's heading show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrintKind {
    /// A diagnostic page. Touches no commercial data.
    TestPage,
    /// A sale's receipt, printed for the first time.
    Receipt,
    /// A sale's receipt again, on request. Opens no drawer and changes nothing
    /// (ADR 037).
    Reprint,
}

impl PrintKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            PrintKind::TestPage => "TEST_PAGE",
            PrintKind::Receipt => "RECEIPT",
            PrintKind::Reprint => "REPRINT",
        }
    }

    pub fn parse(value: &str) -> Result<Self, HardwareError> {
        match value {
            "TEST_PAGE" => Ok(PrintKind::TestPage),
            "RECEIPT" => Ok(PrintKind::Receipt),
            "REPRINT" => Ok(PrintKind::Reprint),
            other => Err(HardwareError::NotConfigured {
                device: "printer",
                detail: format!("`{other}` is not a print kind"),
            }),
        }
    }
}

/// A canonical document, independent of any printer.
///
/// Deliberately small. This is **not** the receipt aggregate: milestone 10
/// proves the transport, the rendering and the failure semantics, and the
/// commercial and fiscal lifecycle of a receipt belongs to its own milestone
/// (ADR 039).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintDocument {
    pub job_id: Uuid,
    pub kind: PrintKind,
    pub lines: Vec<PrintLine>,
    /// Characters per line of the configured paper. 48 on 80mm at font A.
    pub width: usize,
    /// Whether to cut the paper at the end.
    pub cut: bool,
    pub copies: u8,
}

impl PrintDocument {
    pub fn new(job_id: Uuid, kind: PrintKind, width: usize) -> Self {
        Self {
            job_id,
            kind,
            lines: Vec::new(),
            width,
            cut: true,
            copies: 1,
        }
    }

    pub fn line(mut self, line: PrintLine) -> Self {
        self.lines.push(line);
        self
    }

    pub fn blank(self) -> Self {
        self.line(PrintLine::plain(""))
    }

    pub fn rule(self) -> Self {
        let width = self.width;
        self.line(PrintLine::plain("-".repeat(width)))
    }
}

/// A printer, as the application sees it.
///
/// Blocking on purpose: a print is a short, bounded I/O call made outside any
/// database transaction, and an async runtime here would buy nothing but a
/// colour to paint every caller (CLAUDE.md: no async everywhere).
pub trait PrinterPort: Send + Sync {
    /// Which driver this is, for the diagnostic view. Never a host or a port.
    fn driver(&self) -> &'static str;

    /// Print it. One call per copy is the adapter's business, not the
    /// caller's.
    fn print(&self, document: &PrintDocument) -> Result<(), HardwareError>;

    /// Can we reach it? Must not put ink on paper.
    fn probe(&self) -> Result<(), HardwareError>;
}

// =============================================================== its health

/// How much paper is left, as the sensors can report it.
///
/// `Unknown` is not a degree of fullness: it means nothing answered, and a
/// screen must say so rather than draw a reassuring bar (ADR 035).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PaperLevel {
    /// The roll is above the near-end sensor.
    Ok,
    /// The near-end sensor is covered. **A sale may still be made** — this is
    /// a warning to whoever restocks, not a reason to refuse a customer.
    NearEnd,
    /// The end sensor says there is no paper. Nothing can be printed.
    End,
    #[default]
    Unknown,
}

impl PaperLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::NearEnd => "NEAR_END",
            Self::End => "END",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// What the printer is, as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrinterState {
    /// Answering, and ready to print.
    Online,
    /// Answering, and refusing to print — a cover, the paper, or an error.
    Offline,
    /// Answering, with an error condition set.
    Error,
    /// Asked, and said nothing. Not the same as "offline": an offline printer
    /// answered.
    NoResponse,
    #[default]
    Unknown,
}

impl PrinterState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Online => "ONLINE",
            Self::Offline => "OFFLINE",
            Self::Error => "ERROR",
            Self::NoResponse => "NO_RESPONSE",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Who answered the question.
///
/// Published because the two sources do not know the same things, and a floor
/// that cannot tell them apart will read a spooler's silence as a healthy
/// printer. `DleEot` is the device's own firmware. `Spooler` is the host's
/// queue flags — which do report paper out, a door open and offline, and do
/// not report paper *near* end. `None` means nothing was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StatusSource {
    DleEot,
    Spooler,
    #[default]
    None,
}

impl StatusSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DleEot => "DLE_EOT",
            Self::Spooler => "SPOOLER",
            Self::None => "NONE",
        }
    }
}

/// Everything known about the printer at one moment.
///
/// Every field is a fact somebody answered, or the `Unknown` that says nobody
/// did. Nothing here is inferred from a print having succeeded: "the queue
/// took the job" is not a sensor, and treating it as one is how a customer
/// pays for a receipt that cannot come out (ADR 035).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterStatus {
    pub state: PrinterState,
    /// True only when the state is `Online`. Derived, so the two can never
    /// disagree.
    pub online: bool,
    pub cover_open: bool,
    pub paper: PaperLevel,
    pub cutter_error: bool,
    /// A recoverable error the firmware wants cleared before it will print.
    ///
    /// On a receipt printer that is a paper jam or a lifted head — the
    /// firmware does not distinguish them, and neither does this. The name is
    /// what an operator is looking for; the honest reading is "stuck, and
    /// clearable by hand".
    pub jammed: bool,
    /// An error the device says it cannot clear by itself.
    pub unrecoverable_error: bool,
    pub source: StatusSource,
    /// Why the read failed, when it did. Never a stack trace, never an
    /// address.
    pub error: Option<String>,
}

impl PrinterStatus {
    /// Nobody has asked. Not a failure, and not a healthy printer either.
    pub fn unknown() -> Self {
        Self {
            state: PrinterState::Unknown,
            online: false,
            cover_open: false,
            paper: PaperLevel::Unknown,
            cutter_error: false,
            jammed: false,
            unrecoverable_error: false,
            source: StatusSource::None,
            error: None,
        }
    }

    /// Asked, and nothing came back.
    pub fn no_response(source: StatusSource, why: impl Into<String>) -> Self {
        Self {
            state: PrinterState::NoResponse,
            source,
            error: Some(why.into()),
            ..Self::unknown()
        }
    }

    /// Whether a payment may be started against this printer.
    ///
    /// **The rule, in one place.** A customer must not put money in for a
    /// receipt that cannot come out, so a cover that is open, a roll that has
    /// run out, and a printer that is not answering each stop a payment before
    /// it starts. Paper that is merely *near* its end does not: that is a
    /// warning for whoever restocks, and refusing a sale over it would close
    /// the lane for an hour of perfectly good paper.
    ///
    /// `Unknown` does **not** block. A kiosk with no status channel
    /// configured knows nothing about its printer, and a rule that refuses
    /// every sale on an installation that cannot answer the question is a rule
    /// that closes the shop. What blocks is an answer, not the absence of one.
    pub fn blocks_payment(&self) -> bool {
        self.cover_open
            || self.paper == PaperLevel::End
            || matches!(self.state, PrinterState::NoResponse | PrinterState::Offline)
    }

    /// What to put in front of the customer when it does.
    pub const BLOCKED_MESSAGE: &'static str =
        "Impresora no disponible.\nRevisa el papel y la tapa antes de continuar.";
}

/// A printer that can be asked how it is.
///
/// Separate from [`PrinterPort`] because the two are not the same capability
/// and not always the same wire: the showroom's printers take their work
/// through the host spooler, which cannot carry a status query back, while a
/// network printer answers `DLE EOT` on the socket it prints on. An
/// installation has a print path always and a status channel sometimes.
pub trait PrinterStatusPort: Send + Sync {
    /// Who is answering. Published so a floor can tell the sources apart.
    fn source(&self) -> StatusSource;

    /// Ask. Returns the status even when it is bad news; `Err` is for a read
    /// that could not be made at all.
    fn read(&self) -> Result<PrinterStatus, HardwareError>;
}
