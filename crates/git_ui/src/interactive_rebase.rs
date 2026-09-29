use crate::log_actions::spawn_git_job;
use editor::Editor;
use git::Oid;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, PromptLevel, Render,
    SharedString, Task, WeakEntity, Window,
};
use menu::{Cancel, Confirm, SelectNext, SelectPrevious};
use project::git_store::Repository;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use ui::prelude::*;
use workspace::{ModalView, Workspace, notifications::DetachAndPromptErr};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl RebaseAction {
    fn label(self) -> &'static str {
        match self {
            RebaseAction::Pick => "Pick",
            RebaseAction::Reword => "Reword",
            RebaseAction::Edit => "Edit",
            RebaseAction::Squash => "Squash",
            RebaseAction::Fixup => "Fixup",
            RebaseAction::Drop => "Drop",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RebaseEntry {
    pub(crate) sha: String,
    pub(crate) short_sha: String,
    pub(crate) subject: String,
    pub(crate) message: String,
    pub(crate) author: String,
    pub(crate) date: String,
    pub(crate) action: RebaseAction,
    /// Replacement message for [`RebaseAction::Reword`].
    pub(crate) new_message: Option<String>,
}

/// Builds the rebase todo (oldest first); reworded messages are written by the caller to the returned files.
pub(crate) fn build_todo(entries: &[RebaseEntry], message_dir: &std::path::Path) -> (String, Vec<(PathBuf, String)>) {
    let mut todo = String::new();
    let mut messages = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let verb = match entry.action {
            RebaseAction::Pick | RebaseAction::Reword => "pick",
            RebaseAction::Edit => "edit",
            RebaseAction::Squash => "squash",
            RebaseAction::Fixup => "fixup",
            RebaseAction::Drop => "drop",
        };
        todo.push_str(&format!("{verb} {}\n", entry.sha));
        if entry.action == RebaseAction::Reword
            && let Some(message) = &entry.new_message
        {
            let path = message_dir.join(format!("message-{index}.txt"));
            todo.push_str(&format!(
                "exec git commit --amend --only --no-verify --allow-empty -F {}\n",
                shell_quote(&path.to_string_lossy())
            ));
            messages.push((path, message.clone()));
        }
    }
    (todo, messages)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Environment that makes git non-interactive; env vars win over `-c` and user config.
pub(crate) fn non_interactive_env(todo_path: Option<&std::path::Path>) -> Vec<(String, String)> {
    let mut env = vec![
        // Squash would otherwise open an editor for the combined message; keep git's default.
        ("GIT_EDITOR".to_string(), "true".to_string()),
    ];
    if let Some(todo_path) = todo_path {
        env.push((
            "GIT_SEQUENCE_EDITOR".into(),
            format!("cp {}", shell_quote(&todo_path.to_string_lossy())),
        ));
    }
    env
}

fn rebase_args(base: Option<String>) -> Vec<String> {
    vec![
        // A todo that misses a commit must fail rather than silently drop it.
        "-c".to_string(),
        "rebase.missingCommitsCheck=error".into(),
        "rebase".into(),
        "-i".into(),
        "--autostash".into(),
        base.unwrap_or_else(|| "--root".into()),
    ]
}

/// The commits of an interactive rebase and the HEAD they were read at.
#[derive(Clone)]
pub(crate) struct RebasePlan {
    pub(crate) base: Option<String>,
    pub(crate) head: String,
    pub(crate) entries: Vec<RebaseEntry>,
}

static REBASE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Runs `git rebase -i` with a todo generated from the plan, refusing if HEAD moved since the
/// plan was read (the todo would otherwise drop the new commits).
pub(crate) fn run_interactive_rebase(
    repository: &Entity<Repository>,
    plan: RebasePlan,
    window: &mut Window,
    cx: &mut App,
) {
    let current_head = repository.update(cx, |repository, _| {
        repository.git_output(vec!["rev-parse".into(), "HEAD".into()])
    });
    let repository = repository.clone();
    let task: Task<anyhow::Result<String>> = window.spawn(cx, async move |cx| {
        let current_head = current_head.await??;
        anyhow::ensure!(
            current_head.trim() == plan.head,
            "The branch changed since the rebase was prepared. Open the dialog again."
        );
        let directory = std::env::temp_dir().join(format!(
            "zed-rebase-{}-{}",
            std::process::id(),
            REBASE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory)?;
        let (todo, messages) = build_todo(&plan.entries, &directory);
        let todo_path = directory.join("git-rebase-todo");
        std::fs::write(&todo_path, todo)?;
        for (path, message) in messages {
            std::fs::write(path, message)?;
        }
        let receiver = repository.update(cx, |repository, cx| {
            repository.run_git_command_with_env(
                rebase_args(plan.base),
                non_interactive_env(Some(&todo_path)),
                cx,
            )
        });
        receiver.await?
    });
    let (sender, receiver) = futures::channel::oneshot::channel();
    window
        .spawn(cx, async move |_| sender.send(task.await).ok())
        .detach();
    spawn_git_job(receiver, "Rebase failed", window, cx);
}

const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

/// Loads the commits from `sha` to HEAD, oldest first. Fails for ranges containing merges.
pub(crate) fn load_entries(
    repository: &Entity<Repository>,
    sha: Oid,
    cx: &mut App,
) -> Task<anyhow::Result<RebasePlan>> {
    let sha = sha.to_string();
    let is_ancestor = repository.update(cx, |repository, _| {
        repository.git_output(vec![
            "merge-base".into(),
            "--is-ancestor".into(),
            sha.clone(),
            "HEAD".into(),
        ])
    });
    let head = repository.update(cx, |repository, _| {
        repository.git_output(vec!["rev-parse".into(), "HEAD".into()])
    });
    let parent = repository.update(cx, |repository, _| {
        repository.git_output(vec!["rev-parse".into(), "--verify".into(), "--quiet".into(), format!("{sha}^")])
    });
    let repository = repository.clone();
    cx.spawn(async move |cx| {
        anyhow::ensure!(
            is_ancestor.await?.is_ok(),
            "The commit is not in the current branch."
        );
        let head = head.await??.trim().to_string();
        let base = parent.await?.ok().map(|output| output.trim().to_string());
        let range = match &base {
            Some(base) => format!("{base}..HEAD"),
            None => "HEAD".to_string(),
        };
        let log = repository
            .update(cx, |repository, _| {
                repository.git_output(vec![
                    "log".into(),
                    "--reverse".into(),
                    "--date=short".into(),
                    format!("--format=%H{FIELD}%h{FIELD}%s{FIELD}%an{FIELD}%ad{FIELD}%P{FIELD}%B{RECORD}"),
                    range,
                ])
            })
            .await??;
        let mut entries = Vec::new();
        for record in log.split(RECORD).map(|record| record.trim_start_matches('\n')) {
            if record.is_empty() {
                continue;
            }
            let fields: Vec<&str> = record.split(FIELD).collect();
            let [sha, short_sha, subject, author, date, parents, message] = fields.as_slice() else {
                continue;
            };
            anyhow::ensure!(
                parents.split_whitespace().count() <= 1,
                "Interactive rebase of merge commits is not supported ({short_sha})."
            );
            entries.push(RebaseEntry {
                sha: sha.to_string(),
                short_sha: short_sha.to_string(),
                subject: subject.to_string(),
                message: message.trim_end().to_string(),
                author: author.to_string(),
                date: date.to_string(),
                action: RebaseAction::Pick,
                new_message: None,
            });
        }
        anyhow::ensure!(!entries.is_empty(), "No commits to rebase.");
        Ok(RebasePlan { base, head, entries })
    })
}

pub(crate) fn open_rebase_dialog(
    workspace: WeakEntity<Workspace>,
    repository: Entity<Repository>,
    sha: Oid,
    window: &mut Window,
    cx: &mut App,
) {
    let entries = load_entries(&repository, sha, cx);
    let task = window.spawn(cx, async move |cx| {
        let plan = entries.await?;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                InteractiveRebaseModal::new(repository, plan, window, cx)
            })
        })
    });
    task.detach_and_prompt_err("Interactive rebase", window, cx, |_, _, _| None);
}

/// Runs a rebase from `sha` that applies `action` to `sha` only (used by the Log context menu).
pub(crate) fn rebase_single_commit(
    repository: Entity<Repository>,
    sha: Oid,
    action: RebaseAction,
    new_message: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let entries = load_entries(&repository, sha, cx);
    let target = sha.to_string();
    let task = window.spawn(cx, async move |cx| {
        let mut plan = entries.await?;
        for entry in &mut plan.entries {
            if entry.sha == target {
                entry.action = action;
                entry.new_message = new_message.clone();
            }
        }
        cx.update(|window, cx| run_interactive_rebase(&repository, plan, window, cx))?;
        anyhow::Ok(())
    });
    task.detach_and_prompt_err("Rebase failed", window, cx, |_, _, _| None);
}

pub(crate) struct InteractiveRebaseModal {
    repository: Entity<Repository>,
    base: Option<String>,
    head: String,
    original: Vec<RebaseEntry>,
    entries: Vec<RebaseEntry>,
    selected: usize,
    message_editor: Option<Entity<Editor>>,
    focus_handle: FocusHandle,
}

impl InteractiveRebaseModal {
    fn new(
        repository: Entity<Repository>,
        plan: RebasePlan,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            repository,
            base: plan.base,
            head: plan.head,
            original: plan.entries.clone(),
            entries: plan.entries,
            selected: 0,
            message_editor: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn plan(&self) -> RebasePlan {
        RebasePlan {
            base: self.base.clone(),
            head: self.head.clone(),
            entries: self.entries.clone(),
        }
    }

    fn validation_error(&self) -> Option<&'static str> {
        let first_kept = self
            .entries
            .iter()
            .find(|entry| entry.action != RebaseAction::Drop)?;
        matches!(first_kept.action, RebaseAction::Squash | RebaseAction::Fixup)
            .then_some("The first commit cannot be squashed or fixed up.")
    }

    fn set_action(&mut self, action: RebaseAction, window: &mut Window, cx: &mut Context<Self>) {
        self.store_message(cx);
        let Some(entry) = self.entries.get_mut(self.selected) else {
            return;
        };
        entry.action = action;
        self.message_editor = if action == RebaseAction::Reword {
            let message = entry.new_message.clone().unwrap_or_else(|| entry.message.clone());
            let editor = cx.new(|cx| {
                let mut editor = Editor::auto_height(3, 8, window, cx);
                editor.set_text(message, window, cx);
                editor
            });
            window.focus(&editor.focus_handle(cx), cx);
            Some(editor)
        } else {
            None
        };
        cx.notify();
    }

    fn store_message(&mut self, cx: &App) {
        if let Some(editor) = &self.message_editor
            && let Some(entry) = self.entries.get_mut(self.selected)
            && entry.action == RebaseAction::Reword
        {
            entry.new_message = Some(editor.read(cx).text(cx));
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.store_message(cx);
        self.selected = index.min(self.entries.len().saturating_sub(1));
        let is_reword = self
            .entries
            .get(self.selected)
            .is_some_and(|entry| entry.action == RebaseAction::Reword);
        if is_reword {
            self.set_action(RebaseAction::Reword, window, cx);
        } else {
            self.message_editor = None;
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn move_selected(&mut self, up: bool, cx: &mut Context<Self>) {
        self.store_message(cx);
        let target = if up {
            self.selected.checked_sub(1)
        } else {
            Some(self.selected + 1).filter(|index| *index < self.entries.len())
        };
        if let Some(target) = target {
            self.entries.swap(self.selected, target);
            self.selected = target;
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_editor.as_ref().is_some_and(|editor| editor.focus_handle(cx).is_focused(window)) {
            return;
        }
        self.store_message(cx);
        if self.validation_error().is_some() {
            return;
        }
        run_interactive_rebase(&self.repository, self.plan(), window, cx);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for InteractiveRebaseModal {}
impl ModalView for InteractiveRebaseModal {}
impl Focusable for InteractiveRebaseModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for InteractiveRebaseModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let error = self.validation_error();
        let rows = self.entries.iter().enumerate().map(|(index, entry)| {
            let selected = index == self.selected;
            let dropped = entry.action == RebaseAction::Drop;
            let subject: SharedString = match (&entry.action, &entry.new_message) {
                (RebaseAction::Reword, Some(message)) => {
                    message.lines().next().unwrap_or_default().to_string().into()
                }
                _ => entry.subject.clone().into(),
            };
            h_flex()
                .id(("rebase-entry", index))
                .px_2()
                .py_0p5()
                .gap_2()
                .when(selected, |this| this.bg(colors.element_selected))
                .hover(|this| this.bg(colors.element_hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.select(index, window, cx)))
                .child(
                    div().w(rems(4.5)).child(
                        Label::new(entry.action.label())
                            .size(LabelSize::Small)
                            .color(if entry.action == RebaseAction::Pick { Color::Muted } else { Color::Accent }),
                    ),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Label::new(subject)
                            .size(LabelSize::Small)
                            .when(dropped, |label| label.strikethrough())
                            .truncate(),
                    ),
                )
                .child(div().w(rems(8.)).child(Label::new(entry.author.clone()).size(LabelSize::Small).color(Color::Muted).truncate()))
                .child(div().w(rems(5.5)).child(Label::new(entry.date.clone()).size(LabelSize::Small).color(Color::Muted)))
                .child(div().w(rems(4.5)).child(Label::new(entry.short_sha.clone()).size(LabelSize::Small).color(Color::Muted)))
        });

        let action_button = |id: &'static str, action: RebaseAction| {
            Button::new(id, action.label())
                .label_size(LabelSize::Small)
                .on_click(cx.listener(move |this, _, window, cx| this.set_action(action, window, cx)))
        };

        v_flex()
            .key_context("GitInteractiveRebase")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                let next = this.selected + 1;
                this.select(next, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                let previous = this.selected.saturating_sub(1);
                this.select(previous, window, cx)
            }))
            .elevation_2(cx)
            .w(rems(52.))
            .p_3()
            .gap_2()
            .child(
                Headline::new(format!(
                    "Rebasing {} commits{}",
                    self.entries.len(),
                    self.base
                        .as_ref()
                        .map(|base| format!(" onto {}", &base[..base.len().min(8)]))
                        .unwrap_or_default()
                ))
                .size(HeadlineSize::XSmall),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(action_button("rebase-pick", RebaseAction::Pick))
                    .child(action_button("rebase-edit", RebaseAction::Edit))
                    .child(action_button("rebase-reword", RebaseAction::Reword))
                    .child(action_button("rebase-squash", RebaseAction::Squash))
                    .child(action_button("rebase-fixup", RebaseAction::Fixup))
                    .child(action_button("rebase-drop", RebaseAction::Drop))
                    .child(div().w_2())
                    .child(
                        IconButton::new("rebase-up", IconName::ArrowUp)
                            .icon_size(IconSize::Small)
                            .tooltip(ui::Tooltip::text("Move Up"))
                            .on_click(cx.listener(|this, _, _, cx| this.move_selected(true, cx))),
                    )
                    .child(
                        IconButton::new("rebase-down", IconName::ArrowDown)
                            .icon_size(IconSize::Small)
                            .tooltip(ui::Tooltip::text("Move Down"))
                            .on_click(cx.listener(|this, _, _, cx| this.move_selected(false, cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("rebase-list")
                    .max_h(rems(18.))
                    .overflow_y_scroll()
                    .border_1()
                    .rounded_sm()
                    .border_color(colors.border)
                    .children(rows),
            )
            .children(self.message_editor.clone().map(|editor| {
                div()
                    .px_2()
                    .py_1()
                    .border_1()
                    .rounded_sm()
                    .border_color(colors.border_focused)
                    .child(editor)
            }))
            .children(error.map(|error| Label::new(error).size(LabelSize::Small).color(Color::Error)))
            .child(
                h_flex()
                    .justify_between()
                    .child(Button::new("rebase-reset", "Reset").on_click(cx.listener(|this, _, window, cx| {
                        this.entries = this.original.clone();
                        this.message_editor = None;
                        this.select(0, window, cx);
                    })))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Button::new("rebase-cancel", "Cancel").on_click(
                                cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                            ))
                            .child(
                                Button::new("rebase-start", "Start Rebasing")
                                    .style(ButtonStyle::Filled)
                                    .disabled(error.is_some())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.store_message(cx);
                                        if this.validation_error().is_none() {
                                            run_interactive_rebase(&this.repository, this.plan(), window, cx);
                                            cx.emit(DismissEvent);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}

/// JetBrains "Edit Commit Message" dialog.
pub(crate) struct RewordModal {
    repository: Entity<Repository>,
    sha: Oid,
    is_head: bool,
    editor: Entity<Editor>,
}

impl RewordModal {
    pub(crate) fn new(
        repository: Entity<Repository>,
        sha: Oid,
        is_head: bool,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(4, 12, window, cx);
            editor.set_text(message, window, cx);
            editor
        });
        Self {
            repository,
            sha,
            is_head,
            editor,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.editor.read(cx).text(cx);
        if message.trim().is_empty() {
            return;
        }
        if self.is_head {
            let receiver = self.repository.update(cx, |repository, cx| {
                repository.run_git_command(
                    vec![
                        "commit".into(),
                        "--amend".into(),
                        "--only".into(),
                        "--no-verify".into(),
                        "--allow-empty".into(),
                        "-m".into(),
                        message,
                    ],
                    cx,
                )
            });
            spawn_git_job(receiver, "Editing the commit message failed", window, cx);
        } else {
            rebase_single_commit(
                self.repository.clone(),
                self.sha,
                RebaseAction::Reword,
                Some(message),
                window,
                cx,
            );
        }
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for RewordModal {}
impl ModalView for RewordModal {}
impl Focusable for RewordModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for RewordModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("GitRewordModal")
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .elevation_2(cx)
            .w(rems(36.))
            .p_3()
            .gap_2()
            .child(
                Headline::new(format!("Edit Commit Message ({})", self.sha.display_short()))
                    .size(HeadlineSize::XSmall),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_1()
                    .rounded_sm()
                    .border_color(cx.theme().colors().border)
                    .child(self.editor.clone()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(Button::new("reword-cancel", "Cancel").on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))))
                    .child(
                        Button::new("reword-ok", "OK")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// Asks before rewriting history, as JetBrains does for Drop Commits.
pub(crate) fn confirm_then(
    question: String,
    confirm_label: &'static str,
    window: &mut Window,
    cx: &mut App,
    then: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    let answer = window.prompt(PromptLevel::Warning, &question, None, &[confirm_label, "Cancel"], cx);
    window
        .spawn(cx, async move |cx| {
            if answer.await == Ok(0) {
                cx.update(then).ok();
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str, action: RebaseAction, new_message: Option<&str>) -> RebaseEntry {
        RebaseEntry {
            sha: sha.into(),
            short_sha: sha.into(),
            subject: String::new(),
            message: String::new(),
            author: String::new(),
            date: String::new(),
            action,
            new_message: new_message.map(Into::into),
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn test_rebase_with_real_git() {
        let repo = tempfile::tempdir().unwrap();
        let path = repo.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_CONFIG_GLOBAL", "")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        let mut shas = Vec::new();
        for name in ["one", "two", "three", "four"] {
            std::fs::write(path.join(name), name).unwrap();
            git(&["add", name]);
            git(&["commit", "-qm", name]);
            shas.push(git(&["rev-parse", "HEAD"]).trim().to_string());
        }
        // Reword "two", fold "three" into it, drop "four"; rebase from "two" onto "one".
        let entries = [
            entry(&shas[1], RebaseAction::Reword, Some("two, reworded")),
            entry(&shas[2], RebaseAction::Fixup, None),
            entry(&shas[3], RebaseAction::Drop, None),
        ];
        let work = tempfile::tempdir().unwrap();
        let (todo, messages) = build_todo(&entries, work.path());
        let todo_path = work.path().join("git-rebase-todo");
        std::fs::write(&todo_path, todo).unwrap();
        for (message_path, message) in messages {
            std::fs::write(message_path, message).unwrap();
        }
        let args = rebase_args(Some(shas[0].clone()));
        let mut command = std::process::Command::new("git");
        command
            .args(&args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "")
            // A user editor in the environment must not win over ours.
            .env("GIT_SEQUENCE_EDITOR", ":")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t");
        for (key, value) in non_interactive_env(Some(&todo_path)) {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

        assert_eq!(git(&["log", "--format=%s"]), "two, reworded\none\n");
        assert_eq!(git(&["ls-tree", "--name-only", "HEAD"]), "one\nthree\ntwo\n");
    }

    #[test]
    fn test_build_todo() {
        let dir = std::path::Path::new("/tmp/x");
        let (todo, messages) = build_todo(
            &[
                entry("a1", RebaseAction::Pick, None),
                entry("b2", RebaseAction::Reword, Some("New message")),
                entry("c3", RebaseAction::Fixup, None),
                entry("d4", RebaseAction::Drop, None),
            ],
            dir,
        );
        assert_eq!(
            todo,
            "pick a1\npick b2\nexec git commit --amend --only --no-verify --allow-empty -F '/tmp/x/message-1.txt'\nfixup c3\ndrop d4\n"
        );
        assert_eq!(messages, [(dir.join("message-1.txt"), "New message".to_string())]);
    }
}
