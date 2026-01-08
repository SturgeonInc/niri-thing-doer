mod kdl_utils;
mod window_rule;

use std::collections::HashSet;
use std::io::empty;
use std::{default, fs};

use clap::Parser;
use miette::{Context, IntoDiagnostic};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Event, Request, Response, Window};

use window_rule::{Match, WindowRule, WindowRules};

type WindowId = u64;

#[derive(Parser)]
#[command(about = "Limited generic niri event handler?", long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "FILE", default_value = "rules.kdl")]
    rules: String,
}

fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    let windowrules = parse_config(&cli.rules)?.windowrules;

    let mut listening_socket = Socket::connect().into_diagnostic()?;
    let mut sending_socket = Socket::connect().into_diagnostic()?;
    // TODO: HashSet should probably be on both WindowId and RuleId!!!
    let mut matched_windows: HashSet<WindowId> = HashSet::new();

    listening_socket
        .send(Request::EventStream)
        .into_diagnostic()?
        .and_then(|r| match r {
            Response::Handled => Ok(()),
            code => Err(
                format!("Expected niri to provide either a 'Handled' signal or an error in response to an EventStream request, instead got {code:?}")
            ),
        })
        .unwrap();

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
            Event::WindowClosed { id } => drop(matched_windows.remove(&id)),
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
    windowrules: &Vec<WindowRule>,
    matched_windows: &mut HashSet<WindowId>,
    socket: &mut Socket,
) -> miette::Result<()> {
    let windowrules = rules_that_apply(&window, windowrules);
    if windowrules.is_empty() {
        return Ok(());
    }

    matched_windows.insert(window.id);

    for wr in windowrules {
        take_windowrule_actions(&window, &wr, socket)?;
    }

    return Ok(());
}

fn take_windowrule_actions(
    window: &Window,
    windowrule: &WindowRule,
    socket: &mut Socket,
) -> miette::Result<()> {
    // presumably these should also return a Handled like an EventStream
    // request but the documentation doesn't specify so I don't either
    match windowrule.open_floating {
        None => (),
        Some(true) => {
            let _ = socket
                .send(Request::Action(Action::MoveWindowToFloating {
                    id: Some(window.id),
                }))
                .into_diagnostic()?;
        }
        Some(false) => {
            let _ = socket
                .send(Request::Action(Action::MoveWindowToTiling {
                    id: Some(window.id),
                }))
                .into_diagnostic()?;
        }
    }

    if let Some(command) = &windowrule.spawn_sh {
        let id = window.id.to_string();
        let title = window.title.as_deref().unwrap_or_default();
        let app_id = window.app_id.as_deref().unwrap_or_default();
        let pid = match window.pid {
            None => "''".to_string(),
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

    return Ok(());
}

fn rules_that_apply<'a>(window: &Window, windowrules: &'a Vec<WindowRule>) -> Vec<&'a WindowRule> {
    windowrules
        .iter()
        .filter(|wr| rule_applies(window, wr))
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
    let title = window.title.as_deref().unwrap_or("");
    let matches_title = match &m.title {
        Some(x) => x.0.is_match(title),
        None => true,
    };

    let app_id = window.app_id.as_deref().unwrap_or("");
    let matches_app_id = match &m.app_id {
        Some(x) => x.0.is_match(app_id),
        None => true,
    };

    // This is more complicated, I'd need to check the workspace and shit
    // Missing: active, active in column, is screencast target, on startup
    let matches_focused = match m.is_focused {
        Some(x) => x == window.is_focused,
        None => true,
    };
    let matches_urgent = match m.is_urgent {
        Some(x) => x == window.is_urgent,
        None => true,
    };
    let matches_floating = match m.is_floating {
        Some(x) => x == window.is_floating,
        None => true,
    };
    matches_app_id && matches_title && matches_focused && matches_urgent && matches_floating
}
