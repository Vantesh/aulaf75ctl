//! Reviewed wired static/effect/brightness/speed/palette operations. No retries or rollback.
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::Error;
use crate::device::ReadReport;
use crate::protocol::{self, Effect, LightingPlan, LightingRequest, WriteStep};

#[derive(Serialize)]
pub struct ApplyResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<crate::power::Outcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_rgb: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_effect: Option<Effect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_effect: Option<Effect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_effect: Option<Effect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_brightness: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_brightness: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_speed: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_speed: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_palette: Option<protocol::Palette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_palette: Option<protocol::Palette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_offset: Option<usize>,
    pub configuration_diff: Vec<protocol::ByteChange>,
    pub hid_transport_success: bool,
    pub matrix_write: &'static str,
    pub configuration_write: &'static str,
    pub matrix_readback: &'static str,
    pub configuration_readback: &'static str,
    pub hardware_mode: &'static str,
    pub no_op: bool,
    pub configuration_refresh: bool,
    pub hardware_verified: bool,
    pub visual_verified: bool,
    pub snapshot_path: Option<PathBuf>,
    pub failed_step: Option<&'static str>,
    #[serde(skip)]
    pub error: Option<Error>,
    #[serde(skip)]
    pub configuration: Option<protocol::Configuration>,
    #[serde(skip)]
    pub model: Option<protocol::Model>,
}

impl ApplyResult {
    pub fn new(request: LightingRequest) -> Self {
        Self {
            power: request.power().map(|action| crate::power::Outcome {
                action,
                restoration_target: None,
                restoration_status: "not_loaded",
            }),
            requested_rgb: request.rgb(),
            requested_effect: request.effect(),
            previous_effect: None,
            active_effect: None,
            previous_brightness: None,
            requested_brightness: request.brightness(),
            previous_speed: None,
            requested_speed: request.speed(),
            previous_palette: None,
            requested_palette: request.palette(),
            parameter_offset: None,
            configuration_diff: Vec::new(),
            hid_transport_success: false,
            matrix_write: "not_attempted",
            configuration_write: "not_attempted",
            matrix_readback: "not_attempted",
            configuration_readback: "not_attempted",
            hardware_mode: "unknown",
            no_op: false,
            configuration_refresh: false,
            hardware_verified: false,
            visual_verified: false,
            snapshot_path: None,
            failed_step: None,
            error: None,
            configuration: None,
            model: None,
        }
    }
}

pub fn load_review(path: &Path) -> Result<Value, Error> {
    let invalid = || {
        Error::new(
            "invalid_review",
            "Cannot read a regular, user-owned dry-run JSON file (maximum 256 KiB); rerun --dry-run --details --json",
        )
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.len() > 262144
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(262145)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 262144 {
        return Err(invalid());
    }
    let review: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let plan = review
        .get("power_plan")
        .or_else(|| review.get("palette_plan"))
        .or_else(|| review.get("speed_plan"))
        .or_else(|| review.get("brightness_plan"))
        .or_else(|| review.get("effect_plan"))
        .or_else(|| review.get("static_plan"))
        .unwrap_or(&Value::Null);
    if !plan["review_state"]["configuration_hex"].is_string()
        || !plan["review_state"]["matrix_hex"].is_string()
    {
        return Err(Error::new(
            "incomplete_review",
            "A summary cannot authorize a write; create and inspect a fresh --dry-run --details --json review. No hardware was opened",
        ));
    }
    Ok(review)
}

fn validate_review(plan: &LightingPlan, review: &Value) -> Result<(), Error> {
    // Compare the entire regenerated plan, including old values in every diff
    // and unchanged payload bytes. Never deserialize or send saved packets.
    let expected = serde_json::to_value(plan)
        .map_err(|_| Error::new("invalid_review", "Cannot compare the lighting plan"))?;
    if review["schema_version"] != 1
        || review["command"] != plan.command()
        || review["ok"] != true
        || !review["error"].is_null()
        || review.get("static_result").is_some()
        || review.get("effect_result").is_some()
        || review.get("brightness_result").is_some()
        || review.get("speed_result").is_some()
        || review.get("palette_result").is_some()
        || review.get("power_result").is_some()
        || review[plan.plan_key()] != expected
    {
        return Err(Error::new(
            "review_mismatch",
            "Fresh hardware state/request does not match the reviewed dry-run; no write was sent. Run --dry-run --details --json again and review it",
        ));
    }
    Ok(())
}

// Real I/O is bound to one checked fd; tests inject in-memory replies/sends and
// temporary snapshots. Read-only commands do not call this executor.
pub fn execute(
    request: LightingRequest,
    review: &Value,
    mut read: impl FnMut(ReadReport) -> Result<Vec<u8>, Error>,
    mut send: impl FnMut(&LightingPlan, WriteStep) -> Result<(), Error>,
    save: impl FnOnce(&LightingPlan) -> Result<PathBuf, Error>,
    mut check_access: impl FnMut() -> Result<(), Error>,
    mut power: Option<&mut crate::power::Session>,
) -> ApplyResult {
    let mut result = ApplyResult::new(request);
    let operation = (|| -> Result<(), Error> {
        result.failed_step = Some("model_read");
        result.model = Some(protocol::require_f75_model(&read(ReadReport::Model)?)?);
        result.failed_step = Some("configuration_read");
        let config = protocol::parse_config(&read(ReadReport::Configuration)?)?;
        result.failed_step = Some("matrix_read");
        let matrix = read(ReadReport::StaticMatrix)?;
        let plan = if let Some(action) = request.power() {
            power
                .as_mut()
                .ok_or_else(|| {
                    Error::new(
                        "power_session_required",
                        "No verified USB instance for session restoration",
                    )
                })?
                .prepare(action, &config, &matrix)?
        } else {
            request.prepare(&config, &matrix)?
        };
        if let Some(p) = &plan.power {
            result.requested_effect = plan.requested_effect;
            result.power = Some(crate::power::Outcome {
                action: p.action,
                restoration_target: p.restoration_target,
                restoration_status: p.restoration_status,
            });
        }
        result.previous_effect = plan.previous_effect;
        result.active_effect = plan.active_effect;
        result.previous_brightness = plan.previous_brightness;
        result.previous_speed = plan.previous_speed;
        result.previous_palette = plan.previous_palette;
        result.parameter_offset = plan.parameter_offset;
        result.configuration_diff = plan.configuration_diff.clone();
        result.failed_step = Some("review_validation");
        validate_review(&plan, review)?;
        result.no_op = plan.no_op;
        result.configuration_refresh = plan.configuration_refresh;
        if !plan.no_op {
            result.failed_step = Some("access_validation");
            check_access()?;
            result.failed_step = Some("snapshot");
            result.snapshot_path = Some(save(&plan)?);
            if let Some(session) = power.as_mut() {
                result.failed_step = Some("restoration_pending");
                session.begin(&plan, result.snapshot_path.as_ref().unwrap())?;
            }
        }
        if plan.matrix_diff.is_empty() {
            result.matrix_write = "skipped_unchanged";
        } else {
            result.failed_step = Some("matrix_access");
            check_access()?;
            result.failed_step = Some("matrix_write");
            result.matrix_write = "uncertain";
            send(&plan, WriteStep::Matrix)?;
            result.matrix_write = "accepted";
        }
        if plan.no_op {
            result.configuration_write = "skipped_unchanged";
        } else {
            result.failed_step = Some("configuration_access");
            check_access()?;
            result.failed_step = Some("configuration_write");
            result.configuration_write = "uncertain";
            send(&plan, WriteStep::Configuration)?;
            result.configuration_write = "accepted";
        }
        result.hid_transport_success = true;

        result.failed_step = Some("matrix_readback");
        result.matrix_readback = "failed";
        let reply = read(ReadReport::StaticMatrix)?;
        let actual = protocol::parse_matrix(&reply)?;
        if actual != &plan.matrix {
            result.matrix_readback = "mismatch";
            return Err(Error::new(
                "matrix_readback_mismatch",
                "Stored matrix differs from the intended 384 bytes; stop, inspect the snapshot and do not retry or restore automatically",
            ));
        }
        result.matrix_readback = "verified";
        result.failed_step = Some("configuration_readback");
        result.configuration_readback = "failed";
        let actual = protocol::parse_config(&read(ReadReport::Configuration)?)?;
        result.hardware_mode = actual.lighting_mode;
        let matches = actual.bytes == plan.configuration;
        result.configuration = Some(actual);
        if !matches {
            result.configuration_readback = "mismatch";
            return Err(Error::new(
                "configuration_readback_mismatch",
                "Configuration differs from the intended 128 bytes; stop, inspect the snapshot and do not retry or restore automatically",
            ));
        }
        result.configuration_readback = "verified";
        if !plan.no_op
            && let Some(session) = power.as_mut()
        {
            result.failed_step = Some("restoration_commit");
            session.finish(request.power().expect("power transaction"))?;
        }
        result.failed_step = None;
        Ok(())
    })();
    result.error = operation.err();
    if let (Some(outcome), Some(session)) = (&mut result.power, power) {
        outcome.restoration_status = session.status;
    }
    result
}

pub fn save_snapshot(plan: &LightingPlan) -> Result<PathBuf, Error> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|s| PathBuf::from(s).join(".local/state")))
        .ok_or_else(snapshot_error)?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| snapshot_error())?;
    let name = format!(
        "{}-{}-{}-{}.json",
        if let Some(power) = &plan.power {
            power.action.snapshot_prefix()
        } else if plan.requested_palette.is_some() {
            "palette"
        } else if plan.requested_speed.is_some() {
            "speed"
        } else if plan.requested_brightness.is_some() {
            "brightness"
        } else if plan.requested_effect.is_some() {
            "effect"
        } else {
            "static"
        },
        time.as_secs(),
        time.subsec_nanos(),
        std::process::id()
    );
    save_snapshot_at(&state, &name, time.as_millis(), plan)
}

