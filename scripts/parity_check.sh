#!/bin/sh
# parity_check.sh — verify every `cmd:` id cited in docs/parity.md exists
# in Session::command_ids (composer/src/commands.rs). Exit 1 on mismatch.
set -u
cd "$(dirname "$0")/.."

cited=$(grep -o 'cmd:[a-zA-Z.]*' docs/parity.md | sed 's/^cmd://' | sort -u)
# Session::command_ids aggregates the domain sessions' lists: base ids in
# composer/src/commands.rs, tl.* in motion/src/session.rs, pg.* in
# pages/src/session.rs — extract literals from all three.
known=$(awk '/pub fn command_ids/,/^    }/' composer/src/commands.rs motion/src/session.rs pages/src/session.rs | grep -o '"[a-zA-Z.]*"' | tr -d '"' | sort -u)

missing=0
for id in $cited; do
    echo "$known" | grep -qxF "$id" || { echo "parity: cited command not in Session::command_ids: $id"; missing=1; }
done

[ "$missing" -eq 0 ] && echo "parity: all cited commands exist ($(echo "$cited" | wc -w | tr -d ' ') ids)"
exit "$missing"
