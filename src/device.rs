use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread::sleep;

use nix::{ioctl_read, ioctl_readwrite};
use serde::Serialize;

use crate::Error;
use crate::protocol::{self, Configuration, Interface, REPORT_LEN};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Wired,
    Wireless,
}

pub fn identify(bus: u32, vendor: u16, product: u16) -> Option<Transport> {
    match (bus, vendor, product) {
        (3, 0x258a, 0x010c) => Some(Transport::Wired),
        (3, 0x3554, 0xfa09) => Some(Transport::Wireless),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct Capabilities {
    pub read_configuration: bool,
    pub static_color: bool,
    pub brightness: bool,
    // Configuration-byte support does not establish physical LED dimming.
    pub brightness_visual_verified: bool,
    pub lighting_power: bool,
}

#[derive(Serialize)]
pub struct Device {
    pub usb_id: String,
    pub transport: Transport,
    pub interface_number: Option<u8>,
    pub interface_verified: bool,
    pub protocol_verified: bool,
    pub keyboard_connected: Option<bool>,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_state: Option<&'static str>,
    pub capabilities: Capabilities,
    pub configuration: Option<Configuration>,
    pub model: Option<protocol::Model>,
    pub model_error: Option<Error>,
    pub error: Option<Error>,
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    descriptor: Vec<u8>,
    #[serde(skip)]
    instance: Option<crate::power::Instance>,
}

pub fn discover(filter: Option<Transport>) -> Result<Vec<Device>, Error> {
    discover_at(Path::new("/sys/class/hidraw"), Path::new("/dev"), filter)
}

fn discover_at(root: &Path, nodes: &Path, filter: Option<Transport>) -> Result<Vec<Device>, Error> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_error(e)),
    };
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let hid = entry.path().join("device");
        let uevent = match fs::read_to_string(hid.join("uevent")) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue, // unplug during enumeration
            Err(e) => return Err(io_error(e)),
        };
        let Some((vendor, product, transport)) = identity(&uevent) else {
            continue;
        };
        if filter.is_some_and(|wanted| wanted != transport) {
            continue;
        }
        let descriptor = match fs::read(hid.join("report_descriptor")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_error(e)),
        };
        let shape = protocol::inspect_descriptor(&descriptor);
        // Ordinary typing/mouse collections are never opened. Malformed vendor
        // interfaces remain visible, but cannot reach an ioctl exchange.
        if shape.is_some_and(|s| !s.vendor_collection) {
            continue;
        }
        let verified = shape.is_some_and(|s| matches_transport(s, transport));
        let interface_number = fs::canonicalize(&hid).ok().and_then(|p| {
            p.ancestors().find_map(|parent| {
                fs::read_to_string(parent.join("bInterfaceNumber"))
                    .ok()
                    .and_then(|s| u8::from_str_radix(s.trim(), 16).ok())
            })
        });
        devices.push(Device {
            instance: if transport == Transport::Wired {
                crate::power::Instance::read(&hid, Path::new("/proc/sys/kernel/random/boot_id"))
                    .ok()
            } else {
                None
            },
            usb_id: format!("{vendor:04x}:{product:04x}"),
            transport,
            interface_number,
            interface_verified: verified,
            protocol_verified: false,
            keyboard_connected: None,
            state: if verified {
                "not_probed"
            } else {
                "unverified_interface"
            },
            operation_state: None,
            capabilities: Capabilities {
                read_configuration: false,
                static_color: false,
                brightness: false,
                brightness_visual_verified: false,
                lighting_power: false,
            },
            configuration: None,
            model: None,
            model_error: None,
            error: (!verified).then(|| {
                Error::new(
                    "unverified_interface",
                    "HID descriptor does not match the expected vendor control interface",
                )
            }),
            path: nodes.join(entry.file_name()),
            descriptor,
        });
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(devices)
}

fn identity(uevent: &str) -> Option<(u16, u16, Transport)> {
    let mut id = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?
        .split(':');
    let bus = u32::from_str_radix(id.next()?, 16).ok()?;
    let vendor = u16::from_str_radix(id.next()?, 16).ok()?;
    let product = u16::from_str_radix(id.next()?, 16).ok()?;
    if id.next().is_some() {
        return None;
    }
    Some((vendor, product, identify(bus, vendor, product)?))
}

fn matches_transport(shape: Interface, transport: Transport) -> bool {
    match transport {
        Transport::Wired => shape.wired,
        Transport::Wireless => shape.wireless,
    }
}

