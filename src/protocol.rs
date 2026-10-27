use serde::Serialize;

use crate::Error;

pub const REPORT_LEN: usize = 520;
pub const CONFIG_LEN: usize = 128;
pub const COMMAND_GAP: std::time::Duration = std::time::Duration::from_millis(50);
pub const LIVE_RGB_LEN: usize = 378;
pub const STATIC_RGB_LEN: usize = 384;
pub const PER_KEY_MODE: [u8; 2] = [1, 21];
pub const F75_MODEL: [u8; 6] = [3, 0, 0, 0, 0, 0xcd];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 3]);

impl Color {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let bytes = text.as_bytes();
        if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::new(
                "invalid_arguments",
                "Color must be exactly #RRGGBB (quote it in the shell)",
            ));
        }
        let mut rgb = [0; 3];
        for (index, channel) in rgb.iter_mut().enumerate() {
            *channel = u8::from_str_radix(&text[1 + index * 2..3 + index * 2], 16)
                .map_err(|_| Error::new("invalid_arguments", "Invalid RGB color"))?;
        }
        Ok(Self(rgb))
    }
}

// Fixed read requests. No arbitrary addresses or caller-supplied opcodes.
pub fn config_request() -> [u8; REPORT_LEN] {
    let mut report = [0; REPORT_LEN];
    report[..8].copy_from_slice(&[0x06, 0x84, 0, 0, 1, 0, 0x80, 0]);
    report
}

pub fn model_request() -> [u8; REPORT_LEN] {
    let mut report = [0; REPORT_LEN];
    report[..8].copy_from_slice(&[6, 0x82, 1, 0, 1, 0, 6, 0]);
    report
}

pub fn matrix_request() -> [u8; REPORT_LEN] {
    let mut report = [0; REPORT_LEN];
    report[..8].copy_from_slice(&[6, 0x86, 0, 0, 1, 0, 0x80, 1]);
    report
}

pub fn color_frame(color: Color) -> [u8; REPORT_LEN] {
    let mut report = [0; REPORT_LEN];
    report[..8].copy_from_slice(&[6, 8, 0, 0, 1, 0, 0x7a, 1]);
    for slot in report[8..8 + LIVE_RGB_LEN]
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
    {
        slot.copy_from_slice(&color.0);
    }
    report
}

