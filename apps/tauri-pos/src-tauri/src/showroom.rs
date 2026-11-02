//! What this box is, and what it is doing — for a showroom floor.
//!
//! Everything here is **demo surface**: which brand the screen wears, what the
//! stand calls this kiosk, whether it is open, what it has sold today, and a
//! small HTTP endpoint a floor-monitoring app can poll. None of it is core
//! state. The core owns the money, the catalog, the sale and the business day;
//! this owns the labels around them and a tally of what this screen itself
//! put through.
//!
//! It is deliberately a file and a struct. A showroom kiosk is reconfigured by
//! hand between demos, so the settings have to be readable and editable by
//! whoever is standing in front of it — not hidden in a settings table behind
//! a protocol operation.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::BridgeError;

/// Which lane this installation opens in.
///
/// A presentation choice, not core state: it selects a shell and a channel to
/// read the menu for, and changes nothing the core computes. Four brands, one
/// application — a channel and a shell, never a second build (ADR 047).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Vertical {
    Restaurant,
    TimHortons,
    Sbarro,
    Retail,
}

impl Vertical {
    /// Every lane this build ships, in the order Admin offers them.
    ///
    /// One list, because two would drift: the shell reads it to draw the
    /// brands and bootstrap reads it to decide whether this installation can
    /// actually serve them all. Adding a brand is adding a variant and a row
    /// here, and the next launch notices the new lane has no menu.
    pub const ALL: [Self; 4] = [
        Self::Restaurant,
        Self::TimHortons,
        Self::Sbarro,
        Self::Retail,
    ];

    /// The catalog channel this lane sells from.
    ///
    /// All four read one active snapshot — a store may have exactly one — and
    /// availability per channel decides which aisles each one shows (ADR 044).
    pub fn channel(self) -> &'static str {
        match self {
            Self::Restaurant => "KIOSK",
            Self::TimHortons => "KIOSK-TIM",
            Self::Sbarro => "KIOSK-SBARRO",
            Self::Retail => "KIOSK-RETAIL",
        }
    }
}

/// How this installation reaches a CashGenic note acceptor.
///
/// Stored in the clear beside the database, which is the right trade for a
/// demo machine that is reconfigured by hand on a stand — and the wrong one
/// for anything else. There is no cardholder data here and never can be: the
/// device takes notes, and what comes back is an amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashGenicSettingsV1 {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

fn default_port() -> u16 {
    44_333
}

impl Default for CashGenicSettingsV1 {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_port(),
            username: String::new(),
            password: String::new(),
        }
    }
}

/// What this box is set up as. Local, and only local.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KioskPreferencesV1 {
    pub vertical: Vertical,
    /// So the shell never has to know the mapping; the host owns it.
    pub channel_ref: String,
    /// Whether the retail lane shows the customer their own camera.
    #[serde(default)]
    pub camera_preview: bool,

    // --- what the stand calls this machine -------------------------------
    //
    // Free text, typed in by whoever set the kiosk up. No validation beyond
    // trimming: a showroom labels its machines however it likes, and a demo
    // that refuses "Caja 3" because it wanted a number is a demo that wastes
    // somebody's morning.
    #[serde(default)]
    pub branch_id: String,
    #[serde(default)]
    pub kiosk_id: String,
    #[serde(default)]
    pub cashier_id: String,

    /// Whether the kiosk will take an order at all.
    ///
    /// Closing it stops new sales and shows a notice. It does **not** close
    /// the application, the business day, or anything the core owns — an
    /// operator has to be able to open it again from the same screen.
    #[serde(default = "open_by_default")]
    pub kiosk_open: bool,

    #[serde(default)]
    pub cashgenic: CashGenicSettingsV1,
}

fn open_by_default() -> bool {
    true
}

impl KioskPreferencesV1 {
    fn with_channel(mut self) -> Self {
        self.channel_ref = self.vertical.channel().to_owned();
        self
    }
}

