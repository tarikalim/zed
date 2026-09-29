use anyhow::Context as _;
use collections::HashSet;
use editor::{
    Anchor, Editor, RowHighlightOptions, ToPoint as _,
    display_map::{BlockPlacement, BlockProperties, BlockStyle, CustomBlockId},
};
use git::repository::RepoPath;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, PromptLevel, Render,
    SharedString, WeakEntity, Window,
};
use language::{Buffer, Point, line_diff};
use project::{Project, git_store::Repository};
use std::{ops::Range, sync::Arc};
use ui::prelude::*;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
    notifications::NotifyTaskExt,
};

/// One region of a line-based 3-way merge, in line indices of each text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MergeChunk {
    Stable(Range<usize>),
    Change {
        base: Range<usize>,
        ours: Range<usize>,
        theirs: Range<usize>,
        changed_ours: bool,
        changed_theirs: bool,
        conflict: bool,
    },
}

fn split_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// A diff3-style merge: hunks of base→ours and base→theirs that overlap or touch form one chunk,
/// which conflicts when both sides changed it differently.
pub(crate) fn merge3(base: &str, ours: &str, theirs: &str) -> Vec<MergeChunk> {
    let ours_lines = split_lines(ours);
    let theirs_lines = split_lines(theirs);
    let base_len = split_lines(base).len();
    let to_usize = |range: Range<u32>| range.start as usize..range.end as usize;
    let mut ours_hunks = line_diff(base, ours)
        .into_iter()
        .map(|(b, o)| (to_usize(b), to_usize(o)))
        .peekable();
    let mut theirs_hunks = line_diff(base, theirs)
        .into_iter()
        .map(|(b, t)| (to_usize(b), to_usize(t)))
        .peekable();

    let mut chunks = Vec::new();
    let mut base_position = 0;
    let mut ours_delta: isize = 0;
    let mut theirs_delta: isize = 0;
    loop {
        let start = match (ours_hunks.peek(), theirs_hunks.peek()) {
            (None, None) => break,
            (Some((b, _)), None) | (None, Some((b, _))) => b.start,
            (Some((ours_base, _)), Some((theirs_base, _))) => ours_base.start.min(theirs_base.start),
        };
        if start > base_position {
            chunks.push(MergeChunk::Stable(base_position..start));
        }
        let (ours_delta_before, theirs_delta_before) = (ours_delta, theirs_delta);
        let mut end = start;
        let (mut changed_ours, mut changed_theirs) = (false, false);
        loop {
            let mut grew = false;
            while let Some((b, o)) = ours_hunks.next_if(|(b, _)| b.start <= end) {
                end = end.max(b.end);
                ours_delta += o.len() as isize - b.len() as isize;
                changed_ours = true;
                grew = true;
            }
            while let Some((b, t)) = theirs_hunks.next_if(|(b, _)| b.start <= end) {
                end = end.max(b.end);
                theirs_delta += t.len() as isize - b.len() as isize;
                changed_theirs = true;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        let shift = |position: usize, delta: isize| (position as isize + delta).max(0) as usize;
        let ours = shift(start, ours_delta_before)..shift(end, ours_delta);
        let theirs = shift(start, theirs_delta_before)..shift(end, theirs_delta);
        let conflict = changed_ours
            && changed_theirs
            && ours_lines.get(ours.clone()) != theirs_lines.get(theirs.clone());
        chunks.push(MergeChunk::Change {
            base: start..end,
            ours,
            theirs,
            changed_ours,
            changed_theirs,
            conflict,
        });
        base_position = end;
    }
    if base_position < base_len {
        chunks.push(MergeChunk::Stable(base_position..base_len));
    }
    chunks
}

pub(crate) fn open(
    workspace: &mut Workspace,
    repository: Entity<Repository>,
    repo_path: RepoPath,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    let workspace_handle = workspace.weak_handle();
    let show = |stage: u8, repository: &Entity<Repository>, cx: &mut Context<Workspace>| {
        let spec = format!(":{stage}:{}", repo_path.as_unix_str());
        repository.update(cx, |repository, _| {
            repository.git_output(vec!["show".into(), spec])
        })
    };
    // During a rebase git's stage 2 is the branch being rebased onto; the user's own commit is stage 3.
    let rebasing = crate::conflicts_dialog::is_rebasing(&repository.read(cx).repository_dir_abs_path);
    let (yours_stage, theirs_stage) = if rebasing { (3, 2) } else { (2, 3) };
    let base = show(1, &repository, cx);
    let ours = show(yours_stage, &repository, cx);
    let theirs = show(theirs_stage, &repository, cx);
    let (current_branch, incoming) = {
        let repository = repository.read(cx);
        let incoming = repository
            .merge
            .merge_heads_by_conflicted_path
            .get(&repo_path)
            .and_then(|heads| heads.iter().flatten().next().cloned());
        (
            repository
                .branch
                .as_ref()
                .map(|branch| SharedString::from(branch.name().to_string())),
            incoming,
        )
    };
    let languages = project.read(cx).languages().clone();
    let path_for_language = repo_path.as_std_path().to_path_buf();

    let task = cx.spawn_in(window, async move |workspace, cx| {
        // An add/add conflict has no base stage.
        let base = base.await?.unwrap_or_default();
        let ours = ours.await?.context("this side deleted the file; use Accept Yours or Accept Theirs")?;
        let theirs = theirs.await?.context("this side deleted the file; use Accept Yours or Accept Theirs")?;
        // Text is read lossily; applying a lossy result would corrupt non-UTF-8 files.
        anyhow::ensure!(
            ![&base, &ours, &theirs].iter().any(|text| text.contains('\u{FFFD}')),
            "The merge tool only supports UTF-8 text files; use Accept Yours or Accept Theirs"
        );
        let language = languages
            .load_language_for_file_path(&path_for_language)
            .await
            .ok();
        workspace.update_in(cx, |workspace, window, cx| {
            let merge_tool = cx.new(|cx| {
                MergeTool::new(
                    MergeInput {
                        project,
                        repository,
                        repo_path,
                        workspace: workspace_handle,
                        base,
                        ours,
                        theirs,
                        language,
                        left_label: if rebasing {
                            "your commit".into()
                        } else {
                            current_branch.clone().unwrap_or_else(|| "yours".into())
                        },
                        right_label: if rebasing {
                            current_branch.unwrap_or_else(|| "upstream".into())
                        } else {
                            incoming.unwrap_or_else(|| "theirs".into())
                        },
                        rebasing,
                    },
                    window,
                    cx,
                )
            });
            workspace.add_item_to_active_pane(Box::new(merge_tool), None, true, window, cx);
        })
    });
    task.detach_and_notify_err(workspace.weak_handle(), window, cx);
}

struct MergeInput {
    project: Entity<Project>,
    repository: Entity<Repository>,
    repo_path: RepoPath,
    workspace: WeakEntity<Workspace>,
    base: String,
    ours: String,
    theirs: String,
    language: Option<Arc<language::Language>>,
    left_label: SharedString,
    right_label: SharedString,
    rebasing: bool,
}

struct Conflict {
    result: Range<Anchor>,
    ours_text: String,
    theirs_text: String,
    left_rows: Range<u32>,
    right_rows: Range<u32>,
    resolved: bool,
}

struct ChangeHighlight {
    left_rows: Range<u32>,
    right_rows: Range<u32>,
}

enum MergeConflictHighlight {}
enum MergeChangeHighlight {}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Left,
    Result,
    Right,
}

pub(crate) struct MergeTool {
    /// Chunk starts: anchor in the result, row in the left and right panes.
    boundaries: Vec<(Anchor, u32, u32)>,
    syncing_scroll: bool,
    repository: Entity<Repository>,
    repo_path: RepoPath,
    workspace: WeakEntity<Workspace>,
    left: Entity<Editor>,
    result: Entity<Editor>,
    right: Entity<Editor>,
    result_buffer: Entity<Buffer>,
    conflicts: Vec<Conflict>,
    changes: Vec<ChangeHighlight>,
    block_ids: HashSet<CustomBlockId>,
    left_label: SharedString,
    right_label: SharedString,
    rebasing: bool,
    focus_handle: FocusHandle,
}

impl MergeTool {
    fn new(input: MergeInput, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chunks = merge3(&input.base, &input.ours, &input.theirs);
        let base_lines = split_lines(&input.base);
        let ours_lines = split_lines(&input.ours);
        let theirs_lines = split_lines(&input.theirs);
        let join = |lines: &[&str], range: &Range<usize>| -> String {
            lines.get(range.clone()).map(|lines| lines.concat()).unwrap_or_default()
        };

        let mut result_text = String::new();
        let mut conflict_points = Vec::new();
        let mut conflicts_text = Vec::new();
        let mut changes = Vec::new();
        let rows = |range: &Range<usize>| range.start as u32..range.end as u32;
        // Row where each chunk starts in (result, left, right), for synced scrolling.
        let mut boundary_rows: Vec<(u32, u32, u32)> = Vec::new();
        let (mut left_row, mut right_row) = (0u32, 0u32);
        for chunk in &chunks {
            let result_row = result_text.matches('\n').count() as u32;
            match chunk {
                MergeChunk::Stable(range) => {
                    boundary_rows.push((result_row, left_row, right_row));
                    left_row += range.len() as u32;
                    right_row += range.len() as u32;
                }
                MergeChunk::Change { ours, theirs, .. } => {
                    boundary_rows.push((result_row, ours.start as u32, theirs.start as u32));
                    left_row = ours.end as u32;
                    right_row = theirs.end as u32;
                }
            }
            match chunk {
                MergeChunk::Stable(range) => result_text.push_str(&join(&base_lines, range)),
                MergeChunk::Change {
                    base,
                    ours,
                    theirs,
                    changed_ours,
                    conflict,
                    ..
                } => {
                    if *conflict {
                        let start = Point::new(result_text.matches('\n').count() as u32, 0);
                        result_text.push_str(&join(&base_lines, base));
                        let end = Point::new(result_text.matches('\n').count() as u32, 0);
                        conflict_points.push(start..end);
                        conflicts_text.push((
                            join(&ours_lines, ours),
                            join(&theirs_lines, theirs),
                            rows(ours),
                            rows(theirs),
                        ));
                    } else {
                        result_text.push_str(&if *changed_ours {
                            join(&ours_lines, ours)
                        } else {
                            join(&theirs_lines, theirs)
                        });
                        changes.push(ChangeHighlight {
                            left_rows: rows(ours),
                            right_rows: rows(theirs),
                        });
                    }
                }
            }
        }

        let project = input.project.clone();
        let make_editor = |text: &str, read_only: bool, window: &mut Window, cx: &mut Context<Self>| {
            let buffer = project.update(cx, |project, cx| {
                project.create_local_buffer(text, input.language.clone(), false, cx)
            });
            let editor = cx.new(|cx| {
                let mut editor = Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx);
                editor.set_read_only(read_only);
                editor
            });
            (buffer, editor)
        };
        let (_, left) = make_editor(&input.ours, true, window, cx);
        let (result_buffer, result) = make_editor(&result_text, false, window, cx);
        let (_, right) = make_editor(&input.theirs, true, window, cx);

        let snapshot = result.read(cx).buffer().read(cx).snapshot(cx);
        let conflicts = conflict_points
            .into_iter()
            .zip(conflicts_text)
            .map(|(points, (ours_text, theirs_text, left_rows, right_rows))| Conflict {
                // Both ends bias left so typing at the start of the following line stays outside.
                result: snapshot.anchor_before(points.start)..snapshot.anchor_before(points.end),
                ours_text,
                theirs_text,
                left_rows,
                right_rows,
                resolved: false,
            })
            .collect();

        let boundaries = boundary_rows
            .into_iter()
            .map(|(result_row, left_row, right_row)| {
                (snapshot.anchor_before(Point::new(result_row, 0)), left_row, right_row)
            })
            .collect();
        for (pane, editor) in [(Pane::Left, &left), (Pane::Result, &result), (Pane::Right, &right)] {
            cx.subscribe_in(editor, window, move |this, _, event: &editor::EditorEvent, window, cx| {
                if let editor::EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    this.sync_scroll(pane, window, cx);
                }
            })
            .detach();
        }

        let mut this = Self {
            boundaries,
            syncing_scroll: false,
            repository: input.repository,
            repo_path: input.repo_path,
            workspace: input.workspace,
            left,
            result,
            right,
            result_buffer,
            conflicts,
            changes,
            block_ids: HashSet::default(),
            left_label: input.left_label,
            right_label: input.right_label,
            rebasing: input.rebasing,
            focus_handle: cx.focus_handle(),
        };
        this.refresh_decorations(cx);
        this
    }

    fn boundary_rows(&self, pane: Pane, cx: &App) -> Vec<f64> {
        let snapshot = self.result.read(cx).buffer().read(cx).snapshot(cx);
        self.boundaries
            .iter()
            .map(|(anchor, left, right)| match pane {
                Pane::Left => *left as f64,
                Pane::Right => *right as f64,
                Pane::Result => anchor.to_point(&snapshot).row as f64,
            })
            .collect()
    }

    // ponytail: maps scroll rows through chunk starts (display rows ~ buffer rows; the one-row
    // conflict control blocks add slight drift). Per-row alignment would need spacer blocks.
    fn sync_scroll(&mut self, source: Pane, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing_scroll {
            return;
        }
        self.syncing_scroll = true;
        let editor = |pane: Pane| match pane {
            Pane::Left => self.left.clone(),
            Pane::Result => self.result.clone(),
            Pane::Right => self.right.clone(),
        };
        let position = editor(source).update(cx, |editor, cx| editor.scroll_position(cx));
        let source_rows = self.boundary_rows(source, cx);
        for target in [Pane::Left, Pane::Result, Pane::Right] {
            if target == source {
                continue;
            }
            let y = map_row(&source_rows, &self.boundary_rows(target, cx), position.y);
            editor(target).update(cx, |editor, cx| {
                editor.set_scroll_position(gpui::point(position.x, y), window, cx);
            });
        }
        self.syncing_scroll = false;
    }

    fn unresolved_count(&self) -> usize {
        self.conflicts.iter().filter(|conflict| !conflict.resolved).count()
    }

    fn refresh_decorations(&mut self, cx: &mut Context<Self>) {
        let options = RowHighlightOptions {
            include_gutter: true,
            ..Default::default()
        };
        let rows_to_anchors = |editor: &Entity<Editor>, rows: &Range<u32>, cx: &App| {
            let snapshot = editor.read(cx).buffer().read(cx).snapshot(cx);
            snapshot.anchor_before(Point::new(rows.start, 0))
                ..snapshot.anchor_before(Point::new(rows.end.saturating_sub(1).max(rows.start), 0))
        };
        let conflict_color = |cx: &App| cx.theme().colors().version_control_conflict_marker_theirs;
        let change_color = |cx: &App| cx.theme().colors().version_control_modified.opacity(0.15);

        for (side, pick) in [(self.left.clone(), true), (self.right.clone(), false)] {
            let conflict_ranges: Vec<_> = self
                .conflicts
                .iter()
                .filter(|conflict| !conflict.resolved)
                .map(|conflict| {
                    let rows = if pick { &conflict.left_rows } else { &conflict.right_rows };
                    rows_to_anchors(&side, rows, cx)
                })
                .collect();
            let change_ranges: Vec<_> = self
                .changes
                .iter()
                .filter(|change| {
                    let rows = if pick { &change.left_rows } else { &change.right_rows };
                    !rows.is_empty()
                })
                .map(|change| {
                    let rows = if pick { &change.left_rows } else { &change.right_rows };
                    rows_to_anchors(&side, rows, cx)
                })
                .collect();
            side.update(cx, |editor, cx| {
                editor.clear_row_highlights::<MergeConflictHighlight>();
                editor.clear_row_highlights::<MergeChangeHighlight>();
                for range in conflict_ranges {
                    editor.highlight_rows::<MergeConflictHighlight>(range, conflict_color, options, cx);
                }
                for range in change_ranges {
                    editor.highlight_rows::<MergeChangeHighlight>(range, change_color, options, cx);
                }
            });
        }

        let this = cx.entity().downgrade();
        let blocks: Vec<_> = self
            .conflicts
            .iter()
            .enumerate()
            .filter(|(_, conflict)| !conflict.resolved)
            .map(|(index, conflict)| {
                let this = this.clone();
                BlockProperties {
                    placement: BlockPlacement::Above(conflict.result.start),
                    height: Some(1),
                    style: BlockStyle::Sticky,
                    render: Arc::new(move |block_cx| render_conflict_controls(index, this.clone(), block_cx)),
                    priority: 0,
                }
            })
            .collect();
        let conflict_ranges: Vec<_> = self
            .conflicts
            .iter()
            .filter(|conflict| !conflict.resolved)
            .map(|conflict| conflict.result.clone())
            .collect();
        let old_blocks = std::mem::take(&mut self.block_ids);
        let new_blocks = self.result.update(cx, |editor, cx| {
            editor.remove_blocks(old_blocks, None, cx);
            editor.clear_row_highlights::<MergeConflictHighlight>();
            for range in conflict_ranges {
                editor.highlight_rows::<MergeConflictHighlight>(range, conflict_color, options, cx);
            }
            editor.insert_blocks(blocks, None, cx)
        });
        self.block_ids = new_blocks.into_iter().collect();
        cx.notify();
    }

    fn resolve(&mut self, index: usize, text: Option<String>, cx: &mut Context<Self>) {
        let Some(conflict) = self.conflicts.get_mut(index) else {
            return;
        };
        if let Some(text) = text {
            let range = conflict.result.clone();
            self.result.update(cx, |editor, cx| editor.edit([(range, text)], cx));
        }
        if let Some(conflict) = self.conflicts.get_mut(index) {
            conflict.resolved = true;
        }
        self.refresh_decorations(cx);
    }

    fn accept_side_for_file(&mut self, left: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The left pane is git's "ours", except during a rebase where the panes are swapped.
        let ours = left != self.rebasing;
        let task = crate::conflicts_dialog::accept_side(&self.repository, self.repo_path.clone(), ours, cx);
        let workspace = self.workspace.clone();
        let item_id = cx.entity_id();
        cx.spawn_in(window, async move |_, cx| {
            task.await?;
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.close_item_by_id(item_id, workspace::SaveIntent::Skip, window, cx)
                        .detach_and_log_err(cx);
                });
            })?;
            anyhow::Ok(())
        })
        .detach_and_notify_err(self.workspace.clone(), window, cx);
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let unresolved = self.unresolved_count();
        let text = self.result_buffer.read(cx).text();
        if unresolved == 0 {
            self.write_and_stage(text, window, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("{unresolved} unresolved conflicts left. Apply the result anyway?"),
            None,
            &["Apply", "Continue Resolving"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                this.update_in(cx, |this, window, cx| this.write_and_stage(text, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn write_and_stage(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.repo_path.as_unix_str().to_string();
        self.finish(vec![vec!["add".into(), "--".into(), path]], Some(text), window, cx);
    }

    fn finish(
        &mut self,
        commands: Vec<Vec<String>>,
        contents: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repository = self.repository.clone();
        let abs_path = self
            .repository
            .read(cx)
            .work_directory_abs_path
            .join(self.repo_path.as_std_path());
        let workspace = self.workspace.clone();
        let fs = workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).project().read(cx).fs().clone());
        let item_id = cx.entity_id();
        cx.spawn_in(window, async move |_, cx| {
            if let (Some(contents), Some(fs)) = (contents, fs) {
                fs.atomic_write(abs_path, contents).await?;
            }
            for args in commands {
                repository
                    .update(cx, |repository, cx| repository.run_git_command(args, cx))
                    .await??;
            }
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.close_item_by_id(item_id, workspace::SaveIntent::Skip, window, cx)
                        .detach_and_log_err(cx);
                });
            })?;
            anyhow::Ok(())
        })
        .detach_and_notify_err(self.workspace.clone(), window, cx);
    }
}

