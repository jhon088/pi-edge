//! Which conditions on this kiosk somebody has to walk over and fix.
//!
//! The floor console is a console **by exception**. A printer with paper in
//! it and an acceptor that answers are not news, and a card that lists them
//! buries the one lane that needs a person. So a floor app is not handed
//! flags to interpret — it is handed the conditions that are already wrong,
//! in the order they should be read, and it renders them verbatim. The rule
//! is the repository's: if a client needs something decided, the core decides
//! it and sends it.
//!
//! Derived from **the blocks that go on the wire**, not from a second read of
//! the devices. An alert and the flags beside it in the same payload can
//! therefore never disagree — which matters, because a console showing "sin
//! papel" next to `paper: "OK"` is worse than one showing nothing at all.
//!
//! One condition is deliberately absent. A kiosk that is off cannot report
//! that it is off: `KIOSK_OFFLINE` belongs to whoever polls `/demo/status`
//! and watches the answers stop arriving.

use serde_json::{json, Value};

/// The lane cannot take money until somebody fixes this.
const BLOCKING: &str = "BLOCKING";
/// It can, and a person is needed anyway — now, not at the close.
const ACTION: &str = "ACTION";
/// Nothing has stopped. Whoever restocks should know.
const WARN: &str = "WARN";

/// Every condition this kiosk can report, most urgent first.
///
/// The order is the contract: a card renders the list top to bottom without
/// sorting it, so the first entry is always the most urgent thing wrong.
/// Within one severity the order is fixed too — a basket with a customer
/// standing at it outranks a dead note acceptor, because somebody is waiting
/// on the first and nobody is waiting on the second.
pub(crate) fn raise(printer: &Value, cashgenic: &Value, authorization: &Value) -> Vec<Value> {
    let printer = Printer::read(printer);
    let mut raised = Vec::new();

    if printer.paper_is_out() {
        raised.push(alert("PRINTER_PAPER_END", BLOCKING, "Sin papel"));
    }
    if printer.cover_is_open() {
        raised.push(alert("PRINTER_COVER_OPEN", BLOCKING, "Tapa abierta"));
    }
    if printer.is_offline() {
        raised.push(alert("PRINTER_OFFLINE", BLOCKING, "Impresora desconectada"));
    }
    if waiting_for_a_person(authorization) {
        raised.push(alert(
            "AUTHORIZATION_REQUIRED",
            ACTION,
            "Autorización requerida",
        ));
    }
    if acceptor_is_silent(cashgenic) {
        raised.push(alert("CASHGENIC_OFFLINE", ACTION, "CashGenic desconectado"));
    }
    if printer.paper_is_low() {
        raised.push(alert("PRINTER_PAPER_LOW", WARN, "Papel bajo"));
    }

    raised
}

/// One entry, in the shape a console parses rather than reads.
///
/// `code` is what it branches on and `severity` what it paints; `message` is
/// there so a card that has no table for a code it has never seen still has
/// something to put in front of an operator.
fn alert(code: &str, severity: &str, message: &str) -> Value {
    json!({ "code": code, "severity": severity, "message": message })
}

/// The printer block, as the three conditions actually depend on it.
struct Printer<'a> {
    /// Read once, and it gates every condition below: a kiosk with no printer
    /// set up does not have a printer problem.
    configured: bool,
    state: &'a str,
    paper: &'a str,
    cover_open: bool,
}

impl<'a> Printer<'a> {
    fn read(block: &'a Value) -> Self {
        Self {
            configured: flag(block, "configured"),
            state: text(block, "state"),
            paper: text(block, "paper"),
            cover_open: flag(block, "cover_open"),
        }
    }

    /// The end sensor says there is no paper. Nothing can be printed.
    fn paper_is_out(&self) -> bool {
        self.configured && self.paper == "END"
    }

