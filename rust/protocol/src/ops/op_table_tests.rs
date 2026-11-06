// Wire tests for the op table: each wire name it declares is declared once.

use std::collections::HashSet;

use super::*;

#[test]
fn the_op_table_declares_each_wire_name_once() {
    let mut seen = HashSet::new();
    for name in op::ALL {
        assert!(
            seen.insert(*name),
            "the wire name {name:?} is declared by two entries of op_table!"
        );
    }
}
