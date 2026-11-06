# shellcheck shell=bash
# One test run at a time on this machine (sourced by ci-local.sh).
#
# take_lock DIR waits until it can create DIR, writes the pid and the program
# name of this shell into it and removes it again on exit. A lock whose pid is
# dead, or now belongs to another program, is left over from a killed run and
# is taken over. A lock without a pid file comes from an older script: it is
# never removed here, only reported once a minute.

# Seconds between two tries; the tests make it shorter.
LOCK_POLL=${LOCK_POLL:-5}

# Removes DIR if it still holds the dead pid $2 (another waiter may have taken
# it over in the meantime). DIR.break keeps two waiters from doing this at once.
lock_break() {
  mkdir "$1.break" 2>/dev/null || return 0
  if [ "$(cat "$1/pid" 2>/dev/null)" = "$2" ]; then
    echo "removing $1: pid $2 no longer runs the test that took it"
    rm -rf "$1"
  fi
  rmdir "$1.break"
}

take_lock() {
  local dir=$1 waited=0 pid comm
  until mkdir "$dir" 2>/dev/null; do
    pid=$(cat "$dir/pid" 2>/dev/null || true)
    comm=$(cat "$dir/comm" 2>/dev/null || true)
    if [ -n "$pid" ] && [ -n "$comm" ] && [ "$(ps -o comm= -p "$pid" 2>/dev/null)" != "$comm" ]; then
      lock_break "$dir" "$pid"
      # Removed, or taken over by another waiter: try again at once. A
      # DIR.break that stays (a waiter killed in lock_break) is waited for.
      [ -d "$dir.break" ] || continue
    fi
    if [ $((waited % 60)) -lt "$LOCK_POLL" ]; then
      local since
      since=$(date -r "$dir" +%H:%M 2>/dev/null || echo "?")
      if [ -d "$dir.break" ]; then
        echo "waiting for $dir.break (since $(date -r "$dir.break" +%H:%M 2>/dev/null || echo "?"); if no test runs, rmdir it)"
      elif [ -n "$pid" ]; then
        echo "waiting for $dir (held by pid $pid since $since)"
      else
        echo "waiting for $dir (held by an older script since $since; if no test runs, rmdir it)"
      fi
    fi
    sleep "$LOCK_POLL"
    waited=$((waited + LOCK_POLL))
  done
  # comm first: a waiter that sees the pid also finds the name to compare.
  ps -o comm= -p $$ > "$dir/comm"
  echo $$ > "$dir/pid"
  # shellcheck disable=SC2064 # the path is fixed now
  trap "rm -rf '$dir'" EXIT
}
