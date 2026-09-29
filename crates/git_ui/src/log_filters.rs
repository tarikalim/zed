use super::*;
use git::repository::LogFilter;
use gpui::{DismissEvent, EventEmitter, PathPromptOptions};
use ui::{ContextMenu, ContextMenuEntry, PopoverMenu};
use workspace::ModalView;

/// JetBrains "Log" filter state that lives on the view, not in git.
#[derive(Default)]
pub(super) struct LogFilterState {
    authors: Vec<SharedString>,
    current_user: Option<SharedString>,
    _load_task: Option<Task<()>>,
}

impl GitGraph {
    pub(super) fn log_filter(&self) -> LogFilter {
        match &self.log_source {
            LogSource::Filtered(filter) => filter.clone(),
            _ => LogFilter::default(),
        }
    }

    pub(super) fn shows_log_filters(&self) -> bool {
        matches!(self.log_source, LogSource::All | LogSource::Filtered(_))
    }

    pub(super) fn set_log_filter(&mut self, filter: LogFilter, cx: &mut Context<Self>) {
        let source = if filter.is_empty() {
            LogSource::All
        } else {
            LogSource::Filtered(filter)
        };
        if source == self.log_source {
            return;
        }
        if source.is_linear() != self.log_source.is_linear() {
            let (column_widths, column_visibility) = Self::column_layout(source.is_linear(), cx);
            self.column_widths = column_widths;
            self.column_visibility = column_visibility;
        }
        self.log_source = source;
        self.selected_entry_idx = None;
        self.pending_select_sha = None;
        self.invalidate_state(cx);
    }

