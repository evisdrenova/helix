- log
  - show diffs in detail view maybe in accordion or something on hover, easily jump to new files
  - vi text input at the bottom to search for stuff
  - search through history ("find when we changed the authentication flow")
- branch
  - better branch UI with TUI
- diff
  - better UI
- git status
  - better UI
- agent
  - `agent spawn <task-name>` - creates isolated worktree with automatic branch naming
  - `agent status` - TUI showing all active agent worktrees, their progress, conflcits, token usage
  - `agent pause/resume` - checkpoint agent state, swap between agent contexts
  - Automatic snapshots before agent operations with dry run mode with preview for all chnges
  - Agent permission system (read-only, can-commit, can-push)
  - Agent "commits" include machine-readable metadata about intent
  - Automatic rebase orchestration across multiple agent branches
- bisect
  - a really easy way to see which commit broke main using AI
- merge conflicts
  - "take theirs but keep my error handling" - natural language merge
- natural language commands 
  - git "show me all commits that touched authentication in the last month"
  - git "create a branch from production, cherry-pick the auth fixes"
- automatic branch clean up
  - detect merged branches, clean up
  - git compress - intelligently squash WIP commits while preserving meaningful history
performance
  - everything is incremental 
  - CACHE
monorepo support
  - sparse worktrees by default for agents
  - path scoped variables - agent only sees erelvant code


The VCS needs to become a coordination layer, not just a storage layer.