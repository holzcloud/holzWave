#if canImport(FoundationModels)
import FoundationModels
#endif

// Availability of the system language model, as one integer for the C ABI.
//
//   0  available
//   1  Apple Intelligence is not enabled
//   2  the device is not eligible
//   3  the model is not ready (still downloading, or the system is busy preparing it)
//   4  another reason, including reasons that a later SDK adds
//   5  macOS is older than 26, the framework does not exist on this system
//   6  the SDK this library was built with has no FoundationModels framework
//
// Only 0 means available. The Rust side maps every other number, also an unknown one, to "not available".
@_cdecl("hw_afm_availability")
public func hw_afm_availability() -> Int32 {
    #if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        switch SystemLanguageModel.default.availability {
        case .available:
            return 0
        case .unavailable(let reason):
            switch reason {
            case .appleIntelligenceNotEnabled:
                return 1
            case .deviceNotEligible:
                return 2
            case .modelNotReady:
                return 3
            @unknown default:
                return 4
            }
        }
    } else {
        return 5
    }
    #else
    return 6
    #endif
}
