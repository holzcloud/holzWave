# Flathub submission: the owner's checklist

> **Status: not submitted. Nothing on this page is a submission.** It lists, in order, what the owner does by hand before and
> during a Flathub submission, and why. **The owner writes the Flathub manifest, the metainfo and the pull request by hand.**
> An agent must not open or automate the submission, and the files under `packaging/flatpak/` are references, not a
> submission. This page contains no text for the pull request, and nothing here is meant to be pasted into one.

Why by hand: Flathub's requirements say that its manifests must not contain AI-generated or AI-assisted content, that AI
tools or agents must not open or automate submission pull requests, and that a submitter must disclose AI-generated work
(<https://docs.flathub.org/docs/for-app-authors/requirements> and <https://docs.flathub.org/docs/for-app-authors/submission>,
read on 2026-11-05). holzWave is built with AI help, and the README says which parts and how much (the section `Built with AI
help` of [README.md](../README.md)). The manifest, the AppStream file and the page [flatpak.md](flatpak.md) in this repository
were written with that help, so they are material for the owner to read and to check, not text to copy. The three files under
`packaging/flatpak/generated/` are unmodified output of a tool (see [flatpak.md](flatpak.md)); whether Flathub accepts output
of a generator script that was itself written with AI help is a question for the maintainers, not settled.

## The steps, in order

1. **Run the Flatpak rows on a Linux machine.** Build with `sh scripts/build-flatpak.sh`, then work through row L10 of Part 4
   and rows FPK1 to FPK7 of Part 12 of [test-plan.md](test-plan.md), and write each result into its row. Until then nothing
   about the Flatpak has been run on a desktop, and the texts that go to Flathub must say what the rows say, not what the
   repository hopes. If a row fails, that is a task of its own; do not submit around it.
2. **Take real screenshots.** Screenshots of the running Flatpak, taken by the owner, from the build that the release tag
   points at. Flathub requires at least one for a graphical app. The pictures under `docs/design/` are renders of the design
   page and are not screenshots of the running app. The AppStream file in the repository has no screenshots on purpose.
3. **Cut the release, and set the tag and the screenshot addresses.** The submission points at a release by a `git` source of
   this repository with the tag and its full commit; the local recipe in the manifest reads the working tree instead and is
   replaced. The screenshot addresses are written once the tag exists and resolve at that tag. No release tag exists for this
   yet, so none is written anywhere in the repository.
