"""Toby's cams: the inventory, and a shared feed for each cam's MJPEG.

Cams live on tallow-iot, where a browser on the house LAN never reaches them.
This host dials the cam and forwards the frames. The cam's web server holds one
connection at a time, and a burst of connects and drops jams it for minutes, so
the panel is gentle with it: one connection per cam shared by every viewer, a
short linger before hanging up, a cooling-off after a failed dial, and status
asked of the node's API port instead of the camera port. A cam sends frames
only while somebody watches, plus that short linger. Nothing is written to disk.

data/panel/cams.json, re-read whenever it changes:
  {"title": "Toby", "cams": [{"id": "toby-cam-1", "name": "Cam 1", "address": "10.87.0.21"}]}

`title` is optional. Without it the menu says Cameras.
"""

from __future__ import annotations

import asyncio
import ipaddress
import json
import time
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from starlette.responses import StreamingResponse

import library

IOT_NET = ipaddress.ip_network("10.87.0.0/24")
AP_ADDR = ipaddress.ip_address("10.87.0.1")
STREAM_PORT = 8080
NODE_PORT = 6053  # the ESPHome API, served by the node's main loop rather than the one-socket camera server
VIEWERS_MAX = 8
LINGER_S = 10.0  # hang up this long after the last viewer leaves, so a tab switch does not churn the cam
COOLDOWN_S = (3.0, 6.0, 12.0, 24.0, 30.0)  # pause between dials after a failure
CONNECT_TIMEOUT = 5.0
HEADER_TIMEOUT = 10.0
NAME_MAX = 40
TITLE_MAX = 40
DEFAULT_TITLE = "Cameras"
HEAD_MAX = 8 * 1024
FRAME_MAX = 2 * 1024 * 1024
FRAME_TIMEOUT = 10.0
BOUNDARY = "tallow"


@dataclass(frozen=True)
class Cam:
    id: str
    name: str
    address: str
    port: int = STREAM_PORT
    node_port: int = NODE_PORT


class CamDown(Exception):
    """The cam did not answer with a stream."""


def parse_inventory(raw: Any, path: Path | None = None) -> tuple[str, list[Cam]]:
    """Read a cameras document. `title` may be omitted. Every address must be a node on tallow-iot."""
    where = str(path) if path is not None else "cameras"
    if not isinstance(raw, dict) or not isinstance(raw.get("cams"), list):
        raise ValueError(f'{where}: expected {{"cams": [...]}}')
    extra = set(raw) - {"cams", "title"}
    if extra:
        raise ValueError(f"{where}: unknown fields: " + ", ".join(sorted(extra)))
    title = raw.get("title", DEFAULT_TITLE)
    if not isinstance(title, str) or not title.strip() or len(title.strip()) > TITLE_MAX:
        raise ValueError(f"{where}: title needs 1 to {TITLE_MAX} characters")
    cams: list[Cam] = []
    for entry in raw["cams"]:
        if not isinstance(entry, dict):
            raise ValueError(f"{where}: each cam is an object")
        extra = set(entry) - {"id", "name", "address"}
        if extra:
            raise ValueError(f"{where}: unknown fields: " + ", ".join(sorted(extra)))
        cam_id = entry.get("id")
        if isinstance(cam_id, str):
            cam_id = cam_id.strip()
        if not isinstance(cam_id, str) or library.SLUG.fullmatch(cam_id) is None:
            raise ValueError(f"{where}: id must be a lowercase slug")
        if any(cam.id == cam_id for cam in cams):
            raise ValueError(f"{where}: {cam_id} is listed twice")
        name = entry.get("name", cam_id)
        if not isinstance(name, str) or not name.strip() or len(name) > NAME_MAX:
            raise ValueError(f"{where}: {cam_id} needs a name of at most {NAME_MAX} characters")
        try:
            address = ipaddress.ip_address(str(entry.get("address", "")).strip())
        except ValueError as exc:
            raise ValueError(f"{where}: {cam_id} needs an IPv4 address") from exc
        if address not in IOT_NET or address in (IOT_NET.network_address, IOT_NET.broadcast_address, AP_ADDR):
            raise ValueError(f"{where}: {cam_id} must be a node on {IOT_NET}")
        cams.append(Cam(cam_id, name.strip(), str(address)))
    return title.strip(), cams


