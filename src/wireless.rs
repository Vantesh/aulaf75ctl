//! Receiver protocol. Never send wired reports or factory configuration here.
use std::time::{Duration, Instant};

use crate::{Error, protocol};

pub const REPORT_LEN: usize = 20;
const CHUNK: usize = 14;
const TIMEOUT: Duration = Duration::from_secs(2);
pub type Packet = [u8; REPORT_LEN];

// The I/O seam also makes truncated reads, missing chunks and stale ACKs testable.
pub trait Io {
    fn send(&mut self, packet: &Packet) -> Result<(), Error>;
    fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>, Error>;
}

fn checksum(packet: &[u8]) -> u8 {
    packet[..19].iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
}

fn frame(command: u8, total: u8, index: u8, flags: u8, data: &[u8]) -> Packet {
    assert!(data.len() <= CHUNK);
    let mut packet = [0; REPORT_LEN];
    packet[..5].copy_from_slice(&[0x13, command, total, index, flags | data.len() as u8]);
    packet[5..5 + data.len()].copy_from_slice(data);
    packet[19] = checksum(&packet);
    packet
}

// Fixed read allowlist: 0x06 on this transport is RESET, never a matrix read.
#[derive(Clone, Copy)]
pub enum Read {
    Configuration,
    Matrix,
}

impl Read {
    fn command(self) -> u8 {
        match self {
            Self::Configuration => 0x44,
            Self::Matrix => 0x42,
        }
    }
    fn length(self) -> usize {
        match self {
            Self::Configuration => protocol::CONFIG_LEN,
            // This receiver returns the 126 RGB entries, without a trailer.
            Self::Matrix => protocol::LIVE_RGB_LEN,
        }
    }
}

fn receive(io: &mut impl Io, deadline: Instant) -> Result<Vec<u8>, Error> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::new(
                "wireless_timeout",
                "Receiver response timed out; wake the keyboard and check its 2.4 GHz connection",
            ));
        }
        let packet = io.receive(left)?;
        if packet.is_empty() {
            return Err(Error::new(
                "wireless_timeout",
                "Receiver response timed out; wake the keyboard and check its 2.4 GHz connection",
            ));
        }
        // The same interface multiplexes typing, mouse and consumer reports.
        if packet[0] != 0x13 {
            continue;
        }
        if packet.len() != REPORT_LEN {
            return Err(Error::new(
                "wireless_frame_length",
                "Receiver returned an incomplete or oversized report",
            ));
        }
        if packet[19] != checksum(&packet) {
            return Err(Error::new(
                "wireless_checksum",
                "Receiver response checksum is invalid",
            ));
        }
        return Ok(packet);
    }
}

pub fn read(io: &mut impl Io, kind: Read) -> Result<Vec<u8>, Error> {
    match read_once(io, kind) {
        Err(first)
            if matches!(
                first.code,
                "wireless_timeout" | "wireless_incomplete_response"
            ) =>
        {
            // A read request is safe to repeat. Never merge chunks across
            // attempts: doing so could manufacture a mixed configuration.
            std::thread::sleep(Duration::from_millis(50));
            read_once(io, kind).map_err(|last| {
                if last.code == "wireless_timeout" {
                    first
                } else {
                    last
                }
            })
        }
        result => result,
    }
}

