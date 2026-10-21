use super::*;

fn v(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

#[test]
fn help_for_table() {
    let rows: &[(&str, &str)] = &[
        ("--help", SOTD_HELP),
        ("--version --help", SOTD_HELP),
        ("--label x --help", SOTD_HELP),
        ("--socket --help", SOTD_HELP),
        ("session-socket-path --help", SOTD_HELP),
        ("agent-exec --help", SOTD_HELP),
        ("ancestors --help", comm::registry::ancestors::USAGE),
        ("ancestors --from 1 --help", comm::registry::ancestors::USAGE),
        ("stdio-bridge --help", topology::stdio_bridge::USAGE),
        ("stdio-bridge --host a --help", topology::stdio_bridge::USAGE),
        ("stdio-bridge --endpoint unix:/x --help", topology::stdio_bridge::USAGE),
        ("status --help", topology::status::USAGE),
        ("same-version --endpoint unix:/x --help", topology::same_version::USAGE),
        ("update --help", update::USAGE),
        ("update stage --help", update::USAGE),
        ("update stage d --prefix p --help", update::USAGE),
        ("update pointer --help", update::USAGE),
        ("update pointer --prefix p --target t --help", update::USAGE),
        ("update direction v --prefix p --help", update::USAGE),
        ("update repls --endpoint unix:/x --help", update::USAGE),
        ("topology --help", topology::cli::USAGE),
        ("topology plan --help", topology::cli::USAGE),
        ("topology status --help", topology::cli::USAGE),
        ("topology relay-endpoint --help", topology::cli::USAGE),
        ("topology relay-sockets --help", topology::cli::USAGE),
        ("topology sync --help", topology::cli::USAGE),
        ("topology apply --help", topology::cli::USAGE),
        ("topology apply --yes --help", topology::cli::USAGE),
        ("topology apply --help --yes", topology::cli::USAGE),
        ("topology set --help", topology::cli::USAGE),
        ("topology set add --help", topology::cli::USAGE),
        ("topology set add h --help", topology::cli::USAGE),
        ("topology set remove --help", topology::cli::USAGE),
    ];
    for (argv, want) in rows {
        for flag in ["--help", "-h"] {
            let mut args = v(argv);
            for a in args.iter_mut().filter(|a| *a == "--help") {
                *a = flag.to_string();
            }
            assert_eq!(help_for(&args), Some(*want), "{args:?}");
        }
    }
    for argv in [
        "agent-exec claude --help",
        "agent-exec claude -h",
        "topology plan",
        "status --json",
        "session-socket-path sot",
        "log-path",
        "",
    ] {
        assert_eq!(help_for(&v(argv)), None, "{argv:?}");
    }
}
