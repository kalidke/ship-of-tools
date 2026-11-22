use super::*;

#[test]
fn whole_row_bottom_stops_at_the_last_whole_row() {
    assert_eq!(whole_row_bottom(100.0, 20.0, 175.0), 160.0);
    assert_eq!(whole_row_bottom(100.0, 20.0, 180.0), 180.0);
    assert_eq!(whole_row_bottom(100.0, 20.0, 179.6), 180.0);
    assert_eq!(whole_row_bottom(100.0, 20.0, 115.0), 100.0);
    assert_eq!(whole_row_bottom(40.0, 20.0, 175.0), 160.0);
    assert_eq!(whole_row_bottom(100.0, 0.0, 175.0), 175.0);
}

/// A MathJax-shaped SVG (an `ex`-sized root, glyph outlines in `defs`, one `use` of each), about 4 KB, the shape the
/// sidecar returns for an inline equation of a dozen glyphs.
fn math_shaped_svg(n: usize) -> String {
    let glyphs = 12;
    let mut svg = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="9.4ex" height="2.1ex" role="img" focusable="false" viewBox="0 -750 4150 950" style="vertical-align: -0.4ex;"><defs>"#,
    );
    for g in 0..glyphs {
        svg.push_str(&format!(
            r#"<path id="MJX-{n}-{g}" d="M{a} 0C{a} 300 120 700 420 710C700 720 760 400 760 200C760 -40 540 -190 300 -190C80 -190 {g} -60 {g} 120L{a} 0Z"/>"#,
            a = 20 + g
        ));
    }
    svg.push_str(r#"</defs><g stroke="currentColor" fill="currentColor" stroke-width="0" transform="scale(1,-1)">"#);
    for g in 0..glyphs {
        svg.push_str(&format!(r##"<use xlink:href="#MJX-{n}-{g}" transform="translate({},0)"/>"##, g * 340));
    }
    svg.push_str("</g></svg>");
    svg
}

impl State {
    /// A page of 50 inline equations, all in view and all replied before the first frame, is drawn by the worker in one
    /// child: every one has its raster, and the page's cost from the first frame to the last raster is printed
    /// (`math-page`). The equations are MathJax-shaped stand-ins.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_page_of_math_is_drawn_by_the_worker_in_one_batch(&mut self) -> anyhow::Result<()> {
        self.redraw()?;
        const EQUATIONS: usize = 50;
        let source: String = (0..EQUATIONS)
            .map(|n| format!("term ${}$ of the series{}", format_args!("a_{{{n}}}"), if n % 10 == 9 { "\n\n" } else { " and " }))
            .collect();
        self.preview_source.install("text/markdown".into(), source.clone().into_bytes(), Some("files:m.md".into()), None, None);
        self.render_preview_source("text/markdown", source.as_bytes());
        // The worker's draw counts each child it starts: a page asked together is one.
        let children = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let counted = children.clone();
        self.svg_draws = crate::ui::preview::image::svg_draws::SvgDraws::new(
            crate::ui::preview::image::svg_draws::QUEUED_BYTES,
            || {},
            std::sync::Arc::new(move |jobs, stop| {
                counted.lock().unwrap().push(jobs.len());
                crate::ui::preview::image::svg_run::draw_in_own_child(jobs, stop)
            }),
        );
        let keys: Vec<_> = (0..EQUATIONS).map(|n| (format!("a_{{{n}}}"), false)).collect();
        for (n, (latex, display)) in keys.iter().enumerate() {
            self.on_math_rendered(latex.clone(), math_shaped_svg(n).into_bytes(), 8.0, *display);
        }
        let drawn = |state: &State| keys.iter().filter(|key| state.md_media.math(key).is_some_and(|e| e.rasterised.is_some())).count();
        let begun = std::time::Instant::now();
        while drawn(self) < EQUATIONS {
            anyhow::ensure!(begun.elapsed() < std::time::Duration::from_secs(30), "{} of {EQUATIONS} equations drawn", drawn(self));
            self.redraw()?;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let children = children.lock().unwrap().clone();
        println!("math-page equations={EQUATIONS} children={} ms={}", children.len(), begun.elapsed().as_millis());
        anyhow::ensure!(children == [EQUATIONS], "the page was drawn by {} children ({children:?}), not one", children.len());
        Ok(())
    }
}

/// A page of `tables` wide tables, each of 12 rows of 8 cells of about 110 characters: about 14,500 characters, and
/// about 21,000 bytes of rendered table text, a table. Each table is headed by a line of text, so the rest of the
/// page is not empty.
fn page_of_tables(tag: &str, tables: usize) -> String { page_of_rows(tag, tables, 12, 8) }

/// A page of `tables` tables, each of `rows` rows of `cells` cells of about 110 characters, each headed by a line.
fn page_of_rows(tag: &str, tables: usize, rows: usize, cells: usize) -> String {
    let mut page = String::new();
    for t in 0..tables {
        page.push_str(&format!("{tag} table {t}.\n\n"));
        for r in 0..rows {
            let cells: Vec<String> = (0..cells).map(|c| format!("{tag}t{t}r{r}c{c} ").repeat(11).trim().to_owned()).collect();
            page.push_str(&format!("| {} |\n", cells.join(" | ")));
            if r == 0 {
                page.push_str(&format!("|{}\n", "---|".repeat(cells.len())));
            }
        }
        page.push('\n');
    }
    page
}

impl State {
    /// One frame, its length, and the shaped table rows it left.
    fn tables_frame(&mut self) -> anyhow::Result<(std::time::Duration, usize)> {
        let begun = std::time::Instant::now();
        self.redraw()?;
        Ok((begun.elapsed(), self.held_rows()))
    }

    /// Whether a row of the set held is not shaped yet.
    fn held_unshaped(&self) -> bool {
        self.md_media.tables().iter().flat_map(|entry| &entry.buffer.lines).any(|line| line.layout_opt().is_none())
    }

    /// The shaped rows of the set held, whatever page it is of.
    fn held_rows(&self) -> usize {
        self.md_media.tables().iter().flat_map(|entry| &entry.buffer.lines).filter(|line| line.layout_opt().is_some()).count()
    }

    /// The REPL drawer's own key: open, then full height, then closed.
    fn press_repl_key(&mut self) {
        let base = self.settings.resolve_preset(self.monitor_aspect);
        let effect = self.layout_help.cycle_drawer(DrawerContent::Repl, self.focus_prompts.focus(), base);
        effect.apply(self, None);
    }

    /// Show `page` as the preview of the row now shown, as a reply to its request does.
    fn show_page(&mut self, node: &str, page: &str) {
        self.preview_source.install("text/markdown".into(), page.as_bytes().to_vec(), Some(node.into()), None, None);
        self.render_preview_source("text/markdown", page.as_bytes());
    }

    /// A row whose preview holds seven wide tables (about 100,000 characters of table text) shows the rows in view at
    /// once when the person returns to it, and shows its panes first when it arrives, or its preview is uncovered, with
    /// no set of its tables kept: that frame shapes no row, asks for the next, and the next shapes those in view. A
    /// covered preview builds none, and a return from under the cover takes the set back. A deferred build marks the
    /// pacer dirty and holds no set of another page. A page over the budget keeps its rows and their scroll through a
    /// cover; a page changed under the cover draws none at the uncover and those in view at the next frame. A reply's
    /// frame long after an arrival still shapes in its own frame (the deferral is spent by the frame that took it). The
    /// asserted facts are the shaped rows a frame leaves; each frame's length is printed (`tables-frame`,
    /// microseconds). The one time bound is relative, to this window's own cost of shaping the rows in view in the
    /// frame after the first arrival, so a slow runner moves both alike; the held counts carry the proof.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_row_of_wide_tables_arrives_without_shaping_them_in_its_first_frame(&mut self) -> anyhow::Result<()> {
        let host = "<host-tables>".to_string();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        let row = |slug: &str, default: bool| {
            let mut row = nav::ws_info(slug, &format!("{slug}-session"));
            row.workspace_id = format!("id-{slug}");
            row.is_default = default;
            if default {
                row.agent = "none".into();
            }
            row
        };
        self.on_workspaces(host.clone(), vec![row("default", true), row("s1", false), row("s2", false), row("s3", false)]);
        let go = |s: &mut State, slug: &str| s.switch_to_workspace(host.clone(), Some(slug.into()), None, true);
        let (big, small) = (page_of_tables("big", 7), page_of_tables("small", 1));
        // The characters of table text on the page, and the bytes the kept sets charge for them.
        let sizes = |s: &State| s.shaped.md().media_blocks.iter().fold((0, 0, 0), |(n, chars, bytes), b| match b {
            crate::ui::preview::markdown::MediaBlock::Table { rendered, .. } => (n + 1, chars + rendered.chars().count(), bytes + rendered.len()),
            _ => (n, chars, bytes),
        });
        // Every rule is checked and the fixture fails after the last, so one run says which hold.
        let mut failed = Vec::new();
        let mut check = |held: bool, words: String| if !held { failed.push(words) };
        // The first visit: s1 arrives, the arrival frame shapes nothing, the next shapes those in view.
        go(self, "s1");
        self.show_page("files:big.md", &big);
        let (tables, chars, bytes) = sizes(self);
        anyhow::ensure!((90_000..115_000).contains(&chars) && tables == 7, "the page's tables: {tables} of {chars} characters");
        let (arrival, held) = self.tables_frame()?;
        let (build, built, _, wrong) = self.view_frame(false)?;
        println!("tables-frame first_arrival_us={} held={held} next_us={} held={built} chars={chars} bytes={bytes}", arrival.as_micros(), build.as_micros());
        check(held == 0 && built > 0 && wrong.is_empty(),
            format!("a first arrival held {held} rows in its first frame and {built} in its next: {wrong:?}"));
        // s2 with a small page, then the return to s1: those in view in the first frame.
        go(self, "s2");
        self.show_page("files:small.md", &small);
        let (_, stale) = self.tables_frame()?;
        check(stale == 0 && self.md_media.tables_match(&self.table_sources()),
            format!("the deferred frame of s2 held {stale} shaped rows, or a set of another page"));
        let (_, small_held) = self.tables_frame()?;
        check(small_held > 0, String::from("s2's table was not shaped by its second frame"));
        go(self, "s1");
        let (back, held) = self.tables_frame()?;
        println!("tables-frame return_us={} held={held}", back.as_micros());
        check(held == built, format!("a return held {held} rows in its first frame, not the {built} in view"));
        check(back * 2 < build, format!("a return's first frame took {back:?}, against {build:?} to shape the tables"));
        // An arrival with no set kept: the panes first, the tables a frame later.
        go(self, "s2");
        self.tables_frame()?;
        self.md_media = MarkdownMedia::default();
        go(self, "s1");
        let (cold, held) = self.tables_frame()?;
        let (_, then) = self.tables_frame()?;
        println!("tables-frame cold_arrival_us={} held={held} next_held={then}", cold.as_micros());
        check(held == 0 && then == built, format!("a cold arrival held {held} in its first frame and {then} in its next"));
        check(cold * 2 < build, format!("a cold arrival's first frame took {cold:?}, against {build:?} to shape the tables"));
        // A covered preview: a full-height drawer builds nothing and keeps the set it holds, so uncovering draws it at once.
        self.press_repl_key();
        self.press_repl_key();
        let (_, held) = self.tables_frame()?;
        check(self.preview_covered() && held == built, format!("a covered preview held {held} rows, not the {built} it had (covered: {})", self.preview_covered()));
        self.press_repl_key();
        let (back_uncovered, held) = self.tables_frame()?;
        println!("tables-frame uncover_kept_us={} held={held}", back_uncovered.as_micros());
        check(!self.preview_covered() && held == built, format!("the preview uncovered with its set kept held {held} rows"));
        // With no set kept, the frame that uncovers shapes none, asks for the next, and the next shapes those in view.
        self.press_repl_key();
        self.press_repl_key();
        self.md_media = MarkdownMedia::default();
        for _ in 0..3 {
            let (_, held) = self.tables_frame()?;
            check(held == 0, format!("a covered preview shaped {held} rows"));
        }
        self.press_repl_key();
        let (uncover, held) = self.tables_frame()?;
        check(!self.preview_covered() && held == 0, format!("the frame that uncovered the preview held {held} rows"));
        let (shaped, then) = self.tables_frame()?;
        println!("tables-frame uncover_cold_us={} held={held} next_us={} next_held={then}", uncover.as_micros(), shaped.as_micros());
        check(then == built, format!("the frame after the uncover held {then} rows, not the {built} in view"));
        // A deferral is spent by the frame that takes it: s3 shows the very tables s2 holds, so nothing is shaped and
        // the deferral its arrival set is taken by a frame that has no use for it; a reply that then changes the
        // table is shaped in its own frame.
        go(self, "s2");
        self.tables_frame()?;
        let (_, held) = self.tables_frame()?;
        check(held > 0, format!("s2's table was not held after its second frame ({held})"));
        go(self, "s3");
        self.show_page("files:small.md", &small);
        let (_, held) = self.tables_frame()?;
        check(held > 0, format!("s3 showing s2's page held {held} rows"));
        self.show_page("files:small.md", &page_of_tables("changed", 1));
        let (_, held) = self.tables_frame()?;
        check(held > 0, format!("a reply's frame long after an arrival held {held} rows (a deferral outlived its frame)"));
        // A deferred build asks for the next frame. Other work of a frame marks the pacer too, so this one is asked of the
        // call alone.
        self.md_media = MarkdownMedia::default();
        self.pacer.clear_dirty();
        let (pane, scroll) = (self.shaped.rect(), self.shaped.scroll_px());
        let places = super::table_places(&self.shaped.md().media_blocks, &self.collect_media_paint_targets(pane, scroll));
        self.ensure_table_buffers(true, &places, pane);
        check(self.pacer.is_dirty() && self.held_rows() == 0, String::from("a deferred build did not ask for the next frame"));
        self.ensure_table_buffers(false, &places, pane);
        check(self.held_rows() > 0, String::from("the build after a deferral shaped no row"));
        // A document over the budget is never parked, so a cover must not cost it its set, nor the table scroll: closing a
        // full-height drawer draws its tables, scrolled as they were, in a frame that builds none.
        let huge = page_of_tables("huge", 10);
        self.show_page("files:huge.md", &huge);
        let (n, _, huge_bytes) = sizes(self);
        anyhow::ensure!(huge_bytes > crate::ui::preview::markdown::media_store::PARKED_TABLE_BYTES, "the page is within the budget: {huge_bytes} bytes");
        let (huge_build, huge_held, _, wrong) = self.view_frame(false)?;
        check(huge_held > 0 && wrong.is_empty(), format!("the page over the budget, {n} tables, held {huge_held} rows: {wrong:?}"));
        // The frame of the sideways scroll shapes rows beyond the view too; what the cover keeps is what it left.
        self.md_media.scroll_tables_right(300.0);
        let (_, huge_held) = self.tables_frame()?;
        let scrolled = self.md_media.table_scroll_px();
        check(scrolled > 0.0, String::from("the tables did not scroll sideways"));
        self.press_repl_key();
        self.press_repl_key();
        let (_, held) = self.tables_frame()?;
        check(self.preview_covered() && held == huge_held && self.md_media.table_scroll_px() == scrolled,
            format!("a covered page over the budget held {held} of its {huge_held} rows, scrolled {} of {scrolled}", self.md_media.table_scroll_px()));
        self.press_repl_key();
        let (uncover_huge, held) = self.tables_frame()?;
        println!("tables-frame huge_build_us={} bytes={huge_bytes} uncover_us={} held={held}", huge_build.as_micros(), uncover_huge.as_micros());
        check(!self.preview_covered() && held == huge_held && self.md_media.table_scroll_px() == scrolled,
            format!("an uncovered page over the budget held {held} of its {huge_held} rows, scrolled {} of {scrolled}", self.md_media.table_scroll_px()));
        check(uncover_huge * 2 < huge_build, format!("uncovering a page over the budget took {uncover_huge:?}, against {huge_build:?} to shape it"));
        // A page changed under the cover: the set held is not the page's, so the uncover frame draws none (a deferred frame
        // holds no set of another page) and the next frame shapes the page's own.
        self.press_repl_key();
        self.press_repl_key();
        self.tables_frame()?;
        self.show_page("files:other.md", &page_of_tables("other", 1));
        self.press_repl_key();
        let (_, held) = self.tables_frame()?;
        check(!self.preview_covered() && held == 0, format!("the uncover of a page changed under the cover held {held} rows of the old page"));
        let (_, then) = self.tables_frame()?;
        check(then > 0, format!("the frame after that uncover held {then} rows"));
        anyhow::ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }
}

impl State {
    /// The page's tables as the set is keyed: each one's rendered text and metrics.
    fn table_sources(&self) -> Vec<(String, f32, f32)> {
        self.shaped.md().media_blocks.iter().filter_map(|block| match block {
            crate::ui::preview::markdown::MediaBlock::Table { rendered, font_px, line_h_px, .. } => Some((rendered.clone(), *font_px, *line_h_px)),
            _ => None,
        }).collect()
    }

    /// Per table of the page shown, per row, whether the set held is the page's and has that row shaped.
    fn rows_shaped(&self) -> Vec<Vec<bool>> {
        let sources = self.table_sources();
        let held = self.md_media.tables_match(&sources);
        sources.iter().enumerate().map(|(t, (rendered, ..))| {
            let lines = self.md_media.tables().get(t).filter(|_| held).map(|entry| &entry.buffer.lines);
            (0..rendered.lines().count()).map(|row| lines.and_then(|lines| lines.get(row)).is_some_and(|line| line.layout_opt().is_some())).collect()
        }).collect()
    }

    /// The rows the set held for the page has shaped, in all.
    fn shaped_rows(&self) -> usize { self.rows_shaped().iter().flatten().filter(|shaped| **shaped).count() }

    /// Each table's place in the last frame's layout, in the page's order.
    fn table_views(&self) -> Vec<ScreenRect> {
        let blocks = &self.shaped.md().media_blocks;
        self.collect_media_paint_targets(self.shaped.rect(), self.shaped.scroll_px())
            .into_iter()
            .filter(|(block, _)| matches!(blocks.get(*block), Some(crate::ui::preview::markdown::MediaBlock::Table { .. })))
            .map(|(_, rect)| rect)
            .collect()
    }

    /// The rows of a table the preview's text area can hold whole, and a cell's height more above and below (the pane's
    /// padding around the text is half a cell): the most a frame may shape after a jump.
    fn pane_rows(&self, pitch: f32) -> usize { ((self.shaped.rect().h + 2.0 * self.canvas.cell_h()) / pitch) as usize + 2 }

    /// Whether every row of the set held from the pane's top to `px` below its foot is shaped: what a step of `px`
    /// down brings into view.
    fn rows_ahead_shaped(&self, px: f32) -> bool {
        let mut ahead = self.preview_pane();
        ahead.h += px;
        self.md_media.tables().iter().zip(self.table_views()).all(|(entry, place)| entry.rows_shaped(&entry.rows_in(place, ahead)))
    }

    /// The preview pane of the last frame: its text area with the half cell of padding above and below that
    /// `layout_markdown` takes off it.
    fn preview_pane(&self) -> ScreenRect {
        let (text, pad) = (self.shaped.rect(), 0.5 * self.canvas.cell_h());
        ScreenRect { y: text.y - pad, h: text.h + 2.0 * pad, ..text }
    }

    /// One frame of a page of tables: its length, the rows it shaped in view and beyond the view, and what is wrong: a
    /// row shaped in a frame that is `deferred`, a row in view that a frame not deferred left unshaped, any row shaped
    /// beyond the view in a frame that shaped a row in view, or none in a frame not deferred that shaped none in view
    /// and left rows. How many rows a frame shapes beyond the view is its time budget's, so it is printed, not asserted.
    fn view_frame(&mut self, deferred: bool) -> anyhow::Result<(std::time::Duration, usize, usize, Vec<String>)> {
        let before = self.rows_shaped();
        let begun = std::time::Instant::now();
        self.redraw()?;
        let took = begun.elapsed();
        let after = self.rows_shaped();
        let (views, pane, held) = (self.table_views(), self.preview_pane(), self.md_media.tables_match(&self.table_sources()));
        let mut wrong = Vec::new();
        if views.len() != after.len() {
            wrong.push(format!("{} places for {} tables", views.len(), after.len()));
        }
        let (mut inside, mut beyond, mut missing) = (0, 0, 0);
        for (t, (rows, place)) in after.iter().zip(&views).enumerate() {
            let shown = self.md_media.tables().get(t).filter(|_| held).map_or(0..0, |entry| entry.rows_in(*place, pane));
            for (row, now) in rows.iter().enumerate() {
                let new = usize::from(*now && !before.get(t).and_then(|rows| rows.get(row)).copied().unwrap_or(false));
                if shown.contains(&row) {
                    inside += new;
                    missing += usize::from(!deferred && !now);
                } else {
                    beyond += new;
                }
            }
        }
        let left = after.iter().flatten().any(|shaped| !shaped);
        for (n, what) in [
            (if deferred { inside + beyond } else { 0 }, "rows shaped in a deferred frame"),
            (missing, "rows in view and not shaped"),
            (if inside > 0 { beyond } else { 0 }, "rows shaped beyond the view by a frame that shaped rows in view"),
            (usize::from(!deferred && inside + beyond == 0 && left), "frame not deferred shaped no row with rows left"),
        ] {
            if n > 0 {
                wrong.push(format!("{n} {what}"));
            }
        }
        Ok((took, inside, beyond, wrong))
    }

    /// What reads a page's tables besides their draw, read after one frame (`deferred`: one that shapes none): the
    /// horizontal scroll's clamp, each table's place, the page's height, the hit test inside the first table, and the
    /// selection held across it: its text, its rectangles and the source lines it shares with the agent.
    fn table_readers(&mut self, deferred: bool) -> anyhow::Result<(f32, String)> {
        self.md_media.scroll_tables_right(1.0e6);
        if deferred {
            self.md_media.defer_next();
        }
        self.redraw()?;
        let md = self.shaped.md();
        let height = md.buffer.layout_runs().last().map_or(0.0, |run| run.line_top + run.line_height);
        let places = self.table_views();
        let first = *places.first().ok_or_else(|| anyhow::anyhow!("the page has no table"))?;
        let hit = self.shaped.hit((first.x + 4.0, first.y + first.h / 2.0), true);
        let read = format!("height={height} places={places:?} hit={hit:?} text={:?} rects={:?} shared={:?}",
            self.shaped.selected_text(), self.shaped.selection_rects(), self.shaped.released_selection());
        Ok((self.md_media.table_scroll_px(), read))
    }

    /// Per shaped row of the set held, the address of its shaping, which a second shaping of the row would replace.
    fn row_shapings(&self) -> Vec<usize> {
        self.md_media.tables().iter().flat_map(|entry| &entry.buffer.lines)
            .filter_map(|line| line.shape_opt().as_ref().map(|shape| shape.spans.as_ptr() as usize))
            .collect()
    }

    /// A page of five tables of 60 rows, each taller than the preview, shapes only the rows the preview shows on each
    /// path that once shaped every table, or every table in view, in one frame (a reply's frame, a text-scale step, the
    /// frame after a return with no set kept, the frame after an uncover when the page changed under the cover), and the
    /// frames after it shape rows beyond the view, nearest first, at least one a frame, until the set is whole; then
    /// nothing asks for a frame, and a wheel step, a page step and a jump to the end shape none. A frame that shapes rows
    /// in view shapes none beyond; a covered preview, a canvas over the page and a deferred frame shape none; a set left
    /// for another page waits and comes back as it was, and so does one left for the canvas. A pane made shorter and
    /// taller again shapes no row twice. What reads the tables besides the draw answers the same before and after they
    /// are shaped. The asserted facts are the rows each frame shaped against the rows in view in its layout; each
    /// frame's length is printed (`in-view-frame`, microseconds), and the one time bound is relative to this window's
    /// own cost of shaping every row of the page.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_page_of_tables_shapes_only_the_rows_in_view(&mut self) -> anyhow::Result<()> {
        let host = "<host-in-view>".to_string();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        let row = |slug: &str, default: bool| {
            let mut row = nav::ws_info(slug, &format!("{slug}-session"));
            row.workspace_id = format!("id-{slug}");
            row.is_default = default;
            if default {
                row.agent = "none".into();
            }
            row
        };
        self.on_workspaces(host.clone(), vec![row("default", true), row("v1", false), row("v2", false)]);
        let go = |s: &mut State, slug: &str| s.switch_to_workspace(host.clone(), Some(slug.into()), None, true);
        let page = page_of_rows("tall", 5, 60, 4);
        let mut failed = Vec::new();
        let mut check = |held: bool, words: String| if !held { failed.push(words) };
        let at_once = std::time::Duration::ZERO;
        // A reply into a row already shown with markdown: its own frame shapes the rows in view and none beyond.
        go(self, "v1");
        self.show_page("files:small.md", &page_of_tables("small", 1));
        self.redraw()?;
        self.redraw()?;
        self.show_page("files:tall.md", &page);
        let sources = self.table_sources();
        let pitch = sources.first().map_or(1.0, |(_, _, pitch)| *pitch);
        let (rows, tables) = (sources.iter().map(|(rendered, ..)| rendered.lines().count()).sum::<usize>(), sources.len());
        anyhow::ensure!(tables == 5 && rows == 5 * 63, "the page has {tables} tables of {rows} rows");
        let (reply, inside, beyond, wrong) = self.view_frame(false)?;
        println!("in-view-frame reply_us={} rows={inside} of={rows}", reply.as_micros());
        check(wrong.is_empty() && (1..63).contains(&inside) && beyond == 0, format!("the reply's frame shaped {inside} rows in view and {beyond} beyond: {wrong:?}"));
        // A page step straight after it lands on rows no frame has shaped: those in view are shaped, none beyond.
        let line = self.shaped.md().line_height().max(1.0);
        let page_lines = (self.shaped.rect().h / line) as i32;
        self.shaped.scroll_by(page_lines, std::time::Instant::now(), at_once);
        let (step, inside, beyond, wrong) = self.view_frame(false)?;
        println!("in-view-frame page_us={} in_view={inside} beyond={beyond}", step.as_micros());
        check(wrong.is_empty() && (1..=self.pane_rows(pitch)).contains(&inside) && beyond == 0, format!("a page step shaped {inside} in view and {beyond} beyond: {wrong:?}"));
        // While rows are left the frame asks for the next, asked of the call alone (other work of a frame marks it too).
        let places = |s: &State| super::table_places(&s.shaped.md().media_blocks, &s.collect_media_paint_targets(s.shaped.rect(), s.shaped.scroll_px()));
        self.pacer.clear_dirty();
        let pane = self.preview_pane();
        self.ensure_table_buffers(false, &places(self), pane);
        check(self.pacer.is_dirty(), String::from("a set with rows left did not ask for the next frame"));
        // The frames after shape rows beyond the view, nearest first, at least one a frame, however many their time
        // allows; once they have shaped the rows a wheel step brings into view, the step shapes none in view.
        let mut n = 0;
        while n < 3 || (!self.rows_ahead_shaped(3.0 * line + pitch) && n < rows) {
            let (took, inside, beyond, wrong) = self.view_frame(false)?;
            println!("in-view-frame background_us={} rows={beyond}", took.as_micros());
            check(wrong.is_empty() && inside == 0 && beyond > 0, format!("background frame {n} shaped {inside} in view and {beyond} beyond: {wrong:?}"));
            n += 1;
        }
        self.shaped.scroll_by(3, std::time::Instant::now(), at_once);
        let (wheel, inside, beyond, wrong) = self.view_frame(false)?;
        println!("in-view-frame wheel_us={} in_view={inside} beyond={beyond}", wheel.as_micros());
        check(wrong.is_empty() && inside == 0 && beyond > 0, format!("a wheel step onto rows shaped beyond the view shaped {inside} in view and {beyond} beyond: {wrong:?}"));
        // A covered preview shapes none; the uncover is deferred and shapes none; the frame after goes on.
        self.press_repl_key();
        self.press_repl_key();
        for _ in 0..3 {
            let (_, inside, beyond, _) = self.view_frame(true)?;
            check(self.preview_covered() && inside + beyond == 0, format!("a covered preview shaped {}", inside + beyond));
        }
        self.press_repl_key();
        let (_, inside, beyond, wrong) = self.view_frame(true)?;
        check(!self.preview_covered() && wrong.is_empty() && inside + beyond == 0, format!("the uncover shaped {}: {wrong:?}", inside + beyond));
        let (_, inside, beyond, wrong) = self.view_frame(false)?;
        check(wrong.is_empty() && inside == 0 && beyond > 0, format!("after the uncover the set did not go on: {inside} in view, {beyond} beyond: {wrong:?}"));
        // The canvas over the page shows no markdown and shapes none; taken off, the set comes back as it was and goes on.
        let (had, shapings) = (self.rows_shaped(), self.row_shapings());
        self.ink.set_canvas(true);
        for _ in 0..3 {
            self.redraw()?;
            check(self.held_rows() == 0, format!("the canvas over the page left {} rows held", self.held_rows()));
        }
        self.ink.set_canvas(false);
        self.view_frame(true)?;
        check(self.rows_shaped() == had && self.row_shapings() == shapings, String::from("the set did not come back as it was after the canvas"));
        let (_, inside, beyond, wrong) = self.view_frame(false)?;
        check(wrong.is_empty() && inside == 0 && beyond > 0, format!("after the canvas the set did not go on: {inside} in view, {beyond} beyond: {wrong:?}"));
        // Another row's page: the set left waits, and comes back with its rows and their shapings and goes on.
        let (had, shapings) = (self.rows_shaped(), self.row_shapings());
        go(self, "v2");
        self.show_page("files:small.md", &page_of_tables("small", 1));
        for _ in 0..6 {
            self.redraw()?;
        }
        go(self, "v1");
        self.view_frame(true)?;
        check(self.rows_shaped() == had && self.row_shapings() == shapings, String::from("the set left for another row did not come back as it was"));
        // The set becomes whole within as many frames as it has rows left, since every frame shapes at least one; then
        // nothing asks for a frame.
        let most = rows - self.shaped_rows();
        let mut frames = 0;
        while self.shaped_rows() < rows && frames <= most {
            let (_, inside, beyond, wrong) = self.view_frame(false)?;
            check(wrong.is_empty() && inside == 0 && beyond > 0, format!("frame {frames} toward the whole set shaped {inside} in view and {beyond} beyond: {wrong:?}"));
            frames += 1;
        }
        println!("in-view-frame whole_after_frames={frames} most={most}");
        check(self.shaped_rows() == rows && frames <= most, format!("{} of {rows} rows shaped after {frames} frames, at most {most}", self.shaped_rows()));
        self.pacer.clear_dirty();
        let pane = self.preview_pane();
        self.ensure_table_buffers(false, &places(self), pane);
        check(!self.pacer.is_dirty() && !self.held_unshaped(), String::from("a whole set still asked for a frame"));
        // A whole set: a wheel step, a page step and a jump to the end shape nothing.
        let steps: [(&str, Box<dyn Fn(&mut State)>); 3] = [
            ("wheel", Box::new(move |s: &mut State| s.shaped.scroll_by(3, std::time::Instant::now(), at_once))),
            ("page", Box::new(move |s: &mut State| s.shaped.scroll_by(page_lines, std::time::Instant::now(), at_once))),
            ("end", Box::new(move |s: &mut State| s.shaped.scroll_to_end(std::time::Instant::now(), at_once))),
        ];
        for (name, step) in steps {
            step(self);
            let (took, inside, beyond, wrong) = self.view_frame(false)?;
            println!("in-view-frame whole_{name}_us={} rows={}", took.as_micros(), inside + beyond);
            check(wrong.is_empty() && inside + beyond == 0, format!("the {name} step over a whole set shaped {}: {wrong:?}", inside + beyond));
        }
        // A text-scale step shapes the rows in view at the new size and none beyond. In that new set, a pane made
        // shorter, scrolled, and made taller again shapes no row twice. The step back takes the kept set back whole,
        // its shapings kept.
        self.shaped.scroll_to_top(std::time::Instant::now(), at_once);
        self.redraw()?;
        let (had, kept) = (self.rows_shaped(), self.row_shapings());
        let mult = self.canvas.text_scale_mult();
        self.apply_text_scale(mult + 0.1);
        let (zoom, inside, beyond, wrong) = self.view_frame(false)?;
        check(wrong.is_empty() && (1..63).contains(&inside) && beyond == 0, format!("the zoom frame shaped {inside} in view and {beyond} beyond: {wrong:?}"));
        let (tall, line) = (self.shaped.rect().h, self.shaped.md().line_height().max(1.0));
        self.press_repl_key();
        self.view_frame(false)?;
        let short = self.shaped.rect().h;
        self.shaped.scroll_by((short / line) as i32, std::time::Instant::now(), at_once);
        self.view_frame(false)?;
        let shapings = self.row_shapings();
        self.press_repl_key();
        self.press_repl_key();
        let (taller, _, _, wrong) = self.view_frame(false)?;
        let again = shapings.iter().filter(|shaping| !self.row_shapings().contains(shaping)).count();
        println!("in-view-frame taller_us={} again={again}", taller.as_micros());
        check(short < tall && self.shaped.rect().h == tall && wrong.is_empty() && again == 0 && self.held_unshaped(),
            format!("a pane {tall} px made {short} px and taller again lost {again} rows' shaping: {wrong:?}"));
        self.apply_text_scale(mult);
        let (back, _, _, _) = self.view_frame(false)?;
        println!("in-view-frame zoom_us={} rows={inside} zoom_back_us={}", zoom.as_micros(), back.as_micros());
        check(self.rows_shaped() == had && self.row_shapings() == kept,
            format!("the step back did not take back the set as it was: {} rows for {}", self.shaped_rows(), kept.len()));
        // A return with no set kept: the arrival shapes none, the next frame the rows in view and none beyond.
        go(self, "v2");
        self.redraw()?;
        self.redraw()?;
        self.md_media = MarkdownMedia::default();
        go(self, "v1");
        let (_, inside, beyond, wrong) = self.view_frame(true)?;
        check(wrong.is_empty() && inside + beyond == 0, format!("a return with no set kept shaped {} rows in its first frame: {wrong:?}", inside + beyond));
        let (cold, inside, beyond, wrong) = self.view_frame(false)?;
        println!("in-view-frame return_cold_next_us={} rows={inside}", cold.as_micros());
        check(wrong.is_empty() && (1..=self.pane_rows(pitch)).contains(&inside) && beyond == 0, format!("the frame after a cold return shaped {inside} in view and {beyond} beyond: {wrong:?}"));
        // The page rewritten under a full-height drawer: the uncover shapes none, the next frame the rows in view.
        self.press_repl_key();
        self.press_repl_key();
        self.redraw()?;
        self.show_page("files:tall.md", &page_of_rows("tolt", 5, 60, 4));
        let (_, inside, beyond, _) = self.view_frame(true)?;
        check(self.preview_covered() && inside + beyond == 0, format!("a covered preview shaped {} rows", inside + beyond));
        self.press_repl_key();
        let (_, inside, beyond, wrong) = self.view_frame(true)?;
        check(!self.preview_covered() && wrong.is_empty() && inside + beyond == 0, format!("the uncover shaped {}: {wrong:?}", inside + beyond));
        let (uncovered, inside, beyond, wrong) = self.view_frame(false)?;
        println!("in-view-frame uncover_next_us={} rows={inside}", uncovered.as_micros());
        check(wrong.is_empty() && (1..63).contains(&inside) && beyond == 0, format!("the frame after the uncover shaped {inside} in view and {beyond} beyond: {wrong:?}"));
        // What reads the tables answers the same before they are shaped and after, with a selection from the line above
        // a small first table to the line below it, the tall tables after.
        self.md_media = MarkdownMedia::default();
        self.show_page("files:read.md", &format!("Above.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nBelow.\n\n{page}"));
        self.md_media.defer_next();
        self.redraw()?;
        let (text, line) = (self.shaped.rect(), self.shaped.md().line_height().max(1.0));
        let first = *self.table_views().first().ok_or_else(|| anyhow::anyhow!("the page has no table"))?;
        let from = self.shaped.hit((text.x + 2.0, text.y + crate::ui::render::text::EXTRA_TOP_PAD_PX + 2.0), true);
        self.shaped.press(from);
        self.shaped.extend_drag((text.x + 40.0, first.y + first.h + 3.0 * line));
        self.shaped.release();
        let (clamp_before, before) = self.table_readers(true)?;
        check(self.shaped_rows() == 0, String::from("a deferred frame shaped rows"));
        self.redraw()?;
        let (clamp_after, after) = self.table_readers(false)?;
        println!("in-view-frame clamp_before={clamp_before} clamp_after={clamp_after}");
        check(self.shaped.selected_text().is_some_and(|text| text.contains("Above.") && text.contains("Below")),
            format!("the selection does not cross the first table: {:?}", self.shaped.selected_text()));
        check(before == after && (clamp_before - clamp_after).abs() < 0.5 && clamp_after > 0.0,
            format!("before the tables were shaped: clamp {clamp_before} {before}; after: clamp {clamp_after} {after}"));
        // The one time bound: the reply's frame against shaping every row of the page in this window.
        let begun = std::time::Instant::now();
        let whole: Vec<_> = sources.into_iter().map(|(rendered, font_px, line_h_px)| self.canvas.shape(|fs| super::table_buffer(fs, rendered, font_px, line_h_px))).collect();
        let all = begun.elapsed();
        println!("in-view-frame shape_all_us={} tables={}", all.as_micros(), whole.len());
        check(reply * 2 < all, format!("the reply's frame took {reply:?}, against {all:?} to shape every row"));
        anyhow::ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }
}
