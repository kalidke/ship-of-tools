//! The loop's side of the row socket: a pass's waiting attaches, typed input and resizes.
use super::*;

/// ADR 0041: resize (driver-only) is one `ResizePseudoConsole` call (skipped if out of budget), the parser's
/// geometry updated only on success; whether it was done.
pub(super) fn resize_terminal<P: Producer>(cols: u16, rows: u16, leg: &mut Leg<P>) -> bool {
    let in_budget = (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows);
    if !in_budget {
        return false;
    }
    leg.resize_os_calls += 1;
    match leg.producer.resize(cols, rows) {
        Ok(()) => {
            leg.parser.screen_mut().set_size(rows, cols);
            if let Some(screen) = &mut leg.screen {
                screen.output_arrived();
            }
            true
        }
        Err(_) => false,
    }
}

/// The row socket's side of one pass: waiting attaches get the screen at a ground state, typed input is written to
/// the terminal and answered, a resize is applied.
/// Row requests served in one pass, so output, ticks and the exit checks still run under a flood.
const ROW_REQUESTS_PER_PASS: usize = 16;

pub(super) fn service_row<P: Producer>(leg: &mut Leg<P>) {
    let Some(row) = leg.row.clone() else { return };
    row.tick(&leg.parser);
    for request in row.take_requests(ROW_REQUESTS_PER_PASS) {
        match request {
            // Handed to the input thread: a terminal whose agent reads nothing would hold this loop, and with it the
            // agent's own output.
            crate::row::RowRequest::Input { bytes, reply, .. } => leg.input.write(bytes, reply),
            crate::row::RowRequest::Resize { cols, rows, reply } => {
                let result = if resize_terminal(cols, rows, leg) { Ok(()) } else { Err("the terminal could not be resized".to_string()) };
                let _ = reply.send(result);
            }
        }
    }
}
