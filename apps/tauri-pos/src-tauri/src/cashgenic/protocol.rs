//! The CashGenic API, as *CashGenic API Specification v1* (Innovative
//! Technology, 2025) writes it — and nothing this module wishes it said.
//!
//! ```text
//! POST /token                    form-urlencoded password grant      p. 9
//! GET  /system                   Bearer                              p. 11
//! POST /session                  Bearer, {"request":…,"value":…}     p. 15
//! GET  /status?eventVersion=2    Bearer, events since the last call  p. 45
//! ```
//!
//! Every session command is a `POST /session` whose body names the request.
//! There is no `/PayAmount` and no `/CloseSession` route, and an amount is an
//! **integer in minor units** — `{"request":"PayAmount","value":12345}` for
//! $123.45. A `200` only says the request was carried: whether the device
//! *did* it is `responseCode`, and only `0` is yes (p. 94).
//!
//! Money in these payloads is always an integer of minor units, so nothing
//! here parses a decimal, and nothing reads a field the document does not
//! define.

use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::showroom::CashGenicSettingsV1;

pub(super) const TOKEN_PATH: &str = "/token";
pub(super) const SYSTEM_PATH: &str = "/system";
pub(super) const SESSION_PATH: &str = "/session";
/// Version 2 adds `NewPayment` and the other `New…` events (p. 92). Asked for
/// explicitly, because without the parameter the device answers version 1.
pub(super) const STATUS_PATH: &str = "/status?eventVersion=2";

/// What a request carries, and therefore its `Content-Type`.
///
/// `/token` takes a form body (p. 9); everything else takes JSON. Sending
/// JSON to `/token` is what the first version of this client did, and the
/// device refuses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Body {
    /// `application/x-www-form-urlencoded`, in the order given — a form body
    /// is an ordered list, and the order is part of what a test pins.
    Form(Vec<(&'static str, String)>),
    Json(Value),
}

impl Body {
    pub(super) fn content_type(&self) -> &'static str {
        match self {
            Self::Form(_) => "application/x-www-form-urlencoded",
            Self::Json(_) => "application/json",
        }
    }

    pub(super) fn encode(&self) -> String {
        match self {
            Self::Form(fields) => fields
                .iter()
                .map(|(name, value)| format!("{name}={}", form_encode(value)))
                .collect::<Vec<_>>()
                .join("&"),
            Self::Json(value) => value.to_string(),
        }
    }
}

/// The OAuth2 password grant `/token` takes.
pub(super) fn grant(settings: &CashGenicSettingsV1) -> Body {
    Body::Form(vec![
        ("grant_type", "password".to_owned()),
        ("username", settings.username.clone()),
        ("password", settings.password.clone()),
    ])
}

/// Percent-encode one form value: unreserved characters pass, a space is
/// `+`, everything else is `%XX`. The credential is typed by whoever set the
/// kiosk up, and a password containing `&` must not become two fields.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `access_token` out of a `/token` answer (p. 9).
///
/// `expires_in` is deliberately not read. The document calls it "UTC in
/// milliseconds since epoch" and its own example is `43200000` — twelve hours
/// as a duration, or the first half-day of 1970 as an instant. A token is
/// kept until the device refuses it with `401` instead of until a guess.
pub(super) fn access_token(answer: &Value) -> Option<String> {
    answer
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// The session commands this kiosk issues (p. 16).
///
/// `StopPayment` and the refund, refill, collect and empty sessions are in
/// the document and not here: nothing in PX Kiosk asks for them. The events a
/// stop produces are still understood, because a stop can be started from
/// the device itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionRequest {
    /// An amount in minor units: `12345` is $123.45.
    PayAmount(i64),
    CancelPayment,
    CloseSession,
}

impl SessionRequest {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::PayAmount(_) => "PayAmount",
            Self::CancelPayment => "CancelPayment",
            Self::CloseSession => "CloseSession",
        }
    }

    pub(super) fn body(self) -> Body {
        Body::Json(match self {
            Self::PayAmount(value) => json!({ "request": "PayAmount", "value": value }),
            other => json!({ "request": other.name() }),
        })
    }
}

