//! Hardware.
//!
//! Printers jam, drawers stay shut, scanners get unplugged. The rule this crate
//! exists to hold is the one from ADR 005: **hardware failure is never a domain
//! failure**. A sale is finalized on the strength of its payments and its
//! committed database transaction; paper and solenoids come afterwards and can
//! fail, be reported and be retried without touching a cent (ADR 037).
//!
//! Every port here arrives *with* an adapter. `edge-hardware-api` once shipped
//! as a crate of traits nobody implemented and was deleted for it in milestone
//! 7; the amendment to ADR 005 said the port would arrive with the first device
//! we actually talk to, and this is that milestone.
//!
//! What this crate does **not** know: the POS domain, money, the catalog,
//! SQLite, IPC, Tauri, Android, the application layer. It knows devices, and
//! the only I/O in it is one std TCP socket in the ESC/POS adapter.
//!
//! ```text
//! application ─> PrinterPort ────┬─> EscPosTcpPrinter ─> TCP ─> 80mm printer
//!                                ├─> SimulatorPrinter      (opt-in only)
//!                                └─> UnconfiguredPrinter   (nothing configured)
//!             ─> CashDrawerPort ─┬─> EscPosKickDrawer ─> DK pulse ─> drawer
//!                                ├─> IminDrawer ─> Android host ─> Kotlin ─> SDK
//!                                ├─> SimulatorDrawer       (opt-in only)
//!                                └─> UnconfiguredDrawer    (nothing configured)
//!
//! host keystrokes ─> ScanBuffer ─> ScanEvent ─> application
//! ```
//!
//! **Simulators are explicit development and test adapters.** Missing
//! configuration never selects one: it selects `Unconfigured*`, which answers
//! `HARDWARE_NOT_CONFIGURED` to everything. A misconfigured till reports that
//! it is misconfigured rather than reporting healthy fake hardware (ADR 038).
//!
//! The scanner has no port, on purpose: it is **input**, not a capability the
//! core invokes. The host that owns the window captures the keystrokes, this
//! crate's parser decides what a scan is, and the result travels as an event
//! (ADR 036).

pub mod device;
pub mod drawer;
pub mod error;
pub mod escpos;
pub mod printer;
pub mod scanner;
pub mod simulator;
pub mod spooler;
pub mod swappable;
pub mod unconfigured;

pub use device::{HardwareDeviceKind, HardwareDeviceState, HardwareDeviceStatus};
pub use drawer::{CashDrawerPort, DrawerOpenReason};
pub use error::{DeliveryCertainty, HardwareError};
pub use escpos::{EscPosConfig, EscPosKickDrawer, EscPosTcpPrinter};
pub use printer::{PrintAlign, PrintDocument, PrintKind, PrintLine, PrinterPort};
pub use scanner::{KeyInput, ScanBuffer, ScanEvent, ScanPolicy};
pub use simulator::{IminBridge, IminDrawer, SimulatedHardware, SimulatorDrawer, SimulatorPrinter};
pub use spooler::{list_queues, EscPosSpoolerPrinter, SpoolerQueue};
pub use swappable::SwappablePrinter;
pub use unconfigured::{UnconfiguredDrawer, UnconfiguredPrinter, UNCONFIGURED};
