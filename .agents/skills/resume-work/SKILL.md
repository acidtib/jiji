---
name: resume-work
description: Pick up context from a previous agent session that ran out of context. Review git history, branch state, open PRs and issues, and any continuation summary to understand the current state before starting new work. Use at the start of a session, when resuming after a compaction or long gap, or when the user asks to resume work or invokes /resume-work.
---

# resume-work

Pick up context from a previous agent session that ran out of context.
Run these steps before doing anything else:

1. Review recent git history: `git log --oneline -20` to see what was
   committed recently and by whom.
2. Check the current branch state: `git status` and `git diff --stat`
   to see uncommitted work in progress.
3. List open PRs: `gh pr list` to find PRs that may need updates or are
   awaiting review.
4. List open issues: `gh issue list` to understand what is being worked
   on.
5. Read the conversation summary: if the session includes a continuation
   summary, read it for pending tasks, file locations, and decisions
   already made.

After gathering context, summarize what you found and ask the user what
to work on next, or ask the user to continue the pending task if one is
obvious.
