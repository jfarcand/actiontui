// SPDX-License-Identifier: Apache-2.0
//! Desktop notifications + sound on CI status transitions (macOS-focused,
//! degrades to a terminal bell elsewhere).

use crate::state::Transition;

/// macOS system sounds in /System/Library/Sounds.
const SOUND_FAILURE: &str = "Basso";
const SOUND_RECOVERY: &str = "Glass";

/// Fire a sample failure + recovery notification so the channel can be tested
/// on demand (the `t` key in watch mode, or `--test-notify`).
pub fn test(sound: bool) {
    let demo = [
        Transition::Failure {
            repo: "actiontui/demo".into(),
            workflow: "Sample workflow".into(),
        },
        Transition::Recovery {
            repo: "actiontui/demo".into(),
            workflow: "Sample workflow".into(),
        },
    ];
    announce(&demo, "main", sound);
}

pub fn announce(transitions: &[Transition], branch: &str, sound: bool) {
    for t in transitions {
        match t {
            Transition::Failure { repo, workflow } => fire(
                &format!("GitHub Actions: {repo}"),
                &format!("{workflow} is now failing on {branch}"),
                SOUND_FAILURE,
                sound,
            ),
            Transition::Recovery { repo, workflow } => fire(
                &format!("GitHub Actions: {repo}"),
                &format!("{workflow} is green again on {branch}"),
                SOUND_RECOVERY,
                sound,
            ),
        }
    }
}

fn fire(title: &str, body: &str, sound_name: &str, sound: bool) {
    // Terminal bell as a universal fallback.
    print!("\x07");

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification {body} with title {title}{snd}",
            body = applescript_quote(body),
            title = applescript_quote(title),
            snd = if sound {
                format!(" sound name {}", applescript_quote(sound_name))
            } else {
                String::new()
            },
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();

        if sound {
            // afplay gives a fuller sound than the notification chirp.
            let path = format!("/System/Library/Sounds/{sound_name}.aiff");
            let _ = std::process::Command::new("afplay").arg(path).spawn();
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, body, sound_name, sound);
    }
}

/// Quote a string as an `AppleScript` string literal.
#[cfg(target_os = "macos")]
fn applescript_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
