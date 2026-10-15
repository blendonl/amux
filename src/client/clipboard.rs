use std::process::Stdio;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const OSC52: &[u8] = b"\x1b]52;c;";
const BEL: u8 = 0x07;

pub fn write_osc52(text: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(OSC52);
    out.extend_from_slice(STANDARD.encode(text).as_bytes());
    out.push(BEL);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub argv: Vec<String>,
    pub text: String,
}

impl ClipboardCommand {
    pub async fn run(self) -> Result<(), String> {
        let Some((program, args)) = self.argv.split_first() else {
            return Ok(());
        };
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not run the clipboard command {program}: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(self.text.as_bytes()).await;
        }
        match child.wait().await {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("the clipboard command {program} failed: {status}")),
            Err(error) => Err(format!(
                "could not wait for the clipboard command {program}: {error}"
            )),
        }
    }
}
