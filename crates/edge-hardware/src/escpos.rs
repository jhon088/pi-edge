//! ESC/POS: the renderer and the TCP transport.
//!
//! The only file in the repository that knows an escape sequence. Everything
//! upstream speaks `PrintDocument` (ADR 005 as amended, ADR 039).
//!
//! Thermal receipt printers on a network listen on a raw TCP port — 9100 by
//! convention, configurable here and never in code. The host and the port come
//! from the installation's configuration, never from a protocol payload: a
//! client that could name a host could print anywhere on the network
//! (ADR 039).

use std::io::Write;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use std::io::Read;

use crate::drawer::{CashDrawerPort, DrawerOpenReason};
use crate::error::HardwareError;
use crate::printer::{
    PaperLevel, PrintAlign, PrintDocument, PrintLine, PrinterPort, PrinterState, PrinterStatus,
    PrinterStatusPort, StatusSource,
};

/// Where a printer is and how long to wait for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscPosConfig {
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
    pub write_timeout: Duration,
    /// Characters per line. 48 on 80mm paper at font A.
    pub width: usize,
}

impl EscPosConfig {
    /// The conventional port, as a default a configuration may override. Not a
    /// hardcoded address: there is no default host.
    pub const DEFAULT_PORT: u16 = 9100;
    pub const DEFAULT_WIDTH: usize = 48;

    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            connect_timeout: Duration::from_secs(2),
            write_timeout: Duration::from_secs(5),
            width: Self::DEFAULT_WIDTH,
        }
    }
}

// --------------------------------------------------------------- the renderer

const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;
const DLE: u8 = 0x10;
const DC4: u8 = 0x14;
const EOT: u8 = 0x04;

/// Turn a canonical document into ESC/POS bytes.
///
/// Deterministic, pure and testable without a printer: the bytes for a given
/// document are always the same, which is what makes a golden of them useful.
pub fn render(document: &PrintDocument) -> Vec<u8> {
    let mut out = Vec::with_capacity(document.lines.len() * 48 + 32);
    // Initialise: clears any mode a previous job left behind, so one bad job
    // cannot make every later receipt bold.
    out.extend_from_slice(&[ESC, b'@']);
    for line in &document.lines {
        render_line(&mut out, line);
    }
    // Feed the paper clear of the cutter before cutting.
    out.extend_from_slice(&[ESC, b'd', 4]);
    if document.cut {
        // GS V 66 0: feed and partial cut.
        out.extend_from_slice(&[GS, b'V', 66, 0]);
    }
    out
}

fn render_line(out: &mut Vec<u8>, line: &PrintLine) {
    let align = match line.align {
        PrintAlign::Left => 0,
        PrintAlign::Center => 1,
        PrintAlign::Right => 2,
    };
    out.extend_from_slice(&[ESC, b'a', align]);
    out.extend_from_slice(&[ESC, b'E', u8::from(line.emphasis)]);
    // GS ! n: the low nibble is height, the high nibble width.
    out.extend_from_slice(&[GS, b'!', if line.double_size { 0x11 } else { 0x00 }]);
    out.extend_from_slice(encode(&line.text).as_slice());
    out.push(b'\n');
}

/// Collapse to single-byte characters the printer's default code page can show.
///
/// A receipt printer is not Unicode. Rather than emit a byte the printer will
/// render as garbage, accented Latin characters fold to their base letter and
/// anything else becomes `?` — legible, and never a stream the device cannot
/// parse.
fn encode(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            c if c.is_ascii() => c as u8,
            'á' | 'à' | 'ä' | 'â' => b'a',
            'é' | 'è' | 'ë' | 'ê' => b'e',
            'í' | 'ì' | 'ï' | 'î' => b'i',
            'ó' | 'ò' | 'ö' | 'ô' => b'o',
            'ú' | 'ù' | 'ü' | 'û' => b'u',
            'ñ' => b'n',
            'Á' | 'À' | 'Ä' | 'Â' => b'A',
            'É' | 'È' | 'Ë' | 'Ê' => b'E',
            'Í' | 'Ì' | 'Ï' | 'Î' => b'I',
            'Ó' | 'Ò' | 'Ö' | 'Ô' => b'O',
            'Ú' | 'Ù' | 'Ü' | 'Û' => b'U',
            'Ñ' => b'N',
            '°' => b'o',
            _ => b'?',
        })
        .collect()
}