pub fn inspect(devices: &mut [Device]) {
    // One lock across this user's CLI invocations, held across complete exchanges
    // and pacing. Other vendor programs must be closed; they do not share it.
    let lock = if devices
        .iter()
        .any(|d| d.transport == Transport::Wired && d.interface_verified)
    {
        Some(command_lock())
    } else {
        None
    };
    for device in devices {
        if !device.interface_verified {
            continue;
        }
        if device.transport == Transport::Wireless {
            device.state = "receiver_present";
            device.error = Some(Error::new(
                "wireless_status_unavailable",
                "Receiver detected; keyboard connection, pairing and configuration are unknown (radio probing is disabled)",
            ));
            continue;
        }
        let result = match &lock {
            Some(Ok(_guard)) => read_wired(device),
            Some(Err(error)) => Err(error.clone()),
            None => unreachable!(),
        };
        match result {
            Ok((configuration, model)) => {
                device.state = "verified_read_only";
                device.protocol_verified = true;
                device.keyboard_connected = Some(true);
                device.capabilities.read_configuration = true;
                device.configuration = Some(configuration);
                match model {
                    Ok(model) => device.model = Some(model),
                    Err(error) => device.model_error = Some(error),
                }
                update_brightness_capability(device);
            }
            Err(error) => {
                device.state = "unavailable";
                device.error = Some(error);
            }
        }
    }
}

pub fn io_error(error: io::Error) -> Error {
    match (error.kind(), error.raw_os_error()) {
        (io::ErrorKind::PermissionDenied, _) => Error::new(
            "permission_denied",
            "Access denied; check the documented interface-scoped udev rule and replug the device; do not run as root",
        ),
        (io::ErrorKind::NotFound, _) | (_, Some(19 | 6)) => Error::new(
            "disconnected",
            "Device disappeared; reconnect it and run discovery again",
        ),
        _ => Error::new(
            "device_io",
            "Device I/O failed; it may be disconnected, busy, or using an unsupported protocol",
        ),
    }
}

fn command_lock() -> Result<File, Error> {
    let uid = nix::unistd::geteuid().as_raw();
    command_lock_at(&runtime_directory(), uid)
}

pub(crate) fn runtime_directory() -> PathBuf {
    let uid = nix::unistd::geteuid().as_raw();
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")))
}

fn command_lock_at(runtime: &Path, uid: u32) -> Result<File, Error> {
    let metadata = fs::symlink_metadata(runtime).map_err(|_| {
        Error::new(
            "unsafe_runtime_directory",
            "Cannot inspect the private runtime directory; check XDG_RUNTIME_DIR",
        )
    })?;
    if !runtime.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(Error::new(
            "unsafe_runtime_directory",
            "XDG_RUNTIME_DIR must be an absolute, private, user-owned mode-0700 directory",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(runtime.join("aulaf75ctl.lock"))
        .map_err(|_| {
            Error::new(
                "unsafe_lock_file",
                "Cannot safely open the private command lock file",
            )
        })?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o600 {
        return Err(Error::new(
            "unsafe_lock_file",
            "Refusing an unsafe command lock file",
        ));
    }
    file.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => Error::new(
            "device_busy",
            "Another aulaf75ctl command is active; wait for it to finish",
        ),
        TryLockError::Error(e) => io_error(e),
    })?;
    Ok(file)
}

#[repr(C)]
#[derive(Default)]
struct RawInfo {
    bus: u32,
    vendor: i16,
    product: i16,
}
#[repr(C)]
struct RawDescriptor {
    size: u32,
    value: [u8; 4096],
}
ioctl_read!(raw_info, b'H', 0x03, RawInfo);
ioctl_read!(descriptor_size, b'H', 0x01, i32);
ioctl_read!(report_descriptor, b'H', 0x02, RawDescriptor);
ioctl_readwrite!(send_feature_report, b'H', 0x06, [u8; REPORT_LEN]);
ioctl_readwrite!(receive_feature_report, b'H', 0x07, [u8; REPORT_LEN]);

type WiredStatus = (Configuration, Result<protocol::Model, Error>);

fn read_wired(device: &Device) -> Result<WiredStatus, Error> {
    let file = open_wired(device)?;
    read_status(&file)
}

fn read_status(file: &File) -> Result<WiredStatus, Error> {
    let config = protocol::parse_config(&read_report(file, ReadReport::Configuration)?)?;
    let model =
        read_report(file, ReadReport::Model).and_then(|reply| protocol::parse_model(&reply));
    Ok((config, model))
}

