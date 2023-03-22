use crate::teams::{Team, TEAMS};
use std::time::Duration;
use tokio::task::JoinSet;

const TIMEOUT_MS: u64 = 2000;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Protocol {
    Http,
    Https,
}

impl Protocol {
    const fn as_str(&self) -> &str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

pub enum RequestResult {
    Ok(u16),
    NginxDefaultPage(u16),
    CorrectRedirect(u16),
    IncorrectRedirect(u16),
    UnexpectedResponse(u16),
    Timeout,
    UntrustedCertificate,
    FailedConnect,
    Error(reqwest::Error),
}

pub struct TeamResult {
    pub team: Team,
    pub http: RequestResult,
    pub https: RequestResult,
}

async fn check_protocol(client: reqwest::Client, team: &Team, protocol: Protocol) -> RequestResult {
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
                    RequestResult::NginxDefaultPage(status_code)
                } else {
                    RequestResult::Ok(status_code)
                }
            } else if matches!(
                response.status(),
                reqwest::StatusCode::MOVED_PERMANENTLY
                    | reqwest::StatusCode::PERMANENT_REDIRECT
                    | reqwest::StatusCode::FOUND
            ) && protocol == Protocol::Http
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
                    RequestResult::CorrectRedirect(response.status().as_u16())
                } else {
                    // Redirects incorrectly
                    RequestResult::IncorrectRedirect(response.status().as_u16())
                }
            } else {
                RequestResult::UnexpectedResponse(response.status().as_u16())
            }
        }
        Err(e) => {
            if e.is_timeout() {
                eprintln!("Timeout: {}", url);
                RequestResult::Timeout
            } else if e.is_connect() {
                if protocol == Protocol::Https
                    && format!("{}", e).contains("The certificate was not trusted")
                {
                    RequestResult::UntrustedCertificate
                } else {
                    RequestResult::FailedConnect
                }
            } else {
                RequestResult::Error(e)
            }
        }
    }
}

pub async fn get_results() -> Vec<TeamResult> {
    for _ in 0..2 {
        let mut results = vec![];
        let mut join_set = JoinSet::new();
        let client = reqwest::Client::builder()
            // .proxy(reqwest::Proxy::all("socks5://127.0.0.1:9090").unwrap())
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("https://uob-team-project-2022.nihaal.dev/")
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            .build()
            .unwrap();
        for team in TEAMS {
            let client = client.clone();
            join_set.spawn(async move {
                let (http_result, https_result) = tokio::join!(
                    check_protocol(client.clone(), team, Protocol::Http),
                    check_protocol(client.clone(), team, Protocol::Https)
                );
                TeamResult {
                    team: team.clone(),
                    http: http_result,
                    https: https_result,
                }
            });
        }
        while let Some(handle) = join_set.join_next().await {
            results.push(handle.unwrap());
        }

        if results.iter().all(|result| {
            matches!(
                result.http,
                RequestResult::Timeout | RequestResult::FailedConnect
            ) && matches!(
                result.https,
                RequestResult::Timeout | RequestResult::FailedConnect
            )
        }) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        return results;
    }
    panic!("connection failed");
}
