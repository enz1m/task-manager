use super::*;
use enzui_test_support::test_scratch::ScratchDir;

#[test]
fn invalid_preferred_temperature_does_not_fall_back_to_a_different_sensor() {
    let root = ScratchDir::new_restricted("tm-temperature");
    let thermal = root.join("thermal");
    let hwmon = root.join("hwmon");
    let chip = hwmon.join("hwmon0");
    fs::create_dir(&thermal).unwrap();
    fs::create_dir_all(&chip).unwrap();
    for (file, value) in [
        ("name", "k10temp"),
        ("temp1_label", "Tdie"),
        ("temp1_input", "42000"),
        ("temp2_label", "Tctl"),
        ("temp2_input", "55000"),
    ] {
        fs::write(chip.join(file), value).unwrap();
    }
    let collect = || CpuTemperatureReading::collect_in(&thermal, &hwmon);
    assert!(collect().milli_celsius().is_some());
    fs::write(chip.join("temp1_input"), "999000").unwrap();
    assert!(collect().milli_celsius().is_none());
}
