use crate::{branch_diff::BranchDiff, interactive_rebase};
use editor::Editor;
use git::{
    Oid,
    repository::{CommitOperation, ResetMode},
};
use gpui::{
    Action, App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Render, SharedString, Task, WeakEntity, Window,
};
use menu::{Cancel, Confirm};
use project::git_store::Repository;
use ui::{Checkbox, ContextMenu, ToggleState, prelude::*};
use workspace::{ModalView, Workspace, notifications::DetachAndPromptErr};

/// Adds the JetBrains "Log" context menu entries for a commit, in JetBrains order.
pub(crate) fn jetbrains_log_entries(
    menu: ContextMenu,
    sha: Oid,
    repository: Option<WeakEntity<Repository>>,
    workspace: WeakEntity<Workspace>,
    cx: &App,
) -> ContextMenu {
    let Some(repository) = repository.and_then(|repository| repository.upgrade()) else {
        return menu;
    };
    let is_local = workspace
        .upgrade()
        .is_some_and(|workspace| workspace.read(cx).project().read(cx).is_local());
    let is_head = repository
        .read(cx)
        .head_commit
        .as_ref()
        .is_some_and(|head| head.sha.as_ref() == sha.to_string());
    let sha_short = sha.display_short();
    let repository = repository.downgrade();

    let operation = |operation: CommitOperation| {
        let repository = repository.clone();
        move |window: &mut Window, cx: &mut App| {
            run_commit_operation(&repository, sha, operation.clone(), window, cx)
        }
    };

    menu.entry(
        "Copy Revision Number",
        Some(crate::commit_context_menu::CopyCommitSha.boxed_clone()),
        move |_window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(sha.to_string()));
        },
    )
    .entry_disabled_when(!is_local, "Cherry-Pick", operation(CommitOperation::CherryPick))
    .separator()
    .entry_disabled_when(
        !is_local,
        format!("Checkout Revision '{sha_short}'"),
        operation(CommitOperation::Checkout),
    )
    .entry("Compare with Local", None, {
        let workspace = workspace.clone();
        let repository = repository.clone();
        move |window, cx| compare_with_local(&workspace, &repository, sha, window, cx)
    })
    .separator()
    .entry_disabled_when(!is_local, "Reset Current Branch to Here…", {
        let workspace = workspace.clone();
        let repository = repository.clone();
        move |window, cx| {
            let repository = repository.clone();
            open_modal(&workspace, window, cx, move |window, cx| {
                ResetModal::new(repository, sha, window, cx)
            })
        }
    })
    .entry_disabled_when(!is_local, "Revert Commit", operation(CommitOperation::Revert))
    .when(is_head, |menu| {
        menu.entry_disabled_when(!is_local, "Undo Commit…", {
            let repository = repository.clone();
            move |window, cx| {
                let Some(repository) = repository.upgrade() else {
                    return;
                };
                let receiver = repository
                    .update(cx, |repo, cx| repo.reset("HEAD^".into(), ResetMode::Soft, cx));
                spawn_git_job(receiver, "Undo Commit failed", window, cx);
            }
        })
    })
    .entry_disabled_when(!is_local, "Edit Commit Message…", {
        let workspace = workspace.clone();
        let repository = repository.clone();
        move |window, cx| edit_commit_message(&workspace, &repository, sha, is_head, window, cx)
    })
    .entry_disabled_when(!is_local, "Fixup…", {
        let repository = repository.clone();
        move |window, cx| fixup_commit(&repository, sha, false, window, cx)
    })
    .entry_disabled_when(!is_local, "Squash Into…", {
        let repository = repository.clone();
        move |window, cx| fixup_commit(&repository, sha, true, window, cx)
    })
    .entry_disabled_when(!is_local, "Drop Commits", {
        let repository = repository.clone();
        move |window, cx| {
            let repository = repository.clone();
            interactive_rebase::confirm_then(
                format!("Drop commit {sha_short}? The branch history will be rewritten."),
                "Drop",
                window,
                cx,
                move |window, cx| {
                    if let Some(repository) = repository.upgrade() {
                        interactive_rebase::rebase_single_commit(
                            repository,
                            sha,
                            interactive_rebase::RebaseAction::Drop,
                            None,
                            window,
                            cx,
                        );
                    }
                },
            )
        }
    })
    .entry_disabled_when(!is_local, "Interactively Rebase from Here…", {
        let workspace = workspace.clone();
        let repository = repository.clone();
        move |window, cx| {
            if let Some(repository) = repository.upgrade() {
                interactive_rebase::open_rebase_dialog(workspace.clone(), repository, sha, window, cx);
            }
        }
    })
    .entry_disabled_when(!is_local, "Push All up to Here…", {
        let repository = repository.clone();
        move |window, cx| push_up_to(&repository, sha, window, cx)
    })
    .separator()
    .entry_disabled_when(!is_local, "New Branch…", {
        let workspace = workspace.clone();
        let repository = repository.clone();
        move |window, cx| {
            let repository = repository.clone();
            open_modal(&workspace, window, cx, move |window, cx| {
                NewRefModal::new(NewRefKind::Branch, repository, sha, window, cx)
            })
        }
    })
    .entry_disabled_when(!is_local, "New Tag…", {
        let repository = repository.clone();
        move |window, cx| {
            let repository = repository.clone();
            open_modal(&workspace, window, cx, move |window, cx| {
                NewRefModal::new(NewRefKind::Tag, repository, sha, window, cx)
            })
        }
    })
}

