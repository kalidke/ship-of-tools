use super::*;

#[test]
fn raster_preview_mimes_route_jpeg_and_kin_not_svg() {
    // jpeg is the asked-for case; gif/webp/bmp shared the same latent gap
    // (the router used to accept only image/png). All must decode as rasters.
    for m in [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
    ] {
        assert!(is_raster_preview_mime(m), "{m} should decode as a raster");
    }
    // SVG is vector — it has its own preview path and must NOT be fed to the
    // raster decoder; non-image mimes must not route here either.
    assert!(!is_raster_preview_mime("image/svg+xml"));
    assert!(!is_raster_preview_mime("text/plain; charset=utf-8"));
    assert!(!is_raster_preview_mime("application/json"));
}

#[test]
fn a_preview_is_routed_by_its_mime_an_svg_to_the_picture_not_the_raster_decoder_or_the_text() {
    assert_eq!(preview_route("image/svg+xml"), PreviewRoute::Svg);
    for m in ["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp"] {
        assert_eq!(preview_route(m), PreviewRoute::Raster, "{m}");
    }
    for m in ["text/plain; charset=utf-8", "text/html", "application/json", "text/markdown"] {
        assert_eq!(preview_route(m), PreviewRoute::Other, "{m}");
    }
}

// --- Bug 2 / round 2 provenance fix: `o`/`W`/`O` must route against
// the node the INSTALLED preview reply answered — never the most
// recently REQUESTED one (which can outrun its own reply), and never
// a pin either (a pin is stamped from the cursor row,
// not from an installed reply, so it can equally outrun what's
// shown). ---

#[test]
fn previewed_path_routes_by_installed_node_not_a_fired_one() {
    // The function doesn't even take a "fired" id (or a "pinned" one) — its signature is the proof that neither can
    // leak into the routed path.
    assert_eq!(
        resolve_previewed_path(Some("files:a/index.html"), Some("/proj")),
        Some("/proj/a/index.html".to_string())
    );
}

#[test]
fn previewed_path_none_when_nothing_resolves_to_a_files_node() {
    assert_eq!(resolve_previewed_path(None, Some("/proj")), None);
    assert_eq!(
        resolve_previewed_path(Some("modules:Foo"), Some("/proj")),
        None
    );
}

#[test]
fn looks_binary_detects_nul_not_text() {
    assert!(!looks_binary(b"plain text source\nfn main() {}\n"));
    assert!(!looks_binary(b""));
    // HDF5-ish: signature has a NUL very early.
    assert!(looks_binary(b"\x89HDF\r\n\x1a\n\x00\x00\x00"));
    assert!(looks_binary(&[b'a', b'b', 0u8, b'c']));
    // NUL beyond the 8 KiB window isn't scanned (treated as text).
    let mut late = vec![b'x'; 9000];
    late.push(0);
    assert!(!looks_binary(&late));
}

#[test]
fn files_node_id_under_root_translates_inside_paths() {
    assert_eq!(
        files_node_id_under_root("/a/ws/src/x.jl", "/a/ws"),
        Some("files:src/x.jl".to_string())
    );
    // Trailing slash on the root is tolerated.
    assert_eq!(
        files_node_id_under_root("/a/ws/x.jl", "/a/ws/"),
        Some("files:x.jl".to_string())
    );
    // Root-level file.
    assert_eq!(
        files_node_id_under_root("/a/ws/top.md", "/a/ws"),
        Some("files:top.md".to_string())
    );
}

#[test]
fn files_node_id_under_root_rejects_outside_and_lookalikes() {
    // Outside the root entirely.
    assert_eq!(files_node_id_under_root("/other/z.jl", "/a/ws"), None);
    // Sibling whose name extends the root's last segment — the boundary
    // '/' requirement must reject it (`/a/ws` vs `/a/wsx`).
    assert_eq!(files_node_id_under_root("/a/wsx/y.jl", "/a/ws"), None);
    // The root itself is not a files node.
    assert_eq!(files_node_id_under_root("/a/ws", "/a/ws"), None);
    assert_eq!(files_node_id_under_root("/a/ws/", "/a/ws"), None);
}

