use super::*;
use crate::branch_actions::{CompareHandler, RefKind, branch_actions_menu};
use collections::HashSet;
use git::repository::LogFilter;
use gpui::MouseDownEvent;
use std::{rc::Rc, str::FromStr as _};

/// The JetBrains Log "Branches" pane: HEAD, Local, Remote and Tags, with folders by `/`.
pub(super) struct BranchesPaneState {
    pub(super) open: bool,
    collapsed: HashSet<SharedString>,
    tags: Vec<(SharedString, SharedString)>,
    _tags_task: Option<Task<()>>,
    tags_scan_id: Option<u64>,
}

impl Default for BranchesPaneState {
    fn default() -> Self {
        Self {
            open: true,
            collapsed: HashSet::default(),
            tags: Vec::new(),
            _tags_task: None,
            tags_scan_id: None,
        }
    }
}

#[derive(Clone)]
struct RefRow {
    kind: RefKind,
    /// Full ref name as git accepts it (`main`, `origin/main`, `v1.0`).
    name: SharedString,
    sha: Option<SharedString>,
}

enum Row {
    Group {
        key: SharedString,
        label: SharedString,
        depth: usize,
        icon: IconName,
    },
    Ref {
        row: RefRow,
        label: SharedString,
        depth: usize,
        is_current: bool,
    },
}

