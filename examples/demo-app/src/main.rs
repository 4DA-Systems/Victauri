//! Demo Tauri application showcasing Victauri introspection and MCP tools.
//!
//! Features: multi-window, CRUD, form validation, navigation, state sync —
//! everything needed to demonstrate Victauri's full-stack testing.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{Emitter, Manager, WebviewWindow};
use victauri_plugin::inspectable;

// ── State ──────────────────────────────────────────────────────────────────

#[derive(Default)]
struct AppState {
    counter: Mutex<i32>,
    todos: Mutex<Vec<Todo>>,
    settings: Mutex<Settings>,
    contacts: Mutex<Vec<Contact>>,
    notifications: Mutex<Vec<Notification>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Todo {
    id: u32,
    title: String,
    completed: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    theme: String,
    notifications_enabled: bool,
    language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            notifications_enabled: true,
            language: "en".to_string(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Contact {
    id: u32,
    name: String,
    email: String,
    message: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Notification {
    id: u32,
    title: String,
    body: String,
    read: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct ValidationError {
    field: String,
    message: String,
}

// ── Counter commands ───────────────────────────────────────────────────────

#[tauri::command]
#[inspectable(
    description = "Greet a person by name",
    intent = "generate greeting",
    category = "general",
    example = "greet someone"
)]
fn greet(name: String) -> String {
    format!("Hello, {name}! This response came from Rust.")
}

#[tauri::command]
#[inspectable(
    description = "Get the current counter value",
    intent = "read counter state",
    category = "counter"
)]
fn get_counter(state: tauri::State<'_, AppState>) -> i32 {
    *state.counter.lock().unwrap()
}

#[tauri::command]
#[inspectable(
    description = "Increment the counter by 1 and return the new value",
    intent = "increase counter",
    category = "counter",
    example = "increment the counter"
)]
fn increment(state: tauri::State<'_, AppState>) -> i32 {
    let mut count = state.counter.lock().unwrap();
    *count += 1;
    *count
}

#[tauri::command]
#[inspectable(
    description = "Decrement the counter by 1 and return the new value",
    intent = "decrease counter",
    category = "counter",
    example = "decrement the counter"
)]
fn decrement(state: tauri::State<'_, AppState>) -> i32 {
    let mut count = state.counter.lock().unwrap();
    *count -= 1;
    *count
}

#[tauri::command]
#[inspectable(
    description = "Reset the counter to zero",
    intent = "reset counter state",
    category = "counter",
    example = "reset the counter"
)]
fn reset_counter(state: tauri::State<'_, AppState>) -> i32 {
    let mut count = state.counter.lock().unwrap();
    *count = 0;
    *count
}

// ── Todo commands ──────────────────────────────────────────────────────────

#[tauri::command]
#[inspectable(
    description = "Add a new todo item",
    intent = "create todo",
    category = "todos",
    example = "add a todo"
)]
fn add_todo(state: tauri::State<'_, AppState>, title: String) -> Todo {
    let mut todos = state.todos.lock().unwrap();
    let id = todos.len() as u32 + 1;
    let todo = Todo {
        id,
        title,
        completed: false,
    };
    todos.push(todo.clone());
    todo
}

#[tauri::command]
#[inspectable(
    description = "List all todo items",
    intent = "read todos",
    category = "todos",
    example = "show all todos"
)]
fn list_todos(state: tauri::State<'_, AppState>) -> Vec<Todo> {
    state.todos.lock().unwrap().clone()
}

#[tauri::command]
#[inspectable(
    description = "Toggle the completion status of a todo item",
    intent = "update todo status",
    category = "todos",
    example = "mark todo as done"
)]
fn toggle_todo(state: tauri::State<'_, AppState>, id: u32) -> Result<Todo, String> {
    let mut todos = state.todos.lock().unwrap();
    let todo = todos
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("todo {id} not found"))?;
    todo.completed = !todo.completed;
    Ok(todo.clone())
}

#[tauri::command]
#[inspectable(
    description = "Delete a todo item by ID",
    intent = "remove todo",
    category = "todos",
    example = "delete a todo"
)]
fn delete_todo(state: tauri::State<'_, AppState>, id: u32) -> Result<(), String> {
    let mut todos = state.todos.lock().unwrap();
    let pos = todos
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| format!("todo {id} not found"))?;
    todos.remove(pos);
    Ok(())
}

// ── Settings commands ──────────────────────────────────────────────────────

#[tauri::command]
#[inspectable(
    description = "Get current application settings",
    intent = "read settings",
    category = "settings",
    example = "show settings"
)]
fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
#[inspectable(
    description = "Update application settings",
    intent = "modify settings",
    category = "settings",
    example = "change the theme"
)]
fn update_settings(
    state: tauri::State<'_, AppState>,
    theme: Option<String>,
    notifications_enabled: Option<bool>,
    language: Option<String>,
) -> Settings {
    let mut settings = state.settings.lock().unwrap();
    if let Some(t) = theme {
        settings.theme = t;
    }
    if let Some(n) = notifications_enabled {
        settings.notifications_enabled = n;
    }
    if let Some(l) = language {
        settings.language = l;
    }
    settings.clone()
}

// ── Contact form commands (validation pattern) ─────────────────────────────

