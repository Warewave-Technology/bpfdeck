#!/bin/sh
# Install the mounted public key for every user, make tracefs available, run sshd.
set -eu
for u in root ops pw; do
  home=$(getent passwd "$u" | cut -d: -f6)
  mkdir -p "$home/.ssh"
  cp /keys/key.pub "$home/.ssh/authorized_keys"
  chown -R "$u:$u" "$home/.ssh"
  chmod 700 "$home/.ssh"
  chmod 600 "$home/.ssh/authorized_keys"
done
mountpoint -q /sys/kernel/tracing || mount -t tracefs nodev /sys/kernel/tracing
exec /usr/sbin/sshd -D -e
