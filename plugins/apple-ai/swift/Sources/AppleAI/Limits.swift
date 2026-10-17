import Foundation

#if canImport(FoundationModels)
import FoundationModels
#endif

// What the model says about its own size: the window and the token count of a text. Both are read at run time, so the
// budget of a request follows the model version of the Mac the app runs on.
//
// Neither function prints or logs anything. A failure is the answer -1, never a message.

/// The context window of the system model in tokens, or -1 when it cannot be read: the framework is not in the SDK, the
/// system is older than macOS 26, or the model is not available.
@_cdecl("hw_afm_context_size")
public func hw_afm_context_size() -> Int32 {
    #if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        let model = SystemLanguageModel.default
        guard case .available = model.availability else { return -1 }
        let size = model.contextSize
        return size > 0 ? Int32(clamping: size) : -1
    }
    #endif
    return -1
}

/// The callback of a token count and its opaque pointer, carried into the task. The pointer is only ever handed back to
/// the callback, never read, and a C function pointer has no state.
private struct CountDone: @unchecked Sendable {
    let callback: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
    let pointer: UnsafeMutableRawPointer?
}

/// Counts the tokens of a text and calls `done` exactly once, on a thread of the concurrency pool and never before this
/// function has returned: with the count, or with -1 when counting is not available (the system is older than macOS
/// 26.4, the model is not available), the call fails or the text is over 65 536 bytes.
@_cdecl("hw_afm_token_count")
public func hw_afm_token_count(
    _ text: UnsafePointer<UInt8>?,
    _ length: Int,
    _ done: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void,
    _ userData: UnsafeMutableRawPointer?
) {
    let delivery = CountDone(callback: done, pointer: userData)
    var copied: String? = nil
    if length == 0 {
        copied = ""
    } else if let text, length > 0, length <= 65_536 {
        copied = String(bytes: UnsafeBufferPointer(start: text, count: length), encoding: .utf8)
    }
    let source = copied
    Task.detached {
        await Task.yield()
        let count = await countTokens(source)
        delivery.callback(delivery.pointer, count)
    }
}

private func countTokens(_ text: String?) async -> Int32 {
    guard let text else { return -1 }
    #if canImport(FoundationModels)
    if #available(macOS 26.4, *) {
        let model = SystemLanguageModel.default
        guard case .available = model.availability else { return -1 }
        do {
            let count = try await model.tokenCount(for: Instructions(text))
            return count >= 0 ? Int32(clamping: count) : -1
        } catch {
            return -1
        }
    }
    #endif
    return -1
}
