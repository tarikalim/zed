//! JetBrains-style "Search Everywhere": one popup with All, Classes, Files, Symbols, Actions and Text tabs.

use command_palette_hooks::CommandPaletteFilter;
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use futures::StreamExt as _;
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    Action, AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Render, Subscription, Task, WeakEntity, Window, actions,
};
use language::{Bias, Buffer, Point, ToPoint as _};
use picker::{Picker, PickerDelegate};
use project::{PathMatchCandidateSet, Project, ProjectPath, Symbol, search::SearchQuery};
use schemars::JsonSchema;
use serde::Deserialize;
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use ui::{HighlightedLabel, KeyBinding, ListItem, ListItemSpacing, prelude::*};
use util::{ResultExt as _, paths::PathMatcher};
use workspace::{ModalView, Workspace};

/// Opens Search Everywhere on the given tab.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = search_everywhere)]
#[serde(deny_unknown_fields)]
pub struct Toggle {
    #[serde(default)]
    pub tab: Tab,
}

actions!(
    search_everywhere,
    [
        /// Switches to the next Search Everywhere tab.
        NextTab,
        /// Switches to the previous Search Everywhere tab.
        PreviousTab,
    ]
);

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    #[default]
    All,
    Classes,
    Files,
    Symbols,
    Actions,
    Text,
}

const TABS: [Tab; 6] = [Tab::All, Tab::Classes, Tab::Files, Tab::Symbols, Tab::Actions, Tab::Text];

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Classes => "Classes",
            Tab::Files => "Files",
            Tab::Symbols => "Symbols",
            Tab::Actions => "Actions",
            Tab::Text => "Text",
        }
    }
}

/// How many results of each kind the All tab shows, like JetBrains' grouped list.
const ALL_TAB_LIMIT: usize = 8;
const TAB_LIMIT: usize = 100;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, action: &Toggle, window, cx| {
            SearchEverywhere::toggle(workspace, action.tab, window, cx);
        });
    })
    .detach();
}

pub struct SearchEverywhere {
    picker: Entity<Picker<SearchEverywhereDelegate>>,
    _subscription: Subscription,
}

impl SearchEverywhere {
    fn toggle(workspace: &mut Workspace, tab: Tab, window: &mut Window, cx: &mut Context<Workspace>) {
        if let Some(existing) = workspace.active_modal::<Self>(cx) {
            existing.update(cx, |this, cx| {
                this.picker.update(cx, |picker, cx| {
                    let next = if picker.delegate.tab == tab {
                        next_tab(tab, 1)
                    } else {
                        tab
                    };
                    picker.delegate.set_tab(next, window, cx);
                })
            });
            return;
        }
        let Some(previous_focus) = window.focused(cx) else {
            return;
        };
        let project = workspace.project().clone();
        let workspace_handle = workspace.weak_handle();
        let actions = collect_actions(window, cx);
        let recent = workspace
            .recent_navigation_history(Some(20), cx)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        workspace.toggle_modal(window, cx, |window, cx| {
            let delegate = SearchEverywhereDelegate {
                search_everywhere: cx.entity().downgrade(),
                workspace: workspace_handle,
                project,
                previous_focus,
                tab,
                actions,
                recent,
                results: Vec::new(),
                selected_index: 0,
                search_id: 0,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                query: String::new(),
            };
            let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
            let subscription = cx.subscribe(&picker, |_, _, _: &DismissEvent, cx| cx.emit(DismissEvent));
            Self {
                picker,
                _subscription: subscription,
            }
        });
    }
}

fn next_tab(tab: Tab, step: isize) -> Tab {
    let index = TABS.iter().position(|candidate| *candidate == tab).unwrap_or(0) as isize;
    TABS[(index + step).rem_euclid(TABS.len() as isize) as usize]
}

struct ActionEntry {
    name: SharedString,
    action: Box<dyn Action>,
}

fn collect_actions(window: &Window, cx: &App) -> Vec<ActionEntry> {
    let filter = CommandPaletteFilter::try_global(cx);
    window
        .available_actions(cx)
        .into_iter()
        .filter(|action| !filter.is_some_and(|filter| filter.is_hidden(&**action)))
        .map(|action| ActionEntry {
            name: command_palette::humanize_action_name(action.name()).into(),
            action,
        })
        .collect()
}

impl EventEmitter<DismissEvent> for SearchEverywhere {}
impl ModalView for SearchEverywhere {}

