#!/usr/bin/env bash
# Spike S1 (PLAN.md, P0): can GitHub hold GANG's shared state?
#
# Against a throwaway GitHub repository, this checks that:
#   1. Parentless commits can be pushed to a branch and to custom refs under
#      refs/gang/.
#   2. Concurrent writers never lose an update, comparing three layouts:
#      a. presence on per-user refs (refs/gang/presence/<user>): no contention;
#      b. presence on one shared branch (gang-presence), every writer replacing
#         only its own .gang/<user>.json, at a realistic rate (one update every
#         2-4 s per writer) with compare-and-swap;
#      c. locks on one shared ref (refs/gang/locks), every writer appending to
#         the same locks.txt as fast as it can (worst case) with
#         compare-and-swap and exponential backoff.
#   3. How long ls-remote, fetch and push take.
#   4. Which kinds of pushes start GitHub Actions runs. The pushes are made
#      here; check the runs afterwards with the printed `gh run list` command.
#
# Usage: spikes/s1_shared_refs.sh <https-url-of-throwaway-repo> [work-dir]
# Env:   WRITERS (4), UPDATES (20)
#
# Written for bash 3.2 (macOS) and later.
set -euo pipefail

URL=$1
WORK=${2:-$(mktemp -d)}
WRITERS=${WRITERS:-4}
UPDATES=${UPDATES:-20}
MAX_RETRIES=60
TAB=$'\t'

WORKFLOW='name: spike
on: push
jobs:
  noop:
    runs-on: ubuntu-latest
    steps:
      - run: echo "triggered by ${{ github.ref }}"
'

now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
elapsed() { perl -e "printf '%.2f', $2 - $1"; }
# Sleeps a random time below $1 seconds.
jitter() { perl -e "select(undef, undef, undef, rand($1))"; }

clone() {
  git clone -q "$URL" "$WORK/$1" 2>/dev/null
  git -C "$WORK/$1" config user.name "spike-$1"
  git -C "$WORK/$1" config user.email "spike@example.com"
}

delete_remote_ref() { git -C "$WORK/seed" push -q origin ":$1" 2>/dev/null || true; }

# Prints the id of a tree that holds .github/workflows/spike.yml.
workflow_tree() {
  local repo=$1 blob workflows github
  blob=$(printf '%s' "$WORKFLOW" | git -C "$repo" hash-object -w --stdin)
  workflows=$(printf '100644 blob %s\tspike.yml\n' "$blob" | git -C "$repo" mktree)
  github=$(printf '040000 tree %s\tworkflows\n' "$workflows" | git -C "$repo" mktree)
  printf '040000 tree %s\t.github\n' "$github" | git -C "$repo" mktree
}

# Prints the id of a tree holding only .gang/<user>.json with the given seq.
presence_tree() {
  local repo=$1 user=$2 value=$3 blob sub
  blob=$(printf '{"user":"%s","seq":%s}\n' "$user" "$value" | git -C "$repo" hash-object -w --stdin)
  sub=$(printf '100644 blob %s\t%s.json\n' "$blob" "$user" | git -C "$repo" mktree)
  printf '040000 tree %s\t.gang\n' "$sub" | git -C "$repo" mktree
}

# Per-user ref: no shared state, so a plain force push. Prints 0 retries.
own_ref_update() {
  local repo=$1 user=$2 value=$3 root commit
  root=$(presence_tree "$repo" "$user" "$value")
  commit=$(printf 'presence: %s %s [skip ci]\n' "$user" "$value" | git -C "$repo" commit-tree "$root")
  git -C "$repo" push -q --force origin "$commit:refs/gang/presence/$user" 2>>"$repo.push-errors"
  LAST_COMMIT=$commit
  echo 0
}

