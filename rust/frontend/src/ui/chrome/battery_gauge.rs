//! The battery readout's cache: the label and when the OS was last asked.

/// How often to hit the OS for a fresh battery reading.
const BATTERY_QUERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
pub(in crate::ui) struct BatteryGauge {
    /// Cached battery readout for the top-right chrome (e.g. `85%` or `+72%`
    /// while charging). `None` means no battery present / query failed — we
    /// render nothing in that case (never a fake `0%`). The OS query is not
    /// free, so it's refreshed at most once per `BATTERY_QUERY_INTERVAL`; the
    /// clock ticking every second reuses this cached value in between.
    label: Option<String>,
    /// When the cached label was last (re)computed. `None` forces a
    /// query on the first paint.
    last_query: Option<std::time::Instant>,
}

impl BatteryGauge {
    /// The label, refreshed first if it is stale (or never queried).
    /// The OS query (via the cross-platform `battery` crate) is not free, so
    /// it runs at most once per `BATTERY_QUERY_INTERVAL`; between refreshes the
    /// per-second clock repaint reuses the cached value. On no battery present
    /// (desktop / CI) or any query error the label is `None` so the
    /// chrome paints nothing for the battery — never a fake `0%` or `N/A`.
    pub(in crate::ui) fn label(&mut self) -> Option<String> {
        let now = std::time::Instant::now();
        let fresh = self
            .last_query
            .map(|t| now.duration_since(t) < BATTERY_QUERY_INTERVAL)
            .unwrap_or(false);
        if !fresh {
            self.last_query = Some(now);
            self.label = super::status::query_battery_label();
        }
        self.label.clone()
    }
}
