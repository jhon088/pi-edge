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