/// Maps a row through matching chunk starts: same offset into the chunk, clamped to its length.
fn map_row(source_starts: &[f64], target_starts: &[f64], row: f64) -> f64 {
    let index = source_starts.iter().rposition(|start| *start <= row).unwrap_or(0);
    let offset = row - source_starts.get(index).copied().unwrap_or(0.);
    let start = target_starts.get(index).copied().unwrap_or(0.);
    let chunk_len = target_starts
        .get(index + 1)
        .map(|next| next - start)
        .unwrap_or(f64::MAX);
    start + offset.min(chunk_len)
}

fn render_conflict_controls(
    index: usize,
    this: WeakEntity<MergeTool>,
    cx: &mut editor::display_map::BlockContext,
) -> AnyElement {
    let button = |id: &'static str, label: &'static str, pick: Option<bool>| {
        let this = this.clone();
        Button::new((id, index), label)
            .label_size(LabelSize::Small)
            .on_click(move |_, _, cx| {
                this.update(cx, |this, cx| {
                    let text = this.conflicts.get(index).and_then(|conflict| match pick {
                        Some(true) => Some(conflict.ours_text.clone()),
                        Some(false) => Some(conflict.theirs_text.clone()),
                        None => None,
                    });
                    this.resolve(index, text, cx);
                })
                .ok();
            })
    };
    h_flex()
        .id(cx.block_id)
        .h(cx.line_height)
        .ml(cx.margins.gutter.width)
        .gap_1()
        .bg(cx.theme().colors().editor_background)
        .child(button("merge-accept-left", "» Accept Left", Some(true)))
        .child(button("merge-accept-right", "Accept Right «", Some(false)))
        .child(button("merge-ignore", "× Ignore", None))
        .into_any_element()
}

