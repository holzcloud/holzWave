//! The four capabilities the helper needs, and how it holds them.
//!
//! A process that is not root receives none of the capabilities a container adds; they only fill the container's
//! bounding set. The helper file therefore carries file capabilities `cap_setuid,cap_setgid,cap_chown,cap_kill=p` (the
//! permitted set only, so the program still starts and can say what is wrong when the container did not add them). At
//! start the helper checks its permitted set, keeps exactly those four there, empties the inheritable and ambient sets,
//! and lowers its effective set to nothing. For each operation that needs them it raises the effective set again with
//! [`Raised`] and lowers it when the guard goes out of scope, so no capability is effective at a moment it is not used.
//! The file capabilities are ignored when `no_new_privs` is set on the process that starts the helper, which is why the
//! supervisor and the container must not set it.

/// The names of the four capabilities, in the order the messages list them.
pub const NAMES: [&str; 4] = ["CAP_SETUID", "CAP_SETGID", "CAP_CHOWN", "CAP_KILL"];

/// The sentence printed when capabilities are missing: it names them and says where each side sets them.
pub fn missing_message(missing: &[&str]) -> String {
    format!(
        "the capabilities {} are not permitted to the helper: the container must add them (SETUID, SETGID, CHOWN, KILL) \
         and the helper file must carry the file capabilities cap_setuid,cap_setgid,cap_chown,cap_kill=p; it never runs with \
         a reduced set",
        missing.join(", ")
    )
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::{NAMES, missing_message};
    use rustix::io::Errno;
    use rustix::thread::{CapabilitySet, CapabilitySets, capabilities, clear_ambient_capability_set, set_capabilities};

    /// The four capabilities as a set.
    pub fn required() -> CapabilitySet {
        CapabilitySet::SETUID | CapabilitySet::SETGID | CapabilitySet::CHOWN | CapabilitySet::KILL
    }

    /// The names of the required capabilities that `permitted` lacks.
    pub fn missing(permitted: CapabilitySet) -> Vec<&'static str> {
        let wanted = [CapabilitySet::SETUID, CapabilitySet::SETGID, CapabilitySet::CHOWN, CapabilitySet::KILL];
        NAMES.iter().zip(wanted).filter(|(_, cap)| !permitted.contains(*cap)).map(|(name, _)| *name).collect()
    }

    fn set(effective: CapabilitySet, permitted: CapabilitySet) -> Result<(), Errno> {
        set_capabilities(None, CapabilitySets { effective, permitted, inheritable: CapabilitySet::empty() })
    }

    /// Checks the permitted set, proves that the effective set can be raised, and leaves the helper holding exactly the
    /// four capabilities in the permitted set and nothing in the other sets.
    pub fn init() -> Result<(), String> {
        let have = capabilities(None).map_err(|e| format!("the capability sets cannot be read: {e}"))?;
        let missing = missing(have.permitted);
        if !missing.is_empty() {
            return Err(missing_message(&missing));
        }
        let need = required();
        // The raise is the proof that the permitted set is usable; the set is lowered again at once.
        set(need, need).map_err(|e| format!("the effective capabilities cannot be raised: {e}"))?;
        clear_ambient_capability_set().map_err(|e| format!("the ambient capabilities cannot be cleared: {e}"))?;
        set(CapabilitySet::empty(), need).map_err(|e| format!("the effective capabilities cannot be lowered: {e}"))
    }

    /// [`init`], or one sentence on standard error and exit status 2.
    pub fn ensure_or_exit() {
        if let Err(message) = init() {
            eprintln!("holzwave-spawn-helper: {message}");
            std::process::exit(2);
        }
    }

    /// The four capabilities are effective while this value lives. A child forked in that time inherits them, which is
    /// what lets it change its ids before it drops everything.
    pub struct Raised(());

    impl Raised {
        /// Raises the effective set.
        pub fn new() -> Result<Raised, Errno> {
            let need = required();
            set(need, need)?;
            Ok(Raised(()))
        }
    }

    impl Drop for Raised {
        fn drop(&mut self) {
            // Lowering never fails for a process that was able to raise. If it somehow does, nothing may go on with a
            // capability that should be gone.
            if set(CapabilitySet::empty(), required()).is_err() {
                std::process::abort();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn missing_lists_what_is_not_permitted() {
            assert!(missing(required()).is_empty());
            assert_eq!(missing(CapabilitySet::empty()), NAMES.to_vec());
            assert_eq!(missing(CapabilitySet::SETUID | CapabilitySet::KILL), vec!["CAP_SETGID", "CAP_CHOWN"]);
            // More than the four is not a problem here.
            assert!(missing(required() | CapabilitySet::NET_ADMIN).is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_capabilities_and_where_they_are_set() {
        let text = missing_message(&["CAP_SETUID", "CAP_KILL"]);
        assert!(text.contains("CAP_SETUID, CAP_KILL"));
        assert!(text.contains("cap_setuid,cap_setgid,cap_chown,cap_kill=p"));
        assert!(text.contains("container must add"));
        assert!(!text.contains('\n'), "one sentence on one line");
    }
}