/// `responseCode` (p. 94). None of these is the network: a device that
/// answers `11` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseCode {
    Ok,
    InSession,
    OutOfService,
    InvalidAmount,
    SessionNotActive,
    SessionActive,
    AdminActive,
    InvalidRequest,
    Unknown(i64),
}

impl ResponseCode {
    fn from_code(code: i64) -> Self {
        match code {
            0 => Self::Ok,
            10 => Self::InSession,
            11 => Self::OutOfService,
            12 => Self::InvalidAmount,
            13 => Self::SessionNotActive,
            14 => Self::SessionActive,
            15 => Self::AdminActive,
            16 => Self::InvalidRequest,
            other => Self::Unknown(other),
        }
    }

    /// The constant's name in the document, for a log line a person can look
    /// up.
    pub(crate) fn constant(self) -> String {
        match self {
            Self::Ok => "REQUEST_OK".to_owned(),
            Self::InSession => "REQUEST_FAIL_IN_SESSION".to_owned(),
            Self::OutOfService => "REQUEST_FAIL_OUT_OF_SERVICE".to_owned(),
            Self::InvalidAmount => "REQUEST_FAIL_INVALID_AMOUNT".to_owned(),
            Self::SessionNotActive => "REQUEST_FAIL_SESSION_NOT_ACTIVE".to_owned(),
            Self::SessionActive => "REQUEST_FAIL_SESSION_ACTIVE".to_owned(),
            Self::AdminActive => "REQUEST_FAIL_ADMIN_ACTIVE".to_owned(),
            Self::InvalidRequest => "REQUEST_FAIL_INVALID_REQUEST".to_owned(),
            Self::Unknown(code) => format!("UNKNOWN_{code}"),
        }
    }

    /// The `BridgeError` code a screen receives when a command is refused.
    pub(crate) fn error_code(self) -> &'static str {
        match self {
            Self::Ok => "CASHGENIC_OK",
            Self::InSession => "CASHGENIC_IN_SESSION",
            Self::OutOfService => "CASHGENIC_OUT_OF_SERVICE",
            Self::InvalidAmount => "CASHGENIC_INVALID_AMOUNT",
            Self::SessionNotActive => "CASHGENIC_SESSION_NOT_ACTIVE",
            Self::SessionActive => "CASHGENIC_SESSION_ACTIVE",
            Self::AdminActive => "CASHGENIC_ADMIN_ACTIVE",
            Self::InvalidRequest => "CASHGENIC_INVALID_REQUEST",
            Self::Unknown(_) => "CASHGENIC_UNKNOWN_RESPONSE",
        }
    }
}

/// What a `/session` request answered: `responseCode` and `responseData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionAnswer {
    pub(super) code: ResponseCode,
    /// The transaction id for `PayAmount`, a sentence for the others.
    pub(super) data: String,
}

impl SessionAnswer {
    /// `None` when there is no `responseCode` — a 200 without one is not an
    /// acceptance, it is something this client does not understand.
    pub(super) fn read(answer: &Value) -> Option<Self> {
        let code = match answer.get("responseCode")? {
            Value::Number(number) => number.as_i64()?,
            Value::String(text) => text.trim().parse().ok()?,
            _ => return None,
        };
        let data = match answer.get("responseData") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        Some(Self {
            code: ResponseCode::from_code(code),
            data,
        })
    }
}

/// A `/status` answer (p. 46).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct Status {
    #[serde(default)]
    pub(super) events: Vec<Event>,
    #[serde(rename = "cashStatus", default)]
    pub(super) cash_status: Option<CashStatus>,
    #[serde(rename = "faceDeviceState", default)]
    pub(super) face_device_state: Option<String>,
}

/// One entry of `events` (p. 47).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct Event {
    #[serde(default)]
    pub(super) event: String,
    #[serde(default)]
    pub(super) timestamp: Option<String>,
    #[serde(default, deserialize_with = "integer")]
    pub(super) value: Option<i64>,
    #[serde(rename = "lowFloatLevelWarning", default)]
    pub(super) low_float_level_warning: Option<bool>,
    #[serde(rename = "errorCondition", default)]
    pub(super) error_condition: Option<String>,
    #[serde(default)]
    pub(super) transaction: Option<Transaction>,
}

