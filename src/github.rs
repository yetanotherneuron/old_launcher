//! Downloading from GitHub signed in, which GitHub limits far less than
//! anonymous downloads.
//!
//! The launcher stores no token. It takes one from its environment
//! (`GH_TOKEN`, then `GITHUB_TOKEN`, as the GitHub CLI does) or, when the
//! settings allow it, from the GitHub CLI's login (`gh auth token`), keeps
//! it in memory, and sends it to GitHub's own hosts only.

use std::process::{Command, Stdio};

use reqwest::header::LOCATION;
use reqwest::{StatusCode, Url};

/// The environment variables a token is taken from, in this order: the
/// GitHub CLI's own.
pub const VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The only places a token is sent to. Release files redirect to signed
/// links (`release-assets.githubusercontent.com`) that need none.
const ORIGINS: &[&str] = &[
    "https://github.com",
    "https://api.github.com",
    "https://codeload.github.com",
    "https://raw.githubusercontent.com",
];

/// How many redirects a signed-in download follows.
const REDIRECTS: usize = 5;

/// What the banner says when GitHub refuses an anonymous download for
/// asking too much.
pub const LIMITED: &str = "GitHub is limiting downloads without an account. Try again later, \
                           or sign in to GitHub in the settings, behind the cog at the top.";

/// What it says when GitHub refuses a signed-in one.
pub const LIMITED_SIGNED_IN: &str = "GitHub is limiting downloads right now. Try again later.";

/// A GitHub token. It is never written anywhere, and shows as `Token(…)`
/// when debugged, so logs can't leak it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

#[cfg(test)]
impl Token {
    /// `token`, for tests.
    pub fn new(token: &str) -> Self {
        Self(token.to_owned())
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(…)")
    }
}

/// How downloads from GitHub sign in, if they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    /// No token: downloads are anonymous.
    Anonymous,
    /// Asking the GitHub CLI.
    Asking,
    /// With the token in this environment variable.
    Environment {
        /// One of [`VARIABLES`].
        variable: &'static str,
        /// Its value.
        token: Token,
    },
    /// With the GitHub CLI's login.
    Cli(Token),
    /// The GitHub CLI isn't installed: downloads are anonymous.
    CliMissing,
    /// The GitHub CLI gave no token, for this reason: downloads are
    /// anonymous.
    CliFailed(String),
}

impl Login {
    /// The token downloads sign in with, if any.
    pub fn token(&self) -> Option<&Token> {
        match self {
            Self::Environment { token, .. } | Self::Cli(token) => Some(token),
            _ => None,
        }
    }

    /// How downloads from GitHub go, for people: what follows "Current
    /// downloads:".
    pub fn status(&self) -> String {
        match self {
            Self::Anonymous => "anonymous".to_owned(),
            Self::Asking => "asking the GitHub CLI…".to_owned(),
            Self::Environment { variable, .. } => {
                format!("signed in with the token in {variable}")
            }
            Self::Cli(_) => "signed in with your GitHub CLI login".to_owned(),
            Self::CliMissing => "anonymous, as the GitHub CLI isn't installed".to_owned(),
            Self::CliFailed(why) => format!("anonymous, as the GitHub CLI gave no login: {why}"),
        }
    }
}

/// The token in the first of [`VARIABLES`] that `environment` has set, if
/// any. A variable set to nothing counts as unset.
pub fn from_environment(environment: impl Fn(&str) -> Option<String>) -> Option<Login> {
    VARIABLES.into_iter().find_map(|variable| {
        let token = environment(variable)?.trim().to_owned();

        (!token.is_empty()).then_some(Login::Environment {
            variable,
            token: Token(token),
        })
    })
}

/// Asks the GitHub CLI for its login, `gh auth token`, on a blocking
/// thread.
pub async fn ask_cli() -> Login {
    tokio::task::spawn_blocking(ask_cli_now)
        .await
        .unwrap_or_else(|error| Login::CliFailed(error.to_string()))
}