#[tauri::command]
#[inspectable(
    description = "Submit a contact form with server-side validation",
    intent = "submit contact form",
    category = "contacts",
    example = "submit a contact form"
)]
fn submit_contact(
    state: tauri::State<'_, AppState>,
    name: String,
    email: String,
    message: String,
) -> Result<Contact, Vec<ValidationError>> {
    let mut errors = Vec::new();

    if name.trim().is_empty() {
        errors.push(ValidationError {
            field: "name".to_string(),
            message: "Name is required".to_string(),
        });
    }
    if !email.contains('@') || !email.contains('.') {
        errors.push(ValidationError {
            field: "email".to_string(),
            message: "Valid email address is required".to_string(),
        });
    }
    if message.trim().len() < 10 {
        errors.push(ValidationError {
            field: "message".to_string(),
            message: "Message must be at least 10 characters".to_string(),
        });
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut contacts = state.contacts.lock().unwrap();
    let id = contacts.len() as u32 + 1;
    let contact = Contact {
        id,
        name: name.trim().to_string(),
        email: email.trim().to_string(),
        message: message.trim().to_string(),
    };
    contacts.push(contact.clone());
    Ok(contact)
}

#[tauri::command]
#[inspectable(
    description = "List all submitted contacts",
    intent = "read contacts",
    category = "contacts",
    example = "show contacts"
)]
fn list_contacts(state: tauri::State<'_, AppState>) -> Vec<Contact> {
    state.contacts.lock().unwrap().clone()
}

// ── Notification commands (multi-window pattern) ───────────────────────────

#[tauri::command]
#[inspectable(
    description = "Send a notification (appears in notification window)",
    intent = "create notification",
    category = "notifications",
    example = "send a notification"
)]
fn send_notification(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    title: String,
    body: String,
) -> Notification {
    let mut notifs = state.notifications.lock().unwrap();
    let id = notifs.len() as u32 + 1;
    let notif = Notification {
        id,
        title,
        body,
        read: false,
    };
    notifs.push(notif.clone());

    let _ = app.emit("notification-added", &notif);
    notif
}

#[tauri::command]
#[inspectable(
    description = "List all notifications",
    intent = "read notifications",
    category = "notifications"
)]
fn list_notifications(state: tauri::State<'_, AppState>) -> Vec<Notification> {
    state.notifications.lock().unwrap().clone()
}

#[tauri::command]
#[inspectable(
    description = "Mark a notification as read",
    intent = "update notification",
    category = "notifications"
)]
fn mark_notification_read(
    state: tauri::State<'_, AppState>,
    id: u32,
) -> Result<Notification, String> {
    let mut notifs = state.notifications.lock().unwrap();
    let notif = notifs
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| format!("notification {id} not found"))?;
    notif.read = true;
    Ok(notif.clone())
}

#[tauri::command]
#[inspectable(
    description = "Get count of unread notifications",
    intent = "count unread",
    category = "notifications"
)]
fn unread_count(state: tauri::State<'_, AppState>) -> u32 {
    state
        .notifications
        .lock()
        .unwrap()
        .iter()
        .filter(|n| !n.read)
        .count() as u32
}

// ── Window management ──────────────────────────────────────────────────────

#[tauri::command]
#[inspectable(
    description = "Show or create the notification panel window",
    intent = "open notification window",
    category = "windows",
    example = "show notifications"
)]
fn show_notification_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("notifications") {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    } else {
        WebviewWindow::builder(&app, "notifications", tauri::WebviewUrl::App("notification.html".into()))
            .title("Notifications")
            .inner_size(400.0, 500.0)
            .resizable(true)
            .build()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ── Debug ──────────────────────────────────────────────────────────────────

#[tauri::command]
#[inspectable(
    description = "Get a summary of all application state for verification",
    intent = "read full state",
    category = "debug",
    example = "show app state"
)]
fn get_app_state(state: tauri::State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "counter": *state.counter.lock().unwrap(),
        "todos": *state.todos.lock().unwrap(),
        "settings": *state.settings.lock().unwrap(),
        "contacts": *state.contacts.lock().unwrap(),
        "notifications": *state.notifications.lock().unwrap(),
    })
}

fn main() {
    tauri::Builder::default()
        .plugin(
            victauri_plugin::VictauriBuilder::new()
                .auth_disabled()
                .commands(&[
                    greet__schema(),
                    get_counter__schema(),
                    increment__schema(),
                    decrement__schema(),
                    reset_counter__schema(),
                    add_todo__schema(),
                    list_todos__schema(),
                    toggle_todo__schema(),
                    delete_todo__schema(),
                    get_settings__schema(),
                    update_settings__schema(),
                    submit_contact__schema(),
                    list_contacts__schema(),
                    send_notification__schema(),
                    list_notifications__schema(),
                    mark_notification_read__schema(),
                    unread_count__schema(),
                    show_notification_window__schema(),
                    get_app_state__schema(),
                ])
                .build()
                .expect("victauri config is valid"),
        )
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            greet,
            get_counter,
            increment,
            decrement,
            reset_counter,
            add_todo,
            list_todos,
            toggle_todo,
            delete_todo,
            get_settings,
            update_settings,
            submit_contact,
            list_contacts,
            send_notification,
            list_notifications,
            mark_notification_read,
            unread_count,
            show_notification_window,
            get_app_state,
        ])
        .setup(|app| {
            let window = app.get_webview_window("main").unwrap();
            window.set_title("Victauri Demo").unwrap();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
