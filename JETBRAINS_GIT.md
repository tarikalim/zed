# JetBrains Git UI for Zed — plan

Fork: `tarikalim/zed`, branch `jetbrains-git`. Goal: WebStorm/GoLand Git tool window behaviour in Zed, one feature per step.

## Why a fork, not an extension

Zed extensions (WASM) cannot draw UI: the API only covers languages, themes, icons, snippets, slash commands, MCP servers and debuggers. So this lives in the fork. To keep weekly rebases onto upstream cheap:

- New views go in new files inside `crates/git_ui` (e.g. `log_actions.rs`); new files never conflict on rebase, and a separate crate would need to re-export half of `git_ui`.
- Existing files get only hook lines (a menu entry, an action registration).
- Missing git commands are added the upstream way (`GitRepository` trait → `RealGitRepository` → `Repository` in `git_store.rs` → `FakeGitRepository`), so they could be sent upstream.
- New write actions are local-only (hidden when `project.is_via_collab()`); proto messages for remote/collab come later, only if needed.

## What Zed already has (no work)

Commit graph with search and details pane · git panel with per-file checkboxes, hunk/line staging, amend, tree view · side-by-side/unified diff (`ToggleSplitDiff`) · inline Ours/Theirs/Both conflict buttons · stash list/apply/pop/drop/view · worktrees · branch popup in the title bar · blame · file history.

## Steps

Each step: build → run the fork → try it in the real app → show Tarık → next step only after approval. Status: ⬜ todo · 🟨 in progress · ✅ done.

| # | Feature | JetBrains reference | Work | Status |
|---|---|---|---|---|
| 0 | Fork builds and runs | — | `cargo build -p zed --features gpui_platform/runtime_shaders` (no Xcode), run script, rebase-on-upstream script | ✅ |
| 1 | Log context menu | right-click a commit in Log | Checkout Revision, New Branch…, New Tag…, Cherry-Pick, Revert Commit, Reset Current Branch to Here… (Soft/Mixed/Hard/Keep), Compare with Local, Copy Revision Number, Undo Commit (HEAD only). Backend: `CommitOperation::{CherryPick, Revert, Checkout, CreateTag, CreateBranch}`, `ResetMode::{Hard, Keep}`. Deferred to step 9: Edit Commit Message, Fixup, Squash Into, Drop Commits, Interactively Rebase, Push All up to Here. Later: Create Patch, Show Repository at Revision, Go to Parent/Child | ✅ |
| 2 | Log filters toolbar | Branch ▾ · User ▾ · Date ▾ · Paths ▾ | `log_filters.rs`: Branch (HEAD/Local/Remote), User (me + authors by commit count), Date (Last 24 hours, Last 7 days, Select… period dialog), Paths (Select Folders… native picker), × to clear. Backend: `LogSource::Filtered(LogFilter)` incl. proto, `git_output` for reads. Known gap: user/date/path filters show a linear list (git does not rewrite parents), JetBrains keeps graph lines; filters are not persisted across restarts | ✅ |
| 3 | Branches tree in Log | left pane: HEAD, Local, Remote, Tags | Tree beside the graph; per branch: Checkout, New Branch from…, Checkout and Rebase onto Current, Compare with Current, Show Diff with Working Tree, Rebase Current onto Selected, Merge into Current, Rename…, Delete. Backend: `merge`, `rebase`, `list_tags`, `delete_tag` | ⬜ |
| 4 | Commit window gaps | Commit tool window | Commit and Push…, commit message history (↑ in message box), JetBrains keymap (⌘K commit, ⇧⌘K push, ⌘T update project, ⌘9 Git window) | ⬜ |
| 5 | Diff chunk arrows | `>>` / `<<` in diff gutter | Per-chunk revert/accept arrows in split diff, "N differences" counter, next/prev difference | ⬜ |
| 6 | Stash gaps | Stash Changes / Unstash dialogs | Keep index, Clear, Pop vs Apply toggle, "As new branch" on unstash | ⬜ |
| 7 | Conflicts dialog | Conflicts: file list + Accept Yours / Accept Theirs / Merge… | Modal listing conflicted files; accept-side via `git checkout --ours/--theirs` + stage | ⬜ |
| 8 | 3-pane merge tool | Merge Revisions window | Left (yours) · Result · Right (theirs), per-chunk apply arrows, Apply button | ⬜ |
| 9 | Interactive rebase | Interactively Rebase from Here… | Table of commits: pick/reword/edit/squash/fixup/drop, drag to reorder; drives `git rebase -i` with a generated todo via `GIT_SEQUENCE_EDITOR` | ⬜ |
| 10 | Changelists | Changes view: named changelists | Named groups of changed files in the git panel, commit one changelist; stored per project | ⬜ |
| 11 | Shelf | Shelve Changes / Unshelve | Save selected changes as patches under `.idea`-like dir, list, preview, unshelve | ⬜ |
| 12 | Pull Requests window | GitHub: Pull Requests tool window | List/filter PRs, details, files + diff, review comments, checkout PR branch. GitHub API with the `gh` token | ⬜ |
| 13 | Layout and look | New UI Git tool window | Bottom-docked Git window with Log/Console tabs, JetBrains icons/colours for file status, final keymap pass | ⬜ |

## Log

| Date | Step | Note |
|---|---|---|
| 2026-09-29 | 0 | Forked `zed-industries/zed` → `tarikalim/zed`, branch `jetbrains-git`; gap analysis done; first build running |
| 2026-09-29 | 1 | Backend + `git_ui/src/log_actions.rs` (menu entries, Git Reset dialog, Create New Branch/Tag dialogs) written; waiting on first build |
| 2026-09-29 | 1 | Builds; `test_commit_operations_and_reset_modes` passes on real git (tag, branch, revert, reset Hard/Keep, cherry-pick, checkout). Fork launched on a scratch repo; UI check waiting on Tarık |
| 2026-09-30 | 1 | Committed and pushed (3c8d450003) |
| 2026-09-30 | 2 | Log filters done; real-git test for filtered log, 29/29 git_graph tests pass |
