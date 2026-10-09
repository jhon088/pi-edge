//! Versioned catalog snapshots.
//!
//! A snapshot is immutable content plus a local lifecycle:
//!
//! ```text
//! IMPORTING ──commit──▶ PENDING ──activate──▶ ACTIVE ──superseded by a newer──▶ SUPERSEDED
//!     │
//!     └── abort (discarded)
//! ```
//!
//! * `IMPORTING` never participates in a sale lookup. Not once, not partially.
//! * exactly one `ACTIVE` per store, flipped atomically (ADR 012).
//! * `SUPERSEDED` content is kept, because a sale that captured it must stay
//!   priceable until it is finalized (ADR 013).
//!
//! There is deliberately no `FAILED` state: a commit that fails validation
//! leaves the import in `IMPORTING`, which is exactly the state from which the
//! client can resend chunks and retry. A dead `FAILED` state would need its own
//! cleanup path and would be a second way of saying "not usable".

use edge_domain::prelude::{CatalogSnapshotId, StoreId, TenantId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CatalogSnapshotStatus {
    Importing,
    Pending,
    Active,
    Superseded,
}

impl CatalogSnapshotStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            CatalogSnapshotStatus::Importing => "IMPORTING",
            CatalogSnapshotStatus::Pending => "PENDING",
            CatalogSnapshotStatus::Active => "ACTIVE",
            CatalogSnapshotStatus::Superseded => "SUPERSEDED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CatalogError> {
        match value {
            "IMPORTING" => Ok(CatalogSnapshotStatus::Importing),
            "PENDING" => Ok(CatalogSnapshotStatus::Pending),
            "ACTIVE" => Ok(CatalogSnapshotStatus::Active),
            "SUPERSEDED" => Ok(CatalogSnapshotStatus::Superseded),
            other => Err(CatalogError::InvalidRecord {
                field: "snapshot_status",
                reason: format!("unknown value `{other}`"),
            }),
        }
    }

    /// Whether a sale may resolve products against a snapshot in this state.
    /// `SUPERSEDED` qualifies because an open sale captured it.
    pub const fn resolves_sales(self) -> bool {
        matches!(
            self,
            CatalogSnapshotStatus::Active | CatalogSnapshotStatus::Superseded
        )
    }
}

/// Record counts declared by the producer of the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SnapshotCounts {
    pub tax_profiles: i64,
    pub price_books: i64,
    pub products: i64,
    pub price_entries: i64,
    pub barcodes: i64,
    pub promotions: i64,
}

