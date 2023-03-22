mod teams;

use actix_web::{
    get,
    http::{self, header::ContentType},
    web, App, HttpResponse, HttpServer, Responder,
};
use handlebars::handlebars_helper;
use serde::Serialize;
use serde_json::json;
use std::{
    fs,
    time::{Duration, SystemTime},
};
use teams::{Team, TEAMS};
use tokio::{sync::RwLock, task::JoinSet};

const TIMEOUT_MS: u16 = 2000;
const STALE_SECONDS: i64 = 60;
const MAX_STALE_SECONDS: i64 = 60 * 60;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Protocol {
    Http,
    Https,
}

impl Protocol {
    fn as_str(&self) -> &str {
        match self {
            Protocol::Http => "http",
            Protocol::Https => "https",
        }
    }
}

enum RequestResult {
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

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum RequestResultResponseTemplate {
    #[serde(rename = "OK")]
    Ok { status_code: u16 },
    #[serde(rename = "Teapot")]
    Teapot,
    #[serde(rename = "Redirect")]
    Redirect { status_code: u16 },
    #[serde(rename = "Unexpected response")]
    UnexpectedResponse { status_code: u16 },
    #[serde(rename = "Timeout")]
    Timeout,
    #[serde(rename = "Untrusted certificate")]
    UntrustedCertificate,
    #[serde(rename = "Failed to connect")]
    FailedConnect,
    #[serde(rename = "Error")]
    Error,
}

enum RequestResultStatus {
    Correct,
    NearlyCorrect,
    Incorrect,
}

impl RequestResultStatus {
    fn to_bootstrap_class(&self) -> String {
        match self {
            RequestResultStatus::Correct => "link-success",
            RequestResultStatus::NearlyCorrect => "link-warning",
            RequestResultStatus::Incorrect => "link-danger",
        }
        .to_owned()
    }

    fn to_alt_text(&self) -> String {
        match self {
            RequestResultStatus::Correct => "Correct",
            RequestResultStatus::NearlyCorrect => "Nearly correct",
            RequestResultStatus::Incorrect => "Incorrect",
        }
        .to_owned()
    }
}

#[derive(Clone, Serialize)]
struct RequestResultTemplate {
    result: RequestResultResponseTemplate,
    bootstrap_class: String,
    alt_text: String,
}

impl RequestResultTemplate {
    fn new(result: RequestResultResponseTemplate, status: RequestResultStatus) -> Self {
        Self {
            result,
            bootstrap_class: status.to_bootstrap_class(),
            alt_text: status.to_alt_text(),
        }
    }
}

impl RequestResult {
    fn into_template(self, protocol: Protocol) -> RequestResultTemplate {
        match self {
            RequestResult::Ok(status_code) => RequestResultTemplate::new(
                RequestResultResponseTemplate::Ok { status_code },
                match protocol {
                    Protocol::Http => RequestResultStatus::NearlyCorrect,
                    Protocol::Https => RequestResultStatus::Correct,
                },
            ),
            RequestResult::NginxDefaultPage(status_code) => RequestResultTemplate::new(
                RequestResultResponseTemplate::Ok { status_code },
                RequestResultStatus::NearlyCorrect,
            ),
            RequestResult::CorrectRedirect(status_code) => RequestResultTemplate::new(
                RequestResultResponseTemplate::Redirect { status_code },
                RequestResultStatus::Correct,
            ),
            RequestResult::IncorrectRedirect(status_code) => RequestResultTemplate::new(
                RequestResultResponseTemplate::Redirect { status_code },
                RequestResultStatus::NearlyCorrect,
            ),
            RequestResult::UnexpectedResponse(status_code) => match status_code {
                418 => RequestResultTemplate::new(
                    RequestResultResponseTemplate::Teapot,
                    RequestResultStatus::NearlyCorrect,
                ),
                _ => RequestResultTemplate::new(
                    RequestResultResponseTemplate::UnexpectedResponse { status_code },
                    RequestResultStatus::Incorrect,
                ),
            },
            RequestResult::Timeout => RequestResultTemplate::new(
                RequestResultResponseTemplate::Timeout,
                RequestResultStatus::Incorrect,
            ),
            RequestResult::UntrustedCertificate => RequestResultTemplate::new(
                RequestResultResponseTemplate::UntrustedCertificate,
                RequestResultStatus::NearlyCorrect,
            ),
            RequestResult::FailedConnect => RequestResultTemplate::new(
                RequestResultResponseTemplate::FailedConnect,
                RequestResultStatus::Incorrect,
            ),
            RequestResult::Error(_) => RequestResultTemplate::new(
                RequestResultResponseTemplate::Error,
                RequestResultStatus::Incorrect,
            ),
        }
    }
}

struct TeamResult {
    team: Team,
    http: RequestResult,
    https: RequestResult,
}

#[derive(Clone, Serialize)]
struct TeamResultTemplate {
    team: Team,
    http: RequestResultTemplate,
    https: RequestResultTemplate,
}

impl From<TeamResult> for TeamResultTemplate {
    fn from(team_result: TeamResult) -> Self {
        TeamResultTemplate {
            team: team_result.team,
            http: team_result.http.into_template(Protocol::Http),
            https: team_result.https.into_template(Protocol::Https),
        }
    }
}

async fn check_protocol(client: reqwest::Client, team: &Team, protocol: Protocol) -> RequestResult {
    let url = format!("{}://{}/", protocol.as_str(), team.domain);
    match client
        .get(&url)
        .timeout(Duration::from_millis(TIMEOUT_MS as u64))
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

async fn get_results() -> Vec<TeamResult> {
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

#[derive(Clone)]
struct TestResultsCache {
    results: Vec<TeamResultTemplate>,
    timestamp: chrono::DateTime<chrono::Utc>,
}

struct AppState<'reg> {
    cache: RwLock<Option<TestResultsCache>>,
    handlebars: handlebars::Handlebars<'reg>,
    async_update_channel: tokio::sync::mpsc::Sender<()>,
}

#[derive(PartialEq, Eq)]
enum CacheState {
    Fresh,
    Stale,
}

impl AppState<'_> {
    async fn update_results_cache(&self) {
        let mut cache = self.cache.write().await;

        if let Some(cache) = cache.as_ref() {
            if chrono::Utc::now() - cache.timestamp < chrono::Duration::seconds(STALE_SECONDS) {
                return;
            }
        }

        let mut results: Vec<TeamResultTemplate> =
            get_results().await.into_iter().map(|r| r.into()).collect();
        results.sort_unstable_by_key(|r| r.team.team_number);

        *cache = Some(TestResultsCache {
            results,
            timestamp: chrono::Utc::now(),
        });
    }

    async fn get_results(&self) -> (TestResultsCache, CacheState) {
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref() {
                if chrono::Utc::now() - cache.timestamp < chrono::Duration::seconds(STALE_SECONDS) {
                    return (cache.clone(), CacheState::Fresh);
                } else {
                    if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) =
                        self.async_update_channel.try_send(())
                    {
                        panic!("async update channel closed unexpectedly")
                    }
                    if chrono::Utc::now() - cache.timestamp
                        < chrono::Duration::seconds(MAX_STALE_SECONDS)
                    {
                        return (cache.clone(), CacheState::Stale);
                    }
                };
            };
        }

