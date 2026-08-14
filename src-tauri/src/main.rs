// Prevents an additional console window from appearing on Windows in
// release builds. Standard Tauri v2 scaffold boilerplate; inert on
// macOS/Linux, the two targets this rewrite ships (see the plan's "Linux +
// macOS" scope).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tome_lib::run();
}
