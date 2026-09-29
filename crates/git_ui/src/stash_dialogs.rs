use crate::{commit_view::CommitView, log_actions::spawn_git_job};
use editor::Editor;
use git::stash::StashEntry;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, PromptLevel, Render,
    SharedString, WeakEntity, Window,
};
use menu::{Cancel, Confirm, SelectNext, SelectPrevious};
use project::git_store::Repository;
use ui::{Checkbox, ToggleState, prelude::*};
use workspace::{ModalView, Workspace};

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &git::StashChanges, window, cx| {
        let Some(repository) = workspace.project().read(cx).active_repository(cx) else {
            return;
        };
        workspace.toggle_modal(window, cx, |window, cx| {
            StashChangesModal::new(repository, window, cx)
        });
    });
    workspace.register_action(|workspace, _: &git::UnstashChanges, window, cx| {
        let Some(repository) = workspace.project().read(cx).active_repository(cx) else {
            return;
        };
        let workspace_handle = workspace.weak_handle();
        workspace.toggle_modal(window, cx, |window, cx| {
            UnstashModal::new(repository, workspace_handle, window, cx)
        });
    });
}

fn repository_header(repository: &Entity<Repository>, cx: &App) -> (SharedString, SharedString) {
    let repository = repository.read(cx);
    let root = repository
        .work_directory_abs_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let branch = repository
        .branch
        .as_ref()
        .map(|branch| branch.name().to_string())
        .unwrap_or_else(|| "HEAD".into());
    (root.into(), branch.into())
}

fn info_row(label: &'static str, value: SharedString) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(
            div()
                .w(rems(7.))
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted)),
        )
        .child(Label::new(value).size(LabelSize::Small))
}

fn bordered(editor: Entity<Editor>, cx: &App) -> impl IntoElement {
    div()
        .flex_1()
        .px_2()
        .py_1()
        .border_1()
        .rounded_sm()
        .border_color(cx.theme().colors().border)
        .child(editor)
}

fn run(repository: &Entity<Repository>, args: Vec<String>, error: &str, window: &mut Window, cx: &mut App) {
    let receiver = repository.update(cx, |repository, cx| repository.run_git_command(args, cx));
    spawn_git_job(receiver, error, window, cx);
}

struct StashChangesModal {
    repository: Entity<Repository>,
    message: Entity<Editor>,
    keep_index: bool,
}

impl StashChangesModal {
    fn new(repository: Entity<Repository>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            repository,
            message: cx.new(|cx| Editor::single_line(window, cx)),
            keep_index: false,
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).text(cx).trim().to_string();
        let mut args = vec!["stash".to_string(), "push".to_string()];
        if self.keep_index {
            args.push("--keep-index".into());
        }
        if !message.is_empty() {
            args.push("-m".into());
            args.push(message);
        }
        run(&self.repository, args, "Stash failed", window, cx);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for StashChangesModal {}
impl ModalView for StashChangesModal {}
impl Focusable for StashChangesModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}

impl Render for StashChangesModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (root, branch) = repository_header(&self.repository, cx);
        v_flex()
            .key_context("GitStashChangesModal")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_2(cx)
            .w(rems(30.))
            .p_3()
            .gap_2()
            .child(Headline::new("Stash").size(HeadlineSize::XSmall))
            .child(info_row("Git root:", root))
            .child(info_row("Current branch:", branch))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w(rems(7.))
                            .child(Label::new("Message:").size(LabelSize::Small).color(Color::Muted)),
                    )
                    .child(bordered(self.message.clone(), cx)),
            )
            .child(
                Checkbox::new("stash-keep-index", ToggleState::from(self.keep_index))
                    .label("Keep index")
                    .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                        this.keep_index = state.selected();
                        cx.notify();
                    })),
            )
            .child(buttons("Create Stash", cx))
    }
}

fn buttons<V: 'static>(confirm: impl Into<SharedString>, cx: &mut Context<V>) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_end()
        .gap_1()
        .pt_1()
        .child(Button::new("cancel", "Cancel").on_click(cx.listener(|_, _, window, cx| {
            window.dispatch_action(Box::new(Cancel), cx)
        })))
        .child(
            Button::new("confirm", confirm)
                .style(ButtonStyle::Filled)
                .on_click(cx.listener(|_, _, window, cx| {
                    window.dispatch_action(Box::new(Confirm), cx)
                })),
        )
}

struct UnstashModal {
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    selected: usize,
    pop: bool,
    reinstate_index: bool,
    branch: Entity<Editor>,
    focus_handle: FocusHandle,
}

