use super::*;

#[test]
fn strip_truncate_ellipsizes_long_labels() {
    assert_eq!(strip_truncate("short"), "short");
    let long = "a".repeat(40);
    let t = strip_truncate(&long);
    assert_eq!(t.chars().count(), STRIP_MAX_LABEL);
    assert!(t.ends_with('…'));
}

#[test]
fn strip_items_gives_a_lone_host_its_ship_too() {
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("alpha".to_string(), "two".to_string()),
        ("alpha".to_string(), "three".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    assert_eq!(
        items[0],
        StripItem::Bow {
            host: "alpha".to_string(),
            name: "alpha".to_string()
        },
        "one host is a ship too (owner, 2026-09-27): {items:?}"
    );
    assert_eq!(items.len(), slugs.len() + 1);
    assert_eq!(ship_spans(&items), vec![(0, 3)], "and it is exactly one ship");
}

#[test]
fn strip_items_puts_a_bow_at_every_group_head_including_the_first() {
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("alpha".to_string(), "two".to_string()),
        ("beta".to_string(), "three".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    assert_eq!(
        items,
        vec![
            StripItem::Bow {
                host: "alpha".to_string(),
                name: "alpha".to_string()
            },
            StripItem::Session(slugs[0].clone()),
            StripItem::Session(slugs[1].clone()),
            StripItem::Bow {
                host: "beta".to_string(),
                name: "beta".to_string()
            },
            StripItem::Session(slugs[2].clone()),
        ],
        "every group is a ship: a bow at its head, the first group included"
    );
}

#[test]
fn strip_items_three_hosts_middle_host_already_filtered_out_of_slugs() {
    // A host with zero visible rows never appears in `slugs` to begin
    // with (`fresh_workspace_caches` excludes it) — so from
    // `strip_items`'s point of view this is just two groups, two bows,
    // order preserved.
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("gamma".to_string(), "two".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    assert_eq!(
        items,
        vec![
            StripItem::Bow {
                host: "alpha".to_string(),
                name: "alpha".to_string()
            },
            StripItem::Session(slugs[0].clone()),
            StripItem::Bow {
                host: "gamma".to_string(),
                name: "gamma".to_string()
            },
            StripItem::Session(slugs[1].clone()),
        ]
    );
}

#[test]
fn strip_items_names_every_bow_with_what_the_caller_resolved() {
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("beta".to_string(), "two".to_string()),
        ("beta".to_string(), "three".to_string()),
        ("gamma".to_string(), "four".to_string()),
    ];
    // The draw site passes `host_label`; a stand-in that renames every
    // key proves the name is the caller's projection, not the key — and
    // that the bow still carries the raw HostKey the steered-ship
    // comparison needs.
    let items = strip_items(&slugs, |h| format!("{h}-shown"));
    let bows: Vec<(&str, &str)> = items
        .iter()
        .filter_map(|it| match it {
            StripItem::Bow { host, name } => Some((host.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        bows,
        vec![
            ("alpha", "alpha-shown"),
            ("beta", "beta-shown"),
            ("gamma", "gamma-shown")
        ]
    );
}

#[test]
fn strip_item_widths_measures_a_bow_as_its_wheel_alone() {
    // The bow reserves the WHEEL and nothing else (owner, 2026-09-27): its
    // box name runs beneath the session names on the hull row, so the
    // layout never spends a column on it and the gap after a bow cannot
    // grow with the host's name length.
    let items = vec![
        StripItem::Bow {
            host: "alpha".to_string(),
            name: "a-very-long-host-name".to_string(),
        },
        StripItem::Session(("alpha".to_string(), "one".to_string())),
    ];
    let widths = strip_item_widths(&items, &[40.0], 25.0);
    assert!(
        (widths[0] - 25.0).abs() < 1e-3,
        "the wheel alone, however long the name: {widths:?}"
    );
    assert!(
        (widths[1] - 40.0).abs() < 1e-3,
        "sessions stay in lock-step with `label_widths`"
    );
    // No decoded logo: the wheel goes and the bow measures nothing at all
    // (the logo asset's fail-soft contract) — its NAME still marks the
    // group, a row below, where no width is needed for it.
    let bare = strip_item_widths(&items, &[40.0], 0.0);
    assert!((bare[0]).abs() < 1e-3, "{bare:?}");
}

#[test]
fn the_first_session_sits_three_cells_past_the_wheel_whatever_the_box_is_called() {
    // The defect the wheel-only bow width fixes: reserving wheel + air +
    // name pushed the first session of a group right by the HOST's name
    // length — 8 cells for a 4-letter host, 11.6 for an 8-letter one. Two
    // boxes whose names differ by four letters must start their sessions at
    // the same distance past their own wheel.
    let cell_w = 10.0;
    let wheel_w = 16.0;
    let first_session_offset = |host: &str| -> f32 {
        let slugs: Vec<WsKey> = vec![(host.to_string(), "one".to_string())];
        let items = strip_items(&slugs, |h| h.clone());
        let widths = strip_item_widths(&items, &[40.0], wheel_w);
        let pos = strip_cursor_positions(&widths, |i| strip_gap_before(&items[i], cell_w));
        pos[1] - (pos[0] + wheel_w)
    };
    assert!(
        (first_session_offset("hub") - STRIP_GAP_CELLS * cell_w).abs() < 1e-3,
        "three cells past the wheel: {}",
        first_session_offset("hub")
    );
    assert!(
        (first_session_offset("hub") - first_session_offset("a-much-longer-box-name")).abs() < 1e-3,
        "and the same three cells however long the box is named"
    );
}

#[test]
fn strip_offsets_shift_sessions_past_every_bow() {
    let cell_w = 10.0;
    let gap = STRIP_GAP_CELLS * cell_w;
    let wheel_w = 25.0;
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("beta".to_string(), "two".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    let labels = vec!["aa".to_string(), "bbbb".to_string()];
    let label_widths: Vec<f32> = labels
        .iter()
        .map(|l| l.chars().count() as f32 * cell_w)
        .collect();
    let item_widths = strip_item_widths(&items, &label_widths, wheel_w);
    let offsets = strip_divider_offsets(&items, &item_widths, &label_widths, cell_w);
    // Both bows measure their wheel alone, whatever their hosts are called.
    // The FIRST bow opens no water (nothing precedes it), so session 0 is
    // pushed by that wheel and one word space; session 1 by a second wheel
    // and the water a mid-strip ship boundary opens.
    let ship_gap = strip_gap_before(&items[2], cell_w);
    assert!((offsets[0] - (wheel_w + gap)).abs() < 1e-3, "{offsets:?}");
    assert!(
        (offsets[1] - (offsets[0] + wheel_w + ship_gap)).abs() < 1e-3,
        "offsets: {offsets:?}"
    );
}

#[test]
fn ship_spans_run_each_bow_to_its_own_groups_last_item() {
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("alpha".to_string(), "two".to_string()),
        ("beta".to_string(), "three".to_string()),
        ("gamma".to_string(), "four".to_string()),
        ("gamma".to_string(), "five".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    // [bow a, s, s, bow b, s, bow g, s, s] — three groups, so the inner
    // sterns (from the NEXT bow) are told apart from the last group's,
    // which falls back to the final item.
    assert_eq!(ship_spans(&items), vec![(0, 2), (3, 4), (5, 7)]);
    // No items at all: no ships, and no `items.len()-1` underflow.
    assert!(ship_spans(&[]).is_empty());
    // One host is one ship, running to its own last session.
    let single = strip_items(&slugs[..2], |h| h.clone());
    assert_eq!(ship_spans(&single), vec![(0, 2)]);
}

#[test]
fn strip_label_puts_the_badge_inside_the_label_cap() {
    assert_eq!(strip_label("aa", false), "aa");
    assert_eq!(strip_label("aa", true), "●aa");
    let long = "x".repeat(STRIP_MAX_LABEL);
    assert_eq!(strip_label(&long, false), long, "an unbadged name at the cap is untouched");
    let badged = strip_label(&long, true);
    assert_eq!(
        badged.chars().count(),
        STRIP_MAX_LABEL,
        "a badged name is never wider than the cap: {badged:?}"
    );
    assert!(badged.starts_with('●') && badged.ends_with('…'), "{badged:?}");
}