impl Focusable for SearchEverywhere {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for SearchEverywhere {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SearchEverywhere")
            .w(rems(46.))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.picker.update(cx, |picker, cx| {
                    let next = next_tab(picker.delegate.tab, 1);
                    picker.delegate.set_tab(next, window, cx);
                })
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.picker.update(cx, |picker, cx| {
                    let previous = next_tab(picker.delegate.tab, -1);
                    picker.delegate.set_tab(previous, window, cx);
                })
            }))
            .child(self.picker.clone())
    }
}

#[derive(Clone)]
enum ResultItem {
    File {
        path: ProjectPath,
        display: SharedString,
        positions: Vec<usize>,
    },
    Symbol {
        symbol: Symbol,
        is_class: bool,
        positions: Vec<usize>,
    },
    Action {
        index: usize,
        positions: Vec<usize>,
    },
    Text {
        buffer: Entity<Buffer>,
        range: Range<language::Anchor>,
        path: SharedString,
        line: SharedString,
        row: u32,
    },
}

impl ResultItem {
    fn group(&self) -> &'static str {
        match self {
            ResultItem::File { .. } => "Files",
            ResultItem::Symbol { is_class: true, .. } => "Classes",
            ResultItem::Symbol { .. } => "Symbols",
            ResultItem::Action { .. } => "Actions",
            ResultItem::Text { .. } => "Text",
        }
    }
}

pub struct SearchEverywhereDelegate {
    search_everywhere: WeakEntity<SearchEverywhere>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    previous_focus: FocusHandle,
    tab: Tab,
    actions: Vec<ActionEntry>,
    recent: Vec<ProjectPath>,
    results: Vec<ResultItem>,
    selected_index: usize,
    search_id: usize,
    cancel_flag: Arc<AtomicBool>,
    query: String,
}

