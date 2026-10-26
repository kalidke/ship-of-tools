//! The Monitor drawer's coordinator: the subscription's lifecycle, the monitor replies and the chart's layout.

use crate::ui::*;

impl State {
    /// The toggle's half of the Monitor subscription's lifecycle (ADR 0020): a drawer toggled to the Monitor takes the
    /// subscription, and one toggled to other content or closed releases it. Help, and a REPL run that takes the slot
    /// without the toggle, keep it. Sampling is always on; the subscription gates only this connection's ticks.
    pub(in crate::ui) fn sync_monitor_subscription(&mut self) {
        let shown = self.layout_help.drawer() == DrawerContent::Monitor;
        if shown && !self.monitor.subscribed() {
            self.subscribe_monitor();
        } else if !shown && self.monitor.subscribed() {
            self.unsubscribe_monitor();
        }
    }

    /// `--start-monitor`: the drawer opens at construction holding its subscription, which the monitor host's first
    /// Connected sends (`resubscribe_monitor`), as every later one does.
    pub(in crate::ui) fn hold_monitor_subscription(&mut self) {
        self.monitor.hold_subscription();
    }

    /// Subscribe to the monitor host's ticks and prefill the chart from its history (ADR 0020). ADR 0042 L2a: always
    /// `monitor_host`, the declared hub; the drawer never follows `active_host`.
    fn subscribe_monitor(&mut self) {
        let monitor_host = self.monitor_host();
        let _ = self.send_to(&monitor_host, OutgoingReq::MonitorSubscribe);
        let history = OutgoingReq::MonitorHistory { window_s: 300.0, points: 300, until: None, host: None };
        let _ = self.send_to(&monitor_host, history);
        self.monitor.hold_subscription();
    }

    /// Release the subscription: the monitor host stops sending this connection ticks.
    fn unsubscribe_monitor(&mut self) {
        let monitor_host = self.monitor_host();
        let _ = self.send_to(&monitor_host, OutgoingReq::MonitorUnsubscribe);
        self.monitor.release_subscription();
    }

    /// The daemon starts every connection unsubscribed, so a new connection to the monitor host is sent the
    /// subscription this window holds; without it the drawer would get no reading until it was reopened.
    pub(in crate::ui) fn resubscribe_monitor(&mut self, connected: &HostKey) {
        if self.monitor.subscribed() && *connected == self.monitor_host() {
            self.subscribe_monitor();
        }
    }

    pub(crate) fn on_monitor_subscribed(&mut self, hosts: Vec<String>) {
        self.monitor.set_roster(hosts);
        self.window.request_redraw();
    }

    pub(crate) fn on_monitor_history(&mut self, hosts: Vec<sot_protocol::HostSeries>) {
        self.monitor.apply_history(hosts);
        self.window.request_redraw();
    }

    pub(crate) fn on_monitor_tick(&mut self, hosts: Vec<sot_protocol::HostLatest>) {
        self.monitor.apply_ticks(hosts);
        self.window.request_redraw();
    }

    pub(in crate::ui) fn layout_drawer_px(
        &mut self,
        cells_to_px: impl Fn(ratatui::layout::Rect) -> ScreenRect,
        repl_scrollback_cells: ratatui::layout::Rect,
        repl_window: (usize, usize),
        repl_cells: ratatui::layout::Rect,
    ) {
        // Ctrl+M monitor drawer (ADR 0020): the chart shares the drawer rect.
        // Regenerate the SVG → wgpu quad whenever the readings changed
        // (the pane's dirty mark) or the chart on screen was drawn for another size,
        // text scale or set of marks (`MonitorView::drawn`, `None` once the
        // drawer shows another tenant), mirroring the math-SVG rasterise path.
        self.repl_presentation.commit_layout(cells_to_px(repl_scrollback_cells), repl_window);
        // Scale the chart's text + gutters to match the chrome's
        // effective text size (cell_h is BASE_CELL_H * scale), so the
        // SVG's logical-px labels aren't tiny on a hi-DPI window.
        let mon_scale = (self.cell_h / BASE_CELL_H) as f64;
        let shown = self.layout_help.drawer() == DrawerContent::Monitor;
        if let Some(draw) = self.monitor.plan(cells_to_px(repl_cells), shown, mon_scale) {
            let quad = quad_from_svg_bytes(
                self.gpu.device(),
                self.gpu.queue(),
                &self.quad_pipeline,
                draw.svg.as_bytes(),
                draw.w,
                draw.h,
            )
            .ok();
            self.monitor.install(draw, quad);
        }
    }
}

/// The Monitor's native State fixture: a `State` exists only inside a native window, so it runs in the
/// window_minimized target's fixtures child (`ui/app/native_children.rs`).
#[cfg(all(test, feature = "test-window-progress"))]
#[path = "monitor_native_fixture_tests.rs"]
mod native_fixture;
