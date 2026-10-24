//! Downloading from GitLab, which allows anyone, signed in or not, only a
//! few repository archives a minute: downloads from it stay anonymous, and
//! when it says to wait, the banner says so.

use reqwest::StatusCode;

/// What the banner says when GitLab refuses a download for asking too much.
pub const LIMITED: &str = "GitLab is limiting downloads right now. Try again in a minute.";

/// What to tell people when `response` is GitLab refusing a download for
/// asking too much, if it is.
pub fn refusal(response: &reqwest::Response) -> Option<&'static str> {
    refused(response.url().host_str()?, response.status())
}

/// Whether an answer of `status` from `host` is GitLab refusing to serve
/// more: 429 from gitlab.com.
fn refused(host: &str, status: StatusCode) -> Option<&'static str> {
    let gitlab = host == "gitlab.com" || host.ends_with(".gitlab.com");

    (gitlab && status == StatusCode::TOO_MANY_REQUESTS).then_some(LIMITED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_gitlabs_answers_to_asking_too_much_are_refusals() {
        assert_eq!(
            refused("gitlab.com", StatusCode::TOO_MANY_REQUESTS),
            Some(LIMITED)
        );
        assert_eq!(refused("gitlab.com", StatusCode::NOT_FOUND), None);
        assert_eq!(refused("gitlab.com", StatusCode::FORBIDDEN), None);
        assert_eq!(
            refused("notgitlab.com", StatusCode::TOO_MANY_REQUESTS),
            None
        );
        assert_eq!(refused("example.com", StatusCode::TOO_MANY_REQUESTS), None);
    }
}
