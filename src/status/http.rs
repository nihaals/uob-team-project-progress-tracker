use crate::teams::Team;
use std::time::Duration;

const TIMEOUT_MS: u64 = 2000;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum HttpProtocol {
    Http,
    Https,
}

impl HttpProtocol {
    const fn as_str(&self) -> &str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

pub(super) enum HttpRequestResult {
    Ok(u16),
    NginxDefaultPage(u16),
    CorrectRedirect(u16),
    IncorrectRedirect(u16),
    UnexpectedResponse(u16),
    Timeout,
    UntrustedCertificate,
    InvalidCertificate,
    FailedConnect,
    Error(reqwest::Error),
}

pub(super) async fn http_check_protocol(
    client: reqwest::Client,
    team: &Team,
    protocol: HttpProtocol,
) -> HttpRequestResult {
    let url = format!("{}://{}/", protocol.as_str(), team.domain);
    match client
        .get(&url)
        .timeout(Duration::from_millis(TIMEOUT_MS))
        .send()
        .await
    {
        Ok(response) => {
            if response.status() == reqwest::StatusCode::OK {
                // OK
                let status_code = response.status().as_u16();
                if response.text().await.unwrap().contains(
                    "<p>If you see this page, the nginx web server is successfully installed and\nworking. Further configuration is required.</p>",
                ) {
                    HttpRequestResult::NginxDefaultPage(status_code)
                } else {
                    HttpRequestResult::Ok(status_code)
                }
            } else if matches!(
                response.status(),
                reqwest::StatusCode::MOVED_PERMANENTLY
                    | reqwest::StatusCode::PERMANENT_REDIRECT
                    | reqwest::StatusCode::FOUND
            ) && protocol == HttpProtocol::Http
                && response.headers().contains_key("Location")
            {
                // Redirect from HTTP
                if response
                    .headers()
                    .get("Location")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    == format!("https://{}/", team.domain)
                {
                    // Redirects correctly
                    HttpRequestResult::CorrectRedirect(response.status().as_u16())
                } else {
                    // Redirects incorrectly
                    HttpRequestResult::IncorrectRedirect(response.status().as_u16())
                }
            } else {
                HttpRequestResult::UnexpectedResponse(response.status().as_u16())
            }
        }
        Err(e) => {
            if e.is_timeout() {
                eprintln!("Timeout: {}", url);
                HttpRequestResult::Timeout
            } else if e.is_connect() {
                if protocol == HttpProtocol::Https
                    && format!("{}", e).contains("The certificate was not trusted")
                {
                    HttpRequestResult::UntrustedCertificate
                } else if protocol == HttpProtocol::Https
                    && format!("{}", e).contains("invalid peer certificate contents: invalid peer certificate: CertNotValidForName")
                {
                    HttpRequestResult::InvalidCertificate
                } else {
                    HttpRequestResult::FailedConnect
                }
            } else {
                HttpRequestResult::Error(e)
            }
        }
    }
}