/// The pulse that opens a drawer wired to a printer's DK port.
///
/// `DLE DC4 n m t`: generate a pulse on connector pin `n` for `t` units. Pin 0
/// is the usual one; 50ms on, 50ms off suits every drawer we have met.
pub fn drawer_kick() -> Vec<u8> {
    vec![DLE, DC4, 1, 50, 50]
}

// -------------------------------------------------------------- the transport

/// An ESC/POS printer reached over TCP.
#[derive(Debug, Clone)]
pub struct EscPosTcpPrinter {
    config: EscPosConfig,
}

impl EscPosTcpPrinter {
    pub fn new(config: EscPosConfig) -> Self {
        Self { config }
    }

    pub fn width(&self) -> usize {
        self.config.width
    }

    /// Open a connection, with the configured timeouts applied to it.
    fn connect(&self, device: &'static str) -> Result<TcpStream, HardwareError> {
        let started = Instant::now();
        let address = format!("{}:{}", self.config.host, self.config.port);
        let mut resolved =
            address
                .to_socket_addrs()
                .map_err(|error| HardwareError::Unavailable {
                    device,
                    detail: format!("cannot resolve the configured address: {error}"),
                })?;
        let socket = resolved.next().ok_or(HardwareError::NotConfigured {
            device,
            detail: "the configured address resolves to nothing".to_owned(),
        })?;

        // A connection that never opened is `Unavailable`, timeout included:
        // no socket means no bytes, and reporting it as a timeout would make a
        // printer we never reached look like one that may have printed
        // (ADR 043).
        let stream =
            TcpStream::connect_timeout(&socket, self.config.connect_timeout).map_err(|error| {
                match error.kind() {
                    std::io::ErrorKind::TimedOut => HardwareError::Unavailable {
                        device,
                        detail: format!(
                            "the connection timed out after {}ms",
                            started.elapsed().as_millis()
                        ),
                    },
                    _ => HardwareError::Unavailable {
                        device,
                        // The host is deliberately not echoed: a diagnostic
                        // message must not disclose the installation's network.
                        detail: format!("connection refused or unreachable ({})", error.kind()),
                    },
                }
            })?;
        stream
            .set_write_timeout(Some(self.config.write_timeout))
            .map_err(|error| HardwareError::Io {
                device,
                detail: error.to_string(),
            })?;
        Ok(stream)
    }

    fn send(&self, device: &'static str, bytes: &[u8]) -> Result<(), HardwareError> {
        let started = Instant::now();
        let mut stream = self.connect(device)?;
        let written = stream.write_all(bytes).and_then(|()| stream.flush());
        // Closing is best effort: the bytes are already on the wire.
        let _ = stream.shutdown(Shutdown::Both);
        written.map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                HardwareError::Timeout {
                    device,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }
            }
            _ => HardwareError::Io {
                device,
                detail: format!("write failed ({})", error.kind()),
            },
        })
    }
}

impl PrinterPort for EscPosTcpPrinter {
    fn driver(&self) -> &'static str {
        "escpos_tcp"
    }

    fn print(&self, document: &PrintDocument) -> Result<(), HardwareError> {
        let bytes = render(document);
        for _ in 0..document.copies.max(1) {
            self.send("printer", &bytes)?;
        }
        Ok(())
    }

    fn probe(&self) -> Result<(), HardwareError> {
        // Opening the socket is as far as a probe may go: anything written
        // would come out of the machine as paper.
        self.connect("printer").map(|_| ())
    }
}

// ------------------------------------------------------------- the status wire
//
// `DLE EOT n` — real-time status transmission. The printer answers a single
// byte *immediately*, ahead of whatever is in its own buffer, which is what
// makes it usable as a health check and what makes it dangerous to send in
// the middle of a document: the reply is a byte on the same wire the receipt
// is travelling down. Serialising the two is the caller's job and is done
// once, in the composition root.
//
// Four questions, one byte each. Every answer carries four fixed bits —
// bit 1 and bit 4 set, bit 0 and bit 7 clear — which is the only way to tell
// a status byte from an echo, a stale buffer or line noise.

