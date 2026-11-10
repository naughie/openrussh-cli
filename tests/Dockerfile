FROM rust:trixie AS toolchain

ARG SSH_TEST_ID

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_TARGET_DIR=/cargo-target \
    PATH=/usr/local/cargo/bin:$PATH

WORKDIR /repo
COPY . .

RUN mkdir /dist \
    && cp ./tests/docker/entrypoint.sh /dist/entrypoint.sh \
    && cp ./tests/docker/setup.sh /dist/setup.sh

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/cargo-target \
       cargo build --locked --manifest-path Cargo.toml --bin ssh \
    && cp /cargo-target/debug/ssh /dist/ssh \
    && cargo build --locked --manifest-path ./tests/${SSH_TEST_ID}/Cargo.toml \
    && cp /cargo-target/debug/test-${SSH_TEST_ID} /dist/run-test


FROM debian:13.7-slim

ARG OPENSSH_VERSION
ARG APT_SNAPSHOT

RUN printf '%s\n' 'Acquire::Retries "5";' > /etc/apt/apt.conf.d/80-retries \
    && rm -f /etc/apt/sources.list /etc/apt/sources.list.d/* \
    && printf '%s\n' \
       "deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/${APT_SNAPSHOT}/ trixie main" \
       "deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/${APT_SNAPSHOT}/ trixie-security main" \
       > /etc/apt/sources.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
       "openssh-server=${OPENSSH_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -g 1001 bob \
    && useradd --create-home --shell /bin/sh bob -u 1001 -g 1001 \
    && passwd -d bob \
    && groupadd -g 1002 alice \
    && useradd --create-home --shell /bin/sh alice -u 1002 -g 1002 \
    && install -d -m 0700 -o bob -g bob /home/bob/.ssh \
    && install -d -m 0700 -o alice -g alice /home/alice/.ssh

RUN printf '%s\n' \
       'Host *' \
       '    HostName 127.0.0.1' \
       '    Port 2222' \
       '    User bob' \
       > /home/alice/.ssh/config \
    && chmod 0600 /home/alice/.ssh/config \
    && chown alice:alice /home/alice/.ssh/config

RUN rm -f /etc/ssh/ssh_host_* \
    && ssh-keygen -q -t ed25519 -N '' -f /etc/ssh/ssh_host_ed25519_key \
    && { \
       printf '[127.0.0.1]:2222 '; \
       cat /etc/ssh/ssh_host_ed25519_key.pub; \
       } > /home/alice/.ssh/known_hosts \
    && ssh-keygen -H -f /home/alice/.ssh/known_hosts \
    && rm -f /home/alice/.ssh/known_hosts.old \
    && chmod 0600 /home/alice/.ssh/known_hosts \
    && chown alice:alice /home/alice/.ssh/known_hosts \
    && mkdir -p /run/sshd \
    && printf '%s\n' \
       'AllowUsers bob' \
       'AuthenticationMethods publickey' \
       'AuthorizedKeysFile .ssh/authorized_keys' \
       'HostKey /etc/ssh/ssh_host_ed25519_key' \
       'KbdInteractiveAuthentication no' \
       'ListenAddress 127.0.0.1' \
       'MaxStartups 100' \
       'PasswordAuthentication no' \
       'PermitEmptyPasswords no' \
       'PermitRootLogin no' \
       'PerSourcePenalties no' \
       'Port 2222' \
       'PubkeyAuthentication yes' \
       'UsePAM no' \
       > /etc/ssh/sshd_config

COPY --from=toolchain /dist/setup.sh /opt/tests/setup.sh
RUN /bin/sh /opt/tests/setup.sh

COPY --from=toolchain /dist/ssh /opt/tests/ssh

COPY --chown=alice:alice --from=toolchain /dist/run-test /opt/tests/run-test
COPY --chown=alice:alice --from=toolchain /dist/entrypoint.sh /opt/tests/test-entrypoint
RUN chmod 0700 /opt/tests/test-entrypoint

ENTRYPOINT ["/opt/tests/test-entrypoint"]
CMD ["/opt/tests/run-test"]
