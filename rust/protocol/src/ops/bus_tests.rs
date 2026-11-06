use super::*;

#[test]
fn bus_lost_keeps_its_shape_and_a_missing_count_reads_as_zero() {
    let lost = BusLostEvt { bus: op::PREVIEW_CHANGED.to_string(), skipped: 3 };
    assert_eq!(serde_json::to_value(&lost).unwrap(), serde_json::json!({"bus": "preview.changed", "skipped": 3}));
    let bare: BusLostEvt = serde_json::from_value(serde_json::json!({"bus": "monitor.tick"})).unwrap();
    assert_eq!(bare, BusLostEvt { bus: "monitor.tick".to_string(), skipped: 0 });
}