impl EventEmitter<ItemEvent> for MergeTool {}

impl Focusable for MergeTool {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.result.focus_handle(cx)
    }
}

impl Item for MergeTool {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _: &App) -> SharedString {
        let name = self
            .repo_path
            .file_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| self.repo_path.as_unix_str().to_string());
        format!("Merge Revisions for {name}").into()
    }

    fn tab_icon(&self, _: &Window, _: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitBranch))
    }

    fn to_item_events(event: &ItemEvent, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for MergeTool {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let unresolved = self.unresolved_count();
        let pane = |label: SharedString, editor: Entity<Editor>| {
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(colors.border_variant)
                        .child(Label::new(label).size(LabelSize::Small).color(Color::Muted)),
                )
                .child(div().flex_1().min_h_0().child(editor))
        };
        v_flex()
            .key_context("GitMergeTool")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(colors.editor_background)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(pane(
                        format!("Changes from {} (read-only)", self.left_label).into(),
                        self.left.clone(),
                    ))
                    .child(div().w_px().h_full().bg(colors.border))
                    .child(pane("Result".into(), self.result.clone()))
                    .child(div().w_px().h_full().bg(colors.border))
                    .child(pane(
                        format!("Changes from {} (read-only)", self.right_label).into(),
                        self.right.clone(),
                    )),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .border_t_1()
                    .border_color(colors.border_variant)
                    .child(
                        Label::new(match unresolved {
                            0 => "All conflicts resolved".to_string(),
                            1 => "1 conflict".to_string(),
                            count => format!("{count} conflicts"),
                        })
                        .size(LabelSize::Small)
                        .color(if unresolved == 0 { Color::Success } else { Color::Muted }),
                    )
                    .child(div().flex_1())
                    .child(Button::new("merge-accept-left-file", "Accept Left").on_click(
                        cx.listener(|this, _, window, cx| this.accept_side_for_file(true, window, cx)),
                    ))
                    .child(Button::new("merge-accept-right-file", "Accept Right").on_click(
                        cx.listener(|this, _, window, cx| this.accept_side_for_file(false, window, cx)),
                    ))
                    .child(
                        Button::new("merge-apply", "Apply")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_row() {
        // Chunks start at rows 0, 10, 12 in the source and 0, 10, 20 in the target.
        let source = [0., 10., 12.];
        let target = [0., 10., 20.];
        assert_eq!(map_row(&source, &target, 5.), 5.);
        assert_eq!(map_row(&source, &target, 11.), 11.);
        assert_eq!(map_row(&source, &target, 15.), 23.);
        // A longer source chunk clamps to the shorter target chunk.
        assert_eq!(map_row(&target, &source, 15.), 12.);
    }

    #[test]
    fn test_merge3() {
        let base = "a\nb\nc\nd\ne\n";
        // Ours edits b, theirs edits d: both apply without conflict.
        let chunks = merge3(base, "a\nB\nc\nd\ne\n", "a\nb\nc\nD\ne\n");
        assert_eq!(
            chunks,
            [
                MergeChunk::Stable(0..1),
                MergeChunk::Change {
                    base: 1..2,
                    ours: 1..2,
                    theirs: 1..2,
                    changed_ours: true,
                    changed_theirs: false,
                    conflict: false,
                },
                MergeChunk::Stable(2..3),
                MergeChunk::Change {
                    base: 3..4,
                    ours: 3..4,
                    theirs: 3..4,
                    changed_ours: false,
                    changed_theirs: true,
                    conflict: false,
                },
                MergeChunk::Stable(4..5),
            ]
        );

        // Both edit c differently, and ours also inserts a line before: one conflict whose
        // ranges account for the earlier insertion.
        let chunks = merge3(base, "x\na\nb\nC1\nd\ne\n", "a\nb\nC2\nd\ne\n");
        let conflicts: Vec<_> = chunks
            .iter()
            .filter(|chunk| matches!(chunk, MergeChunk::Change { conflict: true, .. }))
            .collect();
        assert_eq!(
            conflicts,
            [&MergeChunk::Change {
                base: 2..3,
                ours: 3..4,
                theirs: 2..3,
                changed_ours: true,
                changed_theirs: true,
                conflict: true,
            }]
        );

        // Identical edits on both sides are not a conflict.
        let chunks = merge3(base, "a\nX\nc\nd\ne\n", "a\nX\nc\nd\ne\n");
        assert!(chunks.iter().all(|chunk| !matches!(chunk, MergeChunk::Change { conflict: true, .. })));
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use gpui::{TestAppContext, VisualContext};
    use project::FakeFs;
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use workspace::MultiWorkspace;

    #[gpui::test]
    async fn test_merge_tool_resolves_conflicts(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/repo"), json!({ ".git": {}, "file.txt": "" })).await;
        let project = Project::test(fs, [path!("/repo").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        cx.run_until_parked();
        let repository = project
            .read_with(cx, |project, cx| project.active_repository(cx))
            .expect("fake repository");

        let merge_tool = cx.new_window_entity(|window, cx| {
            MergeTool::new(
                MergeInput {
                    project: project.clone(),
                    repository,
                    repo_path: RepoPath::new("file.txt").unwrap(),
                    workspace: workspace.downgrade(),
                    base: "a\nb\nc\nd\n".into(),
                    ours: "a\nOURS\nc\nd\n".into(),
                    theirs: "a\nTHEIRS\nc\nD\n".into(),
                    language: None,
                    left_label: "main".into(),
                    right_label: "feature".into(),
                    rebasing: false,
                },
                window,
                cx,
            )
        });

        merge_tool.update(cx, |tool, cx| {
            // The non-conflicting change from theirs is applied; the conflict starts as base.
            assert_eq!(tool.result_buffer.read(cx).text(), "a\nb\nc\nD\n");
            assert_eq!(tool.unresolved_count(), 1);
            tool.resolve(0, Some("OURS\n".into()), cx);
            assert_eq!(tool.result_buffer.read(cx).text(), "a\nOURS\nc\nD\n");
            assert_eq!(tool.unresolved_count(), 0);
        });
    }
}
