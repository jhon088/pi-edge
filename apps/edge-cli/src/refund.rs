//! Argument shapes for the refund commands.
//!
//! Parsing only. The CLI computes no refundable quantity and no amount: it
//! turns `--line 1:2000` into a protocol DTO and prints what the core sends
//! back (ADR 031).

use edge_protocol::RefundLineRequestV1;

use crate::error::CliError;

/// Parse `--line <original-line>:<thousandths>` arguments.
pub fn parse_lines(raw: &[String]) -> Result<Vec<RefundLineRequestV1>, CliError> {
    raw.iter()
        .map(|spec| {
            let (line, quantity) = spec.split_once(':').ok_or_else(|| {
                CliError::Input(format!("`{spec}` is not `<line>:<thousandths>`"))
            })?;
            Ok(RefundLineRequestV1 {
                original_line_number: line
                    .trim()
                    .parse()
                    .map_err(|_| CliError::Input(format!("`{line}` is not a line number")))?,
                quantity_thousandths: quantity.trim().parse().map_err(|_| {
                    CliError::Input(format!("`{quantity}` is not a quantity in thousandths"))
                })?,
            })
        })
        .collect()
}

/// Accept the short spellings an operator would type, and send the contract's.
pub fn parse_fiscal_action(raw: &str) -> Result<String, CliError> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "NONE" => Ok("NONE".to_owned()),
        "PENDING" | "CREDIT_NOTE_PENDING" => Ok("CREDIT_NOTE_PENDING".to_owned()),
        "REQUEST" | "REQUESTED" | "CREDIT_NOTE_REQUESTED" => Ok("CREDIT_NOTE_REQUESTED".to_owned()),
        other => Err(CliError::Input(format!(
            "`{other}` is not a fiscal action (expected NONE, PENDING or REQUEST)"
        ))),
    }
}
