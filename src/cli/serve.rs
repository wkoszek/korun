use anyhow::Result;
use serde_json::json;

// Note: daemon auto-start and fork logic is in main.rs (before tokio runtime starts).
// serve_cmd is called only after the daemon is confirmed running.
use crate::cli::client::{new_client, DEFAULT_ADDR};

pub async fn serve_cmd(
    command: Vec<String>,
    watch: Vec<String>,
    env_pairs: Vec<String>,
    cwd: Option<String>,
) -> Result<()> {
    let addr = DEFAULT_ADDR;
    let client = new_client();

    let cwd = cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });

    let mut env_map = std::collections::HashMap::new();
    for pair in env_pairs {
        if let Some((k, v)) = pair.split_once('=') {
            env_map.insert(k.to_string(), v.to_string());
        }
    }

    let body = json!({
        "command": command,
        "cwd": cwd,
        "watch": watch,
        "env": env_map,
    });

    let resp = client
        .post(format!("{addr}/v1/sessions"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await?;
        anyhow::bail!("failed to create session: {text}");
    }

    let json: serde_json::Value = resp.json().await?;
    println!("{}", json["id"].as_str().unwrap_or("unknown"));
    Ok(())
}