fn open_wired(device: &Device) -> Result<File, Error> {
    if device.transport != Transport::Wired || !device.interface_verified {
        return Err(Error::new(
            "unverified_interface",
            "Wired reads require a verified wired control interface",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&device.path)
        .map_err(io_error)?;
    if !file
        .metadata()
        .map_err(io_error)?
        .file_type()
        .is_char_device()
    {
        return Err(Error::new(
            "unverified_interface",
            "Expected a Linux hidraw character device",
        ));
    }
    let mut info = RawInfo::default();
    let mut size = 0i32;
    let mut descriptor = RawDescriptor {
        size: 0,
        value: [0; 4096],
    };
    // SAFETY: Each fd is live and each pointer has exactly the kernel UAPI
    // layout/size encoded by its ioctl macro; the kernel bounds descriptor data.
    unsafe {
        raw_info(file.as_raw_fd(), &mut info).map_err(|e| io_error(e.into()))?;
        descriptor_size(file.as_raw_fd(), &mut size).map_err(|e| io_error(e.into()))?;
    }
    if identify(info.bus, info.vendor as u16, info.product as u16) != Some(Transport::Wired)
        || !(1..=4096).contains(&size)
    {
        return Err(Error::new(
            "unverified_interface",
            "Opened device no longer matches the wired candidate",
        ));
    }
    descriptor.size = size as u32;
    // SAFETY: The descriptor is initialized, sized as above, and the fd is live.
    unsafe {
        report_descriptor(file.as_raw_fd(), &mut descriptor).map_err(|e| io_error(e.into()))?;
    }
    if descriptor.size != size as u32 || descriptor.value[..size as usize] != device.descriptor {
        return Err(Error::new(
            "unverified_interface",
            "Opened HID descriptor differs from discovery; run discovery again",
        ));
    }
    check_wired_access(&file)?;
    Ok(file)
}

// Best-effort detection, not mandatory exclusivity: hidraw has no exclusive
// open. Protected processes and a controller opening after this scan can evade
// it. Other controllers must still be fully closed before using the backend.
fn check_wired_access(file: &File) -> Result<(), Error> {
    check_hid_holders(
        Path::new("/proc"),
        file.metadata().map_err(io_error)?.rdev(),
        std::process::id(),
    )
}

fn check_hid_holders(proc_root: &Path, target: u64, own_pid: u32) -> Result<(), Error> {
    let unavailable = || {
        Error::new(
            "access_check_unavailable",
            "Cannot inspect process HID access; refusing the operation. Check /proc visibility and fully close other keyboard controllers",
        )
    };
    for entry in fs::read_dir(proc_root).map_err(|_| unavailable())? {
        let entry = entry.map_err(|_| unavailable())?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let fds = match fs::read_dir(entry.path().join("fd")) {
            Ok(fds) => fds,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(_) => return Err(unavailable()),
        };
        for fd in fds {
            let fd = match fd {
                Ok(fd) => fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(unavailable()),
            };
            let metadata = match fs::metadata(fd.path()) {
                Ok(metadata) => metadata,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue;
                }
                Err(_) => return Err(unavailable()),
            };
            if metadata.file_type().is_char_device() && metadata.rdev() == target {
                let name: String = fs::read_to_string(entry.path().join("comm"))
                    .unwrap_or_default()
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    .take(32)
                    .collect();
                return Err(Error::new(
                    "competing_controller",
                    &format!(
                        "Another process holds the wired HID device: PID {pid} ({}). Fully exit it, including its tray/background process, before another read/review/apply; no process was stopped",
                        if name.is_empty() { "unknown" } else { &name }
                    ),
                ));
            }
        }
    }
    Ok(())
}

// A read path cannot request a color frame or a configuration write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadReport {
    Configuration,
    Model,
    StaticMatrix,
}

fn send_report(file: &File, report: &[u8; REPORT_LEN]) -> Result<(), Error> {
    protocol::validate_outgoing(report)?;
    transfer_report(file, report)
}

// Kept private: the only callers are the ordinary fixed allowlist above and
// the explicit static executor below, which reconstructs two bounded packets.
fn transfer_report(file: &File, report: &[u8; REPORT_LEN]) -> Result<(), Error> {
    let mut report = *report;
    sleep(protocol::COMMAND_GAP);
    // SAFETY: Live fd and exact kernel UAPI buffer size. The callers constrain
    // the packet and hold the device lock; this function never retries.
    let result = unsafe { send_feature_report(file.as_raw_fd(), &mut report) }
        .map_err(|e| io_error(e.into()));
    sleep(protocol::COMMAND_GAP);
    if result? != REPORT_LEN as i32 {
        return Err(Error::new(
            "device_io",
            "HID report was not transferred completely",
        ));
    }
    Ok(())
}

fn read_report(file: &File, kind: ReadReport) -> Result<Vec<u8>, Error> {
    let request = match kind {
        ReadReport::Configuration => protocol::config_request(),
        ReadReport::Model => protocol::model_request(),
        ReadReport::StaticMatrix => protocol::matrix_request(),
    };
    send_report(file, &request)?;
    let mut reply = [0; REPORT_LEN];
    reply[0] = 6;
    // SAFETY: Live fd, initialized report ID, and a correctly sized writable buffer.
    let length = unsafe { receive_feature_report(file.as_raw_fd(), &mut reply) }
        .map_err(|e| io_error(e.into()));
    sleep(protocol::COMMAND_GAP); // hold the process lock through the final gap
    let length = usize::try_from(length?)
        .map_err(|_| Error::new("malformed_reply", "Invalid HID reply length"))?;
    let bytes = reply
        .get(..length)
        .ok_or_else(|| Error::new("malformed_reply", "Oversized HID reply"))?;
    Ok(bytes.to_vec())
}

