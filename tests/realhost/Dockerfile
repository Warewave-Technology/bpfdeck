# Debian 13 + Rust + bpftrace 0.23 for checks against a real kernel (the Docker VM's,
# e.g. OrbStack on macOS). See docs/real-kernel-testing.md.
FROM rust:1-trixie
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq bpftrace tmux procps >/dev/null \
 && rm -rf /var/lib/apt/lists/*
