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

It is not a service for strangers. One container serves one trust domain: people inside it share the network and the process
view of the container. It is not a cluster: one server runs on one volume. It has no mobile layout of the page, no shared
panes between people, no installable web app, no long-lived token for scripts and no operator for Kubernetes. It runs in the
container this guide describes; the binary can run elsewhere, but the isolation of people from each other is the
container's, and this guide says nothing about running it outside one.

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
  and the proxy must pass the WebSocket upgrade for `/ws` and keep it open. Row SRV-R1 is a real proxy.

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
lift a lock. A person has at most eight sockets, 64 panes (the default), two uploads and eight downloads at a time. Every size,
rate and time of the server has a name, a default and a bound in the `limits` section of [config.md](config.md#the-server-configuration);
the server refuses a value outside its bound at start.

## Memory for pictures

A program in a pane can send pictures (the Kitty graphics protocol), and the terminal keeps them for as long as the screen shows
them. The key `workers.max_image_mib` is the most MiB of pictures that one pane keeps, on each of its two screens: the normal
one and the one a full-screen program switches to. The default is 64, the range is 0 to 4096, and `0` turns pictures off on
this server. The cap wins over the person's own setting `image_memory_mib`: a person who allows 512 MiB gets the cap, and a
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

## The audit log

The server writes one JSON line per security event to `/data/server/audit/audit-YYYY-MM-DD.jsonl` (UTC days, mode 0600,
append-only): a sign-in and its failure and lock, a sign-out and an expired session, an account created, invited,
redeemed, disabled, enabled, deleted or given another role, a credential added or changed, a recovery code used or replaced,
and the reset, unlock and lock of the secret store. A line holds the time, the action, the result, the actor and the target
as account numbers, the address and a few typed details. Nothing a person types can enter a line. The server does not rotate or
delete the files: move or remove the files of earlier days yourself, and copy them to where you keep logs.

## The data volume: back up and restore

`/data` is the one volume. It holds `server/server.sqlite3` (accounts, credentials, sessions, invitations and counters),
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
stops the old server before it starts the new one, and a restart ends every pane. Read the release notes first; the guide for
a version states what changed in the files and the keys.

## Compression

`transport.compression` accepts only `off` in this build: the WebSocket never offers the `permessage-deflate` extension, and the value `deflate` is refused at start and by `check-config` with a sentence that says so. Compression is off because the measurement of the candidate library did not pass its rule (the library cannot meet the multi-offer handshake cases of the Autobahn suite, 18 cases unimplemented), although it saved 79 to 94 percent of the bytes and refused the decompression bombs within the memory bound.

A reverse proxy in front of the server may compress the page files; the WebSocket stays uncompressed.

## Limits of this build

- A server restart ends every pane. The accounts, the layout, the valid sessions and the settings stay; the commands that were
  running do not.
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