fn preview_target(devices: &[Device]) -> Result<usize, Error> {
    let wired: Vec<_> = devices
        .iter()
        .enumerate()
        .filter(|(_, d)| d.transport == Transport::Wired)
        .collect();
    if wired.is_empty() {
        return Err(Error::new(
            "device_not_found",
            "No wired F75 candidate; the receiver cannot accept wired previews",
        ));
    }
    if wired.len() != 1 {
        return Err(Error::new(
            "ambiguous_device",
            "Color preview requires exactly one wired control candidate; disconnect extra candidates",
        ));
    }
    let (index, device) = wired[0];
    if device.usb_id != "258a:010c"
        || !device.interface_verified
        || device.interface_number != Some(1)
    {
        return Err(Error::new(
            "unverified_interface",
            "Wired lighting requires the verified 258a:010c vendor interface 1",
        ));
    }
    Ok(index)
}

pub fn preview_color(devices: &mut [Device], color: protocol::Color) -> Result<(), Error> {
    let index = preview_target(devices)?;
    let _guard = command_lock()?;
    let device = &mut devices[index];
    let file = open_wired(device)?;
    let result = preview_exchange(
        color,
        |kind| read_report(&file, kind),
        |packet| {
            send_report(&file, packet).map_err(|_| Error::new("lighting_transfer_uncertain",
            "Color transfer failed or was incomplete; its visual effect is unknown. No retry was attempted; inspect the keyboard before retrying"))
        },
    );
    let config = match result {
        Ok(config) => config,
        Err(error) => {
            device.state = "unavailable";
            return Err(error);
        }
    };
    device.configuration = Some(config);
    device.protocol_verified = true;
    device.keyboard_connected = Some(true);
    device.capabilities.read_configuration = true;
    device.state = "color_frame_sent_unverified";
    Ok(())
}

// Only read requests reach hidraw here. Prepared 0x06/0x04 reports are data for
// review, not input to the send_report allowlist.
pub fn prepare_lighting(
    devices: &mut [Device],
    request: protocol::LightingRequest,
) -> Result<protocol::LightingPlan, Error> {
    let index = preview_target(devices)?;
    let _guard = command_lock()?;
    let device = &mut devices[index];
    let file = open_wired(device)?;
    let (config, model) = read_status(&file)?;
    device.configuration = Some(config);
    device.protocol_verified = true;
    device.keyboard_connected = Some(true);
    device.capabilities.read_configuration = true;
    device.state = "verified_read_only";
    match model {
        Ok(model) => {
            let matches_expected = model.matches_expected;
            device.model = Some(model);
            if !matches_expected {
                return Err(Error::new(
                    "unsupported_model",
                    "Persistent lighting preparation requires the expected F75 model identity; only read requests were sent",
                ));
            }
        }
        Err(error) => {
            device.model_error = Some(error.clone());
            return Err(error);
        }
    }
    update_brightness_capability(device);
    let matrix = read_report(&file, ReadReport::StaticMatrix)?;
    if let Some(action) = request.power() {
        let instance = verify_instance(device, &file)?;
        crate::power::Session::load(instance)?.prepare(
            action,
            device.configuration.as_ref().unwrap(),
            &matrix,
        )
    } else {
        request.prepare(device.configuration.as_ref().unwrap(), &matrix)
    }
}

pub fn apply_lighting(
    devices: &mut [Device],
    request: protocol::LightingRequest,
    review: &serde_json::Value,
) -> crate::static_write::ApplyResult {
    let mut rejected = crate::static_write::ApplyResult::new(request);
    rejected.failed_step = Some("device_validation");
    let setup = (|| {
        let index = preview_target(devices)?;
        let guard = command_lock()?;
        let file = open_wired(&devices[index])?;
        let power = if request.power().is_some() {
            Some(crate::power::Session::load(verify_instance(
                &devices[index],
                &file,
            )?)?)
        } else {
            None
        };
        Ok::<_, Error>((index, guard, file, power))
    })();
    let (index, _guard, file, mut power) = match setup {
        Ok(values) => values,
        Err(error) => {
            rejected.error = Some(error);
            return rejected;
        }
    };
    let mut result = crate::static_write::execute(
        request,
        review,
        |kind| read_report(&file, kind),
        |plan, step| transfer_report(&file, &plan.write_packet(step)?),
        crate::static_write::save_snapshot,
        || {
            check_wired_access(&file)?;
            if request.power().is_some() {
                verify_instance(&devices[index], &file)?;
            }
            Ok(())
        },
        power.as_mut(),
    );
    record_static_result(&mut devices[index], &mut result);
    result
}

