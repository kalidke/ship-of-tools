use super::*;

#[test]
fn unlisted_ambiguous_and_inert_results_refuse_before_effects() {
    let host = "<host>".to_string();
    let mut lists = HashMap::new();
    assert!(matches!(
        result_route_decision(&lists, &host, "project", None, true, false),
        ResultRoute::Refusal(_)
    ));
    let mut row = ws_info("anchor", "anchor-target");
    row.is_default = true;
    row.agent = "none".into();
    lists.insert(host.clone(), vec![row.clone()]);
    for alias in ["", "default", "<default>", "anchor"] {
        assert!(matches!(
            result_route_decision(&lists, &host, alias, None, false, false),
            ResultRoute::Refusal(_)
        ));
        let target = resolve_listed_workspace(&lists, &host, alias).unwrap();
        assert!(matches!(
            result_route_decision(&lists, &host, alias, Some(target.identity()), false, false),
            ResultRoute::Render(_)
        ));
        assert!(matches!(
            result_route_decision(&lists, &host, alias, None, true, true),
            ResultRoute::Switch(_)
        ));
    }
    row.agent = "claude".into();
    row.session_name.clear();
    lists.insert(host.clone(), vec![row.clone()]);
    assert!(matches!(
        result_route_decision(&lists, &host, "anchor", None, false, false),
        ResultRoute::Badge(_)
    ));
    assert!(matches!(
        result_route_decision(&lists, &host, "anchor", None, true, true),
        ResultRoute::Refusal(_)
    ));
    lists.get_mut(&host).unwrap().push(row);
    assert!(matches!(
        result_route_decision(&lists, &host, "anchor", None, true, false),
        ResultRoute::Refusal(_)
    ));
}
