// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit real-interactive observation experiment. No mode lease is constructed.

#[cfg(all(debug_assertions, target_os = "macos"))]
#[path = "support/interactive_permission.rs"]
mod experiment;

fn main() {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Err(error) = experiment::run() {
        eprintln!("interactive permission experiment refused: {error}");
        std::process::exit(1);
    }
    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    println!("NOT RUN: interactive permission experiment requires macOS debug native custody");
}
