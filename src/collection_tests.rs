use super::*;
use crate::collection::{Collected, Collections, Phase, SourceKind};
use enzui_core::cancellation::CancelToken;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(3);
struct Release(mpsc::Sender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
fn gate() -> (Release, Arc<Mutex<mpsc::Receiver<()>>>) {
    let (tx, rx) = mpsc::channel();
    (Release(tx), Arc::new(Mutex::new(rx)))
}
struct EmptyFacts;
impl DeviceFactsSource for EmptyFacts {
    fn prepare(&self) -> device_facts::DeviceFactsOp {
        device_facts::DeviceFactsOp::new(Default::default)
    }
}
fn captured(scope: services::SampleScope, generation: u64) -> Collected {
    Collected::fixture(
        services::Snapshot {
            resources: vec![services::ResourceData {
                id: "cpu".to_owned(),
                name: "CPU".to_owned(),
                value: "Unavailable".to_owned(),
                ..Default::default()
            }],
            processes: vec![services::ProcessData {
                pid: generation.to_string(),
                name: "fixture-process".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        },
        scope,
        generation,
    )
}
fn app_with(
    collect: impl Fn(services::SampleScope, u64, &CancelToken) -> Result<Collected, String>
    + Send
    + Sync
    + 'static,
) -> App {
    let mut model = Model::new();
    model.select_page(Page::Processes);
    model.collections = Some(Collections::with_collector(Arc::new(collect)));
    App {
        shell_activity: Default::default(),
        lifecycle: None,
        finish_then_exit: false,
        model,
        themes: test_theme_integration(EnzTheme::dark()),
        nav: enzui_widgets::nav_split::NavSplitState::new(),
        desktop: enzui_widgets::single_instance::SingleInstance::standalone(),
        device_facts_source: Arc::new(EmptyFacts),
        session: session::store(None),
        #[cfg(feature = "center")]
        center: None,
    }
}
fn frames(app: &mut App) {
    let _ = app.reduce(Message::CollectionFirstFrame);
    let _ = app.reduce(Message::CollectionFirstFrame);
}
fn drive(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let _ = app.reduce(Message::CollectionWake);
        if done(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "collection did not settle within fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn closing_retains_the_loaded_page_with_a_worker_veto() {
    let mut app = app_with(|scope, generation, _| Ok(captured(scope, generation)));
    app.model.select_page(Page::Performance);
    app.request_collection(SourceKind::Boot);
    frames(&mut app);
    drive(&mut app, |app| {
        app.model.collection_health.phase(SourceKind::Telemetry) == Phase::Current
    });

    let _ = app.request_close(iced::window::Id::unique());
    assert_eq!(
        app.model.collection_health.phase(SourceKind::Telemetry),
        Phase::Current
    );
    app.model.collection_health.close_notice = Some("Collection worker did not exit".to_owned());
    assert!(view::collection::blocking_page(&app.model).is_none());
}

#[test]
fn refresh_storm_publishes_only_the_latest_requested_generation() {
    let (release, wait) = gate();
    let (entered, seen) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let mut app = app_with(move |scope, generation, _| {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            entered.send(()).unwrap();
            wait.lock().unwrap().recv_timeout(WAIT).unwrap();
        }
        Ok(captured(scope, generation))
    });
    app.request_collection(SourceKind::Boot);
    frames(&mut app);
    seen.recv_timeout(WAIT).unwrap();
    for _ in 0..64 {
        let _ = app.reduce(Message::Refresh);
    }
    let latest = app.model.collections.as_ref().unwrap().latest_generation();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.0.send(()).unwrap();
    drive(&mut app, |app| {
        app.model
            .snapshot()
            .processes
            .first()
            .is_some_and(|process| process.pid == latest.to_string())
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn navigation_and_minimize_cancel_without_publishing_late_data() {
    let (release, wait) = gate();
    let (entered, seen) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&count);
    let mut app = app_with(move |scope, generation, token| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            entered.send(token.share()).unwrap();
            wait.lock().unwrap().recv_timeout(WAIT).unwrap();
        }
        Ok(captured(scope, generation))
    });
    app.request_collection(SourceKind::Boot);
    frames(&mut app);
    let token = seen.recv_timeout(WAIT).unwrap();
    let _ = app.reduce(Message::SelectPage(Page::Services));
    let _ = app.reduce(Message::Window(
        enzui_widgets::title_bar_gesture::WindowCommand::Minimize,
    ));
    assert!(token.is_terminal());
    let _ = app.reduce(Message::CollectionWake);
    assert_eq!(app.model.collections.as_ref().unwrap().live(), 1);
    assert!(app.model.snapshot().processes.is_empty());
    release.0.send(()).unwrap();
    drive(&mut app, |app| {
        app.model.collections.as_ref().unwrap().live() == 0
    });
    assert!(app.model.snapshot().processes.is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let _ = app.reduce(Message::WindowFocusChanged(true));
    drive(&mut app, |app| {
        app.model.collection_health.phase(SourceKind::Services) == Phase::Empty
    });
}

#[test]
fn unfocused_performance_keeps_sampling_and_resumes_motion_on_focus() {
    let mut app = app_with(|scope, generation, _| {
        let mut sample = captured(scope, generation);
        sample.snapshot.cpu_breakdown.measurement = services::CpuUsage::from_samples(
            &services::CpuTimes::default(),
            &services::CpuTimes {
                user: generation,
                idle: 100 - generation,
                ..Default::default()
            },
        );
        Ok(sample)
    });
    app.model.select_page(Page::Performance);
    frames(&mut app);
    let _ = app.reduce(Message::WindowFocusChanged(true));
    assert!(app.model.animate_samples());
    drive(&mut app, |app| {
        app.model.collections.as_ref().unwrap().is_idle()
    });
    let previous = app.model.active_history().last().unwrap().clone();

    let _ = app.reduce(Message::WindowFocusChanged(false));
    assert!(!app.model.animate_samples());
    let _ = app.reduce(Message::CollectionTick);
    drive(&mut app, |app| {
        app.model.active_history().last().unwrap().at > previous.at
    });
    assert!(app.model.active_history().last().unwrap().user > previous.user);

    let _ = app.reduce(Message::WindowFocusChanged(true));
    let _ = app.reduce(Message::WindowMinimized(Some(true)));
    assert!(app.model.animate_samples());
}
