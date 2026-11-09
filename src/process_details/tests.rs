use super::*;
use crate::App;
use crate::collection::{Collected, Event, SourceKind, Stamp, Terminal};
use crate::message::Message;
use crate::model::{Model, Page};
use crate::services::{ProcessData, SampleScope, Snapshot};
use enzui_test_support::test_scratch::ScratchDir;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

struct Fixture {
    root: ScratchDir,
}

impl Fixture {
    fn new() -> Self {
        let root = ScratchDir::new_restricted("tm-process-details");
        let fixture = Self { root };
        std::fs::create_dir_all(fixture.pid_dir().join("task/424242")).unwrap();
        std::fs::create_dir(fixture.pid_dir().join("fd")).unwrap();
        std::fs::write(fixture.pid_dir().join("task/424242/comm"), "worker\n").unwrap();
        fixture.identity(100, 1000);
        fixture.cwd("/fixture/original");
        fixture
    }

    fn pid_dir(&self) -> PathBuf {
        self.root.join("424242")
    }

    fn identity(&self, start: u64, uid: u32) {
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = "S".into();
        fields[19] = start.to_string();
        std::fs::write(
            self.pid_dir().join("stat"),
            format!("424242 (fixture) {}\n", fields.join(" ")),
        )
        .unwrap();
        std::fs::write(
            self.pid_dir().join("status"),
            format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .unwrap();
    }

    fn cwd(&self, value: &str) {
        let path = self.pid_dir().join("cwd");
        let _ = std::fs::remove_file(&path);
        std::os::unix::fs::symlink(value, path).unwrap();
    }
}

fn row(start_time_ticks: u64, uid: u32) -> ProcessData {
    ProcessData {
        pid: "424242".into(),
        start_time_ticks,
        kernel_uid: Some(uid),
        name: "fixture".into(),
        kind: "System".into(),
        ..Default::default()
    }
}

fn snapshot(processes: Vec<ProcessData>) -> Snapshot {
    Snapshot {
        processes,
        ..Default::default()
    }
}

fn app(collect: Arc<Collector>) -> App {
    let mut app = crate::tests::app_on_page(Page::Processes);
    app.model = Model::from_snapshot(snapshot(vec![row(100, 1000)]));
    app.model.select_page(Page::Processes);
    app.model.process_details.collect = collect;
    let mut owner = crate::collection::Collections::with_collector(Arc::new(|_, _, _| {
        Err("fixture has no further snapshot".into())
    }));
    owner.painted = true;
    app.model.collections = Some(owner);
    app
}

fn replace_snapshot(app: &mut App, rows: Vec<ProcessData>) {
    let intent = app
        .model
        .collections
        .as_mut()
        .unwrap()
        .request(SourceKind::Refresh, SampleScope::ALL)
        .unwrap();
    app.apply_collection_event(Event::Snapshot(
        intent,
        Stamp {
            source: SourceKind::Refresh,
            generation: intent.generation,
            captured_at: Instant::now(),
            terminal: Terminal::Current,
        },
        Ok(Collected::fixture(
            snapshot(rows),
            intent.scope,
            intent.generation,
        )),
    ));
}

fn settle(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let _ = app.update(Message::CollectionWake);
        if app.model.process_details.reading.is_some() && app.model.process_details.flight.is_none()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "detail route did not settle: {:?}",
            app.model.process_details
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn pid_replacement_stale_terminal_is_inert() {
    let fixture = Arc::new(Fixture::new());
    let (entered, received) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let released = Mutex::new(released);
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_fixture = Arc::clone(&fixture);
    let worker_calls = Arc::clone(&calls);
    let mut app = app(Arc::new(move |target, token| {
        worker_calls.fetch_add(1, Ordering::Relaxed);
        let reading = procfs::collect_in(worker_fixture.root.path(), target, token);
        if target.start_time_ticks == 100 {
            entered.send(reading.clone()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .expect("bounded release");
        }
        reading
    }));
    let _ = app.update(Message::SelectProcess(row(100, 1000).target()));
    let old_reading = received
        .recv_timeout(Duration::from_secs(2))
        .expect("detail worker entered");
    assert_eq!(old_reading.cwd, Ok("/fixture/original".into()));
    let old = app.model.process_details.current.clone().unwrap();
    fixture.identity(200, 2000);
    fixture.cwd("/fixture/replacement");
    replace_snapshot(&mut app, vec![row(200, 2000)]);
    assert!(app.model.selected_pid.is_empty());
    assert!(app.model.process_details.current.is_none());
    let _ = app.update(Message::SelectProcess(row(200, 2000).target()));
    let latest = app.model.process_details.current.clone().unwrap();
    assert_ne!(latest.generation, old.generation);
    app.model.process_details.publish(&old, old_reading);
    assert!(app.model.process_details.reading.is_none());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(app.model.process_details.pending, Some(latest));
    release.send(()).unwrap();
    settle(&mut app);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        app.model.process_details.reading.as_ref().unwrap().cwd,
        Ok("/fixture/replacement".into())
    );
}

#[test]
fn procfs_details_refuse_an_owner_change_during_collection() {
    let fixture = Fixture::new();
    let reading = procfs::collect_observed(
        fixture.root.path(),
        &row(100, 1000).target(),
        &CancelToken::new(),
        |phase| {
            if phase == procfs::Census::Files {
                fixture.identity(100, 2000);
            }
        },
    );
    assert_eq!(reading, Reading::failed(Failure::IdentityChanged));
}
