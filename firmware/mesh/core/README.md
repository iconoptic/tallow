# Mesh core

Routing and frames, with no radio and no OS. The firmware in `../node` supplies the clock and the bytes. `tests/sim.rs` runs the mesh on a pretend radio. `tests/routing.rs` covers parent changes one node at a time. `tests/golden.rs` locks one sealed frame of each kind, and the payloads the real code builds.

The header is in `src/wire.rs`. Version is `0xAB`. Boards on another version ignore these frames, so every board is flashed together. Version `0xAB` adds kind 15, Neighbours: each neighbour's id and the short RSSI average. Version `0xAA` adds kind 14, Identity: the product, the running version, and the confirmed floor. Version `0xA9` defines beacon byte 7 bit 0 as `LEAF`: a node that sets it is stored as a neighbour and is never chosen as a parent. Version `0xA8` added the product and version to the OTA Begin. Version `0xA7` added kinds 9 and 13 for OTA. Version `0xA6` added kinds 10 and 12 for GATT (T1.10). Version `0xA5` was the last one before that.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | version |
| 1 | 1 | kind |
| 2 | 1 | ttl, changed by forwarders, not authenticated |
| 3 | 1 | hops, changed by forwarders, not authenticated |
| 4 | 4 | origin node id, little-endian |
| 8 | 4 | origin boot id, little-endian: the boot counter in the top 20 bits, 12 random bits below |
| 12 | 4 | origin sequence, little-endian |
| 16 |  | ciphertext |
| last 16 | 16 | Poly1305 tag |

A boot counter is kept in NVS and raised once per boot, so a larger counter is a later boot. A frame from an earlier boot of its origin is refused, and so is a frame from another boot with the same counter. See `fresh` in `src/mesh.rs`.

The nonce is 12 bytes: origin, boot, and sequence, each little-endian, in that order. Associated data is version, kind, origin, boot, and sequence, 14 bytes as they stand in the header.

A frame with another version or an unknown kind is refused before the tag is checked.

## Beacon, kind 1, 11 bytes

Built in `src/mesh.rs`. A payload of any other length is rejected. A beacon whose source address does not match its origin is rejected, because beacons are not forwarded.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | rank. 0 is the root. 255 means no route |
| 1 | 4 | parent node id, little-endian. 0 if this node has none |
| 5 | 2 | beacon number, little-endian, wrapping |
| 7 | 1 | flags. Bit 0 is `LEAF`: this node must not be chosen as a parent. Other bits are zero |
| 8 | 3 | sent as zero, not read |

## Report, kind 2

Sighting records from `src/sightings.rs`, 10 bytes each, as many as fit in one frame. The host reads them with `Records::new`, which skips a partial record at the end.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 6 | address |
| 6 | 1 | kind: 0 BLE public, 1 BLE random, 2 Classic |
| 7 | 1 | median RSSI of the distances in the window, as `i8`. -127 when the window had none |
| 8 | 1 | how many times it was heard, saturating at 255 |
| 9 | 1 | seconds since the last hear, saturating at 255 |

## Name, kind 3

Name records from `src/names.rs`. Each record is 13 bytes plus the name. The name is at most 32 bytes. The host reads them with `NameRecords::new`, which stops at a record whose name runs past the end. A name need not be UTF-8.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 6 | address |
| 6 | 1 | kind, same codes as a report |
| 7 | 2 | company id, little-endian. `0xFFFF` when unknown |
| 9 | 3 | class of device, low byte first. Zero when unknown |
| 12 | 1 | name length |
| 13 | length | name bytes |

## Stats, kind 4, 88 bytes

Built by `Stats::encode` in `src/stats.rs`, from `node/src/main.rs`, and read by `Stats::decode`. Twenty-one little-endian `u32` values, then four bytes.

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
| 36 | frames that failed authentication, beacons of the wrong length or from the wrong source |
| 40 | duplicate frames, and frames outside the replay window |
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

## Route, kind 7, 13 bytes

Built in `src/mesh.rs`, and read by `Route::decode` in `src/stats.rs`. A node sends one each time its parent changes, and it sends them up, so the host sees them. A node with no parent keeps its losses and sends them once it has one again. A join is a change from no parent, so it is reported with reason 2. The root never changes parent, so it never sends one. The uptime is the node's clock in seconds, which wraps with that clock.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | uptime, seconds, little-endian |
| 4 | 4 | old parent node id, little-endian. 0 if there was none |
| 8 | 4 | new parent node id, little-endian. 0 if there is none |
| 12 | 1 | reason. 0 beacon timeout, 1 failed acknowledgements, 2 better path |

Reason 0 means the parent's beacons stopped. Reason 1 means unicasts to it failed. Reason 2 is everything else: a better neighbour, a first parent, or a board that stopped being the root and joined.

## Watch, kind 8, 3 to 99 bytes

