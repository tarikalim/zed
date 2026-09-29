use git::repository::RepoPath;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, SharedString,
    Task, WeakEntity, Window,
};
use menu::{Cancel, SelectNext, SelectPrevious};
use project::{ProjectPath, git_store::Repository};
use ui::prelude::*;
use workspace::{ModalView, Workspace, notifications::DetachAndPromptErr};

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &git::ResolveConflicts, window, cx| {
        open(workspace, window, cx);
    });
}

pub(crate) fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(repository) = workspace.project().read(cx).active_repository(cx) else {
        return;
    };
    let workspace_handle = workspace.weak_handle();
    workspace.toggle_modal(window, cx, |_, cx| {
        ConflictsModal::new(repository, workspace_handle, cx)
    });
}

#[derive(Clone, Copy)]
enum Side {
    Yours,
    Theirs,
}

struct ConflictsModal {
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    selected: usize,
    focus_handle: FocusHandle,
}

impl ConflictsModal {
    fn new(
        repository: Entity<Repository>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&repository, |this, _, cx| {
            if this.conflicts(cx).is_empty() {
                cx.emit(DismissEvent);
            }
            cx.notify();
        })
        .detach();
        Self {
            repository,
            workspace,
            selected: 0,
            focus_handle: cx.focus_handle(),
        }
    }

    fn conflicts(&self, cx: &App) -> Vec<RepoPath> {
        self.repository
            .read(cx)
            .cached_status()
            .filter(|entry| entry.status.is_conflicted())
            .map(|entry| entry.repo_path)
            .collect()
    }

    // ponytail: checks the rebase dirs on disk; move to the repository snapshot if it is needed elsewhere.
    fn is_rebasing(&self, cx: &App) -> bool {
        let git_dir = &self.repository.read(cx).repository_dir_abs_path;
        git_dir.join(git::REBASE_MERGE_DIR).exists() || git_dir.join("rebase-apply").exists()
    }

    fn side_labels(&self, cx: &App) -> (SharedString, SharedString) {
        let repository = self.repository.read(cx);
        let current = repository
            .branch
            .as_ref()
            .map(|branch| branch.name().to_string())
            .unwrap_or_else(|| "HEAD".into());
        let incoming = repository
            .merge
            .merge_heads_by_conflicted_path
            .iter()
            .flat_map(|(_, heads)| heads.iter().flatten())
            .next()
            .map(|head| head.to_string())
            .unwrap_or_else(|| "incoming".into());
        (
            format!("Yours ({current})").into(),
            format!("Theirs ({incoming})").into(),
        )
    }

    fn accept(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.conflicts(cx).get(self.selected).cloned() else {
            return;
        };
        // During a rebase git's "ours" is the branch being rebased onto, so JetBrains' "Yours" is `--theirs`.
        let flag = match (side, self.is_rebasing(cx)) {
            (Side::Yours, false) | (Side::Theirs, true) => "--ours",
            (Side::Theirs, false) | (Side::Yours, true) => "--theirs",
        };
        let path = path.as_unix_str().to_string();
        let checkout = self.repository.update(cx, |repository, cx| {
            repository.run_git_command(
                vec!["checkout".into(), flag.into(), "--".into(), path.clone()],
                cx,
            )
        });
        let repository = self.repository.clone();
        let task: Task<anyhow::Result<String>> = cx.spawn(async move |_, cx| {
            checkout.await??;
            repository
                .update(cx, |repository, cx| {
                    repository.run_git_command(vec!["add".into(), "--".into(), path], cx)
                })
                .await?
        });
        task.detach_and_prompt_err("Resolving the conflict failed", window, cx, |_, _, _| None);
    }

    fn merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.conflicts(cx).get(self.selected).cloned() else {
            return;
        };
        let Some(project_path) = self
            .repository
            .read(cx)
            .repo_path_to_project_path(&path, cx)
        else {
            return;
        };
        open_project_path(&self.workspace, project_path, window, cx);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.conflicts(cx).len();
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }
}

fn open_project_path(
    workspace: &WeakEntity<Workspace>,
    project_path: ProjectPath,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            workspace
                .open_path(project_path, None, true, window, cx)
                .detach_and_log_err(cx);
        })
        .ok();
}

impl EventEmitter<DismissEvent> for ConflictsModal {}
impl ModalView for ConflictsModal {}
impl Focusable for ConflictsModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConflictsModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let conflicts = self.conflicts(cx);
        if self.selected >= conflicts.len() {
            self.selected = conflicts.len().saturating_sub(1);
        }
        let has_conflicts = !conflicts.is_empty();
        let (yours, theirs) = self.side_labels(cx);
        let colors = cx.theme().colors();
        let column = |text: SharedString, color: Color| {
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(text).size(LabelSize::Small).color(color).truncate())
        };
        let rows = conflicts.iter().enumerate().map(|(index, path)| {
            let selected = index == self.selected;
            let file_name: SharedString = path
                .file_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| path.as_unix_str().to_string())
                .into();
            let directory: SharedString = path
                .parent()
                .map(|parent| parent.as_unix_str().to_string())
                .unwrap_or_default()
                .into();
            h_flex()
                .id(("conflict", index))
                .px_2()
                .py_0p5()
                .gap_2()
                .when(selected, |this| this.bg(colors.element_selected))
                .hover(|this| this.bg(colors.element_hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.selected = index;
                    if event.click_count() >= 2 {
                        this.merge(window, cx);
                    }
                    cx.notify();
                }))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(Label::new(file_name).size(LabelSize::Small))
                        .child(
                            Label::new(directory)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                )
                .child(column("Modified".into(), Color::Modified))
                .child(column("Modified".into(), Color::Modified))
        });

        v_flex()
            .key_context("GitConflictsModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .elevation_2(cx)
            .w(rems(46.))
            .p_3()
            .gap_2()
            .child(Headline::new("Conflicts").size(HeadlineSize::XSmall))
            .child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .border_1()
                            .rounded_sm()
                            .border_color(colors.border)
                            .child(
                                h_flex()
                                    .px_2()
                                    .py_0p5()
                                    .gap_2()
                                    .border_b_1()
                                    .border_color(colors.border_variant)
                                    .child(column("Name".into(), Color::Muted))
                                    .child(column(yours, Color::Muted))
                                    .child(column(theirs, Color::Muted)),
                            )
                            .child(
                                v_flex()
                                    .id("conflict-list")
                                    .h(rems(12.))
                                    .overflow_y_scroll()
                                    .children(rows),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Button::new("accept-yours", "Accept Yours")
                                    .disabled(!has_conflicts)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.accept(Side::Yours, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("accept-theirs", "Accept Theirs")
                                    .disabled(!has_conflicts)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.accept(Side::Theirs, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("merge", "Merge…")
                                    .style(ButtonStyle::Filled)
                                    .disabled(!has_conflicts)
                                    .on_click(cx.listener(|this, _, window, cx| this.merge(window, cx))),
                            ),
                    ),
            )
            .child(
                h_flex().justify_end().child(
                    Button::new("close", "Close")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                ),
            )
    }
}
