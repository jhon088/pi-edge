//! One HTTPS request to the acceptor, and exactly the response it sends back.
//!
//! The request is still written by hand — five routes against a box on a
//! local switch do not justify an HTTP stack — but the response is now read
//! the way HTTP/1.1 says it ends:
//!
//! * `Transfer-Encoding: chunked` — decoded, chunk by chunk;
//! * `Content-Length: N` — exactly `N` bytes, and then **stop**;
//! * neither — up to the end of the stream, the one case where the end of
//!   the stream is the delimiter.
//!
//! The first client read every response to the end of the stream. Against a
//! device that leaves the connection open after a complete answer, that is a
//! full read budget spent on every call, and a "timeout" on an answer that
//! had already arrived.
//!
//! Three budgets, because they fail for three different reasons: the TCP
//! connect (is anything there), the TLS handshake (the device signs with a
//! key it generated itself, and the stand has shown handshakes of six and
//! seven seconds), and the response. They are deliberately generous for now;
//! they get tightened against measurements of the corrected client, not
//! before.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::protocol::Body;

/// The most any CashGenic response may be, headers included. A `/system`
/// answer with every denomination listed is a few kilobytes.
pub(super) const RESPONSE_LIMIT: usize = 256 * 1024;

/// The three budgets of one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Timeouts {
    pub(super) connect: Duration,
    pub(super) handshake: Duration,
    pub(super) response: Duration,
}

/// Conservative on purpose — see the module header.
pub(super) const TIMEOUTS: Timeouts = Timeouts {
    connect: Duration::from_secs(10),
    handshake: Duration::from_secs(15),
    response: Duration::from_secs(15),
};

/// Where a request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Resolve,
    Connect,
    Handshake,
    Write,
    Read,
    /// The bytes arrived and are not an HTTP response this client can read.
    Framing,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Connect => "tcp_connect",
            Self::Handshake => "tls_handshake",
            Self::Write => "http_write",
            Self::Read => "http_read",
            Self::Framing => "http_framing",
        }
    }
}

/// A request that did not produce a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TransportError {
    pub(super) stage: Stage,
    pub(super) detail: String,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage.name(), self.detail)
    }
}

/// A response: its status and its body, exactly as framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Response {
    pub(super) status: u16,
    pub(super) body: Vec<u8>,
}

/// Send one request over a new TLS connection and read its response.
pub(super) fn exchange(
    host: &str,
    port: u16,
    request: &[u8],
    timeouts: Timeouts,
) -> Result<Response, TransportError> {
    let address = format!("{host}:{port}");
    let fail = |stage: Stage| {
        let address = address.clone();
        move |error: io::Error| TransportError {
            stage,
            detail: format!("{address}: {}", describe(&error)),
        }
    };

    let resolved = address
        .to_socket_addrs()
        .map_err(fail(Stage::Resolve))?
        .next()
        .ok_or_else(|| TransportError {
            stage: Stage::Resolve,
            detail: format!("{address} resolves to nothing"),
        })?;
    let mut socket =
        TcpStream::connect_timeout(&resolved, timeouts.connect).map_err(fail(Stage::Connect))?;

    let mut connection = tls_connection(host).map_err(|detail| TransportError {
        stage: Stage::Handshake,
        detail: format!("{address}: {detail}"),
    })?;
    let handshake = Instant::now() + timeouts.handshake;
    while connection.is_handshaking() {
        arm(&socket, handshake).map_err(fail(Stage::Handshake))?;
        let (read, written) = connection
            .complete_io(&mut socket)
            .map_err(fail(Stage::Handshake))?;
        if read == 0 && written == 0 && connection.is_handshaking() {
            return Err(fail(Stage::Handshake)(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the device closed the connection during the handshake",
            )));
        }
    }

    let mut stream = rustls::StreamOwned::new(connection, socket);
    let deadline = Instant::now() + timeouts.response;
    arm(&stream.sock, deadline).map_err(fail(Stage::Write))?;
    stream
        .write_all(request)
        .and_then(|()| stream.flush())
        .map_err(fail(Stage::Write))?;

    read_response(
        &mut Budgeted {
            stream: &mut stream,
            deadline,
        },
        RESPONSE_LIMIT,
    )
    .map_err(|error| match error {
        ReadError::Io(error) => fail(Stage::Read)(error),
        ReadError::Framing(detail) => TransportError {
            stage: Stage::Framing,
            detail: format!("{address}: {detail}"),
        },
    })
}