Built by `Watch::encode` in `src/wire.rs`, and read by `Watch::decode`. The root sends one with `Mesh::send_watch`. It floods from the root, as a command does, and each node forwards it once. It replaces the node's list of Classic addresses to page, whole. A count of 0 clears the list, so nothing is paged. The node firmware pages only these addresses, one at a time.

A Watch whose length does not match its count, or whose count is over 16, is refused and counted in `rx_bad`. It is never cut short, and it is not forwarded. A Watch whose id is not newer than the last one accepted is ignored and not forwarded.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 2 | list id, little-endian |
| 2 | 1 | count, 0 to 16 |
| 3 | 6 each | addresses, in the order the host sent them |

The root makes the id, so the host sends no id. The high 12 bits are the low 12 bits of the boot counter, and the low 4 bits number the lists this boot has sent. Ids compare with wrapping subtraction, as sequence numbers do. Consecutive boots always order correctly. A list from 2048 or more boots earlier cannot be ordered reliably, and neither can one from a board whose counter was seeded from the radio. The root refuses a 17th list in one boot, because its id would repeat.

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

## Ping, kind 11, downward

Sent only by the root, with `Mesh::send_down`, and only for a downward kind (`mesh::is_down`). A Ping is a test frame, and the node firmware does nothing with it. It is never broadcast. Each hop that is not the target unicasts it to the next hop, with the same retries as any unicast. A Ping under 4 bytes is dropped and counted in `down_drops`.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | target node id, little-endian |
| 4 | rest | payload, given to the target as `Event::Down` |

A node learns a route from each upward frame it receives: report, name, stats, probe, route, GATT answer, OTA status and result, or an identity. The route is the origin's id and the neighbour the frame came from. The table holds 32 routes, and the one refreshed longest ago is replaced when it is full. A route expires 60 seconds after its last upward frame. A downward frame with no live route is dropped and counted in `down_drops`. Beacons and commands do not teach routes. `Ota` is a downward kind. `OtaUp`, `GattUp`, and `Identity` travel up, so they do not use the downward path.

## Gatt, kind 10, downward

The host's request to one node's BLE connection. The root sends it with `Mesh::send_down`, so the frame's payload starts with the target id, and the node sees the request after it. The node's client is `src/gatt.rs`. `gatt::parse` reads these bytes, and `gatt::encode` writes a request `parse` accepts. The operations are the ones below. There is no new operation byte. Every request has the same first three bytes: an id, then an operation. The host must not reuse an id for a new request.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 2 | request id, little-endian |
| 2 | 1 | operation: 0 connect, 1 write, 2 read, 3 subscribe, 4 disconnect, 5 whitelist |
| 3 | rest | the body for that operation, below |

| Operation | Body |
| --- | --- |
| 0 connect | 6 bytes, the address; 1 byte, address type: 0 public, 1 random. Exactly 7 bytes. |
| 1 write | 2 bytes, handle; then the value, 0 to 170 bytes |
| 2 read | 2 bytes, handle. Exactly 2 bytes |
| 3 subscribe | 2 bytes, the handle of the characteristic. Exactly 2 bytes |
| 4 disconnect | none |
| 5 whitelist | 1 byte, count, 0 to 8; then 6 bytes per address. Replaces the list whole. Starts empty |

A request that is too short, names another operation, or has a body of the wrong length is malformed. If its first two bytes are there, it answers `Malformed` (6), so the host hears back.

Connect goes ahead only to an address on the whitelist. An empty list refuses every connect. One connection per node, and a second connect answers `Busy`. A request with no connection answers `NotConnected`. A request that outlives its timeout answers `TimedOut`, and the link is dropped. A connect has 12 s, and every other request has 4 s.

## GattUp, kind 12, upward

What a node sends back to the host: the answer to a request, a notification from the connected device, or a report that the link closed. Each one is sent with `Mesh::send_up`, and the root prints it as an `@U 12` line. The first byte after the id, or link id, is the kind. `Reply::encode` writes this payload. `Reply::decode` reads what `Reply::encode` wrote. A short buffer, an unknown kind, an unknown status, or a value past the kind's limit is `None`.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 2 | answer: request id. Notification or closed: the link id, the request id of the connect that opened it |
| 2 | 1 | kind: 1 answer, 2 notification, 3 closed |

Kind 1, an answer, has this layout after the common two bytes:

| Offset | Bytes | Field |
| --- | --- | --- |
| 3 | 1 | status: 0 ok, 1 refused, 2 busy, 3 not connected, 4 timed out, 5 failed, 6 malformed |
| 4 | rest | the value a read returned, 0 to 170 bytes. Empty for every other request |

Kind 2, a notification, is the connected device's bytes, up to 213, and they are never read as text:

| Offset | Bytes | Field |
| --- | --- | --- |
| 3 | 2 | handle, little-endian |
| 5 | rest | the value, as the device sent it |

