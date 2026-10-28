# aulaf75ctl

Linux CLI for AULA F75 lighting over USB. Set static RGB, select onboard effects,
and adjust brightness, speed and palettes without running a background service.

Tested on an AULA F75 over USB: static RGB, Fixed On and Rainbow, Fixed On
brightness/palettes, Rainbow speed, and Off/On.

Fifteen native effects were observed in the physical knob cycle: IDs 1–8,
10–13 and 15–17. Off and Self-defined/per-key are separate modes. Shadow Fade
(9), Scanner (14) and Storm (18) are recognized and can still be requested,
but did not render correctly on the tested F75. Knob exposure does not imply
individual visual verification of every animation.

The stored native-effect ID may differ from the active lighting mode. Schema-1
JSON retains `effect_id`/`effect_name` as the stored selection and adds
`stored_native_effect_id`, `active_effect` and `effect_name_scope`; `lighting_mode`
and saved parameters describe the active mode. Unknown selectors yield an unknown
mode and null active effect/parameters. `observed_native_effects` lists the fifteen
IDs and names using the CLI's protocol mapping.

## Install

Requires Linux and Rust 1.89+:

```sh
git clone git@github.com:Vantesh/aulaf75ctl.git
cd aulaf75ctl
cargo install --path . --locked
```

Run as your normal desktop user, without sudo. If HID access is denied, see the
optional [udev rule](packaging/70-aulaf75ctl.rules) for wired interface 1. It is
not installed automatically.

## Commands

Close F75 Studio and other keyboard controllers before accessing the keyboard.

```sh
aulaf75ctl devices --json
aulaf75ctl status --transport wired --json
aulaf75ctl lighting static '#89b4fa' --dry-run --json
```

| Lighting command | Values |
| --- | --- |
| `static '#RRGGBB'` | Uniform stored RGB; selects Self-defined |
| `effect <name\|id>` | Native effects: 1–8, 10–13, 15–17; Off: 0; Self-defined: 21 |
| `brightness <0-9>` | Active effect's saved level; 0 is not Off |
| `speed <0-4>` | Saved animation speed, where available |
| `palette <name\|0-7>` | red, green, blue, yellow, magenta, cyan, white, random |
| `off` / `on` | Off / restore the previous effect in this USB session |
| `color '#RRGGBB'` | Separate volatile preview; physical behavior unverified |

Prefix each command with `aulaf75ctl lighting`. Palette availability depends on
the active effect; see [protocol notes](docs/protocol-notes.md). Use `--help` for
all options and `--version` for the installed version.

## Charging indicator

Modes: `off` (0x00), `blinking` (0x01), `slow-blinking` (0x02), and
`fixed-on` (0x03), observed through physical keyboard controls.

```sh
aulaf75ctl indicator status --transport wired --json
aulaf75ctl indicator mode slow-blinking --transport wired --dry-run --details --json > review.json
# Inspect review.json before applying:
aulaf75ctl indicator mode slow-blinking --transport wired --apply --review review.json --json
```

Indicator changes use the same fresh-state review, snapshot and readback checks
as lighting changes. Only configuration byte `0x12` changes; lighting and stored
RGB are preserved. Software-write behavior remains physically unverified.
Schema-1 configuration JSON adds `charging_indicator` with `mode` and raw `value`;
unknown values have a null mode and cannot be changed through indicator commands.

## Review and apply

**Static RGB overwrites the stored per-key color layout.** Persistent changes
require a fresh review and explicit approval. Discovery, status and dry-runs do
not change keyboard settings; `color --dry-run` needs no hardware at all.

```sh
umask 077
review_file=$(mktemp "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/aula-review.XXXXXX.json")
aulaf75ctl lighting static '#89b4fa' --transport wired --dry-run --details --json > "$review_file"
python3 -m json.tool "$review_file"
```

Check `ok`, the device/model, changed bytes and planned transfers. After approving
that exact change, apply once:

```sh
aulaf75ctl lighting static '#89b4fa' --transport wired --apply --review "$review_file" --json
```

Use a new review for every operation after state changes. Apply checks the fresh
configuration and RGB matrix against the review. No-ops skip writes and snapshots.
Changed static colors retain the matrix-then-configuration refresh.

Snapshots are saved under `$XDG_STATE_HOME/aulaf75ctl` (default
`~/.local/state/aulaf75ctl`). Results include the snapshot path and write/readback
outcomes. On failure, stop and keep
those files: there are no automatic retries or rollback. See the
[recovery guide](docs/session-lighting-power.md).

`--json` returns one schema-1 result on stdout; diagnostics go to stderr. Exit
codes are 0 for success, 1 for operation failure and 2 for invalid arguments.
Persistent dry-runs include full review data only with `--details --json`.
Readback success verifies stored bytes, not visible lighting.

## Limits

- Wired control: `258a:010c`, interface 1, with descriptor/model checks. Receiver
  `3554:fa09` detection is passive; wireless control and Bluetooth are unsupported.
- Self-defined brightness saves correctly but did not visibly dim LEDs, including
  in Studio.
- Off/On restoration needs the same USB device instance. This F75 has no USB
  serial, so reconnects and reboots invalidate it. Missing, changed or pending
  session state blocks restoration; nothing restores automatically.
- Controller detection is best-effort; protected processes and later openers may
  be missed. The lock coordinates CLI instances only.

## Development

Tests use synthetic fixtures without accessing hardware:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

## Credits

Special thanks to [F75 Studio](https://github.com/trenbolone1122/aula-f75-linux) by [trenbolone1122](https://github.com/trenbolone1122), whose excellent work on the AULA F75 protocol made this project possible.

If you prefer a full-featured graphical interface, definitely check out F75 Studio!

Additional thanks to:

- [Keylux](https://github.com/SibteProf/keylux) for additional protocol research and documentation.
- [OpenRGB](https://github.com/CalcProgrammer1/OpenRGB) for useful HID protocol references.

## License

This project is licensed under the [MIT License](LICENSE).
