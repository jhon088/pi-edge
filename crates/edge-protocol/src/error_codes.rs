//! Stable error codes.
//!
//! Clients branch on these strings; they are part of the contract and must not
//! change meaning. Human-readable messages may change at any time, so no
//! client is allowed to parse them.
//!
//! Domain and application codes come from `AppError::kind()` in
//! `edge-application` and are listed here for reference:
//!
//! | Code | Meaning |
//! | --- | --- |
//! | `DEVICE_NOT_INITIALIZED` | the edge device has no tenant/store provisioned |
//! | `BUSINESS_DAY_ALREADY_OPEN` | a non-closed business day exists for the store |
//! | `NO_OPEN_BUSINESS_DAY` | the operation needs an open business day |
//! | `BUSINESS_DAY_NOT_OPEN` | the business day exists but is CLOSING/CLOSED |
//! | `BUSINESS_DAY_HAS_OPEN_SESSIONS` | cannot close the day with live sessions |
//! | `REGISTER_SESSION_ALREADY_OPEN` | the register already has a live session |
//! | `NO_OPEN_REGISTER_SESSION` | the operation needs an open session |
//! | `AMBIGUOUS_OPEN_SESSION` | more than one open session; the active one is ambiguous |
//! | `REGISTER_SESSION_NOT_OPEN` | the session is CLOSE_REQUESTED/CLOSED |
//! | `SESSION_HAS_ACTIVE_TRANSACTION` | cannot close the session mid-sale |
//! | `TRANSACTION_ALREADY_ACTIVE` | the session already has a live transaction |
//! | `NO_ACTIVE_TRANSACTION` | the operation needs an active transaction |
//! | `INVALID_TRANSACTION_STATE` | the aggregate refused the state transition |
//! | `LINE_NOT_FOUND` | no line of the sale carries that line number |
//! | `BASKET_LOCKED_BY_PAYMENT` | a payment is active; pay or reverse before editing |
//! | `DOMAIN_RULE_VIOLATION` | another domain invariant was violated |
//! | `CONCURRENT_MODIFICATION` | stale write detected |
//! | `STORAGE_FAILURE` | the local database failed |
//! | `SERIALIZATION_FAILURE` | a payload could not be (de)serialized |

// Codes produced by the core. They must stay identical to
// `AppError::kind()` in `edge-application`; the daemon has a test that pins
// the ones referenced here.

/// A state machine refused the transition (the most common business refusal).
pub const INVALID_TRANSACTION_STATE: &str = "INVALID_TRANSACTION_STATE";
/// Another domain invariant was violated.
pub const DOMAIN_RULE_VIOLATION: &str = "DOMAIN_RULE_VIOLATION";
/// No line of the sale carries the given line number.
pub const LINE_NOT_FOUND: &str = "LINE_NOT_FOUND";
/// The local database failed. The runtime reports itself as `DEGRADED`.
pub const STORAGE_FAILURE: &str = "STORAGE_FAILURE";

// Catalog codes, produced by the catalog context.

