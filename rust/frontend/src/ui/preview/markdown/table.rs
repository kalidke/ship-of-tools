//! Table blocks: inline-text flattening and the rendered-table media block.

use super::*;

// (combining-char strikethrough retired — strike now paints as a real
// 1-px quad in the chrome via `strike_glyph_rects`.)

/// Flatten a node's text content, ignoring inline emphasis nodes. Used
/// by table rendering to measure cell widths before laying out the
/// rendered block — full inline-attr rendering inside table cells stays
/// deferred until the rendering moves off the row-string-padding model.
fn collect_inline_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    let nv = node.data.borrow().value.clone();
    match nv {
        NodeValue::Text(s) => out.push_str(&s),
        NodeValue::Code(c) => out.push_str(&c.literal),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
        _ => {
            for ch in node.children() {
                collect_inline_text(ch, out);
            }
        }
    }
}

/// Per-em monospace size used for table cells. Matches the 0.85× code
/// metric so tables and code blocks read at the same density — useful
/// because cells often contain code-like identifiers.
pub(super) const TABLE_FONT_SCALE: f32 = 0.85;

/// Render a GFM table to a box-drawing string + line count. The walk
/// previously pushed the rendered string straight into the main
/// markdown buffer; we now route it through a per-table cosmic-text
/// buffer instead so it can be laid out at *natural* width without
/// soft-wrapping against the preview pane (Windows reported wide
/// tables get the box-drawing mangled by the pane-width wrap).
///
/// Loses inline emphasis (bold, italic, math, links) inside cells —
/// acceptable v1, listed on the TODO under per-glyph rendering.
pub(super) fn build_table_block<'a>(table_node: &'a AstNode<'a>) -> Option<(String, usize)> {
    let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
    for row_node in table_node.children() {
        if let NodeValue::TableRow(is_header) = &row_node.data.borrow().value {
            let mut cells: Vec<String> = Vec::new();
            for cell_node in row_node.children() {
                if matches!(&cell_node.data.borrow().value, NodeValue::TableCell) {
                    let mut cell_text = String::new();
                    collect_inline_text(cell_node, &mut cell_text);
                    cells.push(cell_text.trim().to_string());
                }
            }
            rows.push((*is_header, cells));
        }
    }
    let ncols = rows.iter().map(|(_, r)| r.len()).max().unwrap_or(0);
    if ncols == 0 {
        return None;
    }
    let mut col_widths: Vec<usize> = vec![0; ncols];
    for (_, row) in &rows {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols {
                col_widths[i] = col_widths[i].max(cell.chars().count());
            }
        }
    }

    let mut block = String::new();
    let mut n_lines: usize = 0;
    // Top border.
    block.push('┌');
    for (i, w) in col_widths.iter().enumerate() {
        for _ in 0..(w + 2) {
            block.push('─');
        }
        block.push(if i + 1 < ncols { '┬' } else { '┐' });
    }
    block.push('\n');
    n_lines += 1;
    for (ridx, (is_header, row)) in rows.iter().enumerate() {
        block.push('│');
        for (i, w) in col_widths.iter().enumerate() {
            let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
            block.push(' ');
            block.push_str(cell);
            for _ in cell.chars().count()..*w {
                block.push(' ');
            }
            block.push(' ');
            block.push('│');
        }
        block.push('\n');
        n_lines += 1;
        // Header separator (double-line) or row separator (single-line).
        if *is_header && ridx + 1 < rows.len() {
            block.push('├');
            for (i, w) in col_widths.iter().enumerate() {
                for _ in 0..(w + 2) {
                    block.push('═');
                }
                block.push(if i + 1 < ncols { '┼' } else { '┤' });
            }
            block.push('\n');
            n_lines += 1;
        }
    }
    // Bottom border.
    block.push('└');
    for (i, w) in col_widths.iter().enumerate() {
        for _ in 0..(w + 2) {
            block.push('─');
        }
        block.push(if i + 1 < ncols { '┴' } else { '┘' });
    }
    block.push('\n');
    n_lines += 1;
    Some((block, n_lines))
}