// All outgoing feature reports pass this gate, including read requests. Config,
// persistent static, solid-storage and firmware commands are not in the allowlist.
pub fn validate_outgoing(report: &[u8]) -> Result<(), Error> {
    if report.len() != REPORT_LEN {
        return Err(Error::new(
            "invalid_report",
            "Wired reports must be exactly 520 bytes",
        ));
    }
    let allowed = match report[1] {
        0x84 => report == config_request(),
        0x82 => report == model_request(),
        0x86 => report == matrix_request(),
        8 => {
            report[..8] == [6, 8, 0, 0, 1, 0, 0x7a, 1]
                && report[8..8 + LIVE_RGB_LEN]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .all(|rgb| rgb == &report[8..11])
                && report[8 + LIVE_RGB_LEN..].iter().all(|byte| *byte == 0)
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(Error::new(
            "unsupported_report",
            "Report is outside the fixed wired read/color-preview allowlist",
        ))
    }
}

pub fn require_f75_model(reply: &[u8]) -> Result<Model, Error> {
    let model = parse_model(reply)?;
    if !model.matches_expected {
        return Err(Error::new(
            "unsupported_model",
            "Wired model reply does not match the documented F75 identity; no lighting write was sent",
        ));
    }
    Ok(model)
}

pub fn require_preview_mode(config: &Configuration) -> Result<(), Error> {
    if config.length != CONFIG_LEN
        || !config.signature_valid
        || config.lighting_mode != "per_key"
        || config.mode_bytes[..2] != PER_KEY_MODE
    {
        return Err(Error::new(
            "unsupported_lighting_mode",
            "Volatile preview requires self-defined flag 1 and effect 21; it never switches modes or writes configuration",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Configuration {
    pub length: usize,
    pub signature_valid: bool,
    pub lighting_mode: &'static str,
    pub mode_bytes: [u8; 3],
    pub effect_id: u8,
    pub stored_native_effect_id: u8,
    pub active_effect: Option<Effect>,
    pub effect_name_scope: &'static str,
    pub effect_name: &'static str,
    pub parameter_offset: Option<usize>,
    pub brightness: Option<u8>,
    pub speed: Option<u8>,
    pub speed_raw: Option<u8>,
    pub color_index: Option<u8>,
    pub color_index_raw: Option<u8>,
    pub color_name: &'static str,
    #[serde(skip)]
    pub(crate) bytes: [u8; CONFIG_LEN],
}

#[derive(Debug, Serialize)]
pub struct Model {
    pub payload: [u8; 6],
    pub matches_expected: bool,
    pub identity: &'static str,
}

pub fn parse_model(reply: &[u8]) -> Result<Model, Error> {
    if !(14..=REPORT_LEN).contains(&reply.len()) || reply[..8] != model_request()[..8] {
        return Err(Error::new(
            "unexpected_model_reply",
            "Model reply has an invalid length or header",
        ));
    }
    let payload: [u8; 6] = reply[8..14].try_into().unwrap();
    Ok(Model {
        payload,
        matches_expected: payload == F75_MODEL,
        identity: if payload == F75_MODEL {
            "f75_cd"
        } else {
            "unknown"
        },
    })
}

pub const OBSERVED_NATIVE_EFFECT_IDS: [u8; 15] =
    [1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 15, 16, 17];

const EFFECT_NAMES: [&str; 18] = [
    "fixed_on",
    "respire",
    "rainbow",
    "flash_away",
    "raindrops",
    "rainbow_wheel",
    "ripples",
    "stars_twinkle",
    "shadow_fade",
    "retro_snake",
    "neon_stream",
    "reaction",
    "sine_wave",
    "scanner",
    "windmill",
    "waterfall",
    "blossoming",
    "storm",
];

const PALETTE_NAMES: [&str; 8] = [
    "red", "green", "blue", "yellow", "magenta", "cyan", "white", "random",
];

// F75 Studio's EFFECTS table order, not the numeric effect ID, selects the slot.
fn effect_slot(id: u8) -> Option<(usize, &'static str)> {
    match id {
        1..=18 => Some((usize::from(id - 1), EFFECT_NAMES[usize::from(id - 1)])),
        21 => Some((18, "self_defined")),
        0 => Some((19, "off")),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Effect {
    pub id: u8,
    pub name: &'static str,
}

impl Effect {
    // Studio's has_colour flag; a saved nibble alone does not imply support.
    pub fn supports_palette(self) -> bool {
        matches!(self.id, 1..=18) && !matches!(self.id, 3 | 15..=17)
    }

    // Studio's EFFECTS marks precisely these existing slots as has_speed.
    pub fn supports_speed(self) -> bool {
        matches!(self.id, 2..=18)
    }

    pub fn from_id(id: u8) -> Result<Self, Error> {
        effect_slot(id)
            .map(|(_, name)| Self { id, name })
            .ok_or_else(|| {
                Error::new(
                    "unsupported_effect",
                    "Unknown effect ID; use 0, 1-18, or 21",
                )
            })
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        if let Ok(id) = text.parse::<u8>() {
            return Self::from_id(id);
        }
        let name = text.to_ascii_lowercase().replace(['_', ' '], "-");
        let id = match name.as_str() {
            "off" => 0,
            "self-defined" | "per-key" => 21,
            "rotating-storm" => 18,
            _ => EFFECT_NAMES.iter().position(|candidate| candidate.replace('_', "-") == name)
                .map(|slot| slot as u8 + 1).ok_or_else(|| Error::new("unsupported_effect", "Unknown or ambiguous effect name; use a complete documented name or numeric ID"))?,
        };
        Self::from_id(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brightness(u8);

impl Brightness {
    pub fn parse(text: &str) -> Result<Self, Error> {
        text.parse::<u8>()
            .ok()
            .filter(|level| *level <= 9 && text.bytes().all(|b| b.is_ascii_digit()))
            .map(Self)
            .ok_or_else(|| {
                Error::new(
                    "invalid_brightness",
                    "Brightness must be an integer from 0 through 9",
                )
            })
    }

    pub fn level(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Speed(u8);

impl Speed {
    pub fn parse(text: &str) -> Result<Self, Error> {
        text.parse::<u8>()
            .ok()
            .filter(|level| *level <= 4 && text.bytes().all(|b| b.is_ascii_digit()))
            .map(Self)
            .ok_or_else(|| Error::new("invalid_speed", "Speed must be an integer from 0 through 4"))
    }

    pub fn level(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Palette {
    pub index: u8,
    pub name: &'static str,
}

impl Palette {
    fn from_index(index: u8) -> Result<Self, Error> {
        PALETTE_NAMES.get(usize::from(index))
            .map(|&name| Self { index, name })
            .ok_or_else(|| Error::new("invalid_palette", "Palette must be red, green, blue, yellow, magenta, cyan, white, random, or an index 0 through 7"))
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let index = if text.bytes().all(|b| b.is_ascii_digit()) {
            text.parse::<u8>().ok()
        } else {
            PALETTE_NAMES
                .iter()
                .position(|name| name.eq_ignore_ascii_case(text))
                .map(|i| i as u8)
        };
        Self::from_index(index.unwrap_or(u8::MAX))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightingRequest {
    Static(Color),
    Effect(Effect),
    Brightness(Brightness),
    Speed(Speed),
    Palette(Palette),
    Power(crate::power::Action),
}

impl LightingRequest {
    pub fn command(self) -> &'static str {
        match self {
            Self::Static(_) => "lighting_static",
            Self::Effect(_) => "lighting_effect",
            Self::Brightness(_) => "lighting_brightness",
            Self::Speed(_) => "lighting_speed",
            Self::Palette(_) => "lighting_palette",
            Self::Power(action) => action.command(),
        }
    }
    pub fn rgb(self) -> Option<[u8; 3]> {
        match self {
            Self::Static(color) => Some(color.0),
            _ => None,
        }
    }
    pub fn effect(self) -> Option<Effect> {
        match self {
            Self::Effect(effect) => Some(effect),
            _ => None,
        }
    }
    pub fn brightness(self) -> Option<u8> {
        match self {
            Self::Brightness(level) => Some(level.level()),
            _ => None,
        }
    }
    pub fn speed(self) -> Option<u8> {
        match self {
            Self::Speed(level) => Some(level.level()),
            _ => None,
        }
    }
    pub fn palette(self) -> Option<Palette> {
        match self {
            Self::Palette(palette) => Some(palette),
            _ => None,
        }
    }
    pub fn power(self) -> Option<crate::power::Action> {
        match self {
            Self::Power(action) => Some(action),
            _ => None,
        }
    }
    pub fn prepare(
        self,
        config: &Configuration,
        matrix_reply: &[u8],
    ) -> Result<LightingPlan, Error> {
        match self {
            Self::Static(color) => prepare_static(config, matrix_reply, color),
            Self::Effect(effect) => prepare_effect(config, matrix_reply, effect),
            Self::Brightness(level) => prepare_brightness(config, matrix_reply, level),
            Self::Speed(level) => prepare_speed(config, matrix_reply, level),
            Self::Palette(palette) => prepare_palette(config, matrix_reply, palette),
            Self::Power(_) => Err(Error::new(
                "power_session_required",
                "Session restoration context is required for Off/On",
            )),
        }
    }
}

pub fn parse_config(reply: &[u8]) -> Result<Configuration, Error> {
    // Linux may return just header+payload or the full padded report. Neither a
    // bare feature read nor a receiver's short status report proves identity.
    if !(8 + CONFIG_LEN..=REPORT_LEN).contains(&reply.len()) {
        return Err(Error::new(
            "malformed_reply",
            "Configuration reply is truncated or oversized",
        ));
    }
    if reply[..8] != config_request()[..8] {
        return Err(Error::new(
            "unexpected_reply",
            &format!(
                "Configuration reply header {:02x?} does not match the wired read request {:02x?}",
                &reply[..8],
                &config_request()[..8]
            ),
        ));
    }
    let config = &reply[8..8 + CONFIG_LEN];
    if config[126..] != [0x5a, 0xa5] {
        return Err(Error::new(
            "invalid_signature",
            "Configuration signature is invalid",
        ));
    }
    let mode_bytes = [config[9], config[10], config[59]];
    // Byte 10 retains the native selection when byte 9 selects stored RGB.
    let active_id = match (config[9], config[10]) {
        (1, _) => Some(21),
        (0, 0..=18) => Some(config[10]),
        _ => None,
    };
    let active_effect = active_id.and_then(|id| Effect::from_id(id).ok());
    let effect = active_id.and_then(effect_slot);
    let lighting_mode = match active_id {
        Some(21) => "per_key",
        Some(1) => "solid",
        Some(0) => "off",
        Some(2..=18) => "hardware_effect",
        _ => "unknown",
    };
    let offset = effect.map(|(slot, _)| 0x3a + slot * 2);
    let brightness = offset
        .map(|o| config[o])
        .filter(|b| *b <= 9 && active_id != Some(0) && lighting_mode != "unknown");
    let speed_raw = offset.map(|o| config[o + 1] >> 4);
    let color_raw = offset.map(|o| config[o + 1] & 0x0f);
    let has_speed = active_effect.is_some_and(Effect::supports_speed);
    let has_color = active_effect.is_some_and(Effect::supports_palette);
    let color_index = color_raw.filter(|c| *c <= 7 && has_color);
    let color_name = color_index
        .map(|c| PALETTE_NAMES[usize::from(c)])
        .unwrap_or("unknown");
    Ok(Configuration {
        length: CONFIG_LEN,
        signature_valid: true,
        lighting_mode,
        mode_bytes,
        effect_id: config[10],
        stored_native_effect_id: config[10],
        active_effect,
        effect_name_scope: if active_id == Some(config[10]) {
            "active"
        } else {
            "saved_selection"
        },
        effect_name: effect_slot(config[10]).map_or("unknown", |(_, name)| name),
        parameter_offset: offset,
        brightness,
        speed: speed_raw.filter(|s| *s <= 4 && has_speed),
        speed_raw,
        color_index,
        color_index_raw: color_raw,
        color_name,
        bytes: config.try_into().unwrap(),
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct ByteChange {
    pub offset: usize,
    pub old: u8,
    pub new: u8,
}

#[derive(Debug, Serialize)]
pub struct PlannedPacket {
    pub command: u8,
    pub payload_length: usize,
    pub packet_hex: String,
}

#[derive(Debug, Serialize)]
pub struct ReviewState {
    pub configuration_hex: String,
    pub matrix_hex: String,
}

#[derive(Debug, Serialize)]
pub struct LightingPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<crate::power::Plan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
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
    pub previous_palette: Option<Palette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_palette: Option<Palette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_offset: Option<usize>,
    pub rendering: &'static str,
    pub brightness: Option<u8>,
    pub persistent: bool,
    pub approval_required: bool,
    pub hardware_write_verified: bool,
    pub no_op: bool,
    pub configuration_refresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overwrite_warning: Option<&'static str>,
    pub review_state: ReviewState,
    pub configuration_diff: Vec<ByteChange>,
    pub matrix_diff: Vec<ByteChange>,
    pub steps: Vec<PlannedPacket>,
    #[serde(skip)]
    pub(crate) original_configuration: [u8; CONFIG_LEN],
    #[serde(skip)]
    pub(crate) original_matrix: [u8; STATIC_RGB_LEN],
    #[serde(skip)]
    pub(crate) configuration: [u8; CONFIG_LEN],
    #[serde(skip)]
    pub(crate) matrix: [u8; STATIC_RGB_LEN],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStep {
    Matrix,
    Configuration,
}

impl LightingPlan {
    // Only these two packets can enter the explicit static-apply path. Bytes
    // always come from fresh, validated reads, never from the review file.
    pub fn write_packet(&self, step: WriteStep) -> Result<[u8; REPORT_LEN], Error> {
        if (self.requested_effect.is_some()
            || self.requested_brightness.is_some()
            || self.requested_speed.is_some()
            || self.requested_palette.is_some())
            && step == WriteStep::Matrix
        {
            return Err(Error::new(
                "unsupported_report",
                "Effect selection, brightness, speed and palette cannot send an RGB matrix write",
            ));
        }
        let (command, payload): (u8, &[u8]) = match step {
            WriteStep::Matrix => (6, &self.matrix),
            WriteStep::Configuration => (4, &self.configuration),
        };
        Ok(storage_packet(command, payload))
    }

    pub fn command(&self) -> &'static str {
        if let Some(power) = &self.power {
            power.action.command()
        } else if self.requested_palette.is_some() {
            "lighting_palette"
        } else if self.requested_speed.is_some() {
            "lighting_speed"
        } else if self.requested_brightness.is_some() {
            "lighting_brightness"
        } else if self.requested_effect.is_some() {
            "lighting_effect"
        } else {
            "lighting_static"
        }
    }
    pub fn plan_key(&self) -> &'static str {
        if self.power.is_some() {
            "power_plan"
        } else if self.requested_palette.is_some() {
            "palette_plan"
        } else if self.requested_speed.is_some() {
            "speed_plan"
        } else if self.requested_brightness.is_some() {
            "brightness_plan"
        } else if self.requested_effect.is_some() {
            "effect_plan"
        } else {
            "static_plan"
        }
    }
}

fn byte_diff(before: &[u8], after: &[u8]) -> Vec<ByteChange> {
    before
        .iter()
        .zip(after)
        .enumerate()
        .filter_map(|(offset, (&old, &new))| {
            (old != new).then_some(ByteChange { offset, old, new })
        })
        .collect()
}

fn storage_packet(command: u8, payload: &[u8]) -> [u8; REPORT_LEN] {
    let mut packet = [0u8; REPORT_LEN];
    packet[..8].copy_from_slice(&[
        6,
        command,
        0,
        0,
        1,
        0,
        payload.len() as u8,
        (payload.len() >> 8) as u8,
    ]);
    packet[8..8 + payload.len()].copy_from_slice(payload);
    packet
}

fn planned_packet(command: u8, payload: &[u8]) -> PlannedPacket {
    let packet = storage_packet(command, payload);
    PlannedPacket {
        command,
        payload_length: payload.len(),
        packet_hex: packet.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

// Independent adaptation of Studio's solid -> set_per_key -> edit_led_block:
// store planar colors, then select onboard self-defined effect 21. Prepared
// persistent reports are deliberately NOT accepted by validate_outgoing().
// Static apply has its own reviewed/snapshotted, one-shot execution path.
pub fn prepare_static(
    config: &Configuration,
    matrix_reply: &[u8],
    color: Color,
) -> Result<LightingPlan, Error> {
    if config.length != CONFIG_LEN
        || !config.signature_valid
        || config.bytes[126..] != [0x5a, 0xa5]
        || config.lighting_mode == "unknown"
        || config.bytes[9] > 1
        || (config.effect_id == 21 && config.bytes[9] != 1)
        || config.brightness.is_none()
        || config.bytes[0x5e] > 9
        || !config.speed_raw.is_some_and(|s| s <= 4)
        || !config.color_index_raw.is_some_and(|c| c <= 7)
    {
        return Err(Error::new(
            "unsupported_configuration",
            "Cannot prepare static lighting from unknown mode/parameters or unavailable brightness; no settings were changed",
        ));
    }
    let before = parse_matrix(matrix_reply)?;
    let mut matrix = *before;
    for (channel, value) in color.0.into_iter().enumerate() {
        matrix[channel * 126..(channel + 1) * 126].fill(value);
    }
    // Studio accepts caller-selected brightness; mode selection does not
    // require copying the active effect's level into the self-defined slot.
    let brightness = config.bytes[0x5e];
    let mut modified = config.bytes;
    modified[9] = 1;
    modified[10] = 21;
    // Preserve self-defined brightness and parameters (speed/color controls
    // are inapplicable to effect 21), along with all other effect settings.
    Ok(build_plan(
        config,
        before,
        modified,
        matrix,
        LightingRequest::Static(color),
        Some(brightness),
        None,
    ))
}

pub fn prepare_effect(
    config: &Configuration,
    matrix_reply: &[u8],
    effect: Effect,
) -> Result<LightingPlan, Error> {
    let effect = Effect::from_id(effect.id)?;
    let previous = config.active_effect.ok_or_else(|| Error::new("unsupported_configuration", "Current configuration uses an unknown effect/layout; refusing to replace it with defaults"))?;
    if config.length != CONFIG_LEN
        || !config.signature_valid
        || config.bytes[126..] != [0x5a, 0xa5]
        || config.bytes[9] > 1
    {
        return Err(Error::new(
            "unsupported_configuration",
            "Effect selection requires a validated known configuration; no settings were changed",
        ));
    }
    let matrix = parse_matrix(matrix_reply)?;
    let mut modified = config.bytes;
    // Compare the active mode, not the retained native selection.
    if previous.id != effect.id {
        modified[9] = u8::from(effect.id == 21);
        modified[10] = effect.id;
    }
    // Reuse the status decoder for the selected effect's saved parameters.
    let mut reply = config_request();
    reply[8..8 + CONFIG_LEN].copy_from_slice(&modified);
    let selected = parse_config(&reply)?;
    if (effect.id != 0 && selected.brightness.is_none())
        || (effect.supports_speed() && selected.speed.is_none())
        || (effect.supports_palette() && selected.color_index.is_none())
    {
        return Err(Error::new(
            "unsupported_effect_parameters",
            "Selected effect has unverified saved parameters; they will not be overwritten or normalized",
        ));
    }
    Ok(build_plan(
        config,
        matrix,
        modified,
        *matrix,
        LightingRequest::Effect(effect),
        selected.brightness,
        Some(previous),
    ))
}

// Resolve through the same metadata/decoder as status; in particular ID 21 is
// slot 18, not slot 21. Off has no supported brightness parameter.
pub fn brightness_offset(config: &Configuration) -> Result<usize, Error> {
    let effect = config.active_effect;
    if config.length != CONFIG_LEN
        || !config.signature_valid
        || config.bytes[126..] != [0x5a, 0xa5]
        || config.bytes[9] > 1
        || config.brightness.is_none()
        || (effect.is_some_and(Effect::supports_speed) && config.speed.is_none())
        || (effect.is_some_and(Effect::supports_palette) && config.color_index.is_none())
    {
        return Err(Error::new(
            "unsupported_brightness",
            "Brightness requires a verified active effect with supported saved parameters; Off and unknown configurations are not changed",
        ));
    }
    config.parameter_offset.ok_or_else(|| {
        Error::new(
            "unsupported_brightness",
            "Active effect has no documented brightness slot",
        )
    })
}

pub fn prepare_brightness(
    config: &Configuration,
    matrix_reply: &[u8],
    level: Brightness,
) -> Result<LightingPlan, Error> {
    let offset = brightness_offset(config)?;
    let matrix = parse_matrix(matrix_reply)?;
    let mut modified = config.bytes;
    modified[offset] = level.level();
    Ok(build_plan(
        config,
        matrix,
        modified,
        *matrix,
        LightingRequest::Brightness(level),
        Some(level.level()),
        None,
    ))
}

pub fn prepare_speed(
    config: &Configuration,
    matrix_reply: &[u8],
    level: Speed,
) -> Result<LightingPlan, Error> {
    let effect = config.active_effect.ok_or_else(|| {
        Error::new(
            "unsupported_configuration",
            "Active lighting mode is unknown",
        )
    })?;
    if !effect.supports_speed() {
        return Err(Error::new(
            "unsupported_speed_effect",
            "Active effect does not support speed; Fixed On, Self-defined and Off are not changed",
        ));
    }
    if config.length != CONFIG_LEN
        || !config.signature_valid
        || config.bytes[126..] != [0x5a, 0xa5]
        || config.bytes[9] > 1
        || config.brightness.is_none()
        || config.speed.is_none()
    {
        return Err(Error::new(
            "unsupported_configuration",
            "Speed requires a validated active effect with supported saved brightness and speed; no defaults are substituted",
        ));
    }
    let offset = config.parameter_offset.expect("known effect has a slot") + 1;
    let matrix = parse_matrix(matrix_reply)?;
    let mut modified = config.bytes;
    // Preserve all palette bits, even when their meaning is undocumented.
    modified[offset] = (level.level() << 4) | (modified[offset] & 0x0f);
    Ok(build_plan(
        config,
        matrix,
        modified,
        *matrix,
        LightingRequest::Speed(level),
        config.brightness,
        None,
    ))
}

pub fn prepare_palette(
    config: &Configuration,
    matrix_reply: &[u8],
    palette: Palette,
) -> Result<LightingPlan, Error> {
    let palette = Palette::from_index(palette.index)?;
    let effect = config.active_effect.ok_or_else(|| {
        Error::new(
            "unsupported_configuration",
            "Active lighting mode is unknown",
        )
    })?;
    if !effect.supports_palette() {
        return Err(Error::new(
            "unsupported_palette_effect",
            "Active effect does not support palettes; its stored raw nibble does not establish support",
        ));
    }
    if config.length != CONFIG_LEN
        || !config.signature_valid
        || config.bytes[126..] != [0x5a, 0xa5]
        || config.bytes[9] > 1
        || config.brightness.is_none()
    {
        return Err(Error::new(
            "unsupported_configuration",
            "Palette selection requires a validated active effect and saved brightness; no defaults are substituted",
        ));
    }
    let offset = config.parameter_offset.expect("known effect has a slot") + 1;
    let matrix = parse_matrix(matrix_reply)?;
    let mut modified = config.bytes;
    // Speed is unrelated to palette selection, including undocumented nibbles.
    modified[offset] = (modified[offset] & 0xf0) | palette.index;
    Ok(build_plan(
        config,
        matrix,
        modified,
        *matrix,
        LightingRequest::Palette(palette),
        config.brightness,
        None,
    ))
}

fn build_plan(
    config: &Configuration,
    before: &[u8; STATIC_RGB_LEN],
    modified: [u8; CONFIG_LEN],
    matrix: [u8; STATIC_RGB_LEN],
    request: LightingRequest,
    brightness: Option<u8>,
    previous_effect: Option<Effect>,
) -> LightingPlan {
    let matrix_diff = byte_diff(before, &matrix);
    let configuration_diff = byte_diff(&config.bytes, &modified);
    let mut steps = Vec::new();
    if !matrix_diff.is_empty() {
        steps.push(planned_packet(6, &matrix));
    }
    // Studio always reselects the effect after storing new colors. Keep that
    // proven refresh until a matrix-only update is independently verified.
    let configuration_refresh = !matrix_diff.is_empty() && configuration_diff.is_empty();
    if !configuration_diff.is_empty() || !matrix_diff.is_empty() {
        steps.push(planned_packet(4, &modified));
    }
    LightingPlan {
        power: None,
        rgb: request.rgb(),
        requested_effect: request.effect(),
        previous_effect,
        active_effect: request.brightness().or(request.speed()).or(request.palette().map(|p| p.index)).and(config.active_effect),
        previous_brightness: request.brightness().and(config.brightness),
        requested_brightness: request.brightness(),
        previous_speed: request.speed().and(config.speed),
        requested_speed: request.speed(),
        previous_palette: request.palette().and(config.color_index_raw).map(|index| Palette { index, name: PALETTE_NAMES.get(usize::from(index)).copied().unwrap_or("unknown") }),
        requested_palette: request.palette(),
        parameter_offset: if request.speed().is_some() || request.palette().is_some() { config.parameter_offset.map(|o| o + 1) } else { request.brightness().and(config.parameter_offset) },
        rendering: if request.effect().is_some() || request.brightness().is_some() || request.speed().is_some() || request.palette().is_some() { "onboard_effect" } else { "onboard_self_defined" },
        brightness,
        persistent: true,
        approval_required: true,
        hardware_write_verified: false,
        no_op: steps.is_empty(),
        configuration_refresh,
        overwrite_warning: (!matrix_diff.is_empty()).then_some("Overwrites stored per-key RGB colors in group 0; the previous layout is not retained on the keyboard"),
        review_state: ReviewState {
            configuration_hex: config.bytes.iter().map(|b| format!("{b:02x}")).collect(),
            matrix_hex: before.iter().map(|b| format!("{b:02x}")).collect(),
        },
        configuration_diff,
        matrix_diff,
        steps,
        original_configuration: config.bytes,
        original_matrix: *before,
        configuration: modified,
        matrix,
    }
}

pub fn parse_matrix(matrix_reply: &[u8]) -> Result<&[u8; STATIC_RGB_LEN], Error> {
    if !(8 + STATIC_RGB_LEN..=REPORT_LEN).contains(&matrix_reply.len())
        || matrix_reply[..8] != matrix_request()[..8]
    {
        return Err(Error::new(
            "unexpected_matrix_reply",
            "Static RGB read has an invalid length/header; no template will be substituted",
        ));
    }
    Ok(matrix_reply[8..8 + STATIC_RGB_LEN].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    pub vendor_collection: bool,
    pub wired: bool,
    pub wireless: bool,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u32,
    report: u32,
    size: u32,
    count: u32,
}

// Inspect HID short items, respecting global push/pop and collection scope.
// Unsupported/ambiguous descriptors fail closed. These shapes only select read
// candidates; the configuration exchange remains the wired protocol proof.
pub fn inspect_descriptor(bytes: &[u8]) -> Option<Interface> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return None;
    }
    let mut globals = Globals::default();
    let mut stack = Vec::new();
    let mut collections = Vec::new();
    let mut result = Interface::default();
    let (mut feature, mut input, mut output) = (0u32, 0u32, 0u32);
    let mut at = 0;
    while at < bytes.len() {
        let prefix = bytes[at];
        if prefix == 0xfe {
            return None;
        }
        let length = [0, 1, 2, 4][usize::from(prefix & 3)];
        let data = bytes.get(at + 1..at + 1 + length)?;
        let value = data
            .iter()
            .enumerate()
            .fold(0, |n, (i, b)| n | (u32::from(*b) << (8 * i)));
        at += 1 + length;
        match (prefix >> 2 & 3, prefix >> 4) {
            (1, 0) => globals.page = value,
            (1, 7) => globals.size = value,
            (1, 8) if (1..=255).contains(&value) => globals.report = value,
            (1, 8) => return None,
            (1, 9) => globals.count = value,
            (1, 10) if stack.len() < 16 => stack.push(globals),
            (1, 10) => return None,
            (1, 11) => globals = stack.pop()?,
            (0, 10) if collections.len() < 16 => {
                // A nested collection inherits the top-level collection's page.
                let page = collections.first().copied().unwrap_or(globals.page);
                result.vendor_collection |= page >= 0xff00;
                collections.push(page);
            }
            (0, 10) => return None,
            (0, 12) => {
                collections.pop()?;
            }
            (0, tag @ (8 | 9 | 11)) => {
                let page = *collections.first()?;
                let bits = globals.size.checked_mul(globals.count)?;
                if bits > 4096 * 8 {
                    return None;
                }
                if page >= 0xff00 && globals.report == 6 && tag == 11 && value & 1 == 0 {
                    feature = feature.checked_add(bits)?;
                }
                if page == 0xff02 && globals.report == 0x13 {
                    match tag {
                        8 => input = input.checked_add(bits)?,
                        9 => output = output.checked_add(bits)?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() || !collections.is_empty() {
        return None;
    }
    result.wired = feature == (REPORT_LEN as u32 - 1) * 8;
    result.wireless = input == 19 * 8 && output == 19 * 8;
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_changes_only_the_eligible_active_nibble_with_any_saved_speed() {
        let mut matrix = matrix_request();
        for (i, b) in matrix[8..392].iter_mut().enumerate() {
            *b = i as u8;
        }
        for (id, offset) in [(1, 59), (2, 61), (5, 67), (6, 69), (18, 93)] {
            for speed in 0..=15 {
                let mut reply = config_fixture(u8::from(id == 21), id);
                for slot in 0..20 {
                    reply[8 + 58 + slot * 2] = 9;
                    reply[8 + 59 + slot * 2] = 0x23;
                }
                reply[8 + offset] = (speed << 4) | 3;
                reply[8 + 26] = 0xa7;
                let config = parse_config(&reply).unwrap();
                for name in ["red", "blue", "yellow", "random"] {
                    let palette = Palette::parse(name).unwrap();
                    let plan = prepare_palette(&config, &matrix, palette).unwrap();
                    assert_eq!(plan.active_effect.unwrap().id, id);
                    assert_eq!(
                        plan.previous_palette,
                        Some(Palette::parse("yellow").unwrap())
                    );
                    assert_eq!(plan.requested_palette, Some(palette));
                    assert_eq!(plan.parameter_offset, Some(offset));
                    let mut expected = config.bytes;
                    expected[offset] = (speed << 4) | palette.index;
                    assert_eq!(plan.configuration, expected);
                    assert_eq!(plan.matrix.as_slice(), &matrix[8..392]);
                    assert!(plan.matrix_diff.is_empty() && !plan.configuration_refresh);
                    assert_eq!(plan.no_op, name == "yellow");
                    assert_eq!(plan.configuration_diff.len(), usize::from(!plan.no_op));
                    assert_eq!(
                        plan.steps.iter().map(|s| s.command).collect::<Vec<_>>(),
                        if plan.no_op { vec![] } else { vec![4] }
                    );
                    let packet = plan.write_packet(WriteStep::Configuration).unwrap();
                    assert_eq!(packet[..8], [6, 4, 0, 0, 1, 0, 128, 0]);
                    assert_eq!(&packet[8..136], &expected);
                    assert_eq!(
                        plan.write_packet(WriteStep::Matrix).unwrap_err().code,
                        "unsupported_report"
                    );
                    assert!(validate_outgoing(&packet).is_err());
                }
            }
        }
        for id in (0..=18).chain([19, 20, 21, 255]) {
            let mut reply = config_fixture(u8::from(id == 21), id);
            if let Some((slot, _)) = effect_slot(id) {
                reply[8 + 58 + slot * 2] = 9;
                reply[8 + 59 + slot * 2] = 0xf3;
            }
            let plan = prepare_palette(
                &parse_config(&reply).unwrap(),
                &matrix,
                Palette::parse("green").unwrap(),
            );
            assert_eq!(
                plan.is_ok(),
                [1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 18].contains(&id),
                "effect {id}"
            );
            if matches!(id, 0 | 3 | 15..=17 | 21) {
                assert_eq!(plan.unwrap_err().code, "unsupported_palette_effect");
            }
        }
        let mut reply = config_fixture(0, 1);
        reply[8 + 58] = 9;
        reply[8 + 59] = 0xaf;
        let config = parse_config(&reply).unwrap();
        let plan = prepare_palette(&config, &matrix, Palette::parse("blue").unwrap()).unwrap();
        assert_eq!(
            plan.previous_palette,
            Some(Palette {
                index: 15,
                name: "unknown"
            })
        );
        assert_eq!(plan.configuration[59], 0xa2);
        assert!(
            prepare_palette(
                &config,
                &matrix,
                Palette {
                    index: 8,
                    name: "unknown"
                }
            )
            .is_err()
        );
        for (offset, byte) in [(9, 2), (58, 10)] {
            let mut invalid = reply;
            invalid[8 + offset] = byte;
            assert!(
                prepare_palette(
                    &parse_config(&invalid).unwrap(),
                    &matrix,
                    Palette::parse("red").unwrap()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn speed_edits_only_the_supported_active_nibble_even_for_unknown_palettes() {
        let mut matrix = matrix_request();
        for (i, b) in matrix[8..392].iter_mut().enumerate() {
            *b = i as u8;
        }
        for (id, offset) in [(2, 61), (3, 63), (18, 93)] {
            for palette in 0..=15 {
                let mut reply = config_fixture(u8::from(id == 21), id);
                for slot in 0..20 {
                    reply[8 + 58 + slot * 2] = 9;
                    reply[8 + 59 + slot * 2] = 0x23;
                }
                reply[8 + offset] = 0x20 | palette;
                reply[8 + 26] = 0xa7;
                let config = parse_config(&reply).unwrap();
                for value in ["0", "4", "2"] {
                    let speed = Speed::parse(value).unwrap();
                    let plan = prepare_speed(&config, &matrix, speed).unwrap();
                    assert_eq!(plan.active_effect.unwrap().id, id);
                    assert_eq!(plan.previous_speed, Some(2));
                    assert_eq!(plan.requested_speed, Some(speed.level()));
                    assert_eq!(plan.parameter_offset, Some(offset));
                    let mut expected = config.bytes;
                    expected[offset] = (speed.level() << 4) | palette;
                    assert_eq!(plan.configuration, expected);
                    assert_eq!(plan.matrix.as_slice(), &matrix[8..392]);
                    assert!(plan.matrix_diff.is_empty() && !plan.configuration_refresh);
                    assert_eq!(plan.no_op, value == "2");
                    assert_eq!(
                        plan.steps.iter().map(|p| p.command).collect::<Vec<_>>(),
                        if plan.no_op { vec![] } else { vec![4] }
                    );
                    assert_eq!(plan.configuration_diff.len(), usize::from(!plan.no_op));
                    let packet = plan.write_packet(WriteStep::Configuration).unwrap();
                    assert_eq!(packet[..8], [6, 4, 0, 0, 1, 0, 128, 0]);
                    assert_eq!(&packet[8..136], &expected);
                    assert_eq!(
                        plan.write_packet(WriteStep::Matrix).unwrap_err().code,
                        "unsupported_report"
                    );
                    assert!(validate_outgoing(&packet).is_err());
                }
            }
        }
        for id in (0..=18).chain([19, 20, 21, 255]) {
            let mut reply = config_fixture(u8::from(id == 21), id);
            if let Some((slot, _)) = effect_slot(id) {
                reply[8 + 58 + slot * 2] = 9;
                reply[8 + 59 + slot * 2] = 0x2f;
            }
            let config = parse_config(&reply).unwrap();
            let plan = prepare_speed(&config, &matrix, Speed::parse("0").unwrap());
            assert_eq!(plan.is_ok(), matches!(id, 2..=18), "effect {id}");
            if matches!(id, 0 | 1 | 21) {
                assert_eq!(plan.unwrap_err().code, "unsupported_speed_effect");
            }
        }
        for (offset, byte) in [(9, 2), (62, 10), (63, 0x5f)] {
            let mut reply = config_fixture(0, 3);
            reply[8 + 62] = 9;
            reply[8 + 63] = 0x2f;
            reply[8 + offset] = byte;
            assert!(
                prepare_speed(
                    &parse_config(&reply).unwrap(),
                    &matrix,
                    Speed::parse("4").unwrap()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn brightness_changes_only_the_active_slot_and_never_the_mode_or_matrix() {
        let mut matrix = matrix_request();
        for (i, byte) in matrix[8..392].iter_mut().enumerate() {
            *byte = i as u8;
        }
        for (stored, id, offset) in [
            (1, 1, 58),
            (3, 3, 62),
            (18, 18, 92),
            (21, 21, 94),
            (18, 21, 94),
        ] {
            let mut reply = config_fixture(u8::from(id == 21), stored);
            for slot in 0..20 {
                reply[8 + 58 + slot * 2] = 5;
                reply[8 + 59 + slot * 2] = 0x37;
            }
            reply[8 + 26] = 0xa7;
            let config = parse_config(&reply).unwrap();
            for value in ["0", "9", "5"] {
                let level = Brightness::parse(value).unwrap();
                let plan = prepare_brightness(&config, &matrix, level).unwrap();
                assert_eq!(plan.active_effect.unwrap().id, id);
                assert_eq!(plan.parameter_offset, Some(offset));
                assert_eq!(plan.previous_brightness, Some(5));
                assert_eq!(plan.requested_brightness, Some(level.level()));
                let mut expected = config.bytes;
                expected[offset] = level.level();
                assert_eq!(plan.configuration, expected);
                assert_eq!(plan.original_matrix, plan.matrix);
                assert_eq!(plan.matrix.as_slice(), &matrix[8..392]);
                assert!(plan.matrix_diff.is_empty() && !plan.configuration_refresh);
                assert_eq!(plan.no_op, value == "5");
                assert_eq!(plan.configuration_diff.len(), usize::from(value != "5"));
                assert_eq!(
                    plan.steps.iter().map(|p| p.command).collect::<Vec<_>>(),
                    if plan.no_op { vec![] } else { vec![4] }
                );
                let packet = plan.write_packet(WriteStep::Configuration).unwrap();
                assert_eq!(packet[..8], [6, 4, 0, 0, 1, 0, 128, 0]);
                assert_eq!(&packet[8..136], &expected);
                assert_eq!(
                    plan.write_packet(WriteStep::Matrix).unwrap_err().code,
                    "unsupported_report"
                );
                assert!(validate_outgoing(&packet).is_err());
            }
        }
        for id in [0, 19, 20, 255] {
            let config = parse_config(&config_fixture(0, id)).unwrap();
            assert_eq!(
                prepare_brightness(&config, &matrix, Brightness::parse("5").unwrap())
                    .unwrap_err()
                    .code,
                "unsupported_brightness"
            );
        }
        for (offset, value) in [(9, 2), (92, 10), (93, 0x57), (93, 0x38)] {
            let mut reply = config_fixture(0, 18);
            reply[8 + 92] = 9;
            reply[8 + 93] = 0x37;
            reply[8 + offset] = value;
            let config = parse_config(&reply).unwrap();
            assert!(prepare_brightness(&config, &matrix, Brightness::parse("5").unwrap()).is_err());
        }
    }

    #[test]
    fn effect_names_and_ids_are_exact_and_selection_preserves_all_saved_parameters() {
        for id in (0..=18).chain([21]) {
            let effect = Effect::from_id(id).unwrap();
            assert_eq!(Effect::parse(effect.name).unwrap(), effect);
            assert_eq!(Effect::parse(&id.to_string()).unwrap(), effect);
        }
        assert_eq!(Effect::parse("Rotating Storm").unwrap().id, 18);
        assert_eq!(Effect::parse("per-key").unwrap().id, 21);
        for name in [
            "rain",
            "fixed",
            "19",
            "20",
            "22",
            "255",
            "256",
            "-1",
            "rainbow wheel extra",
        ] {
            assert!(Effect::parse(name).is_err(), "{name}");
        }
        let mut reply = config_fixture(1, 21);
        reply[8 + 26] = 0xa7;
        for slot in 0..20 {
            reply[8 + 58 + slot * 2] = (slot % 10) as u8;
            reply[8 + 59 + slot * 2] = 0x23;
        }
        let config = parse_config(&reply).unwrap();
        let mut matrix = matrix_request();
        for (i, b) in matrix[8..392].iter_mut().enumerate() {
            *b = i as u8;
        }
        for id in [1, 3, 18, 0, 21] {
            let plan = prepare_effect(&config, &matrix, Effect::from_id(id).unwrap()).unwrap();
            assert!(plan.matrix_diff.is_empty());
            assert!(plan.overwrite_warning.is_none() && !plan.configuration_refresh);
            assert_eq!(plan.original_matrix, plan.matrix);
            assert_eq!(plan.requested_effect.unwrap().id, id);
            assert!(plan.rgb.is_none());
            for i in 0..CONFIG_LEN {
                let expected = match i {
                    9 => u8::from(id == 21),
                    10 => id,
                    _ => reply[8 + i],
                };
                assert_eq!(plan.configuration[i], expected);
            }
            assert_eq!(
                plan.steps.iter().map(|p| p.command).collect::<Vec<_>>(),
                if id == 21 { vec![] } else { vec![4] }
            );
            assert_eq!(
                plan.write_packet(WriteStep::Matrix).unwrap_err().code,
                "unsupported_report"
            );
        }
        let mut observed_storm = config_fixture(1, 18);
        observed_storm[8 + 92] = 8;
        observed_storm[8 + 93] = 0x37;
        let storm = prepare_effect(
            &parse_config(&observed_storm).unwrap(),
            &matrix,
            Effect::from_id(18).unwrap(),
        )
        .unwrap();
        assert!(!storm.no_op);
        assert_eq!(
            storm.configuration[9], 0,
            "selecting the retained native ID must activate the native selector"
        );
        for selector in [0, 1] {
            observed_storm[8 + 9] = selector;
            let back = prepare_effect(
                &parse_config(&observed_storm).unwrap(),
                &matrix,
                Effect::from_id(21).unwrap(),
            )
            .unwrap();
            assert_eq!(back.no_op, selector == 1);
            assert_eq!(back.configuration[9], 1);
            assert_eq!(back.configuration[10], if selector == 1 { 18 } else { 21 });
            assert_eq!(back.matrix, back.original_matrix);
            assert_eq!(
                back.steps.iter().map(|p| p.command).collect::<Vec<_>>(),
                if selector == 1 { vec![] } else { vec![4] }
            );
            assert_eq!(back.configuration[94], observed_storm[8 + 94]);
        }
        let mut invalid = config_fixture(1, 21);
        invalid[8 + 62] = 10;
        assert_eq!(
            prepare_effect(
                &parse_config(&invalid).unwrap(),
                &matrix,
                Effect::from_id(3).unwrap()
            )
            .unwrap_err()
            .code,
            "unsupported_effect_parameters"
        );
        // Saved Storm parameters from the wired test. Studio's editor would write
        // brightness 9 and (3<<4)|7 == 0x37; they already match, so preserving
        // them yields the same configuration without a speculative activation.
        let mut saved = config_fixture(1, 21);
        saved[8 + 92] = 9;
        saved[8 + 93] = 0x37;
        let storm = prepare_effect(
            &parse_config(&saved).unwrap(),
            &matrix,
            Effect::from_id(18).unwrap(),
        )
        .unwrap();
        let mut expected: [u8; CONFIG_LEN] = saved[8..136].try_into().unwrap();
        expected[9] = 0;
        expected[10] = 18;
        assert_eq!(storm.configuration, expected);
        let packet = storm.write_packet(WriteStep::Configuration).unwrap();
        assert_eq!(packet[..8], [6, 4, 0, 0, 1, 0, 128, 0]);
        assert_eq!(&packet[8..136], &expected);
        assert_eq!(storm.brightness, Some(9));
        assert_eq!(storm.matrix, storm.original_matrix);
    }

    #[test]
    fn color_preview_packets_are_uniform_volatile_and_allowlisted() {
        let rgb = Color::parse("#89b4FA").unwrap();
        let packet = color_frame(rgb);
        assert_eq!(packet[..8], [6, 8, 0, 0, 1, 0, 0x7a, 1]);
        assert_eq!(packet.len(), 520);
        assert_eq!(packet[8..386].as_chunks::<3>().0.len(), 126);
        assert!(
            packet[8..386]
                .as_chunks::<3>()
                .0
                .iter()
                .all(|c| *c == [137, 180, 250])
        );
        assert!(packet[386..].iter().all(|b| *b == 0));
        validate_outgoing(&packet).unwrap();
        validate_outgoing(&config_request()).unwrap();
        validate_outgoing(&model_request()).unwrap();
        assert!(model_request()[8..].iter().all(|b| *b == 0));
        for command in [4, 6, 0x0a, 0x81, 0x88, 0xff] {
            let mut rejected = packet;
            rejected[1] = command;
            assert_eq!(
                validate_outgoing(&rejected).unwrap_err().code,
                "unsupported_report"
            );
        }
        for index in [0, 2, 3, 4, 5, 6, 7, 11, 386] {
            let mut rejected = packet;
            rejected[index] ^= 1;
            assert!(validate_outgoing(&rejected).is_err());
        }
        assert_eq!(
            validate_outgoing(&packet[..519]).unwrap_err().code,
            "invalid_report"
        );
        let mut bad_read = config_request();
        bad_read[8] = 1;
        assert!(validate_outgoing(&bad_read).is_err());
        for text in [
            "89b4fa", "#fff", "#89b4fax", "#zzzzzz", "#00000é", " #89b4fa",
        ] {
            assert!(Color::parse(text).is_err());
        }
    }

    #[test]
    fn per_key_flag_and_effect_gate_preview_independently_of_fixed_on_parameters() {
        let mut reply = config_fixture(1, 21);
        for saved_parameter in [0, 0x33, 0x47, 0xff] {
            reply[8 + 59] = saved_parameter;
            require_preview_mode(&parse_config(&reply).unwrap()).unwrap();
        }
        for (flag, effect) in [(0, 1), (0, 21), (1, 1), (0, 0), (1, 19)] {
            let config = parse_config(&config_fixture(flag, effect)).unwrap();
            assert_eq!(
                require_preview_mode(&config).unwrap_err().code,
                "unsupported_lighting_mode"
            );
        }
        let mut model = model_request();
        model[8..14].copy_from_slice(&F75_MODEL);
        require_f75_model(&model[..14]).unwrap();
        require_f75_model(&model).unwrap();
        for index in 0..14 {
            let mut bad = model;
            bad[index] ^= 1;
            assert!(require_f75_model(&bad).is_err());
            if index >= 8 {
                assert_eq!(parse_model(&bad).unwrap().identity, "unknown");
            }
        }
        assert!(require_f75_model(&model[..13]).is_err());
    }

    fn config_fixture(flag: u8, effect: u8) -> [u8; REPORT_LEN] {
        let mut reply = config_request();
        reply[8 + 9] = flag;
        reply[8 + 10] = effect;
        reply[8 + 58] = 7;
        reply[8 + 59] = 0x33;
        reply[8 + 94] = 4;
        reply[8 + 95] = 0x20;
        reply[134..136].copy_from_slice(&[0x5a, 0xa5]);
        reply
    }

    #[test]
    fn active_mode_and_saved_selection_are_distinct_in_schema_one() {
        for (selector, stored, active, mode) in [
            (0, 3, Some(3), "hardware_effect"),
            (1, 18, Some(21), "per_key"),
            (1, 3, Some(21), "per_key"),
            (0, 0, Some(0), "off"),
            (2, 3, None, "unknown"),
            (255, 18, None, "unknown"),
            (0, 21, None, "unknown"),
        ] {
            let config = parse_config(&config_fixture(selector, stored)).unwrap();
            let json = serde_json::to_value(&config).unwrap();
            assert_eq!(config.lighting_mode, mode);
            assert_eq!(config.active_effect.map(|e| e.id), active);
            assert_eq!(json["stored_native_effect_id"], stored);
            assert_eq!(json["effect_id"], stored);
            assert_eq!(
                config.effect_name_scope,
                if active == Some(stored) {
                    "active"
                } else {
                    "saved_selection"
                }
            );
            if active.is_none() {
                assert!(config.parameter_offset.is_none() && config.brightness.is_none());
                assert!(
                    prepare_effect(&config, &matrix_request(), Effect::from_id(1).unwrap())
                        .is_err()
                );
            }
        }
        assert_eq!(OBSERVED_NATIVE_EFFECT_IDS.len(), 15);
        for id in [9, 14, 18] {
            assert!(!OBSERVED_NATIVE_EFFECT_IDS.contains(&id));
            assert!(Effect::from_id(id).is_ok());
        }
    }

    #[test]
    fn studio_effect_slots_explain_observed_mode_and_preserve_unknown_values() {
        let config = parse_config(&config_fixture(0, 1)).unwrap();
        assert_eq!(config.mode_bytes, [0, 1, 51]);
        assert_eq!(config.lighting_mode, "solid");
        assert_eq!(config.effect_name, "fixed_on");
        assert_eq!(config.parameter_offset, Some(58));
        assert_eq!(config.brightness, Some(7));
        assert_eq!(config.speed_raw, Some(3));
        assert_eq!(config.speed, None, "Fixed On has no speed control");
        assert_eq!(config.color_name, "yellow");
        assert_eq!(config.color_index, Some(3));
        let custom = parse_config(&config_fixture(1, 21)).unwrap();
        assert_eq!(custom.parameter_offset, Some(94));
        assert_eq!(custom.brightness, Some(4));
        assert_eq!(custom.color_index, None);
        assert_eq!(custom.color_name, "unknown");
        for (flag, id) in [(0, 19), (0, 20)] {
            let config = parse_config(&config_fixture(flag, id)).unwrap();
            assert_eq!(config.lighting_mode, "unknown");
            assert!(prepare_static(&config, &matrix_request(), Color([1, 2, 3])).is_err());
        }
        // Known IDs remain recognizable even when their selector is unsuitable
        // for a write. Do not turn status recognition into a preview bypass.
        for (flag, id, mode) in [(1, 1, "per_key"), (0, 21, "unknown"), (2, 1, "unknown")] {
            let config = parse_config(&config_fixture(flag, id)).unwrap();
            assert_eq!(config.lighting_mode, mode);
            assert!(require_preview_mode(&config).is_err());
            if flag == 2 || id == 21 {
                assert!(prepare_static(&config, &matrix_request(), Color([1, 2, 3])).is_err());
            }
        }
        let mut malformed = config_fixture(0, 2);
        malformed[8 + 60] = 10;
        malformed[8 + 61] = 0xff;
        let invalid = parse_config(&malformed).unwrap();
        assert_eq!(invalid.brightness, None);
        assert_eq!(invalid.speed, None);
        assert_eq!(invalid.color_index, None);
        let json = serde_json::to_value(invalid).unwrap();
        assert!(json["brightness"].is_null());
        assert!(
            json.get("bytes").is_none(),
            "never expose full config in status"
        );
    }

    fn decode_packet(packet: &PlannedPacket) -> Vec<u8> {
        packet
            .packet_hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn per_key_selector_overrides_stored_storm_and_preserves_unrelated_bytes() {
        let mut reply = config_fixture(1, 18);
        reply[8 + 26] = 0xa7; // Preserve arbitrary current data, never normalize it.
        reply[8 + 92] = 8;
        reply[8 + 93] = 0x37;
        reply[8 + 94] = 9;
        let config = parse_config(&reply).unwrap();
        assert_eq!(config.mode_bytes, [1, 18, 51]);
        assert_eq!(config.lighting_mode, "per_key");
        assert_eq!(config.effect_id, 18);
        assert_eq!(config.effect_name, "storm");
        assert_eq!(config.effect_name_scope, "saved_selection");
        assert_eq!(config.active_effect.unwrap().id, 21);
        assert_eq!(config.parameter_offset, Some(94));
        assert_eq!(config.brightness, Some(9));
        assert_eq!(config.speed, None);
        assert_eq!(config.color_index, None);
        assert!(require_preview_mode(&config).is_err());
        let matrix = matrix_request();
        let plan = prepare_static(&config, &matrix, Color([255, 0, 0])).unwrap();
        assert_eq!(
            plan.configuration_diff
                .iter()
                .map(|c| c.offset)
                .collect::<Vec<_>>(),
            [10]
        );
        assert_eq!(plan.configuration[9], 1);
        assert_eq!(plan.configuration[10], 21);
        for i in 0..CONFIG_LEN {
            if i != 10 {
                assert_eq!(plan.configuration[i], reply[8 + i]);
            }
        }
        assert_eq!(plan.brightness, Some(9));
    }

    #[test]
    fn static_plan_uses_planar_storage_then_minimal_configuration_changes_never_sendable() {
        let mut reply = config_fixture(0, 1);
        reply[8 + 38] = 0x42;
        reply[8 + 39] = 0x71;
        let config = parse_config(&reply).unwrap();
        let mut matrix = matrix_request();
        matrix[8..8 + STATIC_RGB_LEN].fill(0x55);
        let plan = prepare_static(&config, &matrix, Color([137, 180, 250])).unwrap();
        assert_eq!(
            plan.brightness,
            Some(4),
            "retain saved self-defined brightness"
        );
        assert_eq!(
            plan.steps.iter().map(|s| s.command).collect::<Vec<_>>(),
            [6, 4]
        );
        assert_eq!(plan.steps[0].payload_length, 384);
        assert_eq!(plan.steps[1].payload_length, 128);
        let stored = decode_packet(&plan.steps[0]);
        assert_eq!(stored[..8], [6, 6, 0, 0, 1, 0, 0x80, 1]);
        for (channel, value) in [137, 180, 250].into_iter().enumerate() {
            assert!(
                stored[8 + channel * 126..8 + (channel + 1) * 126]
                    .iter()
                    .all(|b| *b == value)
            );
        }
        assert_eq!(stored[386..392], [0x55; 6]);
        assert_eq!(plan.matrix_diff.len(), 378);
        let settings = decode_packet(&plan.steps[1]);
        assert_eq!(settings[..8], [6, 4, 0, 0, 1, 0, 128, 0]);
        let offsets: Vec<_> = plan.configuration_diff.iter().map(|c| c.offset).collect();
        assert_eq!(offsets, [9, 10]);
        for offset in 0..128 {
            let expected = match offset {
                9 => 1,
                10 => 21,
                _ => reply[8 + offset],
            };
            assert_eq!(settings[8 + offset], expected);
        }
        assert!(plan.approval_required && plan.persistent && !plan.hardware_write_verified);
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            json["configuration_diff"][0],
            serde_json::json!({"offset": 9, "old": 0, "new": 1})
        );
        assert_eq!(json["rendering"], "onboard_self_defined");
        assert_eq!(json["steps"][0]["command"], 6);
        for packet in [&stored, &settings] {
            assert_eq!(
                validate_outgoing(packet).unwrap_err().code,
                "unsupported_report"
            );
        }
        let mut new_config = config_request();
        new_config[8..136].copy_from_slice(&settings[8..136]);
        let mut new_matrix = matrix_request();
        new_matrix[8..392].copy_from_slice(&stored[8..392]);
        let unchanged = prepare_static(
            &parse_config(&new_config).unwrap(),
            &new_matrix,
            Color([137, 180, 250]),
        )
        .unwrap();
        assert!(
            unchanged.steps.is_empty(),
            "omit identical persistent writes"
        );
        assert!(
            unchanged.overwrite_warning.is_none(),
            "no-op must not claim to overwrite RGB data"
        );
        for length in [0, 8, 391] {
            assert_eq!(
                prepare_static(&config, &matrix[..length], Color([1, 2, 3]))
                    .unwrap_err()
                    .code,
                "unexpected_matrix_reply"
            );
        }
        matrix[3] ^= 1;
        assert!(prepare_static(&config, &matrix, Color([1, 2, 3])).is_err());
        validate_outgoing(&matrix_request()).unwrap();
    }

    #[test]
    fn static_switch_preserves_self_defined_brightness_and_rejects_invalid_levels() {
        let mut reply = config_fixture(0, 1);
        reply[8 + 58] = 1; // Current Fixed On level is independent of slot 18.
        for level in [0, 9] {
            reply[8 + 94] = level;
            let config = parse_config(&reply).unwrap();
            let plan = prepare_static(&config, &matrix_request(), Color([137, 180, 250])).unwrap();
            assert_eq!(plan.brightness, Some(level));
            let settings = decode_packet(plan.steps.iter().find(|s| s.command == 4).unwrap());
            assert_eq!(settings[8 + 94], level);
            assert_eq!(settings[8 + 95], reply[8 + 95]);
            assert_eq!(
                plan.configuration_diff
                    .iter()
                    .map(|c| c.offset)
                    .collect::<Vec<_>>(),
                [9, 10]
            );
        }
        reply[8 + 94] = 10;
        let error = prepare_static(
            &parse_config(&reply).unwrap(),
            &matrix_request(),
            Color([137, 180, 250]),
        )
        .unwrap_err();
        assert_eq!(error.code, "unsupported_configuration");
    }

    #[test]
    fn fixed_read_and_strict_reply_validation() {
        let mut reply = config_request();
        assert_eq!(reply[..8], [0x06, 0x84, 0, 0, 1, 0, 0x80, 0]);
        assert!(reply[8..].iter().all(|b| *b == 0));
        assert_eq!(parse_config(&reply).unwrap_err().code, "invalid_signature");
        reply[134..136].copy_from_slice(&[0x5a, 0xa5]);
        reply[17] = 1;
        reply[18] = 0x15;
        reply[67] = 0x47;
        assert_eq!(
            parse_config(&reply[..136]).unwrap().lighting_mode,
            "per_key"
        );
        assert_eq!(parse_config(&reply).unwrap().mode_bytes, [1, 0x15, 0x47]);
        for length in [0, 9, 127, 135] {
            assert_eq!(
                parse_config(&reply[..length]).unwrap_err().code,
                "malformed_reply"
            );
        }
        for index in 0..8 {
            let mut wrong = reply;
            wrong[index] ^= 1;
            assert_eq!(parse_config(&wrong).unwrap_err().code, "unexpected_reply");
        }
        assert!(parse_config(&vec![0; 521]).is_err());
    }

    #[test]
    fn descriptors_require_vendor_scope_and_the_correct_transport() {
        let wired = [
            0x06, 0x00, 0xff, 0x09, 1, 0xa1, 1, 0x85, 6, 0x75, 8, 0x96, 7, 2, 0xb1, 2, 0xc0,
        ];
        let wireless = [
            0x06, 2, 0xff, 0x09, 2, 0xa1, 1, 0x85, 0x13, 0x75, 8, 0x95, 19, 0x81, 0, 0x91, 0, 0xc0,
        ];
        assert!(inspect_descriptor(&wired).unwrap().wired);
        assert!(!inspect_descriptor(&wired).unwrap().wireless);
        assert!(inspect_descriptor(&wireless).unwrap().wireless);
        assert!(!inspect_descriptor(&wireless).unwrap().wired);
        let mut os_owned = wired;
        os_owned[2] = 0;
        assert!(!inspect_descriptor(&os_owned).unwrap().wired);
        let mut status_trap = wired;
        status_trap[13] = 0;
        assert!(!inspect_descriptor(&status_trap).unwrap().wired);
        let mut constant_feature = wired;
        constant_feature[15] = 1;
        assert!(!inspect_descriptor(&constant_feature).unwrap().wired);
        for length in 0..wired.len() {
            assert!(!inspect_descriptor(&wired[..length]).is_some_and(|s| s.wired));
        }
        assert!(inspect_descriptor(&[0xfe, 0, 0]).is_none());
        assert!(inspect_descriptor(&[0xb4]).is_none());
    }
}
