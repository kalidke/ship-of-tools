use super::*;

#[test]
fn session_name_uses_slug() {
    assert_eq!(session_name("MyPackage.jl"), "sot-be-mypackage_jl");
}
