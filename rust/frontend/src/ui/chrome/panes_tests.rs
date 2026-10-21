use super::*;

#[test]
fn move_in_walks_only_the_laid_out_columns() {
    use crate::ui::persist::settings::Slot as S;
    use PaneFocus::*;
    use SpatialDir::*;
    let full = [S::Nav, S::Preview, S::Llm];
    assert_eq!(NavTree.move_in(Right, &full, None), Preview);
    assert_eq!(Repl.move_in(Up, &full, Some(S::Repl)), Preview);
    assert_eq!(Repl.move_in(Left, &full, Some(S::Repl)), NavTree);
    assert_eq!(Repl.move_in(Right, &full, Some(S::Repl)), Llm);
    let portrait = [S::Nav, S::Preview];
    assert_eq!(Preview.move_in(Right, &portrait, None), Preview);
    assert_eq!(Repl.move_in(Right, &portrait, Some(S::Repl)), Preview);
    assert_eq!(Preview.move_in(Down, &portrait, None), Preview);
    assert_eq!(Preview.move_in(Down, &portrait, Some(S::Repl)), Repl);
    assert_eq!(Llm.move_in(Left, &portrait, None), NavTree);
    let no_preview = [S::Nav, S::Llm];
    assert_eq!(NavTree.move_in(Right, &no_preview, None), Llm);
    assert_eq!(Llm.move_in(Left, &no_preview, None), NavTree);
    assert_eq!(Repl.move_in(Up, &no_preview, Some(S::Repl)), NavTree);
    assert_eq!(Repl.move_in(Up, &full, None), NavTree);
    let custom = [S::Nav, S::Llm];
    assert_eq!(NavTree.move_in(Down, &custom, Some(S::Preview)), Preview);
    assert_eq!(Preview.move_in(Up, &custom, Some(S::Preview)), NavTree);
    assert_eq!(Preview.move_in(Right, &custom, Some(S::Preview)), Llm);
    assert_eq!(Preview.move_in(Down, &custom, Some(S::Preview)), Preview);
    assert_eq!(Repl.move_in(Down, &custom, Some(S::Preview)), NavTree);
}

#[test]
fn drawer_toggle_is_symmetric() {
    use DrawerContent::*;
    // Open from closed.
    assert_eq!(Closed.toggle(Repl), Repl);
    assert_eq!(Closed.toggle(Terminal), Terminal);
    // Same key closes.
    assert_eq!(Repl.toggle(Repl), Closed);
    assert_eq!(Terminal.toggle(Terminal), Closed);
    // Other key swaps content without closing.
    assert_eq!(Repl.toggle(Terminal), Terminal);
    assert_eq!(Terminal.toggle(Repl), Repl);
}

#[test]
fn drawer_is_open_only_when_not_closed() {
    assert!(!DrawerContent::Closed.is_open());
    assert!(DrawerContent::Repl.is_open());
    assert!(DrawerContent::Terminal.is_open());
}