/// No product matches the selector in the snapshot being used.
pub const PRODUCT_NOT_FOUND: &str = "PRODUCT_NOT_FOUND";
/// The product exists but is not active in this snapshot.
pub const PRODUCT_INACTIVE: &str = "PRODUCT_INACTIVE";
/// The product exists and is active but may not be sold.
pub const PRODUCT_NOT_SELLABLE: &str = "PRODUCT_NOT_SELLABLE";
/// The register has no active catalog, so it cannot start a sale.
pub const NO_ACTIVE_CATALOG: &str = "NO_ACTIVE_CATALOG";
/// No price book gives this product a price at the resolution instant.
pub const PRICE_NOT_FOUND: &str = "PRICE_NOT_FOUND";
/// Two or more prices apply with equal specificity: the core refuses to guess.
pub const AMBIGUOUS_PRICE: &str = "AMBIGUOUS_PRICE";
/// Two or more exclusive promotions claim the same target at the same
/// priority: the core refuses to pick one.
pub const AMBIGUOUS_PROMOTION: &str = "AMBIGUOUS_PROMOTION";
/// A record names a promotion the snapshot does not declare.
pub const CATALOG_UNKNOWN_PROMOTION: &str = "CATALOG_UNKNOWN_PROMOTION";
/// A price entry names a book the snapshot does not declare.
pub const CATALOG_UNKNOWN_PRICE_BOOK: &str = "CATALOG_UNKNOWN_PRICE_BOOK";
/// Two prices in one book start at the same instant for one product.
pub const CATALOG_DUPLICATE_PRICE_ENTRY: &str = "CATALOG_DUPLICATE_PRICE_ENTRY";
/// `valid_until` is not after `valid_from`.
pub const CATALOG_INVALID_VALIDITY: &str = "CATALOG_INVALID_VALIDITY";
/// A chunk index was resent with different content.
pub const CATALOG_CHUNK_CONFLICT: &str = "CATALOG_CHUNK_CONFLICT";
/// A chunk is missing, so the import cannot be committed.
pub const CATALOG_IMPORT_INCOMPLETE: &str = "CATALOG_IMPORT_INCOMPLETE";
/// Declared counts do not match what was stored.
pub const CATALOG_COUNT_MISMATCH: &str = "CATALOG_COUNT_MISMATCH";
/// The hash computed from the stored rows differs from the declared one.
pub const CATALOG_HASH_MISMATCH: &str = "CATALOG_HASH_MISMATCH";
/// Two records claim the same barcode within one snapshot.
pub const CATALOG_DUPLICATE_BARCODE: &str = "CATALOG_DUPLICATE_BARCODE";
/// Two records claim the same product within one snapshot.
pub const CATALOG_DUPLICATE_PRODUCT: &str = "CATALOG_DUPLICATE_PRODUCT";
/// A record failed validation at the boundary.
pub const CATALOG_INVALID_RECORD: &str = "CATALOG_INVALID_RECORD";
/// A product references a tax profile the snapshot does not declare.
pub const CATALOG_UNKNOWN_TAX_PROFILE: &str = "CATALOG_UNKNOWN_TAX_PROFILE";
/// The snapshot id is unknown to this installation.
pub const CATALOG_SNAPSHOT_NOT_FOUND: &str = "CATALOG_SNAPSHOT_NOT_FOUND";
/// The import is no longer open for content.
pub const CATALOG_IMPORT_NOT_OPEN: &str = "CATALOG_IMPORT_NOT_OPEN";
/// Activation needs a committed (PENDING) snapshot.
pub const CATALOG_SNAPSHOT_NOT_PENDING: &str = "CATALOG_SNAPSHOT_NOT_PENDING";
/// That version already exists locally with different content.
pub const CATALOG_VERSION_CONFLICT: &str = "CATALOG_VERSION_CONFLICT";
/// The snapshot lifecycle refused the transition.
pub const CATALOG_INVALID_TRANSITION: &str = "CATALOG_INVALID_TRANSITION";

// Payment codes, produced by the payment context.

/// The payment or attempt id is unknown to this installation.
pub const PAYMENT_NOT_FOUND: &str = "PAYMENT_NOT_FOUND";
/// The payment state machine refused the transition.
pub const PAYMENT_INVALID_STATE: &str = "PAYMENT_INVALID_STATE";
/// The payment has already reached a resolved state.
pub const PAYMENT_ALREADY_RESOLVED: &str = "PAYMENT_ALREADY_RESOLVED";
/// A payment reached a provider and the result is unknown. The only way
/// forward is an explicit reconciliation; nothing retries a charge (ADR 018).
pub const PAYMENT_REQUIRES_RECONCILIATION: &str = "PAYMENT_REQUIRES_RECONCILIATION";
/// Reversing this payment is not allowed from its current state.
pub const PAYMENT_REVERSAL_NOT_ALLOWED: &str = "PAYMENT_REVERSAL_NOT_ALLOWED";
/// A tender that may not overpay was asked for more than the remaining
/// balance. Cash may overpay; a card may not.
pub const PAYMENT_EXCEEDS_BALANCE: &str = "PAYMENT_EXCEEDS_BALANCE";
/// A payment amount is not positive, or the balance is already covered.
pub const PAYMENT_INVALID_AMOUNT: &str = "PAYMENT_INVALID_AMOUNT";
/// The provider refused the payment.
pub const PAYMENT_PROVIDER_DECLINED: &str = "PAYMENT_PROVIDER_DECLINED";
/// The provider could not be reached. Distinct from an unknown *outcome*:
/// this is produced only when nothing was sent.
pub const PAYMENT_PROVIDER_UNAVAILABLE: &str = "PAYMENT_PROVIDER_UNAVAILABLE";
/// A payment record failed validation at the boundary.
pub const PAYMENT_INVALID_RECORD: &str = "PAYMENT_INVALID_RECORD";
/// The basket cannot change because a payment is in a financially active
/// state (ADR 019).
pub const BASKET_LOCKED_BY_PAYMENT: &str = "BASKET_LOCKED_BY_PAYMENT";

// Cash management codes, produced by the cash context.