impl Event {
    /// The transaction id it belongs to, empty when it belongs to none.
    pub(super) fn transaction_id(&self) -> &str {
        self.transaction
            .as_ref()
            .and_then(|transaction| transaction.transaction_id.as_deref())
            .unwrap_or_default()
    }

    /// Whether the device says a session is in progress: `transaction_type`
    /// is "an empty string when there is no session active" (p. 47).
    pub(super) fn in_session(&self) -> bool {
        self.transaction
            .as_ref()
            .and_then(|transaction| transaction.transaction_type.as_deref())
            .is_some_and(|kind| !kind.trim().is_empty())
    }
}

/// `transaction` inside an event (p. 47). Amounts in minor units.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct Transaction {
    #[serde(default)]
    pub(super) transaction_type: Option<String>,
    #[serde(default, deserialize_with = "integer")]
    pub(super) request_amount: Option<i64>,
    #[serde(default)]
    pub(super) transaction_id: Option<String>,
    #[serde(default, deserialize_with = "integer")]
    pub(super) cash_in: Option<i64>,
    #[serde(default, deserialize_with = "integer")]
    pub(super) cash_out: Option<i64>,
}

/// `cashStatus` (p. 47). The total is what a floor monitor shows; the per
/// device split is not read by anything here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct CashStatus {
    #[serde(rename = "totalSystemCash", default, deserialize_with = "integer")]
    pub(super) total_system_cash: Option<i64>,
}

/// A `/system` answer (p. 11), the parts a stand acts on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct System {
    #[serde(rename = "systemVersion", default)]
    pub(super) system_version: Option<String>,
    #[serde(rename = "paymentDevices", default)]
    pub(super) payment_devices: Vec<PaymentDevice>,
}

/// One element of `paymentDevices` (p. 12).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct PaymentDevice {
    #[serde(rename = "isConnected", default)]
    pub(super) is_connected: Option<bool>,
    /// `Bills` or `Coins`.
    #[serde(rename = "paymentType", default)]
    pub(super) payment_type: Option<String>,
    #[serde(rename = "firmwareVersion", default)]
    pub(super) firmware_version: Option<String>,
    #[serde(rename = "serialNumber", default)]
    pub(super) serial_number: Option<String>,
    #[serde(rename = "countryCode", default)]
    pub(super) country_code: Option<String>,
    #[serde(rename = "cashboxValue", default, deserialize_with = "integer")]
    pub(super) cashbox_value: Option<i64>,
    #[serde(rename = "storedValue", default, deserialize_with = "integer")]
    pub(super) stored_value: Option<i64>,
}

impl System {
    /// What to call the unit: the document gives a version, not a model.
    pub(super) fn label(&self) -> Option<String> {
        self.system_version
            .as_deref()
            .map(|version| format!("CashGenic {version}"))
    }

    /// Which payment devices say `isConnected: false`, in words. Absent is
    /// "did not say", never "not connected".
    pub(super) fn disconnected(&self) -> Option<String> {
        let names: Vec<&str> = self
            .payment_devices
            .iter()
            .filter(|device| device.is_connected == Some(false))
            .map(|device| match device.payment_type.as_deref() {
                Some(kind) if kind.eq_ignore_ascii_case("bills") => "billetes",
                Some(kind) if kind.eq_ignore_ascii_case("coins") => "monedas",
                _ => "pago",
            })
            .collect();
        (!names.is_empty()).then(|| format!("el módulo de {} no está conectado", names.join(" y ")))
    }
}

