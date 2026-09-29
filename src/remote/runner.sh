# bpfdeck remote runner, sent over `ssh host sh -s` (after an optional fixed sudo line that
# turns the session into a root `sh -s`): nothing is installed on the host.
# Protocol on stdin, after the "ready" handshake: "bpfdeck-remote 1", argv count, one argv
# entry per line, script length, script bytes.
# After "started", any line or EOF on stdin stops the command: SIGINT, then SIGTERM
# after 5 s, SIGKILL after 2 s more, to the command's whole process group. Signals are
# sent by this shell (the command's parent), so the pid cannot have been reused. The
# exit status is the command's.
bpfdeck_main() {
  umask 077
  echo "bpfdeck-remote: ready" >&2
  IFS= read -r version || exit 71
  [ "$version" = "bpfdeck-remote 1" ] || { echo "bpfdeck-remote: bad header" >&2; exit 71; }
  IFS= read -r n || exit 71
  set --
  i=0
  while [ "$i" -lt "$n" ]; do
    IFS= read -r a || exit 71
    set -- "$@" "$a"
    i=$((i + 1))
  done
  IFS= read -r len || exit 71
  d=$(mktemp -d "${TMPDIR:-/tmp}/bpfdeck.XXXXXX") || exit 72
  trap 'rm -rf "$d"' EXIT
  if [ "$len" -gt 0 ]; then
    head -c "$len" > "$d/script.bt" || exit 72
  fi
  cd "$d" || exit 72
  # Own process group (like the local runner), so children of bpftrace are signalled too.
  # A background job starts with SIGINT ignored; undo that where the shell allows it
  # (bash 5, zsh; not dash). bpftrace installs its own handler either way.
  if command -v setsid > /dev/null 2>&1; then
    (trap - INT QUIT; exec setsid "$@") < /dev/null &
    g="-$!"
  else
    (trap - INT QUIT; exec "$@") < /dev/null &
    g="$!"
  fi
  p=$!
  trap 'kill -INT "$g" 2>/dev/null' USR1
  trap 'kill -TERM "$g" 2>/dev/null' USR2
  trap 'kill -KILL "$g" 2>/dev/null' ALRM
  echo "bpfdeck-remote: started" >&2
  exec 3<&0
  ( IFS= read -r _ <&3; kill -USR1 $$; sleep 5; kill -USR2 $$; sleep 2; kill -ALRM $$ ) > /dev/null 2>&1 &
  w=$!
  while :; do
    wait "$p"
    c=$?
    kill -0 "$p" 2>/dev/null || break
  done
  kill "$w" 2>/dev/null
  kill -KILL "$g" 2>/dev/null
  exit "$c"
}
bpfdeck_main; exit $?
