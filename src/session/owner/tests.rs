use super::*;
use crate::model::Page;
use enzui_core::persist::Persistable;
use enzui_test_support::test_scratch::ScratchDir;
use std::sync::mpsc;

const WAIT: Duration = Duration::from_secs(3);

fn drive(owner: &mut Owner, done: impl Fn(&Owner) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        owner.poll(Instant::now());
        if done(owner) {
            return;
        }
        assert!(Instant::now() < deadline, "session owner did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn legacy_sessions_migrate_on_save_and_invalid_files_remain_untouched() {
    let fixture = ScratchDir::new_restricted("tm-session");
    let path = fixture.join("session");
    let canonical = Session {
        page: Some(Page::Processes),
    }
    .persist_serialize()
    .unwrap();
    let legacy = [super::super::LEGACY_FRAME_V1, canonical.as_bytes()].concat();
    std::fs::write(&path, &legacy).unwrap();
    let mut owner = Owner::new(Some(&path));
    owner.begin_load(false);
    drive(&mut owner, |o| !o.loading && o.flight.is_none());
    assert_eq!(owner.baseline.unwrap().page, Some(Page::Processes));
    assert_eq!(std::fs::read(&path).unwrap(), legacy);
    owner.remember(Session {
        page: Some(Page::Processes),
    });
    drive(&mut owner, |o| o.dirty.is_none() && o.flight.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), canonical);

    for bytes in [
        b"malformed".to_vec(),
        String::from_utf8(legacy)
            .unwrap()
            .replace("task-manager-session v1", "foreign-app-session v1")
            .into_bytes(),
        vec![b'x'; super::super::RAW_BYTE_LIMIT.bytes() as usize + 1],
    ] {
        std::fs::write(&path, &bytes).unwrap();
        let mut owner = Owner::new(Some(&path));
        owner.begin_load(false);
        drive(&mut owner, |o| !o.loading && o.flight.is_none());
        owner.remember(Session {
            page: Some(Page::Services),
        });
        assert!(owner.is_write_blocked());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn close_waits_for_the_newest_page_when_an_older_save_is_in_flight() {
    let fixture = ScratchDir::new_restricted("tm-session-close");
    let path = fixture.join("session");
    let mut owner = Owner::new(Some(&path));
    let (release, held) = mpsc::channel();
    let (entered, seen) = mpsc::channel();
    owner.before_save = Some(Box::new(move || {
        entered.send(()).unwrap();
        let _ = held.recv_timeout(WAIT);
    }));
    owner.remember(Session {
        page: Some(Page::Processes),
    });
    seen.recv_timeout(WAIT).unwrap();
    let old = owner.dirty.unwrap();
    owner.remember(Session {
        page: Some(Page::Services),
    });
    let window = iced::window::Id::unique();
    owner.start_close(window);
    owner.settle_save(old, Ok(()), Instant::now());
    assert!(owner.dirty.is_some());
    assert!(owner.ready_window().is_none());
    release.send(()).unwrap();
    drive(&mut owner, |o| o.ready_window().is_some());
    assert_eq!(owner.ready_window(), Some(window));
    assert_eq!(
        Owner::new(Some(&path)).load().value().page,
        Some(Page::Services)
    );
}

#[test]
fn replaced_session_path_is_not_overwritten_by_a_pending_save() {
    let fixture = ScratchDir::new_restricted("tm-session-custody");
    let path = fixture.join("session");
    let mut owner = Owner::new(Some(&path));
    owner.remember(Session {
        page: Some(Page::Processes),
    });
    drive(&mut owner, |o| o.dirty.is_none() && o.flight.is_none());
    std::fs::rename(&path, fixture.join("original")).unwrap();
    std::fs::write(&path, b"replacement owned elsewhere").unwrap();
    owner.remember(Session {
        page: Some(Page::Services),
    });
    drive(&mut owner, |o| o.flight.is_none());
    assert!(owner.failure.is_some() || owner.fatal.is_some());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"replacement owned elsewhere"
    );
}
