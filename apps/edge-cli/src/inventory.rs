//! Opening-position importer.
//!
//! The counterpart of `catalog.rs`: a development stand-in for the upstream
//! system that will eventually hand PI.Edge its starting stock. It reads a
//! file and sends it; the core validates every product against the active
//! catalog snapshot and decides whether the store may be bootstrapped at all
//! (ADR 028).

use std::path::Path;

use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{
    ImportInventoryBootstrapRequestV1, InventoryBootstrapItemV1, InventoryBootstrapV1,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::CliError;

/// On-disk opening position. The shape the upstream system will send.
#[derive(Debug, Deserialize)]
pub struct InventoryFixture {
    /// Which upstream position this is. Also the idempotency key.
    pub source_snapshot_id: String,
    /// When upstream took the count, RFC 3339.
    pub generated_at: String,
    pub items: Vec<FixtureItem>,
}

#[derive(Debug, Deserialize)]
pub struct FixtureItem {
    pub product_ref: String,
    /// Thousandths, so `1500` is `1.500`. No floats in a stock figure, for
    /// the same reason there are none in a price.
    pub quantity_thousandths: i64,
}

pub fn import_bootstrap(
    client: &IpcEdgeClient,
    path: &Path,
    request_id: Option<Uuid>,
) -> Result<InventoryBootstrapV1, CliError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", path.display())))?;
    let fixture: InventoryFixture = serde_json::from_str(&raw).map_err(|error| {
        CliError::Input(format!(
            "{} is not a valid opening position: {error}",
            path.display()
        ))
    })?;

    let request = ImportInventoryBootstrapRequestV1 {
        source_snapshot_id: fixture.source_snapshot_id,
        generated_at: fixture.generated_at,
        items: fixture
            .items
            .into_iter()
            .map(|item| InventoryBootstrapItemV1 {
                product_ref: item.product_ref,
                quantity_thousandths: item.quantity_thousandths,
            })
            .collect(),
    };
    Ok(client.import_inventory_bootstrap(request, request_id)?)
}
