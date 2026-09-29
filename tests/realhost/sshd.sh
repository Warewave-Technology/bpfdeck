#!/bin/sh
# Disposable SSH targets with a real kernel, for trying and testing remote targets.
#   tests/realhost/sshd.sh            # one target (PORT=22022 by default, 127.0.0.1 only)
#   COUNT=3 tests/realhost/sshd.sh    # three targets on ports 22022..22024 (fleet runs)
#   docker rm -f bpfdeck-sshd bpfdeck-sshd-2 bpfdeck-sshd-3   # stop them
# Users on each: root (key login), ops (sudo without password), pw (sudo password
# "secret-pw"). A throwaway key and an ssh wrapper live in $TMPDIR/bpfdeck-sshd; pass the
# wrapper to bpfdeck with --ssh and connect to ops@target (= target1), or to several at
# once: ops@target{1..3}. The targets share the Docker VM's kernel, so bpftrace sees the
# same system-wide activity on each.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
port=${PORT:-22022}
count=${COUNT:-1}
dir=${TMPDIR:-/tmp}/bpfdeck-sshd
mkdir -p "$dir"
[ -f "$dir/key" ] || ssh-keygen -q -t ed25519 -N '' -C bpfdeck-test -f "$dir/key"
docker build -q -t bpfdeck-realhost "$root/tests/realhost" > /dev/null
docker build -q -t bpfdeck-sshd "$root/tests/realhost/sshd" > /dev/null
: > "$dir/ssh_config"
i=1
while [ "$i" -le "$count" ]; do
  name=bpfdeck-sshd
  [ "$i" -gt 1 ] && name="bpfdeck-sshd-$i"
  p=$((port + i - 1))
  docker rm -f "$name" > /dev/null 2>&1 || true
  docker run -d --restart unless-stopped --privileged --name "$name" --hostname "target$i" \
    -p "127.0.0.1:$p:22" -v "$dir/key.pub:/keys/key.pub:ro" bpfdeck-sshd > /dev/null
  aliases="target$i"
  [ "$i" -eq 1 ] && aliases="target target1"
  cat >> "$dir/ssh_config" << CONFIG
Host $aliases
  HostName 127.0.0.1
  Port $p
  IdentityFile $dir/key
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
CONFIG
  echo "target$i is up on 127.0.0.1:$p ($name)"
  i=$((i + 1))
done
printf '#!/bin/sh\nexec ssh -F %s "$@"\n' "$dir/ssh_config" > "$dir/ssh"
chmod +x "$dir/ssh"
echo "  cargo run -- --ssh $dir/ssh tests/fixtures/scripts    # then: c, ops@target, Enter"
[ "$count" -gt 1 ] && echo "  several at once: c, ops@target{1..$count}, Enter"
true
