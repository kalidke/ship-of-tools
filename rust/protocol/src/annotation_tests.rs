use super::*;

#[test]
fn the_header_and_body_are_slices_of_the_original() {
    for s in [
        "---\ntarget: x\nsynced_against: abc\n---\n# Body\n\nText.\n",
        "---\r\ntarget: x\r\n---\r\nbody\r\n",
        "---\ntarget: x\n---",
        "---\n---\n",
        "  ---  \nt: x\n ---\nbody",
    ] {
        let (h, b) = split_frontmatter(s).unwrap();
        assert_eq!(format!("{h}{b}"), s);
        assert!(h.trim_end().ends_with("---"));
    }
    let (h, b) = split_frontmatter("---\ntarget: x\n---\n# Body\n").unwrap();
    assert_eq!((h, b), ("---\ntarget: x\n---\n", "# Body\n"));
    assert_eq!(
        split_frontmatter("---\nt: x\n---").unwrap(),
        ("---\nt: x\n---", "")
    );
}

#[test]
fn no_complete_fence_pair_is_no_header() {
    for s in [
        "",
        "# Body\n",
        "---\ntarget: x\n# never closed\n",
        "---\nt: x\n---x\nbody\n",
        "x\n---\nt: x\n---\n",
    ] {
        assert_eq!(split_frontmatter(s), None, "{s:?}");
        assert_eq!(synced_against(s), None, "{s:?}");
    }
}

#[test]
fn synced_against_reads_every_shape() {
    for s in [
        "---\nsynced_against: abc\n---\n",
        "---\r\nsynced_against: abc\r\n---\r\n",
        "---\n  synced_against: \"abc\"  \n---\n",
        "---\n\tsynced_against: 'abc'\n---",
        "---\nsynced_against:abc\nsynced_against: other\n---\nbody\n",
    ] {
        assert_eq!(synced_against(s).as_deref(), Some("abc"), "{s:?}");
    }
}

#[test]
fn synced_against_absent_or_empty_is_none() {
    for s in [
        "---\ntarget: x\n---\n",
        "---\nsynced_against:\n---\n",
        "---\nsynced_against: \"\"\n---\n",
        "---\nt: x\n---\nsynced_against: in the body\n",
    ] {
        assert_eq!(synced_against(s), None, "{s:?}");
    }
}