trait ContextMenuExt {
    fn entry_disabled_when(
        self,
        disabled: bool,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self;
}

impl ContextMenuExt for ContextMenu {
    fn entry_disabled_when(
        self,
        disabled: bool,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.item(
            ui::ContextMenuEntry::new(label)
                .disabled(disabled)
                .handler(handler),
        )
    }
}

fn edit_commit_message(
    workspace: &WeakEntity<Workspace>,
    repository: &WeakEntity<Repository>,
    sha: Oid,
    is_head: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    let message = repository.update(cx, |repository, _| {
        repository.git_output(vec![
            "log".into(),
            "-1".into(),
            "--format=%B".into(),
            sha.to_string(),
        ])
    });
    let workspace = workspace.clone();
    let task: Task<anyhow::Result<()>> = window.spawn(cx, async move |cx| {
        let message = message.await??.trim_end().to_string();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                interactive_rebase::RewordModal::new(repository, sha, is_head, message, window, cx)
            })
        })
    });
    task.detach_and_prompt_err("Edit Commit Message", window, cx, |_, _, _| None);
}

/// JetBrains "Fixup…" / "Squash Into…": commits the staged changes as `fixup!` / `squash!` of `sha`.
fn fixup_commit(
    repository: &WeakEntity<Repository>,
    sha: Oid,
    squash: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    let flag = if squash { "--squash" } else { "--fixup" };
    let receiver = repository.update(cx, |repository, cx| {
        repository.run_git_command_with_env(
            vec!["commit".into(), format!("{flag}={sha}")],
            interactive_rebase::non_interactive_env(None),
            cx,
        )
    });
    spawn_git_job(receiver, "Fixup commit failed", window, cx);
}

fn push_up_to(repository: &WeakEntity<Repository>, sha: Oid, window: &mut Window, cx: &mut App) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    let target = repository.read(cx).branch.as_ref().and_then(|branch| {
        let upstream = branch.upstream.as_ref()?;
        Some((
            upstream.remote_name()?.to_string(),
            upstream.branch_name()?.to_string(),
        ))
    });
    let Some((remote, remote_branch)) = target else {
        Task::ready(Err::<(), _>(anyhow::anyhow!(
            "The current branch has no upstream branch to push to."
        )))
        .detach_and_prompt_err("Push failed", window, cx, |_, _, _| None);
        return;
    };
    let is_ancestor = repository.update(cx, |repository, _| {
        repository.git_output(vec![
            "merge-base".into(),
            "--is-ancestor".into(),
            sha.to_string(),
            "HEAD".into(),
        ])
    });
    let task: Task<anyhow::Result<()>> = window.spawn(cx, async move |cx| {
        // Pushing a commit from another branch to this branch's upstream would publish unrelated history.
        anyhow::ensure!(
            is_ancestor.await?.is_ok(),
            "The commit is not in the current branch."
        );
        cx.update(|window, cx| {
            interactive_rebase::confirm_then(
                format!(
                    "Push commits up to {} to {remote}/{remote_branch}?",
                    sha.display_short()
                ),
                "Push",
                window,
                cx,
                move |window, cx| {
                    let receiver = repository.update(cx, |repository, cx| {
                        repository.run_git_command(
                            vec!["push".into(), remote, format!("{sha}:refs/heads/{remote_branch}")],
                            cx,
                        )
                    });
                    spawn_git_job(receiver, "Push failed", window, cx);
                },
            )
        })
    });
    task.detach_and_prompt_err("Push failed", window, cx, |_, _, _| None);
}

