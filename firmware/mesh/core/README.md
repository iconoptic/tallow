# Mesh core

Routing and frames, with no radio and no OS. The firmware in `../node` supplies the clock and the bytes. `tests/sim.rs` runs the mesh on a pretend radio. `tests/golden.rs` locks one sealed frame of each kind.

The header is in `src/wire.rs`. Version is `0xA1`.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | version |
| 1 | 1 | kind |
| 2 | 1 | ttl, changed by forwarders, not authenticated |
| 3 | 1 | hops, changed by forwarders, not authenticated |
| 4 | 4 | origin node id, little-endian |
| 8 | 4 | origin boot id, little-endian |
| 12 | 4 | origin sequence, little-endian |
| 16 |  | ciphertext |
| last 16 | 16 | Poly1305 tag |

The nonce is origin, boot, and sequence. Associated data is version, kind, origin, boot, and sequence.

## Beacon, kind 1, 11 bytes

Built in `src/mesh.rs`. A short payload is rejected.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | rank. 0 is the root. 255 means no route |
| 1 | 4 | parent node id, little-endian. 0 if this node has none |
| 5 | 2 | beacon number, little-endian, wrapping |
| 7 | 4 | sent as zero, not read |

## Report, kind 2

Sighting records from `src/sightings.rs`, 10 bytes each, as many as fit in one frame.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 6 | address |
| 6 | 1 | kind: 0 BLE public, 1 BLE random, 2 Classic |
| 7 | 1 | best RSSI, as `i8` |
| 8 | 1 | how many times it was heard, saturating at 255 |
| 9 | 1 | seconds since the last hear, saturating at 255 |

## Name, kind 3

Name records from `src/names.rs`. Each record is 13 bytes plus the name. The name is at most 32 bytes.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 6 | address |
| 6 | 1 | kind, same codes as a report |
| 7 | 2 | company id, little-endian. `0xFFFF` when unknown |
| 9 | 3 | class of device. Zero when unknown |
| 12 | 1 | name length |
| 13 | length | name bytes |

## Stats, kind 4, 88 bytes

Filled in `node/src/main.rs`. Twenty-one little-endian `u32` values, then four bytes.

| Offset | Field |
| --- | --- |
| 0 | uptime, seconds |
| 4 | free heap |
| 8 | lowest free heap since boot |
| 12 | largest free 8-bit block |
| 16 | BLE adverts heard |
| 20 | Classic devices heard |
| 24 | Classic inquiries this node started |
| 28 | mesh frames the radio accepted |
| 32 | mesh frames dropped because the receive queue was full |
| 36 | frames that failed authentication |
| 40 | duplicate frames |
| 44 | radio sends that succeeded |
| 48 | radio sends that failed or timed out |
| 52 | frames dropped because the send queue was full |
| 56 | parent changes |
| 60 | sightings evicted from a full table |
| 64 | parent node id, or 0 |
| 68 | current command id |
| 72 | Classic discovery-started events |
| 76 | Classic discovery-stopped events |
| 80 | Classic inquiry errors |
| 84 | rank |
| 85 | neighbour count |
| 86 | parent RSSI, as `i8`, or 0 |
| 87 | parent quality, recent beacons heard out of 16, or 0 |

## Probe, kind 5

Delivery-test payload from `node/src/main.rs`.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | probe sequence, little-endian |
| 4 | rest | zeros, out to the commanded size |

The size is at least 4 and at most 218.

## Command, kind 6, 11 bytes

`Command::encode` in `src/wire.rs`. A command of any other length is ignored.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 2 | id, little-endian |
| 2 | 2 | BLE scan window, milliseconds. 0 stops BLE scanning |
| 4 | 2 | BLE scan interval, milliseconds |
| 6 | 2 | seconds between Classic inquiries. 0 stops them |
| 8 | 1 | inquiry length, in units of 1.28 seconds |
| 9 | 1 | delivery-test frames per second. 0 stops them |
| 10 | 1 | delivery-test payload size |
