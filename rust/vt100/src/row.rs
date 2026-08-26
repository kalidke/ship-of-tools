#[derive(Clone, Debug)]
pub struct Row {
    cells: Vec<crate::Cell>,
    wrapped: bool,
}

impl Row {
    pub fn new(cols: u16) -> Self {
        Self {
            cells: vec![crate::Cell::new(); usize::from(cols)],
            wrapped: false,
        }
    }

    /// The row's width in cells.
    pub(crate) fn len(&self) -> u16 {
        self.cols()
    }

    fn cols(&self) -> u16 {
        self.cells
            .len()
            .try_into()
            // we limit the number of cols to a u16 (see Size)
            .unwrap()
    }

    pub fn clear(&mut self, attrs: crate::attrs::Attrs) {
        for cell in &mut self.cells {
            cell.clear(attrs);
        }
        self.wrapped = false;
    }

    fn cells(&self) -> impl Iterator<Item = &crate::Cell> {
        self.cells.iter()
    }

    pub fn get(&self, col: u16) -> Option<&crate::Cell> {
        self.cells.get(usize::from(col))
    }

    pub fn get_mut(&mut self, col: u16) -> Option<&mut crate::Cell> {
        self.cells.get_mut(usize::from(col))
    }

    pub fn insert(&mut self, i: u16, cell: crate::Cell) {
        self.cells.insert(usize::from(i), cell);
        self.wrapped = false;
    }

    pub fn remove(&mut self, i: u16) {
        self.clear_wide(i);
        self.cells.remove(usize::from(i));
        self.wrapped = false;
    }

    pub fn erase(&mut self, i: u16, attrs: crate::attrs::Attrs) {
        let wide = self.cells[usize::from(i)].is_wide();
        self.clear_wide(i);
        self.cells[usize::from(i)].clear(attrs);
        if i == self.cols() - if wide { 2 } else { 1 } {
            self.wrapped = false;
        }
    }

    pub fn truncate(&mut self, len: u16) {
        self.cells.truncate(usize::from(len));
        self.wrapped = false;
        self.clear_orphaned_wide_lead();
    }

    pub fn resize(&mut self, len: u16, cell: crate::Cell) {
        self.cells.resize(usize::from(len), cell);
        self.wrapped = false;
        // Shrinking can cut a wide character in half, leaving its leading
        // cell in the final column with no continuation after it. Drawing
        // over such a cell reaches for a continuation that is not there and
        // panics (`screen::text`), so the halves have to be kept paired here
        // — the same repair `truncate` has always done.
        self.clear_orphaned_wide_lead();
    }

    /// Clears a wide leading cell left in the last column, where its
    /// continuation cannot be.
    ///
    /// Growing never produces one (the appended cells are blank) so this is
    /// safe to call after either direction, and an empty row has nothing to
    /// repair.
    fn clear_orphaned_wide_lead(&mut self) {
        if let Some(last_cell) = self.cells.last_mut() {
            if last_cell.is_wide() {
                last_cell.clear(*last_cell.attrs());
            }
        }
    }

    pub fn wrap(&mut self, wrap: bool) {
        self.wrapped = wrap;
    }

    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    pub fn clear_wide(&mut self, col: u16) {
        let cell = &self.cells[usize::from(col)];
        let other = if cell.is_wide() {
            &mut self.cells[usize::from(col + 1)]
        } else if cell.is_wide_continuation() {
            &mut self.cells[usize::from(col - 1)]
        } else {
            return;
        };
        other.clear(*other.attrs());
    }

    pub fn write_contents(
        &self,
        contents: &mut String,
        start: u16,
        width: u16,
        wrapping: bool,
    ) {
        let mut prev_was_wide = false;

        let mut prev_col = start;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            if cell.has_contents() {
                for _ in 0..(col - prev_col) {
                    contents.push(' ');
                }
                prev_col += col - prev_col;

                contents.push_str(cell.contents());
                prev_col += if cell.is_wide() { 2 } else { 1 };
            }
        }
        if prev_col == start && wrapping {
            contents.push('\n');
        }
    }

}

// ---------------------------------------------------------------------------
// Checkpoint codec (ADR 0041 step 3). Format spec: `crate::checkpoint`.
// ---------------------------------------------------------------------------

impl Row {
    pub(crate) fn write_checkpoint(&self, out: &mut Vec<u8>) {
        out.push(u8::from(self.wrapped));
        for cell in &self.cells {
            cell.write_checkpoint(out);
        }
    }

