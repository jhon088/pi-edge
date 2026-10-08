//! Payment value object.
//!
//! This milestone only supports CASH, which commits synchronously inside the
//! sale transaction. Provider-based tenders (terminal, wallet) will add
//! `AUTHORIZED` / `DECLINED` / `VOIDED` states and an async lifecycle; the
//! status enum is the extension point.

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{PaymentId, TransactionId};
use crate::money::Money;
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TenderType {
    Cash,
}

impl TenderType {
    pub const fn as_str(self) -> &'static str {
        match self {
            TenderType::Cash => "CASH",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "CASH" => Ok(TenderType::Cash),
            other => Err(DomainError::UnknownEnumValue {
                kind: "TenderType",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PaymentStatus {
    Created,
    Committed,
}

impl PaymentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            PaymentStatus::Created => "CREATED",
            PaymentStatus::Committed => "COMMITTED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "CREATED" => Ok(PaymentStatus::Created),
            "COMMITTED" => Ok(PaymentStatus::Committed),
            other => Err(DomainError::UnknownEnumValue {
                kind: "PaymentStatus",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub payment_id: PaymentId,
    pub transaction_id: TransactionId,
    pub tender_type: TenderType,
    pub amount: Money,
    pub status: PaymentStatus,
    pub created_at: Timestamp,
    pub committed_at: Option<Timestamp>,
}

impl Payment {
    /// Cash is committed in the same step it is created: the drawer movement
    /// is physical and immediate.
    pub fn committed_cash(
        payment_id: PaymentId,
        transaction_id: TransactionId,
        amount: Money,
        now: Timestamp,
    ) -> Result<Self, DomainError> {
        if !amount.is_positive() {
            return Err(DomainError::NonPositivePayment);
        }
        Ok(Self {
            payment_id,
            transaction_id,
            tender_type: TenderType::Cash,
            amount,
            status: PaymentStatus::Committed,
            created_at: now,
            committed_at: Some(now),
        })
    }

    pub fn is_committed(&self) -> bool {
        self.status == PaymentStatus::Committed
    }
}
