#!/bin/sh
# A disposable SSH target with a real kernel, for trying and testing remote targets.
#   tests/realhost/sshd.sh            # start it (PORT=22022 by default, 127.0.0.1 only)
#   docker rm -f bpfdeck-sshd         # stop it
# Users: root (key login), ops (sudo without password), pw (sudo password "secret-pw").
# A throwaway key and an ssh wrapper live in $TMPDIR/bpfdeck-sshd; pass the wrapper to
# bpfdeck with --ssh and connect to "ops@target", "pw@target" or "root@target".
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
port=${PORT:-22022}
dir=${TMPDIR:-/tmp}/bpfdeck-sshd
mkdir -p "$dir"
[ -f "$dir/key" ] || ssh-keygen -q -t ed25519 -N '' -C bpfdeck-test -f "$dir/key"
docker build -q -t bpfdeck-realhost "$root/tests/realhost" > /dev/null
docker build -q -t bpfdeck-sshd "$root/tests/realhost/sshd" > /dev/null
docker rm -f bpfdeck-sshd > /dev/null 2>&1 || true
docker run -d --restart unless-stopped --privileged --name bpfdeck-sshd -p "127.0.0.1:$port:22" \
  -v "$dir/key.pub:/keys/key.pub:ro" bpfdeck-sshd > /dev/null
cat > "$dir/ssh_config" << CONFIG
Host target
  HostName 127.0.0.1
  Port $port
  IdentityFile $dir/key
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
CONFIG
printf '#!/bin/sh\nexec ssh -F %s "$@"\n' "$dir/ssh_config" > "$dir/ssh"
chmod +x "$dir/ssh"
echo "target is up on 127.0.0.1:$port"
echo "  cargo run -- --ssh $dir/ssh tests/fixtures/scripts    # then: c, ops@target, Enter"
echo "  stop with: docker rm -f bpfdeck-sshd"