# Compare-and-swap update of a shared ref, retried with exponential backoff
# until it wins. Prints the number of retries.
#   kind=presence: replace .gang/<user>.json with {"user":…,"seq":<value>}
#   kind=locks:    append "<user> <value>" to locks.txt
cas_update() {
  local repo=$1 ref=$2 kind=$3 user=$4 value=$5 retries=0 backoff=0.2
  local tip blob others sub root current commit
  while :; do
    git -C "$repo" update-ref -d refs/spike/tip 2>/dev/null || true
    if git -C "$repo" fetch -q origin "+$ref:refs/spike/tip" 2>/dev/null; then
      tip=$(git -C "$repo" rev-parse refs/spike/tip)
    else
      tip=""
    fi

    if [ "$kind" = presence ]; then
      blob=$(printf '{"user":"%s","seq":%s}\n' "$user" "$value" | git -C "$repo" hash-object -w --stdin)
      others=""
      if [ -n "$tip" ]; then
        others=$(git -C "$repo" ls-tree "$tip:.gang" 2>/dev/null | grep -v "${TAB}${user}\.json\$" || true)
      fi
      sub=$({ if [ -n "$others" ]; then printf '%s\n' "$others"; fi
              printf '100644 blob %s\t%s.json\n' "$blob" "$user"; } | git -C "$repo" mktree)
      root=$(printf '040000 tree %s\t.gang\n' "$sub" | git -C "$repo" mktree)
    else
      current=""
      if [ -n "$tip" ]; then
        current=$(git -C "$repo" cat-file -p "$tip:locks.txt")
      fi
      blob=$({ if [ -n "$current" ]; then printf '%s\n' "$current"; fi
               printf '%s %s\n' "$user" "$value"; } | git -C "$repo" hash-object -w --stdin)
      root=$(printf '100644 blob %s\tlocks.txt\n' "$blob" | git -C "$repo" mktree)
    fi

    commit=$(printf 'spike: %s %s %s [skip ci]\n' "$kind" "$user" "$value" | git -C "$repo" commit-tree "$root")
    if git -C "$repo" push -q --force-with-lease="$ref:$tip" origin "$commit:$ref" 2>>"$repo.push-errors"; then
      LAST_COMMIT=$commit
      echo "$retries"
      return 0
    fi
    retries=$((retries + 1))
    if [ "$retries" -ge "$MAX_RETRIES" ]; then
      echo "$user gave up after $retries retries" >&2
      echo "$retries"
      return 1
    fi
    jitter "$backoff"
    backoff=$(perl -e "my \$b = $backoff * 2; print \$b > 3 ? 3 : \$b")
  done
}

# One writer: UPDATES updates, `pause` seconds (max, random) between them.
# Writes "<retries> <failures>" to $WORK/<user>.<label>.result.
writer() {
  local user=$1 label=$2 pause=$3 total=0 failures=0 r i
  shift 3
  for i in $(seq 1 "$UPDATES"); do
    if r=$("$@" "$WORK/$user" "$user" "$i"); then :; else failures=$((failures + 1)); fi
    total=$((total + ${r:-0}))
    if [ "$pause" != 0 ]; then jitter "$pause"; fi
  done
  echo "$total $failures" > "$WORK/$user.$label.result"
}

# Runs WRITERS writers in parallel and prints a summary line.
contend() {
  local label=$1 pause=$2 start end u retries=0 failures=0 r f
  shift 2
  start=$(now)
  for u in $(seq 1 "$WRITERS"); do writer "w$u" "$label" "$pause" "$@" & done
  wait
  end=$(now)
  for u in $(seq 1 "$WRITERS"); do
    read -r r f < "$WORK/w$u.$label.result"
    retries=$((retries + r))
    failures=$((failures + f))
  done
  echo "$label: $WRITERS writers x $UPDATES updates in $(elapsed "$start" "$end") s," \
       "$retries retries, $failures gave up"
}

presence_own() { own_ref_update "$1" "$2" "$3"; }
presence_shared() { cas_update "$1" refs/heads/gang-presence presence "$2" "$3"; }
locks_shared() { cas_update "$1" refs/gang/locks locks "$2" "$3"; }

# Prints "ok" or what was lost.
check_presence() {
  local prefix=$1 u seq_now bad=""
  for u in $(seq 1 "$WRITERS"); do
    seq_now=$(git -C "$WORK/reader" cat-file -p "$prefix$u:.gang/w$u.json" 2>/dev/null \
              | sed 's/.*"seq":\([0-9]*\).*/\1/')
    if [ "$seq_now" != "$UPDATES" ]; then bad="$bad w$u=${seq_now:-missing}"; fi
  done
  echo "${bad:-ok}"
}

echo "== setup ($WORK)"
mkdir -p "$WORK"
clone seed
if ! git -C "$WORK/seed" rev-parse -q --verify HEAD >/dev/null; then
  mkdir -p "$WORK/seed/.github/workflows"
  printf '%s' "$WORKFLOW" > "$WORK/seed/.github/workflows/spike.yml"
  git -C "$WORK/seed" add .
  git -C "$WORK/seed" commit -q -m "Add spike workflow"
  git -C "$WORK/seed" push -q origin HEAD:refs/heads/main
  echo "main pushed with a workflow on every push (expect 1 run: main)"
fi
for u in $(seq 1 "$WRITERS"); do clone "w$u"; done
clone reader
for ref in refs/heads/gang-presence refs/gang/locks; do delete_remote_ref "$ref"; done
for u in $(seq 1 "$WRITERS"); do delete_remote_ref "refs/gang/presence/w$u"; done

echo "== 1. pushes of parentless commits"
presence_own "$WORK/w1" w1 0 >/dev/null && echo "custom ref refs/gang/presence/w1: ok"
r=$(presence_shared "$WORK/w1" w1 0) && echo "branch gang-presence: ok ($r retries)"
r=$(locks_shared "$WORK/w1" w1 0) && echo "custom ref refs/gang/locks: ok ($r retries)"
delete_remote_ref refs/gang/locks

