//! The only file of the crate with foreign calls: the two functions of the Swift package.

unsafe extern "C" {
    fn hw_ln_applies() -> i32;
    fn hw_ln_probe(window_ms: u32) -> i32;
}

/// Whether the system has the Local Network privacy setting (macOS 15 or later).
pub(crate) fn applies() -> bool {
    // SAFETY: the function takes no arguments, passes no pointers and returns a plain integer; it is defined by
    // `@_cdecl("hw_ln_applies")` in the Swift package and linked from the static library of build.rs.
    unsafe { hw_ln_applies() == 1 }
}

/// One bounded browse; the code is 0 allowed, 1 denied, 2 unknown. Touches the local network.
pub(crate) fn probe(window_ms: u32) -> i32 {
    // SAFETY: the function takes one integer, passes no pointers and returns a plain integer; it is defined by
    // `@_cdecl("hw_ln_probe")` in the Swift package, clamps the window itself, blocks for at most that long and cancels
    // its browser before it returns.
    unsafe { hw_ln_probe(window_ms) }
}

/// The addresses of both foreign functions, without calling them: proves the symbols link (used by `tests/pins.rs`
/// through `symbols_link` of the crate root).
pub(crate) fn symbols_link() -> bool {
    let applies: unsafe extern "C" fn() -> i32 = hw_ln_applies;
    let probe: unsafe extern "C" fn(u32) -> i32 = hw_ln_probe;
    applies as usize != 0 && probe as usize != 0
}
