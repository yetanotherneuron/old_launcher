//! Downloading, patiently.

use std::time::Duration;

use anyhow::Context;

/// Downloads `url`, retrying network failures and server errors (a 500, a
/// 429) a couple of times so one flaky moment doesn't cost the catalog an
/// entry. Other HTTP errors (a 404) are final.
pub async fn fetch(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut attempt = 1;

    loop {
        let download = async {
            client
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await
        };

        match download.await {
            Ok(bytes) => return Ok(bytes.into()),
            Err(error)
                if attempt < 3
                    && error.status().is_none_or(|status| {
                        status.is_server_error() || status.as_u16() == 429
                    }) =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(error).with_context(|| format!("downloading {url}")),
        }
    }
}
