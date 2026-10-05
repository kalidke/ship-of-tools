//! Tests for monitor.rs: ring and sample handling, fixtures, and the roster config.

    use super::*;

    fn sample(ts: f64, cpu: f32) -> MonitorSample {
        MonitorSample {
            ts,
            cpu_pct: cpu,
            ram_pct: 10.0,
            cpu_cores: None,
            ram_total_gb: None,
            gpus: Vec::new(),
            top_procs: Vec::new(),
        }
    }

    #[test]
    fn raw_sample_parses_top_procs() {
        let line = r#"{"ts":1.0,"cpu":5.0,"ram":10.0,"cc":8,"rt":32,"gpus":[],"top":[{"n":"julia","o":"alice","c":187.5}]}"#;
        let s = serde_json::from_str::<RawSample>(line).unwrap().into_sample();
        assert_eq!(s.top_procs.len(), 1);
        assert_eq!(s.top_procs[0].name, "julia");
        assert_eq!(s.top_procs[0].user, "alice");
        assert!((s.top_procs[0].cpu_pct - 187.5).abs() < 0.01);
    }

    #[test]
    fn downsampled_bucket_drops_top_procs() {
        let mut r = HostRing::new();
        for i in 0..61 {
            let mut s = sample(1020.0 + i as f64, 30.0);
            s.top_procs = vec![sot_protocol::ProcSample {
                name: "julia".into(),
                user: "alice".into(),
                cpu_pct: 99.0,
            }];
            r.push(s);
        }
        assert_eq!(r.tier1.len(), 1);
        assert!(r.tier1[0].top_procs.is_empty(), "instantaneous data must not survive averaging");
        assert!(!r.tier0[0].top_procs.is_empty(), "raw tier keeps procs");
    }

    #[test]
    fn push_finalizes_minute_bucket_with_average() {
        let mut r = HostRing::new();
        // Bucket 17 spans ts 1020..1079; fill it, then cross to 1080 to roll over.
        for i in 0..60 {
            r.push(sample(1020.0 + i as f64, 30.0));
        }
        r.push(sample(1080.0, 30.0)); // crosses the 60 s boundary -> finalize bucket 17
        assert_eq!(r.tier1.len(), 1, "one minute bucket should have finalized");
        assert!((r.tier1[0].cpu_pct - 30.0).abs() < 0.01, "bucket averages cpu");
        assert!(r.tier0.len() >= 60, "tier0 keeps the raw samples");
    }

    #[test]
    fn query_returns_fine_samples_when_window_exceeds_data_span() {
        // Regression for the tier-selection bug: a window far wider than the
        // stored data must still return the fine 1 s samples we have, not a lone
        // coarse minute-bucket. The samples cross a minute boundary so tier1
        // holds a bucket — the exact condition that surfaced the bug on a shared multi-GPU host.
        let mut r = HostRing::new();
        for i in 0..=10 {
            r.push(sample(1075.0 + i as f64, 50.0)); // 1075..1085, crosses 1080
        }
        assert!(!r.tier1.is_empty(), "a minute bucket should have finalized");
        let hs = r.query("h", 120.0, 1085.0, 20); // window 120 s >> ~10 s of data
        assert_eq!(hs.step_s, 1.0, "must pick the 1 s tier, not the coarse bucket");
        assert!(
            hs.samples.len() >= 10,
            "should return the fine samples, got {}",
            hs.samples.len()
        );
    }

    #[test]
    fn query_picks_minute_tier_for_wide_window() {
        let mut r = HostRing::new();
        for i in 0..=10 {
            r.push(sample(1075.0 + i as f64, 50.0));
        }
        // window 3600 s / 20 points -> ideal step 180 s -> the minute tier.
        let hs = r.query("h", 3600.0, 1085.0, 20);
        assert_eq!(hs.step_s, 60.0);
    }

    #[test]
    fn query_strides_down_to_points() {
        let mut r = HostRing::new();
        for i in 0..100 {
            r.push(sample(2000.0 + i as f64, 50.0));
        }
        let hs = r.query("h", 200.0, 2099.0, 10);
        assert!(hs.samples.len() <= 10, "strided to <= points, got {}", hs.samples.len());
        assert!(!hs.samples.is_empty());
    }

    // ─── Target A: unified-memory GPU (GB10) fixture ──────────────────────
    // Captured live over ssh with the *fixed* SAMPLER_SH (host names
    // replaced with a neutral placeholder; nothing else altered).

    #[test]
    fn fixture_unified_memory_gpu_reports_mem_not_applicable() {
        let fixture = include_str!("../../tests/fixtures/monitor/unified_memory_gpu.ndjson");
        for line in fixture.lines() {
            let s = serde_json::from_str::<RawSample>(line).unwrap().into_sample();
            assert_eq!(s.gpus.len(), 1);
            assert_eq!(s.gpus[0].mem_pct, None, "unified memory has no VRAM to report");
            assert_eq!(s.gpus[0].temp_c, Some(40.0), "temperature must survive the N/A memory field");
            assert_eq!(s.gpus[0].util_pct, 0.0);
            assert_eq!(s.cpu_cores, Some(20));
            assert_eq!(s.ram_total_gb, Some(120.0));
        }
    }

    // ─── Target B: many-process host fixture ───────────────────────────────
    // Captured live over ssh with the *fixed* SAMPLER_SH (host names and
    // usernames replaced with neutral placeholders).

    #[test]
    fn fixture_many_process_host_parses() {
        let fixture = include_str!("../../tests/fixtures/monitor/many_process_host.ndjson");
        for line in fixture.lines() {
            let s = serde_json::from_str::<RawSample>(line).unwrap().into_sample();
            assert_eq!(s.cpu_cores, Some(64));
            assert_eq!(s.ram_total_gb, Some(755.0));
            assert_eq!(s.gpus[0].mem_pct, Some(1.0), "a discrete GPU's memory is still reported");
        }
    }

    /// A `null` in any single GPU field (SAMPLER_SH's rendering of an N/A
    /// reading) must not reject the whole line -- before RawGpu's fields were
    /// `Option<f32>`, a null here would fail `RawSample`'s deserialize and
    /// drop CPU/RAM/everything else in the same sample.
    #[test]
    fn raw_gpu_null_in_any_field_keeps_the_rest_of_the_sample() {
        for field in ["u", "m", "t", "p"] {
            let mut gpu = serde_json::json!({"i": 0, "u": 10.0, "m": 20.0, "t": 30.0, "p": 40.0});
            gpu[field] = serde_json::Value::Null;
            let line = format!(r#"{{"ts":1.0,"cpu":5.0,"ram":6.0,"cc":8,"rt":32,"gpus":[{gpu}],"top":[]}}"#);
            let s = serde_json::from_str::<RawSample>(&line)
                .unwrap_or_else(|e| panic!("a null \"{field}\" must not reject the line: {e}"))
                .into_sample();
            assert_eq!(s.cpu_pct, 5.0);
            assert_eq!(s.ram_pct, 6.0);
            assert_eq!(s.gpus.len(), 1);
        }
    }

    /// Runs the REAL, shipped `SAMPLER_SH` (nvidia-smi stubbed on PATH) and
    /// proves every N/A spelling nvidia-smi actually uses -- "[N/A]", bare
    /// "N/A", "Not Supported", and an empty field -- renders as `null` while
    /// a real number on the *same* row survives untouched.
    /// macOS lane disposition: correctly Linux-only, permanently.
    /// `SAMPLER_SH` itself is a `/proc`-reader (`/proc/stat`,
    /// `/proc/meminfo`, `/proc/cpuinfo`) — none of which Darwin has — so
    /// running the REAL shipped script, which is the whole point of this
    /// test, cannot mean anything there. A macOS monitor pane needs a
    /// different sampler, not a widened gate on this one.
    #[cfg(target_os = "linux")]
    #[test]
    fn sampler_script_treats_every_na_spelling_as_not_reported() {
        let dir = std::env::temp_dir().join(format!("sot-sampler-na-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let stub = dir.join("nvidia-smi");
        std::fs::write(
            &stub,
            "#!/bin/sh\ncat <<'EOF'\n0, 11, [N/A], [N/A], 40, 5.0\n1, 22, N/A, N/A, 41, 5.1\n2, 33, Not Supported, Not Supported, 42, 5.2\n3, 44, , , 43, 5.3\nEOF\n",
        )
        .expect("write stub");
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod stub");

        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        let mut child = std::process::Command::new("bash")
            .arg("-s")
            .arg("1")
            .arg("1")
            .env("PATH", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bash");
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(SAMPLER_SH.as_bytes())
                .expect("feed sampler script");
        }
        let out = child.wait_with_output().expect("sampler run");
        let _ = std::fs::remove_dir_all(&dir);

        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().next().expect("one sample line");
        let raw: RawSample = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("every N/A spelling must still parse: {e}\nline: {line}"));
        assert_eq!(raw.gpus.len(), 4, "all four rows survive");
        for g in &raw.gpus {
            assert!(g.m.is_none(), "gpu {} memory must be not-applicable, got {:?}", g.i, g.m);
            assert!(g.u.is_some(), "utilization on the same row must survive");
            assert!(g.t.is_some(), "temperature on the same row must survive");
        }
    }

    /// Root cause of target B's degraded top-processes list: a pid
    /// glob-expanded by the shell can have exited by the time `awk` opens
    /// it, and awk -- confirmed on both gawk and mawk -- treats a missing
    /// ARGV file as fatal, aborting *before* its END block ever runs, so the
    /// whole scan silently returns nothing even though every other matched
    /// pid was still readable. SAMPLER_SH's proc-scan now pipes through
    /// `cat` instead, which warns per missing file and keeps reading the
    /// rest -- this is the mechanism that fix relies on.
    #[cfg(unix)]
    #[test]
    fn awk_aborts_on_a_vanished_argv_file_but_cat_piping_survives_it() {
        let dir = std::env::temp_dir().join(format!("sot-monitor-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let survivor = dir.join("7.stat");
        std::fs::write(&survivor, "7 (alive) S 1 1\n").expect("write survivor");
        let vanished = dir.join("8.stat"); // never created: a pid that exited between glob and open
        let prog = "{ n++ } END { print n+0 }";

        let old = std::process::Command::new("awk")
            .arg(prog)
            .arg(&vanished)
            .arg(&survivor)
            .output()
            .expect("run awk directly on the argv list");
        assert!(!old.status.success(), "awk must fail fatally on a missing ARGV file");
        assert!(
            old.stdout.is_empty(),
            "END never runs on the fatal path, so the survivor's record is lost too: {:?}",
            String::from_utf8_lossy(&old.stdout)
        );

        let piped = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("cat {} {} 2>/dev/null | awk '{}'", vanished.display(), survivor.display(), prog))
            .output()
            .expect("run cat | awk");
        assert!(piped.status.success());
        assert_eq!(
            String::from_utf8_lossy(&piped.stdout).trim(),
            "1",
            "the fix: cat skips the vanished file and awk still counts the survivor"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Program override per host name, so no test touches `bash` or `ssh`.
    pub(super) static STUB_SAMPLER: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

    /// The respawn backoff is a bare sleep: after the shutdown the loop must
    /// return, not wake and start another sampler.
    #[cfg(unix)]
    #[tokio::test]
    async fn monitor_backoff_returns_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("sampler");
        crate::paths::write_stub(&stub, "#!/bin/sh\nexit 1\n");
        STUB_SAMPLER.lock().unwrap().push(("stub-backoff".to_string(), stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (tick_tx, mut ticks) = broadcast::channel::<HostLatest>(16);
        let host = MonitorHost { name: "stub-backoff".to_string(), ssh_alias: None, local: true };
        let rings = Arc::new(Mutex::new(HashMap::from([(host.name.clone(), HostRing::new())])));
        let task = tokio::spawn(supervise(host, tick_tx, rings, sig));
        loop {
            let tick = tokio::time::timeout(Duration::from_secs(5), ticks.recv())
                .await
                .expect("the stub sampler never died")
                .expect("tick channel");
            if tick.stale {
                break;
            }
        }
        sig.fire();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the monitor backoff outlived the shutdown")
            .expect("supervise task");
    }

#[cfg(test)]
mod config_tests {
    use super::*;

    /// The roster the retired private parser produced for this file, now
    /// reached through the shared parser: same names, same local/remote
    /// split, same alias fallback.
    #[test]
    fn monitor_targets_match_the_old_parser() {
        let text = "hub = \"alpha\"\n[host.alpha]\ndaemon = true\n[monitor]\nalpha = \"alpha\"\nbeta = \"\"\ngpu = \"someone@gpu\"\n";
        let topo = sot_protocol::topology::parse(text).unwrap();
        let hosts = monitor_hosts(topo.monitor_targets(), "alpha");
        assert_eq!(hosts.len(), 3);
        assert!(hosts[0].local && hosts[0].ssh_alias.is_none() && hosts[0].name == "alpha");
        assert!(!hosts[1].local && hosts[1].ssh_alias.as_deref() == Some("beta"));
        assert!(!hosts[2].local && hosts[2].ssh_alias.as_deref() == Some("someone@gpu") && hosts[2].name == "gpu");
        // The alias, not only the label, marks the local box.
        let hosts = monitor_hosts(vec![("lab".into(), "alpha".into())], "alpha");
        assert!(hosts[0].local && hosts[0].name == "lab");
    }

    /// The roster is hub-scoped declaration: only the declared hub executes
    /// it. Every other box samples itself and nothing else, regardless of
    /// what the file declares.
    #[test]
    fn only_a_linux_daemon_samples_itself() {
        let hosts = vec![
            MonitorHost { name: "here".into(), ssh_alias: None, local: true },
            MonitorHost { name: "there".into(), ssh_alias: Some("there".into()), local: false },
        ];
        let linux = without_local_sampler(hosts.clone(), true);
        assert_eq!(linux.len(), 2);
        let other = without_local_sampler(hosts, false);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].name, "there");
        assert!(without_local_sampler(
            vec![MonitorHost { name: "here".into(), ssh_alias: None, local: true }],
            false
        )
        .is_empty());
    }

    #[test]
    fn only_the_hub_executes_the_monitor_roster() {
        let text = "hub = \"alpha\"\n[host.alpha]\ndaemon = true\n[monitor]\nalpha = \"alpha\"\nbeta = \"\"\ngpu = \"someone@gpu\"\n";
        let topo = sot_protocol::topology::parse(text).unwrap();

        let hosts = sampling_roster(&topo, "alpha");
        assert_eq!(hosts.len(), 3);
        assert!(hosts[0].local && hosts[0].ssh_alias.is_none() && hosts[0].name == "alpha");
        assert!(!hosts[1].local && hosts[1].ssh_alias.as_deref() == Some("beta"));
        assert!(!hosts[2].local && hosts[2].ssh_alias.as_deref() == Some("someone@gpu") && hosts[2].name == "gpu");

        let hosts = sampling_roster(&topo, "gamma");
        assert_eq!(hosts.len(), 1);
        assert!(hosts[0].local && hosts[0].ssh_alias.is_none() && hosts[0].name == "gamma");
    }

    /// Pins the pre-existing "no file, no topology at all" fallback: a box
    /// with nothing declared still samples itself, exactly as before this
    /// change — the hub-scoping rule only narrows what a *declared* roster
    /// means, it does not touch the no-declaration case.
    #[test]
    fn a_box_with_no_hub_declaration_samples_itself() {
        let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = crate::paths::EnvGuard::capture("SOT_HOSTS");
        std::env::set_var("SOT_HOSTS", "/nowhere/hosts.toml");
        let hosts = load_hosts();
        if !cfg!(target_os = "linux") {
            // The fallback single-host roster is still local-only, and
            // load_hosts() strips the local sampler off Linux same as any
            // other roster: no declaration means an empty roster there.
            assert!(hosts.is_empty());
        } else {
            assert_eq!(hosts.len(), 1);
            assert!(hosts[0].local && hosts[0].ssh_alias.is_none());
        }
    }

    /// The dedup that keeps a permanently-unreachable target from logging
    /// its reason on every ~5s respawn forever: same reason logs once, a
    /// changed reason logs again, an empty reason never logs.
    #[test]
    fn a_repeated_failure_reason_is_logged_once() {
        assert!(should_log(None, "connection timed out"));
        assert!(!should_log(Some("connection timed out"), "connection timed out"));
        assert!(should_log(Some("connection timed out"), "permission denied"));
        assert!(!should_log(Some("connection timed out"), ""));
        assert!(!should_log(None, ""));
    }
}
