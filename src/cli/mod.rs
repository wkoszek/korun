pub mod client;
pub mod serve;

use crate::cli::client::{new_client, new_streaming_client, DEFAULT_ADDR};
use anyhow::{Context, Result};

fn is_active_session(session: &serde_json::Value) -> bool {
    matches!(
        session["state"].as_str(),
        Some("starting" | "running" | "stopping")
    )
}

async fn read_json_response(resp: reqwest::Response) -> Result<serde_json::Value> {
    let status = resp.status();
    let body = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        anyhow::bail!("{}", serde_json::to_string_pretty(&body)?);
    }
    Ok(body)
}

async fn read_text_response(resp: reqwest::Response) -> Result<String> {
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{body}");
    }
    Ok(body)
}

/// If `id` is Some, return it. If None and exactly one session is active, return its ID.
/// Otherwise error.
async fn resolve_id(id: Option<String>) -> Result<String> {
    if let Some(id) = id {
        return Ok(id);
    }
    let sessions = read_json_response(
        new_client()
            .get(format!("{DEFAULT_ADDR}/v1/sessions"))
            .send()
            .await?,
    )
    .await?;
    let arr = sessions["sessions"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("unexpected response from daemon"))?;
    let active: Vec<&serde_json::Value> = arr.iter().filter(|s| is_active_session(s)).collect();
    match active.len() {
        0 => anyhow::bail!("no active sessions"),
        1 => active[0]["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("session has no id")),
        n => anyhow::bail!("{n} active sessions — specify a UUID"),
    }
}

/// Merge explicit watch paths with paths read from an optional watch file.
pub fn load_watch_paths(mut watch: Vec<String>, watch_file: Option<String>) -> Result<Vec<String>> {
    if let Some(path) = watch_file {
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("could not read watch file {path}: {e}"))?;
        for line in contents.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                watch.push(line.to_string());
            }
        }
    }
    Ok(watch)
}

pub async fn cmd_ls() -> Result<()> {
    let client = new_client();
    let resp = read_json_response(
        client
            .get(format!("{DEFAULT_ADDR}/v1/sessions"))
            .send()
            .await?,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

pub async fn cmd_inspect(id: Option<String>) -> Result<()> {
    let id = resolve_id(id).await?;
    let client = new_client();
    let body = read_json_response(
        client
        .get(format!("{DEFAULT_ADDR}/v1/sessions/{id}"))
        .send()
        .await?,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

pub async fn cmd_restart(id: Option<String>) -> Result<()> {
    let id = resolve_id(id).await?;
    let client = new_client();
    let resp = read_json_response(
        client
        .post(format!("{DEFAULT_ADDR}/v1/sessions/{id}/restart"))
        .send()
        .await?,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

pub async fn cmd_stop(id: Option<String>) -> Result<()> {
    let id = resolve_id(id).await?;
    let client = new_client();
    let resp = read_json_response(
        client
        .post(format!("{DEFAULT_ADDR}/v1/sessions/{id}/stop"))
        .send()
        .await?,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

pub async fn cmd_stop_all() -> Result<()> {
    let client = new_client();
    let sessions = read_json_response(
        client
            .get(format!("{DEFAULT_ADDR}/v1/sessions"))
            .send()
            .await?,
    )
    .await?;
    let ids: Vec<String> = sessions["sessions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|s| is_active_session(s))
                .filter_map(|s| s["id"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        println!("no active sessions");
        return Ok(());
    }
    for id in &ids {
        let resp = read_json_response(
            client
            .post(format!("{DEFAULT_ADDR}/v1/sessions/{id}/stop"))
            .send()
            .await?,
        )
        .await?;
        println!("{id}: {}", resp["state"].as_str().unwrap_or("?"));
    }
    Ok(())
}

pub async fn cmd_tail(id: Option<String>, follow: bool) -> Result<()> {
    let id = resolve_id(id).await?;
    let follow_param = if follow { "1" } else { "0" };
    let url = format!("{DEFAULT_ADDR}/v1/sessions/{id}/tail?follow={follow_param}&format=text");

    if follow {
        // No timeout — stream stays open until the user Ctrl-Cs or the session ends
        let mut resp = new_streaming_client()
            .get(&url)
            .send()
            .await
            .context("connecting to daemon for log stream")?;
        resp.error_for_status_ref()?;
        while let Some(chunk) = resp.chunk().await.context("reading log stream")? {
            print!("{}", String::from_utf8_lossy(&chunk));
        }
    } else {
        let text = read_text_response(
            new_client()
            .get(&url)
            .send()
            .await
            .context("GET tail")?,
        )
        .await
        .context("reading tail response")?;
        print!("{text}");
    }
    Ok(())
}

pub async fn cmd_head(id: Option<String>) -> Result<()> {
    let id = resolve_id(id).await?;
    let client = new_client();
    let text = read_text_response(
        client
        .get(format!("{DEFAULT_ADDR}/v1/sessions/{id}/head?format=text"))
        .send()
        .await?,
    )
    .await?;
    print!("{text}");
    Ok(())
}