    /// The near-end sensor is covered. **Never blocking**: a sale may still be
    /// made on a roll that is nearly out (`PaperLevel::NearEnd`), so this
    /// tells whoever restocks to go now instead of refusing a customer.
    fn paper_is_low(&self) -> bool {
        self.configured && self.paper == "NEAR_END"
    }

    fn cover_is_open(&self) -> bool {
        self.configured && self.cover_open
    }

    /// It was asked, and it either said nothing or said it will not print.
    ///
    /// On `state` alone, never on `online: false`. A configured printer that
    /// has not been asked yet publishes `online: false` with
    /// `state: "UNKNOWN"`, and calling that disconnected would put a red card
    /// on every kiosk for the first seconds of its life — what raises an
    /// alert is an answer, not the absence of one, exactly as it is what
    /// stops a payment (`edge-hardware/src/printer.rs:322`).
    ///
    /// `paper` is not consulted here on purpose: an unplugged printer cannot
    /// report its roll, so `UNKNOWN` paper must never silence this.
    fn is_offline(&self) -> bool {
        self.configured && matches!(self.state, "NO_RESPONSE" | "OFFLINE")
    }
}

/// Somebody has to come to the lane and release a basket.
///
/// The pair, not the flag: `required` stays true once an approval is given —
/// `authorization::approve` sets `approved` beside it rather than clearing
/// the request, so that a floor app can see what it released. A lane already
/// released is not waiting for anyone.
fn waiting_for_a_person(authorization: &Value) -> bool {
    flag(authorization, "required") && !flag(authorization, "approved")
}

/// The note acceptor was asked for, and it is not answering.
///
/// `ACTION` rather than `BLOCKING` because card payment is untouched: the
/// lane still sells, it just cannot take a note. `enabled` gates it — a
/// kiosk that takes no cash has no acceptor to lose.
fn acceptor_is_silent(cashgenic: &Value) -> bool {
    flag(cashgenic, "enabled") && !flag(cashgenic, "online")
}

/// Absent, null, or the wrong type all read as "not reported", never as true.
fn flag(block: &Value, key: &str) -> bool {
    block[key].as_bool().unwrap_or(false)
}

