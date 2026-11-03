//! "Somebody has to come over before this is paid for."
//!
//! A retail lane sells things a shop will not let a customer take
//! unsupervised. The rule a showroom wants to demonstrate is narrow and
//! specific, and every part of it matters:
//!
//! * **Scanning is never blocked.** A customer fills their basket normally;
//!   finding out at the till is how a real self-checkout behaves, and stopping
//!   the scan would hide the behaviour being demonstrated.
//! * **Paying is blocked**, and only paying.
//! * An approval belongs to **one sale and one set of products**. It is keyed
//!   on the sale's own id and the refs it covered, so it cannot be carried
//!   into the next customer's basket, and adding another restricted product
//!   to an approved basket asks again.
//!
//! One pending authorization at a time, in memory for the life of the
//! process. Not persisted on purpose: an approval that outlived a restart
//! would be an approval nobody remembers giving, and the sale it belonged to
//! is the daemon's — this only remembers that somebody said yes to it.
//!
//! This is **not** the core's authority model. Nothing here authorises a
//! refund, a discount, a safe drop or a session close; those are the core's
//! and are decided online (ADR 030, ADR 042). This releases a screen.

use std::collections::BTreeSet;
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One product the sale is waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationProductV1 {
    pub product_ref: String,
    pub name: String,
}

/// What the screen and the floor app are told about a pending authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationV1 {
    /// Whether anything is waiting at all.
    pub required: bool,
    /// The id a remote approval names. Empty when nothing is pending.
    pub request_id: String,
    /// The sale it belongs to. Empty when nothing is pending.
    pub sale_id: String,
    /// Which products asked for it. Empty when nothing is pending.
    pub products: Vec<AuthorizationProductV1>,
    pub approved: bool,
    /// RFC 3339. Empty when nothing is pending.
    pub requested_at: String,
}

impl AuthorizationV1 {
    /// Nothing is waiting. Every field a value, never a null: the floor app
    /// polls this and branches without checking for absence.
    pub fn none() -> Self {
        Self {
            required: false,
            request_id: String::new(),
            sale_id: String::new(),
            products: Vec::new(),
            approved: false,
            requested_at: String::new(),
        }
    }
}

/// What is actually held: the published shape plus the set an approval covers.
#[derive(Debug, Clone)]
struct Pending {
    published: AuthorizationV1,
    /// The refs this request was raised for — and, once approved, the exact
    /// set the approval is good for. A basket that grows past it asks again.
    covered: BTreeSet<String>,
}

static PENDING: OnceLock<RwLock<Option<Pending>>> = OnceLock::new();

fn held() -> &'static RwLock<Option<Pending>> {
    PENDING.get_or_init(|| RwLock::new(None))
}

/// What a poller is told. Never blocks, never waits on anything.
pub fn published() -> AuthorizationV1 {
    held()
        .read()
        .ok()
        .and_then(|slot| slot.as_ref().map(|pending| pending.published.clone()))
        .unwrap_or_else(AuthorizationV1::none)
}

/// Whether this sale may be paid for, given the restricted products in it.
///
/// True when nothing is restricted, or when an approval already in hand
/// belongs to *this* sale and covers *every* restricted ref in it. The second
/// half is the rule that makes an approved basket ask again when another
/// restricted product goes in.
pub fn allows(sale_id: &str, restricted: &BTreeSet<String>) -> bool {
    if restricted.is_empty() {
        return true;
    }
    held()
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .is_some_and(|pending| {
            pending.published.approved
                && pending.published.sale_id == sale_id
                && restricted.is_subset(&pending.covered)
        })
}

