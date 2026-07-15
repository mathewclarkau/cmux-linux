---
name: cmux-hotfix-race
description: Race N specialist agents on the same bug fix in cmux panes; first verified-passing commit wins, the rest are discarded
argument-hint: <describe the bug/failing test and how many racers you want (default 3)>
---

You are running the **hotfix race** pattern in **cmux**: dispatch several agents
at the identical bug/task, each in its own isolated copy of the repo, and adopt
whichever one finishes first with a **verified** fix. This is the IndyDevDan
cmux "race" pattern (`prompts/15-race-and-notify.md` and the 8-agent variant in
upstream cmux's demo guide), adapted to this fork's actual primitives — this
fork has no `cmux notify`/`jump-to-unread` event stream, so winner-detection is a
poll loop instead of a blocking event wait.

The user's request is:

$ARGUMENTS

Do not just describe a plan — actually stand up the race, poll for a winner,
verify it, and report back which racer won and what its diff was.

## 0. Preconditions

Run `env | grep '^CMUX_MUX_'`. You need `CMUX_MUX_SOCKET` (and ideally
`CMUX_MUX_SURFACE` if you want to split off of your own pane). If you're
running from OUTSIDE any cmux pane (a headless/Discord/CI agent-context),
use the `cmux-agent-spawn` pattern first to stand up a session and, if the
user wants to watch, a visible Ghostty window — see
`~/Projects/hermes/home/skills/software-development/cmux-agent-spawn/SKILL.md`
if available, or just start one directly:

```bash
cmux --headless --session <race-name> &
```

## 1. Why each racer needs its OWN isolated copy of the repo

**Do not** run all racers against the same working directory. Upstream cmux's
own race demo does this (all panes share `--cwd "$PWD"`) because its example
task ("find the bug, output a diff") never edits the working tree — but a
*hotfix* race, where each agent is told to actually FIX and commit, means N
concurrent agents writing to the same files. That's a guaranteed corruption
race, not a fair one. Give each racer its own git worktree instead:

```bash
REPO=/path/to/target/repo
RACE=race-$(date +%s)
cd "$REPO"
N=3   # or whatever the user asked for
for i in $(seq 1 "$N"); do
  git worktree add -q -b "racer-$i" "../${RACE}-w${i}" <base-branch-or-commit>
done
```

Clean these up after (see step 5) — `git worktree remove --force ...` for each,
then `git branch -D racer-1 racer-2 ...` if you don't want the losing branches
kept around.

## 2. Stand up one pane per racer and dispatch identically

```bash
cmux new-workspace --session <race-name> --name "Hotfix Race"
# split N-1 more panes off the first pane's id (from list-workspaces), one per racer
cmux split --session <race-name> --pane <pane-id> --dir right   # or down
```

Collect each pane's surface id from `list-workspaces`, then dispatch the
**identical** task to each, `cd`'d into its own worktree:

```bash
TASK='Run: <the exact failing command/test>. It will fail. Find and fix the
bug (name which files are off-limits, e.g. do not touch the test file). When
it passes, commit your fix with `git commit -am "fix"` and stop.'

cmux send --session <race-name> --surface <id> --text "cd <worktree-i> && claude -p '$TASK' --dangerously-skip-permissions
"
```

Repeat for every racer's surface, back to back, so they all start at
approximately the same time. Use `--dangerously-skip-permissions` (or the
equivalent for whichever harness) since these are one-shot, disposable
worktrees — never do this against a real working copy the user cares about.

**Multi-provider variant:** if the user wants a mixed-harness race (Claude
Code + `pi` with different models, matching upstream cmux's 8-agent demo),
swap the dispatched command per pane, e.g. `pi --provider openrouter --model
<slug> -p "$TASK"` for some racers — the win-detection in step 3 doesn't care
what produced the commit.

## 3. Poll for the first verified winner — do not just trust "I'm done"

The completion signal is **a passing verification in that racer's own
worktree**, not "the agent said it's done" (agents can be wrong, or a "fix"
commit can still fail the test). Poll each worktree's git log AND re-run the
actual check:

```bash
WINNER=""
for _ in $(seq 1 <timeout-iterations>); do
  for i in $(seq 1 "$N"); do
    W="../${RACE}-w${i}"
    if git -C "$W" log --oneline -1 2>/dev/null | grep -qi "fix"; then
      # a candidate commit exists — verify it actually passes before crowning it
      if (cd "$W" && <the exact verification command>); then
        WINNER="$i"
        break 2
      fi
    fi
  done
  sleep 3
done
```

Adjust the sleep/iteration count to the task's expected difficulty — a small
bounded bug fix is usually done within 30-90s; scale the timeout up for
harder tasks and tell the user you're doing so.

**Why verify instead of trusting the commit:** in a live run of this pattern,
multiple racers can finish with a `fix` commit around the same time — you
want the first one that's actually correct, not just the first one that
committed. If a racer's first commit doesn't pass, keep it in the running
(it might fix it in a follow-up turn) rather than disqualifying it outright,
unless the user's task said "one shot only."

## 4. Report the winner, don't silently pick

Show the user which racer won and its actual diff:

```bash
git -C "../${RACE}-w${WINNER}" log -p -1
```

If more than one racer also finished correctly (common — the same well-scoped
bug often gets the same fix from multiple agents/models), say so; it's useful
signal about how unambiguous the bug was, even though only the first-verified
one is adopted.

## 5. Close the losers and clean up

```bash
for i in $(seq 1 "$N"); do
  [ "$i" = "$WINNER" ] && continue
  cmux close-surface --session <race-name> --surface <that racer's surface id>
done
```

Leave the winner's pane open so the user can look at it, unless they said
otherwise. Once the user's done reviewing:

```bash
git worktree remove --force "../${RACE}-w1" "../${RACE}-w2" ...
git branch -D racer-1 racer-2 ...   # keep the winner's branch if the user wants to merge it
ps aux | grep "cmux --headless --session <race-name>" | grep -v grep | awk '{print $2}' | xargs -r kill  # only if you started a dedicated headless daemon for this
```

## Verified

This pattern (3 racers, isolated worktrees, poll-for-verified-winner, adopt +
discard) was run live end-to-end on 2026-07-15 against a deliberately
seeded off-by-one bug: all 3 racers converged on the same correct one-line
fix, racer 1 committed first (~42s), was crowned winner, its fix was
re-verified independently (test re-run: PASS), and the other two panes were
closed. See `~/Projects/hermes/home/skills/software-development/cmux-agent-spawn/SKILL.md`
and its verification reference for the launch-mechanics half of this (this
skill assumes that part already works).

## Related

- [[cmux-orchestration]] (sibling skill in this repo) — general pane/agent/
  browser-tab orchestration; this skill is a specific composition of it
  (fan-out identical task → poll → adopt-winner → discard-losers).
- `~/Projects/hermes/home/skills/software-development/cmux-agent-spawn/SKILL.md` —
  how to stand up a visible cmux session at all, if you're not already
  running inside one.
- `~/Projects/cmux-dan/prompts/15-race-and-notify.md` and
  `~/Projects/cmux-dan/guide/index.html` (search "Race eight agents") — the
  source pattern this was adapted from, upstream-cmux flavored (uses `cmux
  notify` + `jump-to-unread`, which this fork doesn't have — hence the poll
  loop here instead).
