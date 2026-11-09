use super::*;

#[path = "procfs_scan_tests.rs"]
mod procfs_scan_tests;

#[test]
fn process_policy_protects_system_desktop_and_other_users_without_blocking_owned_apps() {
    for (pid, name, owner, caller, allowed) in [
        ("1", "systemd", "root", "alice", false),
        ("4242", "gnome-shell", "alice", "alice", false),
        ("4243", "editor", "bob", "alice", false),
        ("4244", "editor", "alice", "alice", true),
        ("4245", "editor", "bob", "root", true),
    ] {
        assert_eq!(
            process_action_allowed(pid, name, name, owner, "App", caller),
            allowed
        );
    }
    assert!(!process_action_allowed(
        &std::process::id().to_string(),
        "task-manager-app",
        "task-manager-app",
        "alice",
        "App",
        "alice"
    ));
}

#[test]
fn service_refresh_parses_units_and_their_metadata() {
    use std::os::unix::process::ExitStatusExt;

    let mut calls = 0;
    let (services, unavailable) =
        collection::collect_services_via(&CancelToken::new(), &mut |_request, _cancel| {
            calls += 1;
            let stdout = if calls == 1 {
                b"example.service loaded active running Example background service\n".to_vec()
            } else {
                b"Id=example.service\nFragmentPath=/usr/lib/systemd/system/example.service\n"
                    .to_vec()
            };
            Ok(ProcessOutput {
                status: std::process::ExitStatus::from_raw(0),
                stdout,
                stderr: Vec::new(),
            })
        });
    assert!(!unavailable);
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].name, "example.service");
    assert_eq!(services[0].description, "Example background service");
    assert_eq!(services[0].state, "active");
    assert_eq!(
        services[0].fragment_path,
        "/usr/lib/systemd/system/example.service"
    );
}