fn read_once(io: &mut impl Io, kind: Read) -> Result<Vec<u8>, Error> {
    let command = kind.command();
    let length = kind.length();
    let total = length.div_ceil(CHUNK);
    io.send(&frame(command, 1, 0, 0, &[]))?;
    let deadline = Instant::now() + TIMEOUT;
    let mut data = vec![0; length];
    let mut seen = vec![false; total];
    while seen.iter().any(|got| !got) {
        let packet = receive(io, deadline).map_err(|e| {
            if e.code == "wireless_timeout" && seen.iter().any(|got| *got) {
                Error::new(
                    "wireless_incomplete_response",
                    "Receiver returned only part of the requested data; no partial state was used",
                )
            } else {
                e
            }
        })?;
        if packet[1] & 0x7f != command {
            continue;
        }
        if packet[1] & 0x80 != 0 || packet[2] & 0x80 != 0 {
            return Err(Error::new(
                "wireless_rejected",
                "Receiver rejected the read request",
            ));
        }
        let index = usize::from(packet[3]);
        if usize::from(packet[2]) != total || index >= total {
            return Err(Error::new(
                "wireless_chunk",
                "Receiver response has an unexpected fragment count or index",
            ));
        }
        let start = index * CHUNK;
        let count = CHUNK.min(length - start);
        if packet[4] != count as u8 {
            return Err(Error::new(
                "wireless_chunk",
                "Receiver response has an unexpected fragment length or subcommand",
            ));
        }
        if seen[index] && data[start..start + count] != packet[5..5 + count] {
            return Err(Error::new(
                "wireless_conflicting_fragment",
                "Receiver returned conflicting copies of a fragment",
            ));
        }
        data[start..start + count].copy_from_slice(&packet[5..5 + count]);
        seen[index] = true;
    }
    if matches!(kind, Read::Configuration) {
        protocol::parse_config_payload(&data)?;
    }
    Ok(data)
}

/// Adapt validated payloads to the existing planner/executor's internal read
/// representation. These feature-report headers are NEVER sent to the radio.
pub fn read_report(io: &mut impl Io, kind: crate::device::ReadReport) -> Result<Vec<u8>, Error> {
    use crate::device::ReadReport;
    let (header, data) = match kind {
        ReadReport::Configuration => (protocol::config_request(), read(io, Read::Configuration)?),
        ReadReport::StaticMatrix => (protocol::matrix_request(), read(io, Read::Matrix)?),
        ReadReport::Model => {
            return Err(Error::new(
                "wireless_model_unavailable",
                "No verified wireless model-query command is known",
            ));
        }
    };
    let mut reply = header.to_vec();
    reply[8..8 + data.len()].copy_from_slice(&data);
    Ok(reply)
}

pub fn parse_config(bytes: &[u8]) -> Result<protocol::Configuration, Error> {
    let mut config = protocol::parse_config_payload(bytes)?;
    // The receiver's later parameter slots and indicator mapping have not
    // been established. Expose raw bytes, without borrowing wired semantics.
    config.charging_indicator.mode = None;
    if config.active_effect.is_some_and(|e| e.id == 21) {
        config.parameter_offset = None;
        config.brightness = None;
        config.speed_raw = None;
        config.color_index_raw = None;
    }
    Ok(config)
}

pub fn require_supported(request: protocol::LightingRequest) -> Result<(), Error> {
    if request.power().is_some()
        || request.indicator().is_some()
        || request.rgb().is_some()
        || request.effect().is_some_and(|e| e.id == 21)
    {
        return Err(Error::new(
            "unsupported_wireless_operation",
            "Wireless persistent control currently supports native effects (0-18), brightness, speed and palette. Stored per-key writes/selection and indicator layout are unverified; session On/Off cannot bind the keyboard behind a receiver. Use wired for these operations",
        ));
    }
    Ok(())
}