#[derive(Debug, Clone)]
pub struct NewCatalogSnapshot {
    pub snapshot_id: CatalogSnapshotId,
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    /// Monotonic version assigned upstream.
    pub version: i64,
    /// Hash the client claims the content has. Verified at commit against what
    /// the core actually stored; never trusted on its own.
    pub declared_content_hash: String,
    pub generated_at: Timestamp,
    pub effective_at: Option<Timestamp>,
    pub expected: SnapshotCounts,
    pub imported_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshotState {
    pub snapshot_id: CatalogSnapshotId,
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub version: i64,
    pub status: CatalogSnapshotStatus,
    pub declared_content_hash: String,
    /// Computed by the core from the stored rows, at commit.
    pub content_hash: Option<String>,
    pub generated_at: Timestamp,
    pub effective_at: Option<Timestamp>,
    pub expected: SnapshotCounts,
    pub imported_at: Timestamp,
    pub committed_at: Option<Timestamp>,
    pub activated_at: Option<Timestamp>,
    pub superseded_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CatalogSnapshot(CatalogSnapshotState);

impl CatalogSnapshot {
    pub fn begin_import(new: NewCatalogSnapshot) -> Self {
        Self(CatalogSnapshotState {
            snapshot_id: new.snapshot_id,
            tenant_id: new.tenant_id,
            store_id: new.store_id,
            version: new.version,
            status: CatalogSnapshotStatus::Importing,
            declared_content_hash: new.declared_content_hash,
            content_hash: None,
            generated_at: new.generated_at,
            effective_at: new.effective_at,
            expected: new.expected,
            imported_at: new.imported_at,
            committed_at: None,
            activated_at: None,
            superseded_at: None,
        })
    }

    /// Rebuild from persisted state (storage adapters only).
    pub fn rehydrate(state: CatalogSnapshotState) -> Self {
        Self(state)
    }

    pub fn state(&self) -> &CatalogSnapshotState {
        &self.0
    }

    pub fn into_state(self) -> CatalogSnapshotState {
        self.0
    }

    pub fn id(&self) -> CatalogSnapshotId {
        self.0.snapshot_id
    }

    pub fn version(&self) -> i64 {
        self.0.version
    }

    pub fn status(&self) -> CatalogSnapshotStatus {
        self.0.status
    }

    pub fn resolves_sales(&self) -> bool {
        self.0.status.resolves_sales()
    }

    /// Validation passed: the content is complete and hashes as declared.
    pub fn mark_pending(
        &mut self,
        content_hash: String,
        now: Timestamp,
    ) -> Result<(), CatalogError> {
        match self.0.status {
            CatalogSnapshotStatus::Importing => {
                self.0.status = CatalogSnapshotStatus::Pending;
                self.0.content_hash = Some(content_hash);
                self.0.committed_at = Some(now);
                Ok(())
            }
            other => Err(CatalogError::InvalidTransition {
                from: other.as_str(),
                attempted: "PENDING",
            }),
        }
    }

    pub fn activate(&mut self, now: Timestamp) -> Result<(), CatalogError> {
        match self.0.status {
            CatalogSnapshotStatus::Pending => {
                self.0.status = CatalogSnapshotStatus::Active;
                self.0.activated_at = Some(now);
                Ok(())
            }
            other => Err(CatalogError::SnapshotNotPending {
                snapshot_id: self.0.snapshot_id.to_string(),
                status: other.as_str(),
            }),
        }
    }

    pub fn supersede(&mut self, now: Timestamp) -> Result<(), CatalogError> {
        match self.0.status {
            CatalogSnapshotStatus::Active => {
                self.0.status = CatalogSnapshotStatus::Superseded;
                self.0.superseded_at = Some(now);
                Ok(())
            }
            other => Err(CatalogError::InvalidTransition {
                from: other.as_str(),
                attempted: "SUPERSEDED",
            }),
        }
    }

    /// Guard for chunk writes: only an open import accepts content.
    pub fn require_importing(&self) -> Result<(), CatalogError> {
        if self.0.status == CatalogSnapshotStatus::Importing {
            Ok(())
        } else {
            Err(CatalogError::ImportNotOpen {
                snapshot_id: self.0.snapshot_id.to_string(),
                status: self.0.status.as_str(),
            })
        }
    }

    /// Compare declared expectations against what was actually stored.
    pub fn verify_counts(&self, stored: SnapshotCounts) -> Result<(), CatalogError> {
        let expected = self.0.expected;
        for (what, expected, stored) in [
            ("tax profiles", expected.tax_profiles, stored.tax_profiles),
            ("price books", expected.price_books, stored.price_books),
            ("products", expected.products, stored.products),
            (
                "price entries",
                expected.price_entries,
                stored.price_entries,
            ),
            ("barcodes", expected.barcodes, stored.barcodes),
            ("promotions", expected.promotions, stored.promotions),
        ] {
            if expected != stored {
                return Err(CatalogError::CountMismatch {
                    what,
                    expected,
                    stored,
                });
            }
        }
        Ok(())
    }

    /// Compare the client's declared hash against the core's computation.
    pub fn verify_content_hash(&self, computed: &str) -> Result<(), CatalogError> {
        if self.0.declared_content_hash == computed {
            Ok(())
        } else {
            Err(CatalogError::ContentHashMismatch {
                declared: self.0.declared_content_hash.clone(),
                computed: computed.to_owned(),
            })
        }
    }
}

/// Progress of an in-flight import, so a client can resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProgress {
    pub snapshot_id: CatalogSnapshotId,
    pub status: CatalogSnapshotStatus,
    pub version: i64,
    /// Chunk indices already stored, ascending and contiguous from zero when
    /// the import is healthy.
    pub received_chunks: Vec<i64>,
    pub received_records: i64,
    pub expected: SnapshotCounts,
    pub stored: SnapshotCounts,
}

impl ImportProgress {
    /// Lowest chunk index the client still needs to send, assuming it sends
    /// them in order.
    pub fn next_chunk_index(&self) -> i64 {
        let mut next = 0;
        for index in &self.received_chunks {
            if *index == next {
                next += 1;
            } else {
                break;
            }
        }
        next
    }

    /// Every chunk from 0 to the highest received must be present.
    pub fn require_contiguous(&self) -> Result<(), CatalogError> {
        for (position, index) in self.received_chunks.iter().enumerate() {
            let expected = i64::try_from(position).unwrap_or(i64::MAX);
            if *index != expected {
                return Err(CatalogError::MissingChunk { index: expected });
            }
        }
        if self.received_chunks.is_empty() {
            return Err(CatalogError::MissingChunk { index: 0 });
        }
        Ok(())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ts(seconds: u32) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-10-08T12:00:{seconds:02}Z")).unwrap()
    }