impl GitGraph {
    fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        let Some(repository) = self.get_repository(cx) else {
            return;
        };
        let scan_id = repository.read(cx).scan_id;
        if self.branches_pane.tags_scan_id == Some(scan_id) {
            return;
        }
        self.branches_pane.tags_scan_id = Some(scan_id);
        let output = repository.update(cx, |repository, _| {
            repository.git_output(vec![
                "for-each-ref".into(),
                "--sort=-creatordate".into(),
                "--format=%(refname:short)%09%(objectname)%09%(*objectname)".into(),
                "refs/tags".into(),
            ])
        });
        self.branches_pane._tags_task = Some(cx.spawn(async move |this, cx| {
            let Some(output) = output
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result)
                .log_err()
            else {
                return;
            };
            let tags = output
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split('\t');
                    let name = parts.next()?;
                    let object = parts.next()?;
                    // Annotated tags point at a tag object; `*objectname` is the commit.
                    let commit = parts.next().filter(|peeled| !peeled.is_empty()).unwrap_or(object);
                    Some((SharedString::from(name.to_string()), SharedString::from(commit.to_string())))
                })
                .collect();
            this.update(cx, |this, cx| {
                this.branches_pane.tags = tags;
                cx.notify();
            })
            .ok();
        }));
    }

    fn branch_rows(&self, cx: &App) -> Vec<Row> {
        let Some(repository) = self.get_repository(cx) else {
            return Vec::new();
        };
        let repository = repository.read(cx);
        let current = repository.branch.as_ref().map(|branch| branch.name().to_string());
        let collapsed = &self.branches_pane.collapsed;
        let mut rows = Vec::new();

        rows.push(Row::Ref {
            row: RefRow {
                kind: RefKind::Head,
                name: "HEAD".into(),
                sha: repository.head_commit.as_ref().map(|commit| commit.sha.clone()),
            },
            label: "HEAD (Current Branch)".into(),
            depth: 0,
            is_current: false,
        });

        let local: Vec<RefRow> = repository
            .branch_list
            .iter()
            .filter(|branch| !branch.is_remote())
            .map(|branch| RefRow {
                kind: RefKind::Local,
                name: branch.name().to_string().into(),
                sha: branch.most_recent_commit.as_ref().map(|commit| commit.sha.clone()),
            })
            .collect();
        push_tree(&mut rows, "local", "Local", IconName::GitBranch, local, collapsed, &current);

        let remote: Vec<RefRow> = repository
            .branch_list
            .iter()
            .filter(|branch| branch.is_remote())
            .map(|branch| RefRow {
                kind: RefKind::Remote,
                name: branch.name().to_string().into(),
                sha: branch.most_recent_commit.as_ref().map(|commit| commit.sha.clone()),
            })
            .collect();
        push_tree(&mut rows, "remote", "Remote", IconName::Public, remote, collapsed, &None);

        let tags: Vec<RefRow> = self
            .branches_pane
            .tags
            .iter()
            .map(|(name, sha)| RefRow {
                kind: RefKind::Tag,
                name: name.clone(),
                sha: Some(sha.clone()),
            })
            .collect();
        push_tree(&mut rows, "tags", "Tags", IconName::Bookmark, tags, collapsed, &None);
        rows
    }

    pub(super) fn render_branches_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_tags(cx);
        let rows = self.branch_rows(cx);
        let entity = cx.entity().downgrade();
        let items = rows.into_iter().enumerate().map(|(index, row)| match row {
            Row::Group {
                key,
                label,
                depth,
                icon,
            } => {
                let expanded = !self.branches_pane.collapsed.contains(&key);
                let entity = entity.clone();
                ListItem::new(("branches-group", index))
                    .indent_level(depth)
                    .indent_step_size(px(12.))
                    .spacing(ui::ListItemSpacing::Sparse)
                    .toggle(expanded)
                    .start_slot(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                    .child(Label::new(label).size(LabelSize::Small))
                    .on_click(move |_, _, cx| {
                        let key = key.clone();
                        entity
                            .update(cx, |this, cx| {
                                if !this.branches_pane.collapsed.remove(&key) {
                                    this.branches_pane.collapsed.insert(key);
                                }
                                cx.notify();
                            })
                            .ok();
                    })
                    .into_any_element()
            }
            Row::Ref {
                row,
                label,
                depth,
                is_current,
            } => {
                let icon = match row.kind {
                    RefKind::Head => IconName::GitBranch,
                    RefKind::Local | RefKind::Remote => IconName::GitBranch,
                    RefKind::Tag => IconName::Bookmark,
                };
                let click_entity = entity.clone();
                let menu_entity = entity.clone();
                let click_row = row.clone();
                ListItem::new(("branches-ref", index))
                    .indent_level(depth)
                    .indent_step_size(px(12.))
                    .spacing(ui::ListItemSpacing::Sparse)
                    .start_slot(
                        Icon::new(if is_current { IconName::Star } else { icon })
                            .size(IconSize::Small)
                            .color(if is_current { Color::Warning } else { Color::Muted }),
                    )
                    .child(Label::new(label).size(LabelSize::Small).truncate())
                    .on_click(move |event, _, cx| {
                        let row = click_row.clone();
                        let double_click = event.click_count() >= 2;
                        click_entity
                            .update(cx, |this, cx| {
                                if double_click {
                                    this.set_log_filter(
                                        LogFilter {
                                            branches: vec![row.name.clone()],
                                            ..this.log_filter()
                                        },
                                        cx,
                                    );
                                } else if let Some(sha) = row.sha.clone() {
                                    this.select_commit_by_sha(sha.as_ref(), cx);
                                }
                            })
                            .ok();
                    })
                    .on_secondary_mouse_down(move |event: &MouseDownEvent, window, cx| {
                        let row = row.clone();
                        let position = event.position;
                        menu_entity
                            .update(cx, |this, cx| {
                                this.deploy_branch_context_menu(row, position, window, cx)
                            })
                            .ok();
                    })
                    .into_any_element()
            }
        });

        let colors = cx.theme().colors();
        v_flex()
            .id("git-log-branches-pane")
            .w(px(220.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(colors.border_variant)
            .py_1()
            .overflow_y_scroll()
            .children(items)
    }

    fn deploy_branch_context_menu(
        &mut self,
        row: RefRow,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repository) = self.get_repository(cx) else {
            return;
        };
        let is_local_project = self
            .workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).project().read(cx).is_local());
        if !is_local_project {
            // ponytail: branch actions are local-only, like the log actions.
            return;
        }
        let graph = cx.entity().downgrade();
        let on_compare: CompareHandler = Rc::new(move |range, _, cx| {
            graph
                .update(cx, |graph, cx| {
                    graph.set_log_filter(
                        LogFilter {
                            branches: vec![range],
                            ..LogFilter::default()
                        },
                        cx,
                    )
                })
                .ok();
        });
        let sha = row.sha.as_ref().and_then(|sha| Oid::from_str(sha).ok());
        let menu = branch_actions_menu(
            row.kind,
            row.name,
            sha,
            self.workspace.clone(),
            repository,
            Some(on_compare),
            window,
            cx,
        );
        self.set_context_menu(menu, position, None, window, cx);
    }
}

