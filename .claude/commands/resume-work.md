Pick up context from a previous agent session that ran out of context.

Run these steps to understand the current state before doing anything else:

1. **Recent git history**: `git log --oneline -20` -- see what was committed
   recently and by whom
2. **Current branch state**: `git status` and `git diff --stat` -- see any
   uncommitted work in progress
3. **Open PRs**: `gh pr list` -- check for PRs that may need updates or are
   awaiting review
4. **Open issues**: `gh issue list` -- browse recent issues to understand what's
   being worked on
5. **Read conversation summary**: If the session includes a continuation
   summary, read it carefully for pending tasks, file locations, and decisions
   already made

After gathering context, summarize what you found and ask the user what to work
on next (or ask the user to continue the pending task if one is obvious).
