use editor::Editor;
use git::repository::RepoPath;
use gpui::{App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Window};
use menu::{Cancel, Confirm};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::ModalView;

pub(crate) const DEFAULT_CHANGELIST: &str = "Changes";
const FILE_NAME: &str = "zed-changelists.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Changelist {
    pub(crate) name: String,
    /// Paths explicitly moved to this list; unassigned changes belong to the active list.
    pub(crate) paths: BTreeSet<String>,
}

/// JetBrains-style changelists of one repository, stored in its git directory.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Changelists {
    pub(crate) lists: Vec<Changelist>,
    pub(crate) active: usize,
    #[serde(skip)]
    location: Option<PathBuf>,
}

impl Default for Changelists {
    fn default() -> Self {
        Self {
            lists: vec![Changelist {
                name: DEFAULT_CHANGELIST.into(),
                paths: BTreeSet::new(),
            }],
            active: 0,
            location: None,
        }
    }
}

impl Changelists {
    pub(crate) fn load(git_dir: &Path) -> Self {
        let location = git_dir.join(FILE_NAME);
        let mut changelists = std::fs::read_to_string(&location)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).log_err())
            .filter(|changelists| !changelists.lists.is_empty())
            .unwrap_or_default();
        changelists.active = changelists.active.min(changelists.lists.len() - 1);
        changelists.location = Some(location);
        changelists
    }

    pub(crate) fn location(&self) -> Option<&Path> {
        self.location.as_deref()
    }

    fn save(&self) {
        if let Some(location) = &self.location
            && let Some(text) = serde_json::to_string_pretty(self).log_err()
        {
            std::fs::write(location, text).log_err();
        }
    }

    pub(crate) fn list_of(&self, path: &RepoPath) -> usize {
        let path = path.as_unix_str();
        self.lists
            .iter()
            .position(|list| list.paths.contains(path))
            .unwrap_or(self.active)
    }

    pub(crate) fn move_paths(&mut self, paths: &[RepoPath], target: usize) {
        if target >= self.lists.len() {
            return;
        }
        for path in paths {
            let path = path.as_unix_str().to_string();
            for list in &mut self.lists {
                list.paths.remove(&path);
            }
            if let Some(list) = self.lists.get_mut(target) {
                list.paths.insert(path);
            }
        }
        self.save();
    }

    pub(crate) fn add(&mut self, name: String, make_active: bool) -> usize {
        self.lists.push(Changelist {
            name,
            paths: BTreeSet::new(),
        });
        let index = self.lists.len() - 1;
        if make_active {
            self.active = index;
        }
        self.save();
        index
    }

    pub(crate) fn rename(&mut self, index: usize, name: String) {
        if let Some(list) = self.lists.get_mut(index) {
            list.name = name;
            self.save();
        }
    }

    pub(crate) fn set_active(&mut self, index: usize) {
        if index < self.lists.len() {
            self.active = index;
            self.save();
        }
    }

    /// Deletes a list; its files fall back to the active list. The last list cannot be deleted.
    pub(crate) fn delete(&mut self, index: usize) {
        if self.lists.len() <= 1 || index >= self.lists.len() {
            return;
        }
        self.lists.remove(index);
        if self.active == index {
            self.active = 0;
        } else if self.active > index {
            self.active -= 1;
        }
        self.save();
    }
}

/// Name prompt used by New Changelist and Rename Changelist.
pub(crate) struct ChangelistNameModal {
    title: SharedString,
    editor: Entity<Editor>,
    on_confirm: Box<dyn Fn(String, &mut Window, &mut App)>,
}

impl ChangelistNameModal {
    pub(crate) fn new(
        title: impl Into<SharedString>,
        initial: &str,
        on_confirm: impl Fn(String, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(initial.to_string(), window, cx);
            editor
        });
        Self {
            title: title.into(),
            editor,
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

impl EventEmitter<DismissEvent> for ChangelistNameModal {}
impl ModalView for ChangelistNameModal {}
impl Focusable for ChangelistNameModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ChangelistNameModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ChangelistNameModal")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_2(cx)
            .w(rems(28.))
            .p_3()
            .gap_2()
            .child(Headline::new(self.title.clone()).size(HeadlineSize::XSmall))
            .child(Label::new("Name:").size(LabelSize::Small))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_1()
                    .rounded_sm()
                    .border_color(cx.theme().colors().border)
                    .child(self.editor.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_changelists_membership_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = |p: &str| RepoPath::new(p).unwrap();
        let mut changelists = Changelists::load(dir.path());
        assert_eq!(changelists.list_of(&path("a.rs")), 0);

        let feature = changelists.add("Feature".into(), false);
        changelists.move_paths(&[path("a.rs")], feature);
        assert_eq!(changelists.list_of(&path("a.rs")), feature);
        assert_eq!(changelists.list_of(&path("b.rs")), 0, "unassigned files go to the active list");

        changelists.set_active(feature);
        assert_eq!(changelists.list_of(&path("b.rs")), feature);

        let reloaded = Changelists::load(dir.path());
        assert_eq!(reloaded.lists, changelists.lists);
        assert_eq!(reloaded.active, feature);

        changelists.delete(feature);
        assert_eq!(changelists.lists.len(), 1);
        assert_eq!(changelists.list_of(&path("a.rs")), 0);
        changelists.delete(0);
        assert_eq!(changelists.lists.len(), 1, "the last list is kept");
    }
}