def load_inventory(path: Path) -> tuple[str, list[Cam]]:
    """Read the inventory file. A missing file is an empty list named Cameras."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return DEFAULT_TITLE, []
    except json.JSONDecodeError as exc:
        raise ValueError(f"{path}: not JSON ({exc.msg}, line {exc.lineno})") from exc
    return parse_inventory(raw, path)


def load_cams(path: Path) -> list[Cam]:
    """The cameras in the inventory, without the title."""
    return load_inventory(path)[1]


def save_inventory(path: Path, raw: Any) -> tuple[str, list[Cam]]:
    """Validate and replace the inventory. The write is a temp file renamed into place."""
    title, cams = parse_inventory(raw, path)
    path.parent.mkdir(parents=True, exist_ok=True)
    document = {
        "title": title,
        "cams": [{"id": cam.id, "name": cam.name, "address": cam.address} for cam in cams],
    }
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    temp.replace(path)
    return title, cams


class Book:
    """The cams the panel knows. A file is re-read when it changes, so a new cam needs no restart.

    A bad edit keeps the last good list and says what is wrong.
    """

    def __init__(self, path: Path | None = None, cams: list[Cam] | None = None) -> None:
        self.path = path
        self.error: str | None = None
        self._title = DEFAULT_TITLE
        self._cams = list(cams or [])
        self._stamp: int | None = -1

    def title(self) -> str:
        """The name on the menu. Reloads the file when it has changed."""
        self.all()
        return self._title

    def forget(self) -> None:
        """Read the file again on the next lookup, even if the mtime did not move."""
        self._stamp = -1

    def all(self) -> list[Cam]:
        if self.path is None:
            return self._cams
        try:
            stamp: int | None = self.path.stat().st_mtime_ns
        except FileNotFoundError:
            stamp = None
        except OSError as exc:
            self.error = f"{self.path}: {exc.strerror or type(exc).__name__}"
            return self._cams
        if stamp != self._stamp:
            self._stamp = stamp
            try:
                self._title, self._cams = load_inventory(self.path)
                self.error = None
            except (OSError, ValueError) as exc:
                self.error = str(exc)
        return self._cams

    def get(self, cam_id: str) -> Cam | None:
        return next((cam for cam in self.all() if cam.id == cam_id), None)


class Upstream:
    """One open connection to a cam, past its response headers."""

    def __init__(self, reader: asyncio.StreamReader, writer: Any) -> None:
        self.reader = reader
        self.writer = writer

    async def frames(self) -> AsyncIterator[bytes]:
        """Each JPEG, as the cam framed it. Ends when the cam goes quiet or hangs up."""
        try:
            while True:
                length: int | None = None
                while True:
                    line = (await asyncio.wait_for(self.reader.readuntil(b"\r\n"), FRAME_TIMEOUT)).strip()
                    if not line:
                        if length is not None:
                            break
                        continue
                    name, sep, value = line.partition(b":")
                    if sep and name.strip().lower() == b"content-length":
                        length = int(value)
                if length is None or not 0 < length <= FRAME_MAX:
                    return
                yield await asyncio.wait_for(self.reader.readexactly(length), FRAME_TIMEOUT)
        except (OSError, asyncio.IncompleteReadError, asyncio.LimitOverrunError, TimeoutError, ValueError):
            return

    def close(self) -> None:
        self.writer.close()


async def dial(cam: Cam, connect_timeout: float = CONNECT_TIMEOUT, header_timeout: float = HEADER_TIMEOUT) -> Upstream:
    """Ask the cam for its stream. Raise CamDown unless it answers with MJPEG."""
    try:
        reader, writer = await asyncio.wait_for(asyncio.open_connection(cam.address, cam.port), connect_timeout)
    except (OSError, TimeoutError) as exc:
        raise CamDown(f"{cam.id}: {type(exc).__name__}") from exc
    try:
        writer.write(f"GET / HTTP/1.1\r\nHost: {cam.address}\r\nConnection: close\r\n\r\n".encode())
        await writer.drain()
        head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), header_timeout)
        if len(head) > HEAD_MAX:
            raise CamDown(f"{cam.id}: headers too long")
        lines = head.decode("latin-1").split("\r\n")
        status = lines[0].split(" ", 2)
        if len(status) < 2 or status[1] != "200":
            raise CamDown(f"{cam.id}: {lines[0]}")
        headers: dict[str, str] = {}
        for line in lines[1:]:
            name, sep, value = line.partition(":")
            if sep:
                headers[name.strip().lower()] = value.strip()
        content_type = headers.get("content-type", "")
        if not content_type.lower().startswith("multipart/x-mixed-replace"):
            raise CamDown(f"{cam.id}: not a stream ({content_type or 'no content type'})")
        return Upstream(reader, writer)
    except CamDown:
        writer.close()
        raise
    except (OSError, TimeoutError, asyncio.IncompleteReadError, asyncio.LimitOverrunError) as exc:
        writer.close()
        raise CamDown(f"{cam.id}: {type(exc).__name__}") from exc
    except BaseException:
        writer.close()  # cancelled mid-dial: do not leave a half-open request in the cam's queue
        raise


async def reachable(cam: Cam, timeout: float = 1.5) -> bool:
    """Does the node pick up? Asks its API port, never the one-socket camera server."""
    try:
        _reader, writer = await asyncio.wait_for(asyncio.open_connection(cam.address, cam.node_port), timeout)
    except (OSError, TimeoutError):
        return False
    writer.close()
    return True


def _offer(queue: asyncio.Queue[bytes | None], item: bytes | None) -> None:
    """Hand a frame to a viewer. A slow viewer skips frames instead of holding the rest back."""
    if queue.full():
        try:
            queue.get_nowait()
        except asyncio.QueueEmpty:
            pass
    queue.put_nowait(item)


class Hub:
    """One connection to one cam, shared by everyone watching it."""

    def __init__(
        self, cam: Cam, connect: Callable[[Cam], Any] = dial, clock: Callable[[], float] = time.monotonic
    ) -> None:
        self.cam = cam
        self.queues: set[asyncio.Queue[bytes | None]] = set()
        self._connect = connect
        self._clock = clock
        self._pump: asyncio.Task[None] | None = None
        self._lock = asyncio.Lock()
        self._idle: asyncio.TimerHandle | None = None
        self._stopping = False
        self._failures = 0
        self._retry_at = 0.0

    @property
    def watching(self) -> int:
        return len(self.queues)

    @property
    def idle(self) -> bool:
        """No connection to the cam, open or on its way out."""
        return self._pump is None or self._pump.done()

    async def join(self) -> asyncio.Queue[bytes | None]:
        """Start watching. The first viewer wakes the cam. Raises CamDown if it will not answer."""
        async with self._lock:
            if self._idle is not None:
                self._idle.cancel()
                self._idle = None
            if self._pump is not None and (self._pump.done() or self._stopping):
                # It ended, or is being hung up on. Let its socket close before asking the cam again.
                await asyncio.wait({self._pump})
                self._pump = None
                self._stopping = False
            if self._pump is None:
                self._pump = asyncio.create_task(self._run(await self._dial()))
            queue: asyncio.Queue[bytes | None] = asyncio.Queue(maxsize=2)
            self.queues.add(queue)
            return queue

    async def _dial(self) -> Upstream:
        """Dial the cam, but not while it is still catching its breath from the last failure."""
        wait = self._retry_at - self._clock()
        if wait > 0:
            raise CamDown(f"{self.cam.id}: cooling off for {wait:.0f} s")
        try:
            upstream = await self._connect(self.cam)
        except CamDown:
            self._failures += 1
            self._retry_at = self._clock() + COOLDOWN_S[min(self._failures, len(COOLDOWN_S)) - 1]
            raise
        self._failures = 0
        return upstream

    def leave(self, queue: asyncio.Queue[bytes | None]) -> None:
        """Stop watching. After a short linger the last one out hangs up, and the cam stops sending."""
        self.queues.discard(queue)
        if not self.queues and not self.idle and self._idle is None:
            self._idle = asyncio.get_running_loop().call_later(LINGER_S, self._hang_up)

    def _hang_up(self) -> None:
        self._idle = None
        if not self.queues and self._pump is not None:
            self._stopping = True
            self._pump.cancel()

    async def _run(self, upstream: Upstream) -> None:
        try:
            async for frame in upstream.frames():
                for queue in list(self.queues):
                    _offer(queue, frame)
        finally:
            upstream.close()
            for queue in list(self.queues):
                _offer(queue, None)


class Hubs:
    """A Hub for each cam that has had a viewer."""

    def __init__(self, connect: Callable[[Cam], Any] = dial) -> None:
        self._connect = connect
        self._hubs: dict[Cam, Hub] = {}

    def hub(self, cam: Cam) -> Hub:
        if cam not in self._hubs:
            self._hubs[cam] = Hub(cam, self._connect)
        return self._hubs[cam]

    def watching(self, cam: Cam) -> int:
        hub = self._hubs.get(cam)
        return hub.watching if hub else 0

    def prune(self, keep: list[Cam]) -> None:
        for cam, hub in list(self._hubs.items()):
            if cam not in keep and not hub.queues and hub.idle:
                del self._hubs[cam]


async def _parts(queue: asyncio.Queue[bytes | None]) -> AsyncIterator[bytes]:
    while (frame := await queue.get()) is not None:
        yield (
            b"--%s\r\nContent-Type: image/jpeg\r\nContent-Length: %d\r\n\r\n" % (BOUNDARY.encode(), len(frame))
            + frame
            + b"\r\n"
        )


class Feed(StreamingResponse):
    """One viewer's copy of a cam. When the viewer goes, the hub is told."""

    def __init__(self, hub: Hub, queue: asyncio.Queue[bytes | None]) -> None:
        super().__init__(
            _parts(queue),
            media_type=f"multipart/x-mixed-replace; boundary={BOUNDARY}",
            headers={"cache-control": "no-store", "x-content-type-options": "nosniff"},
        )
        self.hub = hub
        self.queue = queue

    async def __call__(self, scope: Any, receive: Any, send: Any) -> None:
        try:
            await super().__call__(scope, receive, send)
        finally:
            self.hub.leave(self.queue)
            await self.body_iterator.aclose()  # type: ignore[attr-defined]
