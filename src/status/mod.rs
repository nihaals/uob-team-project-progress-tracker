mod http;

use std::time::Duration;

use tokio::task::JoinSet;

use crate::teams::{Team, TEAMS};

pub use self::http::HttpProtocol;
use self::http::{http_check_protocol, HttpRequestResult};

pub enum ProtocolResult {
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
    pub http: ProtocolResult,
    pub https: ProtocolResult,
}

impl From<HttpRequestResult> for ProtocolResult {
    fn from(result: HttpRequestResult) -> Self {
        match result {
            HttpRequestResult::Ok(status_code) => Self::Ok(status_code),
            HttpRequestResult::NginxDefaultPage(status_code) => Self::NginxDefaultPage(status_code),
            HttpRequestResult::CorrectRedirect(status_code) => Self::CorrectRedirect(status_code),
            HttpRequestResult::IncorrectRedirect(status_code) => {
                Self::IncorrectRedirect(status_code)
            }
            HttpRequestResult::UnexpectedResponse(status_code) => {
                Self::UnexpectedResponse(status_code)
            }
            HttpRequestResult::Timeout => Self::Timeout,
            HttpRequestResult::UntrustedCertificate => Self::UntrustedCertificate,
            HttpRequestResult::FailedConnect => Self::FailedConnect,
            HttpRequestResult::Error(error) => Self::Error(error),
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
                    http_check_protocol(client.clone(), team, HttpProtocol::Http),
                    http_check_protocol(client.clone(), team, HttpProtocol::Https)
                );
                TeamResult {
                    team: team.clone(),
                    http: http_result.into(),
                    https: https_result.into(),
                }
            });
        }
        while let Some(handle) = join_set.join_next().await {
            results.push(handle.unwrap());
        }

        if results.iter().all(|result| {
            matches!(
                result.http,
                ProtocolResult::Timeout | ProtocolResult::FailedConnect
            ) && matches!(
                result.https,
                ProtocolResult::Timeout | ProtocolResult::FailedConnect
            )
        }) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        return results;
    }
    panic!("connection failed");
}
