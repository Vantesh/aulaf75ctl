use std::process::Command;

// Only metadata, dry-runs and rejected operations: these processes cannot reach discovery
// or a hardware request, even when the developer has a keyboard plugged in.
#[test]
fn json_stdout_and_exit_status_are_a_client_contract() {
    for (args, code) in [
        (vec!["devices", "--unknown", "--json"], 2),
        (vec!["indicator", "mode", "off", "--apply", "--json"], 2),
        (vec!["indicator", "mode", "blink", "--dry-run", "--json"], 2),
        (
            vec!["indicator", "status", "--transport", "wireless", "--json"],
            2,
        ),
        (
            vec!["lighting", "color", "#89b4fa", "--dry-run", "--json"],
            0,
        ),
        (vec!["lighting", "color", "#12345g", "--json"], 2),
        (
            vec![
                "lighting",
                "color",
                "#12345g",
                "--transport",
                "wireless",
                "--json",
            ],
            2,
        ),
        (vec!["lighting", "brightness", "70", "--json"], 2),
        (vec!["lighting", "off", "--apply", "--json"], 2),
        (vec!["lighting", "on", "--apply", "--json"], 2),
        (
            vec![
                "lighting",
                "off",
                "--dry-run",
                "--transport",
                "wireless",
                "--json",
            ],
            2,
        ),
        (
            vec![
                "lighting",
                "on",
                "--dry-run",
                "--transport",
                "wireless",
                "--json",
            ],
            2,
        ),
        (vec!["lighting", "palette", "green", "--json"], 2),
        (vec!["lighting", "palette", "green", "--apply", "--json"], 2),
        (vec!["lighting", "palette", "8", "--dry-run", "--json"], 2),
        (
            vec!["lighting", "palette", "#ff0000", "--dry-run", "--json"],
            2,
        ),
        (vec!["lighting", "speed", "2", "--json"], 2),
        (vec!["lighting", "speed", "2", "--apply", "--json"], 2),
        (vec!["lighting", "speed", "-1", "--dry-run", "--json"], 2),
        (vec!["lighting", "speed", "5", "--dry-run", "--json"], 2),
        (vec!["lighting", "brightness", "5", "--json"], 2),
        (vec!["lighting", "brightness", "5", "--apply", "--json"], 2),
        (
            vec!["lighting", "brightness", "-1", "--dry-run", "--json"],
            2,
        ),
        (
            vec!["lighting", "brightness", "1.5", "--dry-run", "--json"],
            2,
        ),
        (vec!["lighting", "off", "--json"], 2),
        (vec!["lighting", "effect", "rainbow", "--json"], 2),
        (
            vec!["lighting", "effect", "rainbow", "--apply", "--json"],
            2,
        ),
        (vec!["lighting", "effect", "rain", "--dry-run", "--json"], 2),
        (
            vec![
                "lighting",
                "effect",
                "21",
                "--dry-run",
                "--transport",
                "wireless",
                "--json",
            ],
            2,
        ),
        (vec!["lighting", "static", "#89b4fa", "--json"], 2),
        (
            vec!["status", "--apply", "--review", "unused.json", "--json"],
            2,
        ),
        (
            vec![
                "lighting",
                "static",
                "#89b4fa",
                "--dry-run",
                "--apply",
                "--review",
                "unused.json",
                "--json",
            ],
            2,
        ),
        (
            vec![
                "lighting",
                "static",
                "#89b4fa",
                "--dry-run",
                "--transport",
                "wireless",
                "--json",
            ],
            2,
        ),
        (
            vec!["lighting", "static", "#89b4fa", "--apply", "--json"],
            2,
        ),
        (vec!["--version", "--json"], 0),
        (vec!["--json", "--help"], 0),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aulaf75ctl"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["ok"], code == 0);
        assert_eq!(output.stderr.is_empty(), code == 0);
        if json["command"] == "lighting_color" {
            assert_eq!(json["lighting"]["outcome"], "prepared");
            assert_eq!(json["lighting"]["hardware_verified"], false);
            assert_eq!(json["devices"], serde_json::json!([]));
            let packet = json["lighting"]["packet_hex"].as_str().unwrap();
            assert_eq!(packet.len(), 1040);
            assert!(packet.starts_with("0608000001007a0189b4fa"));
        }
    }
}

#[test]
fn wireless_color_dry_run_emits_only_output_frames_without_discovery() {
    let output = Command::new(env!("CARGO_BIN_EXE_aulaf75ctl"))
        .args([
            "lighting",
            "color",
            "#89b4fa",
            "--transport",
            "wireless",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["devices"], serde_json::json!([]));
    assert_eq!(json["lighting"]["report_id"], 0x13);
    assert_eq!(json["lighting"]["command"], 0x88);
    assert!(json["lighting"].get("packet_hex").is_none());
    assert!(json["lighting"].get("required_model").is_none());
    let packets = json["lighting"]["packets_hex"].as_array().unwrap();
    assert_eq!(packets.len(), 10);
    assert!(packets.iter().all(|p| p.as_str().unwrap().len() == 40));
    assert!(
        packets[0]
            .as_str()
            .unwrap()
            .starts_with("13880a001e89b4fa7e")
    );
}
