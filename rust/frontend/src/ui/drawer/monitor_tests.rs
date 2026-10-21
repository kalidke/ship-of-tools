use super::*;
use sot_protocol::{GpuSample, HostLatest, HostSeries, MonitorSample};

/// Each host's mark on the window's clocks now.
fn marks_now(v: &MonitorView) -> Vec<String> {
    let (now, now_epoch) = clocks();
    v.marks(now, now_epoch)
}

fn sample(ts: f64, cpu: f32, gpus: &[(u32, f32)]) -> MonitorSample {
    MonitorSample {
        ts,
        cpu_pct: cpu,
        ram_pct: 10.0,
        cpu_cores: Some(32),
        ram_total_gb: Some(128.0),
        gpus: gpus
            .iter()
            .map(|(i, u)| GpuSample {
                index: *i,
                name: None,
                util_pct: *u,
                mem_pct: Some(0.0),
                temp_c: None,
                power_w: None,
            })
            .collect(),
        top_procs: Vec::new(),
    }
}

#[test]
fn render_svg_wellformed_with_per_host_traces() {
    let mut v = MonitorView::new();
    v.set_roster(vec!["host-a".into(), "host-b".into()]);
    v.apply_history(vec![
        HostSeries {
            host: "host-a".into(),
            step_s: 1.0,
            stale: false,
            samples: vec![
                sample(100.0, 50.0, &[(0, 5.0)]),
                sample(101.0, 60.0, &[(0, 7.0)]),
                sample(102.0, 55.0, &[(0, 6.0)]),
            ],
        },
        HostSeries {
            host: "host-b".into(),
            step_s: 1.0,
            stale: false,
            samples: vec![
                sample(100.0, 1.0, &[(0, 0.0), (1, 59.0)]),
                sample(101.0, 2.0, &[(0, 0.0), (1, 60.0)]),
                sample(102.0, 1.5, &[(0, 0.0), (1, 58.0)]),
            ],
        },
    ]);
    let svg = v.render_svg(800, 240, 1.0, &marks_now(&v));
    assert!(svg.starts_with("<svg"), "must be an svg root");
    assert!(svg.trim_end().ends_with("</svg>"), "must close the svg");
    assert!(svg.contains("host-a") && svg.contains("host-b"), "host labels present");
    // host-a: cpu+ram+gpu0 = 3 polylines; host-b: cpu+ram+gpu0+gpu1 = 4 => >=7.
    let polylines = svg.matches("<polyline").count();
    assert!(polylines >= 7, "expected >=7 polylines, got {polylines}");
}

#[test]
fn stale_tick_flags_host_without_appending() {
    let mut v = MonitorView::new();
    v.apply_tick(HostLatest {
        host: "host-a".into(),
        stale: false,
        sample: Some(sample(1.0, 10.0, &[])),
    });
    v.apply_tick(HostLatest {
        host: "host-a".into(),
        stale: true,
        sample: None,
    });
    let svg = v.render_svg(400, 100, 1.0, &marks_now(&v));
    // Stale marker " *" rides next to the host label (ADR 0020 §5).
    assert!(svg.contains("host-a *"), "stale host should be marked");
}