impl Default for KioskPreferencesV1 {
    fn default() -> Self {
        Self {
            vertical: Vertical::Restaurant,
            channel_ref: Vertical::Restaurant.channel().to_owned(),
            camera_preview: false,
            branch_id: String::new(),
            kiosk_id: String::new(),
            cashier_id: String::new(),
            kiosk_open: true,
            cashgenic: CashGenicSettingsV1::default(),
        }
    }
}

/// What this screen has put through since the day was opened.
///
/// The kiosk's own tally, not the core's ledger: it counts the sales *this
/// shell* completed, so "close the day" can zero it without touching anything
/// the core recorded. Good enough for a stand, and said plainly rather than
/// dressed up as reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayTallyV1 {
    /// `OPEN` or `CLOSED`, as the showroom means it.
    #[serde(default = "open_day")]
    pub state: String,
    #[serde(default)]
    pub transaction_count: i64,
    /// Minor units, like every amount the core deals in.
    #[serde(default)]
    pub sales_total_minor: i64,
    #[serde(default)]
    pub cash_total_minor: i64,
    #[serde(default)]
    pub card_total_minor: i64,
    #[serde(default)]
    pub opened_at: Option<String>,
    #[serde(default)]
    pub closed_at: Option<String>,
}

fn open_day() -> String {
    "OPEN".to_owned()
}

impl Default for DayTallyV1 {
    fn default() -> Self {
        Self {
            state: open_day(),
            transaction_count: 0,
            sales_total_minor: 0,
            cash_total_minor: 0,
            card_total_minor: 0,
            opened_at: None,
            closed_at: None,
        }
    }
}

impl DayTallyV1 {
    pub fn is_open(&self) -> bool {
        self.state == "OPEN"
    }

    fn record(&mut self, tender: &str, amount_minor: i64) {
        self.transaction_count += 1;
        self.sales_total_minor += amount_minor;
        match tender {
            "CASH" => self.cash_total_minor += amount_minor,
            _ => self.card_total_minor += amount_minor,
        }
    }
}

// ------------------------------------------------------------------ storage

fn preferences_path(dir: &Path) -> PathBuf {
    dir.join("preferences.json")
}

fn day_path(dir: &Path) -> PathBuf {
    dir.join("day.json")
}

/// Read what is stored, defaulting to a working restaurant with no camera.
///
/// Anything unreadable or unrecognised is the default rather than an error: a
/// corrupt preferences file must not stop a kiosk from opening, and the
/// default is a working screen. The default is also the *quiet* one — nothing
/// here can turn a camera on, or reach for a note acceptor, by failing to
/// parse.
pub fn stored(dir: &Path) -> KioskPreferencesV1 {
    fs::read_to_string(preferences_path(dir))
        .ok()
        .and_then(|text| serde_json::from_str::<KioskPreferencesV1>(&text).ok())
        // The channel is the host's to decide, so it is recomputed rather than
        // trusted: an edited file cannot point a lane at another's goods.
        .map(KioskPreferencesV1::with_channel)
        .unwrap_or_default()
}

pub fn write(dir: &Path, preferences: &KioskPreferencesV1) -> Result<(), BridgeError> {
    save(preferences_path(dir), preferences, "PREFERENCES")
}

pub fn stored_day(dir: &Path) -> DayTallyV1 {
    fs::read_to_string(day_path(dir))
        .ok()
        .and_then(|text| serde_json::from_str::<DayTallyV1>(&text).ok())
        .unwrap_or_default()
}

pub fn write_day(dir: &Path, day: &DayTallyV1) -> Result<(), BridgeError> {
    save(day_path(dir), day, "DAY")
}

fn save<T: Serialize>(path: PathBuf, value: &T, what: &str) -> Result<(), BridgeError> {
    let body = serde_json::to_string_pretty(value).map_err(|error| BridgeError {
        code: format!("{what}_ENCODE_FAILED"),
        message: error.to_string(),
    })?;
    fs::write(path, body).map_err(|error| BridgeError {
        code: format!("{what}_WRITE_FAILED"),
        message: error.to_string(),
    })
}

