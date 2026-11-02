use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
use tauri::Manager;

static LOCK: Mutex<()> = Mutex::new(());
const AGENTS: [&str; 3] = ["claude", "codex", "gemini"];
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ResourceOperation {
    Create,
    Edit,
    Remove,
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    skills: BTreeMap<String, ManagedSkill>,
    #[serde(default)]
    disabled: BTreeMap<String, BTreeMap<String, Value>>,
}
#[derive(Serialize, Deserialize)]
struct ManagedSkill {
    source: String,
    #[serde(default)]
    source_fingerprint: Option<String>,
    targets: BTreeMap<String, String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    name: String,
    description: String,
    paths: Vec<String>,
    source: Option<String>,
    managed: bool,
    installed: BTreeMap<String, bool>,
    owned: BTreeMap<String, bool>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct McpEntry {
    pub id: String,
    pub agent: String,
    pub enabled: bool,
    pub spec: Value,
}
#[derive(Serialize)]
pub struct Snapshot {
    skills: Vec<Skill>,
    mcp: Vec<McpEntry>,
    errors: Vec<String>,
}

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 100
        || id == "."
        || id == ".."
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(
            "Use 1–100 letters, numbers, dots, underscores or hyphens for the identifier".into(),
        );
    }
    Ok(())
}
fn agent_dir(home: &Path, agent: &str) -> Result<PathBuf> {
    match agent {
        "claude" => Ok(home.join(".claude")),
        "codex" => Ok(home.join(".codex")),
        "gemini" => Ok(home.join(".gemini")),
        _ => Err("Unsupported target".into()),
    }
}
fn store(home: &Path) -> PathBuf {
    home.join(".config/easy-cli-proxy-api/agent-resources")
}
fn no_links(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if ancestor
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("Parent traversal is not allowed".into());
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Symbolic links are not writable managed paths".into())
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err("Cannot inspect managed path".into())
            }
            _ => (),
        }
    }
    Ok(())
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    no_links(path)?;
    match fs::read(path) {
        Ok(bytes) if bytes.len() <= 8 * 1024 * 1024 => Ok(Some(bytes)),
        Ok(_) => Err("Configuration exceeds 8 MiB".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Cannot read configuration".into()),
    }
}
fn registry(home: &Path) -> Result<Registry> {
    parse_registry(read(&store(home).join("state.json"))?.as_deref())
}
fn parse_registry(bytes: Option<&[u8]>) -> Result<Registry> {
    match bytes {
        Some(bytes) => serde_json::from_slice(bytes)
            .map_err(|_| "Invalid resource registry; nothing was changed".into()),
        None => Ok(Registry::default()),
    }
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    no_links(path)?;
    let parent = path.parent().ok_or("Missing parent")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create configuration directory")?;
    let temporary = path.with_extension("ecpa-tmp");
    no_links(&temporary)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| "Cannot create temporary file; check for a stale .ecpa-tmp file")?;
    let result = (|| {
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot write temporary configuration")?;
        drop(file);
        #[cfg(windows)]
        let renamed = crate::core_config::replace_file_atomically(&temporary, path);
        #[cfg(not(windows))]
        let renamed = fs::rename(&temporary, path);
        renamed.map_err(|_| "Cannot replace configuration")
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(str::to_string)
}
fn save_registry(home: &Path, state: &Registry) -> Result<()> {
    atomic(
        &store(home).join("state.json"),
        &serde_json::to_vec_pretty(state).map_err(|_| "Cannot serialize registry")?,
    )
}
#[derive(Clone)]
struct SkillFile {
    bytes: Vec<u8>,
    permissions: fs::Permissions,
    directory: bool,
}
fn files(directory: &Path) -> Result<BTreeMap<PathBuf, SkillFile>> {
    fn walk(
        root: &Path,
        current: &Path,
        depth: usize,
        result: &mut BTreeMap<PathBuf, SkillFile>,
        total: &mut usize,
    ) -> Result<()> {
        no_links(current)?;
        if depth > 20 {
            return Err("Skill directory is too deep".into());
        }
        for entry in fs::read_dir(current).map_err(|_| "Cannot read skill directory")? {
            let path = entry.map_err(|_| "Cannot read skill entry")?.path();
            no_links(&path)?;
            if path.is_dir() {
                walk(root, &path, depth + 1, result, total)?;
                if fs::read_dir(&path)
                    .map_err(|_| "Cannot read skill directory")?
                    .next()
                    .is_none()
                {
                    if result.len() >= 2000 {
                        return Err("Skill exceeds 2000 files or empty directories".into());
                    }
                    result.insert(
                        path.strip_prefix(root)
                            .map_err(|_| "Invalid skill path")?
                            .to_path_buf(),
                        SkillFile {
                            bytes: Vec::new(),
                            permissions: fs::metadata(&path)
                                .map_err(|_| "Cannot inspect skill permissions")?
                                .permissions(),
                            directory: true,
                        },
                    );
                }
            } else if path.is_file() {
                let size = fs::metadata(&path)
                    .map_err(|_| "Cannot inspect skill file")?
                    .len();
                if size > 32 * 1024 * 1024
                    || *total as u64 + size > 32 * 1024 * 1024
                    || result.len() >= 2000
                {
                    return Err("Skill exceeds 32 MiB or 2000 files".into());
                }
                let content = fs::read(&path).map_err(|_| "Cannot read skill file")?;
                *total += content.len();
                let permissions = fs::metadata(&path)
                    .map_err(|_| "Cannot inspect skill permissions")?
                    .permissions();
                result.insert(
                    path.strip_prefix(root)
                        .map_err(|_| "Invalid skill path")?
                        .to_path_buf(),
                    SkillFile {
                        bytes: content,
                        permissions,
                        directory: false,
                    },
                );
            } else {
                return Err("Special files are not supported in skills".into());
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    walk(directory, directory, 0, &mut result, &mut 0)?;
    if !result
        .get(Path::new("SKILL.md"))
        .is_some_and(|entry| !entry.directory)
    {
        return Err("Skill directory must contain SKILL.md".into());
    }
    Ok(result)
}
fn fingerprint(content: &BTreeMap<PathBuf, SkillFile>) -> String {
    let mut digest = Sha256::new();
    for (path, file) in content {
        if file.directory {
            digest.update(b"directory\0");
        }
        let name = path.to_string_lossy();
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((file.bytes.len() as u64).to_le_bytes());
        digest.update(&file.bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update((file.permissions.mode() & 0o777).to_le_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}
fn publish_skill(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|_| "Invalid skill path")?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| "Invalid skill path")?;
        #[cfg(target_os = "linux")]
        let outcome = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let outcome =
            unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        if outcome != 0 {
            return Err("Cannot publish skill; destination may already exist".into());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        fs::rename(source, destination)
            .map_err(|_| "Cannot publish skill; destination may already exist".into())
    }
}
fn copy_skill(destination: &Path, content: &BTreeMap<PathBuf, SkillFile>) -> Result<()> {
    no_links(destination)?;
    if destination.exists() {
        return Err("Skill target already exists; no files were overwritten".into());
    }
    let staging = destination.with_extension("ecpa-stage");
    no_links(&staging)?;
    fs::create_dir_all(destination.parent().ok_or("Missing skill parent")?)
        .map_err(|_| "Cannot create skill parent")?;
    fs::create_dir(&staging)
        .map_err(|_| "Skill staging directory already exists or is not writable")?;
    let outcome = (|| {
        for (relative, file) in content {
            let path = staging.join(relative);
            if file.directory {
                fs::create_dir_all(&path).map_err(|_| "Cannot create skill directory")?;
            } else {
                atomic(&path, &file.bytes)?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(file.permissions.mode() & 0o777),
                )
                .map_err(|_| "Cannot preserve skill permissions")?;
            }
        }
        publish_skill(&staging, destination)
    })();
    if outcome.is_err() {
        let _ = fs::remove_dir_all(staging);
    }
    outcome
}
fn description(path: &Path) -> String {
    let Ok(Some(bytes)) = read(&path.join("SKILL.md")) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    text.strip_prefix("---\n")
        .and_then(|body| body.split_once("\n---"))
        .and_then(|(front, _)| serde_yaml::from_str::<Value>(front).ok())
        .and_then(|front| {
            front
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}
fn skills(home: &Path, state: &Registry, errors: &mut Vec<String>) -> Vec<Skill> {
    let mut rows = BTreeMap::<String, Skill>::new();
    for (name, managed) in &state.skills {
        rows.insert(
            name.clone(),
            Skill {
                name: name.clone(),
                description: description(&store(home).join("skills").join(name)),
                paths: vec![store(home).join("skills").join(name).display().to_string()],
                source: Some(managed.source.clone()),
                managed: true,
                installed: BTreeMap::new(),
                owned: BTreeMap::new(),
            },
        );
    }
    for agent in AGENTS {
        let directory = agent_dir(home, agent).unwrap().join("skills");
        if !directory.exists() {
            continue;
        }
        if no_links(&directory).is_err() {
            errors.push(format!("{agent}: symbolic Skills directory is unavailable"));
            continue;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                errors.push(format!("{agent}: cannot read Skills directory"));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join("SKILL.md").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if valid_id(&name).is_err() {
                errors.push(format!("{agent}: unsupported skill directory name"));
                continue;
            }
            let row = rows.entry(name.clone()).or_insert_with(|| Skill {
                name: name.clone(),
                description: description(&path),
                paths: Vec::new(),
                source: None,
                managed: false,
                installed: BTreeMap::new(),
                owned: BTreeMap::new(),
            });
            row.paths.push(path.display().to_string());
            row.installed.insert(agent.into(), true);
            row.owned.insert(
                agent.into(),
                state
                    .skills
                    .get(&name)
                    .is_some_and(|skill| skill.targets.contains_key(agent)),
            );
        }
    }
    rows.into_values().collect()
}

fn skill_change(
    home: &Path,
    name: &str,
    source: Option<&str>,
    targets: &[String],
    operation: ResourceOperation,
) -> Result<()> {
    valid_id(name)?;
    for agent in targets {
        agent_dir(home, agent)?;
    }
    let mut state = registry(home)?;
    let importing = operation == ResourceOperation::Create;
    let remove = operation == ResourceOperation::Remove;
    if importing && state.skills.contains_key(name) {
        return Err("Skill identifier already managed; use Manage to edit it".into());
    }
    if !importing && !state.skills.contains_key(name) {
        return Err("Unmanaged source directories cannot be edited or removed".into());
    }
    let cache = store(home).join("skills").join(name);
    no_links(&cache)?;
    let source_path = if importing {
        let path = PathBuf::from(source.ok_or("Select a source directory")?);
        if !path.is_absolute() {
            return Err("Source must be an absolute directory path".into());
        }
        no_links(&path)?;
        if cache.starts_with(&path) || path.starts_with(store(home)) {
            return Err("Source overlaps managed storage".into());
        }
        path
    } else {
        cache.clone()
    };
    let content = files(&source_path)?;
    let hash = fingerprint(&content);
    let previous = state
        .skills
        .get(name)
        .map(|skill| skill.targets.clone())
        .unwrap_or_default();
    if let Some(managed) = state.skills.get(name) {
        let expected = managed
            .source_fingerprint
            .as_ref()
            .or_else(|| previous.values().next())
            .ok_or("Managed source fingerprint is unknown; no files were changed; import it under a new identifier")?;
        if *expected != hash || previous.values().any(|expected| *expected != hash) {
            return Err(
                "Managed source changed externally; import it under a new identifier".into(),
            );
        }
    }
    let mut additions = Vec::new();
    let mut removals = Vec::new();
    for agent in AGENTS {
        let path = agent_dir(home, agent)?.join("skills").join(name);
        no_links(&path)?;
        let wanted = !remove && targets.iter().any(|target| target == agent);
        if let Some(expected) = previous.get(agent) {
            if path.exists() && fingerprint(&files(&path)?) != *expected {
                return Err(format!(
                    "{agent}: managed copy changed externally; no files were removed"
                ));
            }
            if !wanted && path.exists() {
                removals.push(path.clone());
            }
            if wanted && !path.exists() {
                additions.push(path);
            }
        } else if wanted {
            if path.exists() {
                return Err(format!(
                    "{agent}: an unmanaged skill already occupies this name"
                ));
            }
            additions.push(path);
        }
    }
    if importing && cache.exists() {
        return Err("Managed source already exists".into());
    }
    if remove {
        removals.push(cache.clone());
    }
    for path in &removals {
        let backup = path.with_extension("ecpa-remove");
        no_links(&backup)?;
        if backup.exists() {
            return Err("Pending skill cleanup exists; no files were changed".into());
        }
    }
    let mut created = Vec::new();
    let mut removed = Vec::new();
    let outcome = (|| {
        if importing {
            copy_skill(&cache, &content)?;
            created.push(cache.clone());
        }
        for path in additions {
            copy_skill(&path, &content)?;
            created.push(path);
        }
        for path in removals {
            let backup = path.with_extension("ecpa-remove");
            publish_skill(&path, &backup)?;
            removed.push((path, backup));
        }
        if remove {
            state.skills.remove(name);
        } else {
            state.skills.insert(
                name.into(),
                ManagedSkill {
                    source_fingerprint: Some(hash.clone()),
                    source: state
                        .skills
                        .get(name)
                        .map(|skill| skill.source.clone())
                        .unwrap_or_else(|| source_path.display().to_string()),
                    targets: targets
                        .iter()
                        .map(|agent| (agent.clone(), hash.clone()))
                        .collect(),
                },
            );
        }
        save_registry(home, &state)
    })();
    if outcome.is_err() {
        let mut failed = false;
        for path in created {
            if files(&path).is_ok_and(|content| fingerprint(&content) == hash) {
                failed |= fs::remove_dir_all(path).is_err();
            } else {
                failed = true;
            }
        }
        for (path, backup) in removed.into_iter().rev() {
            failed |= publish_skill(&backup, &path).is_err();
        }
        if failed {
            return Err("Skill save failed and rollback was incomplete; retained .ecpa-remove directories contain prior copies".into());
        }
    } else {
        for (_, backup) in removed {
            fs::remove_dir_all(backup)
                .map_err(|_| "Changes saved; cleanup of a .ecpa-remove directory failed")?;
        }
    }
    outcome
}

fn config_path(home: &Path, agent: &str) -> Result<PathBuf> {
    match agent {
        "claude" => Ok(home.join(".claude.json")),
        "codex" => Ok(agent_dir(home, agent)?.join("config.toml")),
        "gemini" => Ok(agent_dir(home, agent)?.join("settings.json")),
        _ => Err("Unsupported MCP target".into()),
    }
}
type ConfigDocument = (PathBuf, Option<Vec<u8>>, Value);
fn config(home: &Path, agent: &str) -> Result<ConfigDocument> {
    let path = config_path(home, agent)?;
    let bytes = read(&path)?;
    let value = match &bytes {
        None => json!({}),
        Some(bytes) if agent == "codex" => {
            let text = std::str::from_utf8(bytes).map_err(|_| "Invalid TOML encoding")?;
            let value: toml::Value =
                toml::from_str(text).map_err(|_| "Invalid TOML; nothing was changed")?;
            serde_json::to_value(value).map_err(|_| "Cannot read TOML")?
        }
        Some(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| "Invalid JSON; nothing was changed")?
        }
    };
    if !value.is_object() {
        return Err("Configuration root must be an object".into());
    }
    let key = if agent == "codex" {
        "mcp_servers"
    } else {
        "mcpServers"
    };
    if value.get(key).is_some_and(|servers| !servers.is_object()) {
        return Err("MCP servers must be an object; nothing was changed".into());
    }
    if value
        .get(key)
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.values().any(|server| !server.is_object()))
    {
        return Err("MCP server entries must be objects; nothing was changed".into());
    }
    Ok((path, bytes, value))
}
fn normalize(agent: &str, mut spec: Value) -> Result<Value> {
    let object = spec.as_object_mut().ok_or("MCP server must be an object")?;
    if agent == "gemini" {
        if let Some(url) = object.remove("httpUrl") {
            object.insert("url".into(), url);
            object.insert("type".into(), json!("http"));
        } else if object.contains_key("url") {
            object.insert("type".into(), json!("sse"));
        }
    }
    if agent == "codex" {
        if let Some(headers) = object.remove("http_headers") {
            object.insert("headers".into(), headers);
        }
        object.remove("enabled");
    }
    if !object.contains_key("type") {
        object.insert(
            "type".into(),
            json!(if object.contains_key("url") {
                "http"
            } else {
                "stdio"
            }),
        );
    }
    Ok(spec)
}
fn validate(agent: &str, spec: &Value) -> Result<()> {
    agent_dir(Path::new("/"), agent)?;
    let object = spec.as_object().ok_or("MCP server must be an object")?;
    let transport = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or("Choose a transport")?;
    match transport {
        "stdio" => {
            if object
                .get("command")
                .and_then(Value::as_str)
                .is_none_or(|command| command.trim().is_empty())
            {
                return Err("stdio requires a command".into());
            }
            if object.contains_key("url") || object.contains_key("httpUrl") {
                return Err("stdio cannot include a URL".into());
            }
        }
        "http" | "sse" => {
            if agent == "codex" && transport == "sse" {
                return Err("Codex supports stdio and HTTP, not SSE".into());
            }
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .ok_or("HTTP/SSE requires a URL")?;
            let parsed = reqwest::Url::parse(url).map_err(|_| "Invalid MCP URL")?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err("Use an HTTP(S) URL without embedded credentials".into());
            }
            if object.contains_key("command") || object.contains_key("args") {
                return Err("HTTP/SSE cannot include command or args".into());
            }
        }
        _ => return Err("Unsupported MCP transport".into()),
    }
    if let Some(args) = object.get("args") {
        if !args
            .as_array()
            .is_some_and(|args| args.iter().all(Value::is_string))
        {
            return Err("args must be an array of strings".into());
        }
    }
    for field in ["env", "headers"] {
        if let Some(values) = object.get(field) {
            if !values
                .as_object()
                .is_some_and(|values| values.values().all(Value::is_string))
            {
                return Err(format!("{field} must contain string values"));
            }
        }
    }
    if object.contains_key("httpUrl")
        || object.contains_key("http_headers")
        || object.contains_key("enabled")
    {
        return Err("Use normalized type/url/headers fields and the target enable switch".into());
    }
    Ok(())
}
fn encode(agent: &str, mut spec: Value) -> Value {
    let object = spec.as_object_mut().unwrap();
    let transport = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    if agent == "gemini" {
        object.remove("type");
        if transport == "http" {
            if let Some(url) = object.remove("url") {
                object.insert("httpUrl".into(), url);
            }
        }
    }
    if agent == "codex" {
        object.remove("type");
        if let Some(headers) = object.remove("headers") {
            object.insert("http_headers".into(), headers);
        }
    }
    spec
}
fn serialize_config(
    agent: &str,
    original: Option<&[u8]>,
    value: &Value,
    id: &str,
) -> Result<Vec<u8>> {
    if agent != "codex" {
        return serde_json::to_vec_pretty(value).map_err(|_| "Cannot serialize MCP JSON".into());
    }
    let text =
        std::str::from_utf8(original.unwrap_or_default()).map_err(|_| "Invalid TOML encoding")?;
    let mut document = text
        .parse::<toml_edit::Document>()
        .map_err(|_| "Invalid TOML")?;
    if document.get("mcp_servers").is_none() {
        document["mcp_servers"] = toml_edit::table();
    }
    let servers = document["mcp_servers"]
        .as_table_like_mut()
        .ok_or("MCP servers must be a table")?;
    if let Some(spec) = value["mcp_servers"].get(id) {
        let table: toml::Value = serde_json::from_value(spec.clone())
            .map_err(|_| "Server contains values unsupported in TOML")?;
        let encoded = toml::to_string(&table).map_err(|_| "Cannot serialize MCP TOML")?;
        let entry = encoded
            .parse::<toml_edit::Document>()
            .map_err(|_| "Cannot encode server")?;
        servers.insert(id, toml_edit::Item::Table(entry.as_table().clone()));
    } else {
        servers.remove(id);
    }
    Ok(document.to_string().into_bytes())
}
fn mcp_change(
    home: &Path,
    id: &str,
    entries: &[McpEntry],
    operation: ResourceOperation,
    expected: &[McpEntry],
) -> Result<()> {
    mcp_change_with_reader(home, id, entries, operation, expected, config)
}
fn mcp_change_with_reader(
    home: &Path,
    id: &str,
    entries: &[McpEntry],
    operation: ResourceOperation,
    expected: &[McpEntry],
    mut load_config: impl FnMut(&Path, &str) -> Result<ConfigDocument>,
) -> Result<()> {
    valid_id(id)?;
    let registry_path = store(home).join("state.json");
    let registry_original = read(&registry_path)?;
    let mut state = parse_registry(registry_original.as_deref())?;
    let mut actual = Vec::new();
    let remove = operation == ResourceOperation::Remove;
    let mut writes = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        if entry.id != id || !seen.insert(&entry.agent) {
            return Err("Invalid or duplicate MCP target".into());
        }
        validate(&entry.agent, &entry.spec)?;
    }
    for agent in AGENTS {
        let selected = if remove {
            None
        } else {
            entries.iter().find(|entry| entry.agent == agent)
        };
        let (path, original, mut value) =
            load_config(home, agent).map_err(|error| format!("{agent}: {error}"))?;
        let key = if agent == "codex" {
            "mcp_servers"
        } else {
            "mcpServers"
        };
        if let Some(raw) = value.get(key).and_then(|map| map.get(id)) {
            actual.push(McpEntry {
                id: id.into(),
                agent: agent.into(),
                enabled: raw.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                spec: normalize(agent, raw.clone())?,
            });
        } else if let Some(spec) = state
            .disabled
            .get(agent)
            .and_then(|entries| entries.get(id))
        {
            actual.push(McpEntry {
                id: id.into(),
                agent: agent.into(),
                enabled: false,
                spec: spec.clone(),
            });
        }
        let had_live = value.get(key).and_then(|map| map.get(id)).is_some();
        state.disabled.entry(agent.into()).or_default().remove(id);
        if selected.is_none() && !had_live {
            continue;
        }
        if value.get(key).is_none() {
            value[key] = json!({});
        }
        let servers = value[key].as_object_mut().unwrap();
        if let Some(entry) = selected {
            if entry.enabled {
                servers.insert(id.into(), encode(agent, entry.spec.clone()));
            } else {
                servers.remove(id);
                state
                    .disabled
                    .entry(agent.into())
                    .or_default()
                    .insert(id.into(), entry.spec.clone());
            }
        } else {
            servers.remove(id);
        }
        let bytes = serialize_config(agent, original.as_deref(), &value, id)?;
        writes.push((path, original, bytes));
    }
    if operation == ResourceOperation::Create && !actual.is_empty() {
        return Err("Server identifier already exists; use Manage to edit it".into());
    }
    if operation != ResourceOperation::Create && actual.is_empty() {
        return Err("Server no longer exists; refresh before saving".into());
    }
    if serde_json::to_value(&actual).ok() != serde_json::to_value(expected).ok() {
        return Err("Server changed since it was opened; refresh before saving".into());
    }
    writes.push((
        registry_path,
        registry_original,
        serde_json::to_vec_pretty(&state).map_err(|_| "Cannot serialize registry")?,
    ));
    for (path, original, _) in &writes {
        if read(path)? != *original {
            return Err("Configuration changed while saving; retry".into());
        }
        if let Some(bytes) = original {
            atomic(&path.with_extension("ecpa-backup"), bytes)?;
        }
    }
    let mut completed: Vec<(PathBuf, Option<Vec<u8>>, Vec<u8>)> = Vec::new();
    for (path, original, bytes) in writes {
        let outcome = (|| {
            if read(&path)? != original {
                return Err("Configuration changed while saving; backups retained".into());
            }
            atomic(&path, &bytes)
        })();
        if let Err(error) = outcome {
            let mut rollback_failed = false;
            for (path, previous, written) in completed.into_iter().rev() {
                if read(&path).ok().flatten().as_deref() != Some(written.as_slice()) {
                    rollback_failed = true;
                    continue;
                }
                let outcome = match previous {
                    Some(bytes) => atomic(&path, &bytes),
                    None => fs::remove_file(path)
                        .map_err(|_| "Cannot roll back new configuration".into()),
                };
                rollback_failed |= outcome.is_err();
            }
            return Err(if rollback_failed {
                "Save failed and rollback was incomplete; restore .ecpa-backup files".into()
            } else {
                error
            });
        }
        completed.push((path, original, bytes));
    }
    Ok(())
}
fn snapshot(home: &Path) -> Result<Snapshot> {
    let state = registry(home)?;
    let mut errors = Vec::new();
    let skills = skills(home, &state, &mut errors);
    let mut mcp = Vec::new();
    for agent in AGENTS {
        match config(home, agent) {
            Ok((_, _, value)) => {
                let key = if agent == "codex" {
                    "mcp_servers"
                } else {
                    "mcpServers"
                };
                if let Some(servers) = value.get(key).and_then(Value::as_object) {
                    for (id, raw) in servers {
                        match normalize(agent, raw.clone()) {
                            Ok(spec) => mcp.push(McpEntry {
                                id: id.clone(),
                                agent: agent.into(),
                                enabled: raw
                                    .get("enabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                                spec,
                            }),
                            Err(error) => errors.push(format!("{agent}: {error}")),
                        }
                    }
                }
                if let Some(disabled) = state.disabled.get(agent) {
                    for (id, spec) in disabled {
                        if !mcp
                            .iter()
                            .any(|entry| entry.agent == agent && entry.id == *id)
                        {
                            mcp.push(McpEntry {
                                id: id.clone(),
                                agent: agent.into(),
                                enabled: false,
                                spec: spec.clone(),
                            });
                        }
                    }
                }
            }
            Err(error) => errors.push(format!("{agent}: {error}")),
        }
    }
    Ok(Snapshot {
        skills,
        mcp,
        errors,
    })
}
fn home(app: &tauri::AppHandle) -> Result<PathBuf> {
    app.path()
        .home_dir()
        .map_err(|_| "Cannot resolve home directory".into())
}
#[tauri::command]
pub fn get_agent_resources(app: tauri::AppHandle) -> Result<Snapshot> {
    let _guard = LOCK.lock().map_err(|_| "Resource operation unavailable")?;
    snapshot(&home(&app)?)
}
#[tauri::command]
pub fn update_agent_skill(
    app: tauri::AppHandle,
    name: String,
    source: Option<String>,
    targets: Vec<String>,
    operation: ResourceOperation,
) -> Result<()> {
    let _guard = LOCK.lock().map_err(|_| "Resource operation unavailable")?;
    skill_change(&home(&app)?, &name, source.as_deref(), &targets, operation)
}
#[tauri::command]
pub fn update_agent_mcp(
    app: tauri::AppHandle,
    id: String,
    entries: Vec<McpEntry>,
    operation: ResourceOperation,
    expected: Vec<McpEntry>,
) -> Result<()> {
    let _guard = LOCK.lock().map_err(|_| "Resource operation unavailable")?;
    mcp_change(&home(&app)?, &id, &entries, operation, &expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ecpa-resources-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
        fn write(&self, relative: &str, text: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }
        fn skill(&self) -> PathBuf {
            self.write(
                "source/SKILL.md",
                "---\nname: fixture\ndescription: Local fixture description\n---\nInstructions",
            );
            self.write("source/scripts/check.sh", "echo fixture");
            self.0.join("source")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn targets() -> Vec<String> {
        AGENTS.iter().map(|agent| agent.to_string()).collect()
    }
    fn entries(id: &str) -> Vec<McpEntry> {
        AGENTS.iter().map(|agent| McpEntry { id: id.into(), agent: agent.to_string(), enabled: true, spec: json!({"type":"stdio", "command":"never-executed-fixture", "args":["--local"], "env":{"TOKEN":"fixture"}, "unknown":{"keep":true}}) }).collect()
    }
    fn expected(fixture: &Fixture, id: &str) -> Vec<McpEntry> {
        snapshot(&fixture.0)
            .unwrap()
            .mcp
            .into_iter()
            .filter(|entry| entry.id == id)
            .collect()
    }

    fn fixture_tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn visit(root: &Path, directory: &Path, tree: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    tree.insert(relative, None);
                    visit(root, &path, tree);
                } else {
                    tree.insert(relative, Some(fs::read(path).unwrap()));
                }
            }
        }
        let mut tree = BTreeMap::new();
        visit(root, root, &mut tree);
        tree
    }

    #[test]
    fn duplicate_skill_import_preserves_sources_registry_and_installed_copies() {
        let fixture = Fixture::new();
        let source_a = fixture.skill();
        fixture.write("source-b/SKILL.md", "Different source B instructions");
        fixture.write("source-b/scripts/check.sh", "echo source-b");
        let source_b = fixture.0.join("source-b");
        skill_change(
            &fixture.0,
            "fixture",
            source_a.to_str(),
            &["claude".into()],
            ResourceOperation::Create,
        )
        .unwrap();
        let before = fixture_tree(&fixture.0);
        for source in [source_b.to_str(), None] {
            let error = skill_change(
                &fixture.0,
                "fixture",
                source,
                &["codex".into(), "gemini".into()],
                ResourceOperation::Create,
            )
            .unwrap_err();
            assert!(error.contains("already managed"), "{error}");
            assert_eq!(fixture_tree(&fixture.0), before);
        }
        assert!(skill_change(
            &fixture.0,
            "missing",
            source_b.to_str(),
            &[],
            ResourceOperation::Edit
        )
        .is_err());
        assert_eq!(fixture_tree(&fixture.0), before);
        skill_change(
            &fixture.0,
            "fixture",
            None,
            &["gemini".into()],
            ResourceOperation::Edit,
        )
        .unwrap();
        assert!(!fixture.0.join(".claude/skills/fixture").exists());
        assert_eq!(
            fingerprint(&files(&fixture.0.join(".gemini/skills/fixture")).unwrap()),
            fingerprint(&files(&source_a).unwrap())
        );
    }

    #[test]
    fn skill_import_preserves_empty_directories_and_guards_layout_changes() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        let legacy_hash = fingerprint(&files(&source).unwrap());
        fs::create_dir_all(source.join("output/nested-empty")).unwrap();
        fs::create_dir(source.join("cache")).unwrap();
        let source_tree = fixture_tree(&source);
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create,
        )
        .unwrap();
        let cache = store(&fixture.0).join("skills/fixture");
        assert_eq!(fixture_tree(&cache), source_tree);
        assert_ne!(fingerprint(&files(&cache).unwrap()), legacy_hash);
        for agent in AGENTS {
            assert_eq!(
                fixture_tree(&agent_dir(&fixture.0, agent).unwrap().join("skills/fixture")),
                source_tree
            );
        }
        skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Edit).unwrap();
        fs::remove_dir(cache.join("output/nested-empty")).unwrap();
        let before = fixture_tree(&fixture.0);
        assert!(
            skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Remove)
                .unwrap_err()
                .contains("changed externally")
        );
        assert_eq!(fixture_tree(&fixture.0), before);
        assert_eq!(fixture_tree(&source), source_tree);
    }

    #[test]
    fn mcp_external_change_during_config_read_is_never_overwritten() {
        for change_after_read in [false, true] {
            let fixture = Fixture::new();
            mcp_change(
                &fixture.0,
                "docs",
                &entries("docs"),
                ResourceOperation::Create,
                &[],
            )
            .unwrap();
            let previous = expected(&fixture, "docs");
            let mut reads = BTreeMap::new();
            let mut external_tree = None;
            let error = mcp_change_with_reader(&fixture.0, "docs", &entries("docs"), ResourceOperation::Edit, &previous, |home, agent| {
                *reads.entry(agent.to_string()).or_insert(0) += 1;
                let captured = if change_after_read { Some(config(home, agent)?) } else { None };
                if agent == "claude" {
                    fixture.write(".claude.json", r#"{"mcpServers":{"docs":{"command":"external-new-server"}},"external":true}"#);
                    external_tree = Some(fixture_tree(&fixture.0));
                }
                match captured { Some(value) => Ok(value), None => config(home, agent) }
            }).unwrap_err();
            assert!(error.contains("changed"), "{error}");
            assert_eq!(reads.values().copied().collect::<Vec<_>>(), vec![1, 1, 1]);
            assert_eq!(fixture_tree(&fixture.0), external_tree.unwrap());
        }
    }

    #[test]
    fn duplicate_mcp_create_preserves_active_and_disabled_entries_without_writes() {
        let fixture = Fixture::new();
        for enabled in [true, false] {
            let id = if enabled { "live" } else { "disabled" };
            let mut original = entries(id);
            for entry in &mut original {
                entry.enabled = enabled;
            }
            mcp_change(&fixture.0, id, &original, ResourceOperation::Create, &[]).unwrap();
            let previous = expected(&fixture, id);
            let before = fixture_tree(&fixture.0);
            for expected in [&[][..], previous.as_slice()] {
                let error = mcp_change(&fixture.0, id, &[], ResourceOperation::Create, expected)
                    .unwrap_err();
                assert!(error.contains("already exists"), "{error}");
                assert_eq!(fixture_tree(&fixture.0), before);
            }
            mcp_change(
                &fixture.0,
                id,
                &original,
                ResourceOperation::Edit,
                &previous,
            )
            .unwrap();
        }
        let before = fixture_tree(&fixture.0);
        assert!(mcp_change(
            &fixture.0,
            "missing",
            &entries("missing"),
            ResourceOperation::Edit,
            &[]
        )
        .is_err());
        assert_eq!(fixture_tree(&fixture.0), before);
    }

    #[test]
    fn discovery_import_sync_uninstall_never_delete_source() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        fixture.write(
            ".claude/skills/existing/SKILL.md",
            "---\ndescription: Existing skill\n---\nText",
        );
        let found = snapshot(&fixture.0).unwrap();
        assert_eq!(found.skills[0].name, "existing");
        assert_eq!(found.skills[0].description, "Existing skill");
        assert!(!found.skills[0].managed);
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create,
        )
        .unwrap();
        let found = snapshot(&fixture.0).unwrap();
        let imported = found
            .skills
            .iter()
            .find(|skill| skill.name == "fixture")
            .unwrap();
        assert_eq!(imported.description, "Local fixture description");
        assert_eq!(imported.installed.len(), 3);
        skill_change(
            &fixture.0,
            "fixture",
            None,
            &["codex".into()],
            ResourceOperation::Edit,
        )
        .unwrap();
        assert!(!fixture.0.join(".claude/skills/fixture").exists());
        assert!(fixture.0.join(".codex/skills/fixture/SKILL.md").exists());
        skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Remove).unwrap();
        assert!(source.join("SKILL.md").exists());
        assert!(!store(&fixture.0).join("skills/fixture").exists());
        assert!(fixture.0.join(".claude/skills/existing/SKILL.md").exists());
    }
    #[test]
    fn unmanaged_and_modified_source_copies_cannot_be_removed() {
        let fixture = Fixture::new();
        let original = fixture.skill();
        let unmanaged = fixture.write(".claude/skills/unmanaged/SKILL.md", "Keep original");
        assert!(skill_change(
            &fixture.0,
            "unmanaged",
            None,
            &[],
            ResourceOperation::Remove
        )
        .is_err());
        assert_eq!(fs::read_to_string(unmanaged).unwrap(), "Keep original");
        skill_change(
            &fixture.0,
            "fixture",
            original.to_str(),
            &targets(),
            ResourceOperation::Create,
        )
        .unwrap();
        let cached = store(&fixture.0).join("skills/fixture/SKILL.md");
        fs::write(&cached, "Edited private source").unwrap();
        assert!(skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Remove).is_err());
        assert_eq!(fs::read_to_string(cached).unwrap(), "Edited private source");
        assert!(original.join("SKILL.md").exists());
        for agent in AGENTS {
            assert!(agent_dir(&fixture.0, agent)
                .unwrap()
                .join("skills/fixture/SKILL.md")
                .exists());
        }
    }
    #[cfg(unix)]
    #[test]
    fn mcp_symlink_config_is_rejected_without_writing_other_targets() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let config = outside.write("settings.json", r#"{"keep":"fixture"}"#);
        fs::create_dir(fixture.0.join(".gemini")).unwrap();
        symlink(&config, fixture.0.join(".gemini/settings.json")).unwrap();
        assert!(mcp_change(
            &fixture.0,
            "docs",
            &entries("docs"),
            ResourceOperation::Create,
            &[]
        )
        .is_err());
        assert_eq!(fs::read_to_string(config).unwrap(), r#"{"keep":"fixture"}"#);
        assert!(!fixture.0.join(".claude.json").exists());
        assert!(!fixture.0.join(".codex/config.toml").exists());
        assert!(!store(&fixture.0).exists());
    }
    fn assert_source_operations_rejected(fixture: &Fixture, message: &str) {
        let cache = store(&fixture.0).join("skills/fixture");
        let original_hash = fingerprint(&files(&fixture.0.join("source")).unwrap());
        let cached_hash = fingerprint(&files(&cache).unwrap());
        let registry_path = store(&fixture.0).join("state.json");
        let before = fs::read(&registry_path).unwrap();
        for (selected, remove) in [(Vec::new(), true), (targets(), false), (Vec::new(), false)] {
            let error = skill_change(
                &fixture.0,
                "fixture",
                None,
                &selected,
                if remove {
                    ResourceOperation::Remove
                } else {
                    ResourceOperation::Edit
                },
            )
            .unwrap_err();
            assert!(error.contains(message), "{error}");
            assert_eq!(fingerprint(&files(&cache).unwrap()), cached_hash);
            assert_eq!(
                fingerprint(&files(&fixture.0.join("source")).unwrap()),
                original_hash
            );
            assert_eq!(fs::read(&registry_path).unwrap(), before);
            assert!(!cache.with_extension("ecpa-remove").exists());
            for agent in AGENTS {
                assert!(!agent_dir(&fixture.0, agent)
                    .unwrap()
                    .join("skills/fixture")
                    .exists());
            }
        }
    }
    #[test]
    fn zero_target_import_rejects_modified_source_on_remove_and_reinstall() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        let original_hash = fingerprint(&files(&source).unwrap());
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &[],
            ResourceOperation::Create,
        )
        .unwrap();
        assert_eq!(
            registry(&fixture.0).unwrap().skills["fixture"].source_fingerprint,
            Some(original_hash.clone())
        );
        fs::write(
            store(&fixture.0).join("skills/fixture/SKILL.md"),
            "Externally edited cached source",
        )
        .unwrap();
        assert_source_operations_rejected(&fixture, "Managed source changed externally");
        assert_eq!(fingerprint(&files(&source).unwrap()), original_hash);
    }
    #[test]
    fn deselecting_all_targets_retains_source_modification_guard() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        let original_hash = fingerprint(&files(&source).unwrap());
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create,
        )
        .unwrap();
        skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Edit).unwrap();
        let state = registry(&fixture.0).unwrap();
        assert!(state.skills["fixture"].targets.is_empty());
        assert_eq!(
            state.skills["fixture"].source_fingerprint,
            Some(original_hash.clone())
        );
        fs::write(
            store(&fixture.0).join("skills/fixture/scripts/check.sh"),
            "Externally edited script",
        )
        .unwrap();
        assert_source_operations_rejected(&fixture, "Managed source changed externally");
        assert_eq!(fingerprint(&files(&source).unwrap()), original_hash);
    }
    fn remove_source_fingerprint(fixture: &Fixture) {
        let path = store(&fixture.0).join("state.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        state["skills"]["fixture"]
            .as_object_mut()
            .unwrap()
            .remove("source_fingerprint");
        fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
    }
    #[test]
    fn legacy_target_fingerprint_migrates_before_deselecting_all_targets() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        let original_hash = fingerprint(&files(&source).unwrap());
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create,
        )
        .unwrap();
        remove_source_fingerprint(&fixture);
        skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Edit).unwrap();
        assert_eq!(
            registry(&fixture.0).unwrap().skills["fixture"].source_fingerprint,
            Some(original_hash.clone())
        );
        fs::write(
            store(&fixture.0).join("skills/fixture/SKILL.md"),
            "Edited after migration",
        )
        .unwrap();
        assert_source_operations_rejected(&fixture, "Managed source changed externally");
        assert_eq!(fingerprint(&files(&source).unwrap()), original_hash);
    }
    #[test]
    fn legacy_zero_target_source_without_fingerprint_is_never_adopted_or_deleted() {
        for edited in [false, true] {
            let fixture = Fixture::new();
            let source = fixture.skill();
            let original_hash = fingerprint(&files(&source).unwrap());
            skill_change(
                &fixture.0,
                "fixture",
                source.to_str(),
                &[],
                ResourceOperation::Create,
            )
            .unwrap();
            remove_source_fingerprint(&fixture);
            if edited {
                fs::write(
                    store(&fixture.0).join("skills/fixture/SKILL.md"),
                    "Unknown cached edits",
                )
                .unwrap();
            }
            assert_source_operations_rejected(&fixture, "Managed source fingerprint is unknown");
            assert_eq!(fingerprint(&files(&source).unwrap()), original_hash);
        }
    }
    #[test]
    fn skill_conflicts_and_modified_targets_are_not_overwritten() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        let occupied = fixture.write(".gemini/skills/fixture/SKILL.md", "Do not overwrite");
        assert!(skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create
        )
        .is_err());
        assert!(!fixture.0.join(".claude/skills/fixture").exists());
        assert!(!store(&fixture.0).exists());
        assert_eq!(fs::read_to_string(occupied).unwrap(), "Do not overwrite");
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &["codex".into()],
            ResourceOperation::Create,
        )
        .unwrap();
        let modified = fixture.write(".codex/skills/fixture/SKILL.md", "User modification");
        assert!(skill_change(&fixture.0, "fixture", None, &[], ResourceOperation::Remove).is_err());
        assert_eq!(fs::read_to_string(modified).unwrap(), "User modification");
    }
    #[test]
    fn skill_traversal_invalid_source_and_unsupported_targets() {
        let fixture = Fixture::new();
        let source = fixture.skill();
        for name in ["../escape", "/absolute", "..", "", "a/b"] {
            assert!(skill_change(
                &fixture.0,
                name,
                source.to_str(),
                &targets(),
                ResourceOperation::Create
            )
            .is_err());
        }
        assert!(skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &["unsupported".into()],
            ResourceOperation::Create
        )
        .is_err());
        assert!(skill_change(
            &fixture.0,
            "fixture",
            Some("relative"),
            &[],
            ResourceOperation::Create
        )
        .is_err());
        assert!(skill_change(
            &fixture.0,
            "fixture",
            Some(fixture.0.to_str().unwrap()),
            &[],
            ResourceOperation::Create
        )
        .is_err());
        assert!(skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &[],
            ResourceOperation::Remove
        )
        .is_err());
        assert!(!store(&fixture.0).exists());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_rejected_and_script_execute_bit_preserved() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = Fixture::new();
        let source = fixture.skill();
        fs::set_permissions(
            source.join("scripts/check.sh"),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        symlink(source.join("SKILL.md"), source.join("linked")).unwrap();
        assert!(skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create
        )
        .is_err());
        fs::remove_file(source.join("linked")).unwrap();
        skill_change(
            &fixture.0,
            "fixture",
            source.to_str(),
            &targets(),
            ResourceOperation::Create,
        )
        .unwrap();
        assert_eq!(
            fs::metadata(fixture.0.join(".codex/skills/fixture/scripts/check.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        let outside = Fixture::new();
        symlink(&outside.0, fixture.0.join(".redirect")).unwrap();
        assert!(atomic(&fixture.0.join(".redirect/config"), b"secret").is_err());
        assert!(!outside.0.join("config").exists());
    }
    #[test]
    fn mcp_roundtrip_preserves_unrelated_settings_and_comments() {
        let fixture = Fixture::new();
        let claude = fixture.write(".claude.json", r#"{"theme":"dark","projects":{"local":{"trust":true}},"mcpServers":{"other":{"command":"other"}}}"#);
        let codex = fixture.write(".codex/config.toml", "# keep this comment\nmodel = 'fixture'\n[mcp_servers.other]\ncommand = 'other'\n[unknown]\nretain = 42\n");
        let gemini = fixture.write(
            ".gemini/settings.json",
            r#"{"security":{"trust":true},"mcpServers":{"other":{"command":"other"}}}"#,
        );
        let original = fs::read(&claude).unwrap();
        mcp_change(
            &fixture.0,
            "docs",
            &entries("docs"),
            ResourceOperation::Create,
            &[],
        )
        .unwrap();
        assert_eq!(expected(&fixture, "docs").len(), 3);
        let claude_value: Value = serde_json::from_slice(&fs::read(&claude).unwrap()).unwrap();
        assert_eq!(claude_value["projects"]["local"]["trust"], true);
        assert_eq!(claude_value["mcpServers"]["other"]["command"], "other");
        assert_eq!(
            fs::read(claude.with_extension("ecpa-backup")).unwrap(),
            original
        );
        let codex_text = fs::read_to_string(&codex).unwrap();
        assert!(codex_text.contains("# keep this comment"));
        assert!(codex_text.contains("retain = 42"));
        let gemini_value: Value = serde_json::from_slice(&fs::read(gemini).unwrap()).unwrap();
        assert_eq!(gemini_value["security"]["trust"], true);
        let previous = expected(&fixture, "docs");
        mcp_change(
            &fixture.0,
            "docs",
            &[],
            ResourceOperation::Remove,
            &previous,
        )
        .unwrap();
        assert!(expected(&fixture, "docs").is_empty());
        assert_eq!(expected(&fixture, "other").len(), 3);
    }
    #[test]
    fn per_target_disable_survives_reload_and_enable_restores_secrets() {
        let fixture = Fixture::new();
        mcp_change(
            &fixture.0,
            "docs",
            &entries("docs"),
            ResourceOperation::Create,
            &[],
        )
        .unwrap();
        let previous = expected(&fixture, "docs");
        let mut changed = previous.clone();
        for entry in &mut changed {
            entry.enabled = false;
        }
        mcp_change(
            &fixture.0,
            "docs",
            &changed,
            ResourceOperation::Edit,
            &previous,
        )
        .unwrap();
        let disabled = expected(&fixture, "docs");
        assert_eq!(disabled.len(), 3);
        assert!(disabled.iter().all(|entry| !entry.enabled));
        for agent in AGENTS {
            let (_, _, value) = config(&fixture.0, agent).unwrap();
            assert!(value[if agent == "codex" {
                "mcp_servers"
            } else {
                "mcpServers"
            }]
            .get("docs")
            .is_none());
        }
        let mut enabled = disabled.clone();
        enabled[0].enabled = true;
        mcp_change(
            &fixture.0,
            "docs",
            &enabled,
            ResourceOperation::Edit,
            &disabled,
        )
        .unwrap();
        let reloaded = expected(&fixture, "docs");
        assert_eq!(reloaded.iter().filter(|entry| entry.enabled).count(), 1);
        assert_eq!(reloaded[0].spec["env"]["TOKEN"], "fixture");
        mcp_change(
            &fixture.0,
            "docs",
            &[],
            ResourceOperation::Remove,
            &reloaded,
        )
        .unwrap();
        assert!(expected(&fixture, "docs").is_empty());
    }
    #[test]
    fn http_headers_gemini_field_mapping_and_sse_capabilities() {
        let fixture = Fixture::new();
        let mut remote = entries("remote");
        for entry in &mut remote {
            entry.spec = json!({"type":"http","url":"http://localhost:1234/mcp","headers":{"Authorization":"fixture"}});
        }
        mcp_change(
            &fixture.0,
            "remote",
            &remote,
            ResourceOperation::Create,
            &[],
        )
        .unwrap();
        let (_, _, gemini) = config(&fixture.0, "gemini").unwrap();
        assert_eq!(
            gemini["mcpServers"]["remote"]["httpUrl"],
            "http://localhost:1234/mcp"
        );
        let (_, _, codex) = config(&fixture.0, "codex").unwrap();
        assert_eq!(
            codex["mcp_servers"]["remote"]["http_headers"]["Authorization"],
            "fixture"
        );
        let sse = json!({"type":"sse","url":"https://example.invalid/events"});
        assert!(validate("codex", &sse).is_err());
        assert!(validate("gemini", &sse).is_ok());
        assert!(validate("claude", &sse).is_ok());
        assert!(validate(
            "claude",
            &json!({"type":"http","url":"https://user:secret@example.invalid"})
        )
        .is_err());
    }
    #[test]
    fn malformed_configs_never_write_any_target() {
        for (path, text) in [
            (".gemini/settings.json", "{"),
            (".codex/config.toml", "model = ["),
            (".claude.json", "[]"),
            (".claude.json", r#"{"mcpServers":[]}"#),
            (".claude.json", r#"{"mcpServers":{"broken":false}}"#),
        ] {
            let fixture = Fixture::new();
            let bad = fixture.write(path, text);
            let before = fs::read(&bad).unwrap();
            assert!(!snapshot(&fixture.0).unwrap().errors.is_empty());
            assert!(mcp_change(
                &fixture.0,
                "docs",
                &entries("docs"),
                ResourceOperation::Create,
                &[]
            )
            .is_err());
            assert_eq!(fs::read(bad).unwrap(), before);
            assert!(!store(&fixture.0).join("state.json").exists());
        }
    }
    #[test]
    fn stale_editor_conflicts_invalid_json_and_unsupported_transport_do_not_write() {
        let fixture = Fixture::new();
        mcp_change(
            &fixture.0,
            "docs",
            &entries("docs"),
            ResourceOperation::Create,
            &[],
        )
        .unwrap();
        let stale = expected(&fixture, "docs");
        fixture.write(
            ".claude.json",
            r#"{"mcpServers":{"docs":{"type":"stdio","command":"changed"}}}"#,
        );
        assert!(mcp_change(
            &fixture.0,
            "docs",
            &entries("docs"),
            ResourceOperation::Edit,
            &stale
        )
        .is_err());
        let before = fs::read(fixture.0.join(".claude.json")).unwrap();
        let mut invalid = entries("new");
        invalid[1].spec = json!({"type":"sse","url":"http://localhost/events"});
        assert!(mcp_change(&fixture.0, "new", &invalid, ResourceOperation::Create, &[]).is_err());
        invalid[1].spec = json!({"type":"stdio","command":"fixture","env":{"TOKEN":false}});
        assert!(mcp_change(&fixture.0, "new", &invalid, ResourceOperation::Create, &[]).is_err());
        assert_eq!(fs::read(fixture.0.join(".claude.json")).unwrap(), before);
    }
    #[test]
    fn write_failure_rolls_back_previously_written_targets() {
        let fixture = Fixture::new();
        let claude = fixture.write(".claude.json", "{\"unknown\":true}");
        fixture.write(".gemini/settings.ecpa-tmp", "occupied temporary file");
        assert!(mcp_change(
            &fixture.0,
            "docs",
            &entries("docs"),
            ResourceOperation::Create,
            &[]
        )
        .is_err());
        assert_eq!(fs::read_to_string(claude).unwrap(), "{\"unknown\":true}");
        assert!(!fixture.0.join(".codex/config.toml").exists());
        assert!(!store(&fixture.0).join("state.json").exists());
    }
}
