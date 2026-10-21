#[test]
fn the_status_names_the_host_and_leads_with_what_to_do() {
    let said = super::pluto_starting_status("lab", "Pluto is still starting after 120 s");
    assert_eq!(said, "pluto · still starting on lab; open again once it is ready · Pluto is still starting after 120 s");
}