/// Absent reads as empty, which matches no condition.
fn text<'a>(block: &'a Value, key: &str) -> &'a str {
    block[key].as_str().unwrap_or_default()
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    /// A printer that is set up and answering.
    fn printer(state: &str, paper: &str) -> Value {
        json!({
            "configured": true,
            "state": state,
            "paper": paper,
            "cover_open": false,
            "status_source": "DEVICE",
        })
    }

    fn healthy_printer() -> Value {
        printer("ONLINE", "OK")
    }

    fn healthy_acceptor() -> Value {
        json!({ "enabled": true, "online": true })
    }

    fn nothing_pending() -> Value {
        json!({ "required": false, "approved": false })
    }

    /// The codes raised, in the order they were raised.
    fn codes(raised: &[Value]) -> Vec<&str> {
        raised
            .iter()
            .map(|entry| entry["code"].as_str().unwrap_or_default())
            .collect()
    }

    /// The severity of one code, or `NOT_RAISED` — which fails the assertion
    /// it is compared against rather than passing quietly.
    fn severity_of(raised: &[Value], code: &str) -> String {
        raised
            .iter()
            .find(|entry| entry["code"] == code)
            .and_then(|entry| entry["severity"].as_str())
            .unwrap_or("NOT_RAISED")
            .to_owned()
    }

    #[test]
    fn a_kiosk_with_nothing_wrong_raises_nothing() {
        let raised = raise(&healthy_printer(), &healthy_acceptor(), &nothing_pending());
        assert!(
            raised.is_empty(),
            "a healthy kiosk put something on the card: {raised:?}"
        );
    }

    #[test]
    fn every_entry_carries_a_code_a_severity_and_a_message() {
        let raised = raise(
            &json!({ "configured": true, "state": "OFFLINE", "paper": "END", "cover_open": true }),
            &json!({ "enabled": true, "online": false }),
            &json!({ "required": true, "approved": false }),
        );
        // Five, not six: `END` and `NEAR_END` are the same sensor, so no
        // kiosk can ever raise both paper conditions at once.
        assert_eq!(raised.len(), 5, "{raised:?}");
        for entry in &raised {
            let object = entry.as_object().expect("an alert is an object");
            assert_eq!(object.len(), 3, "{entry:?} carries more than the contract");
            for key in ["code", "severity", "message"] {
                let value = object.get(key).expect(key);
                assert!(value.is_string(), "{key} is not a string in {entry:?}");
                assert!(
                    !value.as_str().unwrap_or_default().is_empty(),
                    "{key} is empty in {entry:?}"
                );
            }
        }
    }

    #[test]
    fn no_paper_blocks_and_low_paper_does_not() {
        let out = raise(
            &printer("ONLINE", "END"),
            &healthy_acceptor(),
            &nothing_pending(),
        );
        assert_eq!(codes(&out), vec!["PRINTER_PAPER_END"]);
        assert_eq!(severity_of(&out, "PRINTER_PAPER_END"), "BLOCKING");

        let low = raise(
            &printer("ONLINE", "NEAR_END"),
            &healthy_acceptor(),
            &nothing_pending(),
        );
        assert_eq!(codes(&low), vec!["PRINTER_PAPER_LOW"]);
        // A roll that is nearly out still sells. If this ever says BLOCKING,
        // a near-end sensor closes a shop.
        assert_eq!(severity_of(&low, "PRINTER_PAPER_LOW"), "WARN");
    }

    #[test]
    fn an_open_cover_blocks() {
        let mut block = healthy_printer();
        block["cover_open"] = json!(true);
        let raised = raise(&block, &healthy_acceptor(), &nothing_pending());
        assert_eq!(codes(&raised), vec!["PRINTER_COVER_OPEN"]);
        assert_eq!(severity_of(&raised, "PRINTER_COVER_OPEN"), "BLOCKING");
    }

    #[test]
    fn a_printer_that_said_nothing_and_one_that_refused_both_block() {
        for state in ["NO_RESPONSE", "OFFLINE"] {
            let raised = raise(
                &printer(state, "OK"),
                &healthy_acceptor(),
                &nothing_pending(),
            );
            assert_eq!(codes(&raised), vec!["PRINTER_OFFLINE"], "state {state}");
            assert_eq!(severity_of(&raised, "PRINTER_OFFLINE"), "BLOCKING");
        }
    }

    /// An unplugged printer cannot report its roll. The one condition that
    /// survives `paper: "UNKNOWN"`.
    #[test]
    fn an_unplugged_printer_blocks_even_though_its_paper_is_unknown() {
        for state in ["NO_RESPONSE", "OFFLINE"] {
            let raised = raise(
                &printer(state, "UNKNOWN"),
                &healthy_acceptor(),
                &nothing_pending(),
            );
            assert_eq!(codes(&raised), vec!["PRINTER_OFFLINE"], "state {state}");
        }
    }

    #[test]
    fn a_silent_acceptor_asks_for_a_person_without_stopping_the_lane() {
        let raised = raise(
            &healthy_printer(),
            &json!({ "enabled": true, "online": false }),
            &nothing_pending(),
        );
        assert_eq!(codes(&raised), vec!["CASHGENIC_OFFLINE"]);
        // Card still works, so this is not BLOCKING.
        assert_eq!(severity_of(&raised, "CASHGENIC_OFFLINE"), "ACTION");
    }

    #[test]
    fn a_waiting_basket_asks_for_a_person_and_a_released_one_does_not() {
        let waiting = raise(
            &healthy_printer(),
            &healthy_acceptor(),
            &json!({ "required": true, "approved": false }),
        );
        assert_eq!(codes(&waiting), vec!["AUTHORIZATION_REQUIRED"]);
        assert_eq!(severity_of(&waiting, "AUTHORIZATION_REQUIRED"), "ACTION");

        // `required` stays true after an approval. Nobody has to come over.
        let released = raise(
            &healthy_printer(),
            &healthy_acceptor(),
            &json!({ "required": true, "approved": true }),
        );
        assert!(released.is_empty(), "{released:?}");
    }

    // --- The silences. What must never reach a card. ---

    #[test]
    fn an_unconfigured_printer_is_never_a_printer_problem() {
        // Every printer condition true at once, and no printer set up.
        let raised = raise(
            &json!({
                "configured": false,
                "state": "NO_RESPONSE",
                "paper": "END",
                "cover_open": true,
            }),
            &healthy_acceptor(),
            &nothing_pending(),
        );
        assert!(
            raised.is_empty(),
            "a kiosk with no printer was told to fix one: {raised:?}"
        );
    }

    #[test]
    fn unknown_paper_on_its_own_says_nothing() {
        let raised = raise(
            &printer("ONLINE", "UNKNOWN"),
            &healthy_acceptor(),
            &nothing_pending(),
        );
        assert!(raised.is_empty(), "{raised:?}");
    }

    /// A printer whose model cannot answer `DLE EOT` reports `UNKNOWN`
    /// forever. That is a printer nobody can ask, not a printer with a fault.
    #[test]
    fn a_printer_with_no_status_channel_says_nothing_by_itself() {
        let raised = raise(
            &json!({
                "configured": true,
                "state": "UNKNOWN",
                "paper": "UNKNOWN",
                "cover_open": false,
                "status_source": "NONE",
                "online": false,
                "status_checked_at": "",
            }),
            &healthy_acceptor(),
            &nothing_pending(),
        );
        assert!(raised.is_empty(), "{raised:?}");
    }

    #[test]
    fn a_disabled_acceptor_is_never_an_alert() {
        let raised = raise(
            &healthy_printer(),
            &json!({ "enabled": false, "online": false, "cash_total": Value::Null }),
            &nothing_pending(),
        );
        assert!(raised.is_empty(), "{raised:?}");
    }

    /// Missing keys are how a block from an older kiosk arrives. Absence is
    /// never a fault.
    #[test]
    fn empty_blocks_raise_nothing() {
        assert!(raise(&json!({}), &json!({}), &json!({})).is_empty());
        assert!(raise(&Value::Null, &Value::Null, &Value::Null).is_empty());
    }

    #[test]
    fn the_order_is_most_urgent_first_and_does_not_move() {
        let raised = raise(
            &json!({
                "configured": true,
                "state": "NO_RESPONSE",
                "paper": "END",
                "cover_open": true,
            }),
            &json!({ "enabled": true, "online": false }),
            &json!({ "required": true, "approved": false }),
        );
        assert_eq!(
            codes(&raised),
            vec![
                "PRINTER_PAPER_END",
                "PRINTER_COVER_OPEN",
                "PRINTER_OFFLINE",
                "AUTHORIZATION_REQUIRED",
                "CASHGENIC_OFFLINE",
            ],
            "a console renders this list verbatim; the order is the contract"
        );
    }

    #[test]
    fn severities_are_grouped_and_never_interleave() {
        let raised = raise(
            &printer("ONLINE", "NEAR_END"),
            &json!({ "enabled": true, "online": false }),
            &json!({ "required": true, "approved": false }),
        );
        let order: Vec<&str> = raised
            .iter()
            .map(|entry| entry["severity"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(order, vec!["ACTION", "ACTION", "WARN"]);
        // And the WARN is the paper, last, behind both people-needed entries.
        assert_eq!(
            codes(&raised),
            vec![
                "AUTHORIZATION_REQUIRED",
                "CASHGENIC_OFFLINE",
                "PRINTER_PAPER_LOW",
            ]
        );
    }
}
