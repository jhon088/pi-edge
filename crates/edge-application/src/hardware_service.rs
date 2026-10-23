//! Hardware use cases: diagnostics, printing, and the drawer.
//!
//! The rule the whole file is arranged around: **a physical side effect never
//! participates in a commercial transaction** (ADR 005, ADR 037). So every
//! device call happens *after* the database has committed, and what it answers
//! is recorded in a second transaction. A printer that jams and a drawer that
//! stays shut are reported; they change no money, no stock and no sale.
//!
//! The claim-before-call shape is the one payments already use for providers
//! (ADR 020): the action is durable *before* the device is touched, so a
//! process that dies mid-action leaves evidence that it may already have
//! happened — and the safe answer to "did the drawer open?" is yes.

use std::sync::{Arc, Mutex};

use edge_domain::prelude::*;
use edge_hardware::{
    CashDrawerPort, DrawerOpenReason, HardwareDeviceKind, HardwareDeviceStatus, HardwareError,
    PrintDocument, PrintKind, PrintLine, PrinterPort, SimulatorDrawer, SimulatorPrinter,
    UnconfiguredDrawer, UnconfiguredPrinter,
};
use uuid::Uuid;

use crate::commands::{OpenDrawerCommand, PrintTransactionCommand, SubmitScanCommand};
use crate::context::require_device;
use crate::error::AppError;
use crate::ports::{
    Clock, DeviceProfile, EdgeStore, HardwareAction, HardwareActionKind, HardwareActionStatus,
    IdFactory,
};
use crate::queries::HardwareStatusView;
use crate::service::EdgeApplication;

/// The devices one installation drives.
///
/// Composed at the composition root, which is the only place that knows a
/// host, a port or a driver name. Nothing downstream can discover them
/// (ADR 039).
#[derive(Clone)]
pub struct Hardware {
    pub printer: Arc<dyn PrinterPort>,
    pub drawer: Arc<dyn CashDrawerPort>,
    /// Characters per line of the configured paper, so the document the core
    /// composes matches the paper it will come out on.
    pub paper_width: usize,
    /// When this process last saw a scan become an item.
    ///
    /// A scanner cannot be probed — it is input — so the only evidence it works
    /// is that it worked. Held in process memory rather than read from the
    /// action log, because a restart must not let a scan from yesterday claim
    /// the device is working now (ADR 035).
    scanner_seen: Arc<Mutex<Option<Timestamp>>>,
}

impl std::fmt::Debug for Hardware {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Hardware")
            .field("printer", &self.printer.driver())
            .field("drawer", &self.drawer.driver())
            .field("paper_width", &self.paper_width)
            .finish()
    }
}

impl Hardware {
    /// Nothing configured. What a core gets unless a composition root says
    /// otherwise.
    ///
    /// **A simulator is never selected implicitly.** A runtime whose devices
    /// were not configured answers `HARDWARE_NOT_CONFIGURED` to everything,
    /// because quietly succeeding against a fake device would let a
    /// misconfigured till report healthy hardware (ADR 038).
    pub fn unconfigured() -> Self {
        Self {
            printer: Arc::new(UnconfiguredPrinter),
            drawer: Arc::new(UnconfiguredDrawer),
            paper_width: 48,
            scanner_seen: Arc::new(Mutex::new(None)),
        }
    }

    /// Everything simulated. **Opt-in only**: a test, a demo or a development
    /// runtime asks for this explicitly, which is what makes it visible that
    /// the hardware is fake.
    pub fn simulated() -> Self {
        Self {
            printer: Arc::new(SimulatorPrinter::working()),
            drawer: Arc::new(SimulatorDrawer::working()),
            paper_width: 48,
            scanner_seen: Arc::new(Mutex::new(None)),
        }
    }

    pub fn new(
        printer: Arc<dyn PrinterPort>,
        drawer: Arc<dyn CashDrawerPort>,
        paper_width: usize,
    ) -> Self {
        Self {
            printer,
            drawer,
            paper_width,
            scanner_seen: Arc::new(Mutex::new(None)),
        }
    }

    /// Record that a scan arrived and became an item.
    fn saw_scan(&self, now: Timestamp) {
        if let Ok(mut seen) = self.scanner_seen.lock() {
            *seen = Some(now);
        }
    }

