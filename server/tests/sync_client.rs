//! The host's link to a server and its sync transport against the real server: the real router and store behind a real socket on
//! the loopback address, and the runtimes that the apps use. The sign-in goes through the whole grant (the person's approval is
//! played by the rig), the keychain is a memory store, and the server's clock is the environment's manual one.
//!
//! The last test runs two devices of one person through the server's sync routes with the real engine and one passphrase.

#![cfg(feature = "in-process-workers")]

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/device_env.rs"]
mod device_env;
#[path = "support/edge.rs"]
mod edge;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use api_env::ApiEnv;
use device_env::{account_of, approve, devices_of, open, serve_loopback, turn_on};
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::server_link::TOKEN_ACCOUNT;
use holzwave_host::server::ServerRuntime;
use holzwave_host::sync::{SyncRuntime, Transport};
use holzwave_server::device::POLL_INTERVAL;
use holzwave_server::http::features::Feature;

/// One device of a person: its own directory, its own keychain stand-in and its own link.
struct Device {
    dir: PathBuf,
    store: Arc<MemoryStore>,
    server: Arc<ServerRuntime>,
    sync: Arc<SyncRuntime>,
}

fn device(tag: &str) -> Device {
    let dir = std::env::temp_dir().join(format!("hw-sync-client-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory");
    let store = Arc::new(MemoryStore::default());
    let server = Arc::new(ServerRuntime::new_for_loopback(dir.clone(), store.clone()));
    let sync = Arc::new(SyncRuntime::new(dir.clone(), store.clone(), server.clone()));
    Device {
        dir,
        store,
        server,
        sync,
    }
}

/// Runs a blocking call of a runtime on a thread that may block, while the server goes on in this test's runtime.
async fn blocking<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(call)
        .await
        .expect("the runtime's thread")
}

