//! Forward-only migrations.
//!
//! Each migration is applied once, inside its own transaction, and its
//! checksum is recorded. Editing an applied migration is a hard error instead
//! of a silent schema drift.

use rusqlite::{Connection, OptionalExtension};

use crate::error::StorageError;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_initial_schema",
        sql: include_str!("../migrations/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "0002_catalog",
        sql: include_str!("../migrations/0002_catalog.sql"),
    },
    Migration {
        version: 3,
        name: "0003_price_books",
        sql: include_str!("../migrations/0003_price_books.sql"),
    },
    Migration {
        version: 4,
        name: "0004_promotions",
        sql: include_str!("../migrations/0004_promotions.sql"),
    },
    Migration {
        version: 5,
        name: "0005_payments",
        sql: include_str!("../migrations/0005_payments.sql"),
    },
    Migration {
        version: 6,
        name: "0006_cash_management",
        sql: include_str!("../migrations/0006_cash_management.sql"),
    },
    Migration {
        version: 7,
        name: "0007_inventory",
        sql: include_str!("../migrations/0007_inventory.sql"),
    },
];

/// Schema version this binary expects.
pub fn target_version() -> i64 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

pub fn apply(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             checksum   TEXT NOT NULL,
             applied_at TEXT NOT NULL
         );",
    )?;

    for migration in MIGRATIONS {
        let recorded: Option<String> = conn
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get(0),
            )
            .optional()?;
        let current = checksum(migration.sql);
        match recorded {
            Some(recorded) if recorded == current => continue,
            Some(recorded) => {
                return Err(StorageError::MigrationChecksumMismatch {
                    version: migration.version,
                    name: migration.name,
                    recorded,
                    current,
                })
            }
            None => {
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(migration.sql)?;
                tx.execute(
                    "INSERT INTO schema_migrations (version, name, checksum, applied_at)
                     VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    rusqlite::params![migration.version, migration.name, current],
                )?;
                tx.commit()?;
                tracing::info!(
                    version = migration.version,
                    name = migration.name,
                    "applied migration"
                );
            }
        }
    }
    Ok(())
}