        self.update_results_cache().await;
        let cache = self.cache.read().await;
        (cache.as_ref().unwrap().clone(), CacheState::Fresh)
    }
}

#[get("/")]
async fn index(data: web::Data<AppState<'_>>) -> impl Responder {
    let (TestResultsCache { results, timestamp }, cache_state) = data.get_results().await;
    let timestamp_template = timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let html = data
        .handlebars
        .render(
            "main",
            &json!({ "teams": results, "timestamp": timestamp_template }),
        )
        .unwrap();

    let mut response_builder = HttpResponse::Ok();

    if cache_state == CacheState::Fresh {
        let expiry: SystemTime = (timestamp + chrono::Duration::seconds(STALE_SECONDS)).into();
        response_builder.insert_header(("Expires", http::header::Expires(expiry.into())));
    } else {
        response_builder
            .insert_header(("Cache-Control", "public, max-age=1, stale-if-error=86400"));
    }

    response_builder
        .insert_header(("Link", "<https://cdn.jsdelivr.net/npm/bootstrap@5.3.0-alpha1/dist/css/bootstrap.min.css>; rel=preload"))
        .content_type(ContentType::html())
        .body(html)
}

// #[get("/favicon.ico")]
// async fn favicon() -> impl Responder {
//     HttpResponse::Ok()
//         .content_type("image/x-icon")
//         .body(include_bytes!("upload/favicon.ico").to_vec())
// }

handlebars_helper!(zero_pad: |x: i32| format!("{:02}", x));

#[actix_web::main]
async fn main() {
    let mut handlebars = handlebars::Handlebars::new();
    handlebars.register_helper("zero_pad", Box::new(zero_pad));
    handlebars
        .register_template_string("main", include_str!("index.html.hbs"))
        .unwrap();
    let (async_update_channel, mut async_update_receiver) = tokio::sync::mpsc::channel(1);
    let app_data = web::Data::new(AppState {
        cache: RwLock::new(None),
        handlebars,
        async_update_channel,
    });

    let (TestResultsCache { results, timestamp }, _) = app_data.get_results().await;
    let timestamp_template = timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    app_data
        .handlebars
        .render_to_write(
            "main",
            &json!({ "teams": results, "timestamp": timestamp_template }),
            &mut fs::File::create("upload/index.html").unwrap(),
        )
        .unwrap();

    // {
    //     let app_data = app_data.clone();
    //     tokio::spawn(async move {
    //         app_data.update_results_cache().await;
    //         while async_update_receiver.recv().await.is_some() {
    //             app_data.update_results_cache().await;
    //         }
    //     });
    // }

    // HttpServer::new(move || App::new().app_data(app_data.clone()).service(index))
    //     .bind(("127.0.0.1", 8080))
    //     .unwrap()
    //     .run()
    //     .await
    //     .unwrap();
}