Kind 3, closed, has no more bytes. It is sent once when the link drops with no request in flight. A request in flight when the link drops answers `NotConnected` instead.

A node that repeats the request id of the last finished request sends that answer again, and does not run the operation again. So a lost answer costs one more frame, not a second write.

## Ota, kind 9, downward

One image, sent to one node with `Mesh::send_down`. The frame payload starts with the target id. The node sees the bytes below, and `ota::decode_down` reads them. `ota::encode_down` writes what that reads. A short buffer, an unknown sub-kind, or a body of the wrong length is `None`.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | sub-kind: 1 begin, 2 chunk, 3 query, 4 commit |

Begin is 106 bytes, so 110 with the target id:

| Offset | Bytes | Field |
| --- | --- | --- |
| 1 | 4 | image size, little-endian. 0, or larger than this receiver's slot, is refused |
| 5 | 1 | product: 1 mesh node, 2 `toby-cam-0` |
| 6 | 4 | version, little-endian |
| 10 | 32 | SHA-256 of the image |
| 42 | 64 | Ed25519 signature over the signed message below |

The signed message is 50 bytes, from `ota::signed_message`:

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 13 | `tallow-ota-v1` |
| 13 | 1 | product |
| 14 | 4 | version, little-endian |
| 18 | 32 | SHA-256 of the image |

Begin is checked before any flash action, in this order: size, product, the version floor, then the signature. A refused Begin answers with a Result, yields no action, and leaves any image in progress alone. A version equal to the floor is a retry and is accepted. The floor is what the board last confirmed, passed to `Receiver::new`. The size cap is that receiver's slot: `SLOT` (`0x1C0000`) for a classic node, `CAM_SLOT` (`0x7C0000`) for a camera.

Chunk is 6 to 175 bytes:

| Offset | Bytes | Field |
| --- | --- | --- |
| 1 | 4 | offset, little-endian, a multiple of 170 |
| 5 | 1 to 170 | the bytes at that offset. A chunk that is not the last is 170 bytes. The last is whatever remains |

Query is 3 bytes: offset 1, window number, little-endian. Commit is the one sub-kind byte and nothing else.

`ota::Receiver` accepts a chunk only when its offset is the next byte not yet hashed. That chunk is hashed into the running SHA-256, and the action is to write those bytes at that offset. A chunk already hashed is ignored. A chunk ahead of that cursor is not stored, so a later Status still names it. A second Begin drops the image in progress and starts the new one. Commit first checks the floor again. The floor can rise after Begin, when the running image is confirmed, so a Commit whose version is now below it answers `5` (`Downgrade`). Otherwise Commit hashes what was written and checks it against Begin's digest, then checks the signature again with the public key the caller passes in. A missing chunk, a bad hash, or a bad signature yields no finish. Any Commit that does not end `Ok` queues an abort and clears the image, so the next Begin starts clean. Finish is the only action that may activate the image. It carries the version that was signed, and that does not raise the floor. The board does that once the image is confirmed.

## OtaUp, kind 13, upward

What the receiver sends back, with `Mesh::send_up`. The root prints it as an `@U 13` line. `ota::encode_up` writes this payload. `ota::decode_up` reads it.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | sub-kind: 1 status, 2 result |

Status, 35 bytes:

| Offset | Bytes | Field |
| --- | --- | --- |
| 1 | 2 | window, little-endian. One window is 256 chunks |
| 3 | 32 | bitmap. Bit `i` set means chunk `window * 256 + i` is missing. Bits past the image are clear |

Result, 2 bytes:

| Offset | Bytes | Field |
| --- | --- | --- |
| 1 | 1 | code: 0 ok, 1 bad signature, 2 bad hash, 3 missing chunks, 4 refused, 5 below the version floor (from Begin, or from Commit when the floor rose after Begin), 6 another product |

## Identity, kind 14, upward

What a node or a camera sends up with its stats, every ten seconds. `Identity::encode` writes this payload. `Identity::decode` reads it. A payload of any other length is rejected.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | product. 1 is a classic node, 2 is a camera |
| 1 | 4 | running version, little-endian. The image that is executing |
| 5 | 4 | floor, little-endian. The highest version this board has confirmed |

While the boot slot is pending verify, the running version is the pending image, so a host does not push that same image again during the five-minute mark. Otherwise the running version is the floor. The host sends an image only when this product matches the signature and the running version is lower.

## Neighbours, kind 15, upward

What a node or a camera sends up with its stats, every ten seconds, when it has at least one neighbour. `Mesh::encode_neighbours` writes this payload from the neighbour table. `HeardRecords` reads it. Each record is 5 bytes, and as many fit as the node has neighbours, at most 12. A short tail is not a record.

The RSSI is the short average the node already keeps for that neighbour, `(3·old + new) / 4` on each beacon. It is not a long median. The host medians these reports over a day.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | neighbour node id, little-endian |
| 4 | 1 | RSSI, as `i8` |
