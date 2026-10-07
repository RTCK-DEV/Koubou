#!/bin/sh
# parity_check.sh — verify every `cmd:` id cited in docs/parity.md exists
# in Session::command_ids (composer/src/commands.rs). Exit 1 on mismatch.
set -u
cd "$(dirname "$0")/.."

cited=$(grep -o 'cmd:[a-zA-Z.]*' docs/parity.md | sed 's/^cmd://' | sort -u)
known=$(awk '/pub fn command_ids/,/^    }/' composer/src/commands.rs | grep -o '"[a-zA-Z.]*"' | tr -d '"' | sort -u)

missing=0
for id in $cited; do
    echo "$known" | grep -qxF "$id" || { echo "parity: cited command not in Session::command_ids: $id"; missing=1; }
done

[ "$missing" -eq 0 ] && echo "parity: all cited commands exist ($(echo "$cited" | wc -w | tr -d ' ') ids)"
exit "$missing"
