# Native 2.4 GHz support

## Sources and variant

Investigation used these pinned primary implementations:

- [Aula-Manager protocol](https://github.com/free-soldier28/Aula-Manager/blob/b98c0afad7ae57bbd4f073230c1ee1cb87006318/docs/PROTOCOL.md)
  and [wireless service](https://github.com/free-soldier28/Aula-Manager/blob/b98c0afad7ae57bbd4f073230c1ee1cb87006318/src/Aula.Core/Services/WirelessLightingService.cs).
- [Keylux protocol](https://github.com/SibteProf/keylux/blob/575ef572b5da915aae22551551475904d6a960ec/docs/PROTOCOL.md)
  and [capture-backed framing](https://github.com/SibteProf/keylux/blob/575ef572b5da915aae22551551475904d6a960ec/crates/aula-protocol/src/dongle/protocol.rs).
- [WebHID driver](https://github.com/cloudglides/aula-f75-linux-drivers/tree/879f155336d1a0fa002a4a72b2e0332fb4b274cd),
  whose README limits radio support to volatile streaming despite settings code.
- [F75 Studio protocol](https://github.com/trenbolone1122/aula-f75-linux/blob/4f7cebe04ad6a70345e7d1df72c1ce17786c14c4/docs/PROTOCOL.md),
  already used for this repository's wired protocol notes.

The actual connected device on 2026-11-02 identified as USB `3554:fa09`,
`Compx 2.4G Wireless Receiver`. Its interface 1 descriptor has vendor page
`ff02`, report `13`, and 19 input/output bytes plus the report ID. Ordinary
keyboard and unrelated mouse interfaces were excluded. No wired F75 was
connected during this work; wired validation was the complete synthetic regression
suite. No wireless model query is established: `model: null` is intentional.
Matching a receiver ID is not proof of a unique keyboard model. Control also
requires the expected descriptor, a valid full configuration and, for persistent
changes, the F75-sized 126-entry RGB readback. Other layouts fail closed.

## Framing and implementation

Each output/input report is 20 bytes:

| Byte | Meaning |
| --- | --- |
| 0 | `13` |
| 1 | Command |
| 2 | Total fragments; high bit in a response indicates rejection |
| 3 | Zero-based fragment index |
| 4 | Subcommand in high nibble, data length in low nibble |
| 5–18 | Up to **14** data bytes |
| 19 | Sum of bytes 0–18 modulo 256 |

Aula-Manager's “15-byte payload” includes byte 4, the length byte. Its source
frames agree with the 14-data-byte format. Its table offsets become payload
58 + 2*(effect-1) after correct assembly, matching the shared decoder for native
effects 1–18. Blindly concatenating bytes 4–18 would corrupt configuration.

`44` requests configuration with `13 44 01 00 00 ... 58`. A complete reply has
nine 14-byte fragments plus the two-byte `5a a5` signature (128 bytes).
`42` reads group-0 RGB: the connected receiver returns **27 full fragments,
378 bytes**, without the wired read's six extra bytes. Reassembly checks length,
command, count, index, checksum and rejection flags, accepts reordered fragments
and identical duplicates, and rejects conflicting duplicates. Non-`13` typing,
consumer and mouse reports are skipped under an absolute two-second deadline.
Timeout/incomplete reads start at most one new request with an empty assembly;
chunks from separate attempts are not deliberately merged. There is no protocol
transaction ID, so competing controllers must remain closed.

Native operations reuse request validation, diff planning, review matching,
private snapshots and exact readbacks. A radio plan is tagged `transport:
"wireless"`, contains the actual ten `04` packets, and cannot authorize a wired
operation. Unknown bytes, including wireless payload byte 14, come from a fresh
wireless read and are preserved. No factory template is substituted.

Internally, the existing matrix planner receives six zero padding bytes to reuse
its 384-byte representation. These are never observed radio data: wireless
reviews and snapshots contain only the 378 actual bytes. No matrix writes are
allowed on this backend. Snapshot metadata identifies `3554:fa09`, wireless,
and an unknown model, rather than borrowing the wired model response.

Each configuration fragment is paced by 20 ms and needs a checksummed ACK matching
command, fragment count, index and length before the next is sent. ACK data is
not used as configuration readback. Any transfer/ACK failure stops the transaction
with `configuration_write: "uncertain"`; no configuration fragment is retried.
Complete configuration and RGB reads verify the result. A no-op still checks
readbacks but creates no snapshot and sends no configuration packets.

Aula-Manager always writes a palette template and sends `0a` save. The existing
Studio protocol supports configuration-only edits, and this receiver accepted and
read back them without either extra operation. The minimal backend therefore
sends neither: replacing an unread palette or guessing a commit is unnecessary.
Stored-byte readback does not prove persistence across power loss.

Volatile `lighting color --transport wireless` uses Keylux's captured `88`
format: `[R,G,B,count,indices...]`, with `0x10 | length` in byte 4. The complete
126-entry uniform frame occupies ten packets; it is sent once, without ACKs or
keepalives. The exact existing `[1,21]` mode is required; the command never changes
configuration. Firmware may repaint afterward. This encoding has synthetic tests;
its visible behavior was not tested here.

## Hardware validation

Read-only probing preceded all writes. The CLI then generated and checked fresh
reviews for each reversible change, saved snapshots, and verified exact readbacks:

- Fixed On brightness 5 → 4 → 5 (configuration byte 58 only).
- Fixed On palette green → cyan → green (low nibble at byte 59 only).
- Fixed On → Rainbow → Fixed On (selector/effect fields only; selector stayed 0).
- Rainbow speed 3 → 2 → 3 (high nibble at byte 63 only).
- Reapplying brightness 5: verified no-op, no snapshot or configuration write.
- Final detailed review matched **all original 128 configuration and 378 RGB bytes**.

The receiver sometimes dropped a fragment and stopped answering while the keyboard
was idle. Initial failures reported `wireless_incomplete_response` or
`wireless_timeout`; failed preflight reads sent no configuration packets. A fresh
review restored the palette after one such failure. A bounded fresh-read retry
was then added and the effect/speed/no-op sequence completed successfully. The
keyboard must be awake and in range. User confirmation established normal typing;
no visual claims are inferred from byte readback.

Development checks: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`
and `cargo build --locked --release`. All 59 tests passed. The suite covers wired regressions and wireless framing,
checksum overflow, malformed/missing/conflicting chunks, bounded fresh-read
assembly, stale transport/state reviews, snapshots preceding writes, uncertain
ACKs/transfers, no-ops, descriptor selection and CLI authorization/JSON. Automated
tests do not open hardware. Physical unplug during a write, persistent storage
across power cycles, visual rendering of every effect, and other receiver firmware
variants remain untested.

## Deliberate boundaries

Persistent uniform RGB and Self-defined selection are disabled. The sources disagree
about the six-byte per-key write trailer (zeros versus `00 00 5a a5 00 00`),
and Keylux reports shifted later parameter slots between wired/wireless blocks.
A matching 128-byte length/signature is insufficient to adopt those fields.
Keylux documents a keyboard that stopped typing after a blind wired-config upload.
Only native slots whose offsets agree with the reference and live reads are used;
Self-defined brightness and indicator mode remain unknown in wireless status.
These are unverified capabilities, not assertions that the hardware cannot do them.

Session `off`/`on` restoration remains wired-only: the receiver's USB instance can
survive keyboard reconnections or re-pairing and cannot establish the same radio
keyboard session. Wireless `lighting effect off` is available; resume with an
explicit native effect. No reset (`06`), save/profile, pairing, Bluetooth, keymap
or macro commands are exposed or sent. Existing wired behavior and packet formats
remain intact.
