/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
// Allow the crate to have a non-snake-case name (touchHLE).
// This also allows items in the crate to have non-snake-case names.
#![allow(non_snake_case)]
// On the web, the entry point is a C-style `main` (see below).
#![cfg_attr(target_os = "emscripten", no_main)]

#[cfg(not(target_os = "emscripten"))]
fn main() -> Result<(), String> {
    touchHLE::main(std::env::args())
}

/// The entry point for the web build.
///
/// This is a plain C `main` rather than a Rust one so that Rust's runtime
/// start-up code is not on the stack while the emulator runs. That code
/// catches panics, and Asyncify (which is what lets the emulator pause so the
/// browser can do its work) can't pause through a frame that does that. For
/// the same reason the web build is compiled with `panic=abort`, see
/// `.cargo/config.toml`.
#[cfg(target_os = "emscripten")]
#[no_mangle]
pub extern "C" fn main(argc: std::ffi::c_int, argv: *const *const std::ffi::c_char) -> std::ffi::c_int {
    let args: Vec<String> = (0..argc as usize)
        .map(|i| {
            unsafe { std::ffi::CStr::from_ptr(*argv.add(i)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    match touchHLE::main(args.into_iter()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("touchHLE errored: {e}");
            1
        }
    }
}
