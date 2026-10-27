use std::path::PathBuf;

use crate::{
    Error,
    device::Transport,
    protocol::{Brightness, Color, Effect, LightingRequest, Palette, Speed},
};

pub const HELP: &str = "aulaf75ctl — guarded wired AULA F75 lighting control (Linux)

Usage: aulaf75ctl [--json] [--transport wired|wireless] <devices|status>
       aulaf75ctl [--json] [--transport wired] lighting color '#RRGGBB' [--dry-run]
       aulaf75ctl [--json] [--transport wired] lighting static '#RRGGBB' --dry-run
       aulaf75ctl [--json] lighting static '#RRGGBB' --apply --review <dry-run.json>
       aulaf75ctl [--json] lighting effect <name|id> --dry-run [--details] --transport wired
       aulaf75ctl [--json] lighting effect <name|id> --apply --review <dry-run.json> --transport wired
       aulaf75ctl [--json] lighting brightness <0-9> --dry-run [--details] --transport wired
       aulaf75ctl [--json] lighting brightness <0-9> --apply --review <dry-run.json> --transport wired
       aulaf75ctl [--json] lighting speed <0-4> --dry-run [--details] --transport wired
       aulaf75ctl [--json] lighting speed <0-4> --apply --review <dry-run.json> --transport wired
       aulaf75ctl [--json] lighting palette <name|0-7> --dry-run [--details] --transport wired
       aulaf75ctl [--json] lighting palette <name|0-7> --apply --review <dry-run.json> --transport wired
       aulaf75ctl --help
       aulaf75ctl [--json] lighting <off|on> --dry-run [--details] --transport wired
       aulaf75ctl [--json] lighting <off|on> --apply --review <dry-run.json> --transport wired
       aulaf75ctl --version

devices   Passive USB HID discovery; sends no protocol requests
status    Validate/read wired configuration; receiver detection stays passive
--json    Emit one schema-versioned JSON object; diagnostics go to stderr
--dry-run Color preview: hardware-free packet; persistent lighting: read-only device-backed diff
--details Include full persistent lighting review data and packets (requires --json)

Color preview sends one volatile 0x08 frame only after wired config/model checks.
The preview may be transient; its physical behavior is unverified. Unknown modes are rejected.
Persistent lighting apply requires a matching review and snapshot before writes.
Wired color changes retain Studio's effect refresh; identical requests skip writes.
Default output is concise; --dry-run --details --json creates an applicable review.
Visible competing HID holders are rejected; the advisory lock is not exclusive.
Effect selection preserves saved brightness, speed, palette and per-key RGB.
Effect names/IDs: fixed-on(1), rainbow(3), rotating-storm(18), self-defined(21),
off(0), and the other complete Studio effect names (see README).
Brightness changes only the active effect's saved level (0-9), preserving RGB.
Speed changes only supported hardware effects (2-18), levels 0-4, preserving palette.
Palettes: red(0), green(1), blue(2), yellow(3), magenta(4), cyan(5), white(6), random(7).
Palette selection preserves speed and requires a palette-capable active effect.
Off selects native effect 0; On restores a verified target for the same USB instance.
Session restoration ends on USB re-enumeration or reboot; no startup restoration.
Close other keyboard-control applications before requesting status or a preview.
Run as your normal desktop user, never with sudo.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Devices,
    Status,
    Color(Color),
    Static(Color),
    Effect(Effect),
    Brightness(Brightness),
    Speed(Speed),
    Palette(Palette),
    Power(crate::power::Action),
    Apply {
        request: LightingRequest,
        review: PathBuf,
    },
    Help,
    Version,
}

pub struct Options {
    pub command: Command,
    pub transport: Option<Transport>,
    pub dry_run: bool,
}

