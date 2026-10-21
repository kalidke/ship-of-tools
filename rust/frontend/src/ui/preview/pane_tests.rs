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
