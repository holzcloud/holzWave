# holzWave privacy statement

Last updated: 2026-11-06

This statement says what holzWave does with your data, and it says only what the repository already shows or tests. Where a
claim is a test, the sentence says so. Where it is best effort, the sentence says that too. The long form, with the file
names, is the Privacy section of [docs/config.md](docs/config.md#privacy).

holzWave is a beta (`0.0.1-beta2` and every `0.0.x` after it). Nothing in this statement has been looked at by a store, and
no store has accepted it.

## Who publishes holzWave

holzWave is published by **holzcloud.ch**. The source code is at <https://github.com/holzcloud/holzWave>.

## What holzWave does not do

- It has no account with the developer, and it does not ask you to sign up for anything.
- It has no telemetry, no crash report and no usage counter.
- It has no update check. It does not ask any server whether a newer version exists, and no build shows a notice that one
  does. You update through the channel you installed it from.
- Opening the app, a chat, the providers dialog or a pane makes no request.

That is a test and not only a promise: `core/tests/project_policy.rs` fails when a dependency that could phone home appears,
when first-party code names a network API or an absolute address, when the page policy allows a remote origin, or when an
updater or a continuous-integration workflow appears. Two named exceptions exist in that test, both for the networking crate
`reqwest`: the Tauri 3 alpha links it for a development-server proxy that a desktop build never starts, and holzWave links it
for the AI chat client, which opens no connection when it is built.

## When holzWave connects

holzWave opens a network connection only when you ask for one:

1. **An SSH connection you start.** It goes to the host you named, with the login you chose.
2. **The sync you set up.** It goes to the Nextcloud or WebDAV folder you named. What is sent is encrypted on your computer
   first (XChaCha20-Poly1305, with a key made by Argon2id from your passphrase), so the server holds encrypted blobs. The
   names of the synced files, such as the name of a theme or a widget, are not hidden from that server.
3. **A link you click.** holzWave hands an `http`, `https` or `mailto` link to your system's own opener (`open`, `xdg-open`
   or `explorer`) and does not fetch it itself. A link whose text names another place asks first.
4. **A provider you set up.** If you set up an AI provider, the message you send goes to the address of that provider.
   Nothing goes to a provider that you did not set up.

A name lookup for a provider that is given by a host name happens only when you press a control that needs its destination.

The tools holzWave starts on your computer when you use the integrations (`kubectl`, `talosctl`, `az` and GAM7) make their own
connections, and what they send is their own business. holzWave starts Azure CLI with its telemetry switched off.

## What is stored on your computer, and where

holzWave keeps its files in its app data folder, named `ch.holzcloud.holzwave`, on your computer. They hold your settings,
themes, layout, saved connections without secrets, known SSH host keys, the AI provider profiles without keys, your AI chats,
your command history, your widgets and integration profiles. The full list, with what each file holds and which ones are
synced, is in [docs/config.md](docs/config.md#where-the-files-live).

- **Secrets are in the operating system keychain only:** passwords and passphrases of connections, the sync's app password and
  passphrase, the AI provider keys, and the secret variables of an integration profile. They are never written to a file and
  never synced. A provider key is bound to the address it was saved for and is never shown again.
- **Chats and history stay on the computer.** They are not synced. A chat file can be read by you only. Terminal text that you
  shared with a chat is stored as a marker unless you turn on `ai_persist_context`.
- **The command history** leaves out a command that starts with a space and one that contains words such as `password` or
  `token`. That filter is best effort and no guarantee: a secret typed in another form is kept, and the history dialog can
  forget it.

## The AI chat

What leaves your computer in an AI chat is the text you type, the terminal context of the panes you shared, and the results
of the tool calls you allowed on their own cards, and nothing else. A pane is not shared until you share it, every send shows
exactly what goes out, and every tool action asks first. A send that nobody approved leaves nothing: `core/tests/ai_egress.rs`
counts the connections of an idle service (zero) and searches what a provider receives for planted secrets.

Secrets of known shapes are replaced by a marker before the preview. That is best effort and no guarantee: a secret in another
form can still be sent. holzWave asks OpenAI not to store a request and OpenRouter not to collect it, and it cannot promise
more: what a provider keeps is that provider's own policy. Read it before you use the provider. With `ai_local_only` set,
holzWave refuses every destination that leaves your computer, and the Apple on-device model answers on the Mac itself.

## The server build

The server build runs holzWave in a browser from a container that you run. The person who runs the server holds its accounts,
sessions and audit log, on that server's own volume, and holzcloud.ch holds none of it. What that server records is described
in the [Server guide](docs/server.md). This statement does not speak for the operator of a server.

## Which builds this covers

This statement covers the desktop builds and the server build as they are today. The Android app does not exist yet, so this
statement makes no claim about it. It will get its own sentences here when it exists.

A store that distributes holzWave (Flathub, the Microsoft Store, F-Droid, Obtainium's source on GitHub) has its own statement
about the data it collects about you. This one does not cover that.

## Children

holzWave is a developer tool. It is not aimed at children.

## Questions and reports

Ask a question or report a problem as an issue at <https://github.com/holzcloud/holzWave/issues>. That is the only support
channel. Please do not put a password, a key or a private address into an issue.
