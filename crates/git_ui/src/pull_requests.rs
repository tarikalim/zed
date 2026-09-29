use anyhow::Context as _;
use editor::Editor;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, PromptLevel, Render, SharedString,
    Task, WeakEntity, Window,
};
use markdown::{Markdown, MarkdownElement};
use project::Project;
use serde::Deserialize;
use std::path::PathBuf;
use ui::{ContextMenu, PopoverMenu, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
    notifications::DetachAndPromptErr,
};

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &git::OpenPullRequests, window, cx| {
        let existing = workspace.items_of_type::<PullRequestsView>(cx).next();
        if let Some(existing) = existing {
            workspace.activate_item(&existing, true, true, window, cx);
            return;
        }
        let Some(repository) = workspace.project().read(cx).active_repository(cx) else {
            return;
        };
        let directory = repository.read(cx).work_directory_abs_path.to_path_buf();
        let project = workspace.project().clone();
        let workspace_handle = workspace.weak_handle();
        let view = cx.new(|cx| PullRequestsView::new(directory, project, workspace_handle, window, cx));
        workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
    });
}

#[derive(Clone, Debug, Deserialize)]
struct Author {
    login: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestSummary {
    number: u64,
    title: String,
    author: Author,
    head_ref_name: String,
    base_ref_name: String,
    state: String,
    is_draft: bool,
    #[serde(default)]
    review_decision: Option<String>,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ChangedFile {
    path: String,
    additions: u64,
    deletions: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    author: Author,
    body: String,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Review {
    author: Author,
    state: String,
    body: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestDetails {
    number: u64,
    title: String,
    body: String,
    url: String,
    #[serde(default)]
    files: Vec<ChangedFile>,
    #[serde(default)]
    comments: Vec<Comment>,
    #[serde(default)]
    reviews: Vec<Review>,
    additions: u64,
    deletions: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateFilter {
    Open,
    Closed,
    Merged,
    All,
}

impl StateFilter {
    fn arg(self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
            StateFilter::Merged => "merged",
            StateFilter::All => "all",
        }
    }

    fn label(self) -> &'static str {
        match self {
            StateFilter::Open => "Open",
            StateFilter::Closed => "Closed",
            StateFilter::Merged => "Merged",
            StateFilter::All => "All",
        }
    }
}

/// Runs the GitHub CLI in the repository and returns stdout.
fn gh(directory: PathBuf, args: Vec<String>, cx: &App) -> Task<anyhow::Result<String>> {
    cx.background_spawn(async move {
        let output = util::command::new_command("gh")
            .args(&args)
            .current_dir(&directory)
            .output()
            .await
            .context("could not run the GitHub CLI (`gh`); install it and run `gh auth login`")?;
        anyhow::ensure!(
            output.status.success(),
            "gh {} failed:\n{}",
            args.first().map(String::as_str).unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

pub(crate) struct PullRequestsView {
    directory: PathBuf,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    state_filter: StateFilter,
    search: Entity<Editor>,
    pull_requests: Vec<PullRequestSummary>,
    loading: bool,
    error: Option<SharedString>,
    selected: Option<u64>,
    details: Option<(PullRequestDetails, Entity<Markdown>)>,
    comment_editor: Entity<Editor>,
    focus_handle: FocusHandle,
    _list_task: Option<Task<()>>,
    _details_task: Option<Task<()>>,
}

impl PullRequestsView {
    fn new(
        directory: PathBuf,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search", window, cx);
            editor
        });
        cx.subscribe(&search, |_, _, event: &editor::EditorEvent, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                cx.notify();
            }
        })
        .detach();
        let comment_editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(2, 6, window, cx);
            editor.set_placeholder_text("Comment", window, cx);
            editor
        });
        let mut this = Self {
            directory,
            project,
            workspace,
            state_filter: StateFilter::Open,
            search,
            pull_requests: Vec::new(),
            loading: false,
            error: None,
            selected: None,
            details: None,
            comment_editor,
            focus_handle: cx.focus_handle(),
            _list_task: None,
            _details_task: None,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        let task = gh(
            self.directory.clone(),
            vec![
                "pr".into(),
                "list".into(),
                "--state".into(),
                self.state_filter.arg().into(),
                "--limit".into(),
                "200".into(),
                "--json".into(),
                "number,title,author,headRefName,baseRefName,state,isDraft,reviewDecision,updatedAt".into(),
            ],
            cx,
        );
        self._list_task = Some(cx.spawn(async move |this, cx| {
            let result = task
                .await
                .and_then(|output| Ok(serde_json::from_str::<Vec<PullRequestSummary>>(&output)?));
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(pull_requests) => this.pull_requests = pull_requests,
                    Err(error) => this.error = Some(format!("{error:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select(&mut self, number: u64, cx: &mut Context<Self>) {
        self.selected = Some(number);
        self.details = None;
        let task = gh(
            self.directory.clone(),
            vec![
                "pr".into(),
                "view".into(),
                number.to_string(),
                "--json".into(),
                "number,title,body,url,files,comments,reviews,additions,deletions".into(),
            ],
            cx,
        );
        let languages = self.project.read(cx).languages().clone();
        self._details_task = Some(cx.spawn(async move |this, cx| {
            let Some(details) = task
                .await
                .and_then(|output| Ok(serde_json::from_str::<PullRequestDetails>(&output)?))
                .log_err()
            else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.selected == Some(details.number) {
                    let body = if details.body.trim().is_empty() {
                        "*No description provided.*".to_string()
                    } else {
                        details.body.clone()
                    };
                    let markdown = cx.new(|cx| Markdown::new(body.into(), Some(languages), None, cx));
                    this.details = Some((details, markdown));
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn run_on_selected(
        &mut self,
        args: impl FnOnce(u64) -> Vec<String>,
        error: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(number) = self.selected else {
            return;
        };
        let task = gh(self.directory.clone(), args(number), cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await?;
            this.update(cx, |this, cx| {
                this.refresh(cx);
                this.select(number, cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_prompt_err(error, window, cx, |_, _, _| None);
    }

    fn merge(&mut self, method: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("{} pull request #{number}?", match method {
                "--squash" => "Squash and merge",
                "--rebase" => "Rebase and merge",
                _ => "Merge",
            }),
            None,
            &["Merge", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                this.update_in(cx, |this, window, cx| {
                    this.run_on_selected(
                        |number| vec!["pr".into(), "merge".into(), number.to_string(), method.into()],
                        "Merge failed",
                        window,
                        cx,
                    )
                })
                .ok();
            }
        })
        .detach();
    }

    fn show_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        let task = gh(
            self.directory.clone(),
            vec!["pr".into(), "diff".into(), number.to_string(), "--color=never".into()],
            cx,
        );
        let project = self.project.clone();
        let workspace = self.workspace.clone();
        let languages = project.read(cx).languages().clone();
        cx.spawn_in(window, async move |_, cx| {
            let diff = task.await?;
            let language = languages.language_for_name("Diff").await.ok();
            workspace.update_in(cx, |workspace, window, cx| {
                let buffer = project.update(cx, |project, cx| {
                    project.create_local_buffer(&diff, language, false, cx)
                });
                let editor = cx.new(|cx| {
                    let mut editor = Editor::for_buffer(buffer, Some(project.clone()), window, cx);
                    editor.set_read_only(true);
                    editor
                });
                workspace.add_item_to_active_pane(Box::new(editor), None, true, window, cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_prompt_err("Show Diff failed", window, cx, |_, _, _| None);
    }

    fn visible_pull_requests(&self, cx: &App) -> Vec<PullRequestSummary> {
        let query = self.search.read(cx).text(cx).to_lowercase();
        self.pull_requests
            .iter()
            .filter(|pr| {
                query.is_empty()
                    || pr.title.to_lowercase().contains(&query)
                    || pr.author.login.to_lowercase().contains(&query)
                    || pr.head_ref_name.to_lowercase().contains(&query)
                    || format!("#{}", pr.number).contains(&query)
            })
            .cloned()
            .collect()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let this = cx.entity().downgrade();
        let state_filter = self.state_filter;
        let rows = self.visible_pull_requests(cx).into_iter().map(|pr| {
            let selected = self.selected == Some(pr.number);
            let number = pr.number;
            let status = match (pr.is_draft, pr.review_decision.as_deref()) {
                (true, _) => Some(("Draft", Color::Muted)),
                (_, Some("APPROVED")) => Some(("Approved", Color::Success)),
                (_, Some("CHANGES_REQUESTED")) => Some(("Changes requested", Color::Error)),
                (_, Some("REVIEW_REQUIRED")) => Some(("Review required", Color::Warning)),
                _ => None,
            };
            v_flex()
                .id(("pull-request", number))
                .px_2()
                .py_1()
                .gap_0p5()
                .border_b_1()
                .border_color(colors.border_variant)
                .when(selected, |this| this.bg(colors.element_selected))
                .hover(|this| this.bg(colors.element_hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.select(number, cx)))
                .child(
                    h_flex()
                        .gap_1()
                        .child(Label::new(pr.title.clone()).size(LabelSize::Small).truncate()),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Label::new(format!(
                                "#{} by {} · {} ← {} · {}",
                                pr.number,
                                pr.author.login,
                                pr.base_ref_name,
                                pr.head_ref_name,
                                pr.updated_at.get(..10).unwrap_or(&pr.updated_at),
                            ))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate(),
                        )
                        .when(pr.state != "OPEN", |this| {
                            this.child(Label::new(pr.state.to_lowercase()).size(LabelSize::XSmall).color(Color::Muted))
                        })
                        .children(status.map(|(label, color)| {
                            Label::new(label).size(LabelSize::XSmall).color(color)
                        })),
                )
        });

        v_flex()
            .w(relative(0.4))
            .h_full()
            .border_r_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .p_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(
                        div()
                            .flex_1()
                            .px_1p5()
                            .py_0p5()
                            .border_1()
                            .rounded_sm()
                            .border_color(colors.border_variant)
                            .child(self.search.clone()),
                    )
                    .child(
                        PopoverMenu::new("pr-state-filter")
                            .trigger(
                                Button::new("pr-state-trigger", format!("State: {}", state_filter.label()))
                                    .label_size(LabelSize::Small)
                                    .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                            )
                            .menu(move |window, cx| {
                                let this = this.clone();
                                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                    for filter in [StateFilter::Open, StateFilter::Closed, StateFilter::Merged, StateFilter::All] {
                                        let this = this.clone();
                                        menu = menu.toggleable_entry(
                                            filter.label(),
                                            filter == state_filter,
                                            IconPosition::Start,
                                            None,
                                            move |_, cx| {
                                                this.update(cx, |this, cx| {
                                                    this.state_filter = filter;
                                                    this.refresh(cx);
                                                })
                                                .ok();
                                            },
                                        );
                                    }
                                    menu
                                }))
                            }),
                    )
                    .child(
                        IconButton::new("pr-refresh", IconName::RotateCw)
                            .icon_size(IconSize::Small)
                            .tooltip(ui::Tooltip::text("Refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("pull-request-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(self.loading, |this| {
                        this.child(div().p_2().child(Label::new("Loading…").color(Color::Muted)))
                    })
                    .children(self.error.clone().map(|error| {
                        div().p_2().child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    })),
            )
    }

    fn render_details(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((details, body)) = &self.details else {
            let message = if self.selected.is_some() { "Loading…" } else { "Select a pull request" };
            return v_flex()
                .flex_1()
                .justify_center()
                .items_center()
                .child(Label::new(message).color(Color::Muted))
                .into_any_element();
        };
        let colors = cx.theme().colors();
        let url = details.url.clone();
        let markdown_style = editor::hover_markdown_style(window, cx);
        v_flex()
            .id("pull-request-details")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .p_3()
            .gap_2()
            .child(Headline::new(format!("{} #{}", details.title, details.number)).size(HeadlineSize::Small))
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(Button::new("pr-checkout", "Checkout").on_click(cx.listener(|this, _, window, cx| {
                        this.run_on_selected(
                            |number| vec!["pr".into(), "checkout".into(), number.to_string()],
                            "Checkout failed",
                            window,
                            cx,
                        )
                    })))
                    .child(Button::new("pr-diff", "Show Diff").on_click(cx.listener(|this, _, window, cx| this.show_diff(window, cx))))
                    .child(Button::new("pr-approve", "Approve").on_click(cx.listener(|this, _, window, cx| {
                        this.run_on_selected(
                            |number| vec!["pr".into(), "review".into(), number.to_string(), "--approve".into()],
                            "Approve failed",
                            window,
                            cx,
                        )
                    })))
                    .child(Button::new("pr-merge", "Merge").on_click(cx.listener(|this, _, window, cx| this.merge("--merge", window, cx))))
                    .child(Button::new("pr-squash", "Squash and Merge").on_click(cx.listener(|this, _, window, cx| this.merge("--squash", window, cx))))
                    .child(Button::new("pr-rebase", "Rebase and Merge").on_click(cx.listener(|this, _, window, cx| this.merge("--rebase", window, cx))))
                    .child(
                        Button::new("pr-open", "Open on GitHub")
                            .end_icon(Icon::new(IconName::ArrowUpRight).size(IconSize::XSmall))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .p_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(colors.border_variant)
                    .child(MarkdownElement::new(body.clone(), markdown_style)),
            )
            .child(
                Label::new(format!(
                    "Files changed ({}) · +{} −{}",
                    details.files.len(),
                    details.additions,
                    details.deletions
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .children(details.files.iter().map(|file| {
                h_flex()
                    .gap_2()
                    .child(Label::new(file.path.clone()).size(LabelSize::Small).truncate())
                    .child(Label::new(format!("+{}", file.additions)).size(LabelSize::XSmall).color(Color::Created))
                    .child(Label::new(format!("−{}", file.deletions)).size(LabelSize::XSmall).color(Color::Deleted))
            }))
            .children(details.reviews.iter().filter(|review| review.state != "COMMENTED" || !review.body.is_empty()).map(|review| {
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .rounded_sm()
                    .bg(colors.element_background)
                    .child(
                        Label::new(format!("{} · {}", review.author.login, review.state.to_lowercase().replace('_', " ")))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .when(!review.body.is_empty(), |this| this.child(Label::new(review.body.clone()).size(LabelSize::Small)))
            }))
            .children(details.comments.iter().map(|comment| {
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .rounded_sm()
                    .bg(colors.element_background)
                    .child(
                        Label::new(format!(
                            "{} · {}",
                            comment.author.login,
                            comment.created_at.get(..10).unwrap_or(&comment.created_at)
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    )
                    .child(Label::new(comment.body.clone()).size(LabelSize::Small))
            }))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .border_1()
                            .rounded_sm()
                            .border_color(colors.border)
                            .child(self.comment_editor.clone()),
                    )
                    .child(
                        h_flex().justify_end().child(
                            Button::new("pr-comment", "Comment")
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let body = this.comment_editor.read(cx).text(cx);
                                    if body.trim().is_empty() {
                                        return;
                                    }
                                    this.comment_editor.update(cx, |editor, cx| editor.clear(window, cx));
                                    this.run_on_selected(
                                        move |number| vec!["pr".into(), "comment".into(), number.to_string(), "--body".into(), body],
                                        "Comment failed",
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl EventEmitter<ItemEvent> for PullRequestsView {}

impl Focusable for PullRequestsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for PullRequestsView {
    type Event = ItemEvent;

    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        "Pull Requests".into()
    }

    fn tab_icon(&self, _: &Window, _: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitBranch))
    }

    fn to_item_events(event: &ItemEvent, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for PullRequestsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .key_context("PullRequests")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(self.render_list(cx))
            .child(self.render_details(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes captured from `gh pr list` / `gh pr view` (gh 2.x).
    #[test]
    fn test_parses_gh_json() {
        let list: Vec<PullRequestSummary> = serde_json::from_str(
            r#"[{"author":{"id":"x","is_bot":false,"login":"ada","name":"Ada"},"baseRefName":"main","headRefName":"feature","isDraft":true,"number":7,"reviewDecision":"","state":"OPEN","title":"Add x","updatedAt":"2026-09-29T22:49:15Z"}]"#,
        )
        .unwrap();
        assert_eq!(list[0].number, 7);
        assert_eq!(list[0].author.login, "ada");
        assert!(list[0].is_draft);

        let details: PullRequestDetails = serde_json::from_str(
            r#"{"additions":3,"body":"Body","comments":[{"author":{"login":"bob"},"body":"LGTM","createdAt":"2026-09-30T10:00:00Z"}],"deletions":0,"files":[{"path":"Cargo.lock","additions":3,"deletions":0,"changeType":"MODIFIED"}],"number":7,"reviews":[{"author":{"login":"bob"},"state":"APPROVED","body":"","submittedAt":"2026-09-30T10:00:00Z"}],"title":"Add x","url":"https://github.com/o/r/pull/7"}"#,
        )
        .unwrap();
        assert_eq!(details.files[0].path, "Cargo.lock");
        assert_eq!(details.reviews[0].state, "APPROVED");
        assert_eq!(details.comments[0].body, "LGTM");
    }
}
