use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

/// A discovered MCP server configuration.
///
/// `env` values are SECRETS: the field is `skip_serializing` because this
/// struct is embedded in the `listMcpServers` RPC response, and no log
/// statement may format it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub server_type: String, // "stdio", "sse", "http"
    pub url: Option<String>,
    pub disabled: bool,
    pub source: String, // "user", "project", "local", "plugin:<name>"
    pub config_path: String,
    #[serde(default, skip_serializing)]
    pub env: HashMap<String, String>,
    /// Per-server HTTP headers for url-based transports. Values are secrets
    /// (Authorization tokens): skipped in serialization, never logged.
    #[serde(default, skip_serializing)]
    pub headers: HashMap<String, String>,
}

/// MCP config file format (mcp.json)
#[derive(Debug, Deserialize)]
struct McpJsonConfig {
    #[serde(rename = "mcpServers", default)]
    mcp_servers: HashMap<String, McpServerEntry>,
}

#[derive(Debug, Deserialize)]
struct McpServerEntry {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    disabled: bool,
    url: Option<String>,
    #[serde(rename = "type")]
    server_type: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    headers: HashMap<String, String>,
}

/// Discover all MCP server configurations from standard locations.
/// Reads from (first definition of a name wins):
///   - ~/.baoclaw/mcp.json (user scope)
///   - ~/.baoclaw/plugins/*/mcp.json (user plugins)
///   - .baoclaw/mcp.json in cwd (project scope)
///   - .baoclaw/plugins/*/mcp.json in cwd (project plugins)
///   - .baoclaw/mcp.local.json in cwd (local scope, gitignored)
pub async fn discover_mcp_servers(cwd: &Path) -> Vec<McpServerInfo> {
    let home = dirs_path();
    discover_mcp_servers_in(home.as_deref(), cwd).await
}

/// Hermetic twin of [`discover_mcp_servers`] with the home directory
/// injected (tests pass a tempdir; production passes `$HOME`).
pub async fn discover_mcp_servers_in(home: Option<&Path>, cwd: &Path) -> Vec<McpServerInfo> {
    let mut servers = Vec::new();

    // User-level config: ~/.baoclaw/mcp.json
    if let Some(home) = home {
        let user_config = home.join(".baoclaw").join("mcp.json");
        servers.extend(load_mcp_config(&user_config, "user").await);

        // Plugin MCP configs: ~/.baoclaw/plugins/*/mcp.json
        servers.extend(scan_plugin_mcp(&home.join(".baoclaw").join("plugins")).await);
    }

    // Project-level config: <cwd>/.baoclaw/mcp.json
    let project_config = cwd.join(".baoclaw").join("mcp.json");
    servers.extend(load_mcp_config(&project_config, "project").await);

    // Project plugin MCP configs: <cwd>/.baoclaw/plugins/*/mcp.json
    servers.extend(scan_plugin_mcp(&cwd.join(".baoclaw").join("plugins")).await);

    // Local config (gitignored): <cwd>/.baoclaw/mcp.local.json
    let local_config = cwd.join(".baoclaw").join("mcp.local.json");
    servers.extend(load_mcp_config(&local_config, "local").await);

    // Dedup first-wins by name (source order above is deterministic); a
    // name defined in user scope shadows the same name in project scope.
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for server in servers {
        let key = server.name.to_lowercase();
        if seen.insert(key) {
            deduped.push(server);
        } else {
            eprintln!(
                "[mcp] duplicate server '{}' skipped (first definition wins)",
                server.name
            );
        }
    }
    deduped
}

/// Scan all plugins in a plugins directory for mcp.json configs.
async fn scan_plugin_mcp(plugins_dir: &Path) -> Vec<McpServerInfo> {
    let mut servers = Vec::new();
    let Ok(mut entries) = fs::read_dir(plugins_dir).await else {
        return servers;
    };
    // read_dir order is OS-dependent; sort so dedup-first-wins (and the
    // discovered list) is deterministic across restarts.
    let mut plugin_dirs = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            plugin_dirs.push(entry.path());
        }
    }
    plugin_dirs.sort();
    for dir in plugin_dirs {
        let plugin_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mcp_config = dir.join("mcp.json");
        let source = format!("plugin:{}", plugin_name);
        servers.extend(load_mcp_config(&mcp_config, &source).await);
    }
    servers
}

/// A missing file is normal (silent); a file that EXISTS but fails to parse
/// is a misconfiguration the operator must see.
async fn load_mcp_config(path: &Path, source: &str) -> Vec<McpServerInfo> {
    let content = match fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!("[mcp] WARNING: cannot read {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    let config: McpJsonConfig = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[mcp] WARNING: failed to parse {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    let config_path = path.to_string_lossy().to_string();

    config
        .mcp_servers
        .into_iter()
        .map(|(name, entry)| {
            let server_type = entry.server_type.unwrap_or_else(|| {
                if entry.url.is_some() {
                    "sse".to_string()
                } else {
                    "stdio".to_string()
                }
            });

            McpServerInfo {
                name,
                command: entry.command,
                args: entry.args,
                server_type,
                url: entry.url,
                disabled: entry.disabled,
                source: source.to_string(),
                config_path: config_path.clone(),
                env: entry.env,
                headers: entry.headers,
            }
        })
        .collect()
}

fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}
