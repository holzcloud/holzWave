//! Native integrations: the person's own command-line tools (Kubernetes, Talos, Azure, GAM) as blocks and launches.
//!
//! The design in one paragraph: every tool is a compiled-in typed description (a spec with a closed list of verbs), run
//! by one generic engine as an argument list and never through a shell. Every name that can reach an argument (a context,
//! a namespace, a pod, a node, a subscription, a section) comes from a cluster, an account, a directory or a synced file,
//! so it is another party's text: [`args`] turns it into a type that cannot be built any other way than by validating it.
//!
//! Module map:
//!
//! - [`args`]: the validators for names, the equals-form flag builder and the fixed refusal sentences.
//! - [`detect`]: finding a tool on this computer.
//! - [`run`]: running a tool with a scrubbed environment and bounded output.
//! - [`view`]: the view model the output becomes and its cleaning.
//! - [`profiles`]: the profile files.
//! - [`context`]: the selection per pane and its application to a run.
//! - [`ticket`]: prepare and confirm for verbs that change something.
//! - [`service`]: the service the app shell holds.
//! - [`kube`], [`talos`], [`azure`], [`gam`]: one module per tool, with its argument lists and parsers.

pub mod args;
pub mod azure;
pub mod context;
pub mod detect;
pub mod gam;
pub mod kube;
pub mod profiles;
pub mod run;
pub mod service;
pub mod talos;
pub mod ticket;
pub mod view;
