//! The schema `#[inspectable]` emits is what an agent builds its call from: an argument wrongly
//! marked optional, or dropped, produces a "missing required key" it cannot explain.
#![allow(dead_code, unused_variables, clippy::needless_pass_by_value)]

use victauri_core::registry::CommandInfo;

pub struct Options;
pub struct OptionalThing;
mod api {
    pub struct Request;
    pub struct State;
    pub struct Window;
}
mod tauri {
    pub struct State<'a, T>(pub &'a T);
    pub struct Window;
    pub struct AppHandle;
}
use tauri::AppHandle;

#[victauri_macros::inspectable]
fn takes_lookalikes(opts: Options, thing: OptionalThing) {}

#[victauri_macros::inspectable]
fn takes_vec_of_option(items: Vec<std::option::Option<u8>>) {}

#[victauri_macros::inspectable]
fn takes_real_options(a: Option<u8>, b: std::option::Option<u8>) {}

#[victauri_macros::inspectable]
fn takes_own_types(request: api::Request, state: api::State, window: api::Window, n: u8) {}

#[victauri_macros::inspectable]
fn takes_framework_types(
    app: AppHandle,
    rooted: tauri::Window,
    state: tauri::State<'_, u8>,
    n: u8,
) {
}

mod commands {
    #[victauri_macros::inspectable]
    pub fn greet() {}
}

/// `register_commands!(app, commands::greet__schema())` must compile with commands in a module.
#[test]
fn the_schema_companion_is_as_visible_as_its_command() {
    assert_eq!(commands::greet__schema().name, "greet");
}

fn required(info: &CommandInfo) -> Vec<(&str, bool)> {
    info.args
        .iter()
        .map(|a| (a.name.as_str(), a.required))
        .collect()
}

#[test]
fn required_is_decided_by_the_outermost_type_not_its_spelling() {
    assert_eq!(
        required(&takes_lookalikes__schema()),
        [("opts", true), ("thing", true)]
    );
    assert_eq!(required(&takes_vec_of_option__schema()), [("items", true)]);
    assert_eq!(
        required(&takes_real_options__schema()),
        [("a", false), ("b", false)]
    );
}

#[test]
fn an_apps_own_request_or_state_is_a_real_argument() {
    assert_eq!(
        required(&takes_own_types__schema()),
        [
            ("request", true),
            ("state", true),
            ("window", true),
            ("n", true)
        ]
    );
}

#[test]
fn tauri_injected_arguments_stay_out_of_the_schema() {
    assert_eq!(required(&takes_framework_types__schema()), [("n", true)]);
}
