# Wired protocol and troubleshooting notes

Primary reference: F75 Studio revision
[`4f7cebe04ad6a70345e7d1df72c1ce17786c14c4`](https://github.com/trenbolone1122/aula-f75-linux/tree/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4),
particularly [PROTOCOL.md](https://github.com/trenbolone1122/aula-f75-linux/blob/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4/docs/PROTOCOL.md),
[protocol.rs](https://github.com/trenbolone1122/aula-f75-linux/blob/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4/src/protocol.rs),
[device.rs](https://github.com/trenbolone1122/aula-f75-linux/blob/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4/src/device.rs),
[ui.rs](https://github.com/trenbolone1122/aula-f75-linux/blob/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4/src/ui.rs)
and [worker.rs](https://github.com/trenbolone1122/aula-f75-linux/blob/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4/src/worker.rs).

## Configuration and transport

Wired interface 1 uses 520-byte feature reports, report ID 6. The eight-byte
header includes command, subcommand and chunk/length fields; responses must match
exactly. Read configuration with `0x84` (128 bytes), model with `0x82`, subcommand
1 (expected payload `03 00 00 00 00 cd`), and stored RGB with `0x86` (384 bytes).
Transfers are paced at 50 ms without retries. Model `f75_cd` is shared, not unique.

All offsets below are zero-based configuration payload offsets (add 8 within a
report). Required signature: offsets 126/127 = `5a a5`.

| Offset | Meaning |
| --- | --- |
| 9 | Selector: Studio writes 1 for Self-defined, otherwise 0 |
| 10 | Active effect ID; authoritative independently of selector 9 |
| `58 + 2*slot` | Saved brightness, levels 0–9 |
| `59 + 2*slot` | Speed in high nibble (0–4), palette in low nibble (0–7), where applicable |
| 58/59 | Fixed On, slot 0 |
| 92/93 | Storm (18), slot 17 |
| 94/95 | Self-defined (21), slot 18 |

Slots for IDs 1–18 are 0–17; ID 21 is slot 18 and Off (0) is slot 19. IDs 1–18
in order: fixed_on, respire, rainbow, flash_away, raindrops, rainbow_wheel,
ripples, stars_twinkle, shadow_fade, retro_snake, neon_stream, reaction, sine_wave,
scanner, windmill, waterfall, blossoming, storm. Speed applies to 2–18; palette
to 1–18 except 3 and 15–17. Off has no brightness control.

Legacy `mode_bytes` are offsets **9, 10, 59**, not an active-effect parameter
triple: `[0,1,51]` means Fixed On with saved packed byte `0x33` (yellow palette;
speed nibble 3 is inapplicable). `[1,18,51]` means Storm with parameters at 92/93,
not an invalid mode. Unknown effects/parameters stay unknown; write preparation
additionally validates selector/layout and applicable saved ranges. Volatile
preview requires exact selector/effect `[1,21]`.

Stored static RGB (`0x06`, subcommand 0) is planar R[126], G[126], B[126], plus
six padding bytes. The backend preserves padding. Configuration `0x04` selects
Self-defined through 9/10 and preserves saved brightness 94, packed byte 95 and
all unrelated bytes. Studio's `set_per_key` sends matrix then configuration;
the CLI keeps that refresh for changed colors.
A same-color, same-mode verified request is a zero-write no-op. Native effect
selection and individual parameters use configuration-only read/modify/write.
Volatile `0x08` is a separate real-time frame, never a static-storage stream.

## Brightness and Storm

For Self-defined brightness, Studio's GUI queues `PerKey`: host profile colors
via `0x06`, then effect 21/brightness via `0x04`. Its CLI `effect 21` uses only
configuration. Stored RGB channels are raw, not brightness-scaled; white remains
255 at all saved levels. Screen-preview dimming and host-scaled live frames are
separate. There is no documented extra activation opcode. Self-defined dimming failed
in both the CLI and Studio. Rewriting the matrix has not been shown to fix it;
stored RGB is left unchanged when adjusting brightness.

Brightness once returned from 5 to 9 between operations; the cause is unknown.
Studio's entry into live streaming explicitly selects brightness 9; its solid/key
commands or a profile apply can also write 9. Idle polling is not an automatic
reset in the reviewed worker. These paths can change saved brightness, but there is no trace identifying
the writer in that test. Apparent restarts while adjusting Studio brightness
also remain unexplained.

For Storm, saved 92/93 = `9/0x37` means valid brightness 9, speed 3, random palette
7. Studio's effect/worker path uses configuration `0x04` only, without a special
latch or follow-up command. Its doctor visually tests Fixed On/Rainbow, not Storm;
no documented Storm-specific firmware limitation or additional activation was
established. To investigate further, compare Studio alone with the same saved parameters,
record full before/after payloads and a USB trace, and observe the LEDs. Review
and approve the writes first. Avoid profile application and broad doctor tests,
which modify other settings.

## Stored layouts and recovery

Static color replaces all 126 RGB entries in group 0. Returning to Self-defined
shows that stored layout; it does not recover the layout replaced by a static
write. Configuration selection activates the chosen effect's saved brightness,
which may differ from the previous effect's level without resetting either value.

Snapshots contain the 128-byte configuration and 384-byte group-0 matrix. They do
not contain keymaps, macros, other color groups, the separate palette table or
Studio profiles. The original per-key layout remains recoverable from its
pre-write snapshot. See the [recovery guide](session-lighting-power.md) before
planning any restoration.

Offset 11's possible color-group role and byte 26's meaning remain unverified.
Byte 26 changed after an earlier successful static-write readback while the stored
layout also changed to match a Studio profile; the responsible writer is unknown.
Preserve unknown bytes from fresh state, rather than normalizing them from an old
snapshot. Studio profiles include sleep, keymaps and macros as well as lighting,
so applying a profile is not a lighting-only restore. Other color groups are not
known backups.

## Receiver protocol boundary (reference only)

Studio's `3554:fa09` transport uses 20-byte output/input packets, report `0x13`,
usage page `0xff02`, read flag `0x40`. Configuration is `04/44`, stored RGB `02/42`,
palette table `09/49`, volatile RGB `88`. Bytes 1–4 encode command, total chunks,
chunk index and packed subcommand/length; 5–18 hold up to 14 bytes; byte 19 sums
0–18 modulo 256. Future work must filter multiplexed reports, validate checksums
and assemble complete ordered chunks. `0x07` queries connectivity, not battery;
**receiver `0x06` is reset**, not the wired RGB command. Do not reuse wired packets
or adopt Studio's persistent retries.

Supplementary references: [Keylux protocol](https://github.com/SibteProf/keylux/blob/9817718e93b5ee129f221cb6fe3f59db025aa078/docs/PROTOCOL.md)
for framing/signatures/storage versus volatile operations;
[OpenRGB controller](https://github.com/CalcProgrammer1/OpenRGB/blob/da927bbf0e19493b407afce047f2e6156df86e2c/Controllers/SinowealthController/SinowealthKeyboard10cController/SinowealthKeyboard10cController.cpp)
for model/framing cross-checks.
