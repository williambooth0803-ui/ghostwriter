//! Ask through `codex exec`, using the writer's Codex login. Only the final structured reply
//! is returned; CLI progress, diagnostics and unsuccessful runs must never become suggestions.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Codex {
    pub program: String,
    /// `None` lets Codex choose its default, rather than sending Claude's `sonnet` alias.
    pub model: Option<String>,
    pub extra: Vec<String>,
    pub timeout: Duration,
    pub research: bool,
}

const SCHEMA: &str = r#"{
  "type": "object", "additionalProperties": false, "required": ["suggestions"],
  "properties": {"suggestions": {"type": "array", "items": {"anyOf": [
    {"type":"object","additionalProperties":false,"required":["anchor","information","kind","why"],
     "properties":{"anchor":{"type":"string"},"information":{"type":"string"},"kind":{"type":"string","enum":["answer","context","fact-check","vocabulary"]},"why":{"type":"string"}}},
    {"type": "object", "additionalProperties": false, "required": ["find", "replace", "why"],
     "properties": {"find": {"type": "string"}, "replace": {"type": "string"}, "why": {"type": "string"}}},
    {"type": "object", "additionalProperties": false, "required": ["after", "insert", "why"],
     "properties": {"after": {"type": "string"}, "insert": {"type": "string"}, "why": {"type": "string"}}}
  ]}}}
}"#;

impl Codex {
    fn command(&self, dir: &Path) -> Command {
        let mut c = Command::new(&self.program);
        // A fresh working directory and no user config prevent loading the writer's MCP
        // servers or project instructions. Authentication still comes from their Codex login.
        c.args(["exec", "--ignore-user-config", "--ephemeral", "--skip-git-repo-check", "--sandbox", "read-only", "--color", "never"])
            .args(["-c", "approval_policy=\"never\"", "-c", "features.shell_tool=false", "-c", "features.unified_exec=false"])
            .args([
                "-c",
                "features.apply_patch_freeform=false",
                "-c",
                "features.apps=false",
                "-c",
                if self.research { "web_search=\"live\"" } else { "web_search=\"disabled\"" },
                "-c",
                "project_doc_max_bytes=0",
            ])
            .arg("--output-schema")
            .arg(dir.join("schema.json"))
            .arg("--output-last-message")
            .arg(dir.join("reply.json"));
        if let Some(model) = &self.model {
            c.arg("--model").arg(model);
        }
        c.args(&self.extra).arg("-").current_dir(dir);
        c
    }

    pub fn ask(&self, prompt: &str) -> Result<String, String> {
        let dir = Workdir::new()?;
        let path = &dir.0;
        fs::write(path.join("schema.json"), SCHEMA).map_err(|e| format!("writing Codex response schema: {e}"))?;
        fs::write(path.join("prompt.txt"), prompt).map_err(|e| format!("writing Codex prompt: {e}"))?;
        // Files rather than pipes: even a large prompt or noisy CLI cannot deadlock the
        // timeout loop. The private directory is removed on every success/error path.
        let input = File::open(path.join("prompt.txt")).map_err(|e| format!("opening Codex prompt: {e}"))?;
        let output = File::create(path.join("stdout.txt")).map_err(|e| format!("creating Codex output: {e}"))?;
        let error = File::create(path.join("stderr.txt")).map_err(|e| format!("creating Codex diagnostics: {e}"))?;
        let mut child = self
            .command(path)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error))
            .spawn()
            .map_err(|e| format!("can't start {}: {e} (install Codex CLI and run `codex login`; --codex sets its path)", self.program))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("codex took longer than {} s", self.timeout.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("waiting for codex: {e}"));
                }
            }
        };
        if !status.success() {
            let detail = read_limited(&path.join("stderr.txt")).unwrap_or_default();
            return Err(format!(
                "codex failed ({status}): {} (check `codex login status`; update Codex CLI if an option is unknown)",
                detail.trim().chars().take(400).collect::<String>()
            ));
        }
        let reply = read_limited(&path.join("reply.json"))?;
        if reply.trim().is_empty() {
            return Err("codex returned an empty final reply".into());
        }
        // Fail closed on a missing/malformed final response, even when stdout contains JSON.
        let value: serde_json::Value = serde_json::from_str(&reply).map_err(|e| format!("codex returned invalid JSON: {e}"))?;
        if !value.get("suggestions").is_some_and(serde_json::Value::is_array) {
            return Err("codex reply is missing its suggestions array".into());
        }
        Ok(reply)
    }
}

fn read_limited(path: &Path) -> Result<String, String> {
    const LIMIT: u64 = 1024 * 1024;
    let file = File::open(path).map_err(|e| format!("reading Codex {}: {e}", path.file_name().unwrap_or_default().to_string_lossy()))?;
    let mut text = String::new();
    file.take(LIMIT + 1).read_to_string(&mut text).map_err(|e| format!("reading Codex output: {e}"))?;
    if text.len() as u64 > LIMIT {
        return Err("codex output exceeds 1 MiB".into());
    }
    Ok(text)
}

