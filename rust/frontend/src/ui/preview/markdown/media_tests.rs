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
fn page_of_tables(tag: &str, tables: usize) -> String {
    let mut page = String::new();
    for t in 0..tables {
        page.push_str(&format!("{tag} table {t}.\n\n"));
        for r in 0..12 {
            let cells: Vec<String> = (0..8).map(|c| format!("{tag}t{t}r{r}c{c} ").repeat(11).trim().to_owned()).collect();
            page.push_str(&format!("| {} |\n", cells.join(" | ")));
            if r == 0 {
                page.push_str(&format!("|{}\n", "---|".repeat(8)));
            }
        }
        page.push('\n');
    }
    page
}

impl State {
    /// One frame, its length, and the table buffers it left.
    fn tables_frame(&mut self) -> anyhow::Result<(std::time::Duration, usize)> {
        let begun = std::time::Instant::now();
        self.redraw()?;
        Ok((begun.elapsed(), self.md_media.tables().len()))
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

    /// A row whose preview holds seven wide tables (about 100,000 characters of table text) shows them at once when
    /// the person returns to it, and shows its panes first when it arrives, or its preview is uncovered, with no set of
    /// its tables kept: that frame shapes no table, asks for the next, and the next shapes all seven. A covered preview
    /// builds none, and a return from under the cover takes the set back. A deferred build marks the pacer dirty and
    /// holds no set of another page. A page over the budget keeps its tables and their scroll through a cover; a page
    /// changed under the cover draws none at the uncover and all at the next frame. A reply's frame long after an
    /// arrival still shapes in its own frame (the deferral is spent by the frame that took it). The asserted facts are the tables a frame leaves; each frame's length is printed (`tables-frame`,
    /// microseconds). The one time bound is relative, to this window's own cost of shaping the seven in the frame after
    /// the first arrival, so a slow runner moves both alike; the held counts carry the proof.
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
        // The first visit: s1 arrives, the arrival frame shapes nothing, the next shapes all seven.
        go(self, "s1");
        self.show_page("files:big.md", &big);
        let (tables, chars, bytes) = sizes(self);
        anyhow::ensure!((90_000..115_000).contains(&chars) && tables == 7, "the page's tables: {tables} of {chars} characters");
        let (arrival, held) = self.tables_frame()?;
        let (build, built) = self.tables_frame()?;
        println!("tables-frame first_arrival_us={} held={held} next_us={} held={built} chars={chars} bytes={bytes}", arrival.as_micros(), build.as_micros());
        check(held == 0 && built == 7, format!("a first arrival held {held} tables in its first frame and {built} in its next"));
        // s2 with a small page, then the return to s1: all seven in the first frame.
        go(self, "s2");
        self.show_page("files:small.md", &small);
        let (_, stale) = self.tables_frame()?;
        check(stale == 0, format!("the deferred frame of s2 held {stale} tables of another page"));
        let (_, small_held) = self.tables_frame()?;
        check(small_held == 1, String::from("s2's table was not shaped by its second frame"));
        go(self, "s1");
        let (back, held) = self.tables_frame()?;
        println!("tables-frame return_us={} held={held}", back.as_micros());
        check(held == 7, format!("a return held {held} tables in its first frame, not 7"));
        check(back * 2 < build, format!("a return's first frame took {back:?}, against {build:?} to shape the tables"));
        // An arrival with no set kept: the panes first, the tables a frame later.
        go(self, "s2");
        self.tables_frame()?;
        self.md_media = MarkdownMedia::default();
        go(self, "s1");
        let (cold, held) = self.tables_frame()?;
        let (_, then) = self.tables_frame()?;
        println!("tables-frame cold_arrival_us={} held={held} next_held={then}", cold.as_micros());
        check(held == 0 && then == 7, format!("a cold arrival held {held} in its first frame and {then} in its next"));
        check(cold * 2 < build, format!("a cold arrival's first frame took {cold:?}, against {build:?} to shape the tables"));
        // A covered preview: a full-height drawer builds nothing and keeps the set it holds, so uncovering draws it at once.
        self.press_repl_key();
        self.press_repl_key();
        let (_, held) = self.tables_frame()?;
        check(self.preview_covered() && held == 7, format!("a covered preview held {held} tables, not the 7 it had (covered: {})", self.preview_covered()));
        self.press_repl_key();
        let (back_uncovered, held) = self.tables_frame()?;
        println!("tables-frame uncover_kept_us={} held={held}", back_uncovered.as_micros());
        check(!self.preview_covered() && held == 7, format!("the preview uncovered with its set kept held {held} tables"));
        // With no set kept, the frame that uncovers shapes none, asks for the next, and the next shapes all seven.
        self.press_repl_key();
        self.press_repl_key();
        self.md_media = MarkdownMedia::default();
        for _ in 0..3 {
            let (_, held) = self.tables_frame()?;
            check(held == 0, format!("a covered preview shaped {held} tables"));
        }
        self.press_repl_key();
        let (uncover, held) = self.tables_frame()?;
        check(!self.preview_covered() && held == 0, format!("the frame that uncovered the preview held {held} tables"));
        let (shaped, then) = self.tables_frame()?;
        println!("tables-frame uncover_cold_us={} held={held} next_us={} next_held={then}", uncover.as_micros(), shaped.as_micros());
        check(then == 7, format!("the frame after the uncover held {then} tables, not 7"));
        // A deferral is spent by the frame that takes it: s3 shows the very tables s2 holds, so nothing is shaped and
        // the deferral its arrival set is taken by a frame that has no use for it; a reply that then changes the
        // table is shaped in its own frame.
        go(self, "s2");
        self.tables_frame()?;
        let (_, held) = self.tables_frame()?;
        check(held == 1, format!("s2's table was not held after its second frame ({held})"));
        go(self, "s3");
        self.show_page("files:small.md", &small);
        let (_, held) = self.tables_frame()?;
        check(held == 1, format!("s3 showing s2's page held {held} tables, not 1"));
        self.show_page("files:small.md", &page_of_tables("changed", 1));
        let (_, held) = self.tables_frame()?;
        check(held == 1, format!("a reply's frame long after an arrival held {held} tables, not 1 (a deferral outlived its frame)"));
        // A deferred build asks for the next frame. Other work of a frame marks the pacer too, so this one is asked of the
        // call alone.
        self.md_media = MarkdownMedia::default();
        self.pacer.clear_dirty();
        self.ensure_table_buffers(true);
        check(self.pacer.is_dirty() && self.md_media.tables().is_empty(), String::from("a deferred build did not ask for the next frame"));
        self.ensure_table_buffers(false);
        check(self.md_media.tables().len() == 1, String::from("the build after a deferral shaped no table"));
        // A document over the budget is never parked, so a cover must not cost it its set, nor the table scroll: closing a
        // full-height drawer draws its tables, scrolled as they were, in a frame that builds none.
        let huge = page_of_tables("huge", 10);
        self.show_page("files:huge.md", &huge);
        let (n, _, huge_bytes) = sizes(self);
        anyhow::ensure!(huge_bytes > crate::ui::preview::markdown::media_store::PARKED_TABLE_BYTES, "the page is within the budget: {huge_bytes} bytes");
        let (huge_build, held) = self.tables_frame()?;
        check(held == n, format!("the page over the budget held {held} of {n} tables"));
        self.md_media.scroll_tables_right(300.0);
        self.tables_frame()?;
        let scrolled = self.md_media.table_scroll_px();
        check(scrolled > 0.0, String::from("the tables did not scroll sideways"));
        self.press_repl_key();
        self.press_repl_key();
        let (_, held) = self.tables_frame()?;
        check(self.preview_covered() && held == n && self.md_media.table_scroll_px() == scrolled,
            format!("a covered page over the budget held {held} of {n} tables, scrolled {} of {scrolled}", self.md_media.table_scroll_px()));
        self.press_repl_key();
        let (uncover_huge, held) = self.tables_frame()?;
        println!("tables-frame huge_build_us={} bytes={huge_bytes} uncover_us={} held={held}", huge_build.as_micros(), uncover_huge.as_micros());
        check(!self.preview_covered() && held == n && self.md_media.table_scroll_px() == scrolled,
            format!("an uncovered page over the budget held {held} of {n} tables, scrolled {} of {scrolled}", self.md_media.table_scroll_px()));
        check(uncover_huge * 2 < huge_build, format!("uncovering a page over the budget took {uncover_huge:?}, against {huge_build:?} to shape it"));
        // A page changed under the cover: the set held is not the page's, so the uncover frame draws none (a deferred frame
        // holds no set of another page) and the next frame shapes the page's own.
        self.press_repl_key();
        self.press_repl_key();
        self.tables_frame()?;
        self.show_page("files:other.md", &page_of_tables("other", 1));
        self.press_repl_key();
        let (_, held) = self.tables_frame()?;
        check(!self.preview_covered() && held == 0, format!("the uncover of a page changed under the cover held {held} tables of the old page"));
        let (_, then) = self.tables_frame()?;
        check(then == 1, format!("the frame after that uncover held {then} tables, not 1"));
        anyhow::ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }
}