    /// Loads the User filter's list once: `git config user.name` for "me" and authors by commit count.
    pub(super) fn load_log_filter_users(&mut self, cx: &mut Context<Self>) {
        if self.log_filter_state._load_task.is_some() {
            return;
        }
        let Some(repository) = self.get_repository(cx) else {
            return;
        };
        let (authors, me) = repository.update(cx, |repository, _| {
            (
                repository.git_output(vec![
                    "shortlog".into(),
                    "-sn".into(),
                    "--all".into(),
                    "--no-merges".into(),
                ]),
                repository.git_output(vec!["config".into(), "user.name".into()]),
            )
        });
        self.log_filter_state._load_task = Some(cx.spawn(async move |this, cx| {
            let authors = authors
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result)
                .log_err()
                .unwrap_or_default();
            let me = me.await.ok().and_then(|result| result.ok());
            this.update(cx, |this, cx| {
                this.log_filter_state.authors = authors
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .map(|(_, name)| SharedString::from(name.trim().to_string()))
                    .collect();
                this.log_filter_state.current_user = me
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty())
                    .map(SharedString::from);
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn render_log_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.log_filter();
        let this = cx.entity().downgrade();

        let branch_value = (!filter.branches.is_empty()).then(|| filter.branches.join(", "));
        let user_value = (!filter.authors.is_empty()).then(|| filter.authors.join(", "));
        let date_value = date_label(&filter);
        let path_value = (!filter.paths.is_empty()).then(|| {
            filter
                .paths
                .iter()
                .map(|path| path.as_unix_str().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        });

        h_flex()
            .gap_0p5()
            .child(filter_button(
                "log-filter-branch",
                "Branch",
                branch_value,
                this.clone(),
                |filter| filter.branches.clear(),
                {
                    let this = this.clone();
                    move |window, cx| branch_menu(this.clone(), window, cx)
                },
            ))
            .child(filter_button(
                "log-filter-user",
                "User",
                user_value,
                this.clone(),
                |filter| filter.authors.clear(),
                {
                    let this = this.clone();
                    move |window, cx| user_menu(this.clone(), window, cx)
                },
            ))
            .child(filter_button(
                "log-filter-date",
                "Date",
                date_value,
                this.clone(),
                |filter| {
                    filter.since = None;
                    filter.until = None;
                },
                {
                    let this = this.clone();
                    move |window, cx| date_menu(this.clone(), window, cx)
                },
            ))
            .child(filter_button(
                "log-filter-paths",
                "Paths",
                path_value,
                this.clone(),
                |filter| filter.paths.clear(),
                move |window, cx| paths_menu(this.clone(), window, cx),
            ))
    }
}

fn date_label(filter: &LogFilter) -> Option<String> {
    match (&filter.since, &filter.until) {
        (None, None) => None,
        (Some(since), None) if since.as_ref() == LAST_24_HOURS => Some("Last 24 hours".into()),
        (Some(since), None) if since.as_ref() == LAST_7_DAYS => Some("Last 7 days".into()),
        (Some(since), None) => Some(format!("after {since}")),
        (None, Some(until)) => Some(format!("before {until}")),
        (Some(since), Some(until)) => Some(format!("{since} – {until}")),
    }
}

const LAST_24_HOURS: &str = "24 hours ago";
const LAST_7_DAYS: &str = "7 days ago";

fn update_filter(
    this: &WeakEntity<GitGraph>,
    cx: &mut App,
    change: impl FnOnce(&mut LogFilter),
) {
    this.update(cx, |this, cx| {
        let mut filter = this.log_filter();
        change(&mut filter);
        this.set_log_filter(filter, cx);
    })
    .ok();
}

fn filter_button(
    id: &'static str,
    name: &'static str,
    value: Option<String>,
    this: WeakEntity<GitGraph>,
    clear: impl Fn(&mut LogFilter) + Copy + 'static,
    menu: impl Fn(&mut Window, &mut App) -> Entity<ContextMenu> + 'static,
) -> impl IntoElement {
    let is_set = value.is_some();
    let label = match value {
        Some(value) => format!("{name}: {value}"),
        None => name.to_string(),
    };
    h_flex()
        .child(
            PopoverMenu::new(id)
                .trigger(
                    Button::new(SharedString::from(format!("{id}-trigger")), label)
                        .label_size(LabelSize::Small)
                        .truncate(true)
                        .when(is_set, |button| button.color(Color::Accent))
                        .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                )
                .menu(move |window, cx| Some(menu(window, cx))),
        )
        .when(is_set, |this_flex| {
            this_flex.child(
                IconButton::new(SharedString::from(format!("{id}-clear")), IconName::Close)
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Clear filter"))
                    .on_click(move |_, _, cx| update_filter(&this, cx, clear)),
            )
        })
}

fn branch_menu(this: WeakEntity<GitGraph>, window: &mut Window, cx: &mut App) -> Entity<ContextMenu> {
    let (branches, selected) = this
        .read_with(cx, |this, cx| {
            let branches = this
                .get_repository(cx)
                .map(|repository| repository.read(cx).branch_list.to_vec())
                .unwrap_or_default();
            (branches, this.log_filter().branches)
        })
        .unwrap_or_default();

    ContextMenu::build(window, cx, move |mut menu, _, _| {
        let entry = |menu: ContextMenu, label: SharedString, value: SharedString| {
            let this = this.clone();
            let checked = selected.contains(&value);
            menu.toggleable_entry(label, checked, IconPosition::Start, None, move |_, cx| {
                let value = value.clone();
                update_filter(&this, cx, |filter| filter.branches = vec![value]);
            })
        };
        menu = entry(menu, "HEAD (Current Branch)".into(), "HEAD".into());
        menu = menu.separator().header("Local");
        for branch in branches.iter().filter(|branch| !branch.is_remote()) {
            let name: SharedString = branch.name().to_string().into();
            menu = entry(menu, name.clone(), name);
        }
        let remote: Vec<_> = branches.iter().filter(|branch| branch.is_remote()).collect();
        if !remote.is_empty() {
            menu = menu.separator().header("Remote");
            for branch in remote {
                let name: SharedString = branch.name().to_string().into();
                menu = entry(menu, name.clone(), name);
            }
        }
        menu
    })
}

fn user_menu(this: WeakEntity<GitGraph>, window: &mut Window, cx: &mut App) -> Entity<ContextMenu> {
    let (authors, me, selected) = this
        .update(cx, |this, cx| {
            this.load_log_filter_users(cx);
            (
                this.log_filter_state.authors.clone(),
                this.log_filter_state.current_user.clone(),
                this.log_filter().authors,
            )
        })
        .unwrap_or_default();

    ContextMenu::build(window, cx, move |mut menu, _, _| {
        let entry = |menu: ContextMenu, label: SharedString, value: SharedString| {
            let this = this.clone();
            let checked = selected.contains(&value);
            menu.toggleable_entry(label, checked, IconPosition::Start, None, move |_, cx| {
                let value = value.clone();
                update_filter(&this, cx, |filter| filter.authors = vec![value]);
            })
        };
        if let Some(me) = me.clone() {
            menu = entry(menu, "me".into(), me).separator();
        }
        if authors.is_empty() {
            menu = menu.item(ContextMenuEntry::new("Loading…").disabled(true));
        }
        for author in authors.iter() {
            menu = entry(menu, author.clone(), author.clone());
        }
        menu
    })
}

fn date_menu(this: WeakEntity<GitGraph>, window: &mut Window, cx: &mut App) -> Entity<ContextMenu> {
    let selected = this
        .read_with(cx, |this, _| this.log_filter())
        .unwrap_or_default();
    ContextMenu::build(window, cx, move |menu, _, _| {
        let preset = |menu: ContextMenu, label: &'static str, since: &'static str| {
            let this = this.clone();
            let checked = selected.until.is_none()
                && selected.since.as_ref().is_some_and(|value| value.as_ref() == since);
            menu.toggleable_entry(label, checked, IconPosition::Start, None, move |_, cx| {
                update_filter(&this, cx, |filter| {
                    filter.since = Some(since.into());
                    filter.until = None;
                });
            })
        };
        let menu = preset(menu, "Last 24 hours", LAST_24_HOURS);
        let menu = preset(menu, "Last 7 days", LAST_7_DAYS);
        let this = this.clone();
        let selected = selected.clone();
        menu.separator().entry("Select…", None, move |window, cx| {
            let Some(workspace) = this.read_with(cx, |this, _| this.workspace.clone()).ok() else {
                return;
            };
            let this = this.clone();
            let selected = selected.clone();
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_modal(window, cx, move |window, cx| {
                        DateRangeModal::new(this, &selected, window, cx)
                    })
                })
                .ok();
        })
    })
}

fn paths_menu(this: WeakEntity<GitGraph>, window: &mut Window, cx: &mut App) -> Entity<ContextMenu> {
    ContextMenu::build(window, cx, move |menu, _, _| {
        menu.entry("Select Folders…", None, move |_, cx| {
            let Some(repository) = this
                .read_with(cx, |this, cx| this.get_repository(cx))
                .ok()
                .flatten()
            else {
                return;
            };
            let (work_directory, path_style) = {
                let repository = repository.read(cx);
                (
                    repository.work_directory_abs_path.clone(),
                    repository.path_style,
                )
            };
            let prompt = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: true,
                multiple: true,
                prompt: Some("Select".into()),
            });
            let this = this.clone();
            cx.spawn(async move |cx| {
                let Ok(Ok(Some(paths))) = prompt.await else {
                    return;
                };
                let repo_paths: Vec<RepoPath> = paths
                    .iter()
                    .filter_map(|path| path.strip_prefix(&work_directory).ok())
                    .filter_map(|path| RepoPath::from_std_path(path, path_style).log_err())
                    .collect();
                if repo_paths.is_empty() {
                    return;
                }
                cx.update(|cx| update_filter(&this, cx, |filter| filter.paths = repo_paths));
            })
            .detach();
        })
    })
}

