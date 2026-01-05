mod error;
mod kdl_utils;
mod window_rule;

use std::collections::HashSet;
use std::fs;

use clap::Parser;
use miette::{Context, IntoDiagnostic};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Event, Request, Response, Window};
use regex::Regex;

use window_rule::WindowRules;

type WindowId = u64;

#[derive(Parser)]
#[command(about = "Limited generic niri event handler?", long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "FILE", default_value = "rules.kdl")]
    rules: String,
}

fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    let config = parse_config(&cli.rules)?;

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
                    do_thing_with_window(window, &mut matched_windows, &mut sending_socket);
                }
            }
            Event::WindowOpenedOrChanged { window } => {
                do_thing_with_window(window, &mut matched_windows, &mut sending_socket);
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
        .wrap_err_with(|| format!("cannot read {:?}", path))?;
    Ok(knuffel::parse(path, &text)?)
}

fn do_thing_with_window(
    window: Window,
    matched_windows: &mut HashSet<WindowId>,
    socket: &mut Socket,
) {
    if rule_applies(&window) {
        matched_windows.insert(window.id);
        match socket.send(Request::Action(Action::MoveWindowToFloating {
            id: Some(window.id),
        })) {
            Err(error) => panic!("Problem with sending niri message: {error:?}"),
            _ => (),
        }
    }
}

fn rule_applies(window: &Window) -> bool {
    let title = window.title.clone().unwrap_or("".to_string());
    let app_id = window.app_id.clone().unwrap_or("".to_string());

    let title_match = Regex::new("^.*Bitwarden.*$").unwrap().is_match(&title);
    let appid_match = Regex::new("^librewolf$").unwrap().is_match(&app_id);

    title_match && appid_match
}
