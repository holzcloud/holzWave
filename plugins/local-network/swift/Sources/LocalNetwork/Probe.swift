import Foundation
import Network

// The Local Network privacy probe, as two integers for the C ABI.
//
// macOS 15 and later let the person switch the Local Network permission per app. There is no API to ask for it and no
// general API to read it (Apple radar FB8711182, TN3179 "Understanding local network privacy"). The documented detection
// is a Bonjour operation: while the permission is denied, a browser waits with the DNS error "policy denied".
// Browsing is the only thing done here. No connection is opened, no listener is started, nothing is published, nothing
// is sent and nothing is printed.

/// The private Bonjour type that is browsed. The Rust constant `SERVICE_TYPE` and the key `NSBonjourServices` of the
/// app's Info.plist name the same string, and a test pins the three together.
private let serviceType = "_holzwave-probe._tcp"

/// `kDNSServiceErr_PolicyDenied` of dns_sd.h: the error a Bonjour operation reports while local network access is
/// denied (TN3179). An `NWBrowser` shows it as `.waiting(.dns(-65570))`.
private let policyDenied: Int32 = -65570

/// How long a browser has to stay ready, without a further state change, before the permission reads as allowed. The
/// system may report ready for a moment while its question is still open.
private let settleMilliseconds = 500

/// The shortest and longest window the Swift side accepts; the Rust side clamps first.
private let windowRange: ClosedRange<UInt32> = 100...10_000

// Results of `hw_ln_probe`:
//   0  allowed
//   1  denied
//   2  unknown (also: no network, the browse failed, nothing was seen in the window)

/// 1 when the operating system has the Local Network privacy setting (macOS 15 or later), else 0. Below macOS 15 the
/// system never asks, so showing a notice would say something untrue.
@_cdecl("hw_ln_applies")
public func hw_ln_applies() -> Int32 {
    let fifteen = OperatingSystemVersion(majorVersion: 15, minorVersion: 0, patchVersion: 0)
    return ProcessInfo.processInfo.isOperatingSystemAtLeast(fifteen) ? 1 : 0
}

/// One browse, bounded by the window in milliseconds. Blocks the caller for at most that long and always cancels its
/// browser before it returns.
@_cdecl("hw_ln_probe")
public func hw_ln_probe(_ windowMilliseconds: UInt32) -> Int32 {
    let window = min(max(windowMilliseconds, windowRange.lowerBound), windowRange.upperBound)
    return Probe().run(windowMilliseconds: window)
}

/// The state of one probe. The handlers of the browser run on `queue`, the caller waits on `done`, and the lock guards
/// everything that both sides touch. A handler call that arrives after the probe ended does nothing.
private final class Probe: @unchecked Sendable {
    private let lock = NSLock()
    private let done = DispatchSemaphore(value: 0)
    private let queue = DispatchQueue(label: "ch.holzcloud.holzwave.local-network")
    private var finished = false
    private var result: Int32 = 2
    /// The last state was the "policy denied" wait.
    private var deniedNow = false
    private var settle: DispatchWorkItem?

    func run(windowMilliseconds: UInt32) -> Int32 {
        let browser = NWBrowser(for: .bonjour(type: serviceType, domain: nil), using: NWParameters())
        browser.stateUpdateHandler = { [self] state in handle(state) }
        browser.start(queue: queue)

        _ = done.wait(timeout: .now() + .milliseconds(Int(windowMilliseconds)))

        lock.lock()
        if !finished {
            finished = true
            result = deniedNow ? 1 : 2
        }
        settle?.cancel()
        settle = nil
        let answer = result
        lock.unlock()

        browser.stateUpdateHandler = nil
        browser.cancel()
        return answer
    }

    private func handle(_ state: NWBrowser.State) {
        lock.lock()
        defer { lock.unlock() }
        if finished { return }
        // Any change of state restarts the settling.
        settle?.cancel()
        settle = nil
        switch state {
        case .waiting(let error):
            if case .dns(let code) = error, code == policyDenied {
                // The system retries after a grant, so the browser keeps waiting and may still become ready.
                deniedNow = true
            } else {
                deniedNow = false
            }
        case .ready:
            deniedNow = false
            let item = DispatchWorkItem { [self] in settled() }
            settle = item
            queue.asyncAfter(deadline: .now() + .milliseconds(settleMilliseconds), execute: item)
        case .failed, .cancelled:
            deniedNow = false
            end(with: 2)
        default:
            break
        }
    }

    /// The browser stayed ready for the settle time: the permission is allowed.
    private func settled() {
        lock.lock()
        defer { lock.unlock() }
        if finished { return }
        end(with: 0)
    }

    /// Ends the probe with `value`. The lock is held by the caller.
    private func end(with value: Int32) {
        finished = true
        result = value
        done.signal()
    }
}