/// The I/O error, with the OS code when there is one — "os error 10060" on
/// Windows is what a person will search for.
fn describe(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("{error} (kind={:?} os_error={code})", error.kind()),
        None => format!("{error} (kind={:?})", error.kind()),
    }
}

/// Set the socket's timeouts to whatever is left before `deadline`, so a
/// budget is a total for its stage rather than a limit per read.
fn arm(socket: &TcpStream, deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "budget spent"));
    }
    socket.set_read_timeout(Some(remaining))?;
    socket.set_write_timeout(Some(remaining))
}

/// A TLS stream whose every read is bounded by one deadline.
struct Budgeted<'a> {
    stream: &'a mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    deadline: Instant,
}

impl Read for Budgeted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        arm(&self.stream.sock, self.deadline)?;
        self.stream.read(buffer)
    }
}

/// An HTTP/1.1 request as bytes. `Connection: close`: one request per
/// connection, which the document does not forbid and which keeps the
/// device's side of it simple.
pub(super) fn request_bytes(
    method: &str,
    path: &str,
    address: &str,
    token: Option<&str>,
    body: Option<&Body>,
) -> Vec<u8> {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
         Accept: application/json\r\n"
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    match body {
        Some(body) => {
            let encoded = body.encode();
            request.push_str(&format!(
                "Content-Type: {}\r\nContent-Length: {}\r\n\r\n{encoded}",
                body.content_type(),
                encoded.len()
            ));
        }
        None => {
            // The document lists a JSON content type on the authenticated
            // reads too (pp. 11, 45). There is no body for it to describe,
            // so no length.
            if token.is_some() {
                request.push_str("Content-Type: application/json\r\n");
            }
            request.push_str("\r\n");
        }
    }
    request.into_bytes()
}

/// Why a response could not be read.
#[derive(Debug)]
pub(super) enum ReadError {
    Io(io::Error),
    Framing(String),
}

impl From<io::Error> for ReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn framing(detail: impl Into<String>) -> ReadError {
    ReadError::Framing(detail.into())
}

/// Read one HTTP/1.1 response off `reader` — and not one byte past its end.
pub(super) fn read_response(reader: &mut impl Read, limit: usize) -> Result<Response, ReadError> {
    let mut data = Vec::new();
    let head_end = loop {
        if let Some(at) = find(&data, b"\r\n\r\n") {
            break at + 4;
        }
        if data.len() > limit {
            return Err(framing("the headers exceed the response limit"));
        }
        if more(reader, &mut data)? == 0 {
            return Err(framing("the connection closed before the headers ended"));
        }
    };
    let head = std::str::from_utf8(&data[..head_end])
        .map_err(|_| framing("the headers are not text"))?
        .to_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|line| line.starts_with("HTTP/1."))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| framing("no HTTP status line"))?;
    let headers: Vec<(String, String)> = lines
        .filter(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let rest = data.split_off(head_end);

    // No body, whatever the headers say (RFC 9112 §6.3).
    if (100..200).contains(&status) || status == 204 || status == 304 {
        return Ok(Response {
            status,
            body: Vec::new(),
        });
    }

    let body = if let Some(coding) = header(&headers, "transfer-encoding") {
        let last = coding.rsplit(',').next().unwrap_or_default().trim();
        if !last.eq_ignore_ascii_case("chunked") {
            return Err(framing(format!("unsupported transfer coding `{coding}`")));
        }
        dechunk(reader, rest, limit)?
    } else if let Some(length) = content_length(&headers)? {
        if length > limit {
            return Err(framing(format!(
                "Content-Length {length} exceeds the {limit} byte limit"
            )));
        }
        let mut body = rest;
        while body.len() < length {
            if more(reader, &mut body)? == 0 {
                return Err(framing(format!(
                    "the body ended after {} of {length} bytes",
                    body.len()
                )));
            }
        }
        body.truncate(length);
        body
    } else {
        to_end(reader, rest, limit)?
    };
    Ok(Response { status, body })
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The one `Content-Length`, or an error if it is not a plain number or
/// several of them disagree.
fn content_length(headers: &[(String, String)]) -> Result<Option<usize>, ReadError> {
    let mut found: Option<usize> = None;
    for (_, value) in headers.iter().filter(|(key, _)| key == "content-length") {
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(framing(format!("malformed Content-Length `{value}`")));
            }
            let length: usize = part
                .parse()
                .map_err(|_| framing(format!("malformed Content-Length `{value}`")))?;
            if found.is_some_and(|seen| seen != length) {
                return Err(framing("conflicting Content-Length values"));
            }
            found = Some(length);
        }
    }
    Ok(found)
}