pub fn prepare(
    request: protocol::LightingRequest,
    config: &protocol::Configuration,
    matrix: &[u8],
) -> Result<protocol::LightingPlan, Error> {
    require_supported(request)?;
    if request.brightness().is_some() && config.active_effect.is_some_and(|e| e.id == 21) {
        return Err(Error::new(
            "unsupported_brightness",
            "Wireless Self-defined parameter offsets are unverified; no write was planned",
        ));
    }
    let mut plan = request.prepare(config, matrix)?;
    plan.transport = Some(crate::device::Transport::Wireless);
    // The six internal padding bytes exist only to reuse the wired planner.
    // They are not observed receiver data and are never transmitted.
    plan.review_state
        .matrix_hex
        .truncate(protocol::LIVE_RGB_LEN * 2);
    plan.steps = if plan.no_op {
        Vec::new()
    } else {
        packets(0x04, &plan.configuration)
            .iter()
            .map(|p| protocol::PlannedPacket {
                command: 4,
                payload_length: usize::from(p[4]),
                packet_hex: hex(p),
            })
            .collect()
    };
    Ok(plan)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn packets(command: u8, data: &[u8]) -> Vec<Packet> {
    let total = data.len().div_ceil(CHUNK) as u8;
    data.chunks(CHUNK)
        .enumerate()
        .map(|(i, data)| frame(command, total, i as u8, 0, data))
        .collect()
}

pub fn write_step(
    io: &mut impl Io,
    plan: &protocol::LightingPlan,
    step: protocol::WriteStep,
) -> Result<(), Error> {
    if plan.transport != Some(crate::device::Transport::Wireless)
        || step != protocol::WriteStep::Configuration
        || !plan.matrix_diff.is_empty()
    {
        return Err(Error::new(
            "unsupported_report",
            "Only reviewed wireless configuration changes may be sent",
        ));
    }
    for packet in packets(0x04, &plan.configuration) {
        io.send(&packet)?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let ack = receive(io, deadline)?;
            if ack[1] & 0x7f != packet[1] {
                continue;
            }
            if ack[1] & 0x80 != 0 || ack[2] & 0x80 != 0 {
                return Err(Error::new(
                    "wireless_rejected",
                    "Receiver rejected a configuration fragment; transfer state is uncertain. No retry or rollback was attempted",
                ));
            }
            if ack[2] != packet[2] || ack[3] != packet[3] || ack[4] != packet[4] {
                return Err(Error::new(
                    "wireless_ack_mismatch",
                    "Receiver acknowledgement does not match the current fragment; stopped without retry or rollback",
                ));
            }
            break;
        }
    }
    Ok(())
}

/// Capture-backed 0x88 format: RGB, count, indices. One complete uniform frame;
/// no keepalive or animation loop. No firmware/configuration writes are needed.
pub fn preview_packets(color: protocol::Color) -> Vec<Packet> {
    let mut data = Vec::with_capacity(130);
    data.extend_from_slice(&color.0);
    data.push(126);
    data.extend(0..126);
    let total = data.len().div_ceil(CHUNK) as u8;
    data.chunks(CHUNK)
        .enumerate()
        .map(|(index, data)| frame(0x88, total, index as u8, 0x10, data))
        .collect()
}