/// Which of the four status bytes to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusQuery {
    /// Online or offline, and the drawer pin.
    Printer,
    /// *Why* it is offline: the cover, a feed button, paper having run out.
    OfflineCause,
    /// The cutter, a recoverable error, an unrecoverable one.
    Error,
    /// The roll sensors: near-end and end.
    PaperSensor,
}

impl StatusQuery {
    /// Every question, in the order a full read asks them.
    pub const ALL: [Self; 4] = [
        Self::Printer,
        Self::OfflineCause,
        Self::Error,
        Self::PaperSensor,
    ];

    const fn n(self) -> u8 {
        match self {
            Self::Printer => 1,
            Self::OfflineCause => 2,
            Self::Error => 3,
            Self::PaperSensor => 4,
        }
    }

    /// `DLE EOT n`. The request, as bytes.
    pub const fn request(self) -> [u8; 3] {
        [DLE, EOT, self.n()]
    }
}

/// The bits every `DLE EOT` reply has fixed, and what they are fixed to.
///
/// Bit 1 and bit 4 are always set; bit 0 and bit 7 are always clear. A byte
/// that disagrees is not a status byte, and accepting it would turn noise on
/// a serial line into "the cover is open" — which, under the rule that stops
/// a payment, would close the lane.
const FIXED_MASK: u8 = 0b1001_0011;
const FIXED_BITS: u8 = 0b0001_0010;

/// Whether this byte can be a `DLE EOT` reply at all.
pub const fn is_status_byte(byte: u8) -> bool {
    byte & FIXED_MASK == FIXED_BITS
}

/// The four replies, folded into one status.
///
/// A question nobody answered contributes nothing rather than a default: that
/// is why each argument is an `Option` and why a read with no `Printer` byte
/// is `NO_RESPONSE` instead of a healthy printer.
pub fn read_status(
    printer: Option<u8>,
    offline_cause: Option<u8>,
    error: Option<u8>,
    paper: Option<u8>,
) -> PrinterStatus {
    let Some(printer) = printer else {
        return PrinterStatus::no_response(
            StatusSource::DleEot,
            "the printer did not answer DLE EOT 1",
        );
    };

    // n=1: bit 3 set means the device has taken itself offline.
    let offline = printer & 0b0000_1000 != 0;

    // n=2: why.
    let cover_open = offline_cause.is_some_and(|byte| byte & 0b0000_0100 != 0);
    let stopped_for_paper = offline_cause.is_some_and(|byte| byte & 0b0010_0000 != 0);
    let offline_error = offline_cause.is_some_and(|byte| byte & 0b0100_0000 != 0);

    // n=3: which error. A receipt printer calls a jam and a lifted head the
    // same thing — a *recoverable* error — so that is what `jammed` carries,
    // and the doc comment on the field says so rather than inventing a
    // sensor the firmware does not have.
    let jammed = error.is_some_and(|byte| byte & 0b0000_0100 != 0);
    let cutter_error = error.is_some_and(|byte| byte & 0b0000_1000 != 0);
    let unrecoverable = error.is_some_and(|byte| byte & 0b0010_0000 != 0);
    let overheated = error.is_some_and(|byte| byte & 0b0100_0000 != 0);

    // n=4: the roll. Both sensors report with two bits, set together.
    let paper_level = match paper {
        None => PaperLevel::Unknown,
        Some(byte) if byte & 0b0110_0000 != 0 => PaperLevel::End,
        Some(byte) if byte & 0b0000_1100 != 0 => PaperLevel::NearEnd,
        Some(_) => PaperLevel::Ok,
    };
    // A device that stopped *because* the paper ran out has no paper, whatever
    // a sensor it does not have would have said.
    let paper_level = if stopped_for_paper && paper_level == PaperLevel::Unknown {
        PaperLevel::End
    } else {
        paper_level
    };

    let faulted = jammed || cutter_error || unrecoverable || overheated || offline_error;
    let state = if offline || cover_open || paper_level == PaperLevel::End {
        PrinterState::Offline
    } else if faulted {
        PrinterState::Error
    } else {
        PrinterState::Online
    };

    PrinterStatus {
        state,
        online: state == PrinterState::Online,
        cover_open,
        paper: paper_level,
        cutter_error,
        jammed,
        unrecoverable_error: unrecoverable,
        source: StatusSource::DleEot,
        // What a person on a stand can act on, in the order it stops them.
        error: if cover_open {
            Some("the cover is open".to_owned())
        } else if paper_level == PaperLevel::End {
            Some("the paper has run out".to_owned())
        } else if jammed {
            Some("the paper is jammed or the head is lifted".to_owned())
        } else if cutter_error {
            Some("the cutter is stuck".to_owned())
        } else if unrecoverable {
            Some("the printer reports an error it cannot clear".to_owned())
        } else if overheated {
            Some("the print head is too hot; it will recover".to_owned())
        } else if offline {
            Some("the printer has taken itself offline".to_owned())
        } else {
            None
        },
    }
}