/// A cash amount is not positive, or a declaration is negative.
pub const CASH_INVALID_AMOUNT: &str = "CASH_INVALID_AMOUNT";
/// A movement contradicts its own type: the wrong direction, or a cash sale
/// whose tender audit does not balance against what stayed in the drawer.
pub const CASH_INVALID_MOVEMENT: &str = "CASH_INVALID_MOVEMENT";
/// An operator-initiated movement arrived with no reason. An unexplained
/// drawer movement is not an audit trail.
pub const CASH_REASON_REQUIRED: &str = "CASH_REASON_REQUIRED";
/// A cash record failed validation at the boundary.
pub const CASH_INVALID_RECORD: &str = "CASH_INVALID_RECORD";
/// The safe drop is not in a state that allows this decision.
pub const SAFE_DROP_INVALID_STATE: &str = "SAFE_DROP_INVALID_STATE";
/// Nobody approves their own safe drop.
pub const SAFE_DROP_SELF_APPROVAL: &str = "SAFE_DROP_SELF_APPROVAL";
pub const SAFE_DROP_NOT_FOUND: &str = "SAFE_DROP_NOT_FOUND";
/// A declaration arrived before a close was requested. The ordering is what
/// makes the close blind.
pub const SESSION_DECLARATION_REQUIRED: &str = "SESSION_DECLARATION_REQUIRED";
/// This close attempt already has a declaration. Declarations are evidence,
/// not working state.
pub const SESSION_ALREADY_DECLARED: &str = "SESSION_ALREADY_DECLARED";
/// The session cannot close yet; the message names every blocker.
pub const SESSION_CLOSE_BLOCKED: &str = "SESSION_CLOSE_BLOCKED";
/// The business day cannot close yet; the message names every blocker.
pub const BUSINESS_DAY_CLOSE_BLOCKED: &str = "BUSINESS_DAY_CLOSE_BLOCKED";

// Inventory codes, produced by the inventory context.

/// A movement contradicts its own type: `SALE` without a transaction, a type
/// recorded against the wrong direction, a sale line with no source.
pub const INVENTORY_INVALID_MOVEMENT: &str = "INVENTORY_INVALID_MOVEMENT";
/// A manual movement arrived with no reason. Stock that moves for no stated
/// reason is not a ledger.
pub const INVENTORY_REASON_REQUIRED: &str = "INVENTORY_REASON_REQUIRED";
/// The active catalog snapshot does not know that product. PI.Edge never
/// invents product master data (ADR 011).
pub const INVENTORY_UNKNOWN_PRODUCT: &str = "INVENTORY_UNKNOWN_PRODUCT";
/// No active catalog snapshot, so no product can be validated.
pub const INVENTORY_CATALOG_REQUIRED: &str = "INVENTORY_CATALOG_REQUIRED";
/// The store already has an opening position from a *different* upstream
/// snapshot. Replaying the same one is a no-op; a different one is refused
/// rather than guessed at (ADR 028).
pub const INVENTORY_ALREADY_BOOTSTRAPPED: &str = "INVENTORY_ALREADY_BOOTSTRAPPED";
/// The opening position is empty, or names the same product twice.
pub const INVENTORY_INVALID_BOOTSTRAP: &str = "INVENTORY_INVALID_BOOTSTRAP";
/// The sale needs more than the ledger says is here, and this installation
/// does not allow selling into negative stock (ADR 029).
pub const INVENTORY_INSUFFICIENT_STOCK: &str = "INVENTORY_INSUFFICIENT_STOCK";
/// An inventory record failed validation at the boundary.
pub const INVENTORY_INVALID_RECORD: &str = "INVENTORY_INVALID_RECORD";

/// The `Hello` frame was rejected. Never discloses *why*.
pub const AUTHENTICATION_FAILED: &str = "AUTHENTICATION_FAILED";
/// A request arrived before a successful handshake.
pub const NOT_AUTHENTICATED: &str = "NOT_AUTHENTICATED";
/// The client speaks a protocol version this daemon does not support.
pub const PROTOCOL_VERSION_MISMATCH: &str = "PROTOCOL_VERSION_MISMATCH";
/// The frame was not valid JSON Lines, or not a known frame shape.
pub const MALFORMED_FRAME: &str = "MALFORMED_FRAME";
/// The operation payload failed validation at the boundary.
pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
/// The operation is not available on this connection (e.g. after SUBSCRIBE).
pub const UNSUPPORTED_OPERATION: &str = "UNSUPPORTED_OPERATION";
/// The daemon could not be reached, or the connection dropped. Produced by the
/// client, never received from the daemon.
pub const CORE_UNAVAILABLE: &str = "CORE_UNAVAILABLE";
/// The daemon answered with a result shape the client did not expect.
pub const UNEXPECTED_RESULT: &str = "UNEXPECTED_RESULT";
/// Another runtime already owns this installation's database.
pub const SINGLE_WRITER_VIOLATION: &str = "SINGLE_WRITER_VIOLATION";
