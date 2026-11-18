//! Things gridwatch can do to a machine instead of only watching it: start, stop and
//! restart a docker container, and end a process. Both are switched off until the user
//! allows them in the settings.

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
}

impl ContainerAction {
    /// The docker subcommand.
    pub fn verb(self) -> &'static str {
        match self {
            ContainerAction::Start => "start",
            ContainerAction::Stop => "stop",
            ContainerAction::Restart => "restart",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContainerAction::Start => "Start",
            ContainerAction::Stop => "Stop",
            ContainerAction::Restart => "Restart",
        }
    }

    fn done(self) -> &'static str {
        match self {
            ContainerAction::Start => "Started",
            ContainerAction::Stop => "Stopped",
            ContainerAction::Restart => "Restarted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Container {
        action: ContainerAction,
        name: String,
    },
    /// `force` ends the process at once (SIGKILL) instead of asking it to end (SIGTERM).
    Kill {
        pid: u32,
        force: bool,
        name: String,
    },
}

/// Docker allows these characters in a container name. Anything else is refused, because
/// the name becomes part of a shell command.
pub fn valid_container_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

impl Command {
    /// What is about to happen, for the question before it and the report after it.
    pub fn describe(&self) -> String {
        match self {
            Command::Container { action, name } => format!("{} container {name}", action.label()),
            Command::Kill { pid, force, name } => {
                format!("{} process {pid} ({name})", if *force { "Kill" } else { "End" })
            }
        }
    }

    fn done(&self) -> String {
        match self {
            Command::Container { action, name } => format!("{} container {name}", action.done()),
            Command::Kill { pid, force: false, .. } => format!("Asked process {pid} to end"),
            Command::Kill { pid, force: true, .. } => format!("Killed process {pid}"),
        }
    }

    /// The shell script that carries it out and prints its exit status as the last line.
    /// `None` if the command cannot be put into a script safely.
    pub fn script(&self) -> Option<String> {
        let run = match self {
            Command::Container { action, name } => {
                if !valid_container_name(name) {
                    return None;
                }
                format!(
                    "PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; docker {} {name} 2>&1",
                    action.verb()
                )
            }
            Command::Kill { pid, force, .. } => {
                format!("kill -{} {pid} 2>&1", if *force { "KILL" } else { "TERM" })
            }
        };
        Some(format!("{run}; echo \"@@status $?\""))
    }