fn open_modal<V: ModalView>(
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
) {
    workspace
        .update(cx, |workspace, cx| workspace.toggle_modal(window, cx, build))
        .ok();
}

pub(crate) fn open_new_branch_modal(
    workspace: &WeakEntity<Workspace>,
    repository: WeakEntity<Repository>,
    sha: Oid,
    window: &mut Window,
    cx: &mut App,
) {
    open_modal(workspace, window, cx, move |window, cx| {
        NewRefModal::new(NewRefKind::Branch, repository, sha, window, cx)
    })
}

pub(crate) fn spawn_git_job<T: 'static>(
    receiver: futures::channel::oneshot::Receiver<anyhow::Result<T>>,
    error_title: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let error_title = error_title.to_string();
    window
        .spawn(cx, async move |cx| {
            let Err(error) = receiver.await.map_err(anyhow::Error::from).and_then(|r| r) else {
                return;
            };
            cx.update(|window, cx| show_git_error(error, &error_title, window, cx))
                .ok();
        })
        .detach();
}

/// Shows a failed git command, with a follow-up action where JetBrains offers one.
pub(crate) fn show_git_error(
    error: anyhow::Error,
    error_title: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let message = format!("{error:#}");
    // Like JetBrains, a merge, rebase or cherry-pick that stops on conflicts opens the Conflicts dialog.
    let follow_up: Option<(&str, &str, Box<dyn Action>)> =
        if ["CONFLICT", "could not apply", "could not revert"]
            .iter()
            .any(|marker| message.contains(marker))
        {
            Some(("Conflicts", "Resolve…", Box::new(git::ResolveConflicts)))
        } else if let Some(path) = worktree_using_branch(&message) {
            Some((
                "Branch Is Checked Out in Another Worktree",
                "Open in New Tab",
                Box::new(zed_actions::OpenWorktreeInNewWindow { path }),
            ))
        } else {
            None
        };
    let Some((title, button, action)) = follow_up else {
        let task: Task<anyhow::Result<()>> = Task::ready(Err(error));
        task.detach_and_prompt_err(error_title, window, cx, |_, _, _| None);
        return;
    };
    // Keep git's message visible; the follow-up runs only on request.
    let answer = window.prompt(
        gpui::PromptLevel::Warning,
        title,
        Some(&message),
        &[button, "Close"],
        cx,
    );
    window
        .spawn(cx, async move |cx| {
            if answer.await == Ok(0) {
                cx.update(|window, cx| window.dispatch_action(action, cx))
                    .ok();
            }
        })
        .detach();
}

/// The worktree path from git's "'<branch>' is already used by worktree at '<path>'".
fn worktree_using_branch(message: &str) -> Option<std::path::PathBuf> {
    let (_, rest) = message.split_once("is already used by worktree at '")?;
    let (path, _) = rest.split_once('\'')?;
    Some(path.into())
}

fn run_commit_operation(
    repository: &WeakEntity<Repository>,
    sha: Oid,
    operation: CommitOperation,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    let error_title = match &operation {
        CommitOperation::CherryPick => "Cherry-pick failed",
        CommitOperation::Revert => "Revert failed",
        CommitOperation::Checkout => "Checkout failed",
        CommitOperation::CreateTag { .. } => "Tag creation failed",
        CommitOperation::CreateBranch { .. } => "Branch creation failed",
    };
    let receiver = repository.update(cx, |repo, cx| {
        repo.run_commit_operation(sha.to_string(), operation, cx)
    });
    spawn_git_job(receiver, error_title, window, cx);
}

