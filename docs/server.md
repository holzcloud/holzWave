# Server guide

holzWave Server is the web build: one container that serves the holzWave page to browsers and runs every person's terminals as
a user of their own. This guide is for someone who has run Docker or Kubernetes before and wants to run it.

Built and tested against a test identity provider, virtual authenticators, the container tests and a Pebble certificate authority on the author's machines; not yet run against a real cluster, a real identity provider, hardware authenticators or real networks.

The rows that close those gaps are Part 10 of [test-plan.md](test-plan.md). Until you have run a row, treat what this guide says
about that thing as written from the code and the tests, not as seen working.

- [What it is and what it is not](#what-it-is-and-what-it-is-not)
- [Requirements](#requirements)
- [The server key](#the-server-key)
- [Run it with Compose](#run-it-with-compose)
- [Run it on Kubernetes with Helm](#run-it-on-kubernetes-with-helm)
- [TLS: three modes](#tls-three-modes)
- [Sign-in methods and single sign-on](#sign-in-methods-and-single-sign-on)
- [The first administrator](#the-first-administrator)
- [Invitations and roles](#invitations-and-roles)
- [Recovery and the secret store](#recovery-and-the-secret-store)
- [Sessions and limits](#sessions-and-limits)
- [Memory for pictures](#memory-for-pictures)
- [Device sign-in and devices](#device-sign-in-and-devices)
- [The two switches](#the-two-switches)
- [The sync target](#the-sync-target)
- [Remote sessions](#remote-sessions)
- [The audit log](#the-audit-log)
- [The data volume: back up and restore](#the-data-volume-back-up-and-restore)
- [Updates](#updates)
- [Compression](#compression)
- [Limits of this build](#limits-of-this-build)
- [When the server refuses to start](#when-the-server-refuses-to-start)

## What it is and what it is not

People open the address of the server in a browser, sign in, and get the holzWave window: workspaces, tabs, split panes, the
file browser, the AI chat and the rest, with the shells running on the server. The commands are the desktop's commands; a
person's panes, files and saved passwords live on the server. One browser tab or several show the same window.

The desktop and Android apps can use the server too, once an administrator turns a feature on: as the target of the
encrypted sync ([The sync target](#the-sync-target)), and to keep terminal sessions running on the server that an app shows in
a pane ([Remote sessions](#remote-sessions)). Both are off on every server until then ([The two switches](#the-two-switches)).
An app signs in as a device, through the person's own sign-in in a browser ([Device sign-in and devices](#device-sign-in-and-devices)).

It is not a service for strangers. One container serves one trust domain: people inside it share the network and the process
view of the container. It is not a cluster: one server runs on one volume. It has no mobile layout of the page, no shared
panes between people, no installable web app, no token for scripts or other programs and no operator for Kubernetes. A device
token is the sign-in of an app: it is never shown to anyone, it can be revoked at once, it is not bound to anything about the
device, and it ends after 90 days without use (`devices.idle`). It runs in the container this guide describes; the binary can
run elsewhere, but the isolation of people from each other is the container's, and this guide says nothing about running it outside one.

## Requirements

- A Linux host with Docker (Compose v2), or a Kubernetes cluster with Helm 3, on amd64 or arm64. The image is not
  published: you build it from this repository, which takes about five minutes and needs Docker with BuildKit and Internet
  access for the base images and the packages.
- A name that points at the server, such as `holzwave.example.net`. Passkeys need a host name, not an IP address, and TLS 1.3
  needs a certificate for that name.
- For the namespace of a cluster: the Pod Security level `baseline` (see [Run it on Kubernetes with
  Helm](#run-it-on-kubernetes-with-helm)).
- Memory and CPU for the people you expect: not measured yet. Row SRV-L1 asks for your numbers.

## The server key

The server needs one secret from you: a key file. It seals the authenticator-app secrets of the people who sign in with a
password and a code, binds the anti-forgery token to a session, and seals the certificate account of the `acme` mode. The
server never makes a key; you make it with keygen and back it up apart from the data volume. Without the same key a restored
volume cannot read the authenticator secrets it holds, and the server refuses to start without it.

Make it once, on your side, with the image you built:

```sh
docker build -f packaging/docker/Dockerfile -t holzwave-server .
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD:/out" -e HOLZWAVE_SERVER_KEY_FILE=/out/server.key holzwave-server keygen
```

The command prints the path of the file and never the key. The file has mode 0600 and is never overwritten: a second run
for the same path fails. Copy it to a place that is not the data volume (a password manager that keeps files, an encrypted
backup). The Compose file makes the key in a volume of its own, and you copy it out of that volume as well.

## Run it with Compose

The Compose file builds the image itself, runs it as user 10001 with a read-only root file system, drops every capability
and adds exactly four (CHOWN, KILL, SETGID, SETUID), keeps the data in a named volume and publishes one port.

1. `cd packaging/compose`
2. `cp server.files.example.json server.json`, and set `public_url` in it to your address.
3. Make a folder `tls` with your certificate chain as `tls.crt` and the private key as `tls.key`, both owned by user 10001
   (`chown 10001:10001 tls/tls.crt tls/tls.key`), the key with mode 0600. The folder is read-only inside the container.
4. `docker compose build`
5. `docker compose --profile setup run --rm keygen` makes the key in its own volume. Copy it out and keep it, as above.
6. `docker compose up -d --wait` starts the server and waits for its health check.
7. `docker compose logs holzwave | grep hwb_` prints the setup token for the [first
   administrator](#the-first-administrator).

`HOLZWAVE_PORT` changes the port on the host (8443 by default) and `HOLZWAVE_TLS_DIR` the folder of the certificate. For the
`acme` mode use `server.acme.example.json` in step 2 and publish port 443 (`HOLZWAVE_PORT=443`); the `tls` folder may be
empty then. The file `docker-compose.yml` has comments for every setting, and `server.json` is described in [config.md](config.md#the-server-configuration).

Do not add `no-new-privileges`, `privileged`, the Docker socket or a host namespace to this service. The first of these stops the
file capabilities of the spawn helper (see [Run it on Kubernetes with Helm](#run-it-on-kubernetes-with-helm)), and the server
then refuses to start.

## Run it on Kubernetes with Helm

The chart in `packaging/helm/holzwave` renders one Deployment, a Service, a claim for the data volume, a service account
without a token and, for the `proxy` mode only, an Ingress. It renders no Secret: the key, the certificate and the client
secrets of single sign-on providers are Secrets that exist already.

1. Label the namespace with the baseline level and make it:

   ```sh
   kubectl create namespace holzwave
   kubectl label namespace holzwave pod-security.kubernetes.io/enforce=baseline
   ```

2. Build the image, push it to a registry you control, and make the two Secrets (the key as above, then):

   ```sh
   kubectl -n holzwave create secret generic holzwave-server-key --from-file=server.key=./server.key
   kubectl -n holzwave create secret tls holzwave-tls --cert=tls.crt --key=tls.key
   ```

3. Install the chart:

   ```sh
   helm install holzwave packaging/helm/holzwave -n holzwave \
     -f packaging/helm/holzwave/ci/files-values.yaml \
     --set image.repository=<registry>/holzwave-server --set config.public_url=https://holzwave.example.net
   ```

4. Read the setup token: `kubectl -n holzwave logs deployment/holzwave | grep hwb_`.

The files `ci/files-values.yaml`, `ci/acme-values.yaml` and `ci/proxy-values.yaml` are the three examples of the modes; the keys
under `config` are the keys of the server's own file ([config.md](config.md#the-server-configuration)) in snake case.

Why the namespace needs the level `baseline`: the pod drops every capability and adds CHOWN, KILL, SETGID and SETUID, because the
spawn helper uses them to give each person's terminal a user id of its own, and the level `restricted` forbids adding them.
Why `allowPrivilegeEscalation` stays unset: setting it to `false` sets the kernel flag `no_new_privs`, and that stops the file
capabilities of the helper from working; the server then refuses to start and says the capabilities are not permitted to
the helper. A pod that fails with that sentence has been given a security context of your own or an admission policy that adds one. The chart's
security context is fixed and is not a value, on purpose. Row SRV-K2 asks you to see this fail closed.

The chart has no replica value and the schema refuses one. A key and a certificate are mounted as single files; after a
renewal of the certificate in the `files` mode, restart the pod. An upgrade stops the old pod before it starts the new one.

Not run yet: the chart has been linted, rendered and judged by the checker, and its configuration has been read by the server's
own `check-config`, but it has never been applied to a cluster. Row SRV-K1 is that run.

## TLS: three modes

The server speaks TLS 1.3 only, with HTTP/2 for the page and HTTP/1.1 for the WebSocket. There is no plain-HTTP mode.

- `files`: your certificate and key. After a renewal the server reloads a changed pair without a restart on Compose, and a bad
  pair never replaces a good one. In the chart the files are single-file mounts, so restart the pod.
- `acme`: the server gets and renews its own certificate from an ACME certificate authority (TLS-ALPN-01, on the server's own
  TLS port, which must therefore be reachable from the Internet as port 443). Set `tls.contact`, `tls.domains` (every name
  people use, including the host of `public_url`) and read the terms of the authority; `tls.terms_agreed` must be `true`, and
  the server never sets it for you. A certificate is renewed after two thirds of its life, or with seven days left. This mode
  was run against a Pebble test authority only; row SRV-T1 is a real one.
- `proxy`: the server listens for plain HTTP from the addresses in `tls.trusted_proxies` only, and a reverse proxy or ingress
  controller ends TLS. The server believes `X-Forwarded-For` and `X-Forwarded-Proto` from those addresses and from no one else,
  and the proxy must pass the WebSocket upgrades for `/ws` (the page) and `/ws/device` (the apps' remote sessions) and keep
  them open. Row SRV-R1 is a real proxy; the device socket was never run through one.

## Sign-in methods and single sign-on

Switch on at least one of `auth.passkey`, `auth.password_totp` and `auth.oidc`; the server refuses to start with none.

- A passkey signs in without typing a name and can unlock the saved passwords when the authenticator supports the PRF
  extension. Tested with virtual authenticators; rows SRV-P1 and SRV-P2 are real ones.
- A password (12 to 256 characters) with a six-digit code of an authenticator app. The page shows the secret and an `otpauth://`
  link; there is no QR code.
- Single sign-on through an OpenID Connect provider:
  1. At the provider, make a client for a web application with the redirect address `https://<your host>/auth/oidc/callback`
     and note the issuer, the client id and the client secret.
  2. Put the client secret in a file the server user can read (one line, no other text) and mount it. The secret is never a value in the
     configuration.
  3. Add an entry under `auth.oidc` (the example `server/oidc.json` in [config.md](config.md#examples)), with `allowed_domains`
     or `allowed_groups` set so that not everyone at the provider gets in.
  4. Restart the server and look for the provider's button on the sign-in page.

  The first sign-in makes a member account, matched later by the issuer and the subject of the ID token and never by the e-mail
  address. The server uses the code flow with PKCE and checks the ID token strictly. It was tested against a test provider;
  row SRV-I1 is a real one.

## The first administrator

A server with no account prints one line at every start: `To create the first administrator open <url> and use the one-time
token hwb_... within 60 minutes.` Open the address in a browser. The setup page asks for the token, a display name and a
sign-in method, and the account it makes is the first administrator. The token is shown on standard output only, never in a
file, and only its hash is kept; a restart prints a new one while no account exists. Anyone who can read the log can make the
first account, so do it first.

## Invitations and roles

There are two roles. An administrator opens `People…` from the palette (the entry exists only for administrators), creates an
invitation (1, 3, 7, 14 or 30 days, 7 preselected, role `member` preselected), and sends the link, which is shown once. The
link is `<public_url>/#invite=<token>`; the part after `#` never goes to a server. The person chooses a user name and a
sign-in method, saves a recovery code and is in. In the same dialog an administrator can change a role, disable and enable an
account, lift a sign-in lock, reset a person's secrets and delete an account. The last administrator cannot be demoted,
disabled or deleted. Disabling or deleting an account ends its sockets and its worker at once and keeps the home.

## Recovery and the secret store

A person's saved passwords and tokens (AI keys, integration secrets, sync secrets) live in one encrypted file in the person's
home. The key that opens it is not on the server: it comes from the person's sign-in. It lives while a session of the person is
open and leaves the server's memory when the last session ends, whether by sign-out, idle time, absolute time, disabling or
deleting. A sign-in that has the right credential (a password, or a passkey with PRF) opens the store at once; a passkey
without PRF, or a single sign-on, leaves it locked until the person types the passphrase or the recovery code, and the status
line shows `secrets locked` with an `Unlock` button. Work that needs the passwords while they are locked reports that secrets
are locked and resumes after the unlock.

A recovery code is made when the account is made and is shown once; it unlocks the store if the person forgets how to. An
administrator can reset a person's secrets and can never read them. A reset removes the file, and the person starts with an empty
store and makes a new recovery code.

## Sessions and limits

A session ends after 30 minutes without input or a day in total, and changing a credential asks for a sign-in within the last
5 minutes; the keys `sessions.idle`, `sessions.absolute` and `sessions.step_up` change these. Five wrong passwords for a name or
twenty for an address within 15 minutes lock the password path for 15 minutes; a locked sign-in is refused before anything is
computed, an unknown name behaves like a known one, passkeys and single sign-on are not throttled, and an administrator can
lift a lock. A person has at most eight sockets, 64 panes (the default), two uploads and eight downloads at a time; the socket
of an app that shows remote sessions counts as one of the eight, and a remote session as one of the 64 panes. Every size,
rate and time of the server has a name, a default and a bound in the `limits` section of [config.md](config.md#the-server-configuration);
the server refuses a value outside its bound at start.

## Memory for pictures

A program in a pane can send pictures (the Kitty graphics protocol), and the terminal keeps them for as long as the screen shows
them. The key `workers.max_image_mib` is the most MiB of pictures that one pane keeps, on each of its two screens: the normal
one and the one a full-screen program switches to. The default is 64, the range is 0 to 4096, and `0` turns pictures off on
this server. A pane never keeps more than 512 MiB, whatever the key says (the same number tops the range of the person's own
setting), so a value above 512 has no further effect. The cap wins over the person's own setting `image_memory_mib`: a person who allows 512 MiB gets the cap, and a
person who allows 2 MiB under a cap of 8 gets 2 MiB. The server reads the key when it starts and hands it to every worker in
the first frame it sends, so a changed value applies after a restart, which ends every pane anyway.

What the cap bounds is the pictures a pane keeps. It does not bound everything:

- While one picture is still arriving, the pane also holds the part that has come. The terminal refuses any request that would
  take a pane past twice the cap plus 64 MiB, which is 192 MiB at the default, and that is the worst case of one hostile
  transmission in one pane. The engine tests measured it with a cap of 16 MiB on a release build on an Apple Silicon Mac: a
  program that never ends a transmission held 88 MiB counted and 162 MiB resident. The resident number is larger because freed
  memory stays mapped until the pane closes. At the default cap the resident number was not measured; the same ratio gives
  about 350 MiB, which is also what one legitimate 64 MiB picture costs (289 MiB measured).
- The sum over a server is bounded, but it is large: that number times the panes of everybody. A person may have 64 panes by
  default, so one person whose every pane is hit at once is bounded by 64 times 192 MiB, about 12 GiB counted at the default
  cap. Lower `workers.max_image_mib` and `workers.max_panes` for a server that many people share.
- The cap says nothing about the memory of the shells and programs, the scrollback or the connections.
- The spawn helper sets no memory limit on a worker, and this build adds none. An operator who needs a hard limit sets the
  memory limit of the container or of the service (`--memory` of Docker, `resources.limits.memory` of Kubernetes, `MemoryMax=` of
  a systemd unit); the cap and the limit work together, the cap keeps one pane from taking all of it.

What ran: the tests start a worker, a supervisor over real workers and a whole server from a configuration file, make a real
shell print pictures of 1 to 3 MiB, and check that a picture over the cap is not placed and not readable, that the same picture
under a larger cap is, that the cap wins over the person's setting, and that `0` shows none. The worker tests also ran in the Linux container (Ubuntu on
an arm64 host). What did not run: a flood of hostile transmissions through a server, a server under a container memory limit,
and the engine's memory measurements on Linux or at the default cap.

## Device sign-in and devices

The desktop app (macOS, Windows, Linux) and the Android app sign in to a server as a **device**, with the device authorization
grant of OAuth (RFC 8628) and the person's own sign-in in a browser; the app never asks for a password, a passkey or a code of
an authenticator app.

1. In the app the person chooses `Sign in…` (in the Connections view or the sync popover) and types the address of the server
   once. The app asks the server for a request and shows an eight-letter code, such as `BCDF-GHJK`, made of the twenty
   consonants `BCDFGHJKLMNPQRSTVWXZ`.
2. The app opens the system browser at `<public_url>/#device=<code>`. The code is in the fragment, which a browser never sends
   to a server. The person signs in there the usual way; the page keeps the code through a sign-in with a passkey or a password,
   and asks to confirm it is the person when the sign-in is older than `sessions.step_up`. A sign-in through single sign-on
   leaves the page for the provider and loses the code: `Open again` in the app opens it once more. An administrator or a
   member can also type a code into the palette entry `Authorize device…`.
3. The page `Authorize device` shows the name of the device, its platform, the network address the request came from, when it
   was made, and the code to compare with the app, and says to approve only what the person started. `Approve` or `Deny`.
4. The app asks the server every 5 seconds whether the request was decided. A request lives 10 minutes. After the approval the
   next answer carries the device token, once; the app puts it into the OS keychain (the account `server:token`) and keeps the
   address, a device id and the name of the device in `server_link.json`, a file of this computer that is never synced.

The token is `hwd_` and 43 characters (256 random bits); the request's own code is the same length with `hwg_`. The server keeps
only the SHA-256 of each, never the token, writes neither to a log or an audit line, and no answer except that one delivery holds
the token. Bounds against guessing and floods: an app that asks too soon is told `slow_down` and must wait 5 seconds longer each
time, up to 60; five wrong codes typed by a person within ten minutes lock that account and that address out of typing codes for
ten minutes (a code that was never issued and one that expired give the same answer); five unknown requests asked for by one
address within ten minutes lock that address for ten minutes; at most 5 open requests come from one address and 200 from
everywhere, and the next start is refused (429, with a hint to try again in a minute) until older ones run out; a person has
at most 20 devices, and an approved request past that waits, answered `too_many`, until a device is revoked or the request
runs out.

A device token ends after `devices.idle` (90 days by default) without a request; every request renews it, and the time it was last
seen is written at most once a minute. In the browser, the Account dialog lists the person's devices (name, platform, last seen)
with `Revoke`, and the People dialog lists the devices of everybody with their owner for an administrator. A revocation asks
for a second click and a recent sign-in, deletes the device and closes its open sockets at once (code 4401); the app learns
at its next request that it is no longer signed in. `Sign out` in the app asks the server to end that device's sign-in, removes
the token from the keychain and keeps the address. The devices of a disabled account are refused, and those of a deleted
account are deleted with it. The lists and the revocation work while both switches are off, so a device can always be
ended.

No binding to the device and no rotation, on purpose. The token is the credential of one self-hosted server for its own app:
it travels only to the server that issued it, in one `Authorization` header over TLS, and lives in the OS keychain. Binding it
to a key of the device (DPoP or mutual TLS) needs a key pair on every platform, a signed proof on every request and a store of
nonces on the server; rotating it adds a second credential and a path on which a lost answer signs a device out. Both were
considered and left out of this build. The controls are the hash at rest, the revocation that takes effect at once and the
idle expiry (90 days by default). A token that is stolen from a device works for whoever holds it until it is revoked or
unused for `devices.idle`.

Approving a device can be phished: a person can be led to approve a code that somebody else started. The page shows the device,
its platform and the address of the request, and says to approve only what you started; nothing else stops it.

On the author's Mac the system's certificate check took 40 to 80 seconds to refuse a server with a certificate the Mac does
not trust, so an app pointed at such a server waits that long before its sign-in fails.

Tested against the real server in one process: the whole grant, its refusals and bounds, the lists, the revocation and the
idle expiry (`server/tests/device_grant.rs`, `server/tests/device_tokens.rs`), the app's client of the grant
(`server/tests/device_client.rs`), and the page in WebKit and Chromium against the real server (`app/tests/web/devices.test.mjs`).
Not run: a real phone or desktop against a real server over a real network, a real passkey or single sign-on approving a
device, the hand-back from the Android system browser to the app, and two real people.

## The two switches

Two features of the server are for the apps: `Sync target` and `Remote sessions`. Both are **off** on every server, a new one
and an upgraded one, until an administrator turns one on in the People dialog, in the group `Device access`. A change asks
for a sign-in within `sessions.step_up` and writes one audit line, `device_access.change`, with the switch and its new value.
The switches are kept in the server's database and survive a restart.

The operator keeps a ceiling in the configuration: `device_access.sync` and `device_access.remote`
([config.md](config.md#the-server-configuration)). Unset, or `true`, the administrator decides; `false` keeps the feature off
whatever the switch says, and the People dialog draws that switch `OFF` with a dashed outline and the line
`Turned off in the server configuration.` The ceiling never turns a feature on.

While a feature is off, every path of it answers exactly as a path that does not exist (404, the code `not_found`), before any
credential is read, so a probe learns nothing: `/sync/v1/` for the sync target, `/ws/device` for the remote sessions, and the
sign-in of devices under `/api/device/` while both are off. Turning a feature off takes effect with the next request: the next
run of an app's sync fails with `This feature is off on the server.` (and the set-up of the popover says `Sync is off on this
server.`), and every open device socket closes with code 1001. The devices stay signed in, their tokens stay valid, and the
sessions keep running in the workers until the idle limit or a restart ends them; turning the feature on again lets the apps attach to them. In the browser window the commands that list,
make, open or end a session are answered `Remote sessions are off on this server.` and never reach the worker; a pane of the
browser window that already shows a session stays until it is closed, and `Take over` and the loading of older lines still
work in it.

## The sync target

With `Sync target` on, the apps of a person can keep their encrypted sync files on this server instead of a WebDAV folder
([sync.md](sync.md#the-holzwave-server-as-the-target)). The server stores what the sync engine of each device already produces:
files encrypted with the person's passphrase, which the server never sees, and one small `meta.json` (at most 4 KiB, written
once) with the salt and the key check. It checks only the header of each file and its size, never opens one, and writes no path,
name or byte of a file to its log or its audit log; a request of the sync writes no audit line at all.

The four routes (`POST /sync/v1/ensure/{dir}`, `GET /sync/v1/list/{dir}`, `GET` and `PUT /sync/v1/blob/{path}`) take only a device
token, never a cookie, and every query is the device's own person's. A path is `meta.json`, `files/<name>` or `backups/<name>`
and nothing else. The bounds are per person: `sync.quota_mib` (64), `sync.max_files` (512), `sync.backups_keep` (20) and
`sync.max_blob_bytes` (8 MiB). A write past the space or the count is refused with 507 and the code `quota` and stores nothing, a
file over the size with 413 and `too_large` before its body is read past the bound, a lost race with 412. The files are rows of
`server.sqlite3` (written with `secure_delete`), are part of the backup of the volume, and are deleted with the person's account.
A server can serve an older valid file instead of the newest, and nothing in the format notices (no rollback protection), as with
a WebDAV server.

Tested with the real sync engine and client of two devices against the real server in one process
(`server/tests/sync_target.rs`, `server/tests/sync_client.rs`). Not run: a server on a real network or behind a real reverse
proxy, two real computers or phones, and a body that arrives slowly (the 120-second limit on reading one is in the code and not
in a test).

## Remote sessions

With `Remote sessions` on, a person's apps can keep terminal sessions running on the server. A **session** is a shell that the
server runs for the person in their own worker, as their own user id, outside every window of the person. It outlives every
client: an app that closes, loses its network or is put to sleep leaves it running, and the output made meanwhile is on its
screen and in its scrollback when an app attaches again.

A session ends when its program ends, when a person presses `End`, and when the person's worker ends: after
`workers.idle_timeout` (a day by default) with no socket of the person attached (a browser tab or an app counts), when the
account is disabled or deleted, and **when the server stops or restarts. A restart ends every session**, as it ends every pane;
the workers do not survive it. A session counts against `workers.max_panes` (64) together with the panes of the person's
browser window, and the socket of an app against `limits.sockets_per_person` (8) together with the page's sockets: the ninth
socket of a person is closed with 1013.

**One writer.** A session has one writer and any number of watchers; what a watcher types is dropped. The app that makes a
session writes to it. The writer keeps the right while it says it is alive (an attached app does so every 10 seconds); after
`remote.writer_timeout` (30 seconds) of silence the right is free. It never moves by itself: everyone sees `FREE` until
somebody presses `Take over`, which moves it and tells every attached app. A writer that detaches gives it up at once. A
session has one size, the size of the writer's pane; a watcher draws the same grid from the top-left corner.

**Attach.** An attach sends the screen and a tail of the scrollback in one snapshot, then every byte the program writes. The
tail is `remote.attach_tail_lines` (1000) unless the app asks for another; an app that scrolls to the top of what it holds asks
again with a longer one, at most `remote.history_max_lines` (10000) and never more than the session's own scrollback. A snapshot
over `remote.snapshot_max_bytes` (4 MiB) is refused whole and the session runs on. The snapshot carries the screen with its
styles, links, the cursor and the terminal modes; it does not carry pictures, the window title, the selection or the main screen
behind a full-screen program. An app that cannot keep up loses its stream and gets a fresh snapshot; the others are not slowed.

**The device socket.** An app reaches its sessions over `GET /ws/device` with its device token in the `Authorization` header of
the upgrade; a cookie is never accepted there and a device token never opens the page's socket. The socket carries text
requests (`list`, `new`, `attach`, `detach`, `resize`, `takeover`, `end`, `beat`), answered with an id and one fixed word on
failure, and binary frames with the bytes. It is bounded: 4 KiB for a request, 64 KiB of bytes in a frame, 32 requests waiting
for an answer and 16 sessions attached at once, with the rate, silence and violation limits of the `limits` section that the
page's socket has. A person reaches only their own sessions: another person's session answers `not_found` exactly as one that
never existed. The close codes: 4401 the token was revoked or ran out or the account ended (at once when it is announced, and
within `limits.session_recheck` otherwise), 1001 the feature was turned off or the server is stopping, 1013 too many sockets or
a write that made no progress, 1012 the worker ended, 1008 a flood of requests or too many broken rules, 1009 a message over
its size, 1003 a message of a kind the socket does not take. No terminal byte and no token reaches a log, the audit files or the database.

**The browser window.** The window in a browser lists the server it was loaded from as `This server` in the Connections view,
with `New session`, the person's sessions, `Attach` and `End`. A pane of the browser window that shows a session is one more
client of the same session in the person's worker, so one writer rule covers the browser and every app. Two things differ from
an app:

- All tabs of one person share the window, so they share that pane: its scroll position, selection and search, and one right to
  write. In an app each device draws the session itself, so those are its own.
- The pane has no socket of its own. It counts as alive while at least one browser tab of the person is open, so **a browser tab
  left open keeps the right to write until somebody presses `Take over`**. When the last tab closes, the right is free after the
  30 seconds of silence (a reload inside that time keeps it); closing the pane gives it up at once.

The worker renders a browser pane of a session with an engine of its own beside the session's, so such a session costs two
engines in the worker; that cost was not measured.

Tested against the real server in one process, with real shells on real pseudo terminals: the device socket and its bounds,
the writer rules, revocation, the switch and the shared socket cap (`server/tests/remote_sessions.rs`), an app's pane against a
real session including a cut of the network (`server/tests/remote_client.rs`, `server/tests/remote_host.rs`), the browser's
panes (`server/tests/session_local.rs`, `server/tests/session_web.rs`) and a real browser against the real server
(`app/tests/web/sessions.test.mjs`). **Never run:** a real phone or desktop against a real server over a real network, a real
reverse proxy in front of the device socket, two real people, Android with the hand-back from its system browser and its secret
vault, Android's Doze or the end of a background app, and a server restart under a real workload.

## The audit log

The server writes one JSON line per security event to `/data/server/audit/audit-YYYY-MM-DD.jsonl` (UTC days, mode 0600,
append-only): a sign-in and its failure and lock, a sign-out and an expired session, an account created, invited,
redeemed, disabled, enabled, deleted or given another role, a credential added or changed, a recovery code used or replaced,
the reset, unlock and lock of the secret store, a device approved (`device.approve`), denied (`device.deny`) or revoked
(`device.revoke`, with the number of the device; also when a device ends its own sign-in), and a change of a switch of the
device features (`device_access.change`, with the switch and its new value). The sync target and the remote sessions write no
line per request. A line holds the time, the action, the result, the actor and the target
as account numbers, the address and a few typed details. Nothing a person types can enter a line. The server does not rotate or
delete the files: move or remove the files of earlier days yourself, and copy them to where you keep logs.

## The data volume: back up and restore

`/data` is the one volume. It holds `server/server.sqlite3` (accounts, credentials, sessions, invitations and counters, and
the devices, their open requests, the two switches and the encrypted sync files of the people),
`server/audit/`, and `homes/<user id>/` for every person (files, saved passwords, uploads). The server key is not in it.

Back up the volume and the key file separately, and stop the server first so that the database is whole:

```sh
docker compose stop holzwave
docker run --rm -v <the data volume>:/data:ro -v "$PWD:/backup" debian:trixie-slim \
  tar --numeric-owner -czf /backup/holzwave-data.tar.gz -C /data .
docker compose start holzwave
```

To restore, stop the server, empty a new volume, unpack with `--numeric-owner` as root so that the owners 10001 and the
user ids of the people are kept, put the key file back where `HOLZWAVE_SERVER_KEY_FILE` points, and start the server with the
same `public_url`. A volume restored without its key, or with another one, cannot read the authenticator secrets of the people
who use a password and a code. These commands have not been run by the author on a second host; row SRV-E1 is that run.

## Updates

Build the new image from the new source, back up as above, then `docker compose up -d --build` or `helm upgrade`. An upgrade
stops the old server before it starts the new one, and a restart ends every pane and every remote session. Read the release
notes first; the guide for
a version states what changed in the files and the keys.

## Compression

`transport.compression` accepts only `off` in this build: the WebSocket never offers the `permessage-deflate` extension, and the value `deflate` is refused at start and by `check-config` with a sentence that says so. Compression is off because the measurement of the candidate library did not pass its rule (the library cannot meet the multi-offer handshake cases of the Autobahn suite, 18 cases unimplemented), although it saved 79 to 94 percent of the bytes and refused the decompression bombs within the memory bound.

A reverse proxy in front of the server may compress the page files; the WebSocket stays uncompressed.

## Limits of this build

- A server restart ends every pane. It ends every remote session too. The accounts, the layout, the valid sessions, the devices, the
  switches, the sync files and the settings stay; the commands that were running do not. A remote session also ends when the
  person's worker ends after `workers.idle_timeout` with no tab and no app attached.
- The device features (the sync target, remote sessions and the sign-in of apps) were never run on a real phone or a real
  desktop against a real server over a real network, with a real passkey or single sign-on approving a device, behind a real
  reverse proxy in front of the device socket, with two real people, on Android (with the hand-back from the system browser,
  the secret vault, Doze and the end of a background app), or across a server restart under a real workload. Everything was
  run against the real server in one process, over the loopback address, on the author's Mac.
- A device token is not bound to the device: whoever holds it can use it until it is revoked or has been unused for
  `devices.idle`. Approving a device can be phished (see [Device sign-in and devices](#device-sign-in-and-devices)).
- A browser tab left open keeps the right to write of a session shown in its window until somebody presses `Take over`, and
  all tabs of one person share that pane. A pane of the browser window that shows a session stays when `Remote sessions` is
  turned off, until it is closed.
- The 64 panes and the 8 sockets of a person are shared between the browser window, the remote sessions and the apps.
- One server container or pod on one volume: the store is SQLite, never run a second one. The chart has no replica value.
- One container serves one trust domain. People share the container's network and process view, so a person can see the
  processes of the others and reach the same network; `/tmp` is one directory for all of them (a person's own temporary files
  are in their home). Run a separate container for people who must be kept apart.
- A person's user id has no entry in the container's `passwd` file, so a shell may show a placeholder name in its prompt
  (bash prints `I have no name!`).
- The audit files are not rotated.
- The namespace needs the Pod Security level `baseline`, and `allowPrivilegeEscalation: false` stops the server from starting.
- A program's clipboard escape sequence does nothing in the browser, as on the desktop. Download works for images, video, audio
  and PDF files, and a refused download says why (secrets locked, file not found, not allowed to read it, another kind of file).
  Upload takes a file dropped on a shell pane, on a chat pane (the path goes into the message) and on a file browser (the
  file is selected when the list shows the upload folder). The refusal for a saved SSH or SFTP connection was not tried over a
  real SSH login.
- The dependency lock of the project holds two generations of the `rsa` crate: one comes with the SSH client and one with the OpenID
  Connect library.
- Linux only: the server and its isolation tests run in a Linux container; nothing ran on Windows or iOS, and the isolation tests
  run in a container only.

## When the server refuses to start

`holzwave-server check-config` prints `configuration ok` or one of these sentences, and `serve` prints the same sentence and exits
with status 1 before any socket exists. A name in angle brackets stands for the value in your file; a path in a sentence about a key stands
for the file you named.

| The sentence | What to do |
|---|---|
| ``HOLZWAVE_SERVER_KEY_FILE is not set; create a key file with `holzwave-server keygen --out FILE` and name it in that variable`` | Make the key ([the server key](#the-server-key)) and set the variable to its path. |
| `the configuration file <path> cannot be read (entity not found); name the file in HOLZWAVE_SERVER_CONFIG` | Mount the file where the server looks (`/etc/holzwave/server.json`) or set `HOLZWAVE_SERVER_CONFIG`. |
| `public_url is not set; name the address people type in the browser, such as https://holzwave.example.org` | Set `public_url`. |
| `public_url "<address>" is not usable: it must start with https:// because passkeys and sign-in cookies need TLS` | Use an `https://` address. The same sentence form says the address must not have a path, a query, a fragment or user information. |
| `public_url "<address>" has an IP address as its host, but passkeys need a name; use a host name or turn auth.passkey off` | Use a host name, or switch passkeys off. |
| `listen "<value>" is not an address and port such as 0.0.0.0:8443` | Write the address and the port. |
| `the configuration has no tls section; choose a mode: "files", "acme" or "proxy" (the server has no plain-HTTP mode)` | Add a `tls` section. |
| `tls mode "files" needs cert_file and key_file` | Name both files. |
| `the certificate file <path> cannot be read; check tls.cert_file` | Mount the certificate where `tls.cert_file` says; the server user (10001) must be able to read it. |
| `the certificate key file <path> cannot be read; check tls.key_file` | Mount the key where `tls.key_file` says; the server user (10001) must be able to read it. |
| `tls mode "acme" needs tls.terms_agreed set to true: by setting it you, the operator, agree to the terms of service of the certificate authority at https://acme-v02.api.letsencrypt.org/directory; the server never agrees for you` | Read the terms of the authority, and set `tls.terms_agreed` to `true` yourself. |
| `tls mode "acme" needs at least one name in tls.domains` | List every name of the certificate. |
| `tls mode "acme" needs tls.contact, an address such as mailto:admin@example.org` | Set `tls.contact`. |
| `the host "<host>" of public_url is not in tls.domains; the certificate must cover the address people type` | Add the host of `public_url` to `tls.domains`. |
| `tls mode "proxy" needs tls.trusted_proxies, the addresses of the proxies allowed to connect; the list is empty` | List the addresses or networks of your proxy. |
| `tls.trusted_proxies entry "<entry>" is not an address or a network such as 10.0.0.1 or 10.0.0.0/8` | Write an IP address or a network with a prefix. |
| `tls.trusted_proxies entry "<entry>" would trust every address; name the networks of the proxies` | Name the proxies; `0.0.0.0/0` and `::/0` are refused. |
| `no sign-in method is on; turn on auth.passkey or auth.password_totp, or add a provider to auth.oidc (the server never runs without sign-in)` | Switch a method on. |
| `auth.oidc "<id>": the client secret file <path> cannot be read; check client_secret_file` | Mount the file with the client secret where `client_secret_file` says. |
| `auth.oidc "<id>": issuer must be an https address` | Use the `https://` issuer address of the provider. |
| `workers.uid_min 0 is below 1000, which could name a system account; use a range of 1000 or more` | Use a range of 1000 or more; the default is 20000 to 29999. |
| `the worker range 10000..10002 contains the user id the server itself runs as (10001); choose a range without it` | Move the range away from the user id of the server (10001 in the image). |
| `limits.max_connections is 0 but must be between 1 and 65536` | The shape is the same for every limit, session time and worker setting: the key, your value and both bounds. Use a value inside them ([config.md](config.md#the-server-configuration) lists the bounds). |
| `sessions.idle 2d is longer than sessions.absolute 1d` | Make `sessions.idle` the shorter of the two. |
| `upload.dir ../elsewhere must be a relative path inside the home, without ".." or a leading slash` | Name a folder inside the home. |
| `transport.compression is deflate, but compression is not available in this build; it must be off` | Set `transport.compression` to `off`, or delete the key. |
| `the server key file <path> does not exist; check the path or create the file` | Check `HOLZWAVE_SERVER_KEY_FILE`, or restore the key from your backup. |
| `the server key file <path> has mode 0644, which is too open; run chmod 0600 on it (a group may read it, nobody else may write or read it)` | Run `chmod 0600` on the file. |
| `the server key file <path> is a link and links are refused; name the file itself` | Name the file, not a link to it. |
| ``the server key file <path> is not a key made by `holzwave-server keygen`; make a new key file with that command`` | Restore the right file from your backup, or make a new key (a new key cannot read what the old one sealed). |

Two more refusals come from the files around the server and not from the configuration. A configuration file with a key the
server does not know is refused with `the configuration file <path> is not valid:` and the parser's words, which name the keys the
section does accept. And if the spawn helper does not hold its file capabilities, it prints `the capabilities CAP_SETUID,
CAP_SETGID, CAP_CHOWN, CAP_KILL are not permitted to the helper: the container must add them (SETUID, SETGID, CHOWN, KILL) and the
helper file must carry the file capabilities cap_setuid,cap_setgid,cap_chown,cap_kill=p; it never runs with a reduced set`, and
the server exits: add the four capabilities to the container, and remove `no-new-privileges` or `allowPrivilegeEscalation: false`.
