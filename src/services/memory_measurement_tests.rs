use super::*;

#[test]
fn memory_distinguishes_measured_zero_disabled_swap_and_unavailable_ram() {
    let zero = MemoryReading::from_meminfo(Some(
        "MemTotal: 100 kB\nMemAvailable: 100 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
    ));
    assert_eq!(zero.ram().unwrap().used_bytes(), 0);
    assert_eq!(zero.swap().unwrap().used_percent(), 0);
    for text in [
        None,
        Some("MemTotal: 100 kB\nMemAvailable: 101 kB\n"),
        Some("MemTotal: 0 kB\nMemAvailable: 0 kB\n"),
    ] {
        assert!(MemoryReading::from_meminfo(text).ram().is_none());
    }
}