/// FNV-1a over the migration text. Enough to detect edits; not a security
/// primitive.
fn checksum(sql: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in sql.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        apply(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64);
        assert_eq!(target_version(), MIGRATIONS.len() as i64);
    }

    /// The cash ledger and the close declarations are evidence, not working
    /// state. The domain refuses to edit them and the adapter has no statement
    /// that could; these triggers make it impossible rather than merely
    /// unimplemented (ADR 021, ADR 022).
    #[test]
    fn the_cash_ledger_and_close_declarations_are_immutable_in_the_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO business_days
                 (business_day_id, tenant_id, store_id, status, opened_at, version,
                  created_at, updated_at)
             VALUES ('day', 'T', 'S', 'OPEN', 'now', 1, 'now', 'now');
             INSERT INTO register_sessions
                 (session_id, business_day_id, register_id, operator_id, status,
                  opening_cash_minor, currency, opened_at, version, created_at, updated_at)
             VALUES ('s1', 'day', 'R', 'O', 'OPEN', 0, 'MXN', 'now', 1, 'now', 'now');
             INSERT INTO cash_movements
                 (movement_id, business_day_id, session_id, register_id, operator_id,
                  movement_type, direction, amount_minor, currency, created_at)
             VALUES ('m1', 'day', 's1', 'R', 'O', 'OPENING_FLOAT', 'IN', 1000, 'MXN', 'now');
             INSERT INTO session_close_declarations
                 (declaration_id, session_id, closure_kind, declared_minor, expected_minor,
                  variance_minor, currency, declared_by, declared_at)
             VALUES ('d1', 's1', 'DECLARED', 1000, 1000, 0, 'MXN', 'O', 'now');",
        )
        .unwrap();

        for sql in [
            "UPDATE cash_movements SET amount_minor = 1 WHERE movement_id = 'm1'",
            "DELETE FROM cash_movements WHERE movement_id = 'm1'",
            "UPDATE session_close_declarations SET declared_minor = 1 WHERE declaration_id = 'd1'",
        ] {
            let refused = conn.execute(sql, []).is_err();
            assert!(refused, "the database must refuse `{sql}`");
        }

        // The rows are exactly as they were written.
        let amount: i64 = conn
            .query_row(
                "SELECT amount_minor FROM cash_movements WHERE movement_id = 'm1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(amount, 1000);
    }

    /// The inventory ledger is the local record of what happened here. A
    /// movement cannot be edited away, and a bootstrap is recorded once
    /// (ADR 026, ADR 028).
    #[test]
    fn the_inventory_ledger_is_immutable_in_the_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO inventory_movements
                 (movement_id, store_id, product_ref, movement_type, direction,
                  quantity_thousandths, source_ref, occurred_at)
             VALUES ('m1', 'S', 'SKU-001', 'OPENING_BALANCE', 'IN', 5000, 'WMS-1', 'now');
             INSERT INTO inventory_bootstrap_state
                 (store_id, source_snapshot_id, generated_at, item_count, bootstrapped_at)
             VALUES ('S', 'WMS-1', 'now', 1, 'now');",
        )
        .unwrap();

        for sql in [
            "UPDATE inventory_movements SET quantity_thousandths = 1 WHERE movement_id = 'm1'",
            "DELETE FROM inventory_movements WHERE movement_id = 'm1'",
            "UPDATE inventory_bootstrap_state SET source_snapshot_id = 'WMS-2'",
        ] {
            let refused = conn.execute(sql, []).is_err();
            assert!(refused, "the database must refuse `{sql}`");
        }

        let quantity: i64 = conn
            .query_row(
                "SELECT quantity_thousandths FROM inventory_movements WHERE movement_id = 'm1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quantity, 5000);
    }

    /// Each movement type moves exactly one way, a sale movement names its
    /// document, and a manual movement states a reason. These are not
    /// application conventions: the table refuses the row (ADR 026).
    #[test]
    fn the_database_refuses_a_movement_that_contradicts_its_own_type() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        apply(&conn).unwrap();

        for (what, sql) in [
            (
                "an opening balance going out",
                "INSERT INTO inventory_movements
                     (movement_id, store_id, product_ref, movement_type, direction,
                      quantity_thousandths, source_ref, occurred_at)
                 VALUES ('x', 'S', 'P', 'OPENING_BALANCE', 'OUT', 1000, 'r', 'now')",
            ),
            (
                "a sale with no transaction",
                "INSERT INTO inventory_movements
                     (movement_id, store_id, product_ref, movement_type, direction,
                      quantity_thousandths, source_ref, occurred_at)
                 VALUES ('x', 'S', 'P', 'SALE', 'OUT', 1000, 'r', 'now')",
            ),
            (
                "a manual movement with a blank reason",
                "INSERT INTO inventory_movements
                     (movement_id, store_id, product_ref, movement_type, direction,
                      quantity_thousandths, source_ref, reason_code, occurred_at)
                 VALUES ('x', 'S', 'P', 'MANUAL_OUT', 'OUT', 1000, 'r', '  ', 'now')",
            ),
            (
                "a non-positive magnitude",
                "INSERT INTO inventory_movements
                     (movement_id, store_id, product_ref, movement_type, direction,
                      quantity_thousandths, source_ref, occurred_at)
                 VALUES ('x', 'S', 'P', 'OPENING_BALANCE', 'IN', 0, 'r', 'now')",
            ),
        ] {
            let refused = conn.execute(sql, []).is_err();
            assert!(refused, "the database must refuse {what}");
        }
    }

    /// One movement per cause. A replay cannot write a second movement for the
    /// same source, and the uniqueness is per store (ADR 027).
    #[test]
    fn the_database_refuses_a_second_movement_for_the_same_source() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        apply(&conn).unwrap();
        let insert = |id: &str, store: &str| {
            conn.execute(
                "INSERT INTO inventory_movements
                     (movement_id, store_id, product_ref, movement_type, direction,
                      quantity_thousandths, source_ref, reason_code, occurred_at)
                 VALUES (?1, ?2, 'P', 'MANUAL_IN', 'IN', 1000, 'req-1', 'RECOUNT', 'now')",
                rusqlite::params![id, store],
            )
        };
        insert("m1", "STORE-A").unwrap();
        assert!(insert("m2", "STORE-A").is_err());
        // A different store is a different ledger, so the same source id there
        // is not a duplicate.
        insert("m3", "STORE-B").unwrap();
    }

    /// A cash sale states what stayed in the drawer, and the tender audit has
    /// to agree with it (ADR 021).
    #[test]
    fn the_database_refuses_an_unbalanced_cash_sale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        apply(&conn).unwrap();
        let insert = |amount: i64, tendered: i64, change: i64| {
            conn.execute(
                "INSERT INTO cash_movements
                     (movement_id, business_day_id, session_id, register_id, operator_id,
                      movement_type, direction, amount_minor, currency, tendered_minor,
                      change_minor, source_ref, created_at)
                 VALUES (?1, 'day', 's1', 'R', 'O', 'CASH_SALE', 'IN', ?2, 'MXN', ?3, ?4,
                         ?1, 'now')",
                rusqlite::params![format!("m{amount}-{tendered}"), amount, tendered, change],
            )
        };
        // 200 tendered, 84 back, 116 stays: balanced.
        assert!(insert(11_600, 20_000, 8_400).is_ok());
        // Anything else is refused.
        assert!(insert(11_600, 20_000, 1_000).is_err());
    }

    #[test]
    fn detects_edited_migration() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        conn.execute(
            "UPDATE schema_migrations SET checksum = 'fnv1a64:deadbeefdeadbeef' WHERE version = 1",
            [],
        )
        .unwrap();
        let err = apply(&conn).unwrap_err();
        assert!(matches!(
            err,
            StorageError::MigrationChecksumMismatch { version: 1, .. }
        ));
    }
}