pub fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, BridgeError> {
    crate::bootstrap::data_dir(app).map_err(|error| BridgeError {
        code: "DATA_DIRECTORY_UNAVAILABLE".to_owned(),
        message: error.to_string(),
    })
}

// ----------------------------------------------------------------- commands

#[tauri::command]
pub fn kiosk_preferences(app: tauri::AppHandle) -> Result<KioskPreferencesV1, BridgeError> {
    Ok(stored(&data_dir(&app)?))
}

/// Name this machine: which branch, which kiosk, which cashier.
#[tauri::command]
pub fn set_identity(
    app: tauri::AppHandle,
    branch_id: String,
    kiosk_id: String,
    cashier_id: String,
) -> Result<KioskPreferencesV1, BridgeError> {
    let dir = data_dir(&app)?;
    let mut preferences = stored(&dir);
    preferences.branch_id = branch_id.trim().to_owned();
    preferences.kiosk_id = kiosk_id.trim().to_owned();
    preferences.cashier_id = cashier_id.trim().to_owned();
    write(&dir, &preferences)?;
    Ok(preferences)
}

/// Open or close the kiosk to customers.
///
/// Closed means the screen will not start a sale. The application keeps
/// running, Admin keeps working, and the business day is untouched — this is
/// a sign on the door, not a shutdown.
#[tauri::command]
pub fn set_kiosk_open(
    app: tauri::AppHandle,
    open: bool,
) -> Result<KioskPreferencesV1, BridgeError> {
    let dir = data_dir(&app)?;
    let mut preferences = stored(&dir);
    preferences.kiosk_open = open;
    write(&dir, &preferences)?;
    Ok(preferences)
}

#[tauri::command]
pub fn day_tally(app: tauri::AppHandle) -> Result<DayTallyV1, BridgeError> {
    Ok(stored_day(&data_dir(&app)?))
}

/// Add a finished sale to today's tally.
///
/// Called by the shell once the core has finalized — never before, and never
/// instead of it. If this fails the sale is still a sale; only the showroom's
/// own counter is wrong.
#[tauri::command]
pub fn record_sale(
    app: tauri::AppHandle,
    tender_type: String,
    amount_minor: i64,
) -> Result<DayTallyV1, BridgeError> {
    let dir = data_dir(&app)?;
    let mut day = stored_day(&dir);
    day.record(&tender_type, amount_minor);
    write_day(&dir, &day)?;
    Ok(day)
}

/// Close the showroom day: the tally stops and is shown as it stands.
#[tauri::command]
pub fn close_day(app: tauri::AppHandle) -> Result<DayTallyV1, BridgeError> {
    let dir = data_dir(&app)?;
    let mut day = stored_day(&dir);
    day.state = "CLOSED".to_owned();
    day.closed_at = Some(now());
    write_day(&dir, &day)?;
    Ok(day)
}

