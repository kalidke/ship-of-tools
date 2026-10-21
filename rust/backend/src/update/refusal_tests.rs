use super::fit_refusal;

// Role x prepare: only a backend role over an env-less prepare is refused for its depth; no record is refused for any role.
#[test]
fn a_pointer_applies_only_where_its_prepare_fits_the_hosts_role() {
    let missing = || Err("reading prepared.json: No such file or directory".to_string());
    // (wants_backend, windows, prepared, refused)
    let table = [
        (true, false, Ok(true), false),
        (true, false, Ok(false), true),
        (true, false, missing(), true),
        (false, false, Ok(true), false),
        (false, false, Ok(false), false),
        (false, false, missing(), true),
        (true, true, Ok(false), false),
        (true, true, missing(), true),
    ];
    for (wants, windows, prepared, refused) in table {
        let said = fit_refusal(wants, windows, prepared.clone());
        assert_eq!(said.is_some(), refused, "backend {wants} windows {windows} prepared {prepared:?}: {said:?}");
    }
}
