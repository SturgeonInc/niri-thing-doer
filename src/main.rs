mod kdl_utils;
mod window_rule;

use std::collections::HashSet;
use std::convert::identity;
use std::{env, fs};

use clap::Parser;
use miette::{Context, IntoDiagnostic};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Event, Request, Response, Window};

use window_rule::{Match, WindowRule, WindowRules};

use crate::kdl_utils::DefaultPresetSize;

type WindowId = u64;

#[derive(Parser)]
#[command(about = "Limited generic niri event handler?", long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "FILE")]
    rules: Option<String>,
}

fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    let rules = match cli.rules {
        Some(rules) => rules,
        None => {
            let conf_home = env::var("XDG_DATA_HOME")
                .unwrap_or(env::var("HOME")
                .map_err(|_| miette::miette!(
                    // help = "try specifying rule file path with --rules",
                    "environment variable $HOME not found, no default rule file path could be used"))? + "/.config");
            conf_home + "/niri/dyn_rules.kdl"
        }
    };
    let windowrules = parse_config(&rules)?.windowrules;

    let mut listening_socket = Socket::connect().into_diagnostic()?;
    let mut sending_socket = Socket::connect().into_diagnostic()?;
    let mut matched_windows: Vec<HashSet<WindowId>> = Vec::with_capacity(windowrules.len());
    for _ in &windowrules {
        matched_windows.push(HashSet::new());
    }

    handle_send(Request::EventStream, &mut listening_socket)?;

    let mut read_event = listening_socket.read_events();
    while let Ok(event) = read_event() {
        match event {
            Event::WindowsChanged { windows } => {
                for window in windows {
                    handle_window(
                        window,
                        &windowrules,
                        &mut matched_windows,
                        &mut sending_socket,
                    )?;
                }
            }
            Event::WindowOpenedOrChanged { window } => {
                handle_window(
                    window,
                    &windowrules,
                    &mut matched_windows,
                    &mut sending_socket,
                )?;
            }
            Event::WindowClosed { id } => drop(matched_windows.iter_mut().map(|x| x.remove(&id))),
            _ => (),
        }
    }

    Ok(())
}

fn parse_config(path: &str) -> miette::Result<WindowRules> {
    let text = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("could not read rule file {:?}", path))?;
    Ok(knuffel::parse(path, &text)?)
}

fn handle_window(
    window: Window,
    windowrules: &[WindowRule],
    matched_windows: &mut [HashSet<WindowId>],
    socket: &mut Socket,
) -> miette::Result<()> {
    let rules_that_apply = rules_that_apply(&window, windowrules);

    for (rule_idx, wr) in rules_that_apply {
        if matched_windows[rule_idx].insert(window.id) {
            take_windowrule_actions(&window, wr, socket)?;
        }
    }

    Ok(())
}

fn rules_that_apply<'a>(
    window: &Window,
    windowrules: &'a [WindowRule],
) -> Vec<(usize, &'a WindowRule)> {
    windowrules
        .iter()
        .enumerate()
        .filter(|(_, wr)| rule_applies(window, wr))
        .collect()
}

fn rule_applies(window: &Window, wr: &WindowRule) -> bool {
    // probably niri has code for this that I should poach

    let excludes = &wr.excludes;
    let excluded = excludes.iter().any(|m| window_matches(window, m));
    if excluded {
        return false;
    }

    let includes = &wr.matches;
    includes.iter().any(|m| window_matches(window, m))
}

fn window_matches(window: &Window, m: &Match) -> bool {
    // This is more complicated, I'd need to check the workspace and shit
    // Missing: active, active in column, is screencast target, on startup
    let regex_rules = [(&m.app_id, &window.app_id), (&m.title, &window.title)]
        .iter()
        .filter_map(|(m, w)| {
            let m = &m.as_ref()?.0;
            let w = w.as_deref().unwrap_or_default();
            Some(m.is_match(w))
        })
        .all(identity);

    let state_rules = [m.is_urgent, m.is_floating, m.is_focused]
        .into_iter()
        .flatten() // a clippy suggestion down from .filter_map(identity)
        .all(identity);

    regex_rules && state_rules
}

fn handle_send(req: Request, socket: &mut Socket) -> miette::Result<()> {
    socket.send(req.clone()).into_diagnostic()?
    .and_then(|r| match r {
        Response::Handled => Ok(()),
        code => Err(
            format!("Expected niri to provide either a 'Handled' signal or an error in response to an {req:#?} request, instead got {code:#?}")
        ),
    })
    .map_err(|issue| miette::miette!(issue))?;
    Ok(())
}

fn take_windowrule_actions(
    window: &Window,
    windowrule: &WindowRule,
    socket: &mut Socket,
) -> miette::Result<()> {
    // presumably these should also return a Handled like an EventStream
    // request but the documentation doesn't specify so I don't either
    // NOTE: Should happen first before other rules apply, I think
    if let Some(open_floating) = windowrule.open_floating {
        handle_send(
            Request::Action(match open_floating {
                true => Action::MoveWindowToFloating {
                    id: Some(window.id),
                },
                false => Action::MoveWindowToTiling {
                    id: Some(window.id),
                },
            }),
            socket,
        )?;
    }

    if let Some(DefaultPresetSize { 0: Some(change) }) = windowrule.default_window_height {
        handle_send(
            Request::Action(Action::SetWindowHeight {
                id: Some(window.id),
                change: change.into(),
            }),
            socket,
        )?;
    }

    if let Some(DefaultPresetSize { 0: Some(change) }) = windowrule.default_column_width {
        handle_send(
            Request::Action(Action::SetWindowWidth {
                id: Some(window.id),
                change: change.into(),
            }),
            socket,
        )?;
    }

    // TODO: why is niri not finishing these actions before the command?
    //       the socket is meant to be blocking isn't it?

    // NOTE: Should occur last
    if let Some(command) = &windowrule.spawn_sh {
        let id = window.id.to_string();
        let title = window.title.as_deref().unwrap_or_default();
        let app_id = window.app_id.as_deref().unwrap_or_default();
        let pid = match window.pid {
            None => "".to_string(),
            Some(pid) => pid.to_string(),
        };
        let command = command
            .to_string()
            .replace("{id}", &id)
            .replace("{title}", title)
            .replace("{app_id}", app_id)
            .replace("{pid}", &pid);
        let _ = socket
            .send(Request::Action(Action::SpawnSh { command }))
            .into_diagnostic()?;
    }

    Ok(())
}
