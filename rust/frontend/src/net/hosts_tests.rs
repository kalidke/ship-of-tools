use super::*;

#[test]
fn lane_dial_matches_the_resolved_control_transport_selection() {
    let recipe = sot_topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let pipe_config = crate::net::transport::TransportConfig {
        dial: crate::net::transport::Dial::Pipe(std::path::PathBuf::from("/tmp/sock")),
        token: Some("tok".to_string()),
    };
    // A pipe-configured host whose control connection resolved LOCAL —
    // the lane dial follows.
    match lane_dial(&pipe_config, ResolvedDial::Local, &Default::default()) {
        Some((sot_topology::lane_client::LaneDial::Local(path), token)) => {
            assert_eq!(path, std::path::PathBuf::from("/tmp/sock"));
            assert_eq!(token.as_deref(), Some("tok"));
        }
        Some((sot_topology::lane_client::LaneDial::Link(_), _)) => {
            panic!("must follow the resolved Local selection, not guess the link")
        }
        None => panic!("a resolved+configured pipe must dial, got None"),
    }
    // An ssh-configured host whose control connection resolved SSH —
    // the lane dial follows, over the host's link (never a second,
    // independent read of `config.dial`, and never a login of its own).
    let ssh_config = crate::net::transport::TransportConfig {
        dial: crate::net::transport::Dial::Ssh(recipe.clone()),
        token: Some("tok".to_string()),
    };
    match lane_dial(&ssh_config, ResolvedDial::Ssh(recipe.clone()), &Default::default()) {
        Some((sot_topology::lane_client::LaneDial::Link(_), token)) => assert_eq!(token.as_deref(), Some("tok")),
        Some((sot_topology::lane_client::LaneDial::Local(_), _)) => {
            panic!("must follow the resolved Ssh selection, not guess local")
        }
        None => panic!("a resolved ssh connection must dial, got None"),
    }
    // A generated hub relay: the lane dial follows the control connection over the host's link, and a
    // relay-configured host whose control connection resolved Local dials nothing.
    let relay_path = std::path::PathBuf::from("/run/user/1000/sot-host-far.sock");
    let relay_config = crate::net::transport::TransportConfig {
        dial: crate::net::transport::Dial::Relay(relay_path.clone()),
        token: Some("tok".to_string()),
    };
    match lane_dial(
        &relay_config,
        ResolvedDial::Relay(relay_path.clone()),
        &Default::default(),
    ) {
        Some((sot_topology::lane_client::LaneDial::Link(_), token)) => assert_eq!(token.as_deref(), Some("tok")),
        _ => panic!("a resolved relay must dial over the host's link"),
    }
    assert!(lane_dial(&relay_config, ResolvedDial::Local, &Default::default()).is_none());
    // Resolved Local but `config.dial` holds Ssh (shouldn't happen —
    // the control connection could not have resolved Local without a
    // configured pipe) degrades to no dial rather than panicking.
    assert!(lane_dial(&ssh_config, ResolvedDial::Local, &Default::default()).is_none());
}
