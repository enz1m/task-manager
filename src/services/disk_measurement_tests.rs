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
            |_| DiskCapacity::from_blocks(1024 * 1024, 100, 38, 30),
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

#[test]
fn sandboxed_root_measures_the_host_root_bind_mount() {
    let mountinfo = "\
1165 920 0:159 /newroot / rw,nosuid,nodev,relatime - tmpfs tmpfs rw,uid=1000
1735 1165 0:36 /root/etc/os-release /run/host/os-release ro,nosuid - btrfs /dev/mapper/crypt rw
";
    let stats = |read, write| format!("253 0 dm-0 10 0 {read} 0 20 0 {write} 0 0 500 0\n");
    let collector = Mutex::new(DiskIoCollector::default());
    let epoch = Instant::now();
    let mut capacity_paths = Vec::new();
    let mut sample = |text: String, seconds| {
        disk_summary_with(
            &collector,
            || {
                (
                    Some(mountinfo.to_owned()),
                    Some(text),
                    epoch + Duration::from_secs(seconds),
                )
            },
            |path| {
                capacity_paths.push(path.to_owned());
                DiskCapacity::from_blocks(4096, 100, 50, 50)
            },
            |device| (device == "/dev/mapper/crypt").then_some((253, 0)),
        )
    };
    sample(stats(0, 0), 0);
    let measured = sample(stats(2048, 4096), 1);
    assert_eq!(measured.activity().unwrap().history_values(), (1024, 2048));
    assert_eq!(measured.device(), Some("/dev/mapper/crypt"));
    assert_eq!(measured.filesystem(), Some("btrfs"));
    assert_eq!(measured.mount(), Some("/"));
    assert_eq!(capacity_paths, ["/run/host/os-release"; 2]);
}

#[test]
fn sysfs_resolves_device_mapper_and_plain_block_sources() {
    let class = std::env::temp_dir().join(format!("tm-sysfs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&class);
    fs::create_dir_all(class.join("dm-0/dm")).unwrap();
    fs::write(class.join("dm-0/dm/name"), "crypt\n").unwrap();
    fs::write(class.join("dm-0/dev"), "253:0\n").unwrap();
    fs::create_dir_all(class.join("nvme0n1p2")).unwrap();
    fs::write(class.join("nvme0n1p2/dev"), "259:2\n").unwrap();
    assert_eq!(
        sysfs_block_major_minor(&class, "/dev/mapper/crypt"),
        Some((253, 0))
    );
    assert_eq!(
        sysfs_block_major_minor(&class, "/dev/nvme0n1p2"),
        Some((259, 2))
    );
    assert_eq!(sysfs_block_major_minor(&class, "/dev/mapper/missing"), None);
    assert_eq!(sysfs_block_major_minor(&class, "/dev/disk/by-uuid/x"), None);
    fs::remove_dir_all(&class).unwrap();
}
