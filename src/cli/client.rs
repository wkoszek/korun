use anyhow::Result;
use reqwest::Client;

pub const DEFAULT_ADDR: &str = "http://127.0.0.1:7777";

/// Standard client with 10s timeout — for all non-streaming requests.
pub fn new_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

/// No-timeout client — for streaming endpoints (tail -f, logs follow=1).
pub fn new_streaming_client() -> Client {
    Client::builder()
        .build()
        .expect("reqwest streaming client")
}

#[allow(dead_code)]
pub async fn healthz(client: &Client, addr: &str) -> Result<bool> {
    let resp = client.get(format!("{addr}/healthz")).send().await?;
    Ok(resp.status().is_success())
}

#[allow(dead_code)]
pub async fn wait_for_daemon(addr: &str) -> Result<()> {
    let client = new_client();
    for _ in 0..50 {
        if healthz(&client, addr).await.unwrap_or(false) {
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("daemon did not start within 5 seconds")
}
