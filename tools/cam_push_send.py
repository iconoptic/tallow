"""Send one signed image to a cam, and report what the cam said.

Called by `tools/mesh.sh cam-push`, which has already refused every address but
10.87.0.21. This repeats that check, and refuses an image signed for anything
but the cam before it opens a socket. Run nothing else against a cam.

The `.sig` file is 69 bytes: product, little-endian version, signature.
The header on the wire is a big-endian size, the product, the little-endian
version, the SHA-256, and the signature. After the image the cam sends one byte,
a `Code` from `firmware/mesh/core/src/ota.rs`.
"""

from __future__ import annotations

import hashlib
import pathlib
import socket
import struct
import sys

ALLOWED = "10.87.0.21"
PORT = 3232
PRODUCT_CAM0 = 2
# app0 and app1 in firmware/cam/partitions.csv.
SLOT = 0x7C_0000
SIG_LEN = 69
CHUNK = 4096
# Longer than the erase before the cam reads the first image byte.
STEP_SECONDS = 60.0

CODES = {
    0: "ok: stored, and the cam is rebooting into it",
    1: "bad signature: nothing was written",
    2: "bad hash: the image did not match the signature, and it will not boot",
    3: "missing: the cam stopped before the whole image arrived",
    4: "refused: bad size, a flash failure, or an unconfirmed image still running",
    5: "downgrade: this version is below what the cam has confirmed",
    6: "wrong product: this image is not for this cam",
}


def parse_sig(raw: bytes) -> tuple[int, int, bytes]:
    """Product, version, signature. Raises ValueError on a wrong length."""
    if len(raw) != SIG_LEN:
        raise ValueError(f"signature file is {len(raw)} bytes, not {SIG_LEN}")
    version = struct.unpack("<I", raw[1:5])[0]
    return raw[0], version, raw[5:]


def build_header(image: bytes, product: int, version: int, signature: bytes) -> bytes:
    digest = hashlib.sha256(image).digest()
    return struct.pack(">I", len(image)) + bytes([product]) + struct.pack("<I", version) + digest + signature


def meaning(answer: bytes) -> tuple[bool, str]:
    """Whether the cam said ok, and what to print."""
    if not answer:
        return False, "the cam closed the connection with no result"
    code = answer[0]
    return code == 0, CODES.get(code, f"unknown result {code}")


def send(address: str, image: bytes, sig: bytes, connect=socket.create_connection) -> tuple[bool, str]:
    product, version, signature = parse_sig(sig)
    if address != ALLOWED:
        return False, f"{address} is not {ALLOWED}"
    if product != PRODUCT_CAM0:
        return False, f"signed for product {product}, not the cam ({PRODUCT_CAM0})"
    if not 0 < len(image) <= SLOT:
        return False, f"image is {len(image)} bytes, which does not fit the cam slot"
    header = build_header(image, product, version, signature)
    with connect((address, PORT), timeout=STEP_SECONDS) as sock:
        sock.settimeout(STEP_SECONDS)
        try:
            sock.sendall(header)
            for start in range(0, len(image), CHUNK):
                sock.sendall(image[start : start + CHUNK])
        except (BrokenPipeError, ConnectionResetError):
            # A refusal closes early. Its byte may already be waiting.
            pass
        try:
            return meaning(sock.recv(1))
        except OSError as err:
            return False, f"no result from the cam: {err}"


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: cam_push_send.py <address> <image> <image.sig>", file=sys.stderr)
        return 2
    address, image_path, sig_path = argv
    image = pathlib.Path(image_path).read_bytes()
    sig = pathlib.Path(sig_path).read_bytes()
    try:
        ok, text = send(address, image, sig)
    except (OSError, ValueError) as err:
        ok, text = False, str(err)
    print(f"{image_path} to {address}:{PORT}: {text}", file=sys.stdout if ok else sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