/// Runs `gh auth token` and reads its answer.
fn ask_cli_now() -> Login {
    let mut command = Command::new("gh");
    let _ = command
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(Stdio::null());

    // The release build has no console of its own: without this, Windows
    // opens one for gh.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = command.creation_flags(CREATE_NO_WINDOW);
    }

    match command.output() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Login::CliMissing,
        Err(error) => Login::CliFailed(error.to_string()),
        Ok(output) => cli_answer(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
    }
}

/// What `gh auth token` answered: the token it printed when it succeeded,
/// or the first line it complained with.
fn cli_answer(success: bool, stdout: &str, stderr: &str) -> Login {
    let token = stdout.trim();

    if success && !token.is_empty() {
        return Login::Cli(Token(token.to_owned()));
    }

    let why = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it printed no token");

    Login::CliFailed(why.trim_end_matches('.').to_owned())
}

/// Downloads signed in to GitHub: a client that follows no redirects by
/// itself, and the token.
#[derive(Debug, Clone)]
pub struct Signed {
    client: reqwest::Client,
    token: Token,
    /// Where the token goes: [`ORIGINS`], or test servers.
    origins: Vec<String>,
}

impl Signed {
    /// Signs downloads made with `client`, which must follow no redirects,
    /// with `token`.
    pub fn new(client: reqwest::Client, token: Token) -> Self {
        Self {
            client,
            token,
            origins: ORIGINS.iter().map(|&origin| origin.to_owned()).collect(),
        }
    }

    /// Signs only the requests to `origins` (`scheme://host[:port]`), for
    /// tests to stand in for GitHub.
    #[cfg(test)]
    pub fn sending_to(client: reqwest::Client, token: &str, origins: &[String]) -> Self {
        Self {
            client,
            token: Token(token.to_owned()),
            origins: origins.to_vec(),
        }
    }

    /// GETs `url`, following up to [`REDIRECTS`] redirects here rather than
    /// in reqwest, which drops the token whenever a redirect changes host:
    /// each request to one of the origins signed in carries the token, and
    /// no other does.
    pub async fn get(&self, url: &str) -> Result<reqwest::Response, String> {
        let mut url = Url::parse(url).map_err(|error| format!("{url} is no address: {error}"))?;

        for _ in 0..=REDIRECTS {
            let mut request = self.client.get(url.clone());
            if self.signs(&url) {
                request = request.bearer_auth(&self.token.0);
            }

            let response = request
                .send()
                .await
                .map_err(|error| crate::describe(&error))?;

            let location = response
                .status()
                .is_redirection()
                .then(|| response.headers().get(LOCATION))
                .flatten();
            let Some(location) = location else {
                return Ok(response);
            };

            url = follow(&url, location.to_str().unwrap_or_default())?;
        }

        Err(format!("{url} redirected more than {REDIRECTS} times"))
    }

    /// Whether requests to `url` carry the token.
    fn signs(&self, url: &Url) -> bool {
        let origin = url.origin().ascii_serialization();

        self.origins.contains(&origin)
    }
}

/// Where a redirect from `from` to `location` leads. Leaving an encrypted
/// address for an unencrypted one is refused.
fn follow(from: &Url, location: &str) -> Result<Url, String> {
    let next = from
        .join(location)
        .map_err(|error| format!("{from} redirected to {location:?}: {error}"))?;

    if from.scheme() == "https" && next.scheme() != "https" {
        return Err(format!(
            "{from} redirected to an unencrypted address, {next}"
        ));
    }

    Ok(next)
}

/// What to tell people when `response` is GitHub refusing a download for
/// asking too much, if it is; `signed_in` says whether the download had a
/// token.
pub fn refusal(response: &reqwest::Response, signed_in: bool) -> Option<&'static str> {
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok());

    refused(
        response.url().host_str()?,
        response.status(),
        remaining,
        signed_in,
    )
}

