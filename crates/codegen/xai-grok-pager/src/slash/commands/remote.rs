//! `/remote` — enable Tailscale remote control for the current session.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Enable remote control via Tailscale (URL + QR).
pub struct RemoteCommand;

impl SlashCommand for RemoteCommand {
    fn name(&self) -> &str {
        "remote"
    }

    fn aliases(&self) -> &[&str] {
        &["rc", "remote-control"]
    }

    fn description(&self) -> &str {
        "Control this session from your phone via Tailscale"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/remote"
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error(
                "No active session. Start a session first, then run /remote.".to_string(),
            );
        }
        CommandResult::Action(Action::StartRemoteControl)
    }
}
