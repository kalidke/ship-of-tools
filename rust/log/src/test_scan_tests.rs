use super::*;

#[test]
fn without_test_modules_keeps_code_below_a_mid_file_test_module() {
    let text = "fn a() {}\n// a comment\n#[cfg(test)]\nmod t {\n    fn t() {}\n}\nfn after() {}\nfn last() {}\n";
    let view = without_test_modules(text);
    assert_eq!(view.lines().count(), text.lines().count());
    assert_eq!(view.lines().nth(6), Some("fn after() {}"), "code after a mid-file test module moved");
    assert_eq!(view.lines().nth(7), Some("fn last() {}"));
    assert_eq!((view.lines().nth(2), view.lines().nth(3), view.lines().nth(5)), (Some(""), Some(""), Some("")));
    assert!(!view.contains("fn t()"), "the test module stayed in: {view}");
}

#[test]
fn a_cfg_all_test_module_is_blanked() {
    let text = "fn a() {}\n#[cfg(all(test, unix))]\nmod t {\n    fn t() {}\n}\nfn after() {}\n";
    let view = without_test_modules(text);
    assert_eq!(view.lines().count(), text.lines().count());
    assert_eq!(view.lines().nth(5), Some("fn after() {}"));
    assert!(!view.contains("fn t()"), "the cfg(all(test, ..)) module stayed in: {view}");
    let kept = "#[cfg(all(unix, windows))]\nmod m {\n    fn m() {}\n}\n";
    assert!(without_test_modules(kept).contains("fn m()"), "a cfg without test was removed");
}

#[test]
fn production_sources_reads_no_test_path() {
    let files = production_sources();
    assert!(files.iter().all(|(path, _)| !is_test_path(path)), "a test path was returned");
    assert!(files.iter().any(|(path, _)| path == "rust/backend/src/main.rs"), "main.rs is not among them");
}

/// No scan cuts a file at its first `#[cfg(test)]`: the text of that attribute followed at once by a double
/// quote is what a string-literal cut (`find`, `split`, `==`) contains, and such a cut misses every production
/// line after a test-only item in the middle of a file. It sees a cut where the attribute text is followed at
/// once by a double quote. A literal that runs on past the attribute (`"#[cfg(test)]\n…"`) is not seen; that
/// limit is accepted (M4 recheck 3, N2).
#[test]
fn no_scan_cuts_a_file_at_its_first_cfg_test() {
    // Built with `concat!`, so this file does not hold the text it looks for.
    let needle = concat!("#[cfg(test)]", "\"");
    let hits: Vec<String> = rust_sources()
        .into_iter()
        .filter(|(path, text)| path != "rust/log/src/test_scan.rs" && text.contains(needle))
        .map(|(path, _)| path)
        .collect();
    assert!(hits.is_empty(), "a scan cuts a file at its first test attribute (use test_scan::without_test_modules): {hits:?}");
}
