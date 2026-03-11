use anyhow::{Context, Result, bail};
use serde_json::json;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalAgentMode {
    Codex,
    Claude,
    OpenCode,
    Generic,
}

#[derive(Debug, Clone)]
pub struct ProposalAgentCommand {
    pub command: String,
    pub args: Vec<String>,
    pub mode: ProposalAgentMode,
}

pub struct PreparedProposalCommand {
    pub command: String,
    pub args: Vec<String>,
    pub mode: ProposalAgentMode,
    pub env_overrides: Vec<(String, String)>,
    pub temp_files: Vec<PathBuf>,
    pub sidecar_output_path: Option<PathBuf>,
    _isolated_home: Option<IsolatedAgentHome>,
}

struct IsolatedAgentHome {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl Drop for IsolatedAgentHome {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub fn proposal_agent_command(agent_name: &str) -> ProposalAgentCommand {
    match agent_name {
        "claude" => ProposalAgentCommand {
            command: "claude".to_string(),
            args: vec![
                "--print".to_string(),
                "--no-session-persistence".to_string(),
                "--verbose".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--permission-mode".to_string(),
                "bypassPermissions".to_string(),
                "--tools".to_string(),
                "Read,Grep,Glob,LS".to_string(),
            ],
            mode: ProposalAgentMode::Claude,
        },
        "codex" => ProposalAgentCommand {
            command: "codex".to_string(),
            args: vec!["exec".to_string(), "--ephemeral".to_string()],
            mode: ProposalAgentMode::Codex,
        },
        "opencode" => ProposalAgentCommand {
            command: "opencode".to_string(),
            args: vec![
                "run".to_string(),
                "--agent".to_string(),
                "plan".to_string(),
                "--format".to_string(),
                "json".to_string(),
            ],
            mode: ProposalAgentMode::OpenCode,
        },
        other => ProposalAgentCommand {
            command: other.to_string(),
            args: vec![],
            mode: ProposalAgentMode::Generic,
        },
    }
}

pub fn prepare_proposal_command(
    command: &str,
    args: &[String],
    workspace_root: &Path,
    debug_run_dir: Option<&Path>,
    output_schema: Option<&str>,
) -> Result<PreparedProposalCommand> {
    let mut effective_args = args.to_vec();
    let mut temp_files = vec![];
    let spec = proposal_agent_command(command);
    let mode = if spec.command == command {
        spec.mode
    } else {
        proposal_agent_mode(command, args)
    };
    let mut sidecar_output_path = None;

    match mode {
        ProposalAgentMode::Codex => {
            if !effective_args.iter().any(|arg| arg == "--json") {
                effective_args.push("--json".into());
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "--skip-git-repo-check")
            {
                effective_args.push("--skip-git-repo-check".into());
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "--sandbox" || arg == "-s")
            {
                effective_args.push("--sandbox".into());
                effective_args.push("read-only".into());
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "-C" || arg == "--cd")
            {
                effective_args.push("-C".into());
                effective_args.push(workspace_root.to_string_lossy().to_string());
            }
            if output_schema.is_some() && !effective_args.iter().any(|arg| arg == "--output-schema")
            {
                let schema_path = create_temp_file_path("distill-codex-schema", "json")?;
                std::fs::write(&schema_path, output_schema.unwrap_or_default()).with_context(
                    || {
                        format!(
                            "Failed to write Codex schema file {}",
                            schema_path.display()
                        )
                    },
                )?;
                effective_args.push("--output-schema".into());
                effective_args.push(schema_path.to_string_lossy().to_string());
                temp_files.push(schema_path);
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "--output-last-message" || arg == "-o")
            {
                let last_message_path = create_temp_file_path("distill-codex-last-message", "txt")?;
                effective_args.push("--output-last-message".into());
                effective_args.push(last_message_path.to_string_lossy().to_string());
                sidecar_output_path = Some(last_message_path.clone());
                temp_files.push(last_message_path);
            }
        }
        ProposalAgentMode::Claude => {
            if !effective_args.iter().any(|arg| arg == "--add-dir") {
                effective_args.push("--add-dir".into());
                effective_args.push(workspace_root.to_string_lossy().to_string());
            }
        }
        ProposalAgentMode::OpenCode => {}
        ProposalAgentMode::Generic => {}
    }

    let isolated_home = match mode {
        ProposalAgentMode::Codex => Some(prepare_isolated_codex_home(debug_run_dir)?),
        ProposalAgentMode::Claude => Some(prepare_isolated_claude_home(debug_run_dir)?),
        ProposalAgentMode::OpenCode => Some(prepare_isolated_opencode_home(debug_run_dir)?),
        ProposalAgentMode::Generic => None,
    };

    let mut env_overrides = Vec::new();
    match (mode, isolated_home.as_ref()) {
        (ProposalAgentMode::Codex, Some(agent_home)) => {
            env_overrides.push((
                "CODEX_HOME".to_string(),
                agent_home.path.to_string_lossy().to_string(),
            ));
        }
        (ProposalAgentMode::Claude, Some(agent_home)) => {
            env_overrides.push((
                "HOME".to_string(),
                agent_home.path.to_string_lossy().to_string(),
            ));
        }
        (ProposalAgentMode::OpenCode, Some(agent_home)) => {
            let home = agent_home.path.to_string_lossy().to_string();
            env_overrides.push(("HOME".to_string(), home.clone()));
            env_overrides.push((
                "OPENCODE_CONFIG_CONTENT".to_string(),
                locked_opencode_config_json(),
            ));
            env_overrides.push((
                "XDG_CONFIG_HOME".to_string(),
                PathBuf::from(&home)
                    .join(".config")
                    .to_string_lossy()
                    .to_string(),
            ));
            env_overrides.push((
                "XDG_DATA_HOME".to_string(),
                PathBuf::from(&home)
                    .join(".local")
                    .join("share")
                    .to_string_lossy()
                    .to_string(),
            ));
        }
        _ => {}
    }

    Ok(PreparedProposalCommand {
        command: command.to_string(),
        args: effective_args,
        mode,
        env_overrides,
        temp_files,
        sidecar_output_path,
        _isolated_home: isolated_home,
    })
}

pub fn finalize_proposal_output(
    mode: ProposalAgentMode,
    stdout: &str,
    sidecar_output_path: Option<&Path>,
) -> Result<String> {
    match mode {
        ProposalAgentMode::Codex => {
            if let Some(path) = sidecar_output_path
                && let Ok(contents) = std::fs::read_to_string(path)
                && !contents.trim().is_empty()
            {
                return Ok(contents);
            }
            Ok(stdout.to_string())
        }
        ProposalAgentMode::Claude => extract_claude_stream_output(stdout),
        ProposalAgentMode::OpenCode => extract_opencode_json_output(stdout),
        ProposalAgentMode::Generic => Ok(stdout.to_string()),
    }
}

pub fn extract_json_value(text: &str) -> Result<serde_json::Value> {
    let trimmed = text.trim();
    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        inner
            .rfind("```")
            .map(|position| &inner[..position])
            .unwrap_or(inner)
            .trim()
    } else {
        trimmed
    };

