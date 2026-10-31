//! Two people in one container: homes, processes and capabilities, as the product runs them.
//!
//! The pure parts (the parser of `/proc/<pid>/status`, the audit of a process table, the check of a descriptor listing) run
//! anywhere, including a development Mac: `cargo test --locked --offline -p holzwave-server --test isolation`.
//!
//! The container tests exist on Linux only and are all ignored in a plain `cargo test`. `sh scripts/web-container-test.sh
//! isolation [-- <filter>]` builds the server binary and this test binary without the `in-process-workers` feature, puts a
//! copy of the spawn helper with the file capabilities `cap_setuid,cap_setgid,cap_chown,cap_kill=p` on a volume, and runs
//! this binary with `--ignored` in a container started as user 10001 with every capability dropped but those four, a
//! read-only root file system and tmpfs mounts for the data and temporary directories. The test process is the supervisor:
//! it starts the real helper, which starts the real worker for each person, which starts real shells in real pseudo-terminals.
//!
//! The tests are named so that a filter selects a group: `tracer`, `homes`, `procs`, `server_files`, `panes`, `caps`,
//! `descriptors`, `limits`, `teardown`.
//!
//! Nothing here reads a person's home as the supervisor or as root. The only path into a home is the person's own worker, so
//! every result travels the real route: socket, command, worker, user id.

#[path = "support/proc.rs"]
mod proc;

#[cfg(target_os = "linux")]
#[path = "support/isolation_env.rs"]
mod isolation_env;

#[cfg(target_os = "linux")]
mod container {
    use crate::isolation_env::{
        How, World, canary, is_missing, is_permission_error, mode_of, owner_of,
    };

    // ---- the tracer -----------------------------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn tracer_the_build_has_no_in_process_workers() {
        assert!(!cfg!(feature = "in-process-workers"), "the supervisor under test can only use the helper");
        assert_eq!(rustix::process::getuid().as_raw(), 10_001, "the test process is the supervisor, user 10001");
        assert_eq!(rustix::process::getgid().as_raw(), 10_001);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn tracer_two_people_run_as_their_own_user_in_a_private_home() {
        let world = World::start().await;
        let alice = world.person("alice").await;
        let bob = world.person("bob").await;
        assert_eq!((alice.uid, bob.uid), (20_000, 20_001), "the first two accounts get the first two user ids of the range");

        // Each shell runs as its person, read from the shell itself.
        for person in [&alice, &bob] {
            let uid = world.shell(person, "id -u").await;
            let gid = world.shell(person, "id -g").await;
            let groups = world.shell(person, "id -G").await;
            assert_eq!(uid.text.trim(), person.uid.to_string(), "{}'s user id", person.name);
            assert_eq!(gid.text.trim(), person.uid.to_string(), "{}'s group id", person.name);
            assert_eq!(groups.text.trim(), person.uid.to_string(), "{} has no group but their own", person.name);
            let here = world.shell(person, "pwd").await;
            assert_eq!(here.text.trim(), person.home.display().to_string(), "the shell starts in the home");
            let listing = world.shell(person, "ls -ld .").await;
            assert!(listing.text.starts_with("drwx------"), "mode of the home: {}", listing.text);
            assert!(listing.text.contains(&format!("{} {}", person.uid, person.uid)), "owner of the home: {}", listing.text);
            // The metadata of the home is public to the supervisor's user (the directory above is traversable); its content is not.
            assert_eq!(mode_of(&person.home), 0o700);
            assert_eq!(owner_of(&person.home), (person.uid, person.uid));
            let inside = world.shell(person, "ls -a1").await;
            for wanted in [".holzwave", ".tmp"] {
                assert!(inside.text.lines().any(|line| line == wanted), "{wanted} is inside the home of {}: {}", person.name, inside.text);
            }
            println!("tracer: {} uid {} gid {} groups {} home {} {}", person.name, uid.text.trim(), gid.text.trim(), groups.text.trim(), person.home.display(), listing.text.trim());
        }

        // A file Alice's shell writes is read back by Alice's worker; Bob's worker is refused by the operating system.
        let alice_text = canary("alice");
        let bob_text = canary("bob");
        let alice_file = world.plant(&alice, "secret.txt", &alice_text).await;
        let bob_file = world.plant(&bob, "secret.txt", &bob_text).await;
        println!("tracer: alice read {alice_file}, {} bytes", alice_text.len());

        let refused = world.attempt(&bob, How::Open(&alice_file)).await;
        println!("tracer: bob asked for alice's file and got: {}", refused.text);
        assert!(!refused.ok, "bob's worker must not open alice's file: {}", refused.text);
        assert!(is_permission_error(&refused.text), "the refusal is the operating system's permission error: {}", refused.text);
        assert!(!is_missing(&refused.text), "a missing file is not a refusal: {}", refused.text);
        assert!(!refused.text.contains(&alice_text), "the answer does not hold the canary");

        let mirrored = world.attempt(&alice, How::Open(&bob_file)).await;
        assert!(!mirrored.ok && is_permission_error(&mirrored.text) && !is_missing(&mirrored.text), "alice is refused bob's file: {}", mirrored.text);
        assert!(!mirrored.text.contains(&bob_text));

        // Each still reads their own file, and no message either person's sockets received holds the other's canary.
        assert_eq!(alice.read(&alice_file).await.expect("alice reads her file"), alice_text);
        assert_eq!(bob.read(&bob_file).await.expect("bob reads his file"), bob_text);
        assert!(!alice.saw(&bob_text).await, "nothing alice received holds bob's canary");
        assert!(!bob.saw(&alice_text).await, "nothing bob received holds alice's canary");
        world.finish().await;
    }
}
