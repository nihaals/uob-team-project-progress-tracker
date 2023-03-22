mod status;
mod teams;

use status::{get_results, Protocol, RequestResult, TeamResult};

use actix_web::{
    get,
    http::{self, header::ContentType},
    web, App, HttpResponse, HttpServer, Responder,
};
use handlebars::handlebars_helper;
use serde::Serialize;
use serde_json::json;
use std::{fs, time::SystemTime};
use teams::Team;
use tokio::sync::RwLock;

const STALE_SECONDS: i64 = 60;
const MAX_STALE_SECONDS: i64 = 60 * 60;

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

impl RequestResultTemplate {
    fn from_result(request_result: RequestResult, protocol: Protocol) -> RequestResultTemplate {
        match request_result {
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
            http: RequestResultTemplate::from_result(team_result.http, Protocol::Http),
            https: RequestResultTemplate::from_result(team_result.https, Protocol::Https),
        }
    }
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