/// An integer of minor units, or absent. A JSON float is *absent* rather than
/// rounded: the document says Integer everywhere, and money does not pass
/// through `f64` in this runtime (CLAUDE.md).
fn integer<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    /// p. 46, with the PDF's line breaks and the elided field put back.
    const SPEC_STATUS: &str = r#"{
        "cashStatus": {
            "billsCashbox": 0, "billsStored": 0, "coinsCashbox": 0, "coinsStored": 0,
            "collectableBillValue": 0, "collectableCoinValue": 0,
            "totalCollectableCash": 0, "totalSystemCash": 0
        },
        "events": [{
            "errorCondition": "None",
            "event": "Idle",
            "lowFloatLevelWarning": false,
            "timestamp": "2023-07-06 12:24:56",
            "transaction": {
                "cash_in": 0, "cash_out": 0, "request_amount": 0,
                "transaction_id": "", "transaction_type": ""
            },
            "value": 0
        }],
        "faceDeviceState": "in-service"
    }"#;

    #[test]
    fn pay_amount_is_a_post_to_session_with_an_integer_value() {
        let body = SessionRequest::PayAmount(12_345).body();
        assert_eq!(body.content_type(), "application/json");
        let sent: Value = serde_json::from_str(&body.encode()).unwrap();
        assert_eq!(sent, json!({ "request": "PayAmount", "value": 12345 }));
        assert!(sent["value"].is_i64(), "an integer, never \"123.45\"");
        assert!(sent.get("amount").is_none());
    }

    #[test]
    fn every_other_command_names_itself_and_carries_nothing_else() {
        for (request, name) in [
            (SessionRequest::CancelPayment, "CancelPayment"),
            (SessionRequest::CloseSession, "CloseSession"),
        ] {
            let sent: Value = serde_json::from_str(&request.body().encode()).unwrap();
            assert_eq!(sent, json!({ "request": name }));
        }
    }

    #[test]
    fn the_grant_is_form_encoded_and_a_password_cannot_split_it() {
        let settings = CashGenicSettingsV1 {
            username: "api user".to_owned(),
            password: "p&a=s s/w?rd+1".to_owned(),
            ..CashGenicSettingsV1::default()
        };
        let body = grant(&settings);
        assert_eq!(body.content_type(), "application/x-www-form-urlencoded");
        assert_eq!(
            body.encode(),
            "grant_type=password&username=api+user&password=p%26a%3Ds+s%2Fw%3Frd%2B1"
        );
        assert_eq!(form_encode("ApiUserOne-1_2.3~4"), "ApiUserOne-1_2.3~4");
    }

    #[test]
    fn the_token_is_access_token_and_expires_in_is_not_interpreted() {
        let answer = json!({
            "access_token": "23409e6e-9330",
            "expires_in": 43200000,
            "token_type": "bearer",
            "userName": "ApiUserOne"
        });
        assert_eq!(access_token(&answer).as_deref(), Some("23409e6e-9330"));
        assert_eq!(access_token(&json!({ "token": "invented" })), None);
        assert_eq!(access_token(&json!({ "access_token": "" })), None);
    }

    #[test]
    fn every_response_code_in_the_appendix_is_mapped() {
        let table = [
            (0, ResponseCode::Ok, "REQUEST_OK"),
            (10, ResponseCode::InSession, "REQUEST_FAIL_IN_SESSION"),
            (
                11,
                ResponseCode::OutOfService,
                "REQUEST_FAIL_OUT_OF_SERVICE",
            ),
            (
                12,
                ResponseCode::InvalidAmount,
                "REQUEST_FAIL_INVALID_AMOUNT",
            ),
            (
                13,
                ResponseCode::SessionNotActive,
                "REQUEST_FAIL_SESSION_NOT_ACTIVE",
            ),
            (
                14,
                ResponseCode::SessionActive,
                "REQUEST_FAIL_SESSION_ACTIVE",
            ),
            (15, ResponseCode::AdminActive, "REQUEST_FAIL_ADMIN_ACTIVE"),
            (
                16,
                ResponseCode::InvalidRequest,
                "REQUEST_FAIL_INVALID_REQUEST",
            ),
        ];
        for (code, expected, constant) in table {
            let answer =
                SessionAnswer::read(&json!({ "responseCode": code, "responseData": "x" })).unwrap();
            assert_eq!(answer.code, expected);
            assert_eq!(answer.code.constant(), constant);
        }
        assert_eq!(
            SessionAnswer::read(&json!({ "responseCode": 99 }))
                .unwrap()
                .code,
            ResponseCode::Unknown(99)
        );
    }

    #[test]
    fn a_200_without_a_response_code_is_not_an_acceptance() {
        assert_eq!(SessionAnswer::read(&json!({ "state": "OK" })), None);
        assert_eq!(SessionAnswer::read(&json!({})), None);
        let paid = SessionAnswer::read(&json!({
            "responseCode": 0,
            "responseData": "d3ac57bb-fa6d-467c-aef4-913e85c1defa"
        }))
        .unwrap();
        assert_eq!(paid.code, ResponseCode::Ok);
        assert_eq!(paid.data, "d3ac57bb-fa6d-467c-aef4-913e85c1defa");
    }

    #[test]
    fn the_status_in_the_document_is_read_as_the_document_writes_it() {
        let status: Status = serde_json::from_str(SPEC_STATUS).unwrap();
        assert_eq!(status.events.len(), 1);
        let idle = &status.events[0];
        assert_eq!(idle.event, "Idle");
        assert_eq!(idle.error_condition.as_deref(), Some("None"));
        assert_eq!(idle.timestamp.as_deref(), Some("2023-07-06 12:24:56"));
        assert_eq!(idle.low_float_level_warning, Some(false));
        assert_eq!(idle.value, Some(0));
        assert!(!idle.in_session());
        assert_eq!(idle.transaction_id(), "");
        assert_eq!(status.cash_status.unwrap().total_system_cash, Some(0));
        assert_eq!(status.face_device_state.as_deref(), Some("in-service"));
    }

    #[test]
    fn invented_root_fields_are_not_part_of_the_status() {
        // What the old client read: none of it exists in the document, and
        // none of it reaches the model.
        let status: Status = serde_json::from_str(
            r#"{"state":"COMPLETE","deposited":200.0,"change":12.5,"cashTotal":9,"errorCondition":"BillJam"}"#,
        )
        .unwrap();
        assert_eq!(status, Status::default());
    }

    #[test]
    fn a_payment_event_carries_its_transaction_in_minor_units() {
        let status: Status = serde_json::from_value(json!({
            "events": [{
                "event": "Payment", "errorCondition": "None", "value": 50000,
                "transaction": {
                    "transaction_type": "Payment", "request_amount": 12345,
                    "transaction_id": "tx-1", "cash_in": 50000, "cash_out": 0
                }
            }]
        }))
        .unwrap();
        let payment = &status.events[0];
        assert!(payment.in_session());
        assert_eq!(payment.transaction_id(), "tx-1");
        let transaction = payment.transaction.as_ref().unwrap();
        assert_eq!(transaction.cash_in, Some(50_000));
        assert_eq!(transaction.request_amount, Some(12_345));
        // A float is not money here; it is absent.
        let float: Transaction = serde_json::from_value(json!({ "cash_in": 500.5 })).unwrap();
        assert_eq!(float.cash_in, None);
    }

    #[test]
    fn system_reads_payment_devices_is_connected() {
        let system: System = serde_json::from_value(json!({
            "systemVersion": "2.1.49",
            "paymentDevices": [
                { "paymentType": "Bills", "isConnected": false, "firmwareVersion": "4.10",
                  "serialNumber": "B1", "countryCode": "MXN", "cashboxValue": 10000,
                  "storedValue": 20000 },
                { "paymentType": "Coins", "isConnected": true }
            ]
        }))
        .unwrap();
        assert_eq!(system.label().as_deref(), Some("CashGenic 2.1.49"));
        assert_eq!(
            system.disconnected().as_deref(),
            Some("el módulo de billetes no está conectado")
        );
        let bills = &system.payment_devices[0];
        assert_eq!(bills.firmware_version.as_deref(), Some("4.10"));
        assert_eq!(bills.serial_number.as_deref(), Some("B1"));
        assert_eq!(bills.country_code.as_deref(), Some("MXN"));
        assert_eq!(bills.cashbox_value, Some(10_000));
        assert_eq!(bills.stored_value, Some(20_000));

        // The shape the old client invented is not read as a disconnection.
        let invented: System = serde_json::from_value(json!({
            "bills": { "connected": false }, "coins": { "connected": false }
        }))
        .unwrap();
        assert_eq!(invented.disconnected(), None);
    }
}