/// Raise, or keep, the authorization this sale is waiting on.
///
/// Idempotent for an unchanged basket: pressing pay twice does not mint a
/// second request id, because a floor app watching the first one would lose
/// it. A basket whose restricted set has *changed* gets a new request —
/// including one that was already approved, which is the point.
pub fn require(sale_id: &str, products: Vec<AuthorizationProductV1>) -> AuthorizationV1 {
    let covered: BTreeSet<String> = products
        .iter()
        .map(|product| product.product_ref.clone())
        .collect();
    let Ok(mut slot) = held().write() else {
        // A poisoned lock must not become "authorized". Report the request
        // without storing it: nothing will approve it, and the sale stays
        // blocked, which is the safe direction.
        return AuthorizationV1 {
            required: true,
            request_id: uuid::Uuid::new_v4().to_string(),
            sale_id: sale_id.to_owned(),
            products,
            approved: false,
            requested_at: now(),
        };
    };

    if let Some(existing) = slot.as_ref() {
        if existing.published.sale_id == sale_id && existing.covered == covered {
            return existing.published.clone();
        }
    }
    let fresh = Pending {
        published: AuthorizationV1 {
            required: true,
            request_id: uuid::Uuid::new_v4().to_string(),
            sale_id: sale_id.to_owned(),
            products,
            approved: false,
            requested_at: now(),
        },
        covered,
    };
    let published = fresh.published.clone();
    *slot = Some(fresh);
    published
}

/// Why an approval was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedBecause {
    /// Nothing is waiting to be approved.
    NothingPending,
    /// A request id that is not the one waiting — a stale screen, or a
    /// floor app approving a request the basket has already replaced.
    WrongRequest,
    /// The PIN did not match, or this kiosk has none set.
    WrongPin,
}

impl RefusedBecause {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NothingPending => "AUTHORIZATION_NOT_PENDING",
            Self::WrongRequest => "AUTHORIZATION_REQUEST_UNKNOWN",
            Self::WrongPin => "AUTHORIZATION_PIN_REFUSED",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::NothingPending => "nothing is waiting for an authorization",
            Self::WrongRequest => {
                "that authorization is no longer the one waiting; the basket has changed"
            }
            Self::WrongPin => "that PIN is not this kiosk's, or no PIN has been set on it",
        }
    }
}

/// Approve the pending authorization, if `request_id` is the one waiting.
///
/// The PIN is checked by the caller — the preferences are the host's, not this
/// module's — and `pin_ok` is its answer. Separated so there is exactly one
/// place that compares a PIN and one place that decides what an approval
/// means.
pub fn approve(request_id: &str, pin_ok: bool) -> Result<AuthorizationV1, RefusedBecause> {
    let mut slot = held().write().map_err(|_| RefusedBecause::NothingPending)?;
    let Some(pending) = slot.as_mut() else {
        return Err(RefusedBecause::NothingPending);
    };
    // The request is checked before the PIN, so a stale screen is told its
    // basket changed rather than that its PIN is wrong.
    if pending.published.request_id != request_id.trim() {
        return Err(RefusedBecause::WrongRequest);
    }
    if !pin_ok {
        return Err(RefusedBecause::WrongPin);
    }
    pending.published.approved = true;
    Ok(pending.published.clone())
}

/// Forget whatever was pending.
///
/// Called when a sale ends, however it ends, and when a basket no longer
/// holds anything restricted. An approval left behind for a sale nobody is
/// paying is an approval the next customer could inherit.
pub fn clear() {
    if let Ok(mut slot) = held().write() {
        *slot = None;
    }
}

/// Forget it unless it belongs to this sale.
///
/// What the gate calls on a basket with nothing restricted in it: the
/// customer removed the item, so the request goes — but a *different* sale's
/// pending request is not this sale's business to discard.
pub fn clear_unless(sale_id: &str) {
    if let Ok(mut slot) = held().write() {
        if slot
            .as_ref()
            .is_some_and(|pending| pending.published.sale_id == sale_id)
        {
            *slot = None;
        }
    }
}

impl AuthorizationV1 {
    /// The JSON a floor app reads. Every field a value, never a null.
    pub fn into_wire(self) -> Value {
        json!({
            "required": self.required,
            "request_id": self.request_id,
            "sale_id": self.sale_id,
            "approved": self.approved,
            "requested_at": self.requested_at,
            "products": self
                .products
                .iter()
                .map(|product| json!({
                    "product_ref": product.product_ref,
                    "name": product.name,
                }))
                .collect::<Vec<Value>>(),
        })
    }
}