/// A host whose readings stop arriving is marked stale with the shown reading's age once no reading has come for
/// `GAP_S`: one that was live, and one prefilled from history whose last reading is an hour old. A new reading
/// clears the mark. The silence is a moved clock, so the test waits for nothing.
#[test]
fn a_reading_that_stops_arriving_is_marked_stale_with_its_age() {
    let (start, epoch) = clocks();
    let mut v = MonitorView::new();
    v.set_roster(vec!["live-a".into(), "hung-b".into()]);
    v.apply_tick(HostLatest { host: "live-a".into(), stale: false, sample: Some(sample(epoch, 10.0, &[])) });
    v.apply_history(vec![HostSeries {
        host: "hung-b".into(),
        step_s: 1.0,
        stale: false,
        samples: vec![sample(epoch - 3600.0, 20.0, &[])],
    }]);
    let label = |v: &MonitorView, (now, now_epoch): (Instant, f64), host: &str| {
        let svg = v.render_svg(800, 240, 1.0, &v.marks(now, now_epoch));
        let at = svg.find(&format!(">{host}")).map_or(0, |i| i + 1);
        svg[at..].split('<').next().unwrap_or("").to_string()
    };
    let silent = GAP_S + 0.5;
    let later = (start + std::time::Duration::from_secs_f64(silent), epoch + silent);
    let mut seen = vec![label(&v, (start, epoch), "live-a"), label(&v, (start, epoch), "hung-b")];
    seen.extend([label(&v, later, "live-a"), label(&v, later, "hung-b")]);
    v.apply_tick(HostLatest { host: "live-a".into(), stale: false, sample: Some(sample(later.1, 11.0, &[])) });
    seen.push(label(&v, clocks(), "live-a"));
    eprintln!("monitor-stale: {seen:?}");
    let right = seen[0] == "live-a"
        && seen[1] == "hung-b"
        && seen[2].starts_with("live-a * ")
        && seen[2].ends_with('s')
        && seen[3] == "hung-b * 1h"
        && seen[4] == "live-a";
    assert!(right, "fresh, after {GAP_S}s silent, after a new reading: {seen:?}");
}

#[test]
fn top_procs_render_and_survive_procless_samples() {
    let mut v = MonitorView::new();
    let mut s = sample(1.0, 50.0, &[]);
    s.top_procs = vec![
        sot_protocol::ProcSample {
            name: "julia".into(),
            user: "alice".into(),
            cpu_pct: 312.0,
        },
        sot_protocol::ProcSample {
            name: "nv(idia)".into(),
            user: "root".into(),
            cpu_pct: 3.0,
        },
    ];
    v.apply_tick(HostLatest {
        host: "host-a".into(),
        stale: false,
        sample: Some(s),
    });
    // A later sample without procs (e.g. a downsampled history refill)
    // must not blank the cached line.
    v.apply_tick(HostLatest {
        host: "host-a".into(),
        stale: false,
        sample: Some(sample(2.0, 51.0, &[])),
    });
    let svg = v.render_svg(800, 240, 1.0, &marks_now(&v));
    assert!(svg.contains("julia\u{b7}alice 312%"), "proc entry should render");
    assert!(svg.contains("nv(idia)\u{b7}root 3%"), "second proc should render");
}

#[test]
fn a_control_character_in_a_name_leaves_the_chart_drawable() {
    // A process name is the file name it ran from, which may hold any byte but `/` and NUL; usvg rejects the
    // whole chart for one character XML 1.0 forbids (a control character, U+FFFE, U+FFFF), so a name is drawn as
    // one row (`cell_row`) with those two as U+FFFD.
    let mut v = MonitorView::new();
    let mut s = sample(1.0, 50.0, &[]);
    s.top_procs = vec![
        sot_protocol::ProcSample {
            name: "AA\u{1}BB\nCC".into(),
            user: "alice".into(),
            cpu_pct: 3.0,
        },
        sot_protocol::ProcSample {
            name: "DD\u{fffe}EE\u{ffff}".into(),
            user: "bob".into(),
            cpu_pct: 2.0,
        },
    ];
    v.apply_tick(HostLatest {
        host: "host-a".into(),
        stale: false,
        sample: Some(s),
    });
    let svg = v.render_svg(800, 240, 1.0, &marks_now(&v));
    let parsed = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default());
    assert!(parsed.is_ok(), "the chart does not parse: {:?}", parsed.err());
    assert!(svg.contains("AA BB ⏎ CC\u{b7}alice 3%"), "{svg}");
    assert!(svg.contains("DD\u{fffd}EE\u{fffd}\u{b7}bob 2%"), "{svg}");
}

#[test]
fn empty_view_still_renders_a_frame() {
    let v = MonitorView::new();
    let svg = v.render_svg(200, 80, 1.0, &[]);
    assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
}
