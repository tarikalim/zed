use crate::git_graph::GitGraph;
use git::repository::LogSource;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    Render, Task, WeakEntity, Window, actions, px,
};
use project::git_store::RepositoryId;
use ui::prelude::*;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

actions!(
    git_tool_window,
    [
        /// Toggles focus on the JetBrains-style Git tool window (Log).
        ToggleFocus,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<GitToolWindow>(window, cx);
        });
    })
    .detach();
}

/// The bottom-docked "Git" tool window of JetBrains IDEs, hosting the Log.
pub struct GitToolWindow {
    workspace: WeakEntity<Workspace>,
    log: Option<(RepositoryId, Entity<GitGraph>)>,
    position: DockPosition,
    focus_handle: FocusHandle,
}

impl GitToolWindow {
    pub fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Task<anyhow::Result<Entity<Self>>> {
        cx.spawn(async move |cx| {
            workspace.update_in(cx, |workspace_ref, _, cx| {
                let git_store = workspace_ref.project().read(cx).git_store().clone();
                cx.new(|cx| {
                    // Re-render when the active repository changes so the Log follows it.
                    cx.subscribe(&git_store, |_, _, event, cx| {
                        if matches!(event, project::git_store::GitStoreEvent::ActiveRepositoryChanged(_)) {
                            cx.notify();
                        }
                    })
                    .detach();
                    Self {
                        workspace: workspace.clone(),
                        log: None,
                        position: DockPosition::Bottom,
                        focus_handle: cx.focus_handle(),
                    }
                })
            })
        })
    }

    /// Creates the Log for the active repository, or replaces it when the active repository changes.
    fn ensure_log(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<GitGraph>> {
        let workspace = self.workspace.upgrade()?;
        let project = workspace.read(cx).project().clone();
        let repository = project.read(cx).active_repository(cx)?;
        let repository_id = repository.read(cx).id;
        if let Some((id, log)) = &self.log
            && *id == repository_id
        {
            return Some(log.clone());
        }
        let git_store = project.read(cx).git_store().clone();
        let workspace = self.workspace.clone();
        let log = cx.new(|cx| {
            GitGraph::new(repository_id, git_store, workspace, Some(LogSource::All), window, cx)
        });
        self.log = Some((repository_id, log.clone()));
        Some(log)
    }
}

impl EventEmitter<PanelEvent> for GitToolWindow {}

impl Focusable for GitToolWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GitToolWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let log = self.ensure_log(window, cx);
        let colors = cx.theme().colors();
        v_flex()
            .key_context("GitToolWindow")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(colors.panel_background)
            .child(
                h_flex()
                    .px_2()
                    .h(px(28.))
                    .gap_3()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(Label::new("Git").size(LabelSize::Small).color(Color::Muted))
                    .child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .border_b_2()
                            .border_color(colors.text_accent)
                            .child(Label::new("Log").size(LabelSize::Small)),
                    ),
            )
            .child(div().flex_1().min_h_0().children(log).when(self.log.is_none(), |this| {
                this.flex()
                    .items_center()
                    .justify_center()
                    .child(Label::new("No Git repository").color(Color::Muted))
            }))
    }
}

impl Panel for GitToolWindow {
    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.log
            .as_ref()
            .map(|(_, log)| log.focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }

    fn persistent_name() -> &'static str {
        "GitToolWindow"
    }

    fn panel_key() -> &'static str {
        "GitToolWindow"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, _: DockPosition) -> bool {
        true
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::GitGraph)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Git")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleFocus.boxed_clone()
    }

    fn activation_priority(&self) -> u32 {
        11
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use workspace::MultiWorkspace;

    #[gpui::test]
    async fn test_git_tool_window_shows_log(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/repo"), json!({ ".git": {}, "a.txt": "a" })).await;
        let project = Project::test(fs, [path!("/repo").as_ref()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        let panel = workspace
            .update_in(cx, |_, window, cx| GitToolWindow::load(workspace.downgrade(), window.to_async(cx)))
            .await
            .unwrap();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_panel(panel.clone(), window, cx);
            workspace.toggle_panel_focus::<GitToolWindow>(window, cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert!(panel.log.is_some(), "the Log is created on render"));
    }
}
