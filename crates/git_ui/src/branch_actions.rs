//! The JetBrains branch actions menu, shared by the Log's Branches pane and the branch popup.

use std::rc::Rc;

use git::Oid;
use gpui::{App, Entity, PromptLevel, SharedString, WeakEntity, Window};
use project::git_store::Repository;
use ui::{ContextMenu, ContextMenuEntry, prelude::*};
use workspace::Workspace;

use crate::{branch_diff::BranchDiff, log_actions};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RefKind {
    Head,
    Local,
    Remote,
    Tag,
}

/// Opens a Log filtered to `current...branch`; `None` hides "Compare with".
pub(crate) type CompareHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;

/// `name` is the ref as git accepts it (`main`, `origin/main`, `v1.0`).
pub(crate) fn branch_actions_menu(
    kind: RefKind,
    name: SharedString,
    sha: Option<Oid>,
    workspace: WeakEntity<Workspace>,
    repository: Entity<Repository>,
    on_compare: Option<CompareHandler>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    let current: Option<SharedString> = repository
        .read(cx)
        .branch
        .as_ref()
        .map(|branch| branch.name().to_string().into());
    let upstream: Option<(String, String)> = (kind == RefKind::Local)
        .then(|| {
            let repository = repository.read(cx);
            let branch = repository
                .branch_list
                .iter()
                .find(|branch| !branch.is_remote() && branch.name() == name.as_ref())?;
            let upstream = branch.upstream.as_ref()?;
            Some((
                upstream.remote_name()?.to_string(),
                upstream.branch_name()?.to_string(),
            ))
        })
        .flatten();
    let current_label = current.clone().unwrap_or_else(|| "HEAD".into());
    let is_current =
        kind == RefKind::Head || current.as_ref().is_some_and(|current| *current == name);
    let repository = repository.downgrade();

    let git = {
        let repository = repository.clone();
        move |args: Vec<String>, error: &'static str| {
            let repository = repository.clone();
            move |window: &mut Window, cx: &mut App| {
                let Some(repository) = repository.upgrade() else {
                    return;
                };
                let receiver = repository
                    .update(cx, |repository, cx| repository.run_git_command(args.clone(), cx));
                log_actions::spawn_git_job(receiver, error, window, cx);
            }
        }
    };
    let new_branch = {
        let workspace = workspace.clone();
        let repository = repository.clone();
        move |window: &mut Window, cx: &mut App| {
            if let Some(sha) = sha {
                log_actions::open_new_branch_modal(&workspace, repository.clone(), sha, window, cx);
            }
        }
    };

    ContextMenu::build(window, cx, move |menu, _, _| {
        let menu = menu.header(name.clone());
        if kind == RefKind::Head {
            return menu.entry_when(sha.is_some(), "New Branch from 'HEAD'…", new_branch);
        }
        let is_local = kind == RefKind::Local;
        let is_remote = kind == RefKind::Remote;
        let is_tag = kind == RefKind::Tag;
        let checkout_args = if is_remote {
            vec!["switch".into(), "--track".into(), name.to_string()]
        } else if is_tag {
            vec!["checkout".into(), "--detach".into(), name.to_string()]
        } else {
            vec!["switch".into(), name.to_string()]
        };
        menu.when(!is_current, |menu| {
            menu.entry("Checkout", None, git(checkout_args, "Checkout failed"))
        })
        .entry_when(sha.is_some(), format!("New Branch from '{name}'…"), new_branch)
        .when(!is_current && is_local, |menu| {
            menu.entry(
                format!("Checkout and Rebase onto '{current_label}'"),
                None,
                git(
                    vec!["rebase".into(), current_label.to_string(), name.to_string()],
                    "Rebase failed",
                ),
            )
        })
        .separator()
        .when_some(on_compare.clone().filter(|_| !is_current), |menu, on_compare| {
            let range: SharedString = format!("{current_label}...{name}").into();
            menu.entry(format!("Compare with '{current_label}'"), None, move |window, cx| {
                on_compare(range.clone(), window, cx)
            })
        })
        .entry("Show Diff with Working Tree", None, {
            let workspace = workspace.clone();
            let repository = repository.clone();
            let name = name.clone();
            move |window, cx| {
                let Some(repository) = repository.upgrade() else {
                    return;
                };
                workspace
                    .update(cx, |workspace, cx| {
                        let project = workspace.project().clone();
                        BranchDiff::deploy_branch_diff_with_base_ref(
                            workspace,
                            project,
                            repository,
                            name.clone(),
                            None,
                            window,
                            cx,
                        );
                    })
                    .ok();
            }
        })
        .when(!is_current, |menu| {
            menu.separator()
                .entry(
                    format!("Rebase '{current_label}' onto '{name}'"),
                    None,
                    git(vec!["rebase".into(), name.to_string()], "Rebase failed"),
                )
                .entry(
                    format!("Merge '{name}' into '{current_label}'"),
                    None,
                    git(
                        vec!["merge".into(), "--no-edit".into(), name.to_string()],
                        "Merge failed",
                    ),
                )
        })
        .when(is_remote, |menu| {
            let (remote, branch) = name.split_once('/').unwrap_or(("origin", name.as_ref()));
            menu.entry(
                format!("Pull into '{current_label}' Using Merge"),
                None,
                git(
                    vec![
                        "pull".into(),
                        "--no-edit".into(),
                        remote.into(),
                        branch.into(),
                    ],
                    "Pull failed",
                ),
            )
            .entry(
                format!("Pull into '{current_label}' Using Rebase"),
                None,
                git(
                    vec!["pull".into(), "--rebase".into(), remote.into(), branch.into()],
                    "Pull failed",
                ),
            )
            .separator()
            .entry(
                "Fetch",
                None,
                git(vec!["fetch".into(), remote.into(), branch.into()], "Fetch failed"),
            )
        })
        .when(is_local, |menu| {
            // Current branch: pull its upstream. Other branches: fast-forward from the upstream.
            let update_args = match (&upstream, is_current) {
                (_, true) => Some(vec!["pull".into(), "--no-edit".into()]),
                (Some((remote, branch)), false) => Some(vec![
                    "fetch".into(),
                    remote.clone(),
                    format!("{branch}:{name}"),
                ]),
                (None, false) => None,
            };
            // ponytail: no upstream pushes to origin; a push dialog if more remotes matter.
            let push_args = match &upstream {
                Some((remote, branch)) => {
                    vec!["push".into(), remote.clone(), format!("{name}:{branch}")]
                }
                None => vec!["push".into(), "-u".into(), "origin".into(), name.to_string()],
            };
            menu.separator()
                .item(
                    ContextMenuEntry::new("Update")
                        .disabled(update_args.is_none())
                        .handler(git(update_args.unwrap_or_default(), "Update failed")),
                )
                .entry("Push", None, git(push_args, "Push failed"))
        })
        .when(!is_remote && !is_tag, |menu| {
            menu.separator().entry("Rename…", None, {
                let workspace = workspace.clone();
                let repository = repository.clone();
                let name = name.clone();
                move |window, cx| {
                    let Some(repository) = repository.upgrade() else {
                        return;
                    };
                    let name = name.to_string();
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.toggle_modal(window, cx, |window, cx| {
                                crate::RenameBranchModal::new(name, repository, window, cx)
                            })
                        })
                        .ok();
                }
            })
        })
        .when(!is_current, |menu| {
            let delete_args = if is_tag {
                vec!["tag".into(), "-d".into(), name.to_string()]
            } else if let Some((remote, branch)) =
                is_remote.then(|| name.split_once('/')).flatten()
            {
                vec![
                    "push".into(),
                    remote.to_string(),
                    "--delete".into(),
                    branch.to_string(),
                ]
            } else {
                vec!["branch".into(), "-d".into(), name.to_string()]
            };
            let run = git(delete_args, "Delete failed");
            let name = name.clone();
            menu.separator().entry("Delete", None, move |window, cx| {
                let detail = format!("Delete {}?", name);
                let answer =
                    window.prompt(PromptLevel::Warning, &detail, None, &["Delete", "Cancel"], cx);
                let run = run.clone();
                window
                    .spawn(cx, async move |cx| {
                        if answer.await == Ok(0) {
                            cx.update(|window, cx| run(window, cx)).ok();
                        }
                    })
                    .detach();
            })
        })
    })
}

trait EntryWhen {
    fn entry_when(
        self,
        enabled: bool,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self;
}

impl EntryWhen for ContextMenu {
    fn entry_when(
        self,
        enabled: bool,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.item(ContextMenuEntry::new(label).disabled(!enabled).handler(handler))
    }
}