fn push_tree(
    rows: &mut Vec<Row>,
    key: &'static str,
    label: &'static str,
    icon: IconName,
    refs: Vec<RefRow>,
    collapsed: &HashSet<SharedString>,
    current: &Option<String>,
) {
    if refs.is_empty() {
        return;
    }
    let root_key = SharedString::from(key);
    let expanded = !collapsed.contains(&root_key);
    rows.push(Row::Group {
        key: root_key,
        label: label.into(),
        depth: 0,
        icon,
    });
    if !expanded {
        return;
    }
    let mut open_folders: Vec<String> = Vec::new();
    let mut hidden_below: Option<usize> = None;
    let mut sorted = refs;
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for reference in sorted {
        let parts: Vec<&str> = reference.name.split('/').collect();
        let (folders, leaf) = parts.split_at(parts.len().saturating_sub(1));
        let common = open_folders
            .iter()
            .zip(folders.iter())
            .take_while(|(open, folder)| open.as_str() == **folder)
            .count();
        open_folders.truncate(common);
        if hidden_below.is_some_and(|level| common < level) {
            hidden_below = None;
        }
        for folder in &folders[common..] {
            open_folders.push(folder.to_string());
            let level = open_folders.len();
            let folder_key: SharedString = format!("{key}/{}", open_folders.join("/")).into();
            if hidden_below.is_none() {
                rows.push(Row::Group {
                    key: folder_key.clone(),
                    label: folder.to_string().into(),
                    depth: level,
                    icon: IconName::Folder,
                });
                if collapsed.contains(&folder_key) {
                    hidden_below = Some(level);
                }
            }
        }
        if hidden_below.is_some() {
            continue;
        }
        let is_current = current
            .as_ref()
            .is_some_and(|current| current == reference.name.as_ref());
        rows.push(Row::Ref {
            label: leaf.first().copied().unwrap_or_default().to_string().into(),
            depth: folders.len() + 1,
            is_current,
            row: reference,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str) -> RefRow {
        RefRow {
            kind: RefKind::Tag,
            name: name.to_string().into(),
            sha: None,
        }
    }

    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Group { label, depth, .. } => format!("{depth}:{label}/"),
                Row::Ref { label, depth, .. } => format!("{depth}:{label}"),
            })
            .collect()
    }

    #[test]
    fn test_push_tree_groups_by_folder_and_collapses() {
        let refs = || {
            vec![
                tag("main"),
                tag("feature/b/two"),
                tag("feature/a"),
                tag("feature/b/one"),
                tag("zeta"),
            ]
        };
        let mut rows = Vec::new();
        push_tree(&mut rows, "t", "Tags", IconName::Bookmark, refs(), &HashSet::default(), &None);
        assert_eq!(
            outline(&rows),
            [
                "0:Tags/", "1:feature/", "2:a", "2:b/", "3:one", "3:two", "1:main", "1:zeta"
            ]
        );

        let collapsed: HashSet<SharedString> = HashSet::from_iter(["t/feature/b".into()]);
        let mut rows = Vec::new();
        push_tree(&mut rows, "t", "Tags", IconName::Bookmark, refs(), &collapsed, &None);
        assert_eq!(
            outline(&rows),
            ["0:Tags/", "1:feature/", "2:a", "2:b/", "1:main", "1:zeta"]
        );

        let collapsed: HashSet<SharedString> = HashSet::from_iter(["t".into()]);
        let mut rows = Vec::new();
        push_tree(&mut rows, "t", "Tags", IconName::Bookmark, refs(), &collapsed, &None);
        assert_eq!(outline(&rows), ["0:Tags/"]);
    }
}
