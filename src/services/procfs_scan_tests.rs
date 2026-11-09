//! The process scan reads `/proc` directly.

use super::*;
use enzui_core::cancellation::CancelToken;
use enzui_test_support::test_scratch::ScratchDir;

fn fixture() -> ScratchDir {
    let root = ScratchDir::new("tm-proc");
    let write = |path: &str, contents: &[u8]| {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    };
    write("meminfo", b"MemTotal:       16000000 kB\nMemFree: 1 kB\n");
    write("uptime", b"10000.00 20000.00\n");
    write(
        "42/stat",
        b"42 (indexer) S 1 42 42 0 -1 0 0 0 0 0 30000 20000 0 0 20 0 3 0 100 0 0",
    );
    write(
        "42/status",
        b"Name:\tindexer\nState:\tS (sleeping)\nUid:\t0\t0\t0\t0\nVmRSS:\t  160000 kB\nThreads:\t3\n",
    );
    write("42/cmdline", b"/usr/bin/indexer\0--name\0two  spaces\0");
    write(
        "7/stat",
        b"7 (kworker/0:1-events) I 2 0 0 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 200 0 0",
    );
    write(
        "7/status",
        b"Name:\tkworker/0:1-events\nState:\tI (idle)\nUid:\t0\t0\t0\t0\nThreads:\t1\n",
    );
    write("7/cmdline", b"");
    write("self/stat", b"not a pid");
    root
}

#[test]
fn procfs_scan_retains_identity_and_opaque_command_lines() {
    let root = fixture();
    let (rows, fabricated) = collect_processes_in(root.path(), &CancelToken::new());

    assert!(!fabricated);
    assert_eq!(rows.len(), 2);
    let indexer = rows.iter().find(|row| row.pid == "42").unwrap();
    assert_eq!(indexer.command_line, "/usr/bin/indexer --name two  spaces");
    assert_eq!(indexer.state, "Sleeping");
    assert_eq!(indexer.start_time_ticks, 100);
    assert_eq!(indexer.kernel_uid, Some(0));

    let worker = rows.iter().find(|row| row.pid == "7").unwrap();
    assert_eq!(worker.command_line, "[kworker/0:1-events]");
    assert_eq!(worker.state, "Idle");
}
