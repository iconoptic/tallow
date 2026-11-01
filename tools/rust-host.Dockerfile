# The Rust toolchain for host/ (tallowd, and the crates under it), with libdbus for bluer.
#
# Built locally by `tools/mesh.sh daemon`, which runs `podman build -f` on this file with
# tools/ as the build context. It is the pinned base image plus two Debian packages and nothing else.
#
# The base image runs as `esp` (uid 1000). apt needs root, so this file switches to
# root for the install and back to `esp` afterwards. The build needs the network for
# apt, and nothing is fetched by curl or a script.
FROM docker.io/espressif/idf-rust:esp32_1.95.0.0

USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*

USER esp