// ponytail: diffs from the merge base of the commit and HEAD, which equals JetBrains'
// "Compare with Local" for ancestors of HEAD; a direct commit-to-worktree diff for other commits comes later.
fn compare_with_local(
    workspace: &WeakEntity<Workspace>,
    repository: &WeakEntity<Repository>,
    sha: Oid,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    workspace
        .update(cx, |workspace, cx| {
            let project = workspace.project().clone();
            BranchDiff::deploy_branch_diff_with_base_ref(
                workspace,
                project,
                repository,
                sha.to_string().into(),
                None,
                window,
                cx,
            );
        })
        .ok();
}

const RESET_MODES: [(ResetMode, &str, &str); 4] = [
    (
        ResetMode::Soft,
        "Soft",
        "Files won't change, differences will be staged for commit.",
    ),
    (
        ResetMode::Mixed,
        "Mixed",
        "Files won't change, differences won't be staged.",
    ),
    (
        ResetMode::Hard,
        "Hard",
        "Files will be reverted to the state of the selected commit.\nWarning: any local changes will be lost.",
    ),
    (
        ResetMode::Keep,
        "Keep",
        "Files will be reverted to the state of the selected commit,\nbut local changes will be kept intact.",
    ),
];

struct ResetModal {
    repository: WeakEntity<Repository>,
    sha: Oid,
    selected: usize,
    branch_name: SharedString,
    focus_handle: FocusHandle,
}

impl ResetModal {
    fn new(
        repository: WeakEntity<Repository>,
        sha: Oid,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch_name = repository
            .upgrade()
            .and_then(|repository| repository.read(cx).branch.as_ref().map(|b| b.name().to_string()))
            .unwrap_or_else(|| "HEAD".to_string());
        Self {
            repository,
            sha,
            selected: 1,
            branch_name: branch_name.into(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = self.repository.upgrade() else {
            return;
        };
        let mode = match RESET_MODES.get(self.selected) {
            Some((ResetMode::Soft, ..)) => ResetMode::Soft,
            Some((ResetMode::Hard, ..)) => ResetMode::Hard,
            Some((ResetMode::Keep, ..)) => ResetMode::Keep,
            _ => ResetMode::Mixed,
        };
        let receiver =
            repository.update(cx, |repo, cx| repo.reset(self.sha.to_string(), mode, cx));
        spawn_git_job(receiver, "Reset failed", window, cx);
        cx.emit(DismissEvent);
    }

    fn select_previous(&mut self, _: &menu::SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = (self.selected + 1).min(RESET_MODES.len() - 1);
        cx.notify();
    }
}

impl EventEmitter<DismissEvent> for ResetModal {}
impl ModalView for ResetModal {}
impl Focusable for ResetModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ResetModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = RESET_MODES
            .iter()
            .enumerate()
            .map(|(index, (_, label, description))| {
                let selected = index == self.selected;
                h_flex()
                    .id(("reset-mode", index))
                    .items_start()
                    .gap_2()
                    .py_1()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        cx.notify();
                    }))
                    .child(radio(selected, cx))
                    .child(
                        v_flex()
                            .child(Label::new(*label))
                            .child(Label::new(*description).size(LabelSize::Small).color(Color::Muted)),
                    )
            });

        v_flex()
            .key_context("GitResetModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .elevation_2(cx)
            .w(rems(34.))
            .p_3()
            .gap_2()
            .child(Headline::new("Git Reset").size(HeadlineSize::XSmall))
            .child(
                Label::new(format!(
                    "This will reset the current branch head '{}' to the selected commit ({}), and update the working tree and the index according to the selected mode:",
                    self.branch_name,
                    self.sha.display_short()
                ))
                .size(LabelSize::Small),
            )
            .children(options)
            .child(dialog_buttons("Reset", cx))
    }
}