pub fn parse(args: &[String]) -> Result<Options, Error> {
    let mut positional = Vec::new();
    let mut transport = None;
    let mut dry_run = false;
    let mut apply = false;
    let mut review = None;
    let mut details = false;
    let json = args.iter().any(|arg| arg == "--json");
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => {}
            "--details" if !details => details = true,
            "--apply" if !apply => apply = true,
            "--review" if review.is_none() => {
                review = Some(PathBuf::from(
                    args.next().filter(|p| !p.starts_with('-')).ok_or_else(|| {
                        Error::new("invalid_arguments", "Expected --review <dry-run.json>")
                    })?,
                ));
            }
            "--dry-run" if !dry_run => dry_run = true,
            "--transport" if transport.is_none() => {
                transport = Some(match args.next().map(String::as_str) {
                    Some("wired") => Transport::Wired,
                    Some("wireless") => Transport::Wireless,
                    _ => {
                        return Err(Error::new(
                            "invalid_arguments",
                            "Expected --transport wired or wireless",
                        ));
                    }
                });
            }
            "--help" | "-h" if positional.is_empty() => {
                return Ok(Options {
                    command: Command::Help,
                    transport,
                    dry_run,
                });
            }
            "--version" | "-V" if positional.is_empty() => {
                return Ok(Options {
                    command: Command::Version,
                    transport,
                    dry_run,
                });
            }
            other
                if other.starts_with('-')
                    && !matches!(
                        positional.as_slice(),
                        ["lighting", "brightness" | "speed" | "palette"]
                    ) =>
            {
                return Err(Error::new(
                    "invalid_arguments",
                    "Unknown or duplicate option; see --help",
                ));
            }
            _ => positional.push(arg.as_str()),
        }
    }
    let persistent = matches!(
        positional.as_slice(),
        [
            "lighting",
            "static" | "effect" | "brightness" | "speed" | "palette",
            _
        ] | ["lighting", "off" | "on"]
    );
    if details && (!dry_run || !json || !persistent) {
        return Err(Error::new(
            "invalid_arguments",
            "--details is only valid for persistent lighting --dry-run --json",
        ));
    }
    let command = if persistent {
        if transport == Some(Transport::Wireless) {
            return Err(Error::new(
                "unsupported_transport",
                "Persistent lighting is wired-only; no receiver was opened",
            ));
        }
        let request = match positional.as_slice() {
            ["lighting", "static", value] => LightingRequest::Static(Color::parse(value)?),
            ["lighting", "effect", value] => LightingRequest::Effect(Effect::parse(value)?),
            ["lighting", "brightness", value] => {
                LightingRequest::Brightness(Brightness::parse(value)?)
            }
            ["lighting", "speed", value] => LightingRequest::Speed(Speed::parse(value)?),
            ["lighting", "palette", value] => LightingRequest::Palette(Palette::parse(value)?),
            ["lighting", "off"] => LightingRequest::Power(crate::power::Action::Off),
            ["lighting", "on"] => LightingRequest::Power(crate::power::Action::On),
            _ => unreachable!("persistent command shape checked above"),
        };
        if apply && !dry_run {
            let review = review.take().ok_or_else(|| {
                Error::new(
                    "approval_required",
                    "--apply requires --review <dry-run.json>; no hardware was opened",
                )
            })?;
            return Ok(Options {
                command: Command::Apply { request, review },
                transport,
                dry_run: false,
            });
        }
        if !dry_run {
            return Err(Error::new(
                "approval_required",
                "Use --dry-run to review changes, or explicitly --apply --review <dry-run.json>; no hardware was opened",
            ));
        }
        match request {
            LightingRequest::Static(color) => Command::Static(color),
            LightingRequest::Effect(effect) => Command::Effect(effect),
            LightingRequest::Brightness(level) => Command::Brightness(level),
            LightingRequest::Speed(level) => Command::Speed(level),
            LightingRequest::Palette(palette) => Command::Palette(palette),
            LightingRequest::Power(action) => Command::Power(action),
        }
    } else {
        match positional.as_slice() {
            ["devices"] => Command::Devices,
            ["status"] => Command::Status,
            ["lighting", "color", rgb] => {
                if transport == Some(Transport::Wireless) {
                    return Err(Error::new(
                        "unsupported_transport",
                        "Color preview is wired-only; no receiver was opened",
                    ));
                }
                Command::Color(Color::parse(rgb)?)
            }
            [] => Command::Help,
            _ => {
                return Err(Error::new(
                    "invalid_arguments",
                    "Expected devices, status, or lighting color/static/effect/brightness/speed/palette/off/on; see --help",
                ));
            }
        }
    };
    if apply || review.is_some() {
        return Err(Error::new(
            "invalid_arguments",
            "--apply and --review are only valid together for persistent lighting, without --dry-run",
        ));
    }
    if dry_run
        && !matches!(
            command,
            Command::Color(_)
                | Command::Static(_)
                | Command::Effect(_)
                | Command::Brightness(_)
                | Command::Speed(_)
                | Command::Palette(_)
                | Command::Power(_)
        )
    {
        return Err(Error::new(
            "invalid_arguments",
            "--dry-run is only valid for lighting color, static, effect, brightness, speed, palette, off or on",
        ));
    }
    Ok(Options {
        command,
        transport,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_requires_operation_specific_wired_review_authorization() {
        for (name, action) in [
            ("off", crate::power::Action::Off),
            ("on", crate::power::Action::On),
        ] {
            let args = [
                "lighting",
                name,
                "--dry-run",
                "--details",
                "--json",
                "--transport",
                "wired",
            ]
            .map(String::from);
            assert_eq!(parse(&args).unwrap().command, Command::Power(action));
            let args = ["lighting", name, "--apply", "--review", "r.json"].map(String::from);
            assert_eq!(
                parse(&args).unwrap().command,
                Command::Apply {
                    request: LightingRequest::Power(action),
                    review: PathBuf::from("r.json")
                }
            );
            for args in [
                vec!["lighting", name],
                vec!["lighting", name, "--apply"],
                vec!["lighting", name, "--review", "r.json"],
                vec!["lighting", name, "--dry-run", "--details"],
                vec![
                    "lighting",
                    name,
                    "--dry-run",
                    "--apply",
                    "--review",
                    "r.json",
                ],
                vec!["lighting", name, "--dry-run", "--transport", "wireless"],
            ] {
                assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
            }
        }
    }

    #[test]
    fn palette_names_indices_and_approval_are_strict() {
        for (index, name) in [
            "red", "green", "blue", "yellow", "magenta", "cyan", "white", "random",
        ]
        .into_iter()
        .enumerate()
        {
            let palette = Palette::parse(name).unwrap();
            assert_eq!(palette.index, index as u8);
            assert_eq!(palette.name, name);
            assert_eq!(Palette::parse(&name.to_uppercase()).unwrap(), palette);
            assert_eq!(Palette::parse(&index.to_string()).unwrap(), palette);
            let args = [
                "lighting",
                "palette",
                name,
                "--dry-run",
                "--details",
                "--json",
            ]
            .map(String::from);
            assert_eq!(parse(&args).unwrap().command, Command::Palette(palette));
        }
        let args = [
            "lighting",
            "palette",
            "Green",
            "--apply",
            "--review",
            "r.json",
            "--transport",
            "wired",
        ]
        .map(String::from);
        assert_eq!(
            parse(&args).unwrap().command,
            Command::Apply {
                request: LightingRequest::Palette(Palette::parse("1").unwrap()),
                review: PathBuf::from("r.json")
            }
        );
        for value in [
            "purple",
            "8",
            "255",
            "256",
            "-1",
            "1.5",
            "#ff0000",
            "+1",
            "",
            "white extra",
        ] {
            let args = ["lighting", "palette", value, "--dry-run", "--json"].map(String::from);
            assert_eq!(
                parse(&args).err().unwrap().code,
                "invalid_palette",
                "{value}"
            );
        }
        for args in [
            vec!["lighting", "palette", "red"],
            vec!["lighting", "palette", "red", "--apply"],
            vec!["lighting", "palette", "red", "--review", "r.json"],
            vec![
                "lighting",
                "palette",
                "red",
                "--dry-run",
                "--apply",
                "--review",
                "r.json",
            ],
            vec![
                "lighting",
                "palette",
                "red",
                "--dry-run",
                "--transport",
                "wireless",
            ],
            vec![
                "lighting",
                "palette",
                "red",
                "--apply",
                "--review",
                "r.json",
                "--transport",
                "wireless",
            ],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn speed_is_bounded_and_reuses_wired_review_authorization() {
        for value in ["0", "4"] {
            let args = [
                "lighting",
                "speed",
                value,
                "--dry-run",
                "--details",
                "--json",
            ]
            .map(String::from);
            assert_eq!(
                parse(&args).unwrap().command,
                Command::Speed(Speed::parse(value).unwrap())
            );
        }
        let args = [
            "lighting",
            "speed",
            "2",
            "--apply",
            "--review",
            "review.json",
            "--transport",
            "wired",
        ]
        .map(String::from);
        assert_eq!(
            parse(&args).unwrap().command,
            Command::Apply {
                request: LightingRequest::Speed(Speed::parse("2").unwrap()),
                review: PathBuf::from("review.json")
            }
        );
        for value in ["-1", "5", "255", "256", "2.5", "fast", "+2", ""] {
            let args = ["lighting", "speed", value, "--dry-run", "--json"].map(String::from);
            assert_eq!(parse(&args).err().unwrap().code, "invalid_speed", "{value}");
        }
        for args in [
            vec!["lighting", "speed", "2"],
            vec!["lighting", "speed", "2", "--apply"],
            vec!["lighting", "speed", "2", "--review", "r.json"],
            vec![
                "lighting",
                "speed",
                "2",
                "--dry-run",
                "--apply",
                "--review",
                "r.json",
            ],
            vec![
                "lighting",
                "speed",
                "2",
                "--dry-run",
                "--transport",
                "wireless",
            ],
            vec![
                "lighting",
                "speed",
                "2",
                "--apply",
                "--review",
                "r.json",
                "--transport",
                "wireless",
            ],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn brightness_is_bounded_and_requires_wired_review_authorization() {
        for value in ["0", "9"] {
            let args = [
                "lighting",
                "brightness",
                value,
                "--dry-run",
                "--details",
                "--transport",
                "wired",
                "--json",
            ]
            .map(String::from);
            assert_eq!(
                parse(&args).unwrap().command,
                Command::Brightness(Brightness::parse(value).unwrap())
            );
        }
        let args = [
            "lighting",
            "brightness",
            "5",
            "--apply",
            "--review",
            "review.json",
            "--transport",
            "wired",
        ]
        .map(String::from);
        assert_eq!(
            parse(&args).unwrap().command,
            Command::Apply {
                request: LightingRequest::Brightness(Brightness::parse("5").unwrap()),
                review: PathBuf::from("review.json")
            }
        );
        for value in ["-1", "10", "256", "1.5", "five", "+5", ""] {
            let args = ["lighting", "brightness", value, "--dry-run", "--json"].map(String::from);
            assert_eq!(
                parse(&args).err().unwrap().code,
                "invalid_brightness",
                "{value}"
            );
        }
        for args in [
            vec!["lighting", "brightness", "5"],
            vec!["lighting", "brightness", "5", "--apply"],
            vec!["lighting", "brightness", "5", "--review", "r.json"],
            vec![
                "lighting",
                "brightness",
                "5",
                "--dry-run",
                "--apply",
                "--review",
                "r.json",
            ],
            vec![
                "lighting",
                "brightness",
                "5",
                "--dry-run",
                "--transport",
                "wireless",
            ],
            vec![
                "lighting",
                "brightness",
                "5",
                "--apply",
                "--review",
                "r.json",
                "--transport",
                "wireless",
            ],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn effect_selection_reuses_explicit_review_authorization_and_rejects_unknown_names() {
        let args = [
            "lighting",
            "effect",
            "rainbow",
            "--dry-run",
            "--details",
            "--transport",
            "wired",
            "--json",
        ]
        .map(String::from);
        assert_eq!(
            parse(&args).unwrap().command,
            Command::Effect(Effect::from_id(3).unwrap())
        );
        let args = [
            "lighting",
            "effect",
            "18",
            "--apply",
            "--review",
            "review.json",
        ]
        .map(String::from);
        assert_eq!(
            parse(&args).unwrap().command,
            Command::Apply {
                request: LightingRequest::Effect(Effect::from_id(18).unwrap()),
                review: PathBuf::from("review.json")
            }
        );
        for args in [
            vec!["lighting", "effect", "rainbow"],
            vec!["lighting", "effect", "rainbow", "--apply"],
            vec![
                "lighting",
                "effect",
                "rainbow",
                "--dry-run",
                "--apply",
                "--review",
                "r.json",
            ],
            vec![
                "lighting",
                "effect",
                "rainbow",
                "--dry-run",
                "--transport",
                "wireless",
            ],
            vec!["lighting", "effect", "rain", "--dry-run"],
            vec!["lighting", "effect", "19", "--dry-run"],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn persistent_authorization_is_specific_to_static_and_incompatible_with_dry_run() {
        let valid = [
            "lighting",
            "static",
            "#ff0000",
            "--apply",
            "--review",
            "review.json",
        ]
        .map(String::from);
        let options = parse(&valid).unwrap();
        assert_eq!(
            options.command,
            Command::Apply {
                request: LightingRequest::Static(Color([255, 0, 0])),
                review: PathBuf::from("review.json")
            }
        );
        for args in [
            vec!["status", "--details", "--json"],
            vec!["lighting", "static", "#ff0000", "--details", "--json"],
            vec!["lighting", "static", "#ff0000", "--dry-run", "--details"],
            vec!["devices", "--apply", "--review", "r.json"],
            vec!["status", "--apply", "--review", "r.json"],
            vec![
                "lighting", "color", "#89b4fa", "--apply", "--review", "r.json",
            ],
            vec!["lighting", "static", "#89b4fa", "--apply"],
            vec!["lighting", "static", "#89b4fa", "--review", "r.json"],
            vec![
                "lighting",
                "static",
                "#89b4fa",
                "--apply",
                "--review",
                "r.json",
                "--dry-run",
            ],
            vec![
                "lighting",
                "static",
                "#89b4fa",
                "--apply",
                "--review",
                "r.json",
                "--transport",
                "wireless",
            ],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        let detailed = [
            "lighting",
            "static",
            "#ff0000",
            "--dry-run",
            "--details",
            "--json",
        ]
        .map(String::from);
        assert_eq!(
            parse(&detailed).unwrap().command,
            Command::Static(Color([255, 0, 0]))
        );
    }
}
