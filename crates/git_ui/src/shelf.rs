use editor::Editor;
use git::repository::RepoPath;
use gpui::{App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Task, Window};
use menu::{Cancel, Confirm};
use project::git_store::Repository;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::ModalView;

const SHELF_DIR: &str = "zed-shelf";
const PATCH_FILE: &str = "changes.patch";
const META_FILE: &str = "meta.json";

/// One JetBrains "shelved changelist": a patch plus its name and files.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct ShelvedChangelist {
    pub(crate) name: String,
    pub(crate) timestamp: i64,
    pub(crate) files: Vec<String>,
    #[serde(skip)]
    pub(crate) directory: PathBuf,
}

impl ShelvedChangelist {
    pub(crate) fn patch_path(&self) -> PathBuf {
        self.directory.join(PATCH_FILE)
    }
}

pub(crate) fn shelf_dir(git_dir: &Path) -> PathBuf {
    git_dir.join(SHELF_DIR)
}

/// Shelved changelists of a repository, newest first.
pub(crate) fn load(git_dir: &Path) -> Vec<ShelvedChangelist> {
    let Ok(entries) = std::fs::read_dir(shelf_dir(git_dir)) else {
        return Vec::new();
    };
    let mut shelved: Vec<ShelvedChangelist> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let directory = entry.path();
            let text = std::fs::read_to_string(directory.join(META_FILE)).ok()?;
            let mut shelved = serde_json::from_str::<ShelvedChangelist>(&text).log_err()?;
            shelved.directory = directory;
            Some(shelved)
        })
        .collect();
    shelved.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    shelved
}

/// Saves the changes of `paths` (staged and unstaged, relative to HEAD) to the shelf and reverts them.
pub(crate) fn shelve(
    repository: &Entity<Repository>,
    name: String,
    paths: Vec<RepoPath>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let git_dir = repository.read(cx).repository_dir_abs_path.to_path_buf();
    let files: Vec<String> = paths.iter().map(|path| path.as_unix_str().to_string()).collect();
    let diff = repository.update(cx, |repository, _| {
        let mut args = vec!["diff".into(), "--binary".into(), "HEAD".into(), "--".into()];
        args.extend(files.iter().cloned());
        repository.git_output(args)
    });
    let repository = repository.clone();
    cx.spawn(async move |cx| {
        let patch = diff.await??;
        anyhow::ensure!(!patch.trim().is_empty(), "There are no changes to shelve.");
        let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
        let directory = shelf_dir(&git_dir).join(format!("{timestamp}-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join(PATCH_FILE), patch)?;
        let meta = ShelvedChangelist {
            name,
            timestamp,
            files: files.clone(),
            directory: directory.clone(),
        };
        std::fs::write(directory.join(META_FILE), serde_json::to_string_pretty(&meta)?)?;

        let mut restore = vec![
            "restore".into(),
            "--source=HEAD".into(),
            "--staged".into(),
            "--worktree".into(),
            "--".into(),
        ];
        restore.extend(files);
        repository
            .update(cx, |repository, cx| repository.run_git_command(restore, cx))
            .await??;
        Ok(())
    })
}

/// Applies a shelved changelist and removes it from the shelf.
pub(crate) fn unshelve(
    repository: &Entity<Repository>,
    shelved: &ShelvedChangelist,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let patch = shelved.patch_path().to_string_lossy().into_owned();
    let directory = shelved.directory.clone();
    let apply = repository.update(cx, |repository, cx| {
        repository.run_git_command(vec!["apply".into(), "--3way".into(), patch], cx)
    });
    cx.background_spawn(async move {
        apply.await??;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    })
}

pub(crate) fn delete(shelved: &ShelvedChangelist) -> anyhow::Result<()> {
    Ok(std::fs::remove_dir_all(&shelved.directory)?)
}

/// JetBrains "Shelve Changes" dialog.
pub(crate) struct ShelveModal {
    editor: Entity<Editor>,
    file_count: usize,
    on_confirm: Box<dyn Fn(String, &mut Window, &mut App)>,
}

impl ShelveModal {
    pub(crate) fn new(
        default_name: &str,
        file_count: usize,
        on_confirm: impl Fn(String, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(default_name.to_string(), window, cx);
            editor
        });
        Self {
            editor,
            file_count,
            on_confirm: Box::new(on_confirm),
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.editor.read(cx).text(cx).trim().to_string();
        if name.is_empty() {
            return;
        }
        (self.on_confirm)(name, window, cx);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for ShelveModal {}
impl ModalView for ShelveModal {}
impl Focusable for ShelveModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ShelveModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ShelveModal")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_2(cx)
            .w(rems(30.))
            .p_3()
            .gap_2()
            .child(Headline::new("Shelve Changes").size(HeadlineSize::XSmall))
            .child(
                Label::new(match self.file_count {
                    1 => "1 file".to_string(),
                    count => format!("{count} files"),
                })
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(Label::new("Comment:").size(LabelSize::Small))
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
                    .child(Button::new("shelve-cancel", "Cancel").on_click(
                        cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                    ))
                    .child(
                        Button::new("shelve-ok", "Shelve Changes")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(Confirm), cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_sorts_newest_first() {
        let git_dir = tempfile::tempdir().unwrap();
        for (id, timestamp) in [("a", 1), ("b", 3), ("c", 2)] {
            let directory = shelf_dir(git_dir.path()).join(id);
            std::fs::create_dir_all(&directory).unwrap();
            let meta = ShelvedChangelist {
                name: id.into(),
                timestamp,
                files: vec!["f".into()],
                directory: PathBuf::new(),
            };
            std::fs::write(directory.join(META_FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        }
        let names: Vec<_> = load(git_dir.path()).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["b", "c", "a"]);
    }
}