/// A separate directory for each request: no stale replies, no cross-instance interference.
pub(crate) struct Workdir(PathBuf);

impl Workdir {
    fn new() -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let path = std::env::temp_dir().join(format!("wordcraft-codex-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let mut builder = fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("creating private Codex working directory: {e}")),
            }
        }
        Err("couldn't create a unique Codex working directory".into())
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn codex() -> Codex {
        Codex { program: "codex".into(), model: None, extra: vec![], timeout: Duration::from_secs(2), research: false }
    }

    // Fake executables exercise the real process protocol, without a login, network or billing.
    #[cfg(unix)]
    pub(crate) fn fake(body: &str) -> (Workdir, Codex) {
        use std::os::unix::fs::PermissionsExt;
        let dir = Workdir::new().unwrap();
        let script = dir.0.join("fake codex");
        fs::write(&script, format!("#!/bin/sh\npwd > \"$(dirname \"$0\")/last-dir\"\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut c = codex();
        c.program = script.to_string_lossy().into();
        (dir, c)
    }

    #[cfg(unix)]
    #[test]
    fn sends_stdin_and_reads_only_the_final_reply_then_cleans_up() {
        let (dir, mut c) = fake(
            r#"
test "$1" = exec || exit 11
capture=$(dirname "$0")
printf '%s\n' "$@" > "$capture/args"
pwd > "$capture/cwd"
cat > "$capture/prompt"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output-last-message) shift; reply="$1" ;;
    --output-schema) shift; test -s "$1" || exit 12 ;;
  esac
  shift
done
printf '%s' '{"suggestions":[{"find":"wordy","replace":"clear","why":"clarity"}]}' > "$reply"
printf '%s' '{"suggestions":[{"find":"WRONG","replace":"stdout"}]}'
printf '%s' 'progress is not the answer' >&2
"#,
        );
        c.model = Some("test-model".into());
        c.extra = vec!["--config".into(), "model_reasoning_effort=\"low\"".into()];
        let prompt = "A private paragraph with café, quotes ' and $(shell syntax).".repeat(10_000);
        let reply = c.ask(&prompt).unwrap();
        assert_eq!(crate::prompt::parse(&reply).unwrap()[0].params["replace"], "clear");
        assert_eq!(fs::read_to_string(dir.0.join("prompt")).unwrap(), prompt);
        let args = fs::read_to_string(dir.0.join("args")).unwrap();
        assert!(args.contains("--ignore-user-config\n--ephemeral\n--skip-git-repo-check\n--sandbox\nread-only\n"));
        assert!(args.contains("approval_policy=\"never\""));
        assert!(args.contains("features.shell_tool=false"));
        assert!(args.contains("--model\ntest-model\n"));
        assert!(args.contains("model_reasoning_effort=\"low\""));
        assert!(!args.contains("private paragraph"));
        let cwd = fs::read_to_string(dir.0.join("cwd")).unwrap();
        assert!(!Path::new(cwd.trim()).exists(), "prompt/reply directory must be removed");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_failures_missing_output_and_malformed_replies() {
        for body in [
            "echo 'login required' >&2; exit 7",
            "echo '{\"suggestions\":[]}'", // Stdout alone must never count as the final reply.
            "while [ \"$1\" != --output-last-message ]; do shift; done; shift; echo 'not json' > \"$1\"",
            "while [ \"$1\" != --output-last-message ]; do shift; done; shift; echo '{}' > \"$1\"",
            "while [ \"$1\" != --output-last-message ]; do shift; done; shift; touch \"$1\"",
            "while [ \"$1\" != --output-last-message ]; do shift; done; shift; echo '{\"suggestions\":[]}' > \"$1\"; exit 7",
        ] {
            let (dir, c) = fake(body);
            assert!(c.ask("paragraph").is_err(), "{body}");
            let cwd = fs::read_to_string(dir.0.join("last-dir")).unwrap();
            assert!(!Path::new(cwd.trim()).exists(), "failed requests must remove private prompt/reply files");
        }
    }

    #[cfg(unix)]
    #[test]
    fn kills_and_reaps_a_timed_out_cli() {
        let (_dir, mut c) = fake("exec sleep 30");
        c.timeout = Duration::from_millis(100);
        let start = Instant::now();
        assert!(c.ask("paragraph").unwrap_err().contains("took longer"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn missing_executable_is_actionable_and_model_defaults_to_codex() {
        let dir = Workdir::new().unwrap();
        let mut c = codex();
        c.program = dir.0.join("missing-executable").to_string_lossy().into();
        let args: Vec<_> = c.command(&dir.0).get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(!args.iter().any(|a| a == "--model" || a == "sonnet"));
        assert!(c.ask("paragraph").unwrap_err().contains("codex login"));
    }
}
