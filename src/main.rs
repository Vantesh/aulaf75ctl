#[cfg(not(target_os = "linux"))]
compile_error!("aulaf75ctl currently supports Linux hidraw only");

mod cli;
mod device;
mod power;
mod protocol;
mod static_write;

use serde::Serialize;
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Clone, Debug, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
struct Report {
    schema_version: u8,
    command: &'static str,
    ok: bool,
    devices: Vec<device::Device>,
    error: Option<Error>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lighting: Option<Lighting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    static_plan: Option<protocol::LightingPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    static_result: Option<static_write::ApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effect_plan: Option<protocol::LightingPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effect_result: Option<static_write::ApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brightness_plan: Option<protocol::LightingPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brightness_result: Option<static_write::ApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed_plan: Option<protocol::LightingPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed_result: Option<static_write::ApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    palette_plan: Option<protocol::LightingPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    palette_result: Option<static_write::ApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    power_plan: Option<protocol::LightingPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    power_result: Option<static_write::ApplyResult>,
}

#[derive(Serialize)]
struct Lighting {
    rgb: [u8; 3],
    command: u8,
    report_id: u8,
    dry_run: bool,
    persistent: bool,
    hardware_verified: bool,
    outcome: &'static str,
    required_mode: [u8; 2],
    required_model: [u8; 6],
    #[serde(skip_serializing_if = "Option::is_none")]
    packet_hex: Option<String>,
}

fn run(options: cli::Options) -> Report {
    let request = match &options.command {
        cli::Command::Static(color) => Some(protocol::LightingRequest::Static(*color)),
        cli::Command::Effect(effect) => Some(protocol::LightingRequest::Effect(*effect)),
        cli::Command::Brightness(level) => Some(protocol::LightingRequest::Brightness(*level)),
        cli::Command::Speed(level) => Some(protocol::LightingRequest::Speed(*level)),
        cli::Command::Palette(palette) => Some(protocol::LightingRequest::Palette(*palette)),
        cli::Command::Power(action) => Some(protocol::LightingRequest::Power(*action)),
        cli::Command::Apply { request, .. } => Some(*request),
        _ => None,
    };
    let name = match options.command {
        cli::Command::Devices => "devices",
        cli::Command::Color(_) => "lighting_color",
        cli::Command::Static(_)
        | cli::Command::Effect(_)
        | cli::Command::Brightness(_)
        | cli::Command::Speed(_)
        | cli::Command::Palette(_)
        | cli::Command::Power(_)
        | cli::Command::Apply { .. } => request.unwrap().command(),
        _ => "status",
    };
    let color = match options.command {
        cli::Command::Color(color) => Some(color),
        _ => None,
    };
    let mut report = Report {
        schema_version: 1,
        command: name,
        ok: false,
        devices: Vec::new(),
        error: None,
        static_plan: None,
        static_result: None,
        effect_plan: None,
        effect_result: None,
        brightness_plan: None,
        brightness_result: None,
        speed_plan: None,
        speed_result: None,
        palette_plan: None,
        palette_result: None,
        power_plan: None,
        power_result: None,
        lighting: color.map(|color| Lighting {
            rgb: color.0,
            command: 8,
            report_id: 6,
            dry_run: options.dry_run,
            persistent: false,
            hardware_verified: false,
            outcome: "not_sent",
            required_mode: protocol::PER_KEY_MODE,
            required_model: protocol::F75_MODEL,
            packet_hex: options.dry_run.then(|| {
                protocol::color_frame(color)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect()
            }),
        }),
    };
    if options.dry_run && matches!(options.command, cli::Command::Color(_)) {
        report.ok = true;
        report
            .lighting
            .as_mut()
            .expect("validated color dry-run")
            .outcome = "prepared";
        return report;
    }
    if nix::unistd::geteuid().is_root() || nix::unistd::getuid().is_root() {
        report.error = Some(Error::new(
            "root_refused",
            "Run as your normal desktop user; root hardware access is disabled",
        ));
        return report;
    }
    let review = if let cli::Command::Apply { review, .. } = &options.command {
        match static_write::load_review(review) {
            Ok(value) => Some(value),
            Err(error) => {
                report.error = Some(error);
                return report;
            }
        }
    } else {
        None
    };
    let transport = if color.is_some() || request.is_some() {
        Some(device::Transport::Wired)
    } else {
        options.transport
    };
    match device::discover(transport) {
        Ok(devices) => report.devices = devices,
        Err(error) => {
            report.error = Some(error);
            return report;
        }
    }
    if let Some(request) = request {
        let effect = request.effect().is_some();
        let brightness = request.brightness().is_some();
        let speed = request.speed().is_some();
        let palette = request.palette().is_some();
        let power = request.power().is_some();
        if matches!(options.command, cli::Command::Apply { .. }) {
            let result =
                device::apply_lighting(&mut report.devices, request, review.as_ref().unwrap());
            report.ok = result.error.is_none();
            report.error = result.error.clone();
            if power {
                report.power_result = Some(result);
            } else if palette {
                report.palette_result = Some(result);
            } else if speed {
                report.speed_result = Some(result);
            } else if brightness {
                report.brightness_result = Some(result);
            } else if effect {
                report.effect_result = Some(result);
            } else {
                report.static_result = Some(result);
            }
        } else {
            match device::prepare_lighting(&mut report.devices, request) {
                Ok(plan) => {
                    report.ok = true;
                    if power {
                        report.power_plan = Some(plan);
                    } else if palette {
                        report.palette_plan = Some(plan);
                    } else if speed {
                        report.speed_plan = Some(plan);
                    } else if brightness {
                        report.brightness_plan = Some(plan);
                    } else if effect {
                        report.effect_plan = Some(plan);
                    } else {
                        report.static_plan = Some(plan);
                    }
                }
                Err(error) => report.error = Some(error),
            }
        }
        return report;
    }
    operate(
        &mut report,
        options.command,
        device::inspect,
        device::preview_color,
    );
    report
}