    /// Reads one row of exactly `cols` cells.
    ///
    /// The column count comes from the checkpoint header rather than from a
    /// per-row length, because every grid mutator that changes a row's length
    /// restores it before returning (`insert_cells` truncates, `delete_cells`
    /// resizes, `set_size` resizes every row). Taking `cols` from the header
    /// makes that invariant explicit: a payload whose rows disagree with the
    /// header runs out of bytes or trails them, and is refused either way.
    pub(crate) fn read_checkpoint(
        r: &mut crate::checkpoint::Reader<'_>,
        cols: u16,
    ) -> Result<Self, crate::checkpoint::CheckpointError> {
        let wrapped = r.bool("row wrap flag is not 0 or 1")?;
        let mut cells = Vec::with_capacity(usize::from(cols));
        for _ in 0..cols {
            cells.push(crate::Cell::read_checkpoint(r)?);
        }
        Self::check_invariants(&cells)?;
        Ok(Self { cells, wrapped })
    }

    /// Refuses a row the parser could not have produced.
    ///
    /// Pairing is the load-bearing one: `screen::text` indexes `col - 1` and
    /// `col + 1` on the strength of every wide character having both halves,
    /// so an orphan does not render oddly — it panics on the next glyph
    /// written over it, far from the restore that admitted it.
    ///
    /// The rest are shape rules that keep restored rows renderable the same
    /// way parsed ones are. A continuation carrying its own text is the clear
    /// case: `Grid::write_contents` skips continuations, so that text would
    /// be invisible in the row's string form while a cell-by-cell renderer
    /// drew it — two views of one screen disagreeing.
    ///
    /// The wrap flag is NOT checked, and the attempt is instructive. It looks
    /// like it should be: `col_wrap` sets it only when the last cell held
    /// something to wrap from, and every mutator that blanks that edge clears
    /// it. But a one-row scroll region breaks the reasoning — parse
    /// `CSI 3;6r` at 10x2, resize to 3x2 (which clamps the region to
    /// `2..=2`), then write three characters: the wrap scrolls the wrapping
    /// row away and `col_wrap` marks the blank row above it instead. That is
    /// an odd flag on an odd row, but it is what the parser produces, and a
    /// rule that refuses it refuses a real session. Repairing `col_wrap`
    /// instead would be a second change to upstream parsing behavior on a
    /// fork carrying none of upstream's conformance tests, to keep a rule
    /// whose only benefit is cosmetic.
    ///
    /// DELIBERATELY NOT CHECKED, and not an oversight: whether the wide bit
    /// agrees with the character's Unicode display width, whether a cell's
    /// trailing characters are zero-width, and whether its content is 22
    /// bytes when the parser stops at 21. The first two would mean consulting
    /// a width table during restore, and those tables change between
    /// `unicode-width` releases — a screen checkpointed under one and
    /// restored under another would be refused for having the wrong idea
    /// about an emoji. The stored structural flags are authoritative
    /// precisely so that cannot happen. The third is arithmetic internal to
    /// `Cell::append` that would turn any upstream tweak into a false
    /// rejection, and admitting one extra byte costs nothing: it cannot
    /// overflow the array, and no renderer reads past the length.
    fn check_invariants(
        cells: &[crate::Cell],
    ) -> Result<(), crate::checkpoint::CheckpointError> {
        let orphan = |_col: usize| {
            Err(crate::checkpoint::CheckpointError::Malformed(
                "a wide character with only one of its two halves",
            ))
        };
        let unreachable_cell = |_col: usize, what: &'static str| {
            Err(crate::checkpoint::CheckpointError::Malformed(what))
        };
        for (col, cell) in cells.iter().enumerate() {
            if cell.is_wide() {
                if cell.is_wide_continuation() {
                    return orphan(col);
                }
                match cells.get(col + 1) {
                    Some(next) if next.is_wide_continuation() => {}
                    _ => return orphan(col),
                }
                // A lead is only ever made by `Cell::set`, which writes the
                // character before setting the bit.
                if !cell.has_contents() {
                    return unreachable_cell(col, "a wide lead with no text");
                }
            } else if cell.is_wide_continuation() {
                match col.checked_sub(1).and_then(|prev| cells.get(prev)) {
                    Some(prev) if prev.is_wide() => {}
                    _ => return orphan(col),
                }
                // Both sites that create a continuation clear the cell to
                // default attributes first — `screen::text` explicitly, and
                // `Grid::insert_cells` by inserting a fresh one.
                if cell.has_contents() {
                    return unreachable_cell(
                        col,
                        "a wide continuation carrying its own text",
                    );
                }
                if cell.attrs() != &crate::attrs::Attrs::default() {
                    return unreachable_cell(
                        col,
                        "a wide continuation with its own attributes",
                    );
                }
            }
        }
        Ok(())
    }
}