fn record_static_result(device: &mut Device, result: &mut crate::static_write::ApplyResult) {
    device.model = result.model.take();
    device.configuration = result.configuration.take();
    device.protocol_verified = device.configuration.is_some();
    device.capabilities.read_configuration = device.protocol_verified;
    update_brightness_capability(device);
    device.keyboard_connected = device.protocol_verified.then_some(true);
    device.state = if result.error.is_none() && result.no_op {
        "static_no_op_verified"
    } else if result.error.is_none() {
        "static_readback_verified_unobserved"
    } else {
        "static_apply_failed"
    };
    // Keep schema-1 state values for existing clients. New clients and human
    // output use the additive, operation-specific label for effect operations.
    device.operation_state = result.requested_effect.map(|_| {
        if result.error.is_some() {
            "effect_apply_failed"
        } else if result.no_op {
            "effect_no_op_verified"
        } else {
            "effect_readback_verified_unobserved"
        }
    });
    if result.requested_brightness.is_some() {
        device.operation_state = Some(if result.error.is_some() {
            "brightness_apply_failed"
        } else if result.no_op {
            "brightness_no_op_verified"
        } else {
            "brightness_readback_verified_unobserved"
        });
    }
    if result.requested_speed.is_some() {
        device.operation_state = Some(if result.error.is_some() {
            "speed_apply_failed"
        } else if result.no_op {
            "speed_no_op_verified"
        } else {
            "speed_readback_verified_unobserved"
        });
    }
    if result.requested_palette.is_some() {
        device.operation_state = Some(if result.error.is_some() {
            "palette_apply_failed"
        } else if result.no_op {
            "palette_no_op_verified"
        } else {
            "palette_readback_verified_unobserved"
        });
    }
    if let Some(power) = &result.power {
        device.operation_state = Some(match (power.action, result.error.is_some(), result.no_op) {
            (crate::power::Action::Off, true, _) => "power_off_failed",
            (crate::power::Action::On, true, _) => "power_on_failed",
            (crate::power::Action::Off, false, true) => "power_off_no_op_verified",
            (crate::power::Action::On, false, true) => "power_on_no_op_verified",
            (crate::power::Action::Off, false, false) => "power_off_readback_verified_unobserved",
            (crate::power::Action::On, false, false) => "power_on_readback_verified_unobserved",
        });
    }
}

fn update_brightness_capability(device: &mut Device) {
    device.capabilities.lighting_power = device.transport == Transport::Wired
        && device.interface_verified
        && device.interface_number == Some(1)
        && device.usb_id == "258a:010c"
        && device.protocol_verified
        && device.model.as_ref().is_some_and(|m| m.matches_expected)
        && device.instance.is_some()
        && device
            .configuration
            .as_ref()
            .is_some_and(|c| c.active_effect.is_some());
    device.capabilities.brightness = device.transport == Transport::Wired
        && device.usb_id == "258a:010c"
        && device.interface_verified
        && device.interface_number == Some(1)
        && device.protocol_verified
        && device.model.as_ref().is_some_and(|m| m.matches_expected)
        && device
            .configuration
            .as_ref()
            .is_some_and(|c| protocol::brightness_offset(c).is_ok());
}

fn verify_instance(device: &Device, file: &File) -> Result<crate::power::Instance, Error> {
    let expected = device.instance.as_ref().ok_or_else(|| {
        Error::new(
            "power_instance_unavailable",
            "Kernel USB instance metadata is unavailable; use explicit effect selection instead",
        )
    })?;
    let rdev = file.metadata().map_err(io_error)?.rdev();
    let path = PathBuf::from(format!(
        "/sys/dev/char/{}:{}/device",
        nix::libc::major(rdev),
        nix::libc::minor(rdev)
    ));
    let actual = crate::power::Instance::read(&path, Path::new("/proc/sys/kernel/random/boot_id"))?;
    if &actual != expected {
        return Err(Error::new(
            "restoration_instance_changed",
            "Opened USB/HID instance differs from discovery; no power write is allowed",
        ));
    }
    Ok(actual)
}

