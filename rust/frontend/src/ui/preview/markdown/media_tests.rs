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
    /// A page of 50 inline equations is drawn by the worker, all in view: every one has its raster, and the page's cost
    /// from the first frame to the last raster is printed (`math-page`). The equations are MathJax-shaped stand-ins.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_page_of_math_is_drawn_by_the_worker_in_one_batch(&mut self) -> anyhow::Result<()> {
        self.redraw()?;
        const EQUATIONS: usize = 50;
        let source: String = (0..EQUATIONS)
            .map(|n| format!("term ${}$ of the series{}", format_args!("a_{{{n}}}"), if n % 10 == 9 { "\n\n" } else { " and " }))
            .collect();
        self.preview_source.install("text/markdown".into(), source.clone().into_bytes(), Some("files:m.md".into()), None, None);
        self.render_preview_source("text/markdown", source.as_bytes());
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
        println!("math-page equations={EQUATIONS} ms={}", begun.elapsed().as_millis());
        Ok(())
    }
}