struct DateRangeModal {
    graph: WeakEntity<GitGraph>,
    after: Entity<Editor>,
    before: Entity<Editor>,
}

impl DateRangeModal {
    fn new(
        graph: WeakEntity<GitGraph>,
        filter: &LogFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |value: &Option<SharedString>, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("YYYY-MM-DD", window, cx);
                if let Some(value) = value {
                    editor.set_text(value.to_string(), window, cx);
                }
                editor
            })
        };
        Self {
            graph,
            after: field(&filter.since, window, cx),
            before: field(&filter.until, window, cx),
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        let value = |editor: &Entity<Editor>, cx: &App| {
            let text = editor.read(cx).text(cx).trim().to_string();
            (!text.is_empty()).then(|| SharedString::from(text))
        };
        let since = value(&self.after, cx);
        let until = value(&self.before, cx);
        update_filter(&self.graph, cx, |filter| {
            filter.since = since;
            filter.until = until;
        });
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for DateRangeModal {}
impl ModalView for DateRangeModal {}
impl Focusable for DateRangeModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.after.focus_handle(cx)
    }
}

impl Render for DateRangeModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().colors().border;
        let field = |label: &'static str, editor: Entity<Editor>| {
            h_flex()
                .gap_2()
                .child(div().w(rems(4.)).child(Label::new(label).size(LabelSize::Small)))
                .child(
                    div()
                        .flex_1()
                        .px_2()
                        .py_1()
                        .border_1()
                        .rounded_sm()
                        .border_color(border)
                        .child(editor),
                )
        };
        v_flex()
            .key_context("GitLogDateFilter")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(24.))
            .p_3()
            .gap_2()
            .child(Headline::new("Select Period").size(HeadlineSize::XSmall))
            .child(field("After:", self.after.clone()))
            .child(field("Before:", self.before.clone()))
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(Button::new("date-cancel", "Cancel").on_click(cx.listener(
                        |_, _, window, cx| window.dispatch_action(Box::new(Cancel), cx),
                    )))
                    .child(
                        Button::new("date-ok", "OK")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(menu::Confirm), cx)
                            })),
                    ),
            )
    }
}