    serde_json::from_str(json_str).context("Failed to parse agent response as JSON")
}

pub fn cleanup_temp_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

pub fn proposal_agent_mode(command: &str, args: &[String]) -> ProposalAgentMode {
    if is_codex_exec(command, args) {
        ProposalAgentMode::Codex
    } else if is_claude_cli(command) {
        ProposalAgentMode::Claude
    } else if is_opencode_run(command, args) {
        ProposalAgentMode::OpenCode
    } else {
        ProposalAgentMode::Generic
    }
}

fn is_codex_exec(command: &str, args: &[String]) -> bool {
    let command_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    command_name == "codex" && args.first().is_some_and(|arg| arg == "exec")
}

fn is_claude_cli(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        == "claude"
}

fn is_opencode_run(command: &str, args: &[String]) -> bool {
    let command_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    command_name == "opencode" && args.first().is_some_and(|arg| arg == "run")
}

fn create_temp_file_path(prefix: &str, extension: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = tmp_dir.join(format!("{prefix}-{pid}-{nanos}-{attempt}.{extension}"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("Failed to create temporary file {}", path.display())
                });
            }
        }
    }
}

fn create_temp_dir_path(prefix: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = tmp_dir.join(format!("{prefix}-{pid}-{nanos}-{attempt}"));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create directory {}", path.display()));
            }
        }
    }
}

fn user_home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot resolve the user home directory")
}

fn codex_home_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    Ok(user_home_dir()?.join(".codex"))
}

