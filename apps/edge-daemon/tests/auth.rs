//! Local client authentication at the IPC boundary.
//!
//! Covers the whole handshake surface: a good credential, a wrong one, a
//! missing one, a malformed frame, a request that skips the handshake, and the
//! guarantee that the credential never shows up in a log or an error.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::{client, open_register, DaemonProcess};
use edge_client::{EdgeClient, IpcEdgeClient};
use edge_ipc::credentials::{AuthToken, LocalCredentialProvider};
use edge_ipc::{DesktopLocalTransport, IpcError, RuntimePaths};
use edge_protocol::error_codes;

/// Credential provider that hands out whatever the test wants.
#[derive(Debug)]
struct StubCredential(Result<String, &'static str>);

impl LocalCredentialProvider for StubCredential {
    fn load(&self) -> Result<AuthToken, IpcError> {
        match &self.0 {
            Ok(token) => AuthToken::from_hex(token),
            Err(reason) => Err(IpcError::Credential {
                reason: (*reason).to_owned(),
            }),
        }
    }

    fn load_or_create(&self) -> Result<AuthToken, IpcError> {
        self.load()
    }
}

fn client_with_credential(database: &std::path::Path, credential: StubCredential) -> IpcEdgeClient {
    let paths = RuntimePaths::for_database(database).unwrap();
    let transport = DesktopLocalTransport::new(
        paths.endpoint().unwrap(),
        Arc::new(credential),
        "auth-test",
        "0.0.0",
    );
    IpcEdgeClient::new(Arc::new(transport))
}

fn stored_token(database: &std::path::Path) -> String {
    let paths = RuntimePaths::for_database(database).unwrap();
    std::fs::read_to_string(paths.token_file())
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn a_valid_credential_is_accepted_and_commands_are_served() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let client = client(&database, "edge-cli");
    assert!(client.core_info().is_ok());
    open_register(&client);
    assert!(client.start_transaction(None).is_ok());
    assert!(client.get_edge_state().unwrap().business_day.is_some());
}

#[test]
fn a_wrong_credential_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let client = client_with_credential(&database, StubCredential(Ok("0".repeat(64))));
    let error = client.core_info().unwrap_err();
    assert_eq!(error.code(), error_codes::AUTHENTICATION_FAILED);
    // The refusal says nothing about the real credential.
    let rendered = error.to_string();
    assert!(!rendered.contains(&stored_token(&database)));
    assert!(!rendered.contains("token"), "{rendered}");
}

#[test]
fn a_missing_credential_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let client =
        client_with_credential(&database, StubCredential(Err("no local credential found")));
    let error = client.core_info().unwrap_err();
    assert_eq!(error.code(), error_codes::AUTHENTICATION_FAILED);
}

#[test]
fn every_connection_authenticates_independently() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let good = client(&database, "edge-cli");
    open_register(&good);

    // A rejected client cannot ride on someone else's authenticated session.
    let bad = client_with_credential(&database, StubCredential(Ok("f".repeat(64))));
    assert_eq!(
        bad.core_info().unwrap_err().code(),
        error_codes::AUTHENTICATION_FAILED
    );

    // The event stream is a second connection and must authenticate too.
    let stream = good.subscribe();
    assert!(stream.is_ok(), "{:?}", stream.err());
    let bad_stream = bad.subscribe();
    assert!(bad_stream.is_err());

    // The legitimate client is unaffected by the rejected ones.
    assert!(good.get_edge_state().unwrap().session.is_some());
}

#[cfg(unix)]
mod raw_frames {
    //! These tests speak the wire directly, which is the only way to send a
    //! handshake that no client would ever produce.
    #![allow(clippy::unwrap_used)]

    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    use super::*;

    fn exchange(endpoint: &str, request: &str) -> serde_json::Value {
        let mut stream = UnixStream::connect(endpoint).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim())
            .unwrap_or_else(|error| panic!("response was not a JSON frame ({error}): {line:?}"))
    }

    #[test]
    fn a_malformed_handshake_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("pi-edge.db");
        let daemon = DaemonProcess::start(&database);

        let response = exchange(&daemon.endpoint, "this is not a frame");
        assert_eq!(response["frame"], "RESPONSE");
        assert_eq!(response["success"], false);
        assert_eq!(response["error"]["code"], error_codes::MALFORMED_FRAME);
    }

    #[test]
    fn a_request_before_the_handshake_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("pi-edge.db");
        let daemon = DaemonProcess::start(&database);

        let response = exchange(
            &daemon.endpoint,
            r#"{"frame":"REQUEST","protocol_version":1,"request_id":"00000000-0000-0000-0000-000000000001","operation":"GET_EDGE_STATE"}"#,
        );
        assert_eq!(response["success"], false);
        assert_eq!(response["error"]["code"], error_codes::NOT_AUTHENTICATED);
    }

    #[test]
    fn a_handshake_with_a_future_protocol_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("pi-edge.db");
        let daemon = DaemonProcess::start(&database);
        let token = stored_token(&database);

        let response = exchange(
            &daemon.endpoint,
            &format!(
                r#"{{"frame":"HELLO","protocol_version":99,"client_name":"c","client_version":"0","auth_token":"{token}"}}"#
            ),
        );
        assert_eq!(response["success"], false);
        assert_eq!(
            response["error"]["code"],
            error_codes::PROTOCOL_VERSION_MISMATCH
        );
    }

    #[test]
    fn an_empty_credential_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("pi-edge.db");
        let daemon = DaemonProcess::start(&database);

        let response = exchange(
            &daemon.endpoint,
            r#"{"frame":"HELLO","protocol_version":1,"client_name":"c","client_version":"0","auth_token":""}"#,
        );
        assert_eq!(response["success"], false);
        assert_eq!(
            response["error"]["code"],
            error_codes::AUTHENTICATION_FAILED
        );
    }
}

#[test]
fn the_credential_never_appears_in_logs_or_errors() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    // The binding's mutability is conditional because its only mutable use
    // is: `stop_gracefully` takes `&mut self` and is Unix-only, so on Windows
    // a `mut` here would be unused and `-D warnings` is right to say so.
    #[cfg(unix)]
    let mut daemon = DaemonProcess::start(&database);
    #[cfg(not(unix))]
    let daemon = DaemonProcess::start(&database);
    let token = stored_token(&database);
    assert_eq!(token.len(), 64);

    let client = client(&database, "edge-cli");
    open_register(&client);
    let bad = client_with_credential(&database, StubCredential(Ok("a".repeat(64))));
    let rejection = bad.core_info().unwrap_err().to_string();

    // Exercise a refusal and a successful diagnostic query as well.
    let refusal = client.open_business_day(None).unwrap_err().to_string();
    let core_info = serde_json::to_string(&client.core_info().unwrap()).unwrap();
    let state = serde_json::to_string(&client.get_edge_state().unwrap()).unwrap();

    #[cfg(unix)]
    let _ = daemon.stop_gracefully();
    let logs = daemon.stderr_contents();
    assert!(
        logs.contains("client authenticated"),
        "logs look empty: {logs}"
    );

    for (what, haystack) in [
        ("daemon logs", logs.as_str()),
        ("rejection error", rejection.as_str()),
        ("refusal error", refusal.as_str()),
        ("core info", core_info.as_str()),
        ("edge state", state.as_str()),
    ] {
        assert!(
            !haystack.contains(&token),
            "the local credential leaked into {what}"
        );
    }
}
