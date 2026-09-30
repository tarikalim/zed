# JetBrains Git UI for Zed

Fork: `tarikalim/zed`, branch `jetbrains-git`. Goal: WebStorm/GoLand Git tool window behaviour in Zed, one feature per step.

## Why a fork, not an extension

Zed extensions (WASM) cannot draw UI: the API only covers languages, themes, icons, snippets, slash commands, MCP servers and debuggers. So this lives in the fork. To keep weekly rebases onto upstream cheap:

- New views go in new files inside `crates/git_ui` (e.g. `log_actions.rs`); new files never conflict on rebase, and a separate crate would need to re-export half of `git_ui`.
- Existing files get only hook lines (a menu entry, an action registration).
- Missing git commands are added the upstream way (`GitRepository` trait → `RealGitRepository` → `Repository` in `git_store.rs` → `FakeGitRepository`), so they could be sent upstream.
- New write actions are local-only (hidden when `project.is_via_collab()`); proto messages for remote/collab come later, only if needed.

## What Zed already has (no work)

Commit graph with search and details pane · git panel with per-file checkboxes, hunk/line staging, amend, tree view · side-by-side/unified diff (`ToggleSplitDiff`) · inline Ours/Theirs/Both conflict buttons · stash list/apply/pop/drop/view · worktrees · branch popup in the title bar · blame · file history.

## Install and update

- Build and install: `script/jetbrains-git-install` builds a release binary (no Xcode needed, `gpui_platform/runtime_shaders`), installs `~/Applications/Zed JetBrains.app` next to the stock Zed and links the CLI `~/.local/bin/zed-jb`. Open a project with `zed-jb <path>`.
- Update to the latest upstream Zed: `script/jetbrains-git-sync` (rebases `jetbrains-git` onto `upstream/main`, pushes with `--force-with-lease`), then `script/jetbrains-git-install`.
- Settings are shared with the stock Zed (`~/.config/zed/settings.json`); use `"base_keymap": "JetBrains"`.
- Only `gh` needs an account (Pull Requests view): `gh auth login`.
- Theme: `jetbrains/islands-iterm.json` is the WebStorm "Islands iTerm" theme (editor colors from the WebStorm scheme, terminal from the iTerm2 dark profile). Install with `cp jetbrains/islands-iterm.json ~/.config/zed/themes/` and set `"theme": { "dark": "Islands iTerm" }`; editor font `"buffer_font_family": "JetBrains Mono"`.

## Where things are