fn copy_snapshot_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to stat snapshot source {}", source.display()));
        }
    };

    if metadata.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create parent directory for snapshot symlink {}",
                    destination.display()
                )
            })?;
        }
        let target = std::fs::read_link(source)
            .with_context(|| format!("Failed to read symlink {}", source.display()))?;
        symlink(&target, destination).with_context(|| {
            format!(
                "Failed to create snapshot symlink {} -> {}",
                destination.display(),
                target.display()
            )
        })?;
        return Ok(());
    }

    if metadata.is_dir() {
        std::fs::create_dir_all(destination).with_context(|| {
            format!(
                "Failed to create snapshot directory {}",
                destination.display()
            )
        })?;
        for entry in std::fs::read_dir(source)
            .with_context(|| format!("Failed to read directory {}", source.display()))?
        {
            let entry = entry
                .with_context(|| format!("Failed to iterate directory {}", source.display()))?;
            copy_snapshot_path(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create parent directory for snapshot file {}",
                    destination.display()
                )
            })?;
        }
        std::fs::copy(source, destination).with_context(|| {
            format!(
                "Failed to copy snapshot file {} to {}",
                source.display(),
                destination.display()
            )
        })?;
    }

    Ok(())
}

fn copy_snapshot_entries(
    source_root: &Path,
    destination_root: &Path,
    entries: &[&str],
) -> Result<()> {
    std::fs::create_dir_all(destination_root).with_context(|| {
        format!(
            "Failed to create snapshot root {}",
            destination_root.display()
        )
    })?;
    for entry in entries {
        let relative_path = Path::new(entry);
        copy_snapshot_path(
            &source_root.join(relative_path),
            &destination_root.join(relative_path),
        )?;
    }
    Ok(())
}

fn populate_isolated_codex_home(source_home: &Path, isolated_home: &Path) -> Result<()> {
    copy_snapshot_entries(
        source_home,
        isolated_home,
        &[
            "auth.json",
            "config.toml",
            "AGENTS.md",
            "rules",
            "skills",
            "vendor_imports",
        ],
    )
}

fn prepare_isolated_codex_home_from_source(
    source_home: &Path,
    debug_run_dir: Option<&Path>,
) -> Result<IsolatedAgentHome> {
    let (path, cleanup_on_drop) = if let Some(run_dir) = debug_run_dir {
        (run_dir.join("codex-home"), false)
    } else {
        (create_temp_dir_path("distill-codex-home")?, true)
    };

    populate_isolated_codex_home(source_home, &path)?;

    Ok(IsolatedAgentHome {
        path,
        cleanup_on_drop,
    })
}

fn prepare_isolated_codex_home(debug_run_dir: Option<&Path>) -> Result<IsolatedAgentHome> {
    let source_home = codex_home_dir()?;
    prepare_isolated_codex_home_from_source(&source_home, debug_run_dir)
}

fn populate_isolated_claude_home(source_home: &Path, isolated_home: &Path) -> Result<()> {
    copy_snapshot_entries(
        source_home,
        isolated_home,
        &[
            ".claude.json",
            ".claude/CLAUDE.md",
            ".claude/settings.json",
            ".claude/commands",
            ".claude/skills",
            ".claude/hooks",
            ".claude/plugins",
            ".claude/ide",
            ".claude/plans",
        ],
    )
}

fn prepare_isolated_claude_home_from_source(
    source_home: &Path,
    debug_run_dir: Option<&Path>,
) -> Result<IsolatedAgentHome> {
    let (path, cleanup_on_drop) = if let Some(run_dir) = debug_run_dir {
        (run_dir.join("claude-home"), false)
    } else {
        (create_temp_dir_path("distill-claude-home")?, true)
    };

    populate_isolated_claude_home(source_home, &path)?;

    Ok(IsolatedAgentHome {
        path,
        cleanup_on_drop,
    })
}

fn prepare_isolated_claude_home(debug_run_dir: Option<&Path>) -> Result<IsolatedAgentHome> {
    let source_home = user_home_dir()?;
    prepare_isolated_claude_home_from_source(&source_home, debug_run_dir)
}

fn populate_isolated_opencode_home(source_home: &Path, isolated_home: &Path) -> Result<()> {
    copy_snapshot_entries(
        source_home,
        isolated_home,
        &[
            ".config/opencode/opencode.json",
            ".config/opencode/agents",
            ".config/opencode/commands",
            ".config/opencode/modes",
            ".config/opencode/plugins",
            ".config/opencode/skills",
            ".config/opencode/tools",
            ".config/opencode/themes",
            ".local/share/opencode/auth.json",
        ],
    )
}

