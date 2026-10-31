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
    use std::sync::Arc;

    use serde_json::{Value, json};

    use crate::isolation_env::{
        Attempt, How, Person, Reply, World, canary, is_missing, is_permission_error, mode_of, owner_of, var, write_secret_file,
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

    // ---- two people and a table of attempts ----------------------------------------------------------------------------

    /// Alice (the administrator, user id 20000) and Bob (20001), each with a shell and a canary file in their own home.
    struct Duo {
        world: World,
        people: [Arc<Person>; 2],
        secrets: [String; 2],
        files: [String; 2],
    }

    /// One direction of an attack: who tries, whose files are the target.
    struct Side<'a> {
        attacker: &'a Person,
        victim: &'a Person,
        victim_secret: &'a str,
        ctx: Ctx,
    }

    /// The paths a row builds its attempt from.
    struct Ctx {
        own: String,
        victim: String,
        vuid: u32,
        homes: String,
    }

    impl Ctx {
        fn vfile(&self) -> String {
            format!("{}/secret.txt", self.victim)
        }
    }

    enum Probe {
        Open(String),
        Stat(String),
        List(String),
        Save(String),
        Shell(String),
    }

    /// One line of the table: a name, a command the attacker's shell runs first (to make a link, say), and the attempt.
    struct Row {
        name: &'static str,
        setup: Option<fn(&Ctx) -> String>,
        probe: fn(&Ctx) -> Probe,
    }

    impl Duo {
        async fn start() -> Duo {
            let world = World::start().await;
            let alice = world.person("alice").await;
            let bob = world.person("bob").await;
            let secrets = [canary("alice"), canary("bob")];
            // Each target exists and its owner reads it: `plant` checks both before it returns.
            let files = [world.plant(&alice, "secret.txt", &secrets[0]).await, world.plant(&bob, "secret.txt", &secrets[1]).await];
            Duo { world, people: [alice, bob], secrets, files }
        }

        fn sides(&self) -> Vec<Side<'_>> {
            (0..2)
                .map(|attacker| {
                    let victim = 1 - attacker;
                    Side {
                        attacker: &self.people[attacker],
                        victim: &self.people[victim],
                        victim_secret: &self.secrets[victim],
                        ctx: Ctx {
                            own: self.people[attacker].home.display().to_string(),
                            victim: self.people[victim].home.display().to_string(),
                            vuid: self.people[victim].uid,
                            homes: self.world.homes().display().to_string(),
                        },
                    }
                })
                .collect()
        }

        /// Runs every row in both directions and requires a refusal by the operating system each time.
        async fn rows(&self, group: &str, rows: &[Row]) {
            for side in self.sides() {
                for row in rows {
                    if let Some(setup) = row.setup {
                        let made = self.world.shell(side.attacker, &setup(&side.ctx)).await;
                        assert_eq!(made.status, 0, "{group}/{}: the setup of {} works: {}", row.name, side.attacker.name, made.text);
                    }
                    let probe = (row.probe)(&side.ctx);
                    let how = match &probe {
                        Probe::Open(path) => How::Open(path),
                        Probe::Stat(path) => How::Stat(path),
                        Probe::List(path) => How::List(path),
                        Probe::Save(path) => How::Save(path, "planted by the other person"),
                        Probe::Shell(command) => How::Shell(command),
                    };
                    let attempt = self.world.attempt(side.attacker, how).await;
                    println!("row {group}/{} {} -> {}: {}", row.name, side.attacker.name, side.victim.name, attempt.text.trim());
                    assert_refused(&format!("{group}/{} {} -> {}", row.name, side.attacker.name, side.victim.name), &attempt, side.victim_secret);
                }
            }
        }

        /// The canary of each person is in no message the other's sockets received; then the world ends in order.
        async fn finish(self) {
            for (index, person) in self.people.iter().enumerate() {
                assert!(!person.saw(&self.secrets[1 - index]).await, "nothing {} received holds the other person's canary", person.name);
            }
            self.world.finish().await;
        }
    }

    /// A refusal is the operating system's permission error. A missing file is a failed probe, not a pass.
    fn assert_refused(label: &str, attempt: &Attempt, secret: &str) {
        assert!(!attempt.ok, "{label}: the attempt must fail, it said: {}", attempt.text);
        if let Some(status) = attempt.status {
            assert_ne!(status, 0, "{label}: a refused command exits with a failure status");
        }
        assert!(is_permission_error(&attempt.text), "{label}: the refusal is a permission error, not this: {}", attempt.text);
        assert!(!is_missing(&attempt.text), "{label}: a missing file is not a refusal: {}", attempt.text);
        assert!(secret.is_empty() || !attempt.text.contains(secret), "{label}: the answer holds the canary");
    }

    // ---- homes: paths ---------------------------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_an_absolute_path_to_the_other_file_is_refused() {
        let duo = Duo::start().await;
        duo.rows(
            "absolute",
            &[
                Row { name: "open", setup: None, probe: |c| Probe::Open(c.vfile()) },
                Row { name: "stat", setup: None, probe: |c| Probe::Stat(c.vfile()) },
                Row { name: "cat", setup: None, probe: |c| Probe::Shell(format!("cat '{}'", c.vfile())) },
            ],
        )
        .await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_dotdot_path_from_the_own_home_is_refused() {
        let duo = Duo::start().await;
        duo.rows(
            "dotdot",
            &[
                Row { name: "open", setup: None, probe: |c| Probe::Open(format!("{}/../{}/secret.txt", c.own, c.vuid)) },
                Row { name: "stat", setup: None, probe: |c| Probe::Stat(format!("{}/../{}/secret.txt", c.own, c.vuid)) },
                Row { name: "cat", setup: None, probe: |c| Probe::Shell(format!("cat '{}/../{}/secret.txt'", c.own, c.vuid)) },
                Row { name: "open_deep", setup: None, probe: |c| Probe::Open(format!("{}/.tmp/../../{}/secret.txt", c.own, c.vuid)) },
            ],
        )
        .await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_path_with_dot_segments_is_refused() {
        let duo = Duo::start().await;
        duo.rows(
            "dots",
            &[
                Row { name: "open_dot_dotdot", setup: None, probe: |c| Probe::Open(format!("{}/./../{}/./secret.txt", c.own, c.vuid)) },
                Row { name: "stat_dot_dotdot", setup: None, probe: |c| Probe::Stat(format!("{}/./../{}/./secret.txt", c.own, c.vuid)) },
                Row { name: "cat_dot_dotdot", setup: None, probe: |c| Probe::Shell(format!("cat '{}/./../{}/./secret.txt'", c.own, c.vuid)) },
            ],
        )
        .await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_doubled_slash_and_a_trailing_dot_are_refused() {
        let duo = Duo::start().await;
        duo.rows(
            "slashes",
            &[
                Row { name: "open_doubled", setup: None, probe: |c| Probe::Open(format!("{}//{}//secret.txt", c.homes, c.vuid)) },
                Row { name: "open_trailing_dot", setup: None, probe: |c| Probe::Open(format!("{}/secret.txt/.", c.victim)) },
                Row { name: "cat_doubled", setup: None, probe: |c| Probe::Shell(format!("cat '{}//{}//secret.txt'", c.homes, c.vuid)) },
                Row { name: "list_trailing_dot", setup: None, probe: |c| Probe::List(format!("{}/.", c.victim)) },
            ],
        )
        .await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_symbolic_link_to_the_other_home_leads_nowhere() {
        let duo = Duo::start().await;
        // The link is the attacker's own, in the attacker's home, made by the attacker's shell: it exists and its owner sees
        // it. What is behind it is the other person's 0700 home.
        for side in duo.sides() {
            let made = duo.world.shell(side.attacker, &format!("ln -s '{}' '{}/link' && readlink '{}/link'", side.ctx.victim, side.ctx.own, side.ctx.own)).await;
            assert_eq!(made.status, 0, "{} makes a link: {}", side.attacker.name, made.text);
            assert_eq!(made.text.trim(), side.ctx.victim, "the link points at the other home");
            let link = duo.world.shell(side.attacker, &format!("ls -l '{}/link'", side.ctx.own)).await;
            assert!(link.text.starts_with('l'), "the owner sees the link: {}", link.text);
        }
        duo.rows(
            "link_to_home",
            &[
                Row { name: "open_through_link", setup: None, probe: |c| Probe::Open(format!("{}/link/secret.txt", c.own)) },
                Row { name: "stat_through_link", setup: None, probe: |c| Probe::Stat(format!("{}/link/secret.txt", c.own)) },
                Row { name: "list_through_link", setup: None, probe: |c| Probe::List(format!("{}/link", c.own)) },
                Row { name: "cat_through_link", setup: None, probe: |c| Probe::Shell(format!("cat '{}/link/secret.txt'", c.own)) },
                Row { name: "ls_through_link", setup: None, probe: |c| Probe::Shell(format!("ls '{}/link/'", c.own)) },
            ],
        )
        .await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_symbolic_link_to_the_other_file_leads_nowhere() {
        let duo = Duo::start().await;
        duo.rows(
            "link_to_file",
            &[
                Row {
                    name: "open_link_to_file",
                    setup: Some(|c| format!("ln -s '{}' '{}/l2'", c.vfile(), c.own)),
                    probe: |c| Probe::Open(format!("{}/l2", c.own)),
                },
                Row { name: "cat_link_to_file", setup: None, probe: |c| Probe::Shell(format!("cat '{}/l2'", c.own)) },
                Row { name: "cp_link_to_file", setup: None, probe: |c| Probe::Shell(format!("cp '{}/l2' '{}/copy.txt'", c.own, c.own)) },
            ],
        )
        .await;
        duo.finish().await;
    }

    // ---- homes: listings -------------------------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_listing_the_other_home_is_refused() {
        let duo = Duo::start().await;
        duo.rows(
            "list_home",
            &[
                Row { name: "files_list", setup: None, probe: |c| Probe::List(c.victim.clone()) },
                Row { name: "ls", setup: None, probe: |c| Probe::Shell(format!("ls -la '{}'", c.victim)) },
                Row { name: "ls_hidden_data", setup: None, probe: |c| Probe::Shell(format!("ls -la '{}/.holzwave'", c.victim)) },
                Row { name: "list_hidden_data", setup: None, probe: |c| Probe::List(format!("{}/.holzwave", c.victim)) },
                Row { name: "find", setup: None, probe: |c| Probe::Shell(format!("find '{}' -type f", c.victim)) },
            ],
        )
        .await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_the_directory_that_holds_the_homes_cannot_be_listed() {
        let duo = Duo::start().await;
        // The directory is traversable (a person's worker must reach its own home through it) and not listable: the other
        // user ids are not learned from it. Mode 0711 owned by the supervisor's user.
        let homes = duo.world.homes();
        assert_eq!(mode_of(&homes), 0o711, "the directory that holds the homes");
        assert_eq!(owner_of(&homes).0, 10_001);
        println!("homes directory {} mode {:o}", homes.display(), mode_of(&homes));
        // The supervisor's user can list it: the target exists and is readable by its owner.
        let names: Vec<String> = std::fs::read_dir(&homes).expect("the owner lists the homes").map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"20000".to_owned()) && names.contains(&"20001".to_owned()), "{names:?}");
        duo.rows(
            "list_homes",
            &[
                Row { name: "files_list", setup: None, probe: |c| Probe::List(c.homes.clone()) },
                Row { name: "ls", setup: None, probe: |c| Probe::Shell(format!("ls '{}'", c.homes)) },
                Row { name: "ls_a", setup: None, probe: |c| Probe::Shell(format!("ls -la '{}'", c.homes)) },
                Row { name: "find", setup: None, probe: |c| Probe::Shell(format!("find '{}' -maxdepth 1", c.homes)) },
            ],
        )
        .await;
        // Not even the other's user id is learned from a listing: the answers above hold only the error.
        for side in duo.sides() {
            let listing = duo.world.attempt(side.attacker, How::Shell(&format!("ls '{}' 2>&1", side.ctx.homes))).await;
            assert!(!listing.text.lines().any(|line| line.trim() == side.ctx.vuid.to_string()), "no other user id in the listing: {}", listing.text);
        }
        duo.finish().await;
    }

    // ---- homes: writes ---------------------------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_save_into_the_other_home_is_refused_and_changes_nothing() {
        let duo = Duo::start().await;
        duo.rows(
            "save",
            &[
                Row { name: "save_new_file", setup: None, probe: |c| Probe::Save(format!("{}/planted.txt", c.victim)) },
                Row { name: "save_over_the_secret", setup: None, probe: |c| Probe::Save(c.vfile()) },
                Row { name: "save_through_dotdot", setup: None, probe: |c| Probe::Save(format!("{}/../{}/planted.txt", c.own, c.vuid)) },
            ],
        )
        .await;
        duo.finish_unchanged().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_a_shell_write_into_the_other_home_is_refused_and_changes_nothing() {
        let duo = Duo::start().await;
        duo.rows(
            "write",
            &[
                Row { name: "redirect_new_file", setup: None, probe: |c| Probe::Shell(format!("printf x > '{}/planted.txt'", c.victim)) },
                Row { name: "redirect_over_the_secret", setup: None, probe: |c| Probe::Shell(format!("printf x > '{}'", c.vfile())) },
                Row { name: "append_to_the_secret", setup: None, probe: |c| Probe::Shell(format!("printf x >> '{}'", c.vfile())) },
                Row { name: "touch", setup: None, probe: |c| Probe::Shell(format!("touch '{}/touched'", c.victim)) },
                Row { name: "rm_the_secret", setup: None, probe: |c| Probe::Shell(format!("rm -f '{}'", c.vfile())) },
                Row { name: "mv_the_secret", setup: None, probe: |c| Probe::Shell(format!("mv '{}' '{}/stolen.txt'", c.vfile(), c.own)) },
                Row { name: "chmod_the_secret", setup: None, probe: |c| Probe::Shell(format!("chmod 0644 '{}'", c.vfile())) },
            ],
        )
        .await;
        duo.finish_unchanged().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_links_directories_and_hard_links_in_or_to_the_other_home_are_refused() {
        let duo = Duo::start().await;
        duo.rows(
            "links",
            &[
                Row { name: "symlink_into_victim", setup: None, probe: |c| Probe::Shell(format!("ln -s /etc/hostname '{}/hostlink'", c.victim)) },
                Row { name: "mkdir_in_victim", setup: None, probe: |c| Probe::Shell(format!("mkdir '{}/dir'", c.victim)) },
                Row { name: "hard_link_to_the_secret", setup: None, probe: |c| Probe::Shell(format!("ln '{}' '{}/mine'", c.vfile(), c.own)) },
                Row { name: "hard_link_into_victim", setup: None, probe: |c| Probe::Shell(format!("ln /etc/hostname '{}/hard'", c.victim)) },
                Row { name: "copy_the_secret", setup: None, probe: |c| Probe::Shell(format!("cp '{}' '{}/copy.txt'", c.vfile(), c.own)) },
            ],
        )
        .await;
        duo.finish_unchanged().await;
    }

    impl Duo {
        /// After attempts to write: each person's home holds exactly what the person put there, and the secret is intact.
        async fn finish_unchanged(self) {
            for (index, person) in self.people.iter().enumerate() {
                let listing = self.world.shell(person, "ls -a1").await;
                for planted in ["planted.txt", "touched", "hostlink", "dir", "hard", "stolen.txt", "mine", "copy.txt", "l2", "link"] {
                    let there = listing.text.lines().any(|line| line == planted);
                    // `link`, `l2`, `mine` and `copy.txt` are the attacker's own names; they may be in the attacker's own home only
                    // when this test made them, and a name from the other person's attempt is never in this home.
                    if matches!(planted, "planted.txt" | "touched" | "hostlink" | "dir" | "hard" | "stolen.txt") {
                        assert!(!there, "{planted} was planted in {}'s home: {}", person.name, listing.text);
                    }
                }
                assert_eq!(person.read(&self.files[index]).await.expect("the owner reads the secret"), self.secrets[index], "the secret of {} is intact", person.name);
            }
            self.finish().await;
        }
    }

    // ---- procs: signals --------------------------------------------------------------------------------------------------

    /// The processes of a person that another person might aim at: the worker, the pane's shell, and a child of the shell.
    async fn person_targets(duo: &Duo, victim: &Person) -> Vec<(&'static str, i32)> {
        let worker = duo.world.worker_pid(victim).unwrap_or_else(|| panic!("{} has a worker", victim.name));
        let shell = duo.world.shell_pid(victim).await;
        let child = duo.world.shell(victim, "sleep 300 > /dev/null 2>&1 & echo $!").await;
        let child: i32 = child.text.trim().parse().unwrap_or_else(|_| panic!("the child's pid, not {:?}", child.text));
        vec![("worker", worker), ("shell", shell), ("child", child)]
    }

    /// The processes of the server itself: the spawn helper and the supervisor (the test process).
    fn server_targets(duo: &Duo) -> Vec<(&'static str, i32)> {
        vec![("helper", duo.world.helper_pid().expect("the helper runs")), ("supervisor", std::process::id() as i32)]
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_a_signal_to_the_other_persons_worker_and_shells_is_refused_and_they_survive() {
        let duo = Duo::start().await;
        for side in duo.sides() {
            let targets = person_targets(&duo, side.victim).await;
            for (what, pid) in &targets {
                let row = duo.world.table().into_iter().find(|row| row.pid == *pid).unwrap_or_else(|| panic!("{what} {pid} exists"));
                assert!(row.has_uid(side.victim.uid) && !row.is_dead(), "{what} {pid} is a living process of {}", side.victim.name);
                for signal in ["0", "TERM", "STOP", "KILL"] {
                    let attempt = duo.world.attempt(side.attacker, How::Shell(&format!("kill -{signal} {pid}"))).await;
                    println!("procs kill -{signal} {what} {pid} ({} -> {}): {}", side.attacker.name, side.victim.name, attempt.text.trim());
                    assert_refused(&format!("kill -{signal} {what} {pid}"), &attempt, side.victim_secret);
                }
            }
            // Every one still runs, none is stopped, and the other person still works.
            let survivors: Vec<String> = targets
                .iter()
                .map(|(what, pid)| {
                    let row = duo.world.table().into_iter().find(|row| row.pid == *pid).unwrap_or_else(|| panic!("{what} {pid} survives"));
                    assert!(!row.is_dead() && row.state != 'T', "{what} {pid} is in state {}", row.state);
                    format!("{what} {pid} state {}", row.state)
                })
                .collect();
            println!("procs survivors of {}: {survivors:?}", side.victim.name);
            assert!(side.victim.ask("get_layout", json!({})).await.is_ok(), "{}'s worker still answers", side.victim.name);
            assert_eq!(duo.world.shell(side.victim, "echo alive").await.text.trim(), "alive", "{}'s shell still runs", side.victim.name);
        }
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_a_signal_to_the_helper_and_the_supervisor_is_refused() {
        let duo = Duo::start().await;
        for side in duo.sides() {
            for (what, pid) in server_targets(&duo) {
                assert!(duo.world.alive(pid), "{what} {pid} runs");
                for signal in ["0", "TERM", "STOP", "KILL"] {
                    let attempt = duo.world.attempt(side.attacker, How::Shell(&format!("kill -{signal} {pid}"))).await;
                    assert_refused(&format!("kill -{signal} {what} {pid}"), &attempt, side.victim_secret);
                }
                assert!(duo.world.alive(pid), "{what} {pid} still runs");
            }
        }
        assert!(duo.world.supervisor.stats().helper_alive, "the helper is still reachable");
        duo.finish().await;
    }

    // ---- procs: what /proc shows ---------------------------------------------------------------------------------------

    /// The attacker's own shell is the control (the probe works on a process of the same user); then every other process is
    /// the target. A target must exist (its status text is readable by everyone) and the probe must be refused.
    async fn proc_refusals(duo: &Duo, group: &str, command: fn(i32) -> String) {
        for side in duo.sides() {
            let own = duo.world.shell_pid(side.attacker).await;
            let control = duo.world.shell(side.attacker, &command(own)).await;
            assert_eq!(control.status, 0, "{group}: the probe works on the attacker's own shell: {}", control.text);
            let mut targets = person_targets(duo, side.victim).await;
            targets.extend(server_targets(duo));
            for (what, pid) in targets {
                let probe = format!("grep -c '^Uid:' /proc/{pid}/status; {}", command(pid));
                let attempt = duo.world.attempt(side.attacker, How::Shell(&probe)).await;
                println!("procs {group} {what} {pid} ({} -> {}): {}", side.attacker.name, side.victim.name, attempt.text.replace('\n', " | "));
                assert_eq!(attempt.text.lines().next(), Some("1"), "{group}/{what}: the target exists and its status is public: {}", attempt.text);
                assert_refused(&format!("{group}/{what} {pid}"), &attempt, side.victim_secret);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_the_environment_of_other_processes_is_refused() {
        let duo = Duo::start().await;
        proc_refusals(&duo, "environ", |pid| format!("cat /proc/{pid}/environ > /dev/null")).await;
        // The owner can read what the attacker cannot: the test process (the supervisor) reads its own environment, and a
        // person's shell reads its own. The worker and the helper are marked undumpable by the kernel, so even their owner is
        // refused; the line below records that.
        assert!(!std::fs::read("/proc/self/environ").expect("the owner reads its environment").is_empty());
        let worker = duo.world.worker_pid(&duo.people[1]).expect("a worker");
        let by_owner = duo.world.shell(&duo.people[1], &format!("cat /proc/{worker}/environ > /dev/null")).await;
        println!("procs environ of a worker read by the worker's own person: status {} {}", by_owner.status, by_owner.text.trim());
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_the_memory_of_other_processes_is_refused() {
        let duo = Duo::start().await;
        // Opening /proc/<pid>/mem is the probe: it is what a debugger would do first. The shell has no tracer to attach.
        proc_refusals(&duo, "mem", |pid| format!(": < /proc/{pid}/mem")).await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_the_descriptors_of_other_processes_are_refused() {
        let duo = Duo::start().await;
        proc_refusals(&duo, "fd", |pid| format!("ls /proc/{pid}/fd > /dev/null")).await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_the_working_directory_and_the_program_of_other_processes_are_refused() {
        let duo = Duo::start().await;
        proc_refusals(&duo, "cwd", |pid| format!("ls -l /proc/{pid}/cwd /proc/{pid}/root /proc/{pid}/exe > /dev/null")).await;
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_the_shells_of_the_image_are_recorded() {
        let duo = Duo::start().await;
        // The probes of these tests use bash builtins (`kill`, `: <`, `exec`): the pane runs bash, as the helper's default says.
        let alice = &duo.people[0];
        let facts = duo.world.shell(alice, "ls -l /bin/sh; readlink -f /bin/sh; echo bash=$BASH_VERSION; readlink /proc/$$/exe; ls -ld /bin/bash").await;
        println!("shell facts:\n{}", facts.text.trim());
        assert_eq!(facts.status, 0, "{}", facts.text);
        assert!(facts.text.lines().any(|line| line.starts_with("bash=") && line.len() > "bash=".len()), "the pane runs bash: {}", facts.text);
        duo.finish().await;
    }

    // ---- procs: becoming someone else ---------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_a_person_cannot_become_another_user_or_give_a_file_away() {
        let duo = Duo::start().await;
        duo.rows(
            "become",
            &[
                Row { name: "setpriv_other_user", setup: None, probe: |c| Probe::Shell(format!("setpriv --reuid {0} --regid {0} --clear-groups id -u", c.vuid)) },
                Row { name: "setpriv_root", setup: None, probe: |_| Probe::Shell("setpriv --reuid 0 --regid 0 --clear-groups id -u".to_owned()) },
                Row {
                    name: "chown_to_the_other_user",
                    setup: Some(|c| format!("printf x > '{}/mine.txt'", c.own)),
                    probe: |c| Probe::Shell(format!("chown {} '{}/mine.txt'", c.vuid, c.own)),
                },
                Row { name: "chgrp_to_the_other_group", setup: None, probe: |c| Probe::Shell(format!("chgrp {} '{}/mine.txt'", c.vuid, c.own)) },
                Row { name: "chown_to_root", setup: None, probe: |c| Probe::Shell(format!("chown 0 '{}/mine.txt'", c.own)) },
            ],
        )
        .await;
        // `su` and `sudo`, where they exist, give no new user. Neither has a terminal or a password here; what matters is
        // that no output says user 0 and that the shell is still the person afterwards.
        for side in duo.sides() {
            let programs = duo.world.shell(side.attacker, "command -v su; command -v sudo; true").await;
            println!("procs become: su and sudo found: {:?}", programs.text.trim());
            let su = duo.world.shell_waiting(side.attacker, "setsid -w timeout -s KILL 10 su -c 'id -u' root < /dev/null 2>&1; echo su-status=$?", std::time::Duration::from_secs(40)).await;
            println!("procs become: su said {:?}", su.text.trim());
            assert!(!su.text.lines().any(|line| line.trim() == "0"), "su gave no root: {}", su.text);
            assert!(!su.text.contains("su-status=0"), "su did not succeed: {}", su.text);
            let sudo = duo.world.shell_waiting(side.attacker, "if command -v sudo > /dev/null; then sudo -n id -u 2>&1; fi; true", std::time::Duration::from_secs(40)).await;
            assert!(!sudo.text.lines().any(|line| line.trim() == "0"), "sudo gave no root: {}", sudo.text);
            let after = duo.world.shell(side.attacker, "id -u").await;
            assert_eq!(after.text.trim(), side.attacker.uid.to_string(), "{} is still {}", side.attacker.name, side.attacker.uid);
        }
        duo.finish().await;
    }

    // ---- the server's own files -------------------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_server_the_database_and_the_audit_files_are_unreadable() {
        let duo = Duo::start().await;
        let server = duo.world.server_dir();
        let database = server.join("server.sqlite3");
        let audit = server.join("audit");
        // The targets exist and the supervisor's user reads them.
        assert_eq!(mode_of(&server), 0o700, "the server directory");
        assert_eq!(owner_of(&server).0, 10_001);
        assert_eq!(mode_of(&audit), 0o700, "the audit directory");
        assert_eq!(mode_of(&database), 0o600, "the database");
        let head = std::fs::read(&database).expect("the supervisor reads its database");
        assert!(head.starts_with(b"SQLite format 3\0"), "the database is an SQLite file");
        let audit_file = std::fs::read_dir(&audit)
            .expect("the supervisor lists its audit directory")
            .map(|entry| entry.expect("entry").path())
            .find(|path| path.extension().is_some_and(|e| e == "jsonl"))
            .expect("the accounts that were made left an audit file");
        assert_eq!(mode_of(&audit_file), 0o600, "an audit file");
        assert!(!std::fs::read(&audit_file).expect("the supervisor reads its audit file").is_empty());
        println!("server files: server {:o} audit dir {:o} database {:o} audit file {:o}", mode_of(&server), mode_of(&audit), mode_of(&database), mode_of(&audit_file));

        // Both people are refused every way in.
        let (server_s, database_s, audit_s, audit_file_s) =
            (server.display().to_string(), database.display().to_string(), audit.display().to_string(), audit_file.display().to_string());
        for side in duo.sides() {
            let planted = format!("{server_s}/planted.txt");
            let shell_ls = format!("ls -la '{server_s}'");
            let shell_db = format!("cat '{database_s}' > /dev/null");
            let shell_audit = format!("ls '{audit_s}'");
            let shell_audit_file = format!("cat '{audit_file_s}' > /dev/null");
            let shell_write = format!("printf x > '{server_s}/planted.txt'");
            let table: Vec<(&str, How<'_>)> = vec![
                ("ls server", How::Shell(&shell_ls)),
                ("cat database", How::Shell(&shell_db)),
                ("ls audit", How::Shell(&shell_audit)),
                ("cat audit file", How::Shell(&shell_audit_file)),
                ("write into server", How::Shell(&shell_write)),
                ("list server", How::List(&server_s)),
                ("open database", How::Open(&database_s)),
                ("open audit file", How::Open(&audit_file_s)),
                ("stat database", How::Stat(&database_s)),
                ("save into server", How::Save(&planted, "x")),
            ];
            for (name, how) in table {
                let attempt = duo.world.attempt(side.attacker, how).await;
                println!("server files {name} ({}): {}", side.attacker.name, attempt.text.trim());
                assert_refused(&format!("server files/{name} {}", side.attacker.name), &attempt, "");
            }
        }
        assert!(!server.join("planted.txt").exists(), "nothing was planted in the server directory");
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_server_the_key_file_and_the_certificate_key_are_unreadable() {
        let duo = Duo::start().await;
        // The operator's key files live outside the data directory, in a directory others may traverse, with the mode the
        // product gives them: 0600, owned by the supervisor's user. Only the mode protects them.
        let keys = duo.world.data.with_file_name(format!("{}-keys", duo.world.data.file_name().expect("a name").to_string_lossy()));
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o711).create(&keys).expect("the key directory");
            std::fs::set_permissions(&keys, std::os::unix::fs::PermissionsExt::from_mode(0o711)).expect("mode");
        }
        let server_key = write_secret_file(&keys, "server.key", 0o600);
        let certificate_key = write_secret_file(&keys, "tls.key", 0o600);
        for path in [&server_key, &certificate_key] {
            assert_eq!(mode_of(path), 0o600);
            assert_eq!(owner_of(path).0, 10_001);
            assert_eq!(std::fs::read(path).expect("the supervisor reads its key").len(), 32, "the owner reads the key");
        }
        for side in duo.sides() {
            for path in [&server_key, &certificate_key] {
                let path = path.display().to_string();
                let shell_cat = format!("cat '{path}' > /dev/null");
                let shell_cp = format!("cp '{path}' '{}/stolen.key'", side.ctx.own);
                let shell_write = format!("printf x > '{path}'");
                let shell_rm = format!("rm -f '{path}'");
                for (name, how) in [
                    ("cat", How::Shell(&shell_cat)),
                    ("cp", How::Shell(&shell_cp)),
                    ("overwrite", How::Shell(&shell_write)),
                    ("open", How::Open(&path)),
                    ("save", How::Save(&path, "x")),
                ] {
                    let attempt = duo.world.attempt(side.attacker, how).await;
                    println!("key file {name} {} ({}): {}", path.rsplit('/').next().unwrap_or(""), side.attacker.name, attempt.text.trim());
                    assert_refused(&format!("key file/{name} {}", side.attacker.name), &attempt, "");
                }
                // Removing a file needs write permission on the directory, which the supervisor's user alone has.
                let removed = duo.world.attempt(side.attacker, How::Shell(&shell_rm)).await;
                assert_refused("key file/rm", &removed, "");
            }
        }
        for path in [&server_key, &certificate_key] {
            assert_eq!(std::fs::read(path).expect("the key is still there").len(), 32, "the key file is intact");
        }
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn homes_server_the_spawn_helper_cannot_be_run_or_copied_by_a_person() {
        let duo = Duo::start().await;
        let helper = var("HW_HELPER");
        // The supervisor's user runs it (it did, to start the helper); the listing shows who else may: nobody.
        assert!(duo.world.helper_pid().is_some(), "the supervisor's user runs the helper");
        for side in duo.sides() {
            let listing = duo.world.shell(side.attacker, &format!("ls -l '{helper}'")).await;
            println!("helper listing for {}: {}", side.attacker.name, listing.text.trim());
            assert!(listing.text.starts_with("-rwxr-x---"), "owner root, group of the supervisor, mode 0750: {}", listing.text);
            for (name, command) in [
                ("run", format!("'{helper}' --version")),
                ("exec", format!("exec '{helper}'")),
                ("run with the control socket", format!("'{helper}' --control-fd 0 --worker-path /bin/true --home-base '{}' < /dev/null", side.ctx.homes)),
            ] {
                let attempt = duo.world.attempt(side.attacker, How::Shell(&command)).await;
                assert_eq!(attempt.status, Some(126), "{name}: the shell cannot execute it: {}", attempt.text);
                assert_refused(&format!("helper/{name} {}", side.attacker.name), &attempt, "");
            }
            let copy = duo.world.attempt(side.attacker, How::Shell(&format!("cp '{helper}' '{}/helper-copy'", side.ctx.own))).await;
            assert_refused("helper/cp", &copy, "");
            let copied = duo.world.shell(side.attacker, &format!("test ! -e '{}/helper-copy'", side.ctx.own)).await;
            assert_eq!(copied.status, 0, "no copy of the helper was made");
        }
        duo.finish().await;
    }

    // ---- panes ------------------------------------------------------------------------------------------------------------

    /// Alice sends one command to every pane number from 1 to 64. Whatever she gets is hers: her own worker answers, or an
    /// error. Bob's panes keep their count, his shell runs and nothing she typed reaches it. One command per run, so that
    /// `close_pane` does not take her own panes away from the other commands.
    async fn pane_ids(command: &'static str, args: fn(u64) -> Value) {
        let duo = Duo::start().await;
        let (alice, bob) = (&duo.people[0], &duo.people[1]);
        let layout_panes = |view: &Value| view["panes"].as_array().map_or(0, Vec::len);
        let bob_view = match bob.ask("get_layout", json!({})).await {
            Reply::Ok(view) => view,
            Reply::Err(why) => panic!("bob's layout: {why}"),
        };
        let bob_panes = layout_panes(&bob_view);
        let bob_started = bob.panes();
        let mut replies = Vec::new();
        for pane in 1..=64u64 {
            let reply = alice.ask(command, args(pane)).await;
            replies.push((pane, reply));
        }
        let own_ok = replies.iter().filter(|(_, reply)| reply.is_ok()).count();
        let all_text: String = replies.iter().map(|(pane, reply)| format!("{pane}: {}\n", reply.text())).collect();
        assert!(!all_text.contains(&duo.secrets[1]), "no answer to alice holds bob's canary");
        println!("panes {command}: 64 ids tried by alice, {own_ok} answered ok (her own panes or a no-op), {} errors", 64 - own_ok);

        // Bob's side is as it was.
        let after = match bob.ask("get_layout", json!({})).await {
            Reply::Ok(view) => view,
            Reply::Err(why) => panic!("bob's layout: {why}"),
        };
        assert_eq!(layout_panes(&after), bob_panes, "bob's pane count is unchanged");
        assert_eq!(bob.panes(), bob_started);
        assert_eq!(duo.world.shell(bob, "echo alive").await.text.trim(), "alive", "bob's shell runs and nothing alice typed reached it");
        assert_eq!(bob.read(&duo.files[1]).await.expect("bob reads his file"), duo.secrets[1]);
        assert!(!bob.saw("hw-alice-marker").await, "alice's marker never reached bob");
        assert_eq!(duo.world.supervisor.stats().dropped_unknown_socket, 0, "no message named a socket that was not attached");
        duo.finish().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_panes_request_full_for_ids_one_to_sixty_four_reaches_only_the_own_worker() {
        pane_ids("request_full", |pane| json!({"pane": pane})).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_panes_selection_text_for_ids_one_to_sixty_four_reaches_only_the_own_worker() {
        pane_ids("selection_text", |pane| json!({"pane": pane})).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_panes_text_for_ids_one_to_sixty_four_reaches_only_the_own_worker() {
        pane_ids("text", |pane| json!({"pane": pane, "text": "hw-alice-marker"})).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "container test: sh scripts/web-container-test.sh isolation"]
    async fn procs_panes_close_pane_for_ids_one_to_sixty_four_reaches_only_the_own_worker() {
        pane_ids("close_pane", |pane| json!({"pane": pane})).await;
    }
}
