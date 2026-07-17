//! `/remote` — enable Tailscale remote control for the current session.
//!
//! `/remote`        enable / re-show QR
//! `/remote stop`   disconnect this session
//! `/remote status` show status (alias of bare /remote when already on)

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

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("stop | status")
    }

    fn usage(&self) -> &str {
        "/remote [stop]"
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error(
                "No active session. Start a session first, then run /remote.".to_string(),
            );
        }
        let sub = args.trim().to_ascii_lowercase();
        match sub.as_str() {
            "" | "status" | "show" | "qr" => CommandResult::Action(Action::StartRemoteControl),
            "stop" | "off" | "disconnect" | "disable" => {
                CommandResult::Action(Action::StopRemoteControl)
            }
            other => CommandResult::Error(format!(
                "Unknown /remote argument `{other}`. Use /remote or /remote stop."
            )),
        }
    }
}
