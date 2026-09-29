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
| 3 | Branches tree in Log | left pane: HEAD, Local, Remote, Tags | `log_branches.rs`: toggleable 220px pane, `/` folders, current branch starred; click selects the tip commit, double-click filters the Log by that branch; menu: Checkout (remote → `switch --track`, tag → detached), New Branch from…, Checkout and Rebase onto current, Show Diff with Working Tree, Rebase current onto, Merge into current, Rename…, Delete (with confirm; remote → `push --delete`). Later: Compare with current (commit lists both ways), Update/Push/Pull per branch, favorites, pane search | ✅ |
| 4 | Commit window gaps | Commit tool window | "Commit and Push…" in the commit split menu (`git::CommitAndPush`, pushes after a successful commit), Commit Message History (`git::ShowCommitMessageHistory`, last 50 messages via picker), `git::OpenCommitWindow` (opens panel + focuses message). JetBrains keymap (macOS): ⌘K commit window, ⌥⌘K commit and push, ⇧⌘K push (existing), ⌘T update (pull), ⌘9 Log, ⌃M message history, ⌘D diff in Log. Later: Push dialog listing outgoing commits (JetBrains shows it before pushing) | ✅ |
| 5 | Diff chunk arrows | `>>` / `<<` in diff gutter | `element.rs::layout_split_revert_buttons`: a `>` Revert button at the left edge of the right pane for every visible hunk in split diffs of editable buffers (reuses Restore); "N differences" in the single-file diff toolbar next to the existing prev/next arrows. Later: `<<` (copy right → left) only makes sense for the 3-pane merge tool (step 8); the counter in the multi-file Changes toolbar | ✅ |
| 6 | Stash gaps | Stash Changes / Unstash dialogs | `stash_dialogs.rs`: `git::StashChanges` (Git root, Current branch, Message, Keep index → Create Stash) and `git::UnstashChanges` (stash list, View/Drop/Clear with confirm, Pop stash, Reinstate index, As new branch → Apply/Pop Stash/Create Branch); both in the git panel's changes menu | ✅ |
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
| 2026-09-30 | 3 | Branches pane done; tree unit test, 30/30 git_graph tests pass |
| 2026-09-30 | 4 | Commit window gaps done; 71/71 git_panel tests pass |
| 2026-09-30 | 6 | Stash dialogs done (step 5 next; done out of order while the editor was mapped) |
| 2026-09-30 | 5 | Split diff revert arrows + differences counter; editor split tests 46/46 |
