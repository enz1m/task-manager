use crate::{
    model::{Model, Page},
    services::{ProcessData, Snapshot},
    table::RowShape,
};
use enzui_core::virtual_sequence::ItemProvider;

fn model() -> Model {
    let p = |pid: &str, name: &str, application: Option<&str>, cpu: &str| ProcessData {
        pid: pid.into(),
        name: name.into(),
        application_name: application.map(str::to_owned),
        kind: if application.is_some() {
            "App"
        } else {
            "System"
        }
        .into(),
        start_time_ticks: 100,
        rss_kib: Some(1024),
        rss: "1.0 MiB".into(),
        cpu: cpu.into(),
        action_allowed: true,
        ..Default::default()
    };
    let mut model = Model::from_snapshot(Snapshot {
        processes: vec![
            p("10", "browser", Some("Browser"), "2.0%"),
            p("11", "Éditeur", Some("Browser"), "3.0%"),
            p("20", "Console", Some("Console"), "4.0%"),
            p("30", "worker", None, "99.0%"),
        ],
        ..Default::default()
    });
    model.select_page(Page::Processes);
    model
}

#[test]
fn grouped_processes_expand_and_search_without_duplicating_identities_or_totals() {
    let mut model = model();
    let table = model.process_table();
    assert!(matches!(table.shape(0), Some(RowShape::Group { name, .. }) if name == "Browser"));
    assert_eq!(table.row(0).unwrap().cpu, "5.0%");
    assert!(
        table.process(0).is_none(),
        "an aggregate has no actionable PID"
    );
    model.toggle_app_group("Browser".into());
    let table = model.process_table();
    let keys: std::collections::HashSet<_> =
        (0..table.len()).map(|i| table.item(i).unwrap()).collect();
    assert_eq!(keys.len(), table.rows().len());
    assert_eq!(table.process_count(), 4);
    model.set_query("E\u{301}DITEUR".into());
    assert_eq!(model.process_table().process_count(), 1);
    assert_eq!(
        model.process_table().processes().next().unwrap().name,
        "Éditeur"
    );
}