/// Signs `device` in at the server of `origin` as `who`: start, the person's approval, and the poll that the client's own clock
/// allows once the server's interval is over.
async fn sign_in(
    env: &ApiEnv,
    who: &edge::Signed,
    device: &Device,
    origin: &str,
) -> holzwave_host::server::SignInPoll {
    let server = device.server.clone();
    let address = origin.to_owned();
    let started = blocking(move || {
        let started = server.sign_in_start(&address).expect("a sign-in starts");
        (started, std::time::Instant::now())
    })
    .await;
    let (view, began) = started;
    assert_eq!(view.interval, POLL_INTERVAL as u64);
    assert_eq!(approve(env, who, &view.code).await.status, 204);
    env.clock.advance(POLL_INTERVAL);
    let server = device.server.clone();
    blocking(move || {
        // The client's own clock holds a poll back until the interval the server stated is over.
        let wait = Duration::from_secs(POLL_INTERVAL as u64) + Duration::from_millis(100);
        std::thread::sleep(wait.saturating_sub(began.elapsed()));
        server.sign_in_poll().expect("a poll")
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_signs_in_through_the_whole_grant_reads_the_servers_switches_and_signs_out() {
    let env = ApiEnv::new();
    open(&env);
    let alice = env.member("alice");
    let account = account_of(&env, "alice");
    let lb = serve_loopback(&env).await;
    let phone = device("grant");

    assert_eq!(phone.server.view().state, "signed_out");
    let polled = sign_in(&env, &alice, &phone, &lb.origin).await;
    assert_eq!(polled.state, "approved");

    // The token is in the keychain, the address and the device in the file, and the server has the device.
    assert!(phone
        .store
        .get(TOKEN_ACCOUNT)
        .expect("read")
        .is_some_and(|t| t.starts_with("hwd_")));
    let file = std::fs::read_to_string(phone.dir.join("server_link.json")).expect("the link file");
    assert!(file.contains(&lb.origin) && !file.contains("hwd_"), "{file}");
    let devices = devices_of(&env, account);
    assert_eq!(devices.len(), 1);
    assert!(file.contains(&format!("\"device_id\": \"{}\"", devices[0].id)));

    // Who the server says this is: the remote switch is on and the sync one is off.
    let server = phone.server.clone();
    let view = blocking(move || server.refresh()).await;
    assert_eq!(
        (view.state, view.sync_on, view.remote_on, view.error.as_deref()),
        ("signed_in", false, true, None)
    );
    turn_on(&env, Feature::Sync);
    let server = phone.server.clone();
    let view = blocking(move || server.refresh()).await;
    assert!(view.sync_on && view.remote_on, "{view:?}");

    // Sign out ends the device at the server and forgets the token here; the address stays.
    let server = phone.server.clone();
    let view = blocking(move || server.sign_out().expect("signed out")).await;
    assert_eq!(
        (view.state, view.url.as_str()),
        ("signed_out", lb.origin.as_str())
    );
    assert!(devices_of(&env, account).is_empty());
    assert_eq!(phone.store.get(TOKEN_ACCOUNT).expect("read"), None);
    assert!(phone.dir.join("server_link.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_the_person_revokes_is_seen_as_signed_out_on_its_next_request() {
    let env = ApiEnv::new();
    open(&env);
    let alice = env.member("alice");
    let account = account_of(&env, "alice");
    let lb = serve_loopback(&env).await;
    let laptop = device("revoked");
    assert_eq!(
        sign_in(&env, &alice, &laptop, &lb.origin).await.state,
        "approved"
    );
    let server = laptop.server.clone();
    assert_eq!(blocking(move || server.refresh()).await.state, "signed_in");

    // The person revokes it in their own list.
    let id = devices_of(&env, account)[0].id;
    let revoked = env
        .send(env.delete_as(&format!("{}/{id}", device_env::MY_DEVICES), &alice))
        .await;
    assert_eq!(revoked.status, 204, "{}", revoked.text());

    // The next request the device makes is refused, and the link reads as not signed in with the address kept.
    let server = laptop.server.clone();
    let view = blocking(move || server.refresh()).await;
    assert_eq!((view.state, view.url.as_str()), ("signed_out", lb.origin.as_str()));
    assert_eq!(laptop.store.get(TOKEN_ACCOUNT).expect("read"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_devices_of_one_person_sync_through_the_server_with_one_passphrase() {
    let env = ApiEnv::new();
    turn_on(&env, Feature::Sync);
    let alice = env.member("alice");
    let lb = serve_loopback(&env).await;
    let first = device("sync-a");
    let second = device("sync-b");
    assert_eq!(
        sign_in(&env, &alice, &first, &lb.origin).await.state,
        "approved"
    );
    assert_eq!(
        sign_in(&env, &alice, &second, &lb.origin).await.state,
        "approved"
    );

    // The first device sets the server up with its files and the passphrase.
    std::fs::write(first.dir.join("settings.json"), br#"{"font_size":14}"#).expect("a file");
    let view = blocking({
        let sync = first.sync.clone();
        move || sync.use_server("a long enough passphrase")
    })
    .await
    .expect("the first run");
    assert_eq!((view.transport, view.state), (Transport::Server, "idle"));
    assert!(view.files >= 1);

    // The second device, with its own token and the same passphrase, ends with the same files; a wrong passphrase is refused.
    let wrong = blocking({
        let sync = second.sync.clone();
        move || sync.use_server("not the passphrase at all")
    })
    .await;
    assert!(wrong.is_err());
    assert!(!second.dir.join("sync.json").exists());
    let view = blocking({
        let sync = second.sync.clone();
        move || sync.use_server("a long enough passphrase")
    })
    .await
    .expect("the second run");
    assert!(view.files >= 1 && view.state == "idle", "{view:?}");

    // Revoking the first device at the server makes its next run end as signed out and not as a generic error.
    let account = account_of(&env, "alice");
    let id = devices_of(&env, account)
        .iter()
        .map(|d| d.id)
        .min()
        .expect("a device");
    let revoked = env
        .send(env.delete_as(&format!("{}/{id}", device_env::MY_DEVICES), &alice))
        .await;
    assert_eq!(revoked.status, 204);
    let view = blocking({
        let sync = first.sync.clone();
        move || sync.run()
    })
    .await
    .expect("a refused token is a state");
    assert_eq!((view.state, view.error), ("signed_out", None));
}
