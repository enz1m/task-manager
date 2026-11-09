use super::*;

#[test]
fn disk_history_uses_interval_throughput_and_clears_after_missing_input() {
    let collector = Mutex::new(DiskIoCollector::default());
    let epoch = Instant::now();
    let counters =
        |read, write, busy| format!("8 1 fixture 10 0 {read} 0 20 0 {write} 0 0 {busy} 0\n");
    let sample = |stats: Option<String>, seconds| {
        disk_summary_with(
            &collector,
            || {
                (
                    Some("36 25 8:1 / / rw - ext4 /dev/fixture-root rw\n".into()),
                    stats,
                    epoch + Duration::from_secs(seconds),
                )
            },
            DiskCapacity::from_blocks(1024 * 1024, 100, 38, 30),
            |_| None,
        )
    };
    sample(Some(counters(10_000, 20_000, 1_000)), 0);
    let measured = sample(Some(counters(14_096, 22_048, 1_500)), 1);
    assert_eq!(measured.activity().unwrap().history_values(), (2048, 1024));
    assert_eq!(measured.capacity().unwrap().used_percent(), 62);
    let mut model = crate::model::Model::from_snapshot(Snapshot {
        resources: vec![measured.resource()],
        disk_reading: measured,
        ..Default::default()
    });
    model.select_resource("disk");
    assert!(!model.active_history().is_empty());
    let theme = enzui_theme::EnzTheme::dark();
    let chart = crate::view::performance::charts::disk_throughput_area_chart(
        model.active_history(),
        200.0,
        &theme,
    );
    let key = chart.key_for(&theme.iced_theme());
    let domain = key.y_domain();
    assert_eq!(domain.0, 0.0);
    assert!(
        domain.1 >= 2048.0,
        "rate samples must fit without rescaling"
    );
    let missing = sample(None, 2);
    model.apply_snapshot(Snapshot {
        resources: vec![missing.resource()],
        disk_reading: missing,
        ..Default::default()
    });
    assert!(model.active_history().is_empty());
    assert!(
        sample(Some(counters(20_000, 30_000, 2_000)), 3)
            .activity()
            .is_none()
    );
}
