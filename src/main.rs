mod kdl_utils;
mod window_rule;

use std::collections::HashSet;
use std::fs;

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
                    do_thing_with_window(
                        window,
                        &windowrules,
                        &mut matched_windows,
                        &mut sending_socket,
                    )?;
                }
            }
            Event::WindowOpenedOrChanged { window } => {
                do_thing_with_window(
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

fn do_thing_with_window(
    window: Window,
    window_rules: &Vec<WindowRule>,
    matched_windows: &mut HashSet<WindowId>,
    socket: &mut Socket,
) -> miette::Result<()> {
    if rule_applies(&window, &window_rules) {
        matched_windows.insert(window.id);

        // presumably this should also return a Handled like an EventStream
        // request but the documentation doesn't specify so I don't either
        let _ = socket
            .send(Request::Action(Action::MoveWindowToFloating {
                id: Some(window.id),
            }))
            .into_diagnostic()?;
    }

    Ok(())
}

// TODO: should return an action to take!
fn rule_applies(window: &Window, windowrules: &Vec<WindowRule>) -> bool {
    // probably niri has code for this that I should poach


    let excludes: Vec<&Vec<Match>> = windowrules.iter().map(|wr| &wr.excludes).collect();
    let excluded = excludes
        .iter()
        .any(|wr_matches| wr_matches.iter().any(|m| window_matches(window, m)));
    if excluded {
        return false;
    }

    let includes: Vec<&Vec<Match>> = windowrules.iter().map(|wr| &wr.matches).collect();
    let included = includes
        .iter()
        .any(|wr_matches| wr_matches.iter().any(|m| window_matches(window, m)));

    return included;
}

fn window_matches (window: &Window, m: &Match) -> bool {
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
