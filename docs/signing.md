# What is signed, and how to check it

> [!IMPORTANT]
> **Status.** The signing scripts exist and were tested with stand-ins, and once with a real minisign and a throwaway
> key in a container. **No release has been signed yet**: the real key does not exist, so
> `packaging/minisign/holzwave.pub` is not in the repository, and every release made so far (including `0.0.1-beta2`)
> has no signature file. The page says what a signed Linux release will carry, how to check it, and what is not signed at
> all. The list of what has not been tried is at the end ([Untested](#untested)).

## What is signed

| Platform and file | Signed? | What there is to check |
|---|---|---|
| Linux `.deb`, `.rpm` and `.AppImage` | **Can be, by minisign; none has been yet.** A signed release carries `<file>.minisig` for each of the three, a `SHA256SUMS` of the three files and `SHA256SUMS.minisig` | the minisign signature, against the public key `packaging/minisign/holzwave.pub` |
| Windows installer (`holzWave_<version>_x64-setup.exe`) | **No.** It is not signed with Authenticode or in any other way | only the `.sha256` file beside it. It says the download is whole; it is not proof of who made the file |
| macOS app and DMG | **Ad hoc only, not notarised.** The signature is the one the build makes for itself; it names no developer and Gatekeeper does not accept it as one. The Homebrew cask removes the quarantine attribute so the app opens | nothing a user can check against a publisher |
| Flatpak | **No bundle is built, so nothing is signed.** See [flatpak.md](flatpak.md) | not applicable |
| A package repository (apt, rpm) | **There is none** | not applicable |

A signature on a Linux package says that the file was made by whoever holds the secret key that belongs to the public key. It
does not say the code is safe, and it does not say the key is the author's if the key came from a place an attacker also
controls (see [Check a Linux package](#check-a-linux-package)).

## Why Windows and macOS are not signed

**Windows.** The route this project looked at is Microsoft's Azure Artifact Signing (earlier called Trusted Signing). Its set-up
page says: "Public Trust certificates are available to organizations in the United States, Canada, the
European Union, the United Kingdom, Australia, New Zealand, Japan, South Korea, Singapore, Switzerland, Norway, and Israel.
Individual developers must be located in the United States or Canada." (<https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart>,
last updated 2026-05-21, read on 2026-11-03.) The author is an individual outside the United States and Canada and has no
organisation behind the project, so that route is closed. The certificate of a public-trust profile carries the name of a
validated legal entity or person, so a certificate cannot be issued without one of the two. Certificates from other authorities
were not looked into. The installer therefore stays unsigned, and Windows SmartScreen warns on the first run (see
[build-windows.md](build-windows.md)). Nothing here says that this will change.

**macOS.** A Developer ID certificate and notarisation need an Apple developer account, and the author has none. The app is signed
ad hoc, which the system needs to run it at all on Apple Silicon, and nothing more. Nothing here says that this will change.

## Check a Linux package

This works only for a release that carries signature files. At the time of writing there is none: if the release page has no
`.minisig` file, there is nothing to check, and the packages are as unsigned as the Windows installer.

1. Install minisign from your package manager (Ubuntu packages it as `minisign`; the project is
   <https://github.com/jedisct1/minisign>).
2. Download the package, its `.minisig`, `SHA256SUMS` and `SHA256SUMS.minisig` from the release, and take `holzwave.pub` from
   the repository at the release's tag (`packaging/minisign/holzwave.pub`).
3. Check the file and then the list, and check the file against the list:

```sh
minisign -Vm holzWave_<version>_amd64.deb -p holzwave.pub
minisign -Vm SHA256SUMS -p holzwave.pub
sha256sum -c SHA256SUMS --ignore-missing
```

Each `minisign` command prints `Signature and comment signature verified` when it holds, and exits with a non-zero status when it
does not. Or, with this repository checked out, check everything in the directory at once:

```sh
sh scripts/verify-release.sh <directory>
```

The script checks the signature of `SHA256SUMS`, then each file that list names against its signature and its hash, and fails on
a signature that the list does not cover. It reads files and changes nothing.

The public key and the packages come from the same place (this repository and its release page). An attacker who could replace
the packages could replace the key too. A user who needs more than that compares the key's id (the text after
`minisign public key` in the first line of the `.pub` file) by another path, for example the same file at an older tag or a
copy the author published elsewhere. No such second path exists yet.

## For the owner: sign a release

Everything is local. There is no CI and nothing is uploaded by a script.

1. Install minisign (`brew install minisign` on a Mac) and create the key pair **outside** the repository. The command asks for
   the password twice; the public half goes into the repository, the secret half stays in `$HOME/.minisign`:

   ```sh
   mkdir -p -m 700 "$HOME/.minisign"
   minisign -G -p packaging/minisign/holzwave.pub -s "$HOME/.minisign/holzwave.key"
   chmod 600 "$HOME/.minisign/holzwave.key"
   ```

   Commit `packaging/minisign/holzwave.pub` and nothing else of it. A test refuses a key file or a signature in the repository.
2. Put the password in the operating system's secret store, where the signing script looks for it and nowhere else. On macOS
   the command prompts for the password, so it is on no command line and in no file:

   ```sh
   security add-generic-password -s holzwave-minisign -a $USER -w
   ```

   On Linux the equivalent is `secret-tool store --label holzWave service holzwave-minisign`.
3. Build the packages: `sh scripts/release-linux-container.sh <version>` (see [build-linux.md](build-linux.md)).
4. Sign: `sh scripts/sign-linux.sh <version>`. It signs the three packages, writes and signs `SHA256SUMS`, checks all four
   signatures against the public key, and prints the `gh release upload` command for the eight files. It refuses to run, and
   touches nothing, if the key is missing or lies inside the repository, if the key file can be read by group or others, if the public key is
   missing, or if the password cannot be read from the secret store.
5. Check it as a user would: `sh scripts/verify-release.sh target/release-cross`.
6. Upload by hand with the command the script printed.
7. Back up the key file and its password somewhere that is not this computer. Without them no later release can be signed with
   this key.

**If the key is lost or exposed.** Make a new key pair, put its public half in the repository in place of the old one, and say in
the release notes that the key changed and which earlier releases carry the old key. A release that was signed with an exposed key
can no longer be trusted by its signature alone; say so in the notes of that release.

## Not done

- **Windows signing** is not done: see the reason above.
- **The Flatpak bundle and its signature** are not done; no bundle is built.
- **macOS Developer ID and notarisation** are not done: see the reason above.
- **A signed apt or rpm repository** is not done; there is no repository.
- **An automatic release build** is not done; releases are built and signed by the owner on the owner's machines.

## Untested

This is what has not been tried. It is listed here so that no reader assumes it was.

- No release has been signed with the real key, because the key has not been created. The first real signing has not been done.
- No user has checked a signature on a real distribution; the commands above were run only in a container of the author's, with a
  throwaway key.
- The signing scripts were tested with stand-ins for the secret store and for minisign. The real round trip (a key pair with a
  password, signing, verifying, a changed byte, a wrong key) ran once with minisign 0.12 and a throwaway key in a Linux container.
  It did not run on the owner's Mac, and the real macOS keychain lookup (`security find-generic-password`) and the real
  `secret-tool` lookup have not been called by the tests.
- The packages that the container build makes have not been signed.
- No Azure Artifact Signing run was made, and no Windows certificate of any kind was tried.
- How Windows SmartScreen treats a signed installer has not been seen, because no signed installer exists.
- macOS notarisation has not been tried, and Gatekeeper with a Developer ID has not been seen.