| JetBrains | In this fork |
|---|---|
| Git tool window → Log | ⌘9 (bottom dock "Git" panel), or command palette "git graph: open" as a tab |
| Log filters Branch / User / Date / Paths | Next to the Log search field |
| Branches tree | Left of the Log; branch icon button toggles it |
| Log context menu (cherry-pick, reset, rebase…) | Right-click a commit |
| Commit tool window | ⌘K (focuses the commit message) or ⌘0; changelists, "Unversioned Files", checkboxes |
| Commit and Push / message history | Commit split button menu, ⌥⌘K / ⌃M in the message box |
| Diff viewer | Click a file; side-by-side with a revert arrow per chunk |
| Staged / unstaged diff | File right-click → Staged Changes / Unstaged Changes; or Group By → Staged & Unstaged (JetBrains "Enable staging area") |
| Branch widget | Title bar, top-left (`⎇ main ⌄`); ⌘⇧` also opens it |
| Stash / Unstash | Git panel `…` menu → Stash Changes… / Unstash Changes… |
| Shelf | Git panel "Shelf" tab; Shelve Changes… on files and changelists |
| Conflicts / Merge Revisions | Opens after a conflicting merge/rebase/cherry-pick; "Resolve" on the Conflicts header |
| Interactive rebase | Log right-click → Interactively Rebase from Here…; Continue/Abort bar in the git panel |
| Pull Requests | Command palette "git: open pull requests" |
| Search Everywhere (Shift Shift) | `crates/search_everywhere`: tabs All · Classes · Files · Symbols · Actions · Text; ⇥ / ⇧⇥ switch tabs, pressing the same shortcut again moves to the next tab |
| Move a tool window (Move To) | Right-click the panel's icon in the status bar → Dock Left / Right / Bottom (no drag and drop) |

## Keymap (JetBrains base keymap, macOS)

⌘K commit window · ⌥⌘K commit and push · ⇧⌘K push · ⌘T update project (pull) · ⌘9 Git Log · ⌘0 git panel · ⌃M commit message history · ⌘D diff in the Log. Search Everywhere: Shift Shift All · ⌘O Classes · ⇧⌘O Files · ⌥⌘O Symbols · ⇧⌘A Actions. General: ⇧⌘F find in files · ⇧⌘R replace in files · ⌘E file finder.

## Known differences from JetBrains

- User, date and path Log filters show a linear list without graph lines (git does not rewrite parents for them).
- Merge tool: per-chunk controls are in the Result pane, not as arrows in the side panes.
- Pull Requests is a tab, without per-file inline review comments; no Console tab in the Git tool window.
- Tool windows move by right-click, not drag and drop; Project/Outline panels only dock left or right.
- New write actions are local-only (disabled in remote/SSH projects).

## Steps

Each step: build → run the fork → try it in the real app → show Tarık → next step only after approval. Status: ⬜ todo · 🟨 in progress · ✅ done.

| # | Feature | JetBrains reference | Work | Status |
|---|---|---|---|---|
| 0 | Fork builds and runs | — | `cargo build -p zed --features gpui_platform/runtime_shaders` (no Xcode), run script, rebase-on-upstream script | ✅ |
| 1 | Log context menu | right-click a commit in Log | Checkout Revision, New Branch…, New Tag…, Cherry-Pick, Revert Commit, Reset Current Branch to Here… (Soft/Mixed/Hard/Keep), Compare with Local, Copy Revision Number, Undo Commit (HEAD only). Backend: `CommitOperation::{CherryPick, Revert, Checkout, CreateTag, CreateBranch}`, `ResetMode::{Hard, Keep}`. Deferred to step 9: Edit Commit Message, Fixup, Squash Into, Drop Commits, Interactively Rebase, Push All up to Here. Later: Create Patch, Show Repository at Revision, Go to Parent/Child | ✅ |
| 2 | Log filters toolbar | Branch ▾ · User ▾ · Date ▾ · Paths ▾ | `log_filters.rs`: Branch (HEAD/Local/Remote), User (me + authors by commit count), Date (Last 24 hours, Last 7 days, Select… period dialog), Paths (Select Folders… native picker), × to clear. Backend: `LogSource::Filtered(LogFilter)` incl. proto, `git_output` for reads. Known gap: user/date/path filters show a linear list (git does not rewrite parents), JetBrains keeps graph lines; filters are not persisted across restarts | ✅ |
| 3 | Branches tree in Log | left pane: HEAD, Local, Remote, Tags | `log_branches.rs`: toggleable 220px pane, `/` folders, current branch starred; click selects the tip commit, double-click filters the Log by that branch; menu: Checkout (remote → `switch --track`, tag → detached), New Branch from…, Checkout and Rebase onto current, Show Diff with Working Tree, Rebase current onto, Merge into current, Rename…, Delete (with confirm; remote → `push --delete`). Compare with '<current>' (Log filtered to `current...branch`, both directions). Later: Update/Push/Pull per branch, favorites, pane search | ✅ |
| 4 | Commit window gaps | Commit tool window | "Commit and Push…" in the commit split menu (`git::CommitAndPush`, pushes after a successful commit), Commit Message History (`git::ShowCommitMessageHistory`, last 50 messages via picker), `git::OpenCommitWindow` (opens panel + focuses message). JetBrains keymap (macOS): ⌘K commit window, ⌥⌘K commit and push, ⇧⌘K push (existing), ⌘T update (pull), ⌘9 Log, ⌃M message history, ⌘D diff in Log. Later: Push dialog listing outgoing commits (JetBrains shows it before pushing) | ✅ |
| 5 | Diff chunk arrows | `>>` / `<<` in diff gutter | `element.rs::layout_split_revert_buttons`: a `>` Revert button at the left edge of the right pane for every visible hunk in split diffs of editable buffers (reuses Restore); "N differences" in the single-file diff toolbar next to the existing prev/next arrows. Later: `<<` (copy right → left) only makes sense for the 3-pane merge tool (step 8); the counter in the multi-file Changes toolbar | ✅ |
| 6 | Stash gaps | Stash Changes / Unstash dialogs | `stash_dialogs.rs`: `git::StashChanges` (Git root, Current branch, Message, Keep index → Create Stash) and `git::UnstashChanges` (stash list, View/Drop/Clear with confirm, Pop stash, Reinstate index, As new branch → Apply/Pop Stash/Create Branch); both in the git panel's changes menu | ✅ |
| 7 | Conflicts dialog | Conflicts: file list + Accept Yours / Accept Theirs / Merge… | `conflicts_dialog.rs` (`git::ResolveConflicts`): Name / Yours (branch) / Theirs (merge head) table; Accept Yours/Theirs = `checkout --ours/--theirs` + `add` (swapped during rebase); Merge… opens the file with Zed's inline conflict UI until step 8; closes itself when no conflicts remain. Opens automatically when a merge, rebase, cherry-pick or revert from the Log stops on conflicts; "Resolve" link on the git panel's Conflicts header. Later: multi-select, real per-side status (Modified/Deleted) | ✅ |
| 8 | 3-pane merge tool | Merge Revisions window | `merge_tool.rs`: tab "Merge Revisions for <file>" with Changes from <yours> (read-only) · Result · Changes from <theirs> (read-only), built from stages :1/:2/:3 and an in-house line-based diff3 (`merge3`); non-conflicting changes auto-applied, each conflict starts as base with » Accept Left / Accept Right « / × Ignore controls; conflict and change highlights in all panes; footer with conflict count, Accept Left/Right (whole file), Apply (writes + stages, asks if conflicts remain). Opened from Conflicts → Merge…. Synced scrolling between the three panes (chunk-aligned). Later: per-chunk arrows in the side panes instead of in the result, 'Apply non-conflicting changes' toggles, remote projects | ✅ |
| 9 | Interactive rebase | Interactively Rebase from Here… | `interactive_rebase.rs`: "Rebasing N commits" dialog (oldest first; Pick/Edit/Reword with inline message editor/Squash/Fixup/Drop, Move Up/Down, Reset, Start Rebasing; first commit cannot be squash/fixup; merge commits refused; commit must be on the current branch). Runs `git -c sequence.editor='cp <todo>' -c core.editor=true rebase -i --autostash`, reword via `exec git commit --amend --only -F`. Log menu now also has Edit Commit Message… (amend for HEAD, rebase otherwise), Fixup… / Squash Into… (`commit --fixup/--squash` of staged changes), Drop Commits (confirm), Push All up to Here… (confirm; `push <remote> <sha>:refs/heads/<upstream>`). `git::ContinueRebase` / `git::AbortRebase` + "Rebasing" bar in the git panel. Conflicts during rebase open the Conflicts dialog. Later: drag-to-reorder, multi-select squash in the Log, autosquash toggle | ✅ |
| 10 | Changelists | Changes view: named changelists | `changelists.rs` + git panel: new `group_by: "changelist"` (this fork's default; view menu → Group By → Changelists). One header per changelist ("Changes" by default, active one highlighted, file count, section checkbox stages the list), conflicts stay on top, unassigned changes go to the active list. File menu: Move to Another Changelist… (picker + New Changelist…); header right-click: New / Rename / Delete / Set Active Changelist. Stored per repo in `.git/zed-changelists.json`. Later: forget paths after they are committed, commit only the active list's checked files by default, changelist comments | ✅ |
| 11 | Shelf | Shelve Changes / Unshelve | `shelf.rs` + git panel "Shelf" tab (next to Changes/History): Shelve Changes… on files and on changelist headers (comment dialog) saves `git diff --binary HEAD -- <paths>` under `.git/zed-shelf/<id>/` and restores the paths to HEAD; the tab lists shelves newest first (expand for files) with Unshelve (`git apply --3way`, then removed from the shelf), Show Diff (opens the patch) and Delete (confirm). Unshelved changes come back unstaged (new files as intent-to-add). Later: partial unshelve, unshelve into a changelist | ✅ |
| 12 | Pull Requests window | GitHub: Pull Requests tool window | `pull_requests.rs` (`git::OpenPullRequests`, command palette "git: open pull requests"): list with search and State filter (Open/Closed/Merged/All), refresh; details with description (Markdown), actions Checkout / Show Diff (patch in a read-only editor) / Approve / Merge / Squash and Merge / Rebase and Merge (confirm) / Open on GitHub, changed files with +/−, reviews, comments and a Comment box. Runs the `gh` CLI in the repository (uses your `gh auth login`). Later: per-file diff viewer with inline review comments, Request Changes, create PR from the IDE, a docked tool window instead of a tab | ✅ |
| 13 | Layout and look | New UI Git tool window | `git_tool_window.rs`: bottom-docked "Git" panel with a Log tab hosting the full Log (graph, filters, branches pane, all context menus) for the active repository; ⌘9 toggles it (JetBrains keymap), ⌘9 inside closes it. Later: Console tab (git command log), JetBrains file-status colours as a theme | ✅ |

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
| 2026-09-30 | 7 | Conflicts dialog done |
| 2026-09-30 | 8 | Merge tool done; merge3 + view tests pass |
| 2026-09-30 | 9 | Interactive rebase + rebase-based Log actions; real-git rebase test; 167/167 git_ui tests |
| 2026-09-30 | 10 | Changelists done; 168/168 git_ui tests (older panel tests pinned to status grouping) |
| 2026-09-30 | 11 | Shelf done; git round trip verified; 169/169 git_ui tests |
| 2026-09-30 | 12 | Pull Requests view on gh; JSON parsing test from real gh output |
| 2026-09-30 | 13 | Git tool window; render test; app launch smoke test clean; 171/171 git_ui tests |
| 2026-09-30 | review | Independent review, 13 findings fixed: rebase editors via GIT_SEQUENCE_EDITOR/GIT_EDITOR env (beat user env), `rebase.missingCommitsCheck=error` + HEAD-moved guard, shelf includes untracked files (intent-to-add), pinned diff format, collision-free shelf dirs, Push up to Here ancestor guard, accept side on modify/delete (`git rm`), merge tool swaps panes during rebase and refuses non-UTF-8, conflict ranges no longer grow, root path filter → `.`, exact author match, conflict prompt keeps git's message, changelist counts match other sections, rebase flag cached, Git tool window follows the active repository. Added `script/jetbrains-git-install` and `script/jetbrains-git-sync` |
| 2026-09-30 | polish | Installed `~/Applications/Zed JetBrains.app` (release) + `zed-jb` CLI; unshelve unstaged; Compare with current branch; merge tool synced scrolling; clippy clean; 172/172 git_ui tests |
| 2026-09-30 | after use | Title bar branch widget like JetBrains (`⎇ main ⌄`, worktree button hidden); untracked files under "Unversioned Files" in changelist mode (panel test); git panel can dock at the bottom; this user guide |
| 2026-09-30 | theme | WebStorm "Islands iTerm" theme ported to Zed (`jetbrains/islands-iterm.json`), JetBrains Mono as editor font |
| 2026-09-30 | search | Search Everywhere (`crates/search_everywhere`): All/Classes/Files/Symbols/Actions/Text tabs, JetBrains keymap bindings; GPUI test opens a file from the Files tab |