fn radio(selected: bool, cx: &App) -> impl IntoElement {
    let colors = cx.theme().colors();
    div()
        .mt(px(3.))
        .size(px(14.))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if selected {
            colors.border_focused
        } else {
            colors.border
        })
        .flex()
        .items_center()
        .justify_center()
        .when(selected, |this| {
            this.child(div().size(px(6.)).rounded_full().bg(colors.border_focused))
        })
}

fn dialog_buttons<V: 'static>(confirm_label: &'static str, cx: &mut Context<V>) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_end()
        .gap_1()
        .pt_1()
        .child(Button::new("cancel", "Cancel").on_click(cx.listener(|_, _, window, cx| {
            window.dispatch_action(Box::new(Cancel), cx)
        })))
        .child(
            Button::new("confirm", confirm_label)
                .style(ButtonStyle::Filled)
                .on_click(cx.listener(|_, _, window, cx| {
                    window.dispatch_action(Box::new(Confirm), cx)
                })),
        )
}

#[derive(Clone, Copy, PartialEq)]
enum NewRefKind {
    Branch,
    Tag,
}

struct NewRefModal {
    kind: NewRefKind,
    repository: WeakEntity<Repository>,
    sha: Oid,
    editor: Entity<Editor>,
    checkout: bool,
}

impl NewRefModal {
    fn new(
        kind: NewRefKind,
        repository: WeakEntity<Repository>,
        sha: Oid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| Editor::single_line(window, cx));
        Self {
            kind,
            repository,
            sha,
            editor,
            checkout: true,
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.editor.read(cx).text(cx).trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(repository) = self.repository.upgrade() else {
            return;
        };
        let sha = self.sha.to_string();
        match (self.kind, self.checkout) {
            (NewRefKind::Branch, true) => {
                let receiver = repository.update(cx, |repo, _| repo.create_branch(name, Some(sha)));
                spawn_git_job(receiver, "Branch creation failed", window, cx);
            }
            (NewRefKind::Branch, false) => {
                run_commit_operation(
                    &self.repository,
                    self.sha,
                    CommitOperation::CreateBranch { name },
                    window,
                    cx,
                );
            }
            (NewRefKind::Tag, _) => {
                run_commit_operation(
                    &self.repository,
                    self.sha,
                    CommitOperation::CreateTag { name },
                    window,
                    cx,
                );
            }
        }
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for NewRefModal {}
impl ModalView for NewRefModal {}
impl Focusable for NewRefModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for NewRefModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, prompt, confirm_label) = match self.kind {
            NewRefKind::Branch => ("Create New Branch".to_string(), "New branch name:", "Create"),
            NewRefKind::Tag => (
                format!("Create New Tag On {}", self.sha.display_short()),
                "Enter the name of new tag:",
                "Create Tag",
            ),
        };
        v_flex()
            .key_context("GitNewRefModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(30.))
            .p_3()
            .gap_2()
            .child(Headline::new(title).size(HeadlineSize::XSmall))
            .child(Label::new(prompt).size(LabelSize::Small))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_1()
                    .rounded_sm()
                    .border_color(cx.theme().colors().border)
                    .child(self.editor.clone()),
            )
            .when(self.kind == NewRefKind::Branch, |this| {
                this.child(
                    Checkbox::new("checkout-branch", ToggleState::from(self.checkout))
                        .label("Checkout branch")
                        .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                            this.checkout = state.selected();
                            cx.notify();
                        })),
                )
            })
            .child(dialog_buttons(confirm_label, cx))
    }
}

#[cfg(test)]
mod worktree_error_tests {
    use super::worktree_using_branch;

    #[test]
    fn test_worktree_using_branch() {
        let message = "git switch failed:\nfatal: 'feat/105685-sandbox-migrate-on-resume' is already used by worktree at '/Users/me/newmind/nmaistro-backend-105685'";
        assert_eq!(
            worktree_using_branch(message),
            Some("/Users/me/newmind/nmaistro-backend-105685".into())
        );
        assert_eq!(worktree_using_branch("fatal: invalid reference: nope"), None);
    }
}
