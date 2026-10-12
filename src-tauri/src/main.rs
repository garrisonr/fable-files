//! Binary entry point — delegates to the library so mobile targets work.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    ultimate_file_manager_lib::run()
}
