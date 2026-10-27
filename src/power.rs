//! Effect-based Off/On for one kernel USB instance. No automatic recovery.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{
    Error,
    protocol::{self, Configuration, Effect, LightingPlan},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Off,
    On,
}

impl Action {
    pub fn command(self) -> &'static str {
        match self {
            Self::Off => "lighting_off",
            Self::On => "lighting_on",
        }
    }
    pub fn snapshot_prefix(self) -> &'static str {
        match self {
            Self::Off => "power-off",
            Self::On => "power-on",
        }
    }
}

// No USB serial exists on the tested F75. These are kernel-instance metadata,
// not a physical serial or a stable port path. An unavailable field fails closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub boot_id: String,
    pub hid_id: u32,
    pub usb_bus: u16,
    pub usb_address: u8,
    pub usb_inode: u64,
    pub usb_ctime: i64,
    pub usb_ctime_ns: i64,
}

impl Instance {
    pub fn read(hid: &Path, boot_path: &Path) -> Result<Self, Error> {
        let path = fs::canonicalize(hid).map_err(|_| identity_error())?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(identity_error)?;
        let suffix = name
            .strip_prefix("0003:258A:010C.")
            .ok_or_else(identity_error)?;
        let hid_id = u32::from_str_radix(suffix, 16)
            .ok()
            .filter(|n| (1..=0x7fff_ffff).contains(n))
            .ok_or_else(identity_error)?;
        let usb = path
            .ancestors()
            .find(|p| p.join("idVendor").is_file() && p.join("idProduct").is_file())
            .ok_or_else(identity_error)?;
        let read = |file| fs::read_to_string(usb.join(file)).map_err(|_| identity_error());
        if read("idVendor")?.trim() != "258a" || read("idProduct")?.trim() != "010c" {
            return Err(identity_error());
        }
        let metadata = fs::metadata(usb).map_err(|_| identity_error())?;
        let instance = Self {
            boot_id: fs::read_to_string(boot_path)
                .map_err(|_| identity_error())?
                .trim()
                .into(),
            hid_id,
            usb_bus: read("busnum")?
                .trim()
                .parse()
                .map_err(|_| identity_error())?,
            usb_address: read("devnum")?
                .trim()
                .parse()
                .map_err(|_| identity_error())?,
            usb_inode: metadata.ino(),
            usb_ctime: metadata.ctime(),
            usb_ctime_ns: metadata.ctime_nsec(),
        };
        if !instance.valid() {
            return Err(identity_error());
        }
        Ok(instance)
    }

    fn valid(&self) -> bool {
        self.boot_id.len() == 36
            && self.boot_id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
            && (1..=0x7fff_ffff).contains(&self.hid_id)
            && self.usb_bus > 0
            && (1..=127).contains(&self.usb_address)
            && self.usb_inode != 0
            && self.usb_ctime > 0
            && (0..1_000_000_000).contains(&self.usb_ctime_ns)
    }
}