/// Whether an answer of `status` from `host`, with `remaining` requests
/// left as GitHub counts them, is GitHub refusing to serve more: 429 from
/// any of its hosts, or 403 with none left.
fn refused(
    host: &str,
    status: StatusCode,
    remaining: Option<&str>,
    signed_in: bool,
) -> Option<&'static str> {
    let github = ["github.com", "githubusercontent.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")));
    let limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN && remaining == Some("0"));

    (github && limited).then_some(if signed_in {
        LIMITED_SIGNED_IN
    } else {
        LIMITED
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn environment(set: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let set: Vec<(String, String)> = set
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();

        move |variable| {
            set.iter()
                .find(|(name, _)| name == variable)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn gh_token_comes_before_github_token_and_empty_ones_count_as_unset() {
        let both = from_environment(environment(&[
            ("GITHUB_TOKEN", "second"),
            ("GH_TOKEN", "first"),
        ]));
        assert_eq!(
            both,
            Some(Login::Environment {
                variable: "GH_TOKEN",
                token: Token("first".into()),
            })
        );

        let empty_first = from_environment(environment(&[
            ("GH_TOKEN", "  "),
            ("GITHUB_TOKEN", "second\n"),
        ]));
        assert_eq!(
            empty_first,
            Some(Login::Environment {
                variable: "GITHUB_TOKEN",
                token: Token("second".into()),
            })
        );

        assert_eq!(from_environment(environment(&[])), None);
    }

    #[test]
    fn the_cli_gives_its_token_or_says_why_not() {
        assert_eq!(
            cli_answer(true, "gho_abc\n", ""),
            Login::Cli(Token("gho_abc".into()))
        );
        assert_eq!(
            cli_answer(
                false,
                "",
                "no oauth token found for github.com.\nrun gh auth login\n"
            ),
            Login::CliFailed("no oauth token found for github.com".into())
        );
        assert_eq!(
            cli_answer(true, "  \n", ""),
            Login::CliFailed("it printed no token".into())
        );
    }

    #[test]
    fn tokens_never_show_when_debugged_or_described() {
        let logins = [
            Login::Cli(Token("gho_secret".into())),
            Login::Environment {
                variable: "GH_TOKEN",
                token: Token("gho_secret".into()),
            },
        ];

        for login in logins {
            assert!(!format!("{login:?}").contains("secret"));
            assert!(!login.status().contains("secret"));
        }
    }

    #[test]
    fn only_githubs_answers_to_asking_too_much_are_refusals() {
        let refused = |host, status, remaining| refused(host, status, remaining, false);

        assert_eq!(
            refused("codeload.github.com", StatusCode::TOO_MANY_REQUESTS, None),
            Some(LIMITED)
        );
        assert_eq!(
            refused(
                "raw.githubusercontent.com",
                StatusCode::FORBIDDEN,
                Some("0")
            ),
            Some(LIMITED)
        );
        assert_eq!(
            refused("github.com", StatusCode::FORBIDDEN, Some("12")),
            None,
            "forbidden for another reason"
        );
        assert_eq!(
            refused("notgithub.com", StatusCode::TOO_MANY_REQUESTS, None),
            None
        );
        assert_eq!(
            refused("cdn.wowinterface.com", StatusCode::TOO_MANY_REQUESTS, None),
            None
        );
        assert_eq!(
            super::refused("github.com", StatusCode::TOO_MANY_REQUESTS, None, true),
            Some(LIMITED_SIGNED_IN)
        );
    }

    #[test]
    fn redirects_never_leave_encryption_behind() {
        let from = Url::parse("https://github.com/a/b/archive/main.zip").unwrap();

        assert_eq!(
            follow(&from, "https://codeload.github.com/a/b/zip/main")
                .unwrap()
                .as_str(),
            "https://codeload.github.com/a/b/zip/main"
        );
        assert_eq!(
            follow(&from, "/a/b/archive/v1.zip").unwrap().as_str(),
            "https://github.com/a/b/archive/v1.zip"
        );
        assert!(follow(&from, "http://codeload.github.com/a/b/zip/main").is_err());
    }

    /// A local server answering each request with what `respond` makes of
    /// the server's own origin and the request's path, and recording in
    /// `seen` each path and whether it carried the token. Returns its
    /// origin.
    async fn server(
        respond: impl Fn(&str, &str) -> String + Send + 'static,
        seen: Arc<Mutex<Vec<(String, bool)>>>,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());

        drop(tokio::spawn({
            let origin = origin.clone();

            async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 4096];
                    let read = stream.read(&mut request).await.unwrap();
                    let request = String::from_utf8_lossy(&request[..read]).into_owned();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let signed = request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("authorization: bearer secret"));

                    seen.lock().unwrap().push((path.clone(), signed));
                    stream
                        .write_all(respond(&origin, &path).as_bytes())
                        .await
                        .unwrap();
                }
            }
        }));

        origin
    }

    fn redirect(to: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn the_token_goes_to_githubs_hosts_only_through_redirects() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        // Standing for a host the token never goes to, like a signed
        // release link.
        let archive = server(|_, _| ok("zip bytes"), Arc::clone(&seen)).await;
        // Standing for github.com: redirects to itself, then away.
        let github = server(
            move |origin, path| match path {
                "/start" => redirect(&format!("{origin}/again")),
                _ => redirect(&format!("{archive}/archive.zip")),
            },
            Arc::clone(&seen),
        )
        .await;

        let signed = Signed::sending_to(client(), "secret", std::slice::from_ref(&github));
        let response = signed.get(&format!("{github}/start")).await.unwrap();

        assert_eq!(response.text().await.unwrap(), "zip bytes");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                ("/start".to_owned(), true),
                ("/again".to_owned(), true),
                ("/archive.zip".to_owned(), false),
            ]
        );
    }

    /// One download of each kind the catalog has from GitHub, signed in with
    /// the token in the environment or the GitHub CLI's login: a branch
    /// snapshot, a file in a repository, a release file, an API zipball.
    #[tokio::test]
    #[ignore = "downloads from GitHub, signed in with GH_TOKEN, GITHUB_TOKEN or gh auth token"]
    async fn every_kind_of_github_download_arrives_signed_in() {
        let login =
            from_environment(|variable| std::env::var(variable).ok()).unwrap_or_else(ask_cli_now);
        let token = login.token().expect("a token").clone();
        // The launcher's own client: GitHub's API refuses requests without
        // a user agent.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let direct = crate::http_client()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let signed = Signed::new(direct, token);

        for url in [
            "https://github.com/KhalGH/_VirtualPlates-WotLK/archive/refs/heads/main.zip",
            "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/AckisRecipeList.zip",
            "https://github.com/NoM0Re/DalaranAH/releases/latest/download/DalaranAH.zip",
            "https://api.github.com/repos/Drakensangs/RecipeColor-WotLK/zipball/3.3",
        ] {
            let response = signed.get(url).await.unwrap();
            let status = response.status();
            let host = response.url().host_str().unwrap_or_default().to_owned();
            let bytes = response.bytes().await.unwrap();

            assert!(status.is_success(), "{url}: {status} from {host}");
            assert!(bytes.starts_with(b"PK"), "{url}: no zip from {host}");
        }
    }

    #[tokio::test]
    async fn endless_redirects_stop() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let looping = server(
            |origin, _| redirect(&format!("{origin}/again")),
            Arc::clone(&seen),
        )
        .await;

        let signed = Signed::sending_to(client(), "secret", &[]);
        let error = signed.get(&format!("{looping}/start")).await.unwrap_err();

        assert!(error.contains("redirected more than 5 times"), "{error}");
        assert_eq!(seen.lock().unwrap().len(), REDIRECTS + 1);
        assert!(
            seen.lock().unwrap().iter().all(|(_, signed)| !signed),
            "not one of the origins signed"
        );
    }
}