    /// The same for this computer when it runs Windows: a program and its arguments.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn windows(&self) -> Option<(&'static str, Vec<String>)> {
        match self {
            Command::Container { action, name } => {
                valid_container_name(name).then(|| ("docker", vec![action.verb().to_string(), name.clone()]))
            }
            Command::Kill { pid, force, .. } => {
                let mut args = vec!["/PID".to_string(), pid.to_string()];
                if *force {
                    args.push("/F".to_string());
                }
                Some(("taskkill", args))
            }
        }
    }

    /// The report shown to the user once the command has run.
    pub fn feedback(&self, ok: bool, message: &str) -> Feedback {
        let message = message.trim();
        let text = match (ok, message.is_empty()) {
            (true, _) => self.done(),
            (false, true) => format!("Could not {}", lowercase_first(&self.describe())),
            (false, false) => format!("Could not {}: {message}", lowercase_first(&self.describe())),
        };
        Feedback {
            text,
            ok,
            at: Instant::now(),
        }
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Split the output of a [`Command::script`] into success and what the tool printed.
pub fn outcome(output: &str) -> (bool, String) {
    let mut ok = false;
    let mut message = Vec::new();
    for line in output.lines() {
        match line.strip_prefix("@@status ") {
            Some(status) => ok = status.trim() == "0",
            None => message.push(line.trim()),
        }
    }
    // Docker repeats the name of the container on success; that is not news.
    let message: Vec<&str> = message.into_iter().filter(|l| !l.is_empty()).collect();
    (ok, message.join(" "))
}

/// What became of the last command, shown for a while at the top of the server page.
#[derive(Clone, Debug)]
pub struct Feedback {
    pub text: String,
    pub ok: bool,
    pub at: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(action: ContainerAction, name: &str) -> Command {
        Command::Container {
            action,
            name: name.into(),
        }
    }

    #[test]
    fn only_real_container_names_reach_the_shell() {
        for name in ["web", "shop_api-1", "a.b", "0db"] {
            assert!(valid_container_name(name), "{name}");
        }
        for name in [
            "",
            "-rf",
            "web; rm -rf /",
            "a b",
            "$(reboot)",
            "a`b`",
            "ä",
            "a/b",
            "a\nb",
        ] {
            assert!(!valid_container_name(name), "{name:?}");
            assert_eq!(container(ContainerAction::Stop, name).script(), None);
            assert_eq!(container(ContainerAction::Stop, name).windows(), None);
        }
    }

    #[test]
    fn builds_the_commands() {
        let stop = container(ContainerAction::Stop, "api");
        assert_eq!(
            stop.script().unwrap(),
            "PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; docker stop api 2>&1; echo \"@@status $?\""
        );
        assert_eq!(
            stop.windows().unwrap(),
            ("docker", vec!["stop".to_string(), "api".to_string()])
        );
        let end = Command::Kill {
            pid: 4711,
            force: false,
            name: "nginx".into(),
        };
        assert_eq!(end.script().unwrap(), "kill -TERM 4711 2>&1; echo \"@@status $?\"");
        let kill = Command::Kill {
            pid: 4711,
            force: true,
            name: "nginx".into(),
        };
        assert_eq!(kill.script().unwrap(), "kill -KILL 4711 2>&1; echo \"@@status $?\"");
        assert_eq!(kill.windows().unwrap().1, ["/PID", "4711", "/F"]);
        // They are wrapped in `sh -c '...'` for SSH.
        assert!(!stop.script().unwrap().contains('\'') && !kill.script().unwrap().contains('\''));
    }

    #[test]
    fn reports_what_happened() {
        let stop = container(ContainerAction::Stop, "api");
        assert_eq!(outcome("api\n@@status 0\n"), (true, "api".to_string()));
        assert_eq!(stop.feedback(true, "api").text, "Stopped container api");

        let (ok, message) = outcome("Error response from daemon: No such container: api\n@@status 1\n");
        assert!(!ok);
        assert_eq!(
            stop.feedback(ok, &message).text,
            "Could not stop container api: Error response from daemon: No such container: api"
        );

        let end = Command::Kill {
            pid: 1,
            force: false,
            name: "init".into(),
        };
        let (ok, message) = outcome("sh: 1: kill: Operation not permitted\n@@status 1\n");
        assert_eq!(
            end.feedback(ok, &message).text,
            "Could not end process 1 (init): sh: 1: kill: Operation not permitted"
        );
        // No status line at all: the connection broke before the command finished.
        assert!(!outcome("").0);
    }
}

#[cfg(all(test, windows))]
mod live {
    use super::*;
    use crate::collector::run_local_status;

    fn run(command: &Command) -> Feedback {
        let (program, args) = command.windows().unwrap();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (ok, output) = run_local_status(program, &args, "").unwrap();
        command.feedback(ok, &output)
    }

    /// Ends a process it starts itself: `cargo test live_kill -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_kill() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "120", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let kill = Command::Kill {
            pid: child.id(),
            force: true,
            name: "ping".into(),
        };
        let feedback = run(&kill);
        println!("{}", feedback.text);
        assert!(feedback.ok);
        assert!(!child.wait().unwrap().success());
        // The same process again: it is gone, and the report says why it failed.
        let again = run(&kill);
        println!("{}", again.text);
        assert!(!again.ok);
    }

    /// Stops, starts and restarts a container it creates itself from an image that is
    /// already there: `GRIDWATCH_TEST_IMAGE=alpine cargo test live_container -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_container() {
        let image = std::env::var("GRIDWATCH_TEST_IMAGE").expect("set GRIDWATCH_TEST_IMAGE");
        let name = "gridwatch-selftest";
        let docker = |args: &[&str]| run_local_status("docker", args, "").unwrap();
        docker(&["rm", "-f", name]);
        assert!(docker(&["run", "-d", "--name", name, &image, "sleep", "600"]).0);
        let state = || {
            docker(&["inspect", "-f", "{{.State.Status}}", name])
                .1
                .trim()
                .to_string()
        };
        assert_eq!(state(), "running");
        for (action, expected) in [
            (ContainerAction::Stop, "exited"),
            (ContainerAction::Start, "running"),
            (ContainerAction::Restart, "running"),
        ] {
            let feedback = run(&Command::Container {
                action,
                name: name.into(),
            });
            println!("{} -> {}", feedback.text, state());
            assert!(feedback.ok);
            assert_eq!(state(), expected);
        }
        docker(&["rm", "-f", name]);
        let gone = run(&Command::Container {
            action: ContainerAction::Stop,
            name: name.into(),
        });
        println!("{}", gone.text);
        assert!(!gone.ok);
    }
}
