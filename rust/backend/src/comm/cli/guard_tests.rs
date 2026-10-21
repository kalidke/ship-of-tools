use super::*;

fn input(agent: &str, cmd: &str) -> String {
    let mut v = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd}});
    if !agent.is_empty() {
        v["agent_id"] = agent.into();
    }
    v.to_string()
}

/// A subagent's run of an installed comm script is denied; the session's own, and everything else, is allowed.
#[test]
fn a_subagent_never_runs_a_comm_script_and_a_session_always_may() {
    let deny = [
        "~/.sot-comm/bin/comm-send.sh @x 'hi'",
        r#"bash -c "comm-poll.sh""#,
        "$HOME/.sot-comm/bin/comm-status.sh waiting x",
        r#"'/h/.sot-comm/bin/comm-'"send".sh @x hi"#,
        "ls && cd /tmp; comm-poll.sh",
        "/home/u/.sot-comm/bin/comm-poll.sh",
        "${HOME}/.sot-comm/bin/comm-send.sh @x hi",
        "comm-send.sh @x hi",
        "true && comm-send.sh @x hi",
        "true; comm-poll.sh",
        "x=$(comm-poll.sh)",
        "bash comm-send.sh @x hi",
        "FOO=1 env BAR=2 comm-send.sh @x hi",
        "echo hi | comm-send.sh @x",
        "echo a\ncomm-poll.sh",
    ];
    for cmd in deny {
        assert_eq!(decide(&input("agent-1", cmd)), Some(DENY), "{cmd}");
        assert_eq!(decide(&input("", cmd)), None, "the session's own: {cmd}");
    }
    let allow = [
        "HOME=/tmp/x bash comm/mail/comm-send.sh @x hi",
        "grep -n x comm/mail/comm-send.sh",
        "cat comm/registry/comm-join.sh",
        "git log -- comm/mail/comm-send.sh",
        r#"git commit -m "fix comm-send.sh quoting""#,
        "./tests/comm-send.sh",
        "ls",
        r#"git commit -m "comm work""#,
        "cat /x/comm/notes.sh",
    ];
    for cmd in allow {
        assert_eq!(decide(&input("agent-1", cmd)), None, "{cmd}");
    }
    assert_eq!(decide("not json {"), None);
}
