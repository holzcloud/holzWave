#![forbid(unsafe_code)]
//! The application logic of holzWave without a window shell: the desktop app and the server both run it.

// The command table spells a type by the path the desktop's wrappers used (`holzwave_host::sync::SyncView`), because the
// recorded surface of the desktop holds that text. This line lets the same text resolve inside the crate as well.
extern crate self as holzwave_host;

pub mod ai_ask;
pub mod ai_device;
pub mod ai_exec;
pub mod ai_host;
pub mod ai_tools;
pub mod ai_write;
pub mod commands;
pub mod handover;
pub mod host_shell;
pub mod integrate_host;
pub mod keystore_wire;
pub mod local_network;
pub mod locked;
pub mod login_prompt;
pub mod ranges;
pub mod seams;
pub mod state;
pub mod sync;
pub mod windows;
pub mod wsh_tunnel;

/// The app's side of `wsh`.
pub struct IpcHandler(pub std::sync::Arc<state::AppCore>);

impl holzwave_ipc::Handler for IpcHandler {
    fn handle(&self, pane: Option<u64>, cmd: holzwave_ipc::Cmd) -> Result<String, String> {
        self.0.ipc_command(pane, cmd)
    }

    /// `wsh ai` is a request that waits for the person in the window and answers as a stream; every other command is
    /// answered as one text, as the trait's default does.
    fn handle_stream(
        &self,
        pane: Option<u64>,
        cmd: holzwave_ipc::Cmd,
        input: String,
        gone: &holzwave_ipc::Gone,
        out: &mut dyn holzwave_ipc::Sink,
    ) -> Result<(), String> {
        match cmd {
            holzwave_ipc::Cmd::Ai { prompt, .. } => self.0.ai_ask(pane, prompt, input, gone, out),
            other => {
                let answer = self.handle(pane, other)?;
                if !answer.is_empty() {
                    out.text(&answer).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
        }
    }
}
