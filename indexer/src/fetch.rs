//! Downloading, patiently.

use std::time::Duration;

use anyhow::Context;
use reqwest::StatusCode;
use reqwest::header::{ETAG, HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use serde::{Deserialize, Serialize};

/// Downloads `url`, retrying network failures and server errors (a 500, a
/// 429) a couple of times so one flaky moment doesn't cost the catalog an
/// entry. Other HTTP errors (a 404) are final.
pub async fn fetch(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    send(|| client.get(url), url).await
}

/// What a server sent to tell one version of a file from another: its
/// `ETag`, its `Last-Modified` date, or both. Sent back with a later
/// request, they let the server answer that the file is still that version
/// instead of sending it again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl Validators {
    /// Those among a response's `headers`.
    fn of(headers: &HeaderMap) -> Self {
        let header = |name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };

        Self {
            etag: header(ETAG),
            last_modified: header(LAST_MODIFIED),
        }
    }

    /// Whether there are none, so a server can't be asked whether its file
    /// changed.
    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    /// `request`, asking for the file only if it is no longer the version
    /// these tell. Both go: a server that ignores one may heed the other,
    /// and one that heeds both goes by the `ETag`.
    fn ask(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(etag) = &self.etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        if let Some(date) = &self.last_modified {
            request = request.header(IF_MODIFIED_SINCE, date);
        }

        request
    }
}

/// What a download asking whether a file changed got.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// The file, which changed or was never asked about, with the validators
    /// its server sent for this version.
    Changed(Vec<u8>, Validators),
    /// Word that the file is still the version the validators sent tell.
    Unchanged,
}

/// Downloads `url` like [`fetch`], unless its server says it is still the
/// version `validators`, from an earlier download, tell. Without validators
/// it is always downloaded.
pub async fn fetch_changed(
    client: &reqwest::Client,
    url: &str,
    validators: &Validators,
) -> anyhow::Result<Fetched> {
    let answer = answer(|| validators.ask(client.get(url)), url).await?;

    Ok(if answer.status == StatusCode::NOT_MODIFIED {
        Fetched::Unchanged
    } else {
        Fetched::Changed(answer.body, answer.validators)
    })
}

/// Sends the request `build` makes and reads the answer, retrying like
/// [`fetch`]; `url` names it in errors. A request is built again for every
/// attempt.
pub async fn send(
    build: impl Fn() -> reqwest::RequestBuilder,
    url: &str,
) -> anyhow::Result<Vec<u8>> {
    Ok(answer(build, url).await?.body)
}

/// A server's answer to a request: a success, or `304 Not Modified`.
struct Answer {
    status: StatusCode,
    /// What it sent to tell this version of the file from others.
    validators: Validators,
    body: Vec<u8>,
}

/// Sends the request `build` makes and reads the whole answer, retrying
/// like [`fetch`]; `url` names it in errors.
async fn answer(build: impl Fn() -> reqwest::RequestBuilder, url: &str) -> anyhow::Result<Answer> {
    let mut attempt = 1;

    loop {
        let download = async {
            let response = build().send().await?.error_for_status()?;
            let status = response.status();
            let validators = Validators::of(response.headers());
            let body = response.bytes().await?.into();

            Ok::<_, reqwest::Error>(Answer {
                status,
                validators,
                body,
            })
        };

        match download.await {
            Ok(answer) => return Ok(answer),
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