fn snapshot_error() -> Error {
    Error::new(
        "snapshot_failed",
        "Cannot safely create/sync a private recovery snapshot; no persistent write was sent. Check XDG_STATE_HOME and disk space",
    )
}

fn save_snapshot_at(
    state: &Path,
    name: &str,
    timestamp: u128,
    plan: &LightingPlan,
) -> Result<PathBuf, Error> {
    let uid = nix::unistd::geteuid().as_raw();
    if !state.is_absolute() {
        return Err(snapshot_error());
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)
        .map_err(|_| snapshot_error())?;
    let metadata = fs::symlink_metadata(state).map_err(|_| snapshot_error())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err(snapshot_error());
    }
    let directory = state.join("aulaf75ctl");
    match DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(snapshot_error()),
    }
    let metadata = fs::symlink_metadata(&directory).map_err(|_| snapshot_error())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Err(snapshot_error());
    }
    let path = directory.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| snapshot_error())?;
    let metadata = file.metadata().map_err(|_| snapshot_error())?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(snapshot_error());
    }
    let mut snapshot = json!({"schema_version": 1, "metadata": {
        "usb_id": "258a:010c", "interface_number": 1, "model_identity": "f75_cd",
        "model_reply": protocol::F75_MODEL, "timestamp_unix_ms": timestamp,
        "intended_operation": {"command": plan.command(),
            "configuration_offsets": plan.configuration_diff.iter().map(|c| c.offset).collect::<Vec<_>>(),
            "configuration_refresh": plan.configuration_refresh}},
        "raw": {"configuration": plan.original_configuration.as_slice(),
            "matrix": plan.original_matrix.as_slice()}});
    if let Some(rgb) = plan.rgb {
        snapshot["metadata"]["intended_operation"]["rgb"] = json!(rgb);
    }
    if let Some(power) = &plan.power {
        snapshot["metadata"]["intended_operation"]["power"] = json!(power);
    }
    if let Some(effect) = plan.requested_effect {
        snapshot["metadata"]["intended_operation"]["effect"] = json!(effect);
    }
    if let Some(level) = plan.requested_brightness {
        snapshot["metadata"]["intended_operation"]["active_effect"] = json!(plan.active_effect);
        snapshot["metadata"]["intended_operation"]["previous_brightness"] =
            json!(plan.previous_brightness);
        snapshot["metadata"]["intended_operation"]["requested_brightness"] = json!(level);
        snapshot["metadata"]["intended_operation"]["parameter_offset"] =
            json!(plan.parameter_offset);
    }
    if let Some(level) = plan.requested_speed {
        snapshot["metadata"]["intended_operation"]["active_effect"] = json!(plan.active_effect);
        snapshot["metadata"]["intended_operation"]["previous_speed"] = json!(plan.previous_speed);
        snapshot["metadata"]["intended_operation"]["requested_speed"] = json!(level);
        snapshot["metadata"]["intended_operation"]["parameter_offset"] =
            json!(plan.parameter_offset);
    }
    if let Some(palette) = plan.requested_palette {
        snapshot["metadata"]["intended_operation"]["active_effect"] = json!(plan.active_effect);
        snapshot["metadata"]["intended_operation"]["previous_palette"] =
            json!(plan.previous_palette);
        snapshot["metadata"]["intended_operation"]["requested_palette"] = json!(palette);
        snapshot["metadata"]["intended_operation"]["parameter_offset"] =
            json!(plan.parameter_offset);
    }
    serde_json::to_writer(&mut file, &snapshot).map_err(|_| snapshot_error())?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(|_| snapshot_error())?;
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(&directory)
        .and_then(|d| d.sync_all())
        .map_err(|_| snapshot_error())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Color;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    fn power_runtime() -> (PathBuf, crate::power::Instance) {
        let root = std::env::temp_dir().join(format!(
            "aula-power-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        (
            root,
            crate::power::Instance {
                boot_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
                hid_id: 7,
                usb_bus: 1,
                usb_address: 5,
                usb_inode: 42,
                usb_ctime: 100,
                usb_ctime_ns: 12,
            },
        )
    }

    fn power_exchange(
        session: &mut crate::power::Session,
        root: &Path,
        action: crate::power::Action,
        config: &[u8],
        matrix: &[u8],
        failure: &str,
    ) -> (ApplyResult, Vec<u8>, Vec<String>) {
        let (model, _, _, _, _) = fixture();
        let plan = session
            .prepare(action, &protocol::parse_config(config).unwrap(), matrix)
            .unwrap();
        let mut review = json!({"schema_version":1,"command":action.command(),"ok":true,"error":null,"power_plan":plan});
        if failure == "wrong_review" {
            review["command"] = json!(if action == crate::power::Action::Off {
                "lighting_on"
            } else {
                "lighting_off"
            });
        }
        let mut fresh = config.to_vec();
        if failure == "stale_review" {
            fresh[8 + 26] ^= 1;
        }
        let mut after = protocol::config_request().to_vec();
        after[8..136].copy_from_slice(&plan.configuration);
        let mut matrix_after = matrix.to_vec();
        if failure == "matrix_mismatch" {
            matrix_after[391] ^= 1;
        }
        if failure == "configuration_mismatch" {
            after[8 + 26] ^= 1;
        }
        let mut replies = [
            (ReadReport::Model, model),
            (ReadReport::Configuration, fresh),
            (ReadReport::StaticMatrix, matrix.to_vec()),
            (ReadReport::StaticMatrix, matrix_after),
            (ReadReport::Configuration, after.clone()),
        ]
        .into_iter()
        .enumerate();
        let trace = RefCell::new(Vec::new());
        let result = execute(
            LightingRequest::Power(action),
            &review,
            |kind| {
                let (i, (expected, reply)) = replies.next().unwrap();
                assert_eq!(kind, expected);
                trace.borrow_mut().push(format!("read:{kind:?}"));
                if failure == "commit" && i == 4 {
                    fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
                }
                Ok(reply)
            },
            |fresh, step| {
                assert_eq!(step, WriteStep::Configuration);
                assert_eq!(fresh.write_packet(step).unwrap()[1], 4);
                trace.borrow_mut().push("write:04".into());
                if failure == "write" {
                    Err(Error::new("device_io", "Synthetic uncertain Off/On write"))
                } else {
                    Ok(())
                }
            },
            |fresh| {
                trace.borrow_mut().push("snapshot".into());
                if failure == "snapshot" {
                    return Err(snapshot_error());
                }
                let time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                save_snapshot_at(
                    &root.join("state"),
                    &format!("power-{time}.json"),
                    time,
                    fresh,
                )
            },
            || Ok(()),
            Some(session),
        );
        if failure == "commit" {
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        (result, after, trace.into_inner())
    }

    #[test]
    fn canonical_power_record_rejects_symlinks_and_unsafe_permissions() {
        use crate::power::Session;
        let (root, instance) = power_runtime();
        let path = root.join("aulaf75ctl-power.json");
        let target = root.join("unrelated.json");
        fs::write(&target, b"unrelated state").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(
            Session::load_at(instance.clone(), root.clone())
                .err()
                .unwrap()
                .code,
            "restoration_corrupt"
        );
        assert_eq!(fs::read(&target).unwrap(), b"unrelated state");
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            Session::load_at(instance.clone(), root.clone())
                .err()
                .unwrap()
                .code,
            "restoration_corrupt"
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            Session::load_at(instance.clone(), root.clone())
                .err()
                .unwrap()
                .code,
            "restoration_corrupt"
        );
        fs::remove_file(&path).unwrap();
        let session = Session::load_at(instance.clone(), root.clone()).unwrap();
        assert_eq!(session.status, "missing");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            Session::load_at(instance, root.clone()).err().unwrap().code,
            "restoration_storage"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_power_roundtrip_restarts_noops_and_full_state_preservation() {
        use crate::power::{Action, Session};
        for effect in [1, 21] {
            let (root, instance) = power_runtime();
            let (_, mut config, matrix, _, _) = fixture();
            config[8 + 9] = u8::from(effect == 21);
            config[8 + 10] = effect;
            let mut session = Session::load_at(instance.clone(), root.clone()).unwrap();
            let (already_on, _, trace) =
                power_exchange(&mut session, &root, Action::On, &config, &matrix, "");
            assert!(already_on.error.is_none() && already_on.no_op);
            assert!(!trace.iter().any(|s| s == "snapshot" || s == "write:04"));
            let (off, off_config, trace) =
                power_exchange(&mut session, &root, Action::Off, &config, &matrix, "");
            assert!(off.error.is_none() && !off.no_op);
            assert_eq!(trace.iter().filter(|s| *s == "write:04").count(), 1);
            assert_eq!(
                off.power.as_ref().unwrap().restoration_target.unwrap().id,
                effect
            );
            assert_eq!(
                off.power.as_ref().unwrap().restoration_status,
                "off_verified"
            );
            assert_eq!(off.matrix_write, "skipped_unchanged");
            assert_eq!(off.configuration_readback, "verified");
            for i in 0..128 {
                assert_eq!(
                    off_config[8 + i],
                    if matches!(i, 9 | 10) {
                        0
                    } else {
                        config[8 + i]
                    }
                );
            }
            let path = root.join("aulaf75ctl-power.json");
            let record = fs::read(&path).unwrap();
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
            let json: Value = serde_json::from_slice(&record).unwrap();
            assert_eq!(json["previous_effect"], effect);
            assert!(json.get("raw").is_none());
            drop(session);
            let mut session = Session::load_at(instance.clone(), root.clone()).unwrap();
            let (again, _, trace) =
                power_exchange(&mut session, &root, Action::Off, &off_config, &matrix, "");
            assert!(again.error.is_none() && again.no_op && again.snapshot_path.is_none());
            assert!(trace.iter().all(|s| s != "write:04" && s != "snapshot"));
            assert_eq!(fs::read(&path).unwrap(), record);
            let plan = session
                .prepare(
                    Action::On,
                    &protocol::parse_config(&off_config).unwrap(),
                    &matrix,
                )
                .unwrap();
            assert_eq!(plan.command(), "lighting_on");
            assert_eq!(plan.plan_key(), "power_plan");
            assert!(plan.matrix_diff.is_empty());
            assert!(plan.write_packet(WriteStep::Matrix).is_err());
            let (on, on_config, trace) =
                power_exchange(&mut session, &root, Action::On, &off_config, &matrix, "");
            assert!(on.error.is_none());
            assert_eq!(trace.iter().filter(|s| *s == "write:04").count(), 1);
            assert_eq!(on.power.as_ref().unwrap().restoration_status, "on_verified");
            assert_eq!(&on_config[8..136], &config[8..136]);
            assert!(!on.hardware_verified && !on.visual_verified);
            let record = fs::read(&path).unwrap();
            let mut session = Session::load_at(instance, root.clone()).unwrap();
            let (again, _, trace) =
                power_exchange(&mut session, &root, Action::On, &on_config, &matrix, "");
            assert!(again.error.is_none() && again.no_op && again.snapshot_path.is_none());
            assert!(trace.iter().all(|s| s != "write:04" && s != "snapshot"));
            assert_eq!(fs::read(path).unwrap(), record);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn session_power_uncertain_transfers_and_commit_failures_remain_pending() {
        use crate::power::{Action, Session};
        for action in [Action::Off, Action::On] {
            for failure in [
                "wrong_review",
                "stale_review",
                "snapshot",
                "write",
                "matrix_mismatch",
                "configuration_mismatch",
                "commit",
            ] {
                let (root, instance) = power_runtime();
                let (_, mut config, matrix, _, _) = fixture();
                let mut session = Session::load_at(instance.clone(), root.clone()).unwrap();
                if action == Action::On {
                    config =
                        power_exchange(&mut session, &root, Action::Off, &config, &matrix, "").1;
                }
                let before = fs::read(root.join("aulaf75ctl-power.json")).ok();
                let (result, _, trace) =
                    power_exchange(&mut session, &root, action, &config, &matrix, failure);
                let code = result.error.as_ref().unwrap().code;
                let attempted = matches!(
                    failure,
                    "write" | "matrix_mismatch" | "configuration_mismatch" | "commit"
                );
                assert_eq!(
                    trace.iter().filter(|s| *s == "write:04").count(),
                    usize::from(attempted)
                );
                if !attempted {
                    assert_eq!(fs::read(root.join("aulaf75ctl-power.json")).ok(), before);
                } else {
                    assert!(result.snapshot_path.as_ref().unwrap().exists());
                    assert_eq!(
                        result.configuration_write,
                        if failure == "write" {
                            "uncertain"
                        } else {
                            "accepted"
                        }
                    );
                    let mut restarted = Session::load_at(instance, root.clone()).unwrap();
                    assert_eq!(
                        restarted
                            .prepare(
                                Action::On,
                                &protocol::parse_config(&config).unwrap(),
                                &matrix
                            )
                            .unwrap_err()
                            .code,
                        "restoration_interrupted"
                    );
                    assert_eq!(
                        result.power.as_ref().unwrap().restoration_status,
                        if action == Action::Off {
                            "off_pending"
                        } else {
                            "on_pending"
                        }
                    );
                }
                if failure == "commit" {
                    assert_eq!(result.configuration_readback, "verified");
                    assert_eq!(result.failed_step, Some("restoration_commit"));
                }
                if failure == "wrong_review" || failure == "stale_review" {
                    assert_eq!(
                        code,
                        if action == Action::On && failure == "stale_review" {
                            "restoration_stale"
                        } else {
                            "review_mismatch"
                        }
                    );
                }
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn session_power_rejects_reenumeration_stale_state_and_lost_or_corrupt_records() {
        use crate::power::{Action, Session};
        let (root, instance) = power_runtime();
        let (_, config, matrix, _, _) = fixture();
        let mut session = Session::load_at(instance.clone(), root.clone()).unwrap();
        let off = power_exchange(&mut session, &root, Action::Off, &config, &matrix, "").1;
        let parsed = protocol::parse_config(&off).unwrap();
        for change in 0..4 {
            let mut other = instance.clone();
            match change {
                0 => other.hid_id += 1,
                1 => other.usb_address += 1,
                2 => other.boot_id = "11234567-89ab-cdef-0123-456789abcdef".into(),
                _ => other.usb_inode += 1,
            };
            let mut session = Session::load_at(other, root.clone()).unwrap();
            assert_eq!(
                session
                    .prepare(Action::On, &parsed, &matrix)
                    .unwrap_err()
                    .code,
                "restoration_instance_changed"
            );
        }
        let mut changed = off.clone();
        changed[8 + 26] ^= 1;
        assert_eq!(
            session
                .prepare(
                    Action::On,
                    &protocol::parse_config(&changed).unwrap(),
                    &matrix
                )
                .unwrap_err()
                .code,
            "restoration_stale"
        );
        let mut changed = matrix.clone();
        changed[391] ^= 1;
        assert_eq!(
            session
                .prepare(Action::On, &parsed, &changed)
                .unwrap_err()
                .code,
            "restoration_stale"
        );
        assert_eq!(
            session
                .prepare(
                    Action::On,
                    &protocol::parse_config(&config).unwrap(),
                    &matrix
                )
                .unwrap_err()
                .code,
            "restoration_conflict"
        );
        let record_path = root.join("aulaf75ctl-power.json");
        let record = fs::read(&record_path).unwrap();
        let json: Value = serde_json::from_slice(&record).unwrap();
        let snapshot = PathBuf::from(json["off_snapshot"].as_str().unwrap());
        let original = fs::read(&snapshot).unwrap();
        fs::write(&snapshot, b"broken").unwrap();
        assert_eq!(
            session
                .prepare(Action::On, &parsed, &matrix)
                .unwrap_err()
                .code,
            "restoration_corrupt"
        );
        fs::write(&snapshot, original).unwrap();
        fs::remove_file(&record_path).unwrap();
        let mut missing = Session::load_at(instance.clone(), root.clone()).unwrap();
        assert_eq!(
            missing
                .prepare(Action::On, &parsed, &matrix)
                .unwrap_err()
                .code,
            "restoration_missing"
        );
        assert!(
            missing
                .prepare(Action::Off, &parsed, &matrix)
                .unwrap()
                .no_op
        );
        for invalid in [b"broken".as_slice(), b"{}".as_slice()] {
            fs::write(&record_path, invalid).unwrap();
            fs::set_permissions(&record_path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(Session::load_at(instance.clone(), root.clone()).is_err());
        }
        fs::write(&record_path, &record).unwrap();
        assert_eq!(
            missing
                .prepare(Action::Off, &parsed, &matrix)
                .unwrap_err()
                .code,
            "restoration_changed"
        );
        fs::set_permissions(&record_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Session::load_at(instance, root.clone()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_power_crash_after_pending_sync_never_invents_verification() {
        use crate::power::{Action, Session};
        for action in [Action::Off, Action::On] {
            let (root, instance) = power_runtime();
            let (_, mut config, matrix, _, _) = fixture();
            let mut session = Session::load_at(instance.clone(), root.clone()).unwrap();
            if action == Action::On {
                config = power_exchange(&mut session, &root, Action::Off, &config, &matrix, "").1;
            }
            let plan = session
                .prepare(action, &protocol::parse_config(&config).unwrap(), &matrix)
                .unwrap();
            let snapshot =
                save_snapshot_at(&root.join("state"), "interrupted.json", 10, &plan).unwrap();
            session.begin(&plan, &snapshot).unwrap();
            drop(session);
            let mut restarted = Session::load_at(instance, root.clone()).unwrap();
            for payload in [plan.original_configuration, plan.configuration] {
                let mut reply = protocol::config_request();
                reply[8..136].copy_from_slice(&payload);
                assert_eq!(
                    restarted
                        .prepare(
                            Action::On,
                            &protocol::parse_config(&reply).unwrap(),
                            &matrix
                        )
                        .unwrap_err()
                        .code,
                    "restoration_interrupted"
                );
            }
            assert!(snapshot.exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, LightingPlan, Value) {
        let mut model = protocol::model_request().to_vec();
        model[8..14].copy_from_slice(&protocol::F75_MODEL);
        let mut config = protocol::config_request().to_vec();
        for (i, byte) in config[8..136].iter_mut().enumerate() {
            *byte = i as u8;
        }
        for (offset, value) in [
            (9, 0),
            (10, 1),
            (58, 1),
            (59, 51),
            (94, 9),
            (95, 0),
            (126, 0x5a),
            (127, 0xa5),
        ] {
            config[8 + offset] = value;
        }
        let mut matrix = protocol::matrix_request().to_vec();
        matrix[8..392].fill(0x55);
        let plan = protocol::prepare_static(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            Color([137, 180, 250]),
        )
        .unwrap();
        let review = json!({"schema_version":1,"command":"lighting_static","ok":true,"error":null,"static_plan":plan});
        (model, config, matrix, plan, review)
    }

    fn synthetic(failure: &str) -> (ApplyResult, Vec<String>) {
        synthetic_case("first", failure, Color([137, 180, 250]))
    }

    fn synthetic_case(case: &str, failure: &str, color: Color) -> (ApplyResult, Vec<String>) {
        synthetic_request(case, failure, LightingRequest::Static(color))
    }

    fn synthetic_request(
        case: &str,
        failure: &str,
        request: LightingRequest,
    ) -> (ApplyResult, Vec<String>) {
        let (mut model, mut config, mut matrix, _, _) = fixture();
        if let Some(effect) = request.effect() {
            for slot in 0..20 {
                config[8 + 58 + slot * 2] = 9;
                config[8 + 59 + slot * 2] = 0x23;
            }
            if case == "effect_noop" {
                config[8 + 10] = effect.id;
                config[8 + 9] = 1;
            }
        }
        if let Some(level) = request.brightness() {
            config[8 + 9] = 1;
            config[8 + 10] = 21;
            config[8 + 94] = if case == "brightness_noop" { level } else { 9 };
        }
        if let Some(level) = request.speed() {
            config[8 + 9] = 0;
            config[8 + 10] = 3;
            config[8 + 62] = 9;
            config[8 + 63] = (if case == "speed_noop" { level } else { 3 } << 4) | 0x0f;
        }
        if let Some(palette) = request.palette() {
            config[8 + 9] = 0;
            config[8 + 10] = 1;
            config[8 + 58] = 9;
            config[8 + 59] = 0xa0
                | if case == "palette_noop" {
                    palette.index
                } else {
                    3
                };
        }
        if matches!(case, "subsequent" | "noop") {
            config[8 + 9] = 1;
            config[8 + 10] = 21;
        }
        if matches!(case, "noop" | "config_only") {
            let desired = request
                .prepare(&protocol::parse_config(&config).unwrap(), &matrix)
                .unwrap();
            matrix[8..392].copy_from_slice(&desired.matrix);
        }
        let plan = request
            .prepare(&protocol::parse_config(&config).unwrap(), &matrix)
            .unwrap();
        let mut review =
            json!({"schema_version":1,"command":plan.command(),"ok":true,"error":null});
        review[plan.plan_key()] = serde_json::to_value(&plan).unwrap();
        match failure {
            "model" => model[13] ^= 1,
            "signature" => config[134] ^= 1,
            "matrix_header" => matrix[1] ^= 1,
            "baseline" => config[8 + 94] = 8,
            "already_applied" => {
                config[8 + 9] = 1;
                config[8 + 10] = 21;
            }
            "wrong_review" => review["command"] = json!("status"),
            "wrong_brightness" => review["brightness_plan"]["requested_brightness"] = json!(6),
            "wrong_speed" => review["speed_plan"]["requested_speed"] = json!(1),
            "wrong_palette" => review["palette_plan"]["requested_palette"]["index"] = json!(1),
            "palette_nibble_changed" => config[8 + 59] ^= 1,
            "palette_speed_changed" => config[8 + 59] ^= 0x10,
            "speed_changed" => config[8 + 63] ^= 0x10,
            "palette_changed" => config[8 + 63] ^= 1,
            "stale_config" => config[8 + 38] ^= 1,
            "stale_matrix" => matrix[8] ^= 1,
            "stale_padding" => matrix[391] ^= 1,
            _ => {}
        }
        let mut config_after = protocol::config_request().to_vec();
        config_after[8..136].copy_from_slice(&plan.configuration);
        let mut matrix_after = protocol::matrix_request().to_vec();
        matrix_after[8..392].copy_from_slice(&plan.matrix);
        if failure == "matrix_mismatch" {
            matrix_after[391] ^= 1;
        }
        if failure == "configuration_mismatch" {
            config_after[8 + 38] ^= 1;
        }
        if failure == "signature_readback" {
            config_after[134] ^= 1;
        }
        let mut replies = [
            (ReadReport::Model, model),
            (ReadReport::Configuration, config),
            (ReadReport::StaticMatrix, matrix),
            (ReadReport::StaticMatrix, matrix_after),
            (ReadReport::Configuration, config_after),
        ]
        .into_iter()
        .enumerate();
        let trace = RefCell::new(Vec::new());
        let mut access_checks = 0;
        let result = execute(
            request,
            &review,
            |kind| {
                trace.borrow_mut().push(format!("read:{kind:?}"));
                let (index, (expected, reply)) = replies.next().unwrap();
                assert_eq!(kind, expected);
                if (index == 3 && failure == "matrix_readback")
                    || (index == 4 && failure == "configuration_readback")
                {
                    Err(Error::new("disconnected", "Synthetic readback disconnect"))
                } else {
                    Ok(reply)
                }
            },
            |fresh, step| {
                trace.borrow_mut().push(format!("write:{step:?}"));
                assert_eq!(
                    fresh.write_packet(step).unwrap(),
                    plan.write_packet(step).unwrap()
                );
                if (step == WriteStep::Matrix && failure == "matrix_write")
                    || (step == WriteStep::Configuration && failure == "configuration_write")
                {
                    Err(Error::new("device_io", "Synthetic uncertain transfer"))
                } else {
                    Ok(())
                }
            },
            |fresh| {
                trace.borrow_mut().push("snapshot".into());
                assert_eq!(fresh.original_configuration, plan.original_configuration);
                if failure == "snapshot" {
                    Err(snapshot_error())
                } else {
                    Ok(PathBuf::from("synthetic-recovery.json"))
                }
            },
            || {
                let blocked = matches!(
                    (failure, access_checks),
                    ("controller_preflight", 0)
                        | ("controller_matrix", 1)
                        | ("controller_configuration", 2)
                );
                access_checks += 1;
                if blocked {
                    Err(Error::new(
                        "competing_controller",
                        "Synthetic competing controller",
                    ))
                } else {
                    Ok(())
                }
            },
            None,
        );
        (result, trace.into_inner())
    }

    #[test]
    fn snapshot_precedes_single_ordered_writes_and_exact_readbacks() {
        let (result, trace) = synthetic("");
        assert!(result.error.is_none());
        assert_eq!(
            trace,
            [
                "read:Model",
                "read:Configuration",
                "read:StaticMatrix",
                "snapshot",
                "write:Matrix",
                "write:Configuration",
                "read:StaticMatrix",
                "read:Configuration"
            ]
        );
        assert!(result.hid_transport_success);
        assert_eq!(result.matrix_readback, "verified");
        assert_eq!(result.configuration_readback, "verified");
        assert_eq!(result.hardware_mode, "per_key");
        assert!(!result.hardware_verified && !result.visual_verified);
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["matrix_write"], "accepted");
        assert_eq!(json["configuration_write"], "accepted");
        assert!(json["failed_step"].is_null());
    }

    #[test]
    fn effect_execution_is_configuration_only_and_noops_skip_snapshot_and_transfers() {
        let request = LightingRequest::Effect(Effect::from_id(3).unwrap());
        let (result, trace) = synthetic_request("effect", "", request);
        assert!(result.error.is_none());
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_write, "accepted");
        assert_eq!(result.matrix_readback, "verified");
        assert_eq!(result.configuration_readback, "verified");
        assert_eq!(result.requested_effect.unwrap().id, 3);
        assert_eq!(result.previous_effect.unwrap().id, 1);
        assert!(result.requested_rgb.is_none());
        assert_eq!(
            trace,
            [
                "read:Model",
                "read:Configuration",
                "read:StaticMatrix",
                "snapshot",
                "write:Configuration",
                "read:StaticMatrix",
                "read:Configuration"
            ]
        );
        assert!(result.snapshot_path.is_some());
        assert!(!result.visual_verified && !result.hardware_verified);
        let (result, trace) = synthetic_request("effect_noop", "", request);
        assert!(result.error.is_none() && result.no_op);
        assert!(result.snapshot_path.is_none());
        assert!(result.configuration_diff.is_empty());
        assert_eq!(result.configuration_write, "skipped_unchanged");
        assert!(
            trace
                .iter()
                .all(|s| !s.starts_with("write:") && s != "snapshot")
        );
    }

    #[test]
    fn brightness_reuses_single_write_readbacks_noops_and_failure_guards() {
        let request = LightingRequest::Brightness(protocol::Brightness::parse("5").unwrap());
        let (result, trace) = synthetic_request("brightness", "", request);
        assert!(result.error.is_none() && !result.no_op);
        assert_eq!(result.requested_brightness, Some(5));
        assert_eq!(result.previous_brightness, Some(9));
        assert_eq!(result.parameter_offset, Some(94));
        assert_eq!(result.active_effect.unwrap().id, 21);
        assert!(result.requested_effect.is_none() && result.requested_rgb.is_none());
        assert_eq!(result.configuration_diff.len(), 1);
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_write, "accepted");
        assert_eq!(result.matrix_readback, "verified");
        assert_eq!(result.configuration_readback, "verified");
        assert!(!result.visual_verified && !result.hardware_verified);
        assert_eq!(
            trace,
            [
                "read:Model",
                "read:Configuration",
                "read:StaticMatrix",
                "snapshot",
                "write:Configuration",
                "read:StaticMatrix",
                "read:Configuration"
            ]
        );
        let (result, trace) = synthetic_request("brightness_noop", "", request);
        assert!(result.error.is_none() && result.no_op && result.snapshot_path.is_none());
        assert_eq!(result.configuration_write, "skipped_unchanged");
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_readback, "verified");
        assert_eq!(result.matrix_readback, "verified");
        assert!(
            trace
                .iter()
                .all(|s| !s.starts_with("write:") && s != "snapshot")
        );
        for (failure, code, writes) in [
            ("stale_config", "review_mismatch", 0),
            ("stale_padding", "review_mismatch", 0),
            ("baseline", "review_mismatch", 0),
            ("wrong_review", "review_mismatch", 0),
            ("wrong_brightness", "review_mismatch", 0),
            ("snapshot", "snapshot_failed", 0),
            ("configuration_write", "device_io", 1),
            (
                "configuration_mismatch",
                "configuration_readback_mismatch",
                1,
            ),
            ("matrix_mismatch", "matrix_readback_mismatch", 1),
        ] {
            let (result, trace) = synthetic_request("brightness", failure, request);
            assert_eq!(result.error.unwrap().code, code, "{failure}");
            assert_eq!(
                trace.iter().filter(|s| s.starts_with("write:")).count(),
                writes
            );
            assert!(!trace.iter().any(|s| s == "write:Matrix"));
            if writes == 1 {
                assert!(result.snapshot_path.is_some());
                assert_eq!(
                    result.configuration_write,
                    if failure == "configuration_write" {
                        "uncertain"
                    } else {
                        "accepted"
                    }
                );
            }
        }
    }

    #[test]
    fn speed_reuses_configuration_only_execution_noops_and_failure_guards() {
        let request = LightingRequest::Speed(protocol::Speed::parse("0").unwrap());
        let (result, trace) = synthetic_request("speed", "", request);
        assert!(result.error.is_none() && !result.no_op);
        assert_eq!(result.active_effect.unwrap().id, 3);
        assert_eq!(result.previous_speed, Some(3));
        assert_eq!(result.requested_speed, Some(0));
        assert_eq!(result.parameter_offset, Some(63));
        assert!(
            result.requested_rgb.is_none()
                && result.requested_effect.is_none()
                && result.requested_brightness.is_none()
        );
        assert_eq!(
            serde_json::to_value(&result.configuration_diff).unwrap(),
            json!([{"offset":63,"old":63,"new":15}])
        );
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_write, "accepted");
        assert_eq!(result.matrix_readback, "verified");
        assert_eq!(result.configuration_readback, "verified");
        assert!(!result.hardware_verified && !result.visual_verified);
        assert_eq!(
            trace,
            [
                "read:Model",
                "read:Configuration",
                "read:StaticMatrix",
                "snapshot",
                "write:Configuration",
                "read:StaticMatrix",
                "read:Configuration"
            ]
        );
        let (result, trace) = synthetic_request("speed_noop", "", request);
        assert!(result.error.is_none() && result.no_op && result.snapshot_path.is_none());
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_write, "skipped_unchanged");
        assert_eq!(result.matrix_readback, "verified");
        assert_eq!(result.configuration_readback, "verified");
        assert!(
            trace
                .iter()
                .all(|s| !s.starts_with("write:") && s != "snapshot")
        );
        for (failure, code, writes) in [
            ("speed_changed", "review_mismatch", 0),
            ("palette_changed", "review_mismatch", 0),
            ("stale_padding", "review_mismatch", 0),
            ("wrong_speed", "review_mismatch", 0),
            ("wrong_review", "review_mismatch", 0),
            ("snapshot", "snapshot_failed", 0),
            ("configuration_write", "device_io", 1),
            (
                "configuration_mismatch",
                "configuration_readback_mismatch",
                1,
            ),
            ("matrix_mismatch", "matrix_readback_mismatch", 1),
        ] {
            let (result, trace) = synthetic_request("speed", failure, request);
            assert_eq!(result.error.unwrap().code, code, "{failure}");
            assert_eq!(
                trace.iter().filter(|s| s.starts_with("write:")).count(),
                writes
            );
            assert!(!trace.iter().any(|s| s == "write:Matrix"));
            if writes == 1 {
                assert!(result.snapshot_path.is_some());
                assert_eq!(
                    result.configuration_write,
                    if failure == "configuration_write" {
                        "uncertain"
                    } else {
                        "accepted"
                    }
                );
            } else {
                assert!(result.snapshot_path.is_none());
            }
        }
    }

    #[test]
    fn palette_reuses_single_write_noops_and_failure_handling() {
        let request = LightingRequest::Palette(protocol::Palette::parse("blue").unwrap());
        let (result, trace) = synthetic_request("palette", "", request);
        assert!(result.error.is_none() && !result.no_op);
        assert_eq!(result.active_effect.unwrap().id, 1);
        assert_eq!(result.previous_palette.unwrap().name, "yellow");
        assert_eq!(result.requested_palette.unwrap().index, 2);
        assert_eq!(result.parameter_offset, Some(59));
        assert!(result.requested_speed.is_none() && result.requested_rgb.is_none());
        assert_eq!(
            serde_json::to_value(&result.configuration_diff).unwrap(),
            json!([{"offset":59,"old":163,"new":162}])
        );
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_write, "accepted");
        assert_eq!(result.matrix_readback, "verified");
        assert_eq!(result.configuration_readback, "verified");
        assert!(!result.visual_verified && !result.hardware_verified);
        assert_eq!(
            trace,
            [
                "read:Model",
                "read:Configuration",
                "read:StaticMatrix",
                "snapshot",
                "write:Configuration",
                "read:StaticMatrix",
                "read:Configuration"
            ]
        );
        let (result, trace) = synthetic_request("palette_noop", "", request);
        assert!(result.error.is_none() && result.no_op && result.snapshot_path.is_none());
        assert_eq!(result.configuration_write, "skipped_unchanged");
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert_eq!(result.configuration_readback, "verified");
        assert_eq!(result.matrix_readback, "verified");
        assert!(
            trace
                .iter()
                .all(|s| !s.starts_with("write:") && s != "snapshot")
        );
        for (failure, code, writes) in [
            ("palette_nibble_changed", "review_mismatch", 0),
            ("palette_speed_changed", "review_mismatch", 0),
            ("stale_padding", "review_mismatch", 0),
            ("wrong_palette", "review_mismatch", 0),
            ("wrong_review", "review_mismatch", 0),
            ("snapshot", "snapshot_failed", 0),
            ("configuration_write", "device_io", 1),
            (
                "configuration_mismatch",
                "configuration_readback_mismatch",
                1,
            ),
            ("matrix_mismatch", "matrix_readback_mismatch", 1),
        ] {
            let (result, trace) = synthetic_request("palette", failure, request);
            assert_eq!(result.error.unwrap().code, code, "{failure}");
            assert_eq!(
                trace.iter().filter(|s| s.starts_with("write:")).count(),
                writes
            );
            assert!(!trace.iter().any(|s| s == "write:Matrix"));
            assert_eq!(result.snapshot_path.is_some(), writes == 1);
            if writes == 1 {
                assert_eq!(
                    result.configuration_write,
                    if failure == "configuration_write" {
                        "uncertain"
                    } else {
                        "accepted"
                    }
                );
            }
        }
    }

    #[test]
    fn effect_failures_reject_stale_state_or_stop_after_the_single_transfer() {
        let request = LightingRequest::Effect(Effect::from_id(3).unwrap());
        for (failure, code, sends) in [
            ("stale_config", "review_mismatch", 0),
            ("stale_padding", "review_mismatch", 0),
            ("snapshot", "snapshot_failed", 0),
            ("configuration_write", "device_io", 1),
            (
                "configuration_mismatch",
                "configuration_readback_mismatch",
                1,
            ),
            ("matrix_mismatch", "matrix_readback_mismatch", 1),
            ("signature_readback", "invalid_signature", 1),
        ] {
            let (result, trace) = synthetic_request("effect", failure, request);
            assert_eq!(result.error.unwrap().code, code, "{failure}");
            assert_eq!(
                result.matrix_write,
                if sends == 0 {
                    "not_attempted"
                } else {
                    "skipped_unchanged"
                }
            );
            assert_eq!(
                trace.iter().filter(|s| s.starts_with("write:")).count(),
                sends
            );
            assert!(!trace.iter().any(|s| s == "write:Matrix"));
            assert!(!result.hardware_verified);
        }
    }

    #[test]
    fn competing_activity_stops_before_each_write_with_accurate_partial_results() {
        for (case, stage, sends, snapshot) in [
            ("controller_preflight", "access_validation", 0, false),
            ("controller_matrix", "matrix_access", 0, true),
            ("controller_configuration", "configuration_access", 1, true),
        ] {
            let (result, trace) = synthetic(case);
            assert_eq!(result.error.unwrap().code, "competing_controller");
            assert_eq!(result.failed_step, Some(stage));
            assert_eq!(
                trace.iter().filter(|s| s.starts_with("write:")).count(),
                sends
            );
            assert_eq!(result.snapshot_path.is_some(), snapshot);
            assert_eq!(
                result.matrix_write,
                if sends == 0 {
                    "not_attempted"
                } else {
                    "accepted"
                }
            );
            assert_eq!(result.configuration_write, "not_attempted");
            assert_eq!(result.matrix_readback, "not_attempted");
        }
    }

    #[test]
    fn repeated_colors_refresh_unchanged_configuration_but_identical_requests_skip_all_writes() {
        for color in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 255],
            [137, 180, 250],
        ] {
            let (changed, trace) = synthetic_case("subsequent", "", Color(color));
            assert!(changed.error.is_none());
            assert!(!changed.no_op);
            assert!(changed.configuration_refresh);
            assert_eq!(changed.requested_rgb, Some(color));
            assert_eq!(
                trace
                    .iter()
                    .filter(|s| s.starts_with("write:"))
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["write:Matrix", "write:Configuration"]
            );
            assert_eq!(changed.configuration_readback, "verified");
            assert!(changed.snapshot_path.is_some());
            let (unchanged, trace) = synthetic_case("noop", "", Color(color));
            assert!(unchanged.error.is_none());
            assert!(unchanged.no_op && !unchanged.configuration_refresh);
            assert_eq!(unchanged.matrix_write, "skipped_unchanged");
            assert_eq!(unchanged.configuration_write, "skipped_unchanged");
            assert_eq!(unchanged.matrix_readback, "verified");
            assert_eq!(unchanged.configuration_readback, "verified");
            assert!(unchanged.snapshot_path.is_none());
            assert!(
                trace
                    .iter()
                    .all(|s| !s.starts_with("write:") && s != "snapshot")
            );
        }
        let (activation, trace) = synthetic_case("config_only", "", Color([255, 0, 0]));
        assert!(activation.error.is_none());
        assert!(!activation.no_op && !activation.configuration_refresh);
        assert_eq!(activation.matrix_write, "skipped_unchanged");
        assert_eq!(
            trace
                .iter()
                .filter(|s| s.starts_with("write:"))
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["write:Configuration"]
        );
        assert!(activation.snapshot_path.is_some());
    }

    #[test]
    fn review_binding_covers_skipped_regions_and_noop_readback_can_still_fail() {
        for case in ["first", "subsequent", "noop", "config_only"] {
            for failure in ["stale_config", "stale_padding"] {
                let (result, trace) = synthetic_case(case, failure, Color([255, 0, 0]));
                assert_eq!(result.error.unwrap().code, "review_mismatch");
                assert!(result.snapshot_path.is_none());
                assert!(trace.iter().all(|s| !s.starts_with("write:")));
            }
        }
        let (result, trace) = synthetic_case("noop", "matrix_mismatch", Color([255, 0, 0]));
        assert_eq!(result.error.unwrap().code, "matrix_readback_mismatch");
        assert!(result.no_op);
        assert_eq!(result.matrix_write, "skipped_unchanged");
        assert!(
            trace
                .iter()
                .all(|s| !s.starts_with("write:") && s != "snapshot")
        );
    }

    #[test]
    fn fresh_identity_state_and_snapshot_failures_prevent_every_write() {
        for (case, code) in [
            ("model", "unsupported_model"),
            ("signature", "invalid_signature"),
            ("matrix_header", "unexpected_matrix_reply"),
            ("baseline", "review_mismatch"),
            ("already_applied", "review_mismatch"),
            ("wrong_review", "review_mismatch"),
            ("stale_config", "review_mismatch"),
            ("stale_matrix", "review_mismatch"),
            ("stale_padding", "review_mismatch"),
            ("snapshot", "snapshot_failed"),
        ] {
            let (result, trace) = synthetic(case);
            assert_eq!(result.error.unwrap().code, code, "{case}");
            assert!(trace.iter().all(|s| !s.starts_with("write:")), "{case}");
            assert_eq!(result.matrix_write, "not_attempted");
            assert_eq!(result.configuration_write, "not_attempted");
            assert!(!result.hid_transport_success);
            assert!(result.snapshot_path.is_none());
        }
    }

    #[test]
    fn partial_writes_and_readback_failures_stop_without_retries_or_rollback() {
        for case in [
            "matrix_write",
            "configuration_write",
            "matrix_readback",
            "configuration_readback",
            "matrix_mismatch",
            "configuration_mismatch",
        ] {
            let (result, trace) = synthetic(case);
            assert!(result.error.is_some(), "{case}");
            assert!(result.snapshot_path.is_some());
            let sends: Vec<_> = trace.iter().filter(|s| s.starts_with("write:")).collect();
            assert_eq!(sends.len(), if case == "matrix_write" { 1 } else { 2 });
            assert_eq!(
                result.failed_step,
                Some(match case {
                    "matrix_mismatch" => "matrix_readback",
                    "configuration_mismatch" => "configuration_readback",
                    _ => case,
                })
            );
            assert!(!result.hardware_verified && !result.visual_verified);
            match case {
                "matrix_write" => {
                    assert_eq!(result.matrix_write, "uncertain");
                    assert_eq!(result.configuration_write, "not_attempted");
                }
                "configuration_write" => {
                    assert_eq!(result.matrix_write, "accepted");
                    assert_eq!(result.configuration_write, "uncertain");
                }
                "matrix_mismatch" => {
                    assert!(result.hid_transport_success);
                    assert_eq!(result.matrix_readback, "mismatch");
                    assert_eq!(result.configuration_readback, "not_attempted");
                }
                "configuration_mismatch" => {
                    assert!(result.hid_transport_success);
                    assert_eq!(result.matrix_readback, "verified");
                    assert_eq!(result.configuration_readback, "mismatch");
                    assert_eq!(result.hardware_mode, "per_key");
                }
                "matrix_readback" => {
                    assert!(result.hid_transport_success);
                    assert_eq!(result.matrix_readback, "failed");
                    assert_eq!(result.configuration_readback, "not_attempted");
                }
                "configuration_readback" => {
                    assert!(result.hid_transport_success);
                    assert_eq!(result.matrix_readback, "verified");
                    assert_eq!(result.configuration_readback, "failed");
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn snapshots_are_private_complete_non_overwriting_and_review_files_are_bounded() {
        let base = std::env::temp_dir().join(format!(
            "aula-static-snapshot-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (_, mut config, matrix, plan, review) = fixture();
        let path = save_snapshot_at(&base, "recovery.json", 123, &plan).unwrap();
        assert_eq!(path, base.join("aulaf75ctl/recovery.json"));
        let data = fs::read(&path).unwrap();
        let snapshot: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(
            snapshot["raw"]["configuration"],
            json!(plan.original_configuration.as_slice())
        );
        assert_eq!(
            snapshot["raw"]["matrix"],
            json!(plan.original_matrix.as_slice())
        );
        assert_eq!(snapshot["metadata"]["model_identity"], "f75_cd");
        assert_eq!(snapshot["metadata"]["timestamp_unix_ms"], 123);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert!(save_snapshot_at(&base, "recovery.json", 124, &plan).is_err());
        assert_eq!(fs::read(&path).unwrap(), data);
        config[8 + 62] = 7;
        config[8 + 63] = 0x23;
        let effect_plan = protocol::prepare_effect(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            Effect::from_id(3).unwrap(),
        )
        .unwrap();
        let effect_path = save_snapshot_at(&base, "effect.json", 124, &effect_plan).unwrap();
        let effect_snapshot: Value =
            serde_json::from_slice(&fs::read(effect_path).unwrap()).unwrap();
        assert_eq!(
            effect_snapshot["metadata"]["intended_operation"]["command"],
            "lighting_effect"
        );
        assert_eq!(
            effect_snapshot["metadata"]["intended_operation"]["effect"]["id"],
            3
        );
        assert!(
            effect_snapshot["metadata"]["intended_operation"]
                .get("rgb")
                .is_none()
        );
        assert_eq!(
            effect_snapshot["raw"]["configuration"],
            json!(effect_plan.original_configuration.as_slice())
        );
        assert_eq!(
            effect_snapshot["raw"]["matrix"],
            json!(effect_plan.original_matrix.as_slice())
        );
        let review_path = base.join("review.json");
        fs::write(&review_path, serde_json::to_vec(&review).unwrap()).unwrap();
        assert_eq!(load_review(&review_path).unwrap(), review);
        let effect_review = json!({"schema_version":1,"command":"lighting_effect","ok":true,"error":null,"effect_plan":effect_plan});
        fs::write(&review_path, serde_json::to_vec(&effect_review).unwrap()).unwrap();
        assert_eq!(load_review(&review_path).unwrap(), effect_review);
        let palette_plan = protocol::prepare_palette(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Palette::parse("green").unwrap(),
        )
        .unwrap();
        let palette_path = save_snapshot_at(&base, "palette.json", 125, &palette_plan).unwrap();
        let snapshot: Value = serde_json::from_slice(&fs::read(palette_path).unwrap()).unwrap();
        let intended = &snapshot["metadata"]["intended_operation"];
        assert_eq!(intended["command"], "lighting_palette");
        assert_eq!(intended["active_effect"]["id"], 1);
        assert_eq!(
            intended["previous_palette"],
            json!({"index":3,"name":"yellow"})
        );
        assert_eq!(
            intended["requested_palette"],
            json!({"index":1,"name":"green"})
        );
        assert_eq!(intended["parameter_offset"], 59);
        assert_eq!(
            snapshot["raw"]["configuration"],
            json!(palette_plan.original_configuration.as_slice())
        );
        assert_eq!(
            snapshot["raw"]["matrix"],
            json!(palette_plan.original_matrix.as_slice())
        );
        let mut palette_review = json!({"schema_version":1,"command":"lighting_palette","ok":true,"error":null,"palette_plan":palette_plan});
        fs::write(&review_path, serde_json::to_vec(&palette_review).unwrap()).unwrap();
        assert_eq!(load_review(&review_path).unwrap(), palette_review);
        palette_review["palette_plan"]
            .as_object_mut()
            .unwrap()
            .remove("review_state");
        fs::write(&review_path, serde_json::to_vec(&palette_review).unwrap()).unwrap();
        assert_eq!(
            load_review(&review_path).unwrap_err().code,
            "incomplete_review"
        );
        let mut speed_config = config.clone();
        speed_config[8 + 10] = 3;
        let speed_plan = protocol::prepare_speed(
            &protocol::parse_config(&speed_config).unwrap(),
            &matrix,
            protocol::Speed::parse("4").unwrap(),
        )
        .unwrap();
        let speed_path = save_snapshot_at(&base, "speed.json", 125, &speed_plan).unwrap();
        let snapshot: Value = serde_json::from_slice(&fs::read(speed_path).unwrap()).unwrap();
        let intended = &snapshot["metadata"]["intended_operation"];
        assert_eq!(intended["command"], "lighting_speed");
        assert_eq!(intended["active_effect"]["id"], 3);
        assert_eq!(intended["previous_speed"], 2);
        assert_eq!(intended["requested_speed"], 4);
        assert_eq!(intended["parameter_offset"], 63);
        assert_eq!(
            snapshot["raw"]["configuration"],
            json!(speed_plan.original_configuration.as_slice())
        );
        assert_eq!(
            snapshot["raw"]["matrix"],
            json!(speed_plan.original_matrix.as_slice())
        );
        let mut speed_review = json!({"schema_version":1,"command":"lighting_speed","ok":true,"error":null,"speed_plan":speed_plan});
        fs::write(&review_path, serde_json::to_vec(&speed_review).unwrap()).unwrap();
        assert_eq!(load_review(&review_path).unwrap(), speed_review);
        speed_review["speed_plan"]
            .as_object_mut()
            .unwrap()
            .remove("review_state");
        fs::write(&review_path, serde_json::to_vec(&speed_review).unwrap()).unwrap();
        assert_eq!(
            load_review(&review_path).unwrap_err().code,
            "incomplete_review"
        );
        let brightness_plan = protocol::prepare_brightness(
            &protocol::parse_config(&config).unwrap(),
            &matrix,
            protocol::Brightness::parse("5").unwrap(),
        )
        .unwrap();
        let brightness_path =
            save_snapshot_at(&base, "brightness.json", 125, &brightness_plan).unwrap();
        let snapshot: Value = serde_json::from_slice(&fs::read(&brightness_path).unwrap()).unwrap();
        assert_eq!(
            fs::metadata(&brightness_path).unwrap().mode() & 0o777,
            0o600
        );
        let intended = &snapshot["metadata"]["intended_operation"];
        assert_eq!(intended["command"], "lighting_brightness");
        assert_eq!(intended["active_effect"]["id"], 1);
        assert_eq!(intended["previous_brightness"], 1);
        assert_eq!(intended["requested_brightness"], 5);
        assert_eq!(intended["parameter_offset"], 58);
        assert_eq!(
            snapshot["raw"]["configuration"],
            json!(brightness_plan.original_configuration.as_slice())
        );
        assert_eq!(
            snapshot["raw"]["matrix"],
            json!(brightness_plan.original_matrix.as_slice())
        );
        let mut brightness_review = json!({"schema_version":1,"command":"lighting_brightness","ok":true,"error":null,"brightness_plan":brightness_plan});
        fs::write(
            &review_path,
            serde_json::to_vec(&brightness_review).unwrap(),
        )
        .unwrap();
        assert_eq!(load_review(&review_path).unwrap(), brightness_review);
        brightness_review["brightness_plan"]
            .as_object_mut()
            .unwrap()
            .remove("review_state");
        fs::write(
            &review_path,
            serde_json::to_vec(&brightness_review).unwrap(),
        )
        .unwrap();
        assert_eq!(
            load_review(&review_path).unwrap_err().code,
            "incomplete_review"
        );
        let mut summary = review.clone();
        summary["static_plan"]
            .as_object_mut()
            .unwrap()
            .remove("review_state");
        fs::write(&review_path, serde_json::to_vec(&summary).unwrap()).unwrap();
        assert_eq!(
            load_review(&review_path).unwrap_err().code,
            "incomplete_review"
        );
        fs::write(&review_path, b"not JSON").unwrap();
        assert_eq!(
            load_review(&review_path).unwrap_err().code,
            "invalid_review"
        );
        fs::write(&review_path, vec![b' '; 262145]).unwrap();
        assert!(load_review(&review_path).is_err());
        fs::remove_file(&review_path).unwrap();
        std::os::unix::fs::symlink(&path, &review_path).unwrap();
        assert!(load_review(&review_path).is_err());
        fs::set_permissions(base.join("aulaf75ctl"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(save_snapshot_at(&base, "another.json", 125, &plan).is_err());
        fs::remove_dir_all(base).unwrap();
    }
}