impl SearchEverywhereDelegate {
    fn set_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.tab = tab;
        let query = self.query.clone();
        let task = self.update_matches(query, window, cx);
        task.detach();
        cx.notify();
    }

    fn search_files(&self, query: &str, limit: usize, cx: &App) -> Task<Vec<ResultItem>> {
        let project = self.project.read(cx);
        if query.is_empty() {
            let items = self
                .recent
                .iter()
                .take(limit)
                .map(|path| ResultItem::File {
                    display: path.path.as_unix_str().to_string().into(),
                    path: path.clone(),
                    positions: Vec::new(),
                })
                .collect();
            return Task::ready(items);
        }
        let worktrees = project.visible_worktrees(cx).collect::<Vec<_>>();
        let include_root_name = worktrees.len() > 1;
        let candidate_sets = worktrees
            .iter()
            .map(|worktree| PathMatchCandidateSet {
                snapshot: worktree.read(cx).snapshot(),
                include_ignored: false,
                include_root_name,
                candidates: project::Candidates::Files,
            })
            .collect::<Vec<_>>();
        let query = query.to_string();
        let cancel_flag = self.cancel_flag.clone();
        let executor = cx.background_executor().clone();
        cx.background_spawn(async move {
            fuzzy_nucleo::match_path_sets(
                candidate_sets.as_slice(),
                &query,
                &None,
                fuzzy_nucleo::Case::Ignore,
                limit,
                &cancel_flag,
                executor,
            )
            .await
            .into_iter()
            .map(|path_match| {
                let display: String = format!(
                    "{}{}",
                    path_match.path_prefix.as_unix_str(),
                    path_match.path.as_unix_str()
                );
                ResultItem::File {
                    path: ProjectPath {
                        worktree_id: project::WorktreeId::from_usize(path_match.worktree_id),
                        path: path_match.path.clone(),
                    },
                    display: display.into(),
                    positions: path_match.positions,
                }
            })
            .collect()
        })
    }

    fn search_symbols(
        &self,
        query: &str,
        classes_only: bool,
        limit: usize,
        cx: &mut App,
    ) -> Task<Vec<ResultItem>> {
        if query.is_empty() {
            return Task::ready(Vec::new());
        }
        let symbols = self.project.update(cx, |project, cx| project.symbols(query, cx));
        let query = query.to_string();
        let cancel_flag = self.cancel_flag.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let Some(symbols) = symbols.await.log_err() else {
                return Vec::new();
            };
            let symbols: Vec<Symbol> = symbols
                .into_iter()
                .filter(|symbol| !classes_only || is_class_kind(symbol.kind))
                .collect();
            let candidates: Vec<StringMatchCandidate> = symbols
                .iter()
                .enumerate()
                .map(|(index, symbol)| StringMatchCandidate::new(index, symbol.label.filter_text()))
                .collect();
            let matches: Vec<StringMatch> =
                fuzzy::match_strings(&candidates, &query, false, true, limit, &cancel_flag, executor)
                    .await;
            matches
                .into_iter()
                .filter_map(|string_match| {
                    let symbol = symbols.get(string_match.candidate_id)?.clone();
                    Some(ResultItem::Symbol {
                        is_class: is_class_kind(symbol.kind),
                        symbol,
                        positions: string_match.positions,
                    })
                })
                .collect()
        })
    }

    fn search_actions(&self, query: &str, limit: usize, cx: &App) -> Task<Vec<ResultItem>> {
        if query.is_empty() {
            return Task::ready(Vec::new());
        }
        let candidates: Vec<StringMatchCandidate> = self
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| StringMatchCandidate::new(index, &action.name))
            .collect();
        let query = query.to_string();
        let cancel_flag = self.cancel_flag.clone();
        let executor = cx.background_executor().clone();
        cx.background_spawn(async move {
            fuzzy::match_strings(&candidates, &query, false, true, limit, &cancel_flag, executor)
                .await
                .into_iter()
                .map(|string_match| ResultItem::Action {
                    index: string_match.candidate_id,
                    positions: string_match.positions,
                })
                .collect()
        })
    }

    fn search_text(&self, query: &str, limit: usize, cx: &mut App) -> Task<Vec<ResultItem>> {
        if query.chars().count() < 2 {
            return Task::ready(Vec::new());
        }
        let Some(search_query) = SearchQuery::text(
            query,
            false,
            false,
            false,
            PathMatcher::default(),
            PathMatcher::default(),
            false,
            None,
        )
        .log_err() else {
            return Task::ready(Vec::new());
        };
        let results = self
            .project
            .update(cx, |project, cx| project.search(search_query, cx));
        cx.spawn(async move |cx| {
            let _task = results.task_handle;
            let mut receiver = std::pin::pin!(results.rx);
            let mut items = Vec::new();
            while let Some(result) = receiver.next().await {
                let project::search::SearchResult::Buffer { buffer, ranges } = result else {
                    continue;
                };
                let found = cx.update(|cx| {
                    let buffer_ref = buffer.read(cx);
                    let snapshot = buffer_ref.snapshot();
                    let path: SharedString = buffer_ref
                        .file()
                        .map(|file| file.path().as_unix_str().to_string())
                        .unwrap_or_default()
                        .into();
                    ranges
                        .into_iter()
                        .map(|range| {
                            let start = range.start.to_point(&snapshot);
                            let line_end = snapshot.line_len(start.row);
                            let line: String = snapshot
                                .text_for_range(Point::new(start.row, 0)..Point::new(start.row, line_end))
                                .collect();
                            ResultItem::Text {
                                buffer: buffer.clone(),
                                range,
                                path: path.clone(),
                                line: line.trim().to_string().into(),
                                row: start.row,
                            }
                        })
                        .collect::<Vec<_>>()
                });
                items.extend(found);
                if items.len() >= limit {
                    items.truncate(limit);
                    break;
                }
            }
            items
        })
    }

    fn open_in_editor(
        &self,
        buffer_task: Task<anyhow::Result<Entity<Buffer>>>,
        position: impl Fn(&Buffer) -> Point + 'static,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let buffer = buffer_task.await?;
            workspace.update_in(cx, |workspace, window, cx| {
                let point = position(buffer.read(cx));
                let editor = workspace.open_project_item::<Editor>(
                    None, buffer, true, true, true, true, window, cx,
                );
                editor.update(cx, |editor, cx| {
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()),
                        window,
                        cx,
                        |selections| selections.select_ranges([point..point]),
                    );
                });
            })
        })
        .detach_and_log_err(cx);
    }
}

fn is_class_kind(kind: language::SymbolKind) -> bool {
    use language::SymbolKind;
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Enum
            | SymbolKind::Struct
            | SymbolKind::TypeParameter
            | SymbolKind::Module
    )
}

