FROM rust:1-bookworm@sha256:c9155318d07341301a78b45ecdd81af2e43b8b297d64e8fd4c5e80d7735607c3 AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS openssh-runtime

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        ca-certificates \
        openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /runtime

# Distroless images do not ship OpenSSH. Copy the client and every shared
# library resolved by the Debian package into the final image.
RUN set -eux; \
    mkdir -p /runtime/usr/bin /runtime/etc/ssh /runtime/etc/ssl/certs; \
    cp /usr/bin/ssh /runtime/usr/bin/ssh; \
    for library in $(ldd /usr/bin/ssh | sed -n 's/.*=> \(\/[^ ]*\).*/\1/p; s/^\(\/[^ ]*\).*/\1/p' | sort -u); do \
        cp --parents "$library" /runtime; \
    done; \
    cp /etc/ssh/ssh_config /runtime/etc/ssh/ssh_config; \
    cp /etc/ssl/certs/ca-certificates.crt /runtime/etc/ssl/certs/ca-certificates.crt

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

COPY --from=builder /src/target/release/remote-luks-unlocker /usr/local/bin/remote-luks-unlocker
COPY --from=openssh-runtime /runtime/ /

ENTRYPOINT ["/usr/local/bin/remote-luks-unlocker"]