fn identity_error() -> Error {
    Error::new(
        "power_instance_unavailable",
        "Cannot verify this kernel USB/HID instance; session Off/On is unavailable. Use an explicitly reviewed lighting effect command instead",
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    OffPending,
    OffVerified,
    OnPending,
    OnVerified,
}

impl Phase {
    fn status(self) -> &'static str {
        match self {
            Self::OffPending => "off_pending",
            Self::OffVerified => "off_verified",
            Self::OnPending => "on_pending",
            Self::OnVerified => "on_verified",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    schema_version: u8,
    instance: Instance,
    usb_id: String,
    interface: u8,
    model: [u8; 6],
    previous_effect: u8,
    phase: Phase,
    off_snapshot: PathBuf,
    on_snapshot: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub action: Action,
    pub restoration_target: Option<Effect>,
    pub restoration_status: &'static str,
    pub instance: Instance,
    // Bound by the ordinary complete-plan comparison; no separate fingerprint.
    pub review_record: Option<Record>,
}

#[derive(Serialize)]
pub struct Outcome {
    pub action: Action,
    pub restoration_target: Option<Effect>,
    pub restoration_status: &'static str,
}

pub struct Session {
    runtime: PathBuf,
    instance: Instance,
    record: Option<Record>,
    pub status: &'static str,
}

fn state_error(code: &'static str, message: &str) -> Error {
    Error::new(code, message)
}

fn check_runtime(path: &Path) -> Result<(), Error> {
    let m = fs::symlink_metadata(path).map_err(|_| {
        state_error(
            "restoration_storage",
            "Cannot inspect the private runtime directory",
        )
    })?;
    if !path.is_absolute()
        || !m.is_dir()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o777 != 0o700
    {
        return Err(state_error(
            "restoration_storage",
            "Restoration requires an absolute user-owned non-symlink mode-0700 runtime directory",
        ));
    }
    Ok(())
}

fn read_private(path: &Path) -> Result<Vec<u8>, Error> {
    let invalid = || {
        state_error(
            "restoration_corrupt",
            "Cannot read a bounded private, user-owned mode-0600 restoration record/snapshot; inspect recovery evidence, do not retry hardware writes",
        )
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid())?;
    let m = file.metadata().map_err(|_| invalid())?;
    if !m.is_file()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o777 != 0o600
        || m.nlink() != 1
        || m.len() > 32768
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(32769)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 32768 {
        return Err(invalid());
    }
    Ok(bytes)
}

impl Session {
    pub fn load(instance: Instance) -> Result<Self, Error> {
        Self::load_at(instance, crate::device::runtime_directory())
    }

    pub(crate) fn load_at(instance: Instance, runtime: PathBuf) -> Result<Self, Error> {
        check_runtime(&runtime)?;
        if !instance.valid() {
            return Err(identity_error());
        }
        let mut session = Self {
            runtime,
            instance,
            record: None,
            status: "missing",
        };
        session.record = session.read_record()?;
        if let Some(record) = &session.record {
            session.status = record.phase.status();
        }
        Ok(session)
    }

    fn path(&self) -> PathBuf {
        self.runtime.join("aulaf75ctl-power.json")
    }

    fn read_record(&self) -> Result<Option<Record>, Error> {
        check_runtime(&self.runtime)?;
        match fs::symlink_metadata(self.path()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(state_error(
                    "restoration_storage",
                    "Cannot inspect restoration metadata",
                ));
            }
            Ok(_) => {}
        }
        let r: Record = serde_json::from_slice(&read_private(&self.path())?).map_err(|_| {
            state_error(
                "restoration_corrupt",
                "Invalid restoration JSON; manual recovery review is required",
            )
        })?;
        if r.schema_version != 1
            || !r.instance.valid()
            || r.usb_id != "258a:010c"
            || r.interface != 1
            || r.model != protocol::F75_MODEL
            || r.previous_effect == 0
            || Effect::from_id(r.previous_effect).is_err()
            || !r.off_snapshot.is_absolute()
            || r.on_snapshot.as_ref().is_some_and(|p| !p.is_absolute())
            || (matches!(r.phase, Phase::OnPending | Phase::OnVerified) != r.on_snapshot.is_some())
        {
            return Err(state_error(
                "restoration_corrupt",
                "Unsupported or inconsistent restoration metadata",
            ));
        }
        Ok(Some(r))
    }

    pub fn prepare(
        &mut self,
        action: Action,
        config: &Configuration,
        matrix: &[u8],
    ) -> Result<LightingPlan, Error> {
        let result = self.prepare_checked(action, config, matrix);
        if let Err(e) = &result {
            self.status = e.code;
        }
        result
    }

    fn prepare_checked(
        &mut self,
        action: Action,
        config: &Configuration,
        matrix: &[u8],
    ) -> Result<LightingPlan, Error> {
        if self.read_record()? != self.record {
            return Err(state_error(
                "restoration_changed",
                "Restoration metadata changed since it was loaded; prepare a fresh review",
            ));
        }
        let current = Effect::from_id(config.effect_id)?;
        if let Some(record) = &self.record {
            if record.instance != self.instance {
                return Err(state_error(
                    "restoration_instance_changed",
                    "USB re-enumeration, reboot or another device invalidated session restoration; use an explicitly reviewed lighting effect command",
                ));
            }
            if matches!(record.phase, Phase::OffPending | Phase::OnPending) {
                return Err(state_error(
                    "restoration_interrupted",
                    "A previous Off/On transaction was not committed after readback. Preserve its snapshots and arrange explicit recovery; no automatic retry",
                ));
            }
            if record.phase == Phase::OffVerified {
                if current.id != 0 {
                    return Err(state_error(
                        "restoration_conflict",
                        "Another operation selected an effect after Off; refusing to override it or replace the saved restoration target",
                    ));
                }
                self.check_off_state(record, config, matrix)?;
            }
        }
        let target = self
            .record
            .as_ref()
            .filter(|r| r.phase == Phase::OffVerified)
            .map(|r| Effect::from_id(r.previous_effect))
            .transpose()?;
        let requested = match action {
            Action::Off => {
                if current.id != 0 { protocol::prepare_effect(config, matrix, current)?; }
                Effect::from_id(0)?
            }
            Action::On if current.id != 0 => current,
            Action::On => target.ok_or_else(|| state_error("restoration_missing", "No verified Off record for this session/instance; select an explicitly reviewed lighting effect instead"))?,
        };
        let restoration_target = if action == Action::Off && current.id != 0 {
            Some(current)
        } else {
            target
        };
        let mut plan = protocol::prepare_effect(config, matrix, requested)?;
        self.status = if action == Action::On && current.id != 0 {
            "already_on"
        } else if action == Action::Off && current.id != 0 {
            "will_save"
        } else if self
            .record
            .as_ref()
            .is_some_and(|r| r.phase == Phase::OnVerified)
        {
            "consumed"
        } else {
            self.record.as_ref().map_or("missing", |r| r.phase.status())
        };
        plan.power = Some(Plan {
            action,
            restoration_target,
            restoration_status: self.status,
            instance: self.instance.clone(),
            review_record: self.record.clone(),
        });
        Ok(plan)
    }

    fn check_off_state(
        &self,
        record: &Record,
        config: &Configuration,
        matrix: &[u8],
    ) -> Result<(), Error> {
        // Compare against the existing snapshot, never store a second full image.
        let parent = record
            .off_snapshot
            .parent()
            .ok_or_else(|| state_error("restoration_corrupt", "Snapshot path is invalid"))?;
        let m = fs::symlink_metadata(parent).map_err(|_| {
            state_error(
                "restoration_corrupt",
                "Recovery snapshot directory is missing",
            )
        })?;
        if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o777 != 0o700 {
            return Err(state_error(
                "restoration_corrupt",
                "Recovery snapshot directory is unsafe",
            ));
        }
        let snapshot: serde_json::Value =
            serde_json::from_slice(&read_private(&record.off_snapshot)?).map_err(|_| {
                state_error("restoration_corrupt", "Recovery snapshot is malformed")
            })?;
        if snapshot["schema_version"] != 1
            || snapshot["metadata"]["usb_id"] != "258a:010c"
            || snapshot["metadata"]["interface_number"] != 1
            || snapshot["metadata"]["model_reply"] != serde_json::json!(protocol::F75_MODEL)
            || snapshot["metadata"]["intended_operation"]["command"] != "lighting_off"
            || snapshot["metadata"]["intended_operation"]["power"]["instance"]
                != serde_json::to_value(&record.instance).map_err(|_| identity_error())?
        {
            return Err(state_error(
                "restoration_corrupt",
                "Snapshot identity/operation does not match the session record",
            ));
        }
        let cfg: Vec<u8> = serde_json::from_value(snapshot["raw"]["configuration"].clone())
            .map_err(|_| state_error("restoration_corrupt", "Invalid saved configuration"))?;
        let rgb: Vec<u8> = serde_json::from_value(snapshot["raw"]["matrix"].clone())
            .map_err(|_| state_error("restoration_corrupt", "Invalid saved matrix"))?;
        if cfg.len() != protocol::CONFIG_LEN
            || cfg[126..] != [0x5a, 0xa5]
            || cfg[10] != record.previous_effect
            || rgb.len() != protocol::STATIC_RGB_LEN
        {
            return Err(state_error(
                "restoration_corrupt",
                "Saved state does not match the restoration target",
            ));
        }
        let mut expected = cfg;
        expected[9] = 0;
        expected[10] = 0;
        if config.bytes.as_slice() != expected || protocol::parse_matrix(matrix)?.as_slice() != rgb
        {
            return Err(state_error(
                "restoration_stale",
                "Configuration or stored RGB changed since verified Off; refusing historical restoration",
            ));
        }
        Ok(())
    }

    // Called only after the ordinary fresh-plan review check and snapshot sync.
    pub fn begin(&mut self, plan: &LightingPlan, snapshot: &Path) -> Result<(), Error> {
        let power = plan.power.as_ref().expect("power transaction");
        let record = match power.action {
            Action::Off => Record {
                schema_version: 1,
                instance: self.instance.clone(),
                usb_id: "258a:010c".into(),
                interface: 1,
                model: protocol::F75_MODEL,
                previous_effect: power
                    .restoration_target
                    .expect("changed Off has a target")
                    .id,
                phase: Phase::OffPending,
                off_snapshot: snapshot.into(),
                on_snapshot: None,
            },
            Action::On => {
                let mut r = self.record.clone().expect("On has verified record");
                r.phase = Phase::OnPending;
                r.on_snapshot = Some(snapshot.into());
                r
            }
        };
        self.commit(record)
    }

    pub fn finish(&mut self, action: Action) -> Result<(), Error> {
        let mut r = self.record.clone().expect("pending transaction");
        let expected = match action {
            Action::Off => Phase::OffPending,
            Action::On => Phase::OnPending,
        };
        if r.phase != expected {
            return Err(state_error(
                "restoration_conflict",
                "Cannot verify a restoration transaction that was not pending",
            ));
        }
        r.phase = match action {
            Action::Off => Phase::OffVerified,
            Action::On => Phase::OnVerified,
        };
        self.commit(r)
    }

    fn commit(&mut self, record: Record) -> Result<(), Error> {
        if self.read_record()? != self.record {
            return Err(state_error(
                "restoration_changed",
                "Restoration metadata changed during the operation; stop and inspect snapshots",
            ));
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| identity_error())?
            .as_nanos();
        let temp = self
            .runtime
            .join(format!("aula-power-{}-{time}.tmp", std::process::id()));
        let save = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(&temp)?;
            let metadata = file.metadata()?;
            if metadata.uid() != nix::unistd::geteuid().as_raw()
                || metadata.mode() & 0o777 != 0o600
                || metadata.nlink() != 1
            {
                return Err(std::io::Error::other("Unsafe restoration temporary file"));
            }
            serde_json::to_writer(&mut file, &record)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp, self.path())?;
            OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
                .open(&self.runtime)?
                .sync_all()
        })();
        if save.is_err() {
            self.status = "commit_uncertain";
            return Err(state_error(
                "restoration_commit_failed",
                "Cannot durably commit restoration metadata. Preserve snapshots; no automatic retry or recovery",
            ));
        }
        self.status = record.phase.status();
        self.record = Some(record);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn instance_binding_uses_kernel_generation_not_the_usb_port_alone() {
        let root = std::env::temp_dir().join(format!(
            "aula-instance-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let usb = root.join("1-2");
        let hid = usb.join("1-2:1.1/0003:258A:010C.0007");
        fs::create_dir_all(&hid).unwrap();
        for (file, value) in [
            ("idVendor", "258a"),
            ("idProduct", "010c"),
            ("busnum", "1"),
            ("devnum", "5"),
        ] {
            fs::write(usb.join(file), value).unwrap();
        }
        let boot = root.join("boot_id");
        fs::write(&boot, "01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let original = Instance::read(&hid, &boot).unwrap();
        assert_eq!(Instance::read(&hid, &boot).unwrap(), original);
        let new_hid = hid.with_file_name("0003:258A:010C.0008");
        fs::rename(&hid, &new_hid).unwrap();
        assert_ne!(Instance::read(&new_hid, &boot).unwrap(), original);
        fs::write(usb.join("devnum"), "6").unwrap();
        assert_ne!(Instance::read(&new_hid, &boot).unwrap(), original);
        fs::write(&boot, "11234567-89ab-cdef-0123-456789abcdef").unwrap();
        assert_ne!(Instance::read(&new_hid, &boot).unwrap(), original);
        fs::write(usb.join("idProduct"), "4001").unwrap();
        assert!(Instance::read(&new_hid, &boot).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
