//! The StatusNotifierItem that renders a [`Snapshot`].

use std::process;
use std::sync::{Arc, Mutex};

use futures_channel::mpsc::UnboundedSender;
use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{ToolTip, Tray};

use crate::api::{Command, Group, Mode, Rule, Snapshot};

/// Kernel state, shared between the tray (reader) and the polling thread.
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<Snapshot>>);

impl Shared {
    pub fn new(snapshot: Snapshot) -> Self {
        Self(Arc::new(Mutex::new(snapshot)))
    }

    /// Edit the state; the polling thread asks the tray to redraw afterwards.
    pub fn update(&self, edit: impl FnOnce(&mut Snapshot)) {
        edit(&mut self.0.lock().expect("shared state is locked briefly"));
    }

    fn snapshot(&self) -> Snapshot {
        self.0
            .lock()
            .expect("shared state is locked briefly")
            .clone()
    }
}

pub struct ClashTray {
    state: Shared,
    /// Hands menu commands over to the task that owns the client.
    commands: UnboundedSender<Command>,
}

impl ClashTray {
    pub fn new(state: Shared, commands: UnboundedSender<Command>) -> Self {
        Self { state, commands }
    }

    /// A menu item that sends `command` to the polling thread.
    fn action(&self, label: &str, command: Command) -> MenuItem<Self> {
        let commands = self.commands.clone();
        StandardItem {
            label: label.into(),
            activate: Box::new(move |_| drop(commands.unbounded_send(command.clone()))),
            ..Default::default()
        }
        .into()
    }

    fn mode_menu(&self, state: &Snapshot) -> MenuItem<Self> {
        let commands = self.commands.clone();
        let selected = Mode::ALL
            .iter()
            .position(|mode| *mode == state.mode)
            .unwrap_or_default();
        SubMenu {
            label: "Mode".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |_, index| {
                        drop(commands.unbounded_send(Command::SetMode(Mode::ALL[index])));
                    }),
                    options: Mode::ALL
                        .iter()
                        .map(|mode| RadioItem {
                            label: mode.label().into(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }

    fn proxies_menu(&self, state: &Snapshot) -> MenuItem<Self> {
        SubMenu {
            label: "Proxies".into(),
            enabled: !state.groups.is_empty(),
            submenu: state
                .groups
                .iter()
                .map(|group| self.group_menu(group))
                .collect(),
            ..Default::default()
        }
        .into()
    }

    fn group_menu(&self, group: &Group) -> MenuItem<Self> {
        let commands = self.commands.clone();
        let (name, nodes) = (group.name.clone(), group.nodes.clone());
        let submenu = vec![
            RadioGroup {
                selected: group.selection(),
                select: Box::new(move |_, index| {
                    if let Some(node) = nodes.get(index) {
                        drop(commands.unbounded_send(Command::Select {
                            group: name.clone(),
                            node: node.clone(),
                        }));
                    }
                }),
                options: group
                    .nodes
                    .iter()
                    .map(|node| RadioItem {
                        label: node.clone(),
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
        ];

        SubMenu {
            label: match &group.now {
                Some(now) => format!("{} — {now}", group.name),
                None => group.name.clone(),
            },
            submenu,
            ..Default::default()
        }
        .into()
    }

    fn rules_menu(&self, state: &Snapshot) -> MenuItem<Self> {
        SubMenu {
            label: "Rules".into(),
            enabled: !state.rules.is_empty(),
            submenu: state
                .rules
                .iter()
                .map(|rule| self.rule_item(rule))
                .collect(),
            ..Default::default()
        }
        .into()
    }

    /// One rule, with a checkmark that switches it off and on again.
    fn rule_item(&self, rule: &Rule) -> MenuItem<Self> {
        let commands = self.commands.clone();
        let (index, disabled) = (rule.index, rule.extra.disabled);
        CheckmarkItem {
            label: rule.to_string(),
            // A checkmark means the rule is active.
            checked: !disabled,
            activate: Box::new(move |_| {
                drop(commands.unbounded_send(Command::SetRuleDisabled {
                    index,
                    disabled: !disabled,
                }));
            }),
            ..Default::default()
        }
        .into()
    }

    /// The things that act on the kernel itself.
    fn options_menu(&self) -> MenuItem<Self> {
        SubMenu {
            label: "Kernal Options".into(),
            submenu: vec![
                self.action("Reload configuration", Command::ReloadConfig),
                self.action("Update GEO database", Command::UpdateGeo),
                self.action("Flush fake-IP cache", Command::FlushFakeIp),
                self.action("Flush DNS cache", Command::FlushDns),
                self.action("Restart kernel", Command::Restart),
            ],
            ..Default::default()
        }
        .into()
    }
}

impl Tray for ClashTray {
    // The menu is all this tray has to offer, so open it on a left click.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn icon_name(&self) -> String {
        "network-vpn".into()
    }

    fn title(&self) -> String {
        "clash tray".into()
    }

    /// The menu is about to be shown, so ask for fresh data. This is what lets
    /// the poll interval stay long; the host applies the update right away.
    fn menu_about_to_show(&mut self) {
        drop(self.commands.unbounded_send(Command::Refresh));
    }

    fn tool_tip(&self) -> ToolTip {
        let state = self.state.snapshot();
        ToolTip {
            icon_name: self.icon_name(),
            title: status(&state),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let state = self.state.snapshot();
        vec![
            StandardItem {
                label: status(&state),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            self.mode_menu(&state),
            self.proxies_menu(&state),
            self.rules_menu(&state),
            MenuItem::Separator,
            self.options_menu(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// `mihomo v1.19.10 · Rule`, or why the kernel cannot be reached.
fn status(state: &Snapshot) -> String {
    if let Some(error) = &state.error {
        return format!("⚠ {error}");
    }
    let kernel = if state.version.is_empty() {
        "mihomo".to_owned()
    } else {
        format!("mihomo {}", state.version)
    };
    format!("{kernel} · {}", state.mode.label())
}
