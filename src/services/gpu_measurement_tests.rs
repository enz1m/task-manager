use super::*;
use enzui_test_support::test_scratch::ScratchDir;
use std::os::unix::fs::symlink;

#[test]
fn gpu_unknown_does_not_graph_a_clock_proxy_or_previous_utilization() {
    let root = ScratchDir::new_restricted("tm-gpu");
    let device = root.join("device");
    let drm = root.join("drm");
    fs::create_dir_all(drm.join("card0")).unwrap();
    fs::create_dir(&device).unwrap();
    symlink(&device, drm.join("card0/device")).unwrap();
    fs::write(device.join("vendor"), "0x1002").unwrap();
    fs::write(device.join("gpu_busy_percent"), "45").unwrap();
    let mut collector = gpu::GpuCollector::default();
    let clock = device.join("drm/card0");
    fs::create_dir_all(&clock).unwrap();
    fs::write(clock.join("gt_cur_freq_mhz"), "500").unwrap();
    fs::write(clock.join("gt_max_freq_mhz"), "1000").unwrap();
    let measured = collector.collect_in(&drm);
    assert_eq!(measured.busy().unwrap().percent(), 45);
    let mut model = crate::model::Model::from_snapshot(Snapshot {
        resources: vec![measured.resource()],
        gpu_reading: measured,
        ..Default::default()
    });
    model.select_resource("gpu");
    assert!(!model.active_history().is_empty());
    fs::write(device.join("gpu_busy_percent"), "101").unwrap();
    let unknown = collector.collect_in(&drm);
    assert!(unknown.busy().is_none());
    model.apply_snapshot(Snapshot {
        resources: vec![unknown.resource()],
        gpu_reading: unknown,
        ..Default::default()
    });
    assert!(model.active_history().is_empty());
    fs::remove_file(device.join("gpu_busy_percent")).unwrap();
    assert!(collector.collect_in(&drm).busy().is_none());
}