/// What a floor app reads, as JSON.
pub fn status() -> Value {
    published().into_wire()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Take the turnstile every test that touches the pending slot shares.
///
/// There is one pending authorization per process — a kiosk has one lane and
/// one basket — so tests across **both** this module and `kiosk` have to take
/// turns rather than race each other. It is taken even when a previous test
/// panicked while holding it: a poisoned lock here would turn one failure
/// into every failure.
#[cfg(test)]
pub(crate) fn turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the customer is told while the lane waits.
pub const WAITING_MESSAGE: &str =
    "Este producto requiere autorización.\nEn un momento alguien se acercará a ayudarte.";

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn product(reference: &str) -> AuthorizationProductV1 {
        AuthorizationProductV1 {
            product_ref: reference.to_owned(),
            name: format!("the {reference}"),
        }
    }

    fn refs(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let guard = turn();
        clear();
        guard
    }

    #[test]
    fn nothing_pending_publishes_a_complete_answer_with_no_nulls() {
        let _turn = fresh();
        let published = published();
        assert!(!published.required);
        assert!(!published.approved);
        assert_eq!(published.request_id, "");
        assert_eq!(published.sale_id, "");
        assert!(published.products.is_empty());
        assert_eq!(published.requested_at, "");

        let wire = status();
        assert_eq!(wire["required"], false);
        assert_eq!(wire["approved"], false);
        assert_eq!(wire["products"], json!([]));
        for field in [
            "required",
            "request_id",
            "sale_id",
            "approved",
            "requested_at",
            "products",
        ] {
            assert!(!wire[field].is_null(), "`{field}` is null");
        }
    }

    /// A basket with nothing restricted in it is always payable.
    #[test]
    fn a_basket_with_nothing_restricted_needs_no_authorization() {
        let _turn = fresh();
        assert!(allows("sale-1", &refs(&[])));
    }

    /// The journey: blocked, approved, payable.
    #[test]
    fn a_restricted_basket_is_blocked_until_the_pin_releases_it() {
        let _turn = fresh();
        let pending = require("sale-1", vec![product("R-ARRACHERA")]);
        assert!(pending.required);
        assert!(!pending.approved);
        assert_eq!(pending.sale_id, "sale-1");
        assert!(!pending.request_id.is_empty());
        assert_eq!(pending.products.len(), 1);
        assert!(!allows("sale-1", &refs(&["R-ARRACHERA"])));

        let approved = approve(&pending.request_id, true).unwrap();
        assert!(approved.approved);
        assert_eq!(approved.request_id, pending.request_id);
        assert!(allows("sale-1", &refs(&["R-ARRACHERA"])));
        assert!(published().approved);
    }

    /// Pressing pay twice does not mint a second request: a floor app
    /// watching the first would lose it.
    #[test]
    fn asking_again_for_the_same_basket_keeps_the_same_request() {
        let _turn = fresh();
        let first = require("sale-1", vec![product("R-ARRACHERA")]);
        let again = require("sale-1", vec![product("R-ARRACHERA")]);
        assert_eq!(first.request_id, again.request_id);
        assert_eq!(first.requested_at, again.requested_at);
    }

    /// **The rule that is easy to get wrong.** An approval covers the set it
    /// was given, so another restricted product asks again.
    #[test]
    fn adding_another_restricted_product_asks_again() {
        let _turn = fresh();
        let first = require("sale-1", vec![product("R-ARRACHERA")]);
        approve(&first.request_id, true).unwrap();
        assert!(allows("sale-1", &refs(&["R-ARRACHERA"])));

        // The customer scans a second restricted item.
        assert!(
            !allows("sale-1", &refs(&["R-ARRACHERA", "R-CLOROX"])),
            "an approval for one product released a basket with two"
        );
        let second = require("sale-1", vec![product("R-ARRACHERA"), product("R-CLOROX")]);
        assert_ne!(second.request_id, first.request_id, "the same request id");
        assert!(!second.approved, "the new request arrived pre-approved");
        assert!(!allows("sale-1", &refs(&["R-ARRACHERA", "R-CLOROX"])));

        approve(&second.request_id, true).unwrap();
        assert!(allows("sale-1", &refs(&["R-ARRACHERA", "R-CLOROX"])));
    }

    /// Taking a restricted product *out* leaves the rest payable on the
    /// approval already given.
    #[test]
    fn removing_one_of_two_restricted_products_stays_approved() {
        let _turn = fresh();
        let request = require("sale-1", vec![product("R-ARRACHERA"), product("R-CLOROX")]);
        approve(&request.request_id, true).unwrap();
        assert!(allows("sale-1", &refs(&["R-ARRACHERA"])));
    }

    /// **Never reused between sales.** The next customer starts blocked.
    #[test]
    fn an_approval_never_carries_into_another_sale() {
        let _turn = fresh();
        let request = require("sale-1", vec![product("R-ARRACHERA")]);
        approve(&request.request_id, true).unwrap();
        assert!(allows("sale-1", &refs(&["R-ARRACHERA"])));
        assert!(
            !allows("sale-2", &refs(&["R-ARRACHERA"])),
            "another sale inherited an approval"
        );
    }

    #[test]
    fn a_wrong_pin_does_not_approve_and_says_so() {
        let _turn = fresh();
        let request = require("sale-1", vec![product("R-ARRACHERA")]);
        assert_eq!(
            approve(&request.request_id, false),
            Err(RefusedBecause::WrongPin)
        );
        assert!(!allows("sale-1", &refs(&["R-ARRACHERA"])));
        assert!(!published().approved);
        // And the request is still there to try again.
        assert_eq!(published().request_id, request.request_id);
    }

    /// A stale screen is told its basket changed, not that its PIN is wrong.
    #[test]
    fn approving_a_request_that_is_no_longer_waiting_is_refused_as_such() {
        let _turn = fresh();
        let first = require("sale-1", vec![product("R-ARRACHERA")]);
        let _second = require("sale-1", vec![product("R-ARRACHERA"), product("R-CLOROX")]);
        assert_eq!(
            approve(&first.request_id, true),
            Err(RefusedBecause::WrongRequest)
        );
    }

    #[test]
    fn approving_with_nothing_pending_is_refused() {
        let _turn = fresh();
        assert_eq!(
            approve("whatever", true),
            Err(RefusedBecause::NothingPending)
        );
    }

    /// A sale that ends takes its pending request with it.
    #[test]
    fn clearing_forgets_only_this_sales_request() {
        let _turn = fresh();
        let request = require("sale-1", vec![product("R-ARRACHERA")]);
        clear_unless("sale-2");
        assert_eq!(
            published().request_id,
            request.request_id,
            "another sale discarded this one's request"
        );
        clear_unless("sale-1");
        assert!(!published().required);
    }

    /// Every refusal has a stable code a client can branch on.
    #[test]
    fn each_refusal_has_its_own_code() {
        let codes: BTreeSet<&str> = [
            RefusedBecause::NothingPending,
            RefusedBecause::WrongRequest,
            RefusedBecause::WrongPin,
        ]
        .iter()
        .map(|refusal| refusal.code())
        .collect();
        assert_eq!(codes.len(), 3, "two refusals share a code");
        for refusal in [
            RefusedBecause::NothingPending,
            RefusedBecause::WrongRequest,
            RefusedBecause::WrongPin,
        ] {
            assert!(!refusal.detail().is_empty());
        }
    }

    /// The words the customer reads, as the brief gave them.
    #[test]
    fn the_customer_is_told_what_the_brief_says() {
        assert!(WAITING_MESSAGE.starts_with("Este producto requiere autorización."));
        assert!(WAITING_MESSAGE.contains("En un momento alguien se acercará a ayudarte."));
    }
}