/// An ESC/POS printer's status, read over the socket it prints on.
///
/// The same address as the print path, deliberately: a status channel that
/// points somewhere else is a status channel that can report a different
/// printer's paper. It is a separate adapter because a status read and a
/// print are separate capabilities, not because they are separate devices.
#[derive(Debug, Clone)]
pub struct EscPosTcpStatus {
    printer: EscPosTcpPrinter,
}

impl EscPosTcpStatus {
    pub fn new(printer: EscPosTcpPrinter) -> Self {
        Self { printer }
    }

    /// Ask one question and read one byte.
    ///
    /// Each question is its own connection. A receipt printer's socket is
    /// single-session and firmware differs on whether it will pipeline four
    /// real-time requests; four short connections always work and cost
    /// milliseconds on a LAN.
    fn ask(&self, query: StatusQuery) -> Option<u8> {
        let mut stream = self.printer.connect("printer").ok()?;
        stream
            .set_read_timeout(Some(STATUS_READ_TIMEOUT))
            .ok()
            .and_then(|()| {
                stream
                    .write_all(&query.request())
                    .and_then(|()| stream.flush())
                    .ok()
            })?;
        // Small, because the answer is one byte and anything else on the wire
        // is something this read has no business consuming.
        let mut buffer = [0_u8; 8];
        let read = stream.read(&mut buffer).unwrap_or(0);
        let _ = stream.shutdown(Shutdown::Both);
        // The first byte that can be a reply. Firmware occasionally prefixes
        // a `DLE` or leaves a stale byte, and a reply is recognisable.
        buffer[..read]
            .iter()
            .copied()
            .find(|byte| is_status_byte(*byte))
    }
}

/// Long enough for firmware to turn a real-time request around on a LAN,
/// short enough that four of them never hold a payment up.
const STATUS_READ_TIMEOUT: Duration = Duration::from_millis(900);

impl PrinterStatusPort for EscPosTcpStatus {
    fn source(&self) -> StatusSource {
        StatusSource::DleEot
    }

    fn read(&self) -> Result<PrinterStatus, HardwareError> {
        let [printer, offline_cause, error, paper] = StatusQuery::ALL.map(|query| self.ask(query));
        Ok(read_status(printer, offline_cause, error, paper))
    }
}

/// A drawer wired to an ESC/POS printer's DK port, opened by a pulse.
///
/// A **real** adapter for that topology, and a different one from a drawer wired
/// directly to an iMin terminal. It is not a substitute for the iMin path and
/// proving this one works proves nothing about that one (ADR 038).
#[derive(Debug, Clone)]
pub struct EscPosKickDrawer {
    printer: EscPosTcpPrinter,
}

impl EscPosKickDrawer {
    pub fn new(printer: EscPosTcpPrinter) -> Self {
        Self { printer }
    }
}

