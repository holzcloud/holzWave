// A release build on Windows is a GUI program: without this attribute the executable is a console program and Windows
// opens a console window beside the app. Debug builds keep the console for `eprintln!`. Other platforms ignore it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    holzwave_app_lib::run()
}
