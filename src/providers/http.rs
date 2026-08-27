//! Shared HTTP plumbing for providers: one client, 30s timeout, and a
//! single retry with backoff on 5xx or timeout.

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Build the shared reqwest client (30s timeout).
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// POST `body` as JSON to `url` and decode a JSON response.
///
/// Retries exactly once, after a 1s backoff, when the failure is a timeout,
/// a connection error, or an HTTP 5xx. 4xx responses fail immediately with
/// the response body included for diagnosis.
pub async fn post_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
    body: &serde_json::Value,
) -> Result<T> {
    let mut last_err = None;
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let mut req = client.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp
                        .json::<T>()
                        .await
                        .map_err(|e| Error::Provider(format!("{url}: bad response JSON: {e}")));
                }
                let text = resp.text().await.unwrap_or_default();
                let err = Error::Provider(format!("{url}: HTTP {status}: {text}"));
                if status.is_server_error() {
                    last_err = Some(err); // retryable
                } else {
                    return Err(err); // 4xx: not retryable
                }
            }
            Err(e) if e.is_timeout() || e.is_connect() => {
                last_err = Some(Error::Provider(format!("{url}: {e}")));
            }
            Err(e) => return Err(Error::Provider(format!("{url}: {e}"))),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::Provider(format!("{url}: request failed"))))
}