#[test]
fn files_node_id_under_root_accepts_windows_backslash_paths() {
    // Not `#[cfg(windows)]` — this is pure string manipulation (no
    // `Path`/`Component`), so it's exactly as correct run on Linux CI as
    // on a real Windows box, and proving that here is the whole point:
    // a native Windows `notify` event path is `\`-separated, and the
    // derived node id must still come out unix-style on the wire.
    assert_eq!(
        files_node_id_under_root(r"C:\a\b\file.jl", r"C:\a\b"),
        Some("files:file.jl".to_string())
    );
    assert_eq!(
        files_node_id_under_root(r"C:\a\b\sub\file.jl", r"C:\a\b"),
        Some("files:sub/file.jl".to_string())
    );
    // Trailing backslash on the root is tolerated, same as `/`.
    assert_eq!(
        files_node_id_under_root(r"C:\a\b\file.jl", r"C:\a\b\"),
        Some("files:file.jl".to_string())
    );
    // The lookalike-sibling rejection holds with backslashes too.
    assert_eq!(
        files_node_id_under_root(r"C:\a\bx\file.jl", r"C:\a\b"),
        None
    );
    // The root itself is not a files node.
    assert_eq!(files_node_id_under_root(r"C:\a\b", r"C:\a\b"), None);
}

#[test]
fn preview_changed_tag_match_uses_carried_node_id() {
    // Active workspace's own event: carried id is trusted verbatim,
    // no root knowledge needed (the workspace.list-lag case).
    assert_eq!(
        resolve_preview_changed(
            Some("alpha"),
            Some("files:fig/out.png"),
            Some("/a/alpha/fig/out.png"),
            Some("alpha"),
            None,
        ),
        Some("files:fig/out.png".to_string())
    );
    // Empty rel ("files:") and non-files ids are never trusted.
    assert_eq!(
        resolve_preview_changed(Some("alpha"), Some("files:"), None, Some("alpha"), None),
        None
    );
    assert_eq!(
        resolve_preview_changed(Some("alpha"), Some("modules:X"), None, Some("alpha"), None),
        None
    );
}

#[test]
fn preview_changed_foreign_ws_translates_by_path_under_known_root() {
    // Overlap case: tagged with another workspace, but the path lies
    // under OUR root — re-addressed into our id space.
    assert_eq!(
        resolve_preview_changed(
            Some("umbrella"),
            Some("files:pais/fig/out.png"),
            Some("/a/alpha/fig/out.png"),
            Some("alpha"),
            Some("/a/alpha"),
        ),
        Some("files:fig/out.png".to_string())
    );
}

#[test]
fn preview_changed_foreign_ws_without_known_root_is_dropped() {
    // Regression (2026-08-17): with the active root UNKNOWN, a foreign
    // event must be dropped — the old daemon-root fallback translated
    // it into the active tree's id space and fired phantom refreshes.
    assert_eq!(
        resolve_preview_changed(
            Some("sot"),
            Some("files:nav-live-A.txt"),
            Some("/a/sot/nav-live-A.txt"),
            Some("alpha"),
            None,
        ),
        None
    );
}

#[test]
fn preview_changed_default_ws_normalization() {
    // Active None normalizes to the default slug at the call site; the
    // resolver itself just compares — verify the default-vs-default
    // shape trusts the carried id.
    assert_eq!(
        resolve_preview_changed(
            Some("sot"),
            Some("files:README.md"),
            Some("/a/sot/README.md"),
            Some("sot"),
            Some("/a/sot"),
        ),
        Some("files:README.md".to_string())
    );
}

#[test]
fn preview_max_scroll_reaches_the_end_of_the_edit_buffer() {
    let lines = |n: usize| {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut fonts = cosmic_text::FontSystem::new();
    let md = MarkdownPreview::new_plain(&mut fonts, &lines(5), 800.0, 1.0);
    let edit = MarkdownPreview::new_plain(&mut fonts, &lines(50), 800.0, 1.0);
    let h = md.line_height();
    let vis = 10.0 * h;
    assert_eq!(preview_max_scroll(h, vis, &md), 0);
    let s = preview_max_scroll(h, vis, &edit);
    assert!(s as f32 * h >= edit.total_visual_pixels(h) - vis - 0.5);
    assert!((s as f32 * h) < edit.total_visual_pixels(h) - vis + h);
}

#[test]
fn preview_scroll_target_measures_only_the_buffer_on_screen() {
    let lines = |n: usize| {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut fonts = cosmic_text::FontSystem::new();
    let md = MarkdownPreview::new_plain(&mut fonts, &lines(100), 800.0, 1.0);
    let edit = MarkdownPreview::new_plain(&mut fonts, &lines(20), 800.0, 1.0);
    let h = md.line_height();
    let (b, bh) = preview_scroll_target(true, &md, 10.0 * h, Some(&edit), 12.0 * h);
    assert!(std::ptr::eq(b, &edit));
    assert_eq!(bh, 12.0 * h);
    let (b2, bh2) = preview_scroll_target(false, &md, 10.0 * h, Some(&edit), 12.0 * h);
    assert!(std::ptr::eq(b2, &md));
    assert_eq!(bh2, 10.0 * h);
    let (b3, bh3) = preview_scroll_target(true, &md, 10.0 * h, None, 12.0 * h);
    assert!(std::ptr::eq(b3, &md));
    assert_eq!(bh3, 10.0 * h);
    let vis = bh - crate::ui::render::text::EXTRA_TOP_PAD_PX;
    let s = preview_max_scroll(h, vis, b);
    assert!((s as f32) * h < edit.total_visual_pixels(h) - vis + h);
}

#[test]
fn a_refused_preview_says_so_in_the_pane_title() {
    assert_eq!(pane_name("a.pdf", Some((2, 5)), false), "a.pdf · p 2/5 · n/p");
    assert_eq!(pane_name("a.pdf", None, true), "a.pdf · REFUSED, view not updated");
}

impl State {
    /// An SVG file's preview is drawn through the real child at the size of the pane as laid out (a layout runs
    /// first): its text shows until the picture is installed by the frame's step, filling the pane on its limiting
    /// side at the SVG's aspect; the same file again is installed from the kept outcome at once; a text-size change
    /// leaves the picture up; a file that is not UTF-8 shows one line saying so and no picture; one that will not
    /// parse leaves no raster, shows its text and says why.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn an_svg_preview_is_a_raster_and_a_broken_one_is_its_text(&mut self) -> anyhow::Result<()> {
        self.draw_chrome()?;
        let (Some(pw), Some(ph)) = self.preview_fit_px() else { anyhow::bail!("no pane after a layout") };
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100"/></svg>"#;
        self.raster.hide();
        self.show_svg_source(svg);
        anyhow::ensure!(!self.raster.is_shown(), "the picture was installed before its drawing settled");
        self.frames_until(|state| state.raster.is_shown(), "an SVG preview installed no raster")?;
        let (w, h) = self.raster.quad_size().ok_or_else(|| anyhow::anyhow!("an SVG preview installed no raster"))?;
        let near = |a: u32, b: u32| a.abs_diff(b) <= 1;
        anyhow::ensure!(near(w, 2 * h), "the raster {w}x{h} lost the SVG's 2:1 aspect");
        anyhow::ensure!(near(w, pw.min(2 * ph)), "the raster {w}x{h} does not fill the {pw}x{ph} pane on its limiting side");
        // A text-size change replays the same source: the picture stays up, at the same size, and no text takes its place.
        let mult = self.canvas.text_scale_mult();
        self.apply_text_scale(mult + 0.1);
        anyhow::ensure!(self.preview_fit_px() == (Some(pw), Some(ph)), "the pane's size changed before its layout did");
        anyhow::ensure!(self.raster.quad_size() == Some((w, h)), "a text-size change took the picture down");
        self.draw_chrome()?;
        self.render_preview_source("image/svg+xml", svg);
        anyhow::ensure!(self.raster.is_shown(), "a text-size change and its layout took the picture down");
        self.apply_text_scale(mult);
        self.draw_chrome()?;
        self.raster.hide();
        self.show_svg_source(svg);
        anyhow::ensure!(self.raster.is_shown(), "the same file again was not installed from its kept outcome");
        // A file that is not UTF-8, of each kind of route, right after a drawn picture over an earlier file's words: the
        // picture and the words both go, and one line says what it is.
        for mime in ["text/plain", "text/markdown", "image/svg+xml"] {
            self.show_text_source("the previous file's words");
            self.show_svg_source(svg);
            anyhow::ensure!(self.raster.is_shown(), "the picture was not up before a {mime} file");
            self.preview_source.install(mime.into(), b"caf\xe9".to_vec(), Some("files:a.bin".into()), None, None);
            self.render_preview_source(mime, b"caf\xe9");
            anyhow::ensure!(!self.raster.is_shown(), "a {mime} file that is not UTF-8 kept the last raster");
            let text = self.shown_text();
            anyhow::ensure!(text.contains("not UTF-8"), "a {mime} file that is not UTF-8 shows `{text}`");
            anyhow::ensure!(!text.contains("previous file"), "a {mime} file that is not UTF-8 left the earlier text");
        }
        self.status.set(String::new());
        self.show_svg_source(svg);
        self.frames_until(|state| state.raster.is_shown(), "the SVG drew no picture again")?;
        self.show_svg_source(b"<svg><not closed");
        anyhow::ensure!(!self.raster.is_shown(), "a broken SVG kept the last raster");
        self.frames_until(|state| state.status.text().starts_with("svg not drawn: "), "the status line never said why")?;
        anyhow::ensure!(!self.raster.is_shown(), "a broken SVG drew a raster");
        anyhow::ensure!(self.status.text().contains("svg parse failed"), "the status line says `{}`", self.status.text());
        Ok(())
    }

    /// `bytes` installed as a new preview source and rendered, as a preview reply does.
    fn show_svg_source(&mut self, bytes: &[u8]) {
        self.preview_source.install("image/svg+xml".into(), bytes.to_vec(), Some("files:a.svg".into()), None, None);
        self.render_preview_source("image/svg+xml", bytes);
    }

    /// The pane's flowed text.
    fn shown_text(&self) -> String {
        self.shaped.md().buffer.lines.iter().map(|line| line.text()).collect::<Vec<_>>().join("\n")
    }

    fn show_text_source(&mut self, text: &str) {
        self.preview_source.install("text/plain".into(), text.as_bytes().to_vec(), Some("files:a.txt".into()), None, None);
        self.render_preview_source("text/plain", text.as_bytes());
    }

    /// The window's frame steps until `done`, which the worker's outcome brings.
    fn frames_until(&mut self, done: impl Fn(&State) -> bool, what: &str) -> anyhow::Result<()> {
        let begun = std::time::Instant::now();
        while !done(self) {
            anyhow::ensure!(begun.elapsed() < std::time::Duration::from_secs(30), "{what}");
            self.frame_upkeep();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(())
    }
}