impl CashDrawerPort for EscPosKickDrawer {
    fn driver(&self) -> &'static str {
        "escpos_kick"
    }

    fn open(&self, _reason: DrawerOpenReason) -> Result<(), HardwareError> {
        self.printer.send("cash drawer", &drawer_kick())
    }

    fn probe(&self) -> Result<(), HardwareError> {
        // All that can be verified without opening it: the printer carrying
        // the pulse is reachable. A solenoid has nothing to answer.
        self.printer.connect("cash drawer").map(|_| ())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::{PrintKind, PrintLine};
    use uuid::Uuid;

    fn document() -> PrintDocument {
        PrintDocument::new(Uuid::from_u128(1), PrintKind::TestPage, 48)
            .line(PrintLine::centered("PI.EDGE").bold())
            .line(PrintLine::plain("linea"))
    }

    #[test]
    fn a_document_renders_deterministically() {
        assert_eq!(render(&document()), render(&document()));
    }

    #[test]
    fn rendering_initialises_the_printer_and_cuts_at_the_end() {
        let bytes = render(&document());
        assert_eq!(&bytes[0..2], &[ESC, b'@'], "ESC @ resets any stale mode");
        assert!(
            bytes.ends_with(&[GS, b'V', 66, 0]),
            "a partial cut closes the job"
        );
    }

    #[test]
    fn a_document_that_does_not_cut_does_not_emit_the_cut() {
        let mut doc = document();
        doc.cut = false;
        let bytes = render(&doc);
        assert!(!bytes.ends_with(&[GS, b'V', 66, 0]));
    }

    #[test]
    fn alignment_and_emphasis_become_their_escape_sequences() {
        let bytes = render(
            &PrintDocument::new(Uuid::from_u128(2), PrintKind::TestPage, 48)
                .line(PrintLine::centered("x").bold().large()),
        );
        // ESC a 1 (centre), ESC E 1 (bold), GS ! 0x11 (double).
        assert!(window(&bytes, &[ESC, b'a', 1]));
        assert!(window(&bytes, &[ESC, b'E', 1]));
        assert!(window(&bytes, &[GS, b'!', 0x11]));
    }

    #[test]
    fn every_line_resets_the_modes_it_does_not_ask_for() {
        // The second line is plain, so it must carry its own zeroes rather
        // than inherit the first line's bold.
        let bytes = render(&document());
        let occurrences = bytes.windows(3).filter(|w| w == &[ESC, b'E', 0]).count();
        assert_eq!(occurrences, 1, "the plain line disables emphasis itself");
    }

    #[test]
    fn non_ascii_text_folds_to_something_the_printer_can_render() {
        let bytes = render(
            &PrintDocument::new(Uuid::from_u128(3), PrintKind::TestPage, 48)
                .line(PrintLine::plain("Leche entera piña ÑOÑO 1°")),
        );
        let text = String::from_utf8(
            bytes
                .iter()
                .copied()
                .filter(|b| b.is_ascii_graphic() || *b == b' ')
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(text.contains("Leche entera pina NONO 1o"), "{text}");
        assert!(
            bytes.iter().all(|b| *b < 0x80 || *b == 0xB0 || true),
            "no multi-byte sequence survives"
        );
    }

    #[test]
    fn the_drawer_kick_is_the_dk_pulse() {
        assert_eq!(drawer_kick(), vec![DLE, DC4, 1, 50, 50]);
    }

    #[test]
    fn columns_pad_to_the_paper_width() {
        let line = PrintLine::columns("TOTAL", "116.00", 20);
        assert_eq!(line.text.chars().count(), 20);
        assert!(line.text.starts_with("TOTAL"));
        assert!(line.text.ends_with("116.00"));
    }

    #[test]
    fn columns_never_collapse_to_nothing_when_the_text_overflows() {
        let line = PrintLine::columns("UN NOMBRE MUY LARGO", "1234567890", 10);
        assert!(line.text.contains(' '), "a separator always survives");
    }

    /// Nothing is reachable at a port nobody is listening on, and that is
    /// `HARDWARE_UNAVAILABLE` rather than a panic or a hang.
    #[test]
    fn an_unreachable_printer_reports_unavailable() {
        let printer = EscPosTcpPrinter::new(EscPosConfig {
            // Port 1 on loopback: nothing listens there.
            ..EscPosConfig::new("127.0.0.1", 1)
        });
        let error = printer.probe().unwrap_err();
        assert_eq!(error.code(), "HARDWARE_UNAVAILABLE");
        assert!(!error.to_string().contains("127.0.0.1"));
    }

    #[test]
    fn an_address_that_does_not_resolve_reports_unavailable() {
        let printer = EscPosTcpPrinter::new(EscPosConfig::new(
            "printer.invalid.pi-edge.test",
            EscPosConfig::DEFAULT_PORT,
        ));
        assert_eq!(printer.probe().unwrap_err().code(), "HARDWARE_UNAVAILABLE");
    }

    fn window(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    // ----------------------------------------------------------- DLE EOT
    //
    // The bytes in these tests are the ones the spec defines, written as
    // binary so the bit under test is visible. `0b0001_0010` is the
    // all-clear reply: the four fixed bits and nothing else.

    /// A healthy printer, by the book.
    const IDLE: u8 = 0b0001_0010;

    #[test]
    fn each_question_is_dle_eot_with_its_own_number() {
        assert_eq!(StatusQuery::Printer.request(), [DLE, EOT, 1]);
        assert_eq!(StatusQuery::OfflineCause.request(), [DLE, EOT, 2]);
        assert_eq!(StatusQuery::Error.request(), [DLE, EOT, 3]);
        assert_eq!(StatusQuery::PaperSensor.request(), [DLE, EOT, 4]);
        assert_eq!(StatusQuery::ALL.len(), 4);
    }

    /// The fixed bits are the only defence against reading noise as a fault.
    #[test]
    fn only_a_byte_with_the_fixed_bits_is_a_reply() {
        assert!(is_status_byte(IDLE));
        assert!(is_status_byte(0b0111_0110), "every variable bit set");
        // bit 1 clear, bit 4 clear, bit 0 set, bit 7 set: each disqualifies.
        assert!(!is_status_byte(0b0001_0000));
        assert!(!is_status_byte(0b0000_0010));
        assert!(!is_status_byte(0b0001_0011));
        assert!(!is_status_byte(0b1001_0010));
        assert!(!is_status_byte(0x00));
        assert!(!is_status_byte(0xFF));
    }

    #[test]
    fn a_printer_with_nothing_wrong_reads_online_with_paper() {
        let status = read_status(Some(IDLE), Some(IDLE), Some(IDLE), Some(IDLE));
        assert_eq!(status.state, PrinterState::Online);
        assert!(status.online);
        assert!(!status.cover_open);
        assert_eq!(status.paper, PaperLevel::Ok);
        assert!(!status.jammed && !status.cutter_error && !status.unrecoverable_error);
        assert_eq!(status.source, StatusSource::DleEot);
        assert_eq!(status.error, None);
        assert!(!status.blocks_payment(), "a healthy printer blocked a sale");
    }

    /// No first byte is no printer. Not an online one.
    #[test]
    fn a_printer_that_says_nothing_is_no_response_and_stops_a_payment() {
        let status = read_status(None, None, None, None);
        assert_eq!(status.state, PrinterState::NoResponse);
        assert!(!status.online);
        assert_eq!(status.paper, PaperLevel::Unknown);
        assert!(status.error.is_some());
        assert!(status.blocks_payment());
    }

    /// n=2 bit 2: the cover.
    #[test]
    fn an_open_cover_is_offline_and_stops_a_payment() {
        let status = read_status(
            Some(IDLE | 0b0000_1000),
            Some(IDLE | 0b0000_0100),
            Some(IDLE),
            Some(IDLE),
        );
        assert!(status.cover_open);
        assert_eq!(status.state, PrinterState::Offline);
        assert!(!status.online);
        assert!(status.blocks_payment());
        assert_eq!(status.error.as_deref(), Some("the cover is open"));
    }

    /// n=4 bits 5-6: the roll has run out.
    #[test]
    fn paper_at_its_end_stops_a_payment() {
        let status = read_status(
            Some(IDLE),
            Some(IDLE | 0b0010_0000),
            Some(IDLE),
            Some(IDLE | 0b0110_0000),
        );
        assert_eq!(status.paper, PaperLevel::End);
        assert_eq!(status.state, PrinterState::Offline);
        assert!(status.blocks_payment());
        assert_eq!(status.error.as_deref(), Some("the paper has run out"));
    }

    /// n=4 bits 2-3: near the end, which is a warning and nothing more.
    #[test]
    fn paper_near_its_end_warns_and_still_sells() {
        let status = read_status(Some(IDLE), Some(IDLE), Some(IDLE), Some(IDLE | 0b0000_1100));
        assert_eq!(status.paper, PaperLevel::NearEnd);
        assert_eq!(status.state, PrinterState::Online);
        assert!(status.online);
        assert!(
            !status.blocks_payment(),
            "a low roll refused a customer who could have been served"
        );
    }

    /// n=3, bit by bit.
    #[test]
    fn the_error_byte_separates_a_jam_a_cutter_and_something_unclearable() {
        let jam = read_status(Some(IDLE), Some(IDLE), Some(IDLE | 0b0000_0100), Some(IDLE));
        assert!(jam.jammed && !jam.cutter_error && !jam.unrecoverable_error);
        assert_eq!(jam.state, PrinterState::Error);

        let cutter = read_status(Some(IDLE), Some(IDLE), Some(IDLE | 0b0000_1000), Some(IDLE));
        assert!(cutter.cutter_error && !cutter.jammed);
        assert_eq!(cutter.state, PrinterState::Error);
        assert_eq!(cutter.error.as_deref(), Some("the cutter is stuck"));

        let dead = read_status(Some(IDLE), Some(IDLE), Some(IDLE | 0b0010_0000), Some(IDLE));
        assert!(dead.unrecoverable_error);
        assert_eq!(dead.state, PrinterState::Error);
    }

    /// An error is not a reason to refuse money — a cover, no paper and
    /// silence are. The rule is written once and this is it, stated as a
    /// table so a change to it has to be deliberate.
    #[test]
    fn only_a_cover_no_paper_or_no_answer_stops_a_payment() {
        let jam = read_status(Some(IDLE), Some(IDLE), Some(IDLE | 0b0000_0100), Some(IDLE));
        assert!(
            !jam.blocks_payment(),
            "a jam is cleared by hand in seconds; the rule names three things"
        );

        for (what, status) in [
            (
                "an open cover",
                read_status(Some(IDLE), Some(IDLE | 0b0000_0100), Some(IDLE), Some(IDLE)),
            ),
            (
                "no paper",
                read_status(Some(IDLE), Some(IDLE), Some(IDLE), Some(IDLE | 0b0110_0000)),
            ),
            ("no answer", read_status(None, None, None, None)),
            (
                "a printer that took itself offline",
                read_status(Some(IDLE | 0b0000_1000), None, None, Some(IDLE)),
            ),
        ] {
            assert!(status.blocks_payment(), "{what} did not stop a payment");
        }
    }

    /// A printer whose roll sensor says nothing, that stopped because the
    /// paper ran out, has no paper. The 58mm kiosk units are like this.
    #[test]
    fn a_stop_for_paper_end_is_no_paper_even_with_no_roll_sensor() {
        let status = read_status(
            Some(IDLE | 0b0000_1000),
            Some(IDLE | 0b0010_0000),
            Some(IDLE),
            None,
        );
        assert_eq!(status.paper, PaperLevel::End);
        assert!(status.blocks_payment());
    }

    /// A partial answer is a partial answer, not a healthy printer.
    #[test]
    fn a_printer_that_answers_only_the_first_question_claims_nothing_else() {
        let status = read_status(Some(IDLE), None, None, None);
        assert_eq!(status.state, PrinterState::Online);
        assert_eq!(status.paper, PaperLevel::Unknown, "paper was invented");
        assert!(!status.cover_open, "a cover state was invented");
        assert!(!status.blocks_payment());
    }

    /// Every state and level has a stable wire spelling: `/demo/status`
    /// publishes these and a floor app matches on them.
    #[test]
    fn the_published_spellings_are_the_ones_the_contract_names() {
        assert_eq!(PaperLevel::Ok.as_str(), "OK");
        assert_eq!(PaperLevel::NearEnd.as_str(), "NEAR_END");
        assert_eq!(PaperLevel::End.as_str(), "END");
        assert_eq!(PaperLevel::Unknown.as_str(), "UNKNOWN");
        assert_eq!(PrinterState::Online.as_str(), "ONLINE");
        assert_eq!(PrinterState::Offline.as_str(), "OFFLINE");
        assert_eq!(PrinterState::Error.as_str(), "ERROR");
        assert_eq!(PrinterState::NoResponse.as_str(), "NO_RESPONSE");
        assert_eq!(PrinterState::Unknown.as_str(), "UNKNOWN");
        assert_eq!(StatusSource::DleEot.as_str(), "DLE_EOT");
        assert_eq!(StatusSource::Spooler.as_str(), "SPOOLER");
        assert_eq!(StatusSource::None.as_str(), "NONE");
    }

    /// Nothing asked is not a working printer, and does not stop a sale
    /// either: an installation with no status channel knows nothing.
    #[test]
    fn an_unasked_printer_is_unknown_and_neither_blocks_nor_reassures() {
        let status = PrinterStatus::unknown();
        assert_eq!(status.state, PrinterState::Unknown);
        assert!(!status.online);
        assert_eq!(status.source, StatusSource::None);
        assert!(!status.blocks_payment());
    }
}