echo "== 2. concurrent writers"
contend presence-per-user-refs 0 presence_own
contend presence-shared-branch 4 presence_shared
contend locks-shared-ref 0 locks_shared

echo "== 2b. verification"
git -C "$WORK/reader" fetch -q origin \
  '+refs/gang/presence/*:refs/spike/presence/*' \
  +refs/heads/gang-presence:refs/spike/shared \
  +refs/gang/locks:refs/spike/locks
echo "presence per-user refs: $(check_presence refs/spike/presence/w)"
bad=""
for u in $(seq 1 "$WRITERS"); do
  seq_now=$(git -C "$WORK/reader" cat-file -p "refs/spike/shared:.gang/w$u.json" 2>/dev/null \
            | sed 's/.*"seq":\([0-9]*\).*/\1/')
  if [ "$seq_now" != "$UPDATES" ]; then bad="$bad w$u=${seq_now:-missing}"; fi
done
echo "presence shared branch: ${bad:-ok}"
locks=$(git -C "$WORK/reader" cat-file -p refs/spike/locks:locks.txt)
lines=$(printf '%s\n' "$locks" | wc -l | tr -d ' ')
dupes=$(printf '%s\n' "$locks" | sort | uniq -d | wc -l | tr -d ' ')
echo "locks shared ref: $lines lines (expected $((WRITERS * UPDATES))), $dupes duplicated"
for ref in refs/spike/presence/w1 refs/spike/shared refs/spike/locks; do
  parents=$(git -C "$WORK/reader" rev-list --parents -n 1 "$ref" | wc -w | tr -d ' ')
  echo "$ref tip has $((parents - 1)) parents"
done
errors=$(cat "$WORK"/w*.push-errors 2>/dev/null | grep -vE '^ ! \[rejected\]|^error: failed to push|^To |stale info|cannot lock ref' | sort | uniq -c | sort -rn | head -5 || true)
echo "push errors other than lease rejections: ${errors:-none}"

echo "== 3. timings (average of 5)"
r=$WORK/reader
t0=$(now); for i in 1 2 3 4 5; do git -C "$r" ls-remote origin 'refs/gang/*' gang-presence >/dev/null; done; t1=$(now)
echo "ls-remote:                    $(perl -e "printf '%.2f', ($t1 - $t0) / 5") s"
t0=$(now); for i in 1 2 3 4 5; do git -C "$r" fetch -q origin '+refs/gang/presence/*:refs/spike/presence/*'; done; t1=$(now)
echo "fetch (no change):            $(perl -e "printf '%.2f', ($t1 - $t0) / 5") s"
t0=$(now); for i in 1 2 3 4 5; do presence_own "$WORK/w1" w1 "t$i" >/dev/null; done; t1=$(now)
echo "per-user ref update (push):   $(perl -e "printf '%.2f', ($t1 - $t0) / 5") s"
t0=$(now); for i in 1 2 3 4 5; do presence_shared "$WORK/w1" w1 "t$i" >/dev/null; done; t1=$(now)
echo "shared ref update (fetch+push): $(perl -e "printf '%.2f', ($t1 - $t0) / 5") s"
visible=0
for i in 1 2 3 4 5; do
  presence_own "$WORK/w2" w2 "v$i" >/dev/null
  seen=$(git -C "$r" ls-remote origin refs/gang/presence/w2 | cut -f1)
  if [ "$seen" = "$LAST_COMMIT" ]; then visible=$((visible + 1)); fi
done
echo "visible to another clone right after push: $visible/5"

echo "== 4. Actions triggers"
s=$WORK/seed
wf=$(workflow_tree "$s")
plain=$(printf '100644 blob %s\tnote.txt\n' "$(echo note | git -C "$s" hash-object -w --stdin)" | git -C "$s" mktree)
push_case() {
  local ref=$1 tree=$2 message=$3 commit
  commit=$(printf '%s\n' "$message" | git -C "$s" commit-tree "$tree")
  git -C "$s" push -q --force origin "$commit:$ref"
  echo "$ref  $commit  \"$message\""
}
push_case refs/heads/spike-a "$plain" "a: branch, no workflow in tree, no skip"
push_case refs/heads/spike-b "$wf" "b: branch, workflow in tree [skip ci]"
push_case refs/heads/spike-c "$wf" "c: branch, workflow in tree, no skip (control)"
push_case refs/gang/spike-d "$wf" "d: custom ref, workflow in tree, no skip"
echo "expected runs: main and spike-c only. Check after a minute with:"
echo "  gh run list --repo <owner/repo> --json headBranch,headSha,event,displayTitle,status"
