use super::*;
use enzui_test_support::test_scratch::ScratchDir;
struct Fixture {
    root: ScratchDir,
    epoch: Instant,
}
impl Fixture {
    fn new() -> Self {
        let root = ScratchDir::new_restricted("tm-network");
        fs::create_dir(root.join("net")).unwrap();
        Self {
            root,
            epoch: Instant::now(),
        }
    }
    fn net(&self) -> PathBuf {
        self.root.join("net")
    }
    fn iface(&self, name: &str, ifindex: u32) -> PathBuf {
        let path = self.net().join(name);
        fs::create_dir_all(path.join("statistics")).unwrap();
        write(&path.join("ifindex"), &ifindex.to_string());
        write(&path.join("operstate"), "up");
        write(&path.join("speed"), "10");
        self.counters(name, 0, 0);
        path
    }
    fn counters(&self, name: &str, rx: u64, tx: u64) {
        write(
            &self.net().join(name).join("statistics/rx_bytes"),
            &rx.to_string(),
        );
        write(
            &self.net().join(name).join("statistics/tx_bytes"),
            &tx.to_string(),
        );
    }
    fn sample(&self, collector: &mut NetworkCollector, elapsed: Duration) -> Vec<NetworkReading> {
        collector.sample(network::read_batch_in(&self.net(), || self.epoch + elapsed))
    }
    fn at(&self, collector: &mut NetworkCollector, second: u64) -> Vec<NetworkReading> {
        self.sample(collector, Duration::from_secs(second))
    }
}
fn write(path: &Path, value: &str) {
    fs::write(path, value).unwrap();
}

#[test]
fn network_resets_and_interface_reuse_require_fresh_counter_pairs() {
    let fixture = Fixture::new();
    fixture.iface("eth0", 1);
    fixture.counters("eth0", 10_000, 20_000);
    let mut collector = NetworkCollector::default();
    fixture.at(&mut collector, 0);
    fixture.counters("eth0", 135_000, 82_500);
    let measured = fixture.at(&mut collector, 1);
    assert_eq!(
        (measured[0].rx_rate_kbps(), measured[0].tx_rate_kbps()),
        (Some(1000), Some(500))
    );
    fixture.counters("eth0", 3, 145_000);
    let reset = fixture.at(&mut collector, 2);
    assert_eq!(
        (reset[0].rx_rate_kbps(), reset[0].tx_rate_kbps()),
        (None, Some(500))
    );
    fs::remove_dir_all(fixture.net().join("eth0")).unwrap();
    let missing = fixture.at(&mut collector, 3);
    assert!(
        missing
            .iter()
            .all(|reading| reading.identity().is_none() && reading.combined_kbps().is_none())
    );
    fixture.iface("eth0", 2);
    fixture.counters("eth0", 9_000_000, 8_000_000);
    assert!(fixture.at(&mut collector, 4)[0].rx_rate_kbps().is_none());
}