impl UnstashModal {
    fn new(
        repository: Entity<Repository>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&repository, |_, _, cx| cx.notify()).detach();
        let branch = cx.new(|cx| Editor::single_line(window, cx));
        cx.subscribe(&branch, |_, _, _: &editor::EditorEvent, cx| cx.notify())
            .detach();
        Self {
            repository,
            workspace,
            selected: 0,
            pop: false,
            reinstate_index: false,
            branch,
            focus_handle: cx.focus_handle(),
        }
    }

    fn entries(&self, cx: &App) -> Vec<StashEntry> {
        self.repository.read(cx).stash_entries.entries.to_vec()
    }

    fn selected_ref(&self, cx: &App) -> Option<(String, StashEntry)> {
        let entry = self.entries(cx).get(self.selected)?.clone();
        Some((format!("stash@{{{}}}", entry.index), entry))
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some((stash, _)) = self.selected_ref(cx) else {
            return;
        };
        let branch = self.branch.read(cx).text(cx).trim().to_string();
        let args = if !branch.is_empty() {
            vec!["stash".into(), "branch".into(), branch, stash]
        } else {
            let mut args = vec![
                "stash".to_string(),
                if self.pop { "pop" } else { "apply" }.to_string(),
            ];
            if self.reinstate_index {
                args.push("--index".into());
            }
            args.push(stash);
            args
        };
        run(&self.repository, args, "Unstash failed", window, cx);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.entries(cx).len();
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    fn view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, entry)) = self.selected_ref(cx) else {
            return;
        };
        CommitView::open(
            entry.oid.to_string(),
            self.repository.downgrade(),
            self.workspace.clone(),
            Some(entry.index),
            None,
            window,
            cx,
        );
        cx.emit(DismissEvent);
    }

    fn confirm_then_run(
        &mut self,
        question: String,
        args: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(PromptLevel::Warning, &question, None, &["Delete", "Cancel"], cx);
        let repository = self.repository.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                this.update_in(cx, |this, window, cx| {
                    run(&repository, args, "Stash operation failed", window, cx);
                    this.selected = 0;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

impl EventEmitter<DismissEvent> for UnstashModal {}
impl ModalView for UnstashModal {}
impl Focusable for UnstashModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for UnstashModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (root, branch) = repository_header(&self.repository, cx);
        let entries = self.entries(cx);
        let has_entries = !entries.is_empty();
        let colors = cx.theme().colors();
        let rows = entries.into_iter().enumerate().map(|(index, entry)| {
            let selected = index == self.selected;
            let label = format!(
                "stash@{{{}}}: {}{}",
                entry.index,
                entry
                    .branch
                    .as_ref()
                    .map(|branch| format!("On {branch}: "))
                    .unwrap_or_default(),
                entry.message
            );
            div()
                .id(("unstash-entry", index))
                .px_2()
                .py_0p5()
                .when(selected, |this| this.bg(colors.element_selected))
                .hover(|this| this.bg(colors.element_hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.selected = index;
                    if event.click_count() >= 2 {
                        this.view(window, cx);
                    }
                    cx.notify();
                }))
                .child(Label::new(label).size(LabelSize::Small).truncate())
        });

        let apply_label = if !self.branch.read(cx).text(cx).trim().is_empty() {
            "Create Branch"
        } else if self.pop {
            "Pop Stash"
        } else {
            "Apply Stash"
        };

        v_flex()
            .key_context("GitUnstashModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .elevation_2(cx)
            .w(rems(40.))
            .p_3()
            .gap_2()
            .child(Headline::new("Unstash Changes").size(HeadlineSize::XSmall))
            .child(info_row("Git root:", root))
            .child(info_row("Current branch:", branch))
            .child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        v_flex()
                            .id("unstash-list")
                            .flex_1()
                            .h(rems(12.))
                            .overflow_y_scroll()
                            .border_1()
                            .rounded_sm()
                            .border_color(colors.border)
                            .children(rows)
                            .when(!has_entries, |this| {
                                this.child(
                                    div().p_2().child(
                                        Label::new("No stashes")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    ),
                                )
                            }),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Button::new("unstash-view", "View")
                                    .disabled(!has_entries)
                                    .on_click(cx.listener(|this, _, window, cx| this.view(window, cx))),
                            )
                            .child(Button::new("unstash-drop", "Drop").disabled(!has_entries).on_click(
                                cx.listener(|this, _, window, cx| {
                                    if let Some((stash, _)) = this.selected_ref(cx) {
                                        this.confirm_then_run(
                                            format!("Do you want to remove {stash}?"),
                                            vec!["stash".into(), "drop".into(), stash],
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                            ))
                            .child(Button::new("unstash-clear", "Clear").disabled(!has_entries).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.confirm_then_run(
                                        "Remove all stashes? This cannot be undone.".into(),
                                        vec!["stash".into(), "clear".into()],
                                        window,
                                        cx,
                                    );
                                }),
                            )),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        Checkbox::new("unstash-pop", ToggleState::from(self.pop))
                            .label("Pop stash")
                            .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                                this.pop = state.selected();
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("unstash-index", ToggleState::from(self.reinstate_index))
                            .label("Reinstate index")
                            .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                                this.reinstate_index = state.selected();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w(rems(7.))
                            .child(Label::new("As new branch:").size(LabelSize::Small).color(Color::Muted)),
                    )
                    .child(bordered(self.branch.clone(), cx)),
            )
            .child(buttons(apply_label, cx).into_any_element())
            .into_any_element()
    }
}
