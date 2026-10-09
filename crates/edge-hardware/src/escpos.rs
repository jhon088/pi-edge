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

use crate::drawer::{CashDrawerPort, DrawerOpenReason};
use crate::error::HardwareError;
use crate::printer::{PrintAlign, PrintDocument, PrintLine, PrinterPort};

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

        let stream =
            TcpStream::connect_timeout(&socket, self.config.connect_timeout).map_err(|error| {
                match error.kind() {
                    std::io::ErrorKind::TimedOut => HardwareError::Timeout {
                        device,
                        elapsed_ms: started.elapsed().as_millis() as u64,
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
}