fn prepare_isolated_opencode_home_from_source(
    source_home: &Path,
    debug_run_dir: Option<&Path>,
) -> Result<IsolatedAgentHome> {
    let (path, cleanup_on_drop) = if let Some(run_dir) = debug_run_dir {
        (run_dir.join("opencode-home"), false)
    } else {
        (create_temp_dir_path("distill-opencode-home")?, true)
    };

    populate_isolated_opencode_home(source_home, &path)?;

    Ok(IsolatedAgentHome {
        path,
        cleanup_on_drop,
    })
}

fn prepare_isolated_opencode_home(debug_run_dir: Option<&Path>) -> Result<IsolatedAgentHome> {
    let source_home = user_home_dir()?;
    prepare_isolated_opencode_home_from_source(&source_home, debug_run_dir)
}

fn locked_opencode_config_json() -> String {
    json!({
        "$schema": "https://opencode.ai/config.json",
        "permission": {
            "edit": "deny",
            "bash": "deny",
            "webfetch": "deny"
        }
    })
    .to_string()
}

fn extract_claude_stream_output(stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("Claude agent returned no output");
    }

    let mut candidates = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if value
            .get("is_error")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let message = value
                .get("result")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown Claude error");
            bail!("Claude agent returned an error: {message}");
        }

        if let Some(structured) = value.get("structured_output") {
            candidates.push(structured.to_string());
        }

        collect_text_candidates(&value, &mut candidates, 0);
    }

    for candidate in candidates.into_iter().rev() {
        if extract_json_value(&candidate).is_ok() {
            return Ok(candidate);
        }
    }

    if extract_json_value(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }

    bail!("Failed to extract structured JSON from Claude stream output")
}

fn extract_opencode_json_output(stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("OpenCode agent returned no output");
    }

    if extract_json_value(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }

    let mut candidates = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if value
            .get("type")
            .and_then(|value| value.as_str())
            .is_some_and(|kind| kind.eq_ignore_ascii_case("error"))
        {
            let message = value
                .get("message")
                .and_then(|value| value.as_str())
                .or_else(|| value.get("error").and_then(|value| value.as_str()))
                .unwrap_or("unknown OpenCode error");
            bail!("OpenCode agent returned an error: {message}");
        }

        if looks_like_wrapped_json(&value) {
            return Ok(value.to_string());
        }

        collect_text_candidates(&value, &mut candidates, 0);
    }

    for candidate in candidates.into_iter().rev() {
        if extract_json_value(&candidate).is_ok() {
            return Ok(candidate);
        }
    }

    bail!("Failed to extract structured JSON from OpenCode output")
}

fn looks_like_wrapped_json(value: &serde_json::Value) -> bool {
    value.get("inspected_files").is_some()
        || value.get("proposals").is_some()
        || value.get("file_findings").is_some()
}