4. **Prove the domain.** The application id `ch.holzcloud.holzwave` is tied to `holzcloud.ch`. After the owner logs in to
   Flathub with GitHub, Flathub shows a token for the id; the owner places it in the file
   `/.well-known/org.flathub.VerifiedApps.txt` on the domain (a DNS TXT record is the other way,
   <https://docs.flathub.org/docs/for-app-authors/verification>). Both are free. This is the owner's step: it needs the owner's
   login and the owner's domain, and an agent cannot do it. The developer in the metainfo is `holzcloud.ch`, with the id
   `ch.holzcloud`.
5. **Decide the beta flow.** The documented way is a `beta` branch of the app's Flathub repository, which builds into the
   `flathub-beta` remote (<https://docs.flathub.org/docs/for-app-authors/maintenance>). A new app starts as a pull request, so
   it appears on the stable remote first; whether a brand-new app may live only on the beta remote is not stated anywhere the
   author could read. If the owner asks the maintainers, write down who answered and when, below.
6. **Fork and open the pull request.** Fork `flathub/flathub` (without the option that copies only the master branch), clone the
   fork at the branch `new-pr`, create a branch of your own from it, add the manifest and the files Flathub's page asks for,
   and open the pull request **against `new-pr`**, never against `master`. Keep Flathub's pull request template and fill it in
   yourself. Flathub's submission page is the source for the exact steps; it may have changed.
7. **Explain each permission, in your own words.** The reviewers read the permissions separately from the build. Topics, not
   sentences: why a terminal needs the host (a sandbox sees none of the person's tools; the setting is off by default and never
   synced; no integration runs in a Flatpak); why the home folder; why the network (SSH, sync and the AI chat only when the
   person starts them, nothing at launch, no telemetry); why the SSH agent socket; why the Secret Service name; and what is
   untested (the rows above). The table in [flatpak.md](flatpak.md) lists the same nine permissions and their cost; the
   explanation in the pull request is the owner's.
8. **Read the linter's warning for the host permission.** Flathub's linter has an entry for the permission to talk to the
   Flatpak service (`finish-args-flatpak-spawn-access`). As far as the author knows it is a warning and not an error, and no
   exception is needed for the plain name; the reviewers decide whether they accept it. Run the linter on the manifest and
   the metainfo once, as Flathub's submission page asks, and read every message.
9. **Disclose the AI use.** Say in the pull request, in the owner's own words, that holzWave is built with AI help, which parts,
   and about how much. The README section named above has the facts and the numbers; the disclosure itself is the owner's text.
10. **Answer the reviewers.** They are volunteers, and there is no deadline. Answer every question, change the pull request
    instead of closing it, and start the test build only when the comments are resolved (Flathub's page says how).
11. **Know the fallbacks before you start** (next section), because the host permission is the one the reviewers are most
    likely to ask about.

## The three designs, in the order they are tried

1. **Design A (the current one): the permission is shipped, a setting decides.** The manifest carries
   `--talk-name=org.freedesktop.Flatpak`; panes start in the sandbox by default; the person can switch `Host shell` on in
   Settings. What it costs the person: the sandbox does not protect the rest of their system from this app, whatever the setting
   says. What it gives: a terminal that is useful when the person asks.
2. **Design B: no permission, a documented override.** The manifest ships without the permission, and the person grants it
   with a Flatpak command that the documentation names (`flatpak override --user --talk-name=org.freedesktop.Flatpak
   ch.holzcloud.holzwave`; the exact form is not run). What it costs the person: a step outside the app before the setting
   works, and a sandbox-only terminal until then, which a reviewer may call not functional. The setting would then say how to
   get the permission instead of only reading `not available`; that needs a design change first.
3. **Design C: the setting only outside Flathub.** The Flathub build has no permission and no setting; the setting lives in a
   build that is not on Flathub (a Flatpak file from the project's own releases). What it costs the person: the Flathub
   package is a sandbox-only terminal, and the useful one is a second install with no Flathub updates.

Each design changes the manifest, the metainfo, [flatpak.md](flatpak.md) and the tests that pin the permission list. If none is
accepted, the Flatpak is moved out of 0.0.1 ([flatpak.md](flatpak.md), the last section).

## What is known and what is not

- **Known (read on 2026-11-05):** five terminals on Flathub ship the permission to talk to the Flatpak service today: Konsole,
  WezTerm, BlackBox, Ptyxis and Contour. None of them offers it as a setting; they ship it always.
- **Not known:** nobody has asked the maintainers whether a listing that starts as a beta is accepted, whether a Zig toolchain
  archive as a build source is accepted, whether registry-hosted native npm packages (the TypeScript compiler, esbuild and the
  canvas library ship binaries for every platform) are accepted, or whether generated source files made by a script written
  with AI help are accepted. Maturity is the reviewers' call: Flathub looks for a meaningful development history, real-world
  use and a commitment to keep maintaining the app, and holzWave is a single-maintainer beta.
- **Maintainers' replies:** none are recorded. If the owner asks and writes a reply here, it is an opinion of the person who
  answered, with their name and the date, and not a rule of Flathub.

## After acceptance

Flathub builds the app on its own servers for x86_64 and aarch64, and updates reach people through Flathub. Then the owner
changes the README and [distribution.md](distribution.md) from `not submitted` to what is true, and records the date and the
channel's real address. Those pages say `not submitted` until that happens.
