use super::valid_name;

#[test]
fn accepts_typical_names() {
    assert!(valid_name("sot-be-myhost"));
    assert!(valid_name("myhost-dev"));
    assert!(valid_name("MyPackage.jl"));
    assert!(valid_name("a"));
    assert!(valid_name(&"a".repeat(64)));
}

#[test]
fn rejects_empty_and_oversize() {
    assert!(!valid_name(""));
    assert!(!valid_name(&"a".repeat(65)));
}

#[test]
fn rejects_shell_and_parser_metacharacters() {
    // The pipe is the specific `tmux.rs` list-parsing corruption vector;
    // the rest are generic shell-injection/whitespace rejects.
    for bad in [
        "a|b",
        "a;b",
        "a b",
        "a'b",
        "a$b",
        "a`b",
        "a\nb",
        "/etc/passwd",
    ] {
        assert!(!valid_name(bad), "expected {bad:?} to be rejected");
    }
}
