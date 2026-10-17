import Foundation

#if canImport(FoundationModels)
import FoundationModels
#endif

// One request to the on-device model, streamed back to Rust through a C callback.
//
// `hw_afm_generate` copies its two buffers, starts a task and returns a handle above zero, or zero when the arguments
// are refused (then the callback is never called). The task is the only emitter, so the terminal event is the last by
// construction, and its registry entry is removed before that event is sent. Events arrive on a thread of the Swift
// concurrency pool, never before `hw_afm_generate` has returned:
//
//   kind 1  a cumulative snapshot of the reply: UTF-8, the whole reply so far
//   kind 2  end, no bytes
//   kind 3  error: the ASCII case label of the framework's error (1 to 48 letters), or `cancelled` or `unavailable`
//
// Exactly one kind 2 or kind 3 event ends every accepted call and nothing follows it.
//
// Only the reply text and the case label cross the ABI. The description or the payload of an error is never read: it
// can carry the prompt. The session is created per request with the system model passed explicitly and without any
// extra capability, and it is dropped when the task ends.

/// The callback and its opaque pointer, carried into the task. The pointer is only ever handed back to the callback,
/// never read, and a C function pointer has no state.
private struct EventSink: @unchecked Sendable {
    let callback: @convention(c) (UnsafeMutableRawPointer?, Int32, UnsafePointer<UInt8>?, Int) -> Void
    let pointer: UnsafeMutableRawPointer?

    func emit(_ kind: Int32, _ text: String) {
        var copy = text
        copy.withUTF8 { buffer in
            callback(pointer, kind, buffer.baseAddress, buffer.count)
        }
    }
}

/// Maps a handle to its task. Starting and removing take the same lock, so a very fast task cannot remove itself
/// before it was inserted.
private final class Registry: @unchecked Sendable {
    private let lock = NSLock()
    private var next: Int64 = 1
    private var tasks: [Int64: Task<Void, Never>] = [:]

    func start(_ make: (Int64) -> Task<Void, Never>) -> Int64 {
        lock.lock()
        defer { lock.unlock() }
        let handle = next
        next += 1
        tasks[handle] = make(handle)
        return handle
    }

    func remove(_ handle: Int64) {
        lock.lock()
        tasks[handle] = nil
        lock.unlock()
    }

    func cancel(_ handle: Int64) {
        lock.lock()
        let task = tasks[handle]
        lock.unlock()
        task?.cancel()
    }
}

private let registry = Registry()

/// The longest text of either buffer, in bytes. Rust refuses a longer one first; this is the second check.
private let maxInputBytes = 65_536

/// Copies `length` bytes as text. Nil for a null pointer with a length, a length over the bound or invalid UTF-8.
private func copyText(_ pointer: UnsafePointer<UInt8>?, _ length: Int) -> String? {
    if length == 0 { return "" }
    guard let pointer, length > 0, length <= maxInputBytes else { return nil }
    return String(bytes: UnsafeBufferPointer(start: pointer, count: length), encoding: .utf8)
}

/// The case name of an error, as ASCII letters only, at most 48 of them. Everything else is `other`. The payload and
/// the description of the error are never read.
private func errorLabel(_ error: Error) -> String {
    if error is CancellationError { return "cancelled" }
    let mirror = Mirror(reflecting: error)
    var raw = ""
    if mirror.displayStyle == .enum {
        if let first = mirror.children.first {
            raw = first.label ?? ""
        } else {
            raw = String(describing: error)
        }
    }
    var letters = ""
    for scalar in raw.unicodeScalars {
        guard scalar.isASCII, (scalar.value >= 65 && scalar.value <= 90) || (scalar.value >= 97 && scalar.value <= 122) else {
            break
        }
        letters.unicodeScalars.append(scalar)
        if letters.count == 48 { break }
    }
    return letters.isEmpty ? "other" : letters
}

@_cdecl("hw_afm_generate")
public func hw_afm_generate(
    _ instructions: UnsafePointer<UInt8>?,
    _ instructionsLength: Int,
    _ prompt: UnsafePointer<UInt8>?,
    _ promptLength: Int,
    _ maxResponseTokens: Int32,
    _ onEvent: @convention(c) (UnsafeMutableRawPointer?, Int32, UnsafePointer<UInt8>?, Int) -> Void,
    _ userData: UnsafeMutableRawPointer?
) -> Int64 {
    guard let instructionsText = copyText(instructions, instructionsLength),
        let promptText = copyText(prompt, promptLength),
        !promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
        (1...8192).contains(Int(maxResponseTokens))
    else {
        return 0
    }
    let sink = EventSink(callback: onEvent, pointer: userData)
    let limit = Int(maxResponseTokens)
    return registry.start { handle in
        Task.detached {
            await run(handle: handle, instructions: instructionsText, prompt: promptText, limit: limit, sink: sink)
        }
    }
}

@_cdecl("hw_afm_cancel")
public func hw_afm_cancel(_ handle: Int64) {
    registry.cancel(handle)
}

/// Ends the call with an error: the entry is removed first, then the one terminal event is sent.
private func fail(_ handle: Int64, _ sink: EventSink, _ label: String) {
    registry.remove(handle)
    sink.emit(3, label)
}

private func run(handle: Int64, instructions: String, prompt: String, limit: Int, sink: EventSink) async {
    #if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        await runModel(handle: handle, instructions: instructions, prompt: prompt, limit: limit, sink: sink)
        return
    }
    #endif
    fail(handle, sink, "unavailable")
}

#if canImport(FoundationModels)
@available(macOS 26.0, *)
private func runModel(handle: Int64, instructions: String, prompt: String, limit: Int, sink: EventSink) async {
    if Task.isCancelled {
        fail(handle, sink, "cancelled")
        return
    }
    let model = SystemLanguageModel.default
    guard case .available = model.availability else {
        fail(handle, sink, "unavailable")
        return
    }
    let session =
        instructions.isEmpty
        ? LanguageModelSession(model: model)
        : LanguageModelSession(model: model, instructions: instructions)
    do {
        let stream = session.streamResponse(to: prompt, options: GenerationOptions(maximumResponseTokens: limit))
        for try await snapshot in stream {
            if Task.isCancelled { throw CancellationError() }
            sink.emit(1, snapshot.content)
        }
        if Task.isCancelled { throw CancellationError() }
        registry.remove(handle)
        sink.emit(2, "")
    } catch {
        fail(handle, sink, errorLabel(error))
    }
}
#endif
