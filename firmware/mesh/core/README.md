# Mesh core

Routing and frames, with no radio and no OS. The firmware in `../node` supplies the clock and the bytes. `tests/sim.rs` runs the mesh on a pretend radio. `tests/routing.rs` covers parent changes one node at a time. `tests/golden.rs` locks one sealed frame of each kind, and the payloads the real code builds.

The header is in `src/wire.rs`. Version is `0xA6`. Boards on another version ignore these frames, so every board is flashed together. Version `0xA6` added kinds 10 and 12 for GATT (T1.10). Version `0xA5` was the last one before it.

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
| 7 | 4 | sent as zero, not read |

## Report, kind 2

Sighting records from `src/sightings.rs`, 10 bytes each, as many as fit in one frame. The host reads them with `Records::new`, which skips a partial record at the end.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 6 | address |
| 6 | 1 | kind: 0 BLE public, 1 BLE random, 2 Classic |
| 7 | 1 | best RSSI, as `i8` |
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

A node learns a route from each upward frame it receives: report, name, stats, probe, route, or GATT answer. The route is the origin's id and the neighbour the frame came from. The table holds 32 routes, and the one refreshed longest ago is replaced when it is full. A route expires 60 seconds after its last upward frame. A downward frame with no live route is dropped and counted in `down_drops`. Beacons and commands do not teach routes. Ota will be a downward kind too. `GattUp` is an upward kind, so it does not use the downward path.

## Gatt, kind 10, downward

The host's request to one node's BLE connection. The root sends it with `Mesh::send_down`, so the frame's payload starts with the target id, and the node sees the request after it. The node's client is `src/gatt.rs`, and `gatt::parse` reads these bytes. Every request has the same first three bytes: an id, then an operation. The host must not reuse an id for a new request.

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

What a node sends back to the host: the answer to a request, a notification from the connected device, or a report that the link closed. Each one is sent with `Mesh::send_up`, and the root prints it as an `@U 12` line. The first byte after the id, or link id, is the kind.

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