pub fn preview(io: &mut impl Io, color: protocol::Color) -> Result<protocol::Configuration, Error> {
    let config = parse_config(&read(io, Read::Configuration)?)?;
    protocol::require_preview_mode(&config)?;
    for packet in preview_packets(color) {
        io.send(&packet).map_err(|_| Error::new("lighting_transfer_uncertain", "Wireless color transfer failed or was incomplete; its visible effect is unknown. No retry was attempted"))?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};

    // Read from the connected Compx 3554:fa09 F75 on 2026-11-02.
    fn config() -> Vec<u8> {
        let hex = "0003030200000404070001200100010000000000020100ff0200010001000301000000000000000000000000000000000000000000000000ffff05310721093709310930093109300937093709370937093209370937093709170937093109000931074707470744074407440744074407440744040104040404040404045aa5";
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[derive(Default)]
    struct Fake {
        replies: VecDeque<Vec<u8>>,
        sent: Vec<Packet>,
        send_failure: Option<usize>,
        disconnected: bool,
    }
    impl Io for Fake {
        fn send(&mut self, p: &Packet) -> Result<(), Error> {
            self.sent.push(*p);
            if self.send_failure == Some(self.sent.len()) {
                return Err(Error::new("disconnected", "Synthetic disconnect"));
            }
            Ok(())
        }
        fn receive(&mut self, _: Duration) -> Result<Vec<u8>, Error> {
            if self.disconnected {
                return Err(Error::new("disconnected", "Synthetic disconnect"));
            }
            Ok(self.replies.pop_front().unwrap_or_default())
        }
    }
    fn responses(command: u8, bytes: &[u8]) -> VecDeque<Vec<u8>> {
        packets(command, bytes)
            .into_iter()
            .map(|p| p.to_vec())
            .collect()
    }
    fn fake() -> Fake {
        Fake {
            replies: responses(0x44, &config()),
            ..Fake::default()
        }
    }
    fn fix_checksum(p: &mut [u8]) {
        p[19] = checksum(p);
    }
    fn plan() -> protocol::LightingPlan {
        let config = parse_config(&config()).unwrap();
        let matrix = protocol::matrix_request();
        prepare(
            protocol::LightingRequest::Brightness(protocol::Brightness::parse("4").unwrap()),
            &config,
            &matrix,
        )
        .unwrap()
    }

    #[test]
    fn captured_configuration_and_framing_match_the_shared_native_fields() {
        let mut io = fake();
        let raw = read(&mut io, Read::Configuration).unwrap();
        assert_eq!(io.sent, [frame(0x44, 1, 0, 0, &[])]);
        assert_eq!(io.sent[0][19], 0x58);
        let c = parse_config(&raw).unwrap();
        assert_eq!(c.active_effect.unwrap().id, 1);
        assert_eq!((c.brightness, c.color_index), (Some(5), Some(1)));
        assert!(c.charging_indicator.mode.is_none());
        let plan = plan();
        assert_eq!(plan.configuration_diff.len(), 1);
        assert_eq!(plan.configuration_diff[0].offset, 58);
        assert_eq!(
            plan.configuration[14], 1,
            "wireless-only baseline flag is preserved"
        );
        assert_eq!(plan.steps.len(), 10);
        assert_eq!(&plan.steps[0].packet_hex[..10], "13040a000e");
        assert_eq!(&plan.steps[9].packet_hex[..14], "13040a09025aa5");
        assert_eq!(plan.review_state.matrix_hex.len(), 378 * 2);
    }

    #[test]
    fn fragmented_reads_handle_multiplexed_reports_reordering_and_identical_duplicates() {
        let mut io = fake();
        io.replies.make_contiguous().reverse();
        io.replies.push_front(io.replies.front().unwrap().clone());
        io.replies.push_front(vec![4, 0, 0]); // typing
        io.replies.push_front(frame(7, 1, 0, 0, &[1]).to_vec()); // unrelated vendor event
        assert_eq!(read(&mut io, Read::Configuration).unwrap(), config());
        assert_eq!(io.sent.len(), 1);
        let mut io = Fake {
            replies: responses(0x42, &[12; 378]),
            ..Fake::default()
        };
        let reply = read_report(&mut io, crate::device::ReadReport::StaticMatrix).unwrap();
        let matrix = protocol::parse_matrix(&reply).unwrap();
        assert_eq!(&matrix[..378], &[12; 378]);
        assert_eq!(
            &matrix[378..],
            &[0; 6],
            "internal padding is never claimed as observed radio data"
        );
        assert_eq!(io.sent[0][1], 0x42);
    }

    #[test]
    fn invalid_incomplete_and_conflicting_reads_fail_closed() {
        for (case, code) in [
            (0, "wireless_frame_length"),
            (1, "wireless_checksum"),
            (2, "wireless_chunk"),
            (3, "wireless_chunk"),
            (4, "wireless_chunk"),
            (5, "wireless_rejected"),
        ] {
            let mut io = fake();
            let p = io.replies.front_mut().unwrap();
            match case {
                0 => {
                    p.pop();
                }
                1 => p[19] ^= 1,
                2 => {
                    p[2] = 9;
                    fix_checksum(p);
                }
                3 => {
                    p[3] = 10;
                    fix_checksum(p);
                }
                4 => {
                    p[4] = 0x1e;
                    fix_checksum(p);
                }
                5 => {
                    p[2] |= 0x80;
                    fix_checksum(p);
                }
                _ => unreachable!(),
            }
            assert_eq!(read(&mut io, Read::Configuration).unwrap_err().code, code);
            assert_eq!(io.sent.len(), 1);
        }
        let mut io = fake();
        let mut conflict = io.replies[0].clone();
        conflict[5] ^= 1;
        fix_checksum(&mut conflict);
        io.replies.insert(1, conflict);
        assert_eq!(
            read(&mut io, Read::Configuration).unwrap_err().code,
            "wireless_conflicting_fragment"
        );
        let mut io = fake();
        io.replies.pop_back();
        assert_eq!(
            read(&mut io, Read::Configuration).unwrap_err().code,
            "wireless_incomplete_response"
        );
        let mut io = Fake::default();
        assert_eq!(
            read(&mut io, Read::Configuration).unwrap_err().code,
            "wireless_timeout"
        );
        let mut io = Fake {
            disconnected: true,
            ..fake()
        };
        assert_eq!(
            read(&mut io, Read::Configuration).unwrap_err().code,
            "disconnected"
        );
        let mut bytes = config();
        bytes[127] = 0;
        let mut io = Fake {
            replies: responses(0x44, &bytes),
            ..Fake::default()
        };
        assert_eq!(
            read(&mut io, Read::Configuration).unwrap_err().code,
            "invalid_signature"
        );
    }

    #[test]
    fn read_retry_discards_partial_state_and_uses_one_complete_new_response() {
        let mut io = fake();
        io.replies.pop_back();
        io.replies.push_back(Vec::new());
        let mut newer = config();
        newer[14] = 7;
        io.replies.extend(responses(0x44, &newer));
        assert_eq!(read(&mut io, Read::Configuration).unwrap(), newer);
        assert_eq!(io.sent.len(), 2);
        let mut io = Fake::default();
        assert_eq!(
            read(&mut io, Read::Matrix).unwrap_err().code,
            "wireless_timeout"
        );
        assert_eq!(io.sent.len(), 2, "read retries are bounded");
    }

    #[test]
    fn volatile_color_is_a_complete_capture_format_frame_without_config_writes_or_retries() {
        let color = protocol::Color([0, 255, 255]);
        let packets = preview_packets(color);
        let mut payload = Vec::new();
        for (index, p) in packets.iter().enumerate() {
            assert_eq!(p[0..4], [0x13, 0x88, 10, index as u8]);
            assert_eq!(p[4] >> 4, 1);
            assert_eq!(p[19], checksum(p));
            payload.extend_from_slice(&p[5..5 + usize::from(p[4] & 15)]);
        }
        assert_eq!(&payload[..4], &[0, 255, 255, 126]);
        assert_eq!(payload[4..], (0..126).collect::<Vec<u8>>());
        let mut c = config();
        c[9] = 1;
        c[10] = 21;
        let mut io = Fake {
            replies: responses(0x44, &c),
            ..Fake::default()
        };
        preview(&mut io, color).unwrap();
        assert_eq!(io.sent.len(), 11);
        assert_eq!(&io.sent[1..], packets);
        let mut io = Fake {
            replies: responses(0x44, &c),
            send_failure: Some(3),
            ..Fake::default()
        };
        assert_eq!(
            preview(&mut io, color).unwrap_err().code,
            "lighting_transfer_uncertain"
        );
        assert_eq!(io.sent.len(), 3);
        let mut io = fake();
        assert_eq!(
            preview(&mut io, color).unwrap_err().code,
            "unsupported_lighting_mode"
        );
        assert_eq!(io.sent.len(), 1);
    }

    #[test]
    fn wireless_noops_skip_snapshots_and_all_writes() {
        let request =
            protocol::LightingRequest::Brightness(protocol::Brightness::parse("5").unwrap());
        let plan = prepare(
            request,
            &parse_config(&config()).unwrap(),
            &protocol::matrix_request(),
        )
        .unwrap();
        assert!(plan.no_op && plan.steps.is_empty());
        assert!(
            plan.write_packet(protocol::WriteStep::Configuration)
                .is_err()
        );
        let review = serde_json::json!({"schema_version":1,"command":"lighting_brightness","ok":true,"error":null,"brightness_plan":plan});
        let mut io = fake();
        io.replies.extend(responses(0x42, &[0; 378]));
        io.replies.extend(responses(0x42, &[0; 378]));
        io.replies.extend(responses(0x44, &config()));
        let result = crate::static_write::execute(
            request,
            &review,
            |kind| read_report(&mut io, kind),
            |_, _| panic!("no-op must not write"),
            |_| panic!("no-op must not snapshot"),
            || panic!("no-op must not enter write access validation"),
            crate::static_write::Target::Wireless,
        );
        assert!(result.error.is_none() && result.no_op);
        assert!(result.snapshot_path.is_none());
        assert_eq!(result.configuration_write, "skipped_unchanged");
        assert_eq!(result.configuration_readback, "verified");
        assert!(io.sent.iter().all(|p| matches!(p[1], 0x42 | 0x44)));
    }

    #[test]
    fn writes_require_matching_acks_and_stop_at_first_uncertain_fragment() {
        let plan = plan();
        let frames = packets(4, &plan.configuration);
        for failure in [
            "", "timeout", "index", "count", "length", "checksum", "reject", "send",
        ] {
            let mut io = Fake {
                replies: frames.iter().map(|p| p.to_vec()).collect(),
                ..Fake::default()
            };
            let p = &mut io.replies[1];
            match failure {
                "timeout" => io.replies.truncate(1),
                "index" => {
                    p[3] = 0;
                    fix_checksum(p);
                }
                "count" => {
                    p[2] = 1;
                    fix_checksum(p);
                }
                "length" => {
                    p[4] = 0;
                    fix_checksum(p);
                }
                "checksum" => p[19] ^= 1,
                "reject" => {
                    p[2] |= 0x80;
                    fix_checksum(p);
                }
                "send" => io.send_failure = Some(2),
                _ => {}
            }
            let result = write_step(&mut io, &plan, protocol::WriteStep::Configuration);
            assert_eq!(result.is_ok(), failure.is_empty(), "{failure}");
            assert_eq!(
                io.sent.len(),
                if failure.is_empty() { 10 } else { 2 },
                "{failure}"
            );
            assert!(io.sent.iter().all(|p| p[1] == 4));
        }
        assert!(write_step(&mut Fake::default(), &plan, protocol::WriteStep::Matrix).is_err());
    }

    #[test]
    fn reviewed_wireless_execution_reuses_snapshots_exact_readbacks_and_stale_state_guards() {
        let plan = plan();
        let request =
            protocol::LightingRequest::Brightness(protocol::Brightness::parse("4").unwrap());
        for failure in ["", "stale", "wired_review", "snapshot", "ack", "readback"] {
            let mut original = config();
            if failure == "stale" {
                original[14] ^= 1;
            }
            let mut io = Fake {
                replies: responses(0x44, &original),
                ..Fake::default()
            };
            io.replies.extend(responses(0x42, &[0; 378]));
            let frames = packets(4, &plan.configuration);
            io.replies.extend(frames.iter().map(|p| p.to_vec()));
            if failure == "ack" {
                io.send_failure = Some(4);
            }
            io.replies.extend(responses(0x42, &[0; 378]));
            let mut after = plan.configuration;
            if failure == "readback" {
                after[14] ^= 1;
            }
            io.replies.extend(responses(0x44, &after));
            let io = RefCell::new(io);
            let mut review = serde_json::json!({"schema_version":1,"ok":true,"error":null,"command":"lighting_brightness","brightness_plan":plan});
            if failure == "wired_review" {
                review["brightness_plan"]
                    .as_object_mut()
                    .unwrap()
                    .remove("transport");
            }
            let saved = std::cell::Cell::new(false);
            let result = crate::static_write::execute(
                request,
                &review,
                |kind| read_report(&mut *io.borrow_mut(), kind),
                |plan, step| {
                    assert!(saved.get());
                    write_step(&mut *io.borrow_mut(), plan, step)
                },
                |_| {
                    if failure == "snapshot" {
                        return Err(Error::new("snapshot_failed", "Synthetic snapshot failure"));
                    }
                    saved.set(true);
                    Ok("/synthetic/snapshot.json".into())
                },
                || Ok(()),
                crate::static_write::Target::Wireless,
            );
            assert_eq!(result.error.is_none(), failure.is_empty(), "{failure}");
            assert!(result.model.is_none());
            assert!(!result.hardware_verified && !result.visual_verified);
            let writes = io.borrow().sent.iter().filter(|p| p[1] == 4).count();
            if matches!(failure, "stale" | "wired_review" | "snapshot") {
                assert_eq!(writes, 0);
            } else if failure == "ack" {
                assert_eq!(writes, 2);
                assert_eq!(result.configuration_write, "uncertain");
            } else {
                assert_eq!(writes, 10);
                assert_eq!(result.matrix_readback, "verified");
            }
        }
    }
}
