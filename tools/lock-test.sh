#!/usr/bin/env bash
# Checks tools/lock.sh on locks in a temporary directory (run by ci-local.sh).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
tmp=$(mktemp -d)
sleeper=
waiter=
cleanup() {
  for p in $sleeper $waiter; do kill "$p" 2>/dev/null && { wait "$p" 2>/dev/null || true; }; done
  rm -rf "$tmp"
}
trap cleanup EXIT
fail() { echo "lock test: $*"; exit 1; }

# Takes the lock in a child shell that exits at once; what it said is in $tmp/out.
# A child still waiting after 10 s is stopped and the test fails.
take() {
  LOCK_POLL=1 bash -c ". '$ROOT/tools/lock.sh'; take_lock '$1'; cat '$1/pid'" > "$tmp/out" 2>&1 &
  waiter=$!
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 "$waiter" 2>/dev/null || break
    sleep 1
  done
  if kill -0 "$waiter" 2>/dev/null; then
    fail "still waiting for $1: $(cat "$tmp/out")"
  fi
  waiter=
}
# Starts a child that waits for the lock in the background.
wait_for() {
  LOCK_POLL=1 bash -c ". '$ROOT/tools/lock.sh'; take_lock '$1'" > "$tmp/out" 2>&1 &
  waiter=$!
  sleep 2
  kill -0 "$waiter" 2>/dev/null || fail "took the lock $1: $(cat "$tmp/out")"
  kill "$waiter"
  wait "$waiter" 2>/dev/null || true
  waiter=
}

# A free lock is taken, holds the pid and is gone after exit.
take "$tmp/a"
[ -s "$tmp/out" ] && [ ! -e "$tmp/a" ] || fail "free lock: $(cat "$tmp/out")"

# The pid of a run that was killed: taken over.
sleep 0 &
dead=$!
wait "$dead"
mkdir "$tmp/b"
echo bash > "$tmp/b/comm"
echo "$dead" > "$tmp/b/pid"
take "$tmp/b"
grep -q "removing $tmp/b" "$tmp/out" || fail "dead pid: $(cat "$tmp/out")"

# A live holder is waited for, with a message.
sleep 30 &
sleeper=$!
mkdir "$tmp/c"
ps -o comm= -p "$sleeper" > "$tmp/c/comm"
echo "$sleeper" > "$tmp/c/pid"
wait_for "$tmp/c"
grep -q "held by pid $sleeper" "$tmp/out" || fail "live pid: $(cat "$tmp/out")"

# The same pid now running another program: taken over.
echo not-the-test > "$tmp/c/comm"
take "$tmp/c"
grep -q "removing $tmp/c" "$tmp/out" || fail "reused pid: $(cat "$tmp/out")"

# A dead pid behind a DIR.break left by a killed waiter: waited for with a
# message, not in a busy loop.
mkdir "$tmp/e" "$tmp/e.break"
echo bash > "$tmp/e/comm"
echo "$dead" > "$tmp/e/pid"
wait_for "$tmp/e"
grep -q "waiting for $tmp/e.break" "$tmp/out" || fail "left-over break: $(cat "$tmp/out")"
[ "$(wc -l < "$tmp/out")" -le 2 ] || fail "left-over break: $(cat "$tmp/out")"
rmdir "$tmp/e.break"
take "$tmp/e"
grep -q "removing $tmp/e" "$tmp/out" || fail "after the break: $(cat "$tmp/out")"

# No pid file (an older script): never removed.
mkdir "$tmp/d"
wait_for "$tmp/d"
grep -q "held by an older script" "$tmp/out" || fail "no pid file: $(cat "$tmp/out")"
[ -d "$tmp/d" ] || fail "no pid file: lock removed"

echo "lock test: OK"
