use super::*;

#[test]
fn cpu_intervals_count_iowait_as_idle_and_invalidate_a_lost_baseline() {
    let mut previous = Some(parse_cpu_stat("cpu 0 0 0 0 0 0 0 0\ncpu0 0 0 0 0 0 0 0 0\n").unwrap());
    let measured = collect_cpu_breakdown_with(
        &mut previous,
        || parse_cpu_stat("cpu 200 50 100 500 50 50 25 25\ncpu0 200 50 100 500 50 50 25 25\n"),
        |_| panic!("warm sample must not prime"),
    );
    let usage = measured.measurement.unwrap();
    assert_eq!(
        (
            usage.busy_percent(),
            usage.user_percent(),
            usage.system_percent(),
            usage.idle_percent()
        ),
        (45, 25, 18, 55)
    );
    assert_eq!(measured.cores[0].percent, Some(45));
    let missing = collect_cpu_breakdown_with(&mut previous, || None, |_| {});
    assert!(missing.measurement.is_none());
    assert!(previous.is_none());
}

#[test]
fn reused_process_identity_never_inherits_a_previous_cpu_sample() {
    let target = ProcessTarget {
        pid: "4242".into(),
        start_time_ticks: 100,
        kernel_uid: Some(1000),
    };
    let now = Instant::now();
    let first = ProcessCpuSample {
        user_ticks: 100,
        system_ticks: 0,
        sampled_at: now,
    };
    let mut samples = ProcessCpuSamples::new();
    process_cpu_reading(
        &ProcessCpuSamples::new(),
        &mut samples,
        target.clone(),
        first,
        100.0,
        4,
    );
    let next = ProcessCpuSample {
        user_ticks: 120,
        sampled_at: now + Duration::from_secs(1),
        ..first
    };
    assert_eq!(
        process_cpu_reading(
            &samples,
            &mut ProcessCpuSamples::new(),
            target.clone(),
            next,
            100.0,
            4
        ),
        "5.0%"
    );
    assert_eq!(
        process_cpu_reading(
            &samples,
            &mut ProcessCpuSamples::new(),
            ProcessTarget {
                start_time_ticks: 200,
                ..target
            },
            next,
            100.0,
            4
        ),
        "—"
    );
}