    /// When this process last saw a scan.
    fn scanner_seen_at(&self) -> Option<Timestamp> {
        self.scanner_seen.lock().ok().and_then(|seen| *seen)
    }
}

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: edge_payments::PaymentProviderPort,
{
    /// What each device is, and what we know about it.
    ///
    /// Every device starts `UNKNOWN` and is *probed*: configured is not
    /// available, and an `AVAILABLE` recorded before the process died says
    /// nothing about now. `last_success_at` is rebuilt from the action log, so
    /// it survives a restart while the current state does not (ADR 035).
    pub fn get_hardware_status(&self) -> Result<HardwareStatusView, AppError> {
        let now = self.clock.now();
        let printer_probe = self.hardware.printer.probe();
        let drawer_probe = self.hardware.drawer.probe();
        let printer_driver = self.hardware.printer.driver().to_owned();
        let drawer_driver = self.hardware.drawer.driver().to_owned();

        self.store().transact(move |uow| {
            let mut printer = HardwareDeviceStatus::unknown(
                HardwareDeviceKind::Printer,
                "printer",
                Some(printer_driver.clone()),
            );
            printer.record(printer_probe.as_ref().copied(), now);
            printer.last_success_at = uow
                .last_hardware_success(HardwareDeviceKind::Printer)?
                .or(printer.last_success_at);

            let mut drawer = HardwareDeviceStatus::unknown(
                HardwareDeviceKind::CashDrawer,
                "cash-drawer",
                Some(drawer_driver.clone()),
            );
            drawer.record(drawer_probe.as_ref().copied(), now);
            drawer.last_success_at = uow
                .last_hardware_success(HardwareDeviceKind::CashDrawer)?
                .or(drawer.last_success_at);

            // A scanner is input: there is nothing to probe. So the only
            // evidence it works is that it worked *in this process* — a scan
            // from before a restart says nothing about now, which is why this
            // comes from memory and `last_success_at` comes from the log
            // (ADR 035, ADR 036).
            let mut scanner = HardwareDeviceStatus::unknown(
                HardwareDeviceKind::Scanner,
                "scanner",
                Some("keyboard_wedge".to_owned()),
            );
            if let Some(seen) = self.hardware.scanner_seen_at() {
                scanner.record(Ok(()), seen);
            }
            scanner.last_success_at = uow
                .last_hardware_success(HardwareDeviceKind::Scanner)?
                .or(scanner.last_success_at);

            Ok(HardwareStatusView {
                scanner,
                printer,
                drawer,
            })
        })
    }

    /// Print a diagnostic page. Touches no commercial data.
    pub fn print_test_page(&self) -> Result<HardwareStatusView, AppError> {
        let now = self.clock.now();
        let job_id = self.ids.new_uuid();
        let device = self.store().transact(require_device)?;

        let document = self.test_page(job_id, &device, now);
        self.perform(
            HardwareActionKind::Print,
            HardwareDeviceKind::Printer,
            PrintKind::TestPage.as_str(),
            // A diagnostic has no commercial cause, so it may repeat as often
            // as an operator asks.
            None,
            now,
            || self.hardware.printer.print(&document),
        )?;
        self.get_hardware_status()
    }

    /// Print a finalized sale, or print it again.
    ///
    /// A reprint puts paper out and does nothing else: no cash moves, no stock
    /// moves, no drawer opens, and the sale is untouched (ADR 037). The source
    /// is the sale itself, never a stored job (ADR 039).
    pub fn print_transaction(
        &self,
        cmd: PrintTransactionCommand,
    ) -> Result<HardwareStatusView, AppError> {
        let now = self.clock.now();
        let job_id = self.ids.new_uuid();
        let (device, transaction) = self.store().transact(move |uow| {
            let device = require_device(uow)?;
            let transaction =
                crate::receipt_service::require_finalized_sale(uow, cmd.transaction_id)?;
            Ok((device, transaction))
        })?;

        if !cmd.reprint {
            // An explicit retry of the first receipt. Allowed only while the
            // obligation says no paper came out; the receipt lifecycle owns
            // that decision (ADR 043).
            self.attempt_first_receipt(cmd.transaction_id, now)?;
            return self.get_hardware_status();
        }

        // A reprint is an explicit act, repeatable, marked as a copy, and it
        // changes nothing: no cash, no stock, no drawer, no sale — and **not
        // the receipt obligation**. If the first receipt's fate is unknown it
        // stays unknown: printing a copy today says nothing about yesterday
        // (ADR 037, ADR 043).
        let document = self.receipt_document(job_id, PrintKind::Reprint, &device, &transaction);
        self.perform(
            HardwareActionKind::Print,
            HardwareDeviceKind::Printer,
            PrintKind::Reprint.as_str(),
            // Unkeyed: an operator asking for a copy is not a replay.
            None,
            now,
            || self.hardware.printer.print(&document),
        )?;
        self.get_hardware_status()
    }

    /// Open the drawer because an operator asked.
    ///
    /// Not keyed by any commercial cause, so it opens every time it is asked —
    /// which is the point of a diagnostic, and why it is audited (ADR 037).
    pub fn open_drawer_diagnostic(
        &self,
        cmd: OpenDrawerCommand,
    ) -> Result<HardwareStatusView, AppError> {
        let now = self.clock.now();
        let _ = cmd.request_id;
        self.perform(
            HardwareActionKind::DrawerOpen,
            HardwareDeviceKind::CashDrawer,
            DrawerOpenReason::Diagnostic.as_str(),
            None,
            now,
            || self.hardware.drawer.open(DrawerOpenReason::Diagnostic),
        )?;
        self.get_hardware_status()
    }

    /// A physical scan became an item on the active sale.
    ///
    /// The scan carries an `event_id` for *this keypress sequence*, and it is
    /// used as the command's `request_id`: a resent event adds one item, and
    /// two scans of the same product add two (ADR 036).
    ///
    /// Resolution goes through the ordinary `AddItem` use case, so a scanned
    /// barcode takes exactly the path a typed one takes — the same catalog
    /// snapshot, the same promotions, the same basket-freeze rules. There is no
    /// hardware-specific product lookup, and there never will be.
    pub fn submit_scan(&self, cmd: SubmitScanCommand) -> Result<RetailTransaction, AppError> {
        let scan = cmd.scan;
        let barcode =
            edge_catalog::Barcode::new(scan.barcode.clone()).map_err(AppError::Catalog)?;
        let outcome = self.add_item(crate::commands::AddItemCommand {
            request_id: Some(scan.event_id),
            selector: edge_catalog::ProductSelector::Barcode(barcode),
            quantity: Quantity::from_units(1).map_err(DomainError::from)?,
        });

        // A scan with nothing to add to is refused: PI.Edge does not open a
        // sale on its own, because no product decision says it should
        // (ADR 036). The code is scanner-specific so a till can tell this
        // apart from a command issued with no sale open.
        let transaction = match outcome {
            Err(AppError::NoActiveTransaction) => Err(AppError::ScanWithoutTransaction),
            other => other,
        }?;

        // The scanner is the one device whose "it worked" arrives as input
        // rather than as a command, so this is where its last success comes
        // from. Keyed by the event, so a replay records nothing new.
        let now = self.clock.now();
        let action_id = self.ids.new_uuid();
        self.store().transact(|uow| {
            if let Some(action_id) = uow.claim_hardware_action(&HardwareAction {
                action_id,
                kind: HardwareActionKind::Print,
                device_kind: HardwareDeviceKind::Scanner,
                reason: "SCAN".to_owned(),
                source_ref: Some(scan.event_id.to_string()),
                status: HardwareActionStatus::Pending,
                error_code: None,
                error_message: None,
                requested_at: now,
                completed_at: None,
            })? {
                uow.resolve_hardware_action(action_id, Ok(()), now)?;
            }
            Ok(())
        })?;
        self.hardware.saw_scan(now);
        Ok(transaction)
    }

    // --------------------------------------------------------------- internals

    /// Claim, call the device, record the answer.
    ///
    /// The three steps are three separate database interactions on purpose:
    /// the device is touched with **no transaction open**, so a slow printer
    /// cannot hold a write lock and a failing one cannot roll anything back
    /// (ADR 005, ADR 037).
    pub(crate) fn perform<F>(
        &self,
        kind: HardwareActionKind,
        device_kind: HardwareDeviceKind,
        reason: &str,
        source_ref: Option<String>,
        now: Timestamp,
        call: F,
    ) -> Result<(), AppError>
    where
        F: FnOnce() -> Result<(), HardwareError>,
    {
        let action = HardwareAction {
            action_id: self.ids.new_uuid(),
            kind,
            device_kind,
            reason: reason.to_owned(),
            source_ref,
            status: HardwareActionStatus::Pending,
            error_code: None,
            error_message: None,
            requested_at: now,
            completed_at: None,
        };
        let Some(action_id) = self
            .store()
            .transact(|uow| uow.claim_hardware_action(&action))?
        else {
            // This cause already has a physical action taken. Saying so is
            // the whole reason the log exists.
            return Err(AppError::Hardware(HardwareError::ActionAlreadyCompleted {
                source_ref: action.source_ref.unwrap_or_default(),
            }));
        };

        let outcome = call();
        let recorded = outcome.as_ref().copied();
        self.store()
            .transact(|uow| uow.resolve_hardware_action(action_id, recorded, self.clock.now()))?;
        outcome.map_err(AppError::Hardware)
    }

    /// Open the drawer because a commercial event says cash moved.
    ///
    /// Keyed by that event, so a replayed payment or refund cannot open it
    /// twice — and a failure is recorded and swallowed, because the money has
    /// already moved and no drawer can un-move it (ADR 037).
    pub(crate) fn open_drawer_for(
        &self,
        reason: DrawerOpenReason,
        source_ref: String,
        now: Timestamp,
    ) {
        let outcome = self.perform(
            HardwareActionKind::DrawerOpen,
            HardwareDeviceKind::CashDrawer,
            reason.as_str(),
            Some(source_ref.clone()),
            now,
            || self.hardware.drawer.open(reason),
        );
        if let Err(error) = outcome {
            // Deliberately not propagated: the sale is paid, the ledger is
            // right, and a drawer that stayed shut is an operational problem,
            // not a financial one.
            tracing::warn!(
                reason = reason.as_str(),
                %source_ref,
                error = %error,
                "the cash drawer did not open; the money has already moved"
            );
        }
    }

    fn test_page(&self, job_id: Uuid, device: &DeviceProfile, now: Timestamp) -> PrintDocument {
        let width = self.hardware.paper_width;
        PrintDocument::new(job_id, PrintKind::TestPage, width)
            .line(PrintLine::centered("PI.EDGE").bold().large())
            .line(PrintLine::centered("PRUEBA DE IMPRESION"))
            .blank()
            .rule()
            .line(PrintLine::columns(
                "Tienda",
                device.store_id.as_str(),
                width,
            ))
            .line(PrintLine::columns(
                "Tenant",
                device.tenant_id.as_str(),
                width,
            ))
            .line(PrintLine::columns(
                "Driver",
                self.hardware.printer.driver(),
                width,
            ))
            .line(PrintLine::columns("Ancho", &width.to_string(), width))
            .line(PrintLine::columns("Fecha", &now.to_rfc3339(), width))
            .rule()
            .blank()
            .line(PrintLine::centered("1234567890 ABCDEFGHIJ"))
            .line(PrintLine::centered("acentos: anio nino 1o"))
            .blank()
    }

    /// A receipt, enough to prove the transport and the layout.
    ///
    /// Deliberately not the final document: no fiscal block, no QR, no
    /// template engine. The commercial and fiscal lifecycle of a receipt is
    /// its own milestone, and this is what milestone 10 needs to put paper out
    /// of a real printer (ADR 039).
    pub(crate) fn receipt_document(
        &self,
        job_id: Uuid,
        kind: PrintKind,
        device: &DeviceProfile,
        transaction: &RetailTransaction,
    ) -> PrintDocument {
        let width = self.hardware.paper_width;
        let state = transaction.state();
        let mut document = PrintDocument::new(job_id, kind, width)
            .line(PrintLine::centered("PI.EDGE").bold().large())
            .line(PrintLine::centered(device.store_id.as_str()))
            .blank();
        if kind == PrintKind::Reprint {
            document = document.line(PrintLine::centered("** COPIA **").bold());
        }
        document = document
            .line(PrintLine::columns(
                "Ticket",
                &state.register_sequence.to_string(),
                width,
            ))
            .line(PrintLine::columns(
                "Caja",
                state.register_id.as_str(),
                width,
            ))
            .line(PrintLine::columns(
                "Cajero",
                state.operator_id.as_str(),
                width,
            ))
            .rule();

        for line in &state.lines {
            document = document
                .line(PrintLine::plain(line.description.clone()))
                .line(PrintLine::columns(
                    &format!("  {} x {}", line.quantity, line.unit_price.format_amount()),
                    &line.line_total.format_amount(),
                    width,
                ));
            if line.discount_amount.is_positive() {
                document = document.line(PrintLine::columns(
                    "  descuento",
                    &format!("-{}", line.discount_amount.format_amount()),
                    width,
                ));
            }
        }

        let totals = &state.totals;
        document = document
            .rule()
            .line(PrintLine::columns(
                "Subtotal",
                &totals.net_total.format_amount(),
                width,
            ))
            .line(PrintLine::columns(
                "Impuesto",
                &totals.tax_total.format_amount(),
                width,
            ))
            .line(
                PrintLine::columns("TOTAL", &totals.grand_total.format_amount(), width)
                    .bold()
                    .large(),
            )
            .blank()
            .line(PrintLine::centered("Gracias por su compra"))
            .blank();
        document
    }
}