// Small I/O seam for synthetic exchanges: the real implementation closes over
// one validated fd. Tests supply replies and count sends, never touching hidraw.
fn preview_exchange(
    color: protocol::Color,
    mut read: impl FnMut(ReadReport) -> Result<Vec<u8>, Error>,
    send: impl FnOnce(&[u8; REPORT_LEN]) -> Result<(), Error>,
) -> Result<Configuration, Error> {
    let config = protocol::parse_config(&read(ReadReport::Configuration)?)?;
    protocol::require_preview_mode(&config)?;
    protocol::require_f75_model(&read(ReadReport::Model)?)?;
    let packet = protocol::color_frame(color);
    protocol::validate_outgoing(&packet)?;
    send(&packet)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn competing_holder_scan_uses_device_identity_and_fails_if_proc_is_unavailable() {
        let root = std::env::temp_dir().join(format!(
            "aula-holders-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let fds = root.join("50010/fd");
        fs::create_dir_all(&fds).unwrap();
        fs::write(root.join("50010/comm"), "f75-studio\n").unwrap();
        // Only character-device metadata and synthetic /proc entries: no HID.
        let target = fs::metadata("/dev/null").unwrap().rdev();
        std::os::unix::fs::symlink("/dev/null", fds.join("7")).unwrap();
        let error = check_hid_holders(&root, target, 50020).unwrap_err();
        assert_eq!(error.code, "competing_controller");
        assert!(error.message.contains("50010 (f75-studio)"));
        check_hid_holders(&root, target, 50010).unwrap(); // own fd ignored
        fs::remove_file(fds.join("7")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", fds.join("7")).unwrap();
        check_hid_holders(&root, target, 50020).unwrap(); // unrelated device
        assert_eq!(
            check_hid_holders(&root.join("missing"), target, 50020)
                .unwrap_err()
                .code,
            "access_check_unavailable"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_requires_config_then_model_and_sends_once_without_retry() {
        let mut config = protocol::config_request().to_vec();
        config[134..136].copy_from_slice(&[0x5a, 0xa5]);
        config[17] = 1;
        config[18] = 0x15;
        config[67] = 0x47;
        let mut model = protocol::model_request().to_vec();
        model[8..14].copy_from_slice(&protocol::F75_MODEL);
        let color = protocol::Color([137, 180, 250]);
        let mut reads = Vec::new();
        let mut writes = 0;
        preview_exchange(
            color,
            |kind| {
                reads.push(kind);
                Ok(match kind {
                    ReadReport::Configuration => config.clone(),
                    ReadReport::Model => model.clone(),
                    ReadReport::StaticMatrix => panic!("preview must never read persistent colors"),
                })
            },
            |packet| {
                writes += 1;
                assert_eq!(*packet, protocol::color_frame(color));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(reads, [ReadReport::Configuration, ReadReport::Model]);
        assert_eq!(writes, 1);

        for (bytes, code) in [
            (vec![0; 9], "malformed_reply"),
            (protocol::config_request().to_vec(), "invalid_signature"),
            (
                {
                    let mut c = config.clone();
                    c[17] = 0;
                    c[18] = 1;
                    c[67] = 51;
                    c
                },
                "unsupported_lighting_mode",
            ),
        ] {
            let error = preview_exchange(
                color,
                |kind| {
                    assert_eq!(kind, ReadReport::Configuration);
                    Ok(bytes.clone())
                },
                |_| panic!("invalid configuration must not reach a lighting send"),
            )
            .unwrap_err();
            assert_eq!(error.code, code);
        }
        let error = preview_exchange(
            color,
            |kind| {
                Ok(match kind {
                    ReadReport::Configuration => config.clone(),
                    ReadReport::Model => protocol::model_request().to_vec(),
                    ReadReport::StaticMatrix => panic!("preview must never read persistent colors"),
                })
            },
            |_| panic!("unverified model must not reach a lighting send"),
        )
        .unwrap_err();
        assert_eq!(error.code, "unsupported_model");
        let error = preview_exchange(
            color,
            |_| Err(io_error(io::Error::from_raw_os_error(19))),
            |_| panic!("disconnected read must not reach a lighting send"),
        )
        .unwrap_err();
        assert_eq!(error.code, "disconnected");
        writes = 0;
        let error = preview_exchange(
            color,
            |kind| {
                Ok(match kind {
                    ReadReport::Configuration => config.clone(),
                    ReadReport::Model => model.clone(),
                    ReadReport::StaticMatrix => panic!("preview must never read persistent colors"),
                })
            },
            |_| {
                writes += 1;
                Err(io_error(io::Error::from_raw_os_error(19)))
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "disconnected");
        assert_eq!(writes, 1, "a failed frame is never retried");
    }

    #[test]
    fn synthetic_discovery_and_receiver_status_never_open_hardware() {
        let root = std::env::temp_dir().join(format!(
            "aulaf75ctl-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sysfs = root.join("class");
        let nodes = root.join("missing-devices");
        let wired = [
            0x06, 0, 0xff, 0x09, 1, 0xa1, 1, 0x85, 6, 0x75, 8, 0x96, 7, 2, 0xb1, 2, 0xc0,
        ];
        let radio = [
            0x06, 2, 0xff, 0x09, 2, 0xa1, 1, 0x85, 0x13, 0x75, 8, 0x95, 19, 0x81, 0, 0x91, 0, 0xc0,
        ];
        for (name, id, descriptor) in [
            ("hidraw0", "0003:0000258A:0000010C", wired.as_slice()),
            ("hidraw1", "0003:00003554:0000FA09", radio.as_slice()),
            ("hidraw2", "0003:0000258A:0000010D", wired.as_slice()),
            ("hidraw3", "0003:0000258A:0000010C", radio.as_slice()),
        ] {
            let hid = sysfs.join(name).join("device");
            fs::create_dir_all(&hid).unwrap();
            fs::write(hid.join("uevent"), format!("HID_ID={id}\n")).unwrap();
            fs::write(hid.join("report_descriptor"), descriptor).unwrap();
        }
        let devices = discover_at(&sysfs, &nodes, None).unwrap();
        assert_eq!(devices.len(), 3, "unknown IDs are excluded");
        assert!(devices[0].interface_verified);
        assert_eq!(
            preview_target(&devices).unwrap_err().code,
            "ambiguous_device"
        );
        assert_eq!(
            devices[2].error.as_ref().unwrap().code,
            "unverified_interface"
        );
        assert_eq!(read_wired(&devices[0]).unwrap_err().code, "disconnected");
        assert_eq!(
            read_wired(&devices[1]).unwrap_err().code,
            "unverified_interface"
        );
        assert_eq!(
            read_wired(&devices[2]).unwrap_err().code,
            "unverified_interface"
        );
        let mut report = crate::Report {
            schema_version: 1,
            command: "status",
            ok: false,
            devices,
            error: None,
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
        let mut inspections = 0;
        let ambiguous = apply_lighting(
            &mut report.devices,
            protocol::LightingRequest::Power(crate::power::Action::Off),
            &serde_json::json!({}),
        );
        assert_eq!(ambiguous.error.unwrap().code, "ambiguous_device");
        for command in [crate::cli::Command::Devices, crate::cli::Command::Status] {
            crate::operate(
                &mut report,
                command,
                |_| inspections += 1,
                |_, _| panic!("discovery/status must never dispatch a lighting write"),
            );
        }
        assert_eq!(
            inspections, 1,
            "only status inspects the synthetic candidates"
        );
        let mut receivers = discover_at(&sysfs, &nodes, Some(Transport::Wireless)).unwrap();
        assert_eq!(
            preview_color(&mut receivers, protocol::Color([1, 2, 3]))
                .unwrap_err()
                .code,
            "device_not_found"
        );
        inspect(&mut receivers);
        assert_eq!(receivers[0].state, "receiver_present");
        assert_eq!(receivers[0].keyboard_connected, None);
        assert!(!receivers[0].protocol_verified);
        assert!(!receivers[0].capabilities.brightness);
        assert!(!receivers[0].capabilities.brightness_visual_verified);
        let json = serde_json::to_value(&receivers).unwrap();
        assert_eq!(json[0]["capabilities"]["static_color"], false);
        assert_eq!(json[0]["error"]["code"], "wireless_status_unavailable");
        assert!(json[0].get("path").is_none() && json[0].get("descriptor").is_none());
        assert!(
            discover_at(&root.join("absent"), &nodes, None)
                .unwrap()
                .is_empty()
        );
        let runtime = root.join("runtime");
        fs::create_dir(&runtime).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        let guard = command_lock_at(&runtime, uid).unwrap();
        let lock_path = runtime.join("aulaf75ctl.lock");
        assert_eq!(fs::metadata(&lock_path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            command_lock_at(&runtime, uid).unwrap_err().code,
            "device_busy"
        );
        drop(guard);
        drop(command_lock_at(&runtime, uid).unwrap());
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            command_lock_at(&runtime, uid).unwrap_err().code,
            "unsafe_lock_file"
        );
        fs::remove_file(&lock_path).unwrap();
        std::os::unix::fs::symlink(runtime.join("absent"), &lock_path).unwrap();
        assert_eq!(
            command_lock_at(&runtime, uid).unwrap_err().code,
            "unsafe_lock_file"
        );
        fs::remove_file(&lock_path).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            command_lock_at(&runtime, uid).unwrap_err().code,
            "unsafe_runtime_directory"
        );
        let mut reply = protocol::config_request();
        reply[17] = 1;
        reply[18] = 21;
        reply[102] = 9;
        reply[134..136].copy_from_slice(&[0x5a, 0xa5]);
        let mut model = protocol::model_request();
        model[8..14].copy_from_slice(&protocol::F75_MODEL);
        let mut result = crate::static_write::ApplyResult::new(protocol::LightingRequest::Static(
            protocol::Color([137, 180, 250]),
        ));
        result.configuration = Some(protocol::parse_config(&reply).unwrap());
        result.model = Some(protocol::require_f75_model(&model).unwrap());
        result.no_op = true;
        report.devices[0].interface_number = Some(1);
        record_static_result(&mut report.devices[0], &mut result);
        let json = serde_json::to_value(&report.devices[0]).unwrap();
        assert_eq!(json["protocol_verified"], true);
        assert_eq!(json["keyboard_connected"], true);
        assert_eq!(json["configuration"]["lighting_mode"], "per_key");
        assert_eq!(json["capabilities"]["brightness"], true);
        assert_eq!(json["capabilities"]["brightness_visual_verified"], false);
        assert_eq!(json["model"]["identity"], "f75_cd");
        assert_eq!(json["state"], "static_no_op_verified");
        assert!(json.get("operation_state").is_none());
        assert!(!report.devices[0].capabilities.lighting_power);
        report.devices[0].instance = Some(crate::power::Instance {
            boot_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            hid_id: 7,
            usb_bus: 1,
            usb_address: 5,
            usb_inode: 42,
            usb_ctime: 100,
            usb_ctime_ns: 12,
        });
        update_brightness_capability(&mut report.devices[0]);
        assert!(report.devices[0].capabilities.lighting_power);
        assert!(!result.visual_verified && !result.hardware_verified);
        reply[8 + 9] = 0;
        reply[8 + 10] = 18;
        reply[8 + 92] = 9;
        reply[8 + 93] = 0x37;
        for (no_op, failed, label, legacy) in [
            (
                false,
                false,
                "effect_readback_verified_unobserved",
                "static_readback_verified_unobserved",
            ),
            (
                true,
                false,
                "effect_no_op_verified",
                "static_no_op_verified",
            ),
            (false, true, "effect_apply_failed", "static_apply_failed"),
        ] {
            let mut effect = crate::static_write::ApplyResult::new(
                protocol::LightingRequest::Effect(protocol::Effect::from_id(18).unwrap()),
            );
            effect.configuration = Some(protocol::parse_config(&reply).unwrap());
            effect.no_op = no_op;
            if failed {
                effect.error = Some(Error::new(
                    "configuration_readback_mismatch",
                    "Synthetic mismatch",
                ));
            }
            record_static_result(&mut report.devices[0], &mut effect);
            let json = serde_json::to_value(&report.devices[0]).unwrap();
            assert_eq!(json["operation_state"], label);
            assert_eq!(json["state"], legacy);
            assert_eq!(json["protocol_verified"], true);
            assert!(!effect.visual_verified && !effect.hardware_verified);
        }
        for (no_op, failed, label) in [
            (false, false, "brightness_readback_verified_unobserved"),
            (true, false, "brightness_no_op_verified"),
            (false, true, "brightness_apply_failed"),
        ] {
            let mut result = crate::static_write::ApplyResult::new(
                protocol::LightingRequest::Brightness(protocol::Brightness::parse("5").unwrap()),
            );
            result.configuration = Some(protocol::parse_config(&reply).unwrap());
            result.model = Some(protocol::require_f75_model(&model).unwrap());
            result.no_op = no_op;
            if failed {
                result.error = Some(Error::new(
                    "configuration_readback_mismatch",
                    "Synthetic mismatch",
                ));
            }
            report.devices[0].interface_number = Some(1);
            record_static_result(&mut report.devices[0], &mut result);
            let json = serde_json::to_value(&report.devices[0]).unwrap();
            assert_eq!(json["operation_state"], label);
            assert_eq!(json["capabilities"]["brightness"], true);
            assert_eq!(json["capabilities"]["brightness_visual_verified"], false);
            assert!(!result.visual_verified && !result.hardware_verified);
        }
        report.devices[0].model = None;
        let mut speed_result = crate::static_write::ApplyResult::new(
            protocol::LightingRequest::Speed(protocol::Speed::parse("0").unwrap()),
        );
        speed_result.configuration = Some(protocol::parse_config(&reply).unwrap());
        record_static_result(&mut report.devices[0], &mut speed_result);
        assert_eq!(
            report.devices[0].operation_state,
            Some("speed_readback_verified_unobserved")
        );
        assert!(report.devices[0].protocol_verified);
        assert!(!speed_result.visual_verified && !speed_result.hardware_verified);
        let mut palette_result = crate::static_write::ApplyResult::new(
            protocol::LightingRequest::Palette(protocol::Palette::parse("green").unwrap()),
        );
        palette_result.configuration = Some(protocol::parse_config(&reply).unwrap());
        record_static_result(&mut report.devices[0], &mut palette_result);
        assert_eq!(
            report.devices[0].operation_state,
            Some("palette_readback_verified_unobserved")
        );
        assert!(report.devices[0].protocol_verified);
        assert!(!palette_result.visual_verified && !palette_result.hardware_verified);
        update_brightness_capability(&mut report.devices[0]);
        assert!(!report.devices[0].capabilities.brightness);
        assert!(!report.devices[0].capabilities.lighting_power);
        reply[8 + 10] = 0;
        report.devices[0].configuration = Some(protocol::parse_config(&reply).unwrap());
        report.devices[0].model = Some(protocol::require_f75_model(&model).unwrap());
        update_brightness_capability(&mut report.devices[0]);
        assert!(!report.devices[0].capabilities.brightness);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identity_and_failures_are_transport_specific_and_private() {
        assert_eq!(
            identity("HID_ID=0003:0000258A:0000010C\n"),
            Some((0x258a, 0x010c, Transport::Wired))
        );
        assert_eq!(identify(3, 0x3554, 0xfa09), Some(Transport::Wireless));
        for (bus, vid, pid) in [
            (5, 0x258a, 0x010c),
            (3, 0x258a, 0x010d),
            (3, 0x3554, 0xfa08),
            (3, 0xb6a4, 0x4001),
        ] {
            assert_eq!(identify(bus, vid, pid), None);
        }
        assert_eq!(
            io_error(io::Error::from_raw_os_error(13)).code,
            "permission_denied"
        );
        for code in [2, 6, 19] {
            assert_eq!(
                io_error(io::Error::from_raw_os_error(code)).code,
                "disconnected"
            );
        }
        let error = io_error(io::Error::other("/dev/private serial-number"));
        assert!(!error.message.contains("private"));
    }
}