fn operate(
    report: &mut Report,
    command: cli::Command,
    inspect: impl FnOnce(&mut [device::Device]),
    preview: impl FnOnce(&mut [device::Device], protocol::Color) -> Result<(), Error>,
) {
    if command == cli::Command::Devices {
        report.ok = true;
    } else if let cli::Command::Color(color) = command {
        match preview(&mut report.devices, color) {
            Ok(()) => {
                report.ok = true;
                report.lighting.as_mut().unwrap().outcome = "sent_unverified";
            }
            Err(error) => {
                if error.code == "lighting_transfer_uncertain" {
                    report.lighting.as_mut().unwrap().outcome = "unknown";
                }
                report.error = Some(error);
            }
        }
    } else if report.devices.is_empty() {
        report.error = Some(Error::new(
            "device_not_found",
            "No supported USB HID candidate found for the selected transport; connect the F75 by cable or its 3554:fa09 receiver",
        ));
    } else {
        inspect(&mut report.devices);
        // A passive receiver can coexist with a successfully read wired board.
        // With receiver-only discovery there is no claim of keyboard connectivity.
        report.ok = report.devices.iter().any(|d| d.protocol_verified)
            && report
                .devices
                .iter()
                .filter(|d| d.transport == device::Transport::Wired)
                .all(|d| d.protocol_verified);
        if !report.ok {
            report.error = Some(Error::new(
                "status_unavailable",
                "No complete verified wired status is available; inspect the per-device diagnostics",
            ));
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let json = args.iter().any(|arg| arg == "--json");
    let details = args.iter().any(|arg| arg == "--details");
    let (report, argument_error) = match cli::parse(&args) {
        Ok(options) => match options.command {
            cli::Command::Help => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"schema_version": 1, "command": "help", "ok": true, "help": cli::HELP})
                    );
                } else {
                    println!("{}", cli::HELP);
                }
                return ExitCode::SUCCESS;
            }
            cli::Command::Version => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"schema_version": 1, "command": "version", "ok": true, "version": env!("CARGO_PKG_VERSION")})
                    );
                } else {
                    println!("aulaf75ctl {}", env!("CARGO_PKG_VERSION"));
                }
                return ExitCode::SUCCESS;
            }
            _ => (run(options), false),
        },
        Err(error) => (
            Report {
                schema_version: 1,
                command: "invalid",
                ok: false,
                devices: Vec::new(),
                error: Some(error),
                lighting: None,
                static_plan: None,
                static_result: None,
                effect_plan: None,
                effect_result: None,
                brightness_plan: None,
                brightness_result: None,
                speed_plan: None,
                speed_result: None,
                palette_plan: None,
                palette_result: None,
                power_plan: None,
                power_result: None,
            },
            true,
        ),
    };
    for error in report
        .devices
        .iter()
        .flat_map(|d| d.error.iter().chain(d.model_error.iter()))
        .chain(report.error.iter())
    {
        eprintln!("{}: {}", error.code, error.message);
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let result = if json {
        json_report(&report, details)
            .and_then(|value| serde_json::to_writer(&mut out, &value))
            .map_err(io::Error::other)
            .and_then(|()| writeln!(out))
    } else {
        (|| {
            if report.devices.is_empty() && report.ok {
                writeln!(out, "No supported USB HID candidates found")?;
            }
            for device in &report.devices {
                writeln!(
                    out,
                    "{} {:?} interface {}: {}",
                    device.usb_id,
                    device.transport,
                    device
                        .interface_number
                        .map_or_else(|| "unknown".into(), |n| n.to_string()),
                    device.operation_state.unwrap_or(device.state)
                )?;
                if let Some(config) = &device.configuration {
                    writeln!(
                        out,
                        "  Config: {} bytes, valid signature; lighting {} (raw mode {:02x?})",
                        config.length, config.lighting_mode, config.mode_bytes
                    )?;
                    writeln!(
                        out,
                        "  Stored effect {} ({}, {}); active {}, brightness {}; color {} (index {})",
                        config.effect_id,
                        config.effect_name,
                        config.effect_name_scope,
                        config
                            .active_effect
                            .map_or_else(|| "unknown".into(), |e| format!("{} ({})", e.id, e.name)),
                        config
                            .brightness
                            .map_or_else(|| "unknown".into(), |b| format!("{b}/9")),
                        config.color_name,
                        config
                            .color_index
                            .map_or_else(|| "unknown".into(), |c| c.to_string())
                    )?;
                }
                if let Some(model) = &device.model {
                    writeln!(
                        out,
                        "  Model: {} ({:02x?}), expected F75 identity matched: {}",
                        model.identity, model.payload, model.matches_expected
                    )?;
                }
            }
            if let Some(lighting) = &report.lighting {
                writeln!(
                    out,
                    "Color preview: {} (hardware effect not verified)",
                    lighting.outcome
                )?;
                writeln!(
                    out,
                    "  RGB #{:02x}{:02x}{:02x}; report 06 command 08; 378 RGB bytes, 520 report bytes",
                    lighting.rgb[0], lighting.rgb[1], lighting.rgb[2]
                )?;
                if lighting.dry_run {
                    writeln!(
                        out,
                        "  No device opened; requires self-defined flag/effect [1,21] and F75 model 03 00 00 00 00 cd"
                    )?;
                }
            }
            if let Some(plan) = report
                .static_plan
                .as_ref()
                .or(report.effect_plan.as_ref())
                .or(report.brightness_plan.as_ref())
                .or(report.speed_plan.as_ref())
                .or(report.palette_plan.as_ref())
                .or(report.power_plan.as_ref())
            {
                print_lighting_plan(&mut out, plan)?;
            }
            if let Some(result) = report
                .static_result
                .as_ref()
                .or(report.effect_result.as_ref())
                .or(report.brightness_result.as_ref())
                .or(report.speed_result.as_ref())
                .or(report.palette_result.as_ref())
                .or(report.power_result.as_ref())
            {
                if let Some(power) = &result.power {
                    writeln!(
                        out,
                        "Lighting {:?}: configuration {}, matrix {}; session restoration {}",
                        power.action,
                        result.configuration_write,
                        result.matrix_write,
                        power.restoration_status
                    )?;
                    if let Some(target) = power.restoration_target {
                        writeln!(out, "  Restoration target: {} ({})", target.name, target.id)?;
                    }
                } else if let Some(palette) = result.requested_palette {
                    writeln!(
                        out,
                        "Palette apply: requested {} ({}); configuration {}, matrix {}",
                        palette.name,
                        palette.index,
                        result.configuration_write,
                        result.matrix_write
                    )?;
                    if let (Some(effect), Some(previous), Some(offset)) = (
                        result.active_effect,
                        result.previous_palette,
                        result.parameter_offset,
                    ) {
                        writeln!(
                            out,
                            "  Active effect {} ({}); palette {} ({}) -> {} ({}) at configuration[{offset}]; speed preserved",
                            effect.name,
                            effect.id,
                            previous.name,
                            previous.index,
                            palette.name,
                            palette.index
                        )?;
                    }
                } else if let Some(level) = result.requested_speed {
                    writeln!(
                        out,
                        "Speed apply: requested {level}/4; configuration {}, matrix {}",
                        result.configuration_write, result.matrix_write
                    )?;
                    if let (Some(effect), Some(previous), Some(offset)) = (
                        result.active_effect,
                        result.previous_speed,
                        result.parameter_offset,
                    ) {
                        writeln!(
                            out,
                            "  Active effect {} ({}); saved speed {previous} -> {level} at configuration[{offset}]; palette preserved",
                            effect.name, effect.id
                        )?;
                    }
                } else if let Some(level) = result.requested_brightness {
                    writeln!(
                        out,
                        "Brightness apply: requested {level}/9; configuration {}, matrix {}",
                        result.configuration_write, result.matrix_write
                    )?;
                    if let (Some(effect), Some(previous), Some(offset)) = (
                        result.active_effect,
                        result.previous_brightness,
                        result.parameter_offset,
                    ) {
                        writeln!(
                            out,
                            "  Active effect {} ({}); brightness {previous} -> {level} at configuration[{offset}]",
                            effect.name, effect.id
                        )?;
                    }
                } else if let Some(effect) = result.requested_effect {
                    writeln!(
                        out,
                        "Effect {} ({}): configuration {}, matrix {}",
                        effect.name, effect.id, result.configuration_write, result.matrix_write
                    )?;
                    if let Some(previous) = result.previous_effect {
                        writeln!(
                            out,
                            "  Previous effect: {} ({})",
                            previous.name, previous.id
                        )?;
                    }
                } else if let Some(rgb) = result.requested_rgb {
                    writeln!(
                        out,
                        "Static apply #{:02x}{:02x}{:02x}: matrix write {}, configuration write {}",
                        rgb[0], rgb[1], rgb[2], result.matrix_write, result.configuration_write
                    )?;
                }
                writeln!(
                    out,
                    "  Readbacks: matrix {}, configuration {}; mode {}; no-op {}",
                    result.matrix_readback,
                    result.configuration_readback,
                    result.hardware_mode,
                    result.no_op
                )?;
                if result.configuration_refresh {
                    writeln!(
                        out,
                        "  Configuration unchanged; refreshed after matrix as in Studio"
                    )?;
                }
                writeln!(
                    out,
                    "  Visual verification has NOT occurred; no retries or automatic rollback"
                )?;
                if let Some(path) = &result.snapshot_path {
                    writeln!(out, "  Recovery snapshot: {}", path.display())?;
                }
                if let Some(step) = result.failed_step {
                    writeln!(out, "  Stopped at: {step}")?;
                }
            }
            Ok(())
        })()
    };
    if result.is_err() {
        return ExitCode::FAILURE;
    }
    if argument_error {
        ExitCode::from(2)
    } else if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_lighting_plan(out: &mut impl Write, plan: &protocol::LightingPlan) -> io::Result<()> {
    if let Some(power) = &plan.power {
        writeln!(
            out,
            "Lighting {:?} plan: effect {} -> {}; no-op {}; session restoration {}",
            power.action,
            plan.previous_effect.unwrap().id,
            plan.requested_effect.unwrap().id,
            plan.no_op,
            power.restoration_status
        )?;
        if let Some(target) = power.restoration_target {
            writeln!(
                out,
                "  Restoration target: {} ({}); same USB instance only",
                target.name, target.id
            )?;
        }
    } else if let Some(palette) = plan.requested_palette {
        let effect = plan.active_effect.expect("palette plan has active effect");
        let previous = plan
            .previous_palette
            .expect("palette plan has previous palette");
        writeln!(
            out,
            "Palette plan {} ({}) at configuration[{}]: {} ({}) -> {} ({}); no-op {}; speed preserved",
            effect.name,
            effect.id,
            plan.parameter_offset.unwrap(),
            previous.name,
            previous.index,
            palette.name,
            palette.index,
            plan.no_op
        )?;
    } else if let Some(level) = plan.requested_speed {
        let effect = plan.active_effect.expect("speed plan has active effect");
        writeln!(
            out,
            "Speed plan {} ({}) at configuration[{}]: {} -> {level}/4; no-op {}; palette preserved",
            effect.name,
            effect.id,
            plan.parameter_offset.unwrap(),
            plan.previous_speed.unwrap(),
            plan.no_op
        )?;
    } else if let Some(level) = plan.requested_brightness {
        let effect = plan
            .active_effect
            .expect("brightness plan has active effect");
        writeln!(
            out,
            "Brightness plan {} ({}) at configuration[{}]: {} -> {level}/9; no-op {}",
            effect.name,
            effect.id,
            plan.parameter_offset.unwrap(),
            plan.previous_brightness.unwrap(),
            plan.no_op
        )?;
    } else if let Some(effect) = plan.requested_effect {
        let previous = plan
            .previous_effect
            .expect("effect plan has previous effect");
        writeln!(
            out,
            "Effect plan {} ({}) -> {} ({}); no-op {}",
            previous.name, previous.id, effect.name, effect.id, plan.no_op
        )?;
        writeln!(
            out,
            "  Saved brightness {}; speed/palette and stored per-key RGB preserved",
            plan.brightness
                .map_or_else(|| "inapplicable".into(), |b| format!("{b}/9"))
        )?;
    } else if let Some(rgb) = plan.rgb {
        writeln!(
            out,
            "Static plan #{:02x}{:02x}{:02x}: brightness {}/9; no-op {}",
            rgb[0],
            rgb[1],
            rgb[2],
            plan.brightness.unwrap(),
            plan.no_op
        )?;
    }
    writeln!(
        out,
        "  Matrix: {} changed bytes; padding preserved; configuration: {} changed bytes{}",
        plan.matrix_diff.len(),
        plan.configuration_diff.len(),
        if plan.configuration_refresh {
            "; unchanged refresh required after matrix"
        } else {
            ""
        }
    )?;
    if let Some(warning) = plan.overwrite_warning {
        writeln!(out, "  Warning: {warning}")?;
    }
    for change in &plan.configuration_diff {
        writeln!(
            out,
            "  configuration[{}]: {} -> {}",
            change.offset, change.old, change.new
        )?;
    }
    writeln!(
        out,
        "  {} planned write(s); none sent. Full review/diffs/packets: --dry-run --details --json",
        plan.steps.len()
    )
}

fn json_report(report: &Report, details: bool) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(report)?;
    value["observed_native_effects"] = serde_json::to_value(
        protocol::OBSERVED_NATIVE_EFFECT_IDS.map(|id| protocol::Effect::from_id(id).unwrap()),
    )?;
    if let Some(plan) = report
        .static_plan
        .as_ref()
        .or(report.effect_plan.as_ref())
        .or(report.brightness_plan.as_ref())
        .or(report.speed_plan.as_ref())
        .or(report.palette_plan.as_ref())
        .or(report.power_plan.as_ref())
    {
        value["review_ready"] = details.into();
        if !details {
            let object = value[plan.plan_key()].as_object_mut().unwrap();
            object.remove("matrix_diff");
            object.remove("review_state");
            object.insert("matrix_changed_bytes".into(), plan.matrix_diff.len().into());
            object.insert(
                "configuration_changed_bytes".into(),
                plan.configuration_diff.len().into(),
            );
            object.insert("steps".into(), plan.steps.iter().map(|step| serde_json::json!({"command":step.command,"payload_length":step.payload_length})).collect::<Vec<_>>().into());
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lighting_plan_views_are_concise_while_json_retains_full_review_details() {
        let mut config = protocol::config_request();
        config[17] = 1;
        config[18] = 21;
        config[102] = 9;
        config[134..136].copy_from_slice(&[0x5a, 0xa5]);
        let matrix = protocol::matrix_request();
        let plan = protocol::prepare_static(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Color([137, 180, 250]),
        )
        .unwrap();
        let mut output = Vec::new();
        print_lighting_plan(&mut output, &plan).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("#89b4fa"));
        assert!(text.contains("378 changed bytes"));
        assert!(text.contains("Warning: Overwrites stored per-key RGB colors in group 0"));
        assert!(text.lines().count() <= 5);
        let json = serde_json::to_value(&plan).unwrap();
        assert!(
            json["overwrite_warning"]
                .as_str()
                .unwrap()
                .contains("previous layout is not retained")
        );
        assert_eq!(json["matrix_diff"].as_array().unwrap().len(), 378);
        assert_eq!(
            json["review_state"]["configuration_hex"]
                .as_str()
                .unwrap()
                .len(),
            256
        );
        assert_eq!(
            json["review_state"]["matrix_hex"].as_str().unwrap().len(),
            768
        );
        assert_eq!(json["steps"][0]["packet_hex"].as_str().unwrap().len(), 1040);
        assert_eq!(json["configuration_refresh"], true);
        let report = Report {
            schema_version: 1,
            command: "lighting_static",
            ok: true,
            devices: Vec::new(),
            error: None,
            lighting: None,
            static_plan: Some(plan),
            static_result: None,
            effect_plan: None,
            effect_result: None,
            brightness_plan: None,
            brightness_result: None,
            speed_plan: None,
            speed_result: None,
            palette_plan: None,
            palette_result: None,
            power_plan: None,
            power_result: None,
        };
        let summary = json_report(&report, false).unwrap();
        assert_eq!(summary["review_ready"], false);
        assert_eq!(summary["static_plan"]["matrix_changed_bytes"], 378);
        assert!(summary["static_plan"].get("matrix_diff").is_none());
        assert!(summary["static_plan"].get("review_state").is_none());
        assert!(
            summary["static_plan"]["steps"][0]
                .get("packet_hex")
                .is_none()
        );
        let detailed = json_report(&report, true).unwrap();
        assert_eq!(detailed["review_ready"], true);
        assert_eq!(detailed["static_plan"], json);
        let effect_plan = protocol::prepare_effect(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Effect::from_id(1).unwrap(),
        )
        .unwrap();
        let report = Report {
            command: "lighting_effect",
            static_plan: None,
            effect_plan: Some(effect_plan),
            ..report
        };
        let summary = json_report(&report, false).unwrap();
        assert_eq!(summary["command"], "lighting_effect");
        assert_eq!(summary["effect_plan"]["requested_effect"]["id"], 1);
        assert_eq!(summary["effect_plan"]["previous_effect"]["id"], 21);
        assert_eq!(summary["effect_plan"]["matrix_changed_bytes"], 0);
        assert!(summary["effect_plan"].get("rgb").is_none());
        assert_eq!(summary["effect_plan"]["steps"][0]["command"], 4);
        assert!(
            summary["effect_plan"]["steps"][0]
                .get("packet_hex")
                .is_none()
        );
        assert!(
            json_report(&report, true).unwrap()["effect_plan"]["review_state"]["matrix_hex"]
                .is_string()
        );
        let plan = protocol::prepare_brightness(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Brightness::parse("5").unwrap(),
        )
        .unwrap();
        let mut output = Vec::new();
        print_lighting_plan(&mut output, &plan).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("self_defined (21) at configuration[94]: 9 -> 5/9"));
        assert!(text.contains("configuration[94]: 9 -> 5"));
        let report = Report {
            command: "lighting_brightness",
            effect_plan: None,
            brightness_plan: Some(plan),
            ..report
        };
        let summary = json_report(&report, false).unwrap();
        assert_eq!(summary["command"], "lighting_brightness");
        let plan = &summary["brightness_plan"];
        assert_eq!(plan["active_effect"]["id"], 21);
        assert_eq!(plan["previous_brightness"], 9);
        assert_eq!(plan["requested_brightness"], 5);
        assert_eq!(plan["parameter_offset"], 94);
        assert_eq!(plan["matrix_changed_bytes"], 0);
        assert_eq!(
            plan["configuration_diff"],
            serde_json::json!([{"offset":94,"old":9,"new":5}])
        );
        assert!(plan.get("requested_effect").is_none() && plan.get("rgb").is_none());
        assert!(plan["steps"][0].get("packet_hex").is_none());
        assert_eq!(plan["steps"][0]["command"], 4);
        assert_eq!(
            json_report(&report, true).unwrap()["brightness_plan"]["steps"][0]["packet_hex"]
                .as_str()
                .unwrap()
                .len(),
            1040
        );
        config[8 + 9] = 0;
        config[8 + 10] = 3;
        config[8 + 62] = 9;
        config[8 + 63] = 0x3f;
        let plan = protocol::prepare_speed(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Speed::parse("0").unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        print_lighting_plan(&mut out, &plan).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Speed plan rainbow (3) at configuration[63]: 3 -> 0/4"));
        assert!(text.contains("palette preserved"));
        let report = Report {
            command: "lighting_speed",
            brightness_plan: None,
            speed_plan: Some(plan),
            ..report
        };
        let summary = json_report(&report, false).unwrap();
        assert_eq!(summary["command"], "lighting_speed");
        let plan = &summary["speed_plan"];
        assert_eq!(plan["active_effect"]["id"], 3);
        assert_eq!(plan["previous_speed"], 3);
        assert_eq!(plan["requested_speed"], 0);
        assert_eq!(plan["parameter_offset"], 63);
        assert_eq!(
            plan["configuration_diff"],
            serde_json::json!([{"offset":63,"old":63,"new":15}])
        );
        assert_eq!(plan["matrix_changed_bytes"], 0);
        assert_eq!(plan["steps"][0]["command"], 4);
        assert!(
            plan.get("requested_brightness").is_none() && plan.get("requested_effect").is_none()
        );
        assert!(plan["steps"][0].get("packet_hex").is_none());
        let detailed = json_report(&report, true).unwrap();
        assert_eq!(
            detailed["speed_plan"]["steps"][0]["packet_hex"]
                .as_str()
                .unwrap()
                .len(),
            1040
        );
        config[8 + 10] = 1;
        config[8 + 58] = 9;
        config[8 + 59] = 0xa3;
        let plan = protocol::prepare_palette(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Palette::parse("green").unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        print_lighting_plan(&mut out, &plan).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                "Palette plan fixed_on (1) at configuration[59]: yellow (3) -> green (1)"
            )
        );
        assert!(text.contains("speed preserved"));
        let report = Report {
            command: "lighting_palette",
            speed_plan: None,
            palette_plan: Some(plan),
            ..report
        };
        let summary = json_report(&report, false).unwrap();
        assert_eq!(summary["command"], "lighting_palette");
        let plan = &summary["palette_plan"];
        assert_eq!(plan["active_effect"]["id"], 1);
        assert_eq!(
            plan["previous_palette"],
            serde_json::json!({"index":3,"name":"yellow"})
        );
        assert_eq!(
            plan["requested_palette"],
            serde_json::json!({"index":1,"name":"green"})
        );
        assert_eq!(plan["parameter_offset"], 59);
        assert_eq!(
            plan["configuration_diff"],
            serde_json::json!([{"offset":59,"old":163,"new":161}])
        );
        assert_eq!(plan["matrix_changed_bytes"], 0);
        assert_eq!(plan["steps"][0]["command"], 4);
        assert!(plan["steps"][0].get("packet_hex").is_none());
        assert!(plan.get("requested_speed").is_none() && plan.get("requested_effect").is_none());
        assert_eq!(
            json_report(&report, true).unwrap()["palette_plan"]["steps"][0]["packet_hex"]
                .as_str()
                .unwrap()
                .len(),
            1040
        );
    }

    #[test]
    fn uncertain_color_transfers_are_not_reported_as_success_or_retried() {
        let args = ["lighting", "color", "#89b4fa", "--dry-run"].map(String::from);
        let mut report = run(cli::parse(&args).unwrap()); // no discovery or device opens
        report.ok = false;
        report.lighting.as_mut().unwrap().dry_run = false;
        operate(
            &mut report,
            cli::Command::Color(protocol::Color([137, 180, 250])),
            |_| panic!("color commands must not dispatch status"),
            |_, _| {
                Err(Error::new(
                    "lighting_transfer_uncertain",
                    "Synthetic disconnected transfer",
                ))
            },
        );
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["lighting"]["outcome"], "unknown");
        assert_eq!(json["lighting"]["hardware_verified"], false);
        assert_eq!(json["error"]["code"], "lighting_transfer_uncertain");
    }

    #[test]
    fn machine_readable_errors_and_reserved_writes_need_no_hardware() {
        let args = ["lighting", "static", "#89b4fa", "--json"].map(String::from);
        assert_eq!(cli::parse(&args).err().unwrap().code, "approval_required");
        for args in [vec!["lighting", "off"], vec!["lighting", "on"]] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            assert_eq!(cli::parse(&args).err().unwrap().code, "approval_required");
        }
        let report = Report {
            schema_version: 1,
            command: "status",
            ok: false,
            devices: Vec::new(),
            error: Some(Error::new("device_not_found", "No supported candidate")),
            lighting: None,
            static_plan: None,
            static_result: None,
            effect_plan: None,
            effect_result: None,
            brightness_plan: None,
            brightness_result: None,
            speed_plan: None,
            speed_result: None,
            palette_plan: None,
            palette_result: None,
            power_plan: None,
            power_result: None,
        };
        let output = json_report(&report, false).unwrap();
        assert_eq!(output["schema_version"], 1);
        let observed = output["observed_native_effects"].as_array().unwrap();
        assert_eq!(observed.len(), 15);
        assert_eq!(
            observed[0],
            serde_json::json!({"id": 1, "name": "fixed_on"})
        );
        assert!(
            !observed
                .iter()
                .any(|e| matches!(e["id"].as_u64(), Some(9 | 14 | 18)))
        );
        assert_eq!(output["ok"], false);
        assert_eq!(output["error"]["code"], "device_not_found");
        let args = ["status", "--json", "--transport", "wireless"].map(String::from);
        let options = cli::parse(&args).unwrap();
        assert_eq!(options.command, cli::Command::Status);
        assert_eq!(options.transport, Some(device::Transport::Wireless));
    }
}
