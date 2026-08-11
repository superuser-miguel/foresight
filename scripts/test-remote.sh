#!/usr/bin/env bash
# test-remote.sh — a throwaway SSH server for exercising remote sync by hand.
#
# Remote sync is the one feature that cannot be tried without a second machine,
# which is exactly why it is the one most likely to ship untested. This stands
# up a real sshd on 127.0.0.1:2222 that the app can push to and pull from, so
# the UI can be driven against a live remote with nothing but a laptop.
#
# It is deliberately unprivileged and self-contained:
#   * the host key is generated fresh here and thrown away on stop;
#   * authorized_keys is a PUBLIC key already loaded in your ssh-agent, so the
#     agent is never modified — it just signs a challenge for this local server;
#   * your real ~/.ssh and the system sshd are never read or touched.
#
# Usage:
#   ./scripts/test-remote.sh start    # start it, print what to type in the app
#   ./scripts/test-remote.sh stop     # kill it, remove its files and its trust
#   ./scripts/test-remote.sh rekey    # swap the host key, to see the app refuse
set -euo pipefail

RIG="${TMPDIR:-/tmp}/foresight-test-remote"
PORT=2222
APP_CONFIG="$HOME/.var/app/io.github.superuser_miguel.Foresight/config/foresight"

start() {
  command -v /usr/sbin/sshd >/dev/null || { echo "sshd not found (install openssh-server)"; exit 1; }
  if ! ssh-add -l >/dev/null 2>&1; then
    echo "Your ssh-agent has no keys — the app authenticates through the agent,"
    echo "so add one first:  ssh-add ~/.ssh/id_ed25519"
    exit 1
  fi

  stop_quiet
  mkdir -p "$RIG/inbox" "$RIG/outbox"

  # A small tree to pull, so the remote has something worth fetching.
  echo "hello from the test remote" > "$RIG/outbox/readme.txt"
  mkdir -p "$RIG/outbox/photos"
  head -c 50000 /dev/urandom > "$RIG/outbox/photos/image.bin"
  echo "a second file" > "$RIG/outbox/photos/notes.txt"

  ssh-keygen -q -t ed25519 -N '' -f "$RIG/host_ed25519" -C foresight-test-remote
  ssh-add -L | head -1 > "$RIG/authorized_keys"
  chmod 600 "$RIG/authorized_keys"

  cat > "$RIG/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $RIG/host_ed25519
AuthorizedKeysFile $RIG/authorized_keys
PidFile $RIG/sshd.pid
StrictModes no
UsePAM no
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
Subsystem sftp internal-sftp
EOF

  /usr/sbin/sshd -f "$RIG/sshd_config"
  sleep 1
  [ -s "$RIG/sshd.pid" ] || { echo "sshd failed to start"; exit 1; }

  cat <<EOF

  Test remote is up (pid $(cat "$RIG/sshd.pid")).

  In Foresight, the remote button (the server icon) beside Sources or
  Destination takes these:

      User   $USER
      Host   127.0.0.1
      Port   $PORT
      Path   $RIG/inbox      (to push into)
             $RIG/outbox     (to pull from)

  Its fingerprint — this is what the trust dialog must show you:

$(ssh-keygen -lf "$RIG/host_ed25519.pub" | sed 's/^/      /')

  Watch what lands:   watch -n1 ls -R $RIG/inbox
  Swap the host key:  $0 rekey     (the app must then refuse to connect)
  Finished:           $0 stop

EOF
}

rekey() {
  [ -s "$RIG/sshd.pid" ] || { echo "not running — start it first"; exit 1; }
  pkill -F "$RIG/sshd.pid" 2>/dev/null || true
  sleep 1
  rm -f "$RIG/host_ed25519" "$RIG/host_ed25519.pub"
  ssh-keygen -q -t ed25519 -N '' -f "$RIG/host_ed25519" -C foresight-test-remote-impostor
  /usr/sbin/sshd -f "$RIG/sshd_config"
  sleep 1
  echo "Host key swapped. The same job should now fail, and the log should say"
  echo "REMOTE HOST IDENTIFICATION HAS CHANGED — not merely that it failed."
}

stop_quiet() {
  [ -s "$RIG/sshd.pid" ] && pkill -F "$RIG/sshd.pid" 2>/dev/null || true
  rm -rf "$RIG"
}

stop() {
  stop_quiet
  # Drop the trust this rig earned, so a later run is a first contact again and
  # a stale localhost key can never be mistaken for a real one.
  if [ -f "$APP_CONFIG/known_hosts" ]; then
    ssh-keygen -R "[127.0.0.1]:$PORT" -f "$APP_CONFIG/known_hosts" >/dev/null 2>&1 || true
    rm -f "$APP_CONFIG/known_hosts.old"
  fi
  echo "Test remote stopped, files removed, its host key untrusted again."
}

case "${1:-}" in
  start) start ;;
  stop)  stop ;;
  rekey) rekey ;;
  *) echo "usage: $0 {start|stop|rekey}"; exit 2 ;;
esac
