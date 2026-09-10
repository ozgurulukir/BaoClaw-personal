#[cfg(test)]
mod tests {
    use super::super::mcp_config::*;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_discover_mcp_servers_empty() {
        let home = tempdir().unwrap();
        let dir = tempdir().unwrap();
        let servers = discover_mcp_servers_in(Some(home.path()), dir.path()).await;
        assert!(
            servers.is_empty(),
            "empty roots produced servers: {servers:?}"
        );
    }

    #[tokio::test]
    async fn test_discover_mcp_servers_project() {
        let dir = tempdir().unwrap();
        let baoclaw_dir = dir.path().join(".baoclaw");
        fs::create_dir_all(&baoclaw_dir).await.unwrap();

        let config_json = r#"{
            "mcpServers": {
                "test-server": {
                    "command": "node",
                    "args": ["index.js"],
                    "disabled": false,
                    "type": "stdio"
                }
            }
        }"#;
        fs::write(baoclaw_dir.join("mcp.json"), config_json)
            .await
            .unwrap();

        let home = tempdir().unwrap();
        let servers = discover_mcp_servers_in(Some(home.path()), dir.path()).await;
        assert_eq!(servers.len(), 1, "exactly the project server: {servers:?}");
        let s = &servers[0];
        assert_eq!(s.name, "test-server");
        assert_eq!(s.command.as_deref(), Some("node"));
        assert_eq!(s.args, vec!["index.js"]);
        assert_eq!(s.server_type, "stdio");
        assert_eq!(s.source, "project");
    }

    #[tokio::test]
    async fn test_discover_mcp_servers_local() {
        let dir = tempdir().unwrap();
        let baoclaw_dir = dir.path().join(".baoclaw");
        fs::create_dir_all(&baoclaw_dir).await.unwrap();

        let config_json = r#"{
            "mcpServers": {
                "local-server": {
                    "url": "http://localhost:8080/sse",
                    "type": "sse"
                }
            }
        }"#;
        fs::write(baoclaw_dir.join("mcp.local.json"), config_json)
            .await
            .unwrap();

        let home = tempdir().unwrap();
        let servers = discover_mcp_servers_in(Some(home.path()), dir.path()).await;
        assert_eq!(servers.len(), 1, "exactly the local server: {servers:?}");
        let s = &servers[0];
        assert_eq!(s.name, "local-server");
        assert_eq!(s.url.as_deref(), Some("http://localhost:8080/sse"));
        assert_eq!(s.server_type, "sse");
        assert_eq!(s.source, "local");
    }

    /// Hermetic seam: with an empty temp home, discovery sees ONLY the
    /// injected roots — the real $HOME is never touched.
    #[tokio::test]
    async fn test_discover_injected_home_is_hermetic() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();

        let servers = discover_mcp_servers_in(Some(home.path()), project.path()).await;
        assert!(servers.is_empty(), "expected no servers, got {servers:?}");

        fs::create_dir_all(home.path().join(".baoclaw"))
            .await
            .unwrap();
        fs::write(
            home.path().join(".baoclaw").join("mcp.json"),
            r#"{"mcpServers": {"home-srv": {"command": "bin"}}}"#,
        )
        .await
        .unwrap();
        let servers = discover_mcp_servers_in(Some(home.path()), project.path()).await;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "home-srv");
        assert_eq!(servers[0].source, "user");
    }

    #[tokio::test]
    async fn test_env_field_parsed_but_never_serialized() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".baoclaw"))
            .await
            .unwrap();
        fs::write(
            home.path().join(".baoclaw").join("mcp.json"),
            r#"{"mcpServers": {"with-env": {"command": "bin", "env": {"SECRET_TOKEN": "abc123"}}}}"#,
        )
        .await
        .unwrap();

        let servers = discover_mcp_servers_in(Some(home.path()), project.path()).await;
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].env.get("SECRET_TOKEN").map(String::as_str),
            Some("abc123")
        );
        // The RPC response serializes McpServerInfo directly: env must not
        // appear in it or the secret would leak to every client.
        let wire = serde_json::to_value(&servers[0]).unwrap();
        assert!(wire.get("env").is_none());
    }

    #[tokio::test]
    async fn test_duplicate_name_first_source_wins() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".baoclaw"))
            .await
            .unwrap();
        fs::create_dir_all(project.path().join(".baoclaw"))
            .await
            .unwrap();
        fs::write(
            home.path().join(".baoclaw").join("mcp.json"),
            r#"{"mcpServers": {"dup": {"command": "home-bin"}}}"#,
        )
        .await
        .unwrap();
        fs::write(
            project.path().join(".baoclaw").join("mcp.json"),
            r#"{"mcpServers": {"dup": {"command": "project-bin"}}}"#,
        )
        .await
        .unwrap();

        let servers = discover_mcp_servers_in(Some(home.path()), project.path()).await;
        let dups: Vec<_> = servers.iter().filter(|s| s.name == "dup").collect();
        assert_eq!(dups.len(), 1, "duplicate must be deduped: {dups:?}");
        assert_eq!(dups[0].command.as_deref(), Some("home-bin"));
        assert_eq!(dups[0].source, "user");
    }

    #[tokio::test]
    async fn test_broken_config_file_warns_but_does_not_stop_discovery() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".baoclaw"))
            .await
            .unwrap();
        fs::create_dir_all(project.path().join(".baoclaw"))
            .await
            .unwrap();
        fs::write(
            home.path().join(".baoclaw").join("mcp.json"),
            "{not valid json",
        )
        .await
        .unwrap();
        fs::write(
            project.path().join(".baoclaw").join("mcp.json"),
            r#"{"mcpServers": {"good": {"command": "bin"}}}"#,
        )
        .await
        .unwrap();

        let servers = discover_mcp_servers_in(Some(home.path()), project.path()).await;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "good");
    }
}
