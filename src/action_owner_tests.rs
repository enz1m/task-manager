use super::*;
use crate::actions::{Actions, State};
use crate::services::{PinnedProcess, ProcessData, ProcessSignalBackend, Signal, SignalFailure};
use enzui_core::modal::DismissCause;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(4);
const PID: &str = "424242";
struct Fake {
    start: u64,
    sends: AtomicUsize,
    sample: AtomicUsize,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: mpsc::Sender<()>,
}
impl ProcessSignalBackend for Arc<Fake> {
    fn pin(&self, pid: u32) -> Result<Box<dyn PinnedProcess>, SignalFailure> {
        assert_eq!(pid.to_string(), PID);
        Ok(Box::new(Arc::clone(self)))
    }
}
impl PinnedProcess for Arc<Fake> {
    fn start_time_ticks(&self) -> Result<u64, SignalFailure> {
        Ok(self.start)
    }
    fn uid(&self) -> Result<u32, SignalFailure> {
        Ok(rustix::process::geteuid().as_raw())
    }
    fn send_signal(&self, _: Signal) -> Result<(), SignalFailure> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn has_exited(&self) -> Result<bool, SignalFailure> {
        let gate = self
            .gate
            .lock()
            .map_err(|_| SignalFailure::IdentityUnverifiable)?
            .take();
        if let Some(gate) = gate {
            let _ = self.entered.send(());
            let _ = gate.recv_timeout(Duration::from_secs(12));
        }
        match self.sample.load(Ordering::SeqCst) {
            1 => Ok(false),
            2 => Ok(true),
            _ => Err(SignalFailure::IdentityUnverifiable),
        }
    }
}
struct Rig {
    release: Option<mpsc::Sender<()>>,
    app: App,
    fake: Arc<Fake>,
    entered: mpsc::Receiver<()>,
}
fn row() -> ProcessData {
    ProcessData {
        pid: PID.into(),
        start_time_ticks: 100,
        kernel_uid: Some(rustix::process::geteuid().as_raw()),
        name: "fixture".into(),
        action_allowed: true,
        ..Default::default()
    }
}
fn rig(start: u64, sample: usize, hold: bool) -> Rig {
    let (entered, observed) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let fake = Arc::new(Fake {
        start,
        sends: AtomicUsize::new(0),
        sample: AtomicUsize::new(sample),
        gate: Mutex::new(hold.then_some(gate)),
        entered,
    });
    let mut app = tests::app_on_page(Page::Processes);
    app.model = Model::from_snapshot(services::Snapshot {
        processes: vec![row()],
        ..Default::default()
    });
    app.model.select_page(Page::Processes);
    app.model.collections = Some(collection::Collections::with_collector(Arc::new(
        |_, _, _| Err("unexpected collection".into()),
    )));
    app.model.collections.as_mut().unwrap().painted = true;
    app.model.actions = Actions::with_backend(Arc::new(Arc::clone(&fake)));
    Rig {
        release: Some(release),
        app,
        fake,
        entered: observed,
    }
}
fn request(app: &mut App) -> u64 {
    drop(app.update(Message::EndSelectedProcess));
    app.model
        .actions
        .confirmation
        .as_ref()
        .expect("confirmation routed")
        .generation
}
fn confirm(app: &mut App, generation: u64) {
    drop(app.update(Message::ActionConfirmEvent(
        generation,
        ConfirmDialogEvent::Resolved(DismissCause::DefaultAction),
    )));
}
fn drive(app: &mut App, done: impl Fn(&App) -> bool) {
    let until = Instant::now() + WAIT;
    while !done(app) {
        drop(app.update(Message::CollectionWake));
        assert!(Instant::now() < until, "action route did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn process_confirmation_rechecks_identity_before_delivering_a_signal() {
    for (start, expected, sends) in [
        (101, State::Failed(SignalFailure::IdentityChanged), 0),
        (100, State::Succeeded, 1),
    ] {
        let mut rig = rig(start, 2, false);
        let generation = request(&mut rig.app);
        assert_eq!(rig.fake.sends.load(Ordering::SeqCst), 0);
        confirm(&mut rig.app, generation);
        drive(&mut rig.app, |app| {
            app.model.actions.state() == Some(&expected)
        });
        assert_eq!(rig.fake.sends.load(Ordering::SeqCst), sends);
    }
}

#[test]
fn cancelling_an_accepted_signal_keeps_close_blocked_until_reconciliation() {
    let mut rig = rig(100, 1, true);
    let generation = request(&mut rig.app);
    confirm(&mut rig.app, generation);
    rig.entered.recv_timeout(WAIT).unwrap();
    drop(rig.app.update(Message::CancelOwnedAction(generation, 1)));
    drop(
        rig.app
            .update(Message::CollectionClose(iced::window::Id::unique())),
    );
    assert_eq!(rig.app.model.actions.live(), 1);
    rig.release.take().unwrap().send(()).unwrap();
    drive(&mut rig.app, |app| {
        matches!(app.model.actions.state(), Some(State::ConfirmedAbsent(_)))
    });
    assert!(rig.app.model.actions.blocks_close());
    drop(rig.app.update(Message::CollectionWake));
    drop(rig.app.update(Message::RetryActionsClose(1)));
    drive(&mut rig.app, |app| {
        matches!(app.model.actions.state(), Some(State::ConfirmedAbsent(_)))
    });
    assert_eq!(
        rig.fake.sends.load(Ordering::SeqCst),
        1,
        "reconciliation must never repeat TERM"
    );
    drop(
        rig.app
            .update(Message::AcknowledgeActionFailure(generation, 2)),
    );
    assert!(!rig.app.model.actions.blocks_close());
}

#[test]
fn dismissing_confirmation_never_admits_a_signal() {
    let mut rig = rig(100, 2, false);
    let generation = request(&mut rig.app);
    drop(rig.app.update(Message::ActionConfirmEvent(
        generation,
        ConfirmDialogEvent::Resolved(DismissCause::Escape),
    )));
    assert!(rig.app.model.actions.confirmation.is_none());
    assert_eq!(rig.fake.sends.load(Ordering::SeqCst), 0);
}

struct Service {
    before: crate::service_actions::Sample,
    commands: AtomicUsize,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: mpsc::Sender<()>,
    observable: bool,
}
impl crate::service_actions::Backend for Service {
    fn sample(
        &self,
        _: &str,
        _: Duration,
        _: &enzui_core::cancellation::CancelToken,
    ) -> Result<crate::service_actions::Sample, crate::service_actions::Failure> {
        let mut sample = self.before.clone();
        if self.commands.load(Ordering::SeqCst) != 0 {
            let gate = self
                .gate
                .lock()
                .map_err(|_| crate::service_actions::Failure::Unverifiable)?
                .take();
            if let Some(gate) = gate {
                let _ = self.entered.send(());
                let _ = gate.recv_timeout(WAIT);
            }
            if !self.observable {
                return Err(crate::service_actions::Failure::Unavailable);
            }
            sample.active = "inactive".into();
            sample.sub = "dead".into();
        }
        Ok(sample)
    }
    fn command(
        &self,
        _: &str,
        _: crate::service_actions::Verb,
        _: Duration,
        _: &enzui_core::cancellation::CancelToken,
    ) -> Result<(), crate::service_actions::Failure> {
        self.commands.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn service_stop_requires_confirmation_and_observed_completion() {
    for observable in [true, false] {
        let mut rig = rig(100, 2, false);
        let row = services::ServiceData {
            name: "fixture.service".into(),
            load: "loaded".into(),
            state: "active".into(),
            sub_state: "running".into(),
            fragment_path: "/fixture/fixture.service".into(),
            action_allowed: true,
            ..Default::default()
        };
        let (release, gate) = mpsc::channel();
        let (entered, seen) = mpsc::channel();
        let service = Arc::new(Service {
            before: crate::service_actions::Sample::from_row(&row).unwrap(),
            commands: AtomicUsize::new(0),
            gate: Mutex::new(Some(gate)),
            entered,
            observable,
        });
        rig.app.model.actions = Actions::with_backends(
            Arc::new(Arc::clone(&rig.fake)),
            Arc::<Service>::clone(&service),
        );
        rig.app.model.apply_snapshot(services::Snapshot {
            services: vec![row],
            ..Default::default()
        });
        drop(
            rig.app
                .update(Message::RequestAction(PendingAction::StopService {
                    unit: "fixture.service".into(),
                })),
        );
        let generation = rig
            .app
            .model
            .actions
            .confirmation
            .as_ref()
            .unwrap()
            .generation;
        assert_eq!(service.commands.load(Ordering::SeqCst), 0);
        confirm(&mut rig.app, generation);
        seen.recv_timeout(WAIT).unwrap();
        assert_ne!(rig.app.model.actions.state(), Some(&State::Succeeded));
        release.send(()).unwrap();
        if observable {
            drive(&mut rig.app, |app| {
                app.model.actions.state() == Some(&State::Succeeded)
            });
        } else {
            drive(&mut rig.app, |app| {
                matches!(app.model.actions.state(), Some(State::ServiceUncertain(_)))
            });
            assert!(
                rig.app.model.actions.blocks_close(),
                "unobservable service completion must remain unresolved"
            );
        }
        assert_eq!(service.commands.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn lifecycle_finish_waits_for_the_accepted_signal_without_resending_it() {
    use enzui_core::activation::lifecycle::{StopChoice, StopControl};
    let mut rig = rig(100, 2, true);
    rig.app.lifecycle = Some(StopControl::detached());
    let generation = request(&mut rig.app);
    confirm(&mut rig.app, generation);
    rig.entered.recv_timeout(WAIT).unwrap();
    let control = rig.app.lifecycle.as_ref().unwrap();
    assert_eq!(control.work().1, 1);
    assert_eq!(
        control.request_stop(StopChoice::FINISH_THEN_EXIT, &control.revision()),
        "accepted"
    );
    drop(rig.app.update(Message::DesktopActivate));
    assert!(rig.app.finish_then_exit);
    assert!(!rig.app.model.actions.is_closing());
    rig.release.take().unwrap().send(()).unwrap();
    drive(&mut rig.app, |app| !app.finish_then_exit);
    assert_eq!(rig.app.model.actions.state(), Some(&State::Succeeded));
    assert_eq!(rig.fake.sends.load(Ordering::SeqCst), 1);
}
