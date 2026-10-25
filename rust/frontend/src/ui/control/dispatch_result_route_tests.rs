use super::*;

#[test]
fn unlisted_ambiguous_and_inert_results_refuse_before_effects() {
    let host = "<host>".to_string();
    let mut directory = RowDirectory::default();
    assert!(matches!(
        result_route_decision(&directory, &host, "project", None, true, false),
        ResultRoute::Refusal(_)
    ));
    let mut row = ws_info("anchor", "anchor-target");
    row.is_default = true;
    row.agent = "none".into();
    let update = directory.prepare_replace(host.clone(), vec![row.clone()]);
    directory.commit_replace(update);
    for alias in ["", "default", "<default>", "anchor"] {
        assert!(matches!(
            result_route_decision(&directory, &host, alias, None, false, false),
            ResultRoute::Refusal(_)
        ));
        let target = directory.resolve(&host, alias).unwrap();
        assert!(matches!(
            result_route_decision(&directory, &host, alias, Some(target.identity()), false, false),
            ResultRoute::Render(_)
        ));
        assert!(matches!(
            result_route_decision(&directory, &host, alias, None, true, true),
            ResultRoute::Switch(_)
        ));
    }
    row.agent = "claude".into();
    row.session_name.clear();
    let update = directory.prepare_replace(host.clone(), vec![row.clone()]);
    directory.commit_replace(update);
    assert!(matches!(
        result_route_decision(&directory, &host, "anchor", None, false, false),
        ResultRoute::Badge(_)
    ));
    assert!(matches!(
        result_route_decision(&directory, &host, "anchor", None, true, true),
        ResultRoute::Refusal(_)
    ));
    let update = directory.prepare_replace(host.clone(), vec![row.clone(), row]);
    directory.commit_replace(update);
    assert!(matches!(
        result_route_decision(&directory, &host, "anchor", None, true, false),
        ResultRoute::Refusal(_)
    ));
}