impl PickerDelegate for SearchEverywhereDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "search everywhere"
    }

    fn match_count(&self) -> usize {
        self.results.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, index: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = index;
    }

    fn placeholder_text(&self, _: &mut Window, _: &mut App) -> Arc<str> {
        match self.tab {
            Tab::All => "Search everywhere".into(),
            Tab::Classes => "Search classes".into(),
            Tab::Files => "Search files".into(),
            Tab::Symbols => "Search symbols".into(),
            Tab::Actions => "Search actions".into(),
            Tab::Text => "Search text in files".into(),
        }
    }

    fn separators_after_indices(&self) -> Vec<usize> {
        if self.tab != Tab::All {
            return Vec::new();
        }
        self.results
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[0].group() != pair[1].group())
            .map(|(index, _)| index)
            .collect()
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.query = query.clone();
        self.cancel_flag.store(true, Ordering::Release);
        self.cancel_flag = Arc::new(AtomicBool::new(false));
        self.search_id += 1;
        let search_id = self.search_id;
        let query = query.trim().to_string();
        let searches: Vec<Task<Vec<ResultItem>>> = match self.tab {
            // JetBrains' All tab: classes, files, symbols and actions, grouped.
            Tab::All => vec![
                self.search_symbols(&query, true, ALL_TAB_LIMIT, cx),
                self.search_files(&query, ALL_TAB_LIMIT, cx),
                self.search_symbols(&query, false, ALL_TAB_LIMIT, cx),
                self.search_actions(&query, ALL_TAB_LIMIT, cx),
            ],
            Tab::Classes => vec![self.search_symbols(&query, true, TAB_LIMIT, cx)],
            Tab::Files => vec![self.search_files(&query, TAB_LIMIT, cx)],
            Tab::Symbols => vec![self.search_symbols(&query, false, TAB_LIMIT, cx)],
            Tab::Actions => vec![self.search_actions(&query, TAB_LIMIT, cx)],
            Tab::Text => vec![self.search_text(&query, TAB_LIMIT, cx)],
        };
        let all_tab = self.tab == Tab::All;
        cx.spawn_in(window, async move |picker, cx| {
            let mut results = Vec::new();
            for search in searches {
                results.extend(search.await);
            }
            if all_tab {
                dedupe_all_tab(&mut results);
            }
            picker
                .update(cx, |picker, cx| {
                    if picker.delegate.search_id == search_id {
                        picker.delegate.results = results;
                        picker.delegate.selected_index = 0;
                        cx.notify();
                    }
                })
                .log_err();
        })
    }

    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(item) = self.results.get(self.selected_index).cloned() else {
            return;
        };
        match item {
            ResultItem::File { path, .. } => {
                if let Some(workspace) = self.workspace.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        let pane = secondary.then(|| workspace.adjacent_pane(window, cx));
                        workspace
                            .open_path(path, pane.map(|pane| pane.downgrade()), true, window, cx)
                            .detach_and_log_err(cx);
                    });
                }
            }
            ResultItem::Symbol { symbol, .. } => {
                let buffer = self
                    .project
                    .update(cx, |project, cx| project.open_buffer_for_symbol(&symbol, cx));
                let start = symbol.range.start;
                self.open_in_editor(
                    buffer,
                    move |buffer| buffer.point_utf16_to_point(buffer.clip_point_utf16(start, Bias::Left)),
                    window,
                    cx,
                );
            }
            ResultItem::Text { buffer, range, .. } => {
                self.open_in_editor(
                    Task::ready(Ok(buffer)),
                    move |buffer| range.start.to_point(buffer),
                    window,
                    cx,
                );
            }
            ResultItem::Action { index, .. } => {
                let Some(action) = self.actions.get(index).map(|entry| entry.action.boxed_clone())
                else {
                    return;
                };
                window.focus(&self.previous_focus, cx);
                self.dismissed(window, cx);
                window.dispatch_action(action, cx);
                return;
            }
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.search_everywhere
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_header(&self, _: &mut Window, cx: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let colors = cx.theme().colors();
        Some(
            h_flex()
                .px_2()
                .pt_1()
                .gap_1()
                .border_b_1()
                .border_color(colors.border_variant)
                .children(TABS.iter().map(|&tab| {
                    let active = tab == self.tab;
                    div()
                        .id(tab.label())
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(active, |this| this.border_b_2().border_color(colors.text_accent))
                        .child(
                            Label::new(tab.label())
                                .size(LabelSize::Small)
                                .color(if active { Color::Default } else { Color::Muted }),
                        )
                        .on_click(cx.listener(move |picker, _, window, cx| {
                            picker.delegate.set_tab(tab, window, cx);
                        }))
                }))
                .into_any_element(),
        )
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let item = self.results.get(index)?;
        let show_group = self.tab == Tab::All
            && self
                .results
                .get(index.wrapping_sub(1))
                .is_none_or(|previous| previous.group() != item.group());
        let (icon, title, detail, end): (IconName, AnyElement, Option<SharedString>, Option<AnyElement>) =
            match item {
                ResultItem::File {
                    display, positions, ..
                } => {
                    let (directory, file_name) = match display.rsplit_once('/') {
                        Some((directory, file_name)) => (Some(directory.to_string()), file_name.to_string()),
                        None => (None, display.to_string()),
                    };
                    let offset = directory.as_ref().map_or(0, |directory| directory.len() + 1);
                    let name_positions: Vec<usize> = positions
                        .iter()
                        .filter_map(|position| position.checked_sub(offset))
                        .collect();
                    (
                        IconName::File,
                        HighlightedLabel::new(file_name, name_positions).into_any_element(),
                        directory.map(Into::into),
                        None,
                    )
                }
                ResultItem::Symbol {
                    symbol,
                    is_class,
                    positions,
                } => (
                    if *is_class { IconName::Code } else { IconName::Hash },
                    HighlightedLabel::new(symbol.label.filter_text().to_string(), positions.clone())
                        .into_any_element(),
                    match &symbol.path {
                        project::lsp_store::SymbolLocation::InProject(path) => {
                            Some(path.path.as_unix_str().to_string().into())
                        }
                        project::lsp_store::SymbolLocation::OutsideProject { abs_path, .. } => {
                            Some(abs_path.to_string_lossy().to_string().into())
                        }
                    },
                    None,
                ),
                ResultItem::Action { index, positions } => {
                    let entry = self.actions.get(*index)?;
                    (
                        IconName::Command,
                        HighlightedLabel::new(entry.name.clone(), positions.clone()).into_any_element(),
                        None,
                        Some(
                            KeyBinding::for_action_in(&*entry.action, &self.previous_focus, cx)
                                .into_any_element(),
                        ),
                    )
                }
                ResultItem::Text {
                    path, line, row, ..
                } => (
                    IconName::MagnifyingGlass,
                    Label::new(line.clone()).single_line().into_any_element(),
                    Some(format!("{path}:{}", row + 1).into()),
                    None,
                ),
            };
        let _ = window;
        Some(
            ListItem::new(index)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(title)
                        .children(detail.map(|detail| {
                            Label::new(detail)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate()
                        })),
                )
                .end_slot(
                    h_flex()
                        .gap_2()
                        .children(end)
                        .when(show_group, |this| {
                            this.child(
                                Label::new(item.group())
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// In the All tab a class also comes back from the symbol search; keep only its Classes row.
fn dedupe_all_tab(results: &mut Vec<ResultItem>) {
    let mut seen = std::collections::HashSet::new();
    results.retain(|item| match item {
        ResultItem::Symbol { symbol, .. } => {
            seen.insert((symbol.label.text.clone(), symbol.range.start, format!("{:?}", symbol.path)))
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{TestAppContext, VisualTestContext};
    use menu::Confirm;
    use project::Project;
    use serde_json::json;
    use util::path;
    use workspace::{AppState, MultiWorkspace};

    fn search_everywhere(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<Picker<SearchEverywhereDelegate>> {
        workspace.update(cx, |workspace, cx| {
            workspace
                .active_modal::<SearchEverywhere>(cx)
                .expect("Search Everywhere is open")
                .read(cx)
                .picker
                .clone()
        })
    }

    #[gpui::test]
    async fn test_files_tab_opens_the_file(cx: &mut TestAppContext) {
        let app_state = cx.update(|cx| {
            let state = AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            init(cx);
            state
        });
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/root"), json!({ "src": { "banana.rs": "", "apple.rs": "" } }))
            .await;
        let project = Project::test(app_state.fs.clone(), [path!("/root").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());

        cx.dispatch_action(Toggle { tab: Tab::Files });
        let picker = search_everywhere(&workspace, cx);
        cx.simulate_input("bana");
        cx.run_until_parked();
        picker.update(cx, |picker, _| {
            assert_eq!(picker.delegate.tab, Tab::Files);
            assert_eq!(picker.delegate.results.len(), 1);
        });

        // Toggling the open tab again moves to the next tab, like JetBrains.
        cx.dispatch_action(Toggle { tab: Tab::Files });
        picker.update(cx, |picker, _| assert_eq!(picker.delegate.tab, Tab::Symbols));
        cx.dispatch_action(PreviousTab);
        cx.run_until_parked();

        cx.dispatch_action(Confirm);
        cx.run_until_parked();
        cx.read(|cx| {
            let editor = workspace.read(cx).active_item_as::<Editor>(cx).expect("editor opened");
            assert_eq!(editor.read(cx).title(cx), "banana.rs");
        });
    }

    #[test]
    fn test_next_tab_wraps() {
        assert_eq!(next_tab(Tab::All, 1), Tab::Classes);
        assert_eq!(next_tab(Tab::Text, 1), Tab::All);
        assert_eq!(next_tab(Tab::All, -1), Tab::Text);
    }
}