/// A chunked body, decoded. Trailers are read and dropped; a stream that ends
/// after the last chunk without its final blank line still yields the body.
fn dechunk(reader: &mut impl Read, mut data: Vec<u8>, limit: usize) -> Result<Vec<u8>, ReadError> {
    let mut body = Vec::new();
    let mut at = 0;
    loop {
        let (line, next) = match line_at(reader, &mut data, at, limit)? {
            Some(found) => found,
            None => return Err(framing("the body ended inside a chunk header")),
        };
        at = next;
        let size_text = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| framing(format!("malformed chunk size `{size_text}`")))?;
        if size == 0 {
            while let Some((trailer, next)) = line_at(reader, &mut data, at, limit)? {
                at = next;
                if trailer.is_empty() {
                    break;
                }
            }
            return Ok(body);
        }
        if body.len().saturating_add(size) > limit {
            return Err(framing("the chunked body exceeds the response limit"));
        }
        while data.len() < at + size + 2 {
            if more(reader, &mut data)? == 0 {
                return Err(framing("the body ended inside a chunk"));
            }
        }
        body.extend_from_slice(&data[at..at + size]);
        if &data[at + size..at + size + 2] != b"\r\n" {
            return Err(framing("a chunk is not followed by CRLF"));
        }
        at += size + 2;
    }
}

/// The CRLF-terminated line starting at `at`, reading more as needed.
/// `None` when the stream ends first.
fn line_at(
    reader: &mut impl Read,
    data: &mut Vec<u8>,
    at: usize,
    limit: usize,
) -> Result<Option<(String, usize)>, ReadError> {
    loop {
        if let Some(found) = find(&data[at..], b"\r\n") {
            let line = String::from_utf8_lossy(&data[at..at + found]).into_owned();
            return Ok(Some((line, at + found + 2)));
        }
        if data.len() - at > limit {
            return Err(framing("a chunk line exceeds the response limit"));
        }
        if more(reader, data)? == 0 {
            return Ok(None);
        }
    }
}

/// A body delimited by the end of the stream. A TLS peer that closes without
/// `close_notify` is ordinary in embedded firmware, and is the end here too.
fn to_end(reader: &mut impl Read, mut body: Vec<u8>, limit: usize) -> Result<Vec<u8>, ReadError> {
    loop {
        if body.len() > limit {
            return Err(framing("the body exceeds the response limit"));
        }
        match more(reader, &mut body) {
            Ok(0) => return Ok(body),
            Ok(_) => {}
            Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Ok(body)
            }
            Err(error) => return Err(error),
        }
    }
}