    fn snapshot() -> CatalogSnapshot {
        CatalogSnapshot::begin_import(NewCatalogSnapshot {
            snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
            tenant_id: TenantId::new("T").unwrap(),
            store_id: StoreId::new("S").unwrap(),
            version: 7,
            declared_content_hash: "sha256:abc".to_owned(),
            generated_at: ts(0),
            effective_at: None,
            expected: SnapshotCounts {
                tax_profiles: 1,
                price_books: 1,
                products: 2,
                price_entries: 2,
                barcodes: 3,
                promotions: 1,
            },
            imported_at: ts(1),
        })
    }

    #[test]
    fn an_import_is_never_usable_for_sales() {
        let snapshot = snapshot();
        assert_eq!(snapshot.status(), CatalogSnapshotStatus::Importing);
        assert!(!snapshot.resolves_sales());
        assert!(snapshot.require_importing().is_ok());
    }

    #[test]
    fn walks_the_lifecycle() {
        let mut snapshot = snapshot();
        snapshot
            .mark_pending("sha256:abc".to_owned(), ts(2))
            .unwrap();
        assert_eq!(snapshot.status(), CatalogSnapshotStatus::Pending);
        // Pending content is complete but not yet in force.
        assert!(!snapshot.resolves_sales());
        assert!(snapshot.require_importing().is_err());

        snapshot.activate(ts(3)).unwrap();
        assert_eq!(snapshot.status(), CatalogSnapshotStatus::Active);
        assert!(snapshot.resolves_sales());

        snapshot.supersede(ts(4)).unwrap();
        assert_eq!(snapshot.status(), CatalogSnapshotStatus::Superseded);
        // Still resolves: an open sale captured it.
        assert!(snapshot.resolves_sales());
        assert_eq!(snapshot.state().superseded_at, Some(ts(4)));
    }

    #[test]
    fn refuses_out_of_order_transitions() {
        let mut snapshot = snapshot();
        assert!(matches!(
            snapshot.activate(ts(2)),
            Err(CatalogError::SnapshotNotPending { .. })
        ));
        assert!(snapshot.supersede(ts(2)).is_err());
        snapshot.mark_pending("h".to_owned(), ts(2)).unwrap();
        assert!(snapshot.mark_pending("h".to_owned(), ts(3)).is_err());
    }

    #[test]
    fn verifies_counts_and_hash() {
        let mut snapshot = snapshot();
        let stored = SnapshotCounts {
            tax_profiles: 1,
            price_books: 1,
            products: 2,
            price_entries: 2,
            barcodes: 3,
            promotions: 1,
        };
        assert!(snapshot.verify_counts(stored).is_ok());
        assert!(matches!(
            snapshot.verify_counts(SnapshotCounts {
                products: 1,
                ..stored
            }),
            Err(CatalogError::CountMismatch {
                what: "products",
                expected: 2,
                stored: 1
            })
        ));
        assert!(snapshot.verify_content_hash("sha256:abc").is_ok());
        assert!(matches!(
            snapshot.verify_content_hash("sha256:other"),
            Err(CatalogError::ContentHashMismatch { .. })
        ));
        snapshot
            .mark_pending("sha256:abc".to_owned(), ts(2))
            .unwrap();
        assert_eq!(snapshot.state().content_hash.as_deref(), Some("sha256:abc"));
    }

    #[test]
    fn progress_tracks_the_next_chunk_and_gaps() {
        let mut progress = ImportProgress {
            snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
            status: CatalogSnapshotStatus::Importing,
            version: 7,
            received_chunks: vec![0, 1, 2],
            received_records: 30,
            expected: SnapshotCounts::default(),
            stored: SnapshotCounts::default(),
        };
        assert_eq!(progress.next_chunk_index(), 3);
        assert!(progress.require_contiguous().is_ok());

        progress.received_chunks = vec![0, 2];
        assert_eq!(progress.next_chunk_index(), 1);
        assert!(matches!(
            progress.require_contiguous(),
            Err(CatalogError::MissingChunk { index: 1 })
        ));

        progress.received_chunks = vec![];
        assert_eq!(progress.next_chunk_index(), 0);
        assert!(progress.require_contiguous().is_err());
    }
}