/// Start a new day: counters back to zero.
#[tauri::command]
pub fn open_day_command(app: tauri::AppHandle) -> Result<DayTallyV1, BridgeError> {
    let dir = data_dir(&app)?;
    let day = DayTallyV1 {
        opened_at: Some(now()),
        ..DayTallyV1::default()
    };
    write_day(&dir, &day)?;
    Ok(day)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_lane_sells_from_its_own_channel() {
        assert_eq!(Vertical::Restaurant.channel(), "KIOSK");
        assert_eq!(Vertical::TimHortons.channel(), "KIOSK-TIM");
        assert_eq!(Vertical::Sbarro.channel(), "KIOSK-SBARRO");
        assert_eq!(Vertical::Retail.channel(), "KIOSK-RETAIL");
    }

    /// Every brand this build ships is in the list bootstrap checks, and no
    /// two of them share a channel. A brand missing here is a brand whose
    /// blank menu nobody would notice.
    #[test]
    fn every_lane_this_build_ships_is_listed_once() {
        let channels: Vec<&str> = Vertical::ALL.iter().map(|lane| lane.channel()).collect();
        assert_eq!(
            channels,
            ["KIOSK", "KIOSK-TIM", "KIOSK-SBARRO", "KIOSK-RETAIL"]
        );
        let unique: std::collections::BTreeSet<&&str> = channels.iter().collect();
        assert_eq!(unique.len(), channels.len(), "two lanes share a channel");
    }

    #[test]
    fn a_fresh_installation_is_open_with_no_camera_and_no_note_acceptor() {
        let dir = tempfile::tempdir().unwrap();
        let preferences = stored(dir.path());
        assert_eq!(preferences.vertical, Vertical::Restaurant);
        assert!(preferences.kiosk_open, "a new kiosk opens closed");
        assert!(!preferences.camera_preview);
        assert!(!preferences.cashgenic.enabled);
        assert_eq!(preferences.cashgenic.port, 44_333);
    }

    #[test]
    fn identity_and_the_open_sign_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut preferences = stored(dir.path());
        preferences.branch_id = "001".to_owned();
        preferences.kiosk_id = "K01".to_owned();
        preferences.cashier_id = "CAJERO01".to_owned();
        preferences.kiosk_open = false;
        preferences.vertical = Vertical::Sbarro;
        write(dir.path(), &preferences).unwrap();

        let back = stored(dir.path());
        assert_eq!(back.branch_id, "001");
        assert_eq!(back.kiosk_id, "K01");
        assert_eq!(back.cashier_id, "CAJERO01");
        assert!(!back.kiosk_open);
        assert_eq!(back.vertical, Vertical::Sbarro);
        assert_eq!(back.channel_ref, "KIOSK-SBARRO");
    }

    #[test]
    fn an_unreadable_file_opens_a_working_kiosk_and_reaches_for_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(preferences_path(dir.path()), "{ not json").unwrap();
        let preferences = stored(dir.path());
        assert_eq!(preferences.vertical, Vertical::Restaurant);
        assert!(preferences.kiosk_open);
        assert!(!preferences.camera_preview);
        assert!(!preferences.cashgenic.enabled);
    }

    #[test]
    fn the_channel_is_recomputed_so_an_edited_file_cannot_cross_lanes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            preferences_path(dir.path()),
            r#"{"vertical":"SBARRO","channel_ref":"KIOSK-RETAIL"}"#,
        )
        .unwrap();
        assert_eq!(stored(dir.path()).channel_ref, "KIOSK-SBARRO");
    }

    #[test]
    fn the_day_counts_cash_and_card_apart_and_zeroes_on_a_new_day() {
        let dir = tempfile::tempdir().unwrap();
        let mut day = stored_day(dir.path());
        assert!(day.is_open());

        day.record("CASH", 18_750);
        day.record("CARD", 40_000);
        day.record("CASH", 1_250);
        write_day(dir.path(), &day).unwrap();

        let back = stored_day(dir.path());
        assert_eq!(back.transaction_count, 3);
        assert_eq!(back.sales_total_minor, 60_000);
        assert_eq!(back.cash_total_minor, 20_000);
        assert_eq!(back.card_total_minor, 40_000);

        let fresh = DayTallyV1::default();
        assert_eq!(fresh.transaction_count, 0);
        assert_eq!(fresh.sales_total_minor, 0);
    }

    #[test]
    fn a_closed_day_keeps_its_figures() {
        let dir = tempfile::tempdir().unwrap();
        let mut day = stored_day(dir.path());
        day.record("CARD", 25_000);
        day.state = "CLOSED".to_owned();
        write_day(dir.path(), &day).unwrap();

        let back = stored_day(dir.path());
        assert!(!back.is_open());
        assert_eq!(back.card_total_minor, 25_000);
    }
}
