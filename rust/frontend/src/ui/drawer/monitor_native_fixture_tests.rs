use super::*;
use crate::net::transport::{IncomingEvt, ResolvedDial};

impl State {
    /// A new connection to the monitor host is sent the subscription this window holds, with its history prefill:
    /// with the drawer open, and with Help shown over it. No other host's connection is sent one, and the hub's
    /// is sent none once the drawer has closed.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_held_monitor_subscription_is_sent_on_each_new_connection(&mut self) -> Result<()> {
        let (hub, other) = ("<monitor-hub>".to_string(), "<host-other>".to_string());
        let (hub_tx, mut hub_rx) = tokio::sync::mpsc::unbounded_channel();
        let (other_tx, mut other_rx) = tokio::sync::mpsc::unbounded_channel();
        // The hub sorts second: only the declared hub, not the first connection, makes it the monitor host.
        self.conns = vec![(other.clone(), other_tx), (hub.clone(), hub_tx)];
        self.monitor_hub = Some(hub.clone());
        self.active_host = other.clone();
        let (events, receiver) = std::sync::mpsc::channel();
        self.evt_rx = receiver;
        let rows = [
            ("open, the hub connects", DrawerContent::Monitor, true, &hub),
            ("help over it, the hub connects", DrawerContent::Help, true, &hub),
            ("open, another host connects", DrawerContent::Monitor, true, &other),
            ("closed, the hub connects", DrawerContent::Closed, false, &hub),
        ];
        let mut seen = Vec::new();
        for (label, drawer, held, host) in rows {
            self.drawer = drawer;
            self.monitor_view.subscribed = held;
            let connected = IncomingEvt::Connected {
                session_id: "session".into(),
                revision: 0,
                host: None,
                project_root: None,
                proxy: false,
                resolved: ResolvedDial::Local,
                backend_version: "test".into(),
                repl_running: Vec::new(),
            };
            events
                .send((host.clone(), connected))
                .map_err(|_| anyhow::anyhow!("the fixture's event channel closed"))?;
            self.drain_events();
            seen.push((label, monitor_ops(&mut hub_rx), monitor_ops(&mut other_rx)));
        }
        self.drawer = DrawerContent::Closed;
        self.monitor_view.subscribed = false;
        self.monitor_hub = None;
        println!("monitor-resubscribe: {seen:?}");
        let resent = |i: usize| seen[i].1 == ["subscribe", "history"];
        let right = resent(0)
            && resent(1)
            && seen[2].1.is_empty()
            && seen[3].1.is_empty()
            && seen.iter().all(|(_, _, to_other)| to_other.is_empty());
        anyhow::ensure!(right, "(row, monitor ops to the hub, to another host): {seen:?}");
        Ok(())
    }

    /// The chart on screen is redrawn whenever what it was drawn for changes with no new reading, as when the
    /// hub has stalled: on a resize, and on the drawer's return to the Monitor from Help.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_monitor_chart_is_redrawn_on_resize_and_on_return(&mut self) -> Result<()> {
        let (_, epoch) = clocks();
        let reading = MonitorSample {
            ts: epoch - 3630.0,
            cpu_pct: 10.0,
            ram_pct: 10.0,
            cpu_cores: None,
            ram_total_gb: None,
            gpus: Vec::new(),
            top_procs: Vec::new(),
        };
        self.monitor_view = MonitorView::new();
        let series = HostSeries { host: "gpu-box".into(), step_s: 1.0, stale: true, samples: vec![reading] };
        self.monitor_view.apply_history(vec![series]);
        let rect = |w: f32, h: f32| move |_: ratatui::layout::Rect| ScreenRect { x: 0.0, y: 0.0, w, h };
        let cells = ratatui::layout::Rect::default();
        let rows = [
            ("drawn", DrawerContent::Monitor, (640.0, 240.0)),
            ("resized", DrawerContent::Monitor, (800.0, 300.0)),
            ("help over it", DrawerContent::Help, (800.0, 300.0)),
            ("back from help", DrawerContent::Monitor, (800.0, 300.0)),
        ];
        let mut seen = Vec::new();
        for (label, drawer, (w, h)) in rows {
            self.drawer = drawer;
            self.layout_drawer_px(rect(w, h), cells, (0, 0), cells);
            seen.push((label, self.monitor_quad.as_ref().map(|q| q.size_px)));
        }
        self.drawer = DrawerContent::Closed;
        self.monitor_quad = None;
        self.monitor_view = MonitorView::new();
        println!("monitor-redraw: {seen:?}");
        let right = seen[0].1 == Some((640, 240))
            && seen[1].1 == Some((800, 300))
            && seen[2].1.is_none()
            && seen[3].1 == Some((800, 300));
        anyhow::ensure!(right, "(row, the chart's size on screen): {seen:?}");
        Ok(())
    }
}

/// The monitor requests queued on one connection's channel, in order.
fn monitor_ops(rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>) -> Vec<&'static str> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|req| match req {
            OutgoingReq::MonitorSubscribe => Some("subscribe"),
            OutgoingReq::MonitorHistory { .. } => Some("history"),
            OutgoingReq::MonitorUnsubscribe => Some("unsubscribe"),
            _ => None,
        })
        .collect()
}