fn collect_text_candidates(value: &serde_json::Value, out: &mut Vec<String>, depth: usize) {
    const MAX_DEPTH: usize = 4;
    if depth > MAX_DEPTH {
        return;
    }

    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_text_candidates(item, out, depth + 1);
            }
        }
        serde_json::Value::Object(map) => {
            for key in [
                "result",
                "text",
                "content",
                "message",
                "structured_output",
                "output",
                "data",
            ] {
                if let Some(next) = map.get(key) {
                    collect_text_candidates(next, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_proposal_agent_command_supports_opencode() {
        let command = proposal_agent_command("opencode");
        assert_eq!(command.command, "opencode");
        assert_eq!(command.mode, ProposalAgentMode::OpenCode);
        assert_eq!(
            command.args,
            vec![
                "run".to_string(),
                "--agent".to_string(),
                "plan".to_string(),
                "--format".to_string(),
                "json".to_string()
            ]
        );
    }

    #[test]
    fn test_prepare_proposal_command_for_opencode_sets_inline_permissions() {
        let prepared = prepare_proposal_command(
            "opencode",
            &proposal_agent_command("opencode").args,
            Path::new("/tmp/workspace"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(prepared.mode, ProposalAgentMode::OpenCode);
        assert!(
            prepared
                .env_overrides
                .iter()
                .any(|(key, value)| key == "OPENCODE_CONFIG_CONTENT"
                    && value.contains("\"edit\":\"deny\""))
        );
    }

    #[test]
    fn test_extract_claude_stream_output_uses_last_json_candidate() {
        let stdout = r#"{"type":"tool_use","tool":"Read","path":"/tmp/workspace/sessions/claude/0001.jsonl"}
{"type":"assistant","text":"thinking"}
{"type":"result","result":"{\"inspected_files\":[\"/tmp/workspace/sessions/claude/0001.jsonl\"],\"file_findings\":[{\"session\":\"/tmp/workspace/sessions/claude/0001.jsonl\",\"summary\":\"Repeated workflow.\"}],\"proposals\":[]}"}"#;

        let output = extract_claude_stream_output(stdout).unwrap();
        assert!(output.contains("\"inspected_files\""));
    }

    #[test]
    fn test_extract_opencode_json_output_reads_embedded_result() {
        let stdout = r#"{"type":"tool","name":"read","path":"/tmp/workspace/sessions/opencode/0001-session.json"}
{"type":"result","output":"{\"inspected_files\":[\"/tmp/workspace/sessions/opencode/0001-session.json\"],\"file_findings\":[{\"session\":\"/tmp/workspace/sessions/opencode/0001-session.json\",\"summary\":\"Repeated workflow.\"}],\"proposals\":[]}"}"#;

        let output = extract_opencode_json_output(stdout).unwrap();
        assert!(output.contains("\"file_findings\""));
    }

    #[test]
    fn test_populate_isolated_codex_home_copies_control_plane_only() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();

        fs::write(source.path().join("auth.json"), "{\"token\":\"secret\"}").unwrap();
        fs::write(source.path().join("config.toml"), "model = \"gpt-5.4\"\n").unwrap();
        fs::write(source.path().join("AGENTS.md"), "# agents\n").unwrap();
        fs::create_dir_all(source.path().join("rules")).unwrap();
        fs::write(source.path().join("rules/policy.md"), "be strict\n").unwrap();
        fs::create_dir_all(source.path().join("vendor_imports")).unwrap();
        fs::write(
            source.path().join("vendor_imports/provider.txt"),
            "imported\n",
        )
        .unwrap();
        fs::create_dir_all(source.path().join("skills")).unwrap();
        fs::write(source.path().join("skills/local.md"), "skill\n").unwrap();
        fs::write(source.path().join("state_5.sqlite"), "do not copy").unwrap();
        fs::create_dir_all(source.path().join("sessions")).unwrap();
        fs::write(source.path().join("sessions/old.jsonl"), "{}\n").unwrap();

        populate_isolated_codex_home(source.path(), destination.path()).unwrap();

        assert!(destination.path().join("auth.json").is_file());
        assert!(destination.path().join("config.toml").is_file());
        assert!(destination.path().join("skills/local.md").is_file());
        assert!(!destination.path().join("state_5.sqlite").exists());
        assert!(!destination.path().join("sessions").exists());
    }

    #[test]
    fn test_populate_isolated_claude_home_copies_control_plane_only() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();

        fs::write(source.path().join(".claude.json"), "{\"token\":\"secret\"}").unwrap();
        fs::create_dir_all(source.path().join(".claude/hooks")).unwrap();
        fs::create_dir_all(source.path().join(".claude/plugins")).unwrap();
        fs::create_dir_all(source.path().join(".claude/plans")).unwrap();
        fs::write(source.path().join(".claude/CLAUDE.md"), "# claude\n").unwrap();
        fs::write(
            source.path().join(".claude/settings.json"),
            "{\"theme\":\"dark\"}",
        )
        .unwrap();
        fs::write(source.path().join(".claude/hooks/pre.sh"), "echo hook\n").unwrap();
        fs::write(source.path().join(".claude/plugins/plugin.js"), "plugin\n").unwrap();
        fs::write(source.path().join(".claude/plans/plan.md"), "plan\n").unwrap();
        fs::create_dir_all(source.path().join(".claude/projects")).unwrap();
        fs::write(source.path().join(".claude/projects/session.jsonl"), "{}\n").unwrap();

        populate_isolated_claude_home(source.path(), destination.path()).unwrap();

        assert!(destination.path().join(".claude.json").is_file());
        assert!(destination.path().join(".claude/hooks/pre.sh").is_file());
        assert!(
            destination
                .path()
                .join(".claude/plugins/plugin.js")
                .is_file()
        );
        assert!(!destination.path().join(".claude/projects").exists());
    }

    #[test]
    fn test_populate_isolated_opencode_home_copies_control_plane_only() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();

        fs::create_dir_all(source.path().join(".config/opencode/agents")).unwrap();
        fs::create_dir_all(source.path().join(".config/opencode/skills")).unwrap();
        fs::create_dir_all(source.path().join(".local/share/opencode")).unwrap();
        fs::write(
            source.path().join(".config/opencode/opencode.json"),
            "{\"theme\":\"dark\"}",
        )
        .unwrap();
        fs::write(
            source.path().join(".config/opencode/agents/review.md"),
            "---\nmode: subagent\n---\n",
        )
        .unwrap();
        fs::write(
            source
                .path()
                .join(".config/opencode/skills/review/SKILL.md"),
            "# Review\n",
        )
        .unwrap_or(());
        fs::create_dir_all(source.path().join(".config/opencode/skills/review")).unwrap();
        fs::write(
            source
                .path()
                .join(".config/opencode/skills/review/SKILL.md"),
            "# Review\n",
        )
        .unwrap();
        fs::write(
            source.path().join(".local/share/opencode/auth.json"),
            "{\"token\":\"secret\"}",
        )
        .unwrap();
        fs::create_dir_all(source.path().join(".local/share/opencode/project")).unwrap();
        fs::write(
            source
                .path()
                .join(".local/share/opencode/project/session.json"),
            "{}",
        )
        .unwrap();

        populate_isolated_opencode_home(source.path(), destination.path()).unwrap();

        assert!(
            destination
                .path()
                .join(".config/opencode/opencode.json")
                .is_file()
        );
        assert!(
            destination
                .path()
                .join(".config/opencode/agents/review.md")
                .is_file()
        );
        assert!(
            destination
                .path()
                .join(".config/opencode/skills/review/SKILL.md")
                .is_file()
        );
        assert!(
            destination
                .path()
                .join(".local/share/opencode/auth.json")
                .is_file()
        );
        assert!(
            !destination
                .path()
                .join(".local/share/opencode/project")
                .exists()
        );
    }

    #[test]
    fn test_prepare_isolated_codex_home_under_debug_run_dir_is_preserved() {
        let source = tempfile::tempdir().unwrap();
        let debug_run_dir = tempfile::tempdir().unwrap();
        fs::write(source.path().join("auth.json"), "{\"token\":\"secret\"}").unwrap();

        let isolated_home =
            prepare_isolated_codex_home_from_source(source.path(), Some(debug_run_dir.path()))
                .unwrap();
        let isolated_path = isolated_home.path.clone();
        drop(isolated_home);

        assert_eq!(isolated_path, debug_run_dir.path().join("codex-home"));
        assert!(isolated_path.join("auth.json").is_file());
    }

    #[test]
    fn test_prepare_isolated_opencode_home_without_debug_run_dir_cleans_up_on_drop() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join(".local/share/opencode")).unwrap();
        fs::write(
            source.path().join(".local/share/opencode/auth.json"),
            "{\"token\":\"secret\"}",
        )
        .unwrap();

        let isolated_path = {
            let isolated_home =
                prepare_isolated_opencode_home_from_source(source.path(), None).unwrap();
            let isolated_path = isolated_home.path.clone();
            assert!(isolated_path.exists());
            isolated_path
        };

        assert!(!isolated_path.exists());
    }

    #[test]
    fn test_extract_json_value_handles_fenced_blocks() {
        let value = extract_json_value("```json\n{\"ok\":true}\n```").unwrap();
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn test_proposal_agent_mode_detects_known_profiles() {
        assert_eq!(
            proposal_agent_mode("claude", &proposal_agent_command("claude").args),
            ProposalAgentMode::Claude
        );
        assert_eq!(
            proposal_agent_mode("codex", &proposal_agent_command("codex").args),
            ProposalAgentMode::Codex
        );
        assert_eq!(
            proposal_agent_mode("opencode", &proposal_agent_command("opencode").args),
            ProposalAgentMode::OpenCode
        );
    }

    #[test]
    fn test_locked_opencode_config_json_denies_mutation_tools() {
        let config = locked_opencode_config_json();
        assert!(config.contains("\"edit\":\"deny\""));
        assert!(config.contains("\"bash\":\"deny\""));
        assert!(config.contains("\"webfetch\":\"deny\""));
    }
}
