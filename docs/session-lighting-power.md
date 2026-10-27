# Session lighting power: identity and recovery

On is host-side restoration of a previous effect ID, not a firmware power opcode.
Off selects native effect 0 through configuration `0x04`. Both preserve stored
RGB, saved effect parameters and unrelated current configuration.

## Instance binding and storage

Linux [`hid_add_device`](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-core.c)
assigns an incrementing HID device ID. The CLI binds this ID to the boot UUID,
USB bus/address and sysfs USB object's inode/change time. It checks the tuple
against `/sys/dev/char/<opened-fd-device>/device` during preparation/apply and
before the write. A stable port or shared model is insufficient; HID addition ID
accounts for re-enumeration, and boot/sysfs fields constrain counter/object reuse.
Driver rebinding or any changed binding metadata also invalidates restoration.

State files:

- Lock: `$XDG_RUNTIME_DIR/aulaf75ctl.lock`.
- Session record: `$XDG_RUNTIME_DIR/aulaf75ctl-power.json`.
- Snapshots: `$XDG_STATE_HOME/aulaf75ctl` (default `~/.local/state/aulaf75ctl`).

`XDG_RUNTIME_DIR` falls back to `/run/user/<uid>`. State outside these locations
is not imported or used for restoration. The private session record
stores schema, model/interface, instance, previous effect, transaction phase and
snapshot references; it does not duplicate configuration/matrix images or store
private topology paths. Runtime ownership/mode 0700 and regular private files are
checked; symlinks, oversized reads and unsafe permissions are rejected. Updates
use non-overwriting mode-0600 temporary files, fsync, atomic rename and directory
sync. The process lock covers reads, review matching and the whole transaction.

On compares the referenced snapshot's expected Off image against **all** current
128 configuration and 384 matrix bytes, including padding. It then edits only
current mode selectors; historical configuration is never sent wholesale.

## Interrupted operations

| Point | Durable evidence / outcome |
| --- | --- |
| Before snapshot | No hardware change; prior record retained |
| Snapshot saved, before pending metadata | No write sent; an orphan snapshot can remain |
| Pending record saved | `off_pending` / `on_pending` with snapshot reference |
| Uncertain write or failed/mismatched readback | Stay pending; no retry or rollback |
| Both exact readbacks pass | Commit `off_verified` / `on_verified` atomically |
| Metadata commit fails after readbacks | Report failure, transfer/readback results and snapshot; record may be pending or committed |

Pending blocks both Off and On even if bytes resemble an endpoint. Do not change
its phase by hand or infer completion from LEDs. Repeated verified Off preserves
the original target; repeated On while active is a no-op. A consumed record cannot
authorize later unrelated Off state. Changed state after a verified Off is a
conflict, not permission to replace it with an old image.

Keep the review, result and snapshot after a failed or interrupted operation.
Do not edit a pending record to make On/Off proceed. If you need a particular
effect, prepare a fresh `aulaf75ctl lighting effect <name>` review against current
state and approve it separately; effect selection does not use the session record.
Leave old metadata intact until recovery is understood. There is no force or
state-repair command.

## Restoring saved RGB or configuration

There is no snapshot-import command. For a manual recovery, identify the intended
layout/effect, validate the snapshot and connected device, and compare all current
configuration/matrix bytes. Review a minimal diff and save a new snapshot before
any separately approved write. Do not overwrite unrelated current settings or
unknown bytes with historical values. A snapshot cannot restore regions it never
captured. See [protocol notes](protocol-notes.md) for those boundaries and the
[README](../README.md) for the normal review/apply workflow.