/// One read, appended. `Ok(0)` is the end of the stream.
fn more(reader: &mut impl Read, data: &mut Vec<u8>) -> Result<usize, ReadError> {
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(read) => {
                data.extend_from_slice(&chunk[..read]);
                return Ok(read);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ReadError::Io(error)),
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A TLS client for one connection to the acceptor.
fn tls_connection(host: &str) -> Result<rustls::ClientConnection, String> {
    // An IP address is the normal case: `ServerName` takes it, and no SNI is
    // sent for a literal address.
    let name = rustls_pki_types::ServerName::try_from(host.to_owned())
        .map_err(|error| format!("`{host}` is not a usable server name: {error}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheDeviceOnTheBench))
        .with_no_client_auth();
    rustls::ClientConnection::new(Arc::new(config), name).map_err(|error| error.to_string())
}

/// Accepts the acceptor's self-signed certificate without checking it
/// (ADR 049).
///
/// **Read ADR 049 before reusing this.** It verifies nothing: not the name,
/// not the chain, not the expiry. It exists because a CashGenic presents a
/// certificate it signed itself, naming nothing, on a showroom LAN, to carry
/// an amount of pesos and no cardholder data. It still checks that the
/// handshake was signed by the key the certificate names.
#[derive(Debug)]
struct AcceptTheDeviceOnTheBench;

impl rustls::client::danger::ServerCertVerifier for AcceptTheDeviceOnTheBench {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::cashgenic::bench::{Bench, Script};

    /// A reader that hands out the given pieces in order and then refuses to
    /// be read again — so a test proves the reader stopped, rather than
    /// waiting to see whether it does.
    struct Pieces {
        pieces: VecDeque<Vec<u8>>,
        /// What comes after the pieces: `None` is the end of the stream,
        /// `Some` an error such as a read that would have blocked.
        after: Option<io::ErrorKind>,
    }

    impl Pieces {
        /// The socket stays open: reading past the pieces is a test failure.
        fn open(pieces: &[&[u8]]) -> Self {
            Self {
                pieces: pieces.iter().map(|piece| piece.to_vec()).collect(),
                after: Some(io::ErrorKind::Other),
            }
        }

        fn then_close(pieces: &[&[u8]]) -> Self {
            Self {
                after: None,
                ..Self::open(pieces)
            }
        }

        fn then(pieces: &[&[u8]], kind: io::ErrorKind) -> Self {
            Self {
                after: Some(kind),
                ..Self::open(pieces)
            }
        }
    }

    impl Read for Pieces {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(mut piece) = self.pieces.pop_front() else {
                return match self.after {
                    None => Ok(0),
                    Some(io::ErrorKind::Other) => {
                        Err(io::Error::other("read past the end of a complete response"))
                    }
                    Some(kind) => Err(io::Error::new(kind, "stream error")),
                };
            };
            let take = piece.len().min(buffer.len());
            buffer[..take].copy_from_slice(&piece[..take]);
            if take < piece.len() {
                self.pieces.push_front(piece.split_off(take));
            }
            Ok(take)
        }
    }

    fn read(reader: &mut impl Read) -> Result<Response, ReadError> {
        read_response(reader, RESPONSE_LIMIT)
    }

    const ANSWER: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"responseCode\"";

    #[test]
    fn content_length_ends_the_response_while_the_socket_stays_open() {
        let response = read(&mut Pieces::open(&[ANSWER])).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{\"responseCode\"");
    }

    #[test]
    fn content_length_with_a_close_reads_the_same() {
        let response = read(&mut Pieces::then_close(&[ANSWER])).unwrap();
        assert_eq!(response.body, b"{\"responseCode\"");
    }

    #[test]
    fn a_body_in_many_reads_is_put_back_together() {
        let bytes: Vec<&[u8]> = ANSWER.chunks(3).collect();
        let response = read(&mut Pieces::open(&bytes)).unwrap();
        assert_eq!(response.body, b"{\"responseCode\"");
    }

    #[test]
    fn bytes_past_content_length_are_not_the_body() {
        let response = read(&mut Pieces::open(&[
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}garbage",
        ]))
        .unwrap();
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn a_chunked_body_is_decoded() {
        let response = read(&mut Pieces::open(&[
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"8;ext=1\r\n{\"event\"",
            b"\r\n7\r\n:\"Idle\"\r\n",
            b"1\r\n}\r\n0\r\n",
            b"\r\n",
        ]))
        .unwrap();
        assert_eq!(response.body, b"{\"event\":\"Idle\"}");
    }

    #[test]
    fn a_body_without_a_length_runs_to_the_end_of_the_stream() {
        let response = read(&mut Pieces::then_close(&[
            b"HTTP/1.1 200 OK\r\n\r\n{\"a\":",
            b"1}",
        ]))
        .unwrap();
        assert_eq!(response.body, b"{\"a\":1}");
        // A close without TLS close_notify is that same end.
        let response = read(&mut Pieces::then(
            &[b"HTTP/1.1 200 OK\r\n\r\n{}"],
            io::ErrorKind::UnexpectedEof,
        ))
        .unwrap();
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn a_body_shorter_than_its_length_is_an_error() {
        let error = read(&mut Pieces::then_close(&[
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{\"a\"",
        ]))
        .unwrap_err();
        assert!(matches!(error, ReadError::Framing(ref detail) if detail.contains("4 of 10")));
        // And a timeout before it arrives is a timeout, not an answer.
        let error = read(&mut Pieces::then(
            &[b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{\"a\""],
            io::ErrorKind::TimedOut,
        ))
        .unwrap_err();
        assert!(matches!(error, ReadError::Io(ref io) if io.kind() == io::ErrorKind::TimedOut));
    }

    #[test]
    fn a_malformed_content_length_is_refused() {
        for length in ["12a", "-1", "", "1 2", "0x10"] {
            let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n{{}}");
            let error = read(&mut Pieces::open(&[raw.as_bytes()])).unwrap_err();
            assert!(matches!(error, ReadError::Framing(_)), "{length}");
        }
        let error = read(&mut Pieces::open(&[
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{}",
        ]))
        .unwrap_err();
        assert!(matches!(error, ReadError::Framing(ref detail) if detail.contains("conflicting")));
    }

    #[test]
    fn an_oversized_body_is_refused_before_it_is_read() {
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            RESPONSE_LIMIT + 1
        );
        let error = read(&mut Pieces::open(&[raw.as_bytes()])).unwrap_err();
        assert!(matches!(error, ReadError::Framing(ref detail) if detail.contains("exceeds")));

        let huge = vec![b'x'; RESPONSE_LIMIT + 10];
        let error = read(&mut Pieces::then_close(&[
            b"HTTP/1.1 200 OK\r\n\r\n",
            &huge,
        ]))
        .unwrap_err();
        assert!(matches!(error, ReadError::Framing(_)));

        let chunk = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            RESPONSE_LIMIT + 1
        );
        let error = read(&mut Pieces::open(&[chunk.as_bytes()])).unwrap_err();
        assert!(matches!(error, ReadError::Framing(_)));
    }

    #[test]
    fn a_response_with_no_status_line_is_refused() {
        let error = read(&mut Pieces::then_close(&[b"garbage\r\n\r\n"])).unwrap_err();
        assert!(matches!(error, ReadError::Framing(_)));
        let error = read(&mut Pieces::then_close(&[b"HTTP/1.1 200 OK\r\n"])).unwrap_err();
        assert!(matches!(error, ReadError::Framing(_)));
    }

    #[test]
    fn a_get_carries_no_body_and_a_post_carries_its_length() {
        let get = String::from_utf8(request_bytes(
            "GET",
            "/status?eventVersion=2",
            "h:1",
            Some("tok"),
            None,
        ))
        .unwrap();
        assert!(get.starts_with("GET /status?eventVersion=2 HTTP/1.1\r\n"));
        assert!(get.contains("Authorization: Bearer tok\r\n"));
        assert!(!get.contains("Content-Length"));
        assert!(get.ends_with("\r\n\r\n"));

        let body = Body::Json(serde_json::json!({ "request": "CloseSession" }));
        let post = String::from_utf8(request_bytes(
            "POST",
            "/session",
            "h:1",
            Some("t"),
            Some(&body),
        ))
        .unwrap();
        let (head, sent) = post.split_once("\r\n\r\n").unwrap();
        assert!(head.contains("Content-Type: application/json"));
        assert!(head.contains(&format!("Content-Length: {}", sent.len())));
        assert!(head.contains("Connection: close"));
        assert_eq!(sent, r#"{"request":"CloseSession"}"#);
    }

    /// Over real TLS, against a device that answers in full and leaves the
    /// connection open: the client is done as soon as `Content-Length` is,
    /// not when the 15 s response budget runs out.
    #[test]
    fn a_complete_answer_on_a_connection_left_open_returns_at_once() {
        let bench = Bench::start(Script {
            hold_open: true,
            ..Script::default()
        });
        let request = request_bytes("GET", "/system", &bench.address(), Some("t"), None);
        let started = Instant::now();
        let response = exchange(&bench.host(), bench.port(), &request, TIMEOUTS);
        let took = started.elapsed();
        // The bench refuses an unknown bearer; what matters is the framing.
        let response = response.unwrap();
        assert_eq!(response.status, 401);
        assert!(took < Duration::from_secs(2), "waited {took:?} for an EOF");
    }

    #[test]
    fn a_device_that_is_not_there_fails_at_the_connect() {
        // A port nothing listens on, on loopback: refused at once.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = exchange("127.0.0.1", port, b"GET / HTTP/1.1\r\n\r\n", TIMEOUTS).unwrap_err();
        assert_eq!(error.stage, Stage::Connect);
        assert!(
            error.to_string().starts_with("tcp_connect: 127.0.0.1:"),
            "{error}"
        );
    }
}
