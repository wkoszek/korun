pub mod client;
pub mod serve;

use crate::cli::client::{new_client, DEFAULT_ADDR};
use anyhow::Result;

pub async fn cmd_ls() -> Result<()> {
    let client = new_client();
    let resp = client
        .get(format!("{DEFAULT_ADDR}/v1/sessions"))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

pub async fn cmd_inspect(id: &str) -> Result<()> {
    let client = new_client();
    let resp = client
        .get(format!("{DEFAULT_ADDR}/v1/sessions/{id}"))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        anyhow::bail!("{}", serde_json::to_string_pretty(&body)?);
    }
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

pub async fn cmd_restart(id: &str) -> Result<()> {
    let client = new_client();
    let resp = client
        .post(format!("{DEFAULT_ADDR}/v1/sessions/{id}/restart"))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

pub async fn cmd_stop(id: &str) -> Result<()> {
    let client = new_client();
    let resp = client
        .post(format!("{DEFAULT_ADDR}/v1/sessions/{id}/stop"))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

pub async fn cmd_tail(id: &str, follow: bool) -> Result<()> {
    let client = new_client();
    let follow_param = if follow { "1" } else { "0" };
    let url = format!("{DEFAULT_ADDR}/v1/sessions/{id}/tail?follow={follow_param}&format=text");

    if follow {
        // Stream response
        let mut resp = client.get(&url).send().await?;
        while let Some(chunk) = resp.chunk().await? {
            print!("{}", String::from_utf8_lossy(&chunk));
        }
    } else {
        let text = client.get(&url).send().await?.text().await?;
        print!("{text}");
    }
    Ok(())
}

pub async fn cmd_head(id: &str) -> Result<()> {
    let client = new_client();
    let text = client
        .get(format!("{DEFAULT_ADDR}/v1/sessions/{id}/head?format=text"))
        .send()
        .await?
        .text()
        .await?;
    print!("{text}");
    Ok(())
}
