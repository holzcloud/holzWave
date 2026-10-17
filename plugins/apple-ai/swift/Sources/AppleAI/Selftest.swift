// A smoke test of the Swift concurrency runtime and of the callback path into Rust.
//
// `hw_afm_selftest` returns at once. A task of the Swift concurrency pool then calls `callback(userData, 42)` exactly
// once, never on the caller's thread before this function has returned. No FoundationModels symbol is used here, so it
// works on every macOS at or above the deployment floor: it shows that the Swift runtime links and runs where the
// Apple model does not exist, which the streaming of the provider depends on.

/// Carries the callback and its opaque pointer into the task. The pointer is only ever handed back to the callback,
/// never read, and a C function pointer has no state.
private struct Delivery: @unchecked Sendable {
    let callback: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
    let pointer: UnsafeMutableRawPointer?
}

@_cdecl("hw_afm_selftest")
public func hw_afm_selftest(
    _ callback: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void,
    _ userData: UnsafeMutableRawPointer?
) {
    let delivery = Delivery(callback: callback, pointer: userData)
    Task.detached {
        await Task.yield()
        delivery.callback(delivery.pointer, 42)
    }
}
