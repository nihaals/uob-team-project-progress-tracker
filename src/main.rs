#![warn(clippy::cast_lossless)]
#![warn(clippy::cast_possible_wrap)]
#![warn(clippy::default_trait_access)]
#![warn(clippy::else_if_without_else)]
#![warn(clippy::empty_enum)]
#![warn(clippy::empty_line_after_outer_attr)]
#![warn(clippy::enum_glob_use)]
#![warn(clippy::equatable_if_let)]
#![warn(clippy::float_cmp)]
#![warn(clippy::fn_params_excessive_bools)]
#![warn(clippy::get_unwrap)]
#![warn(clippy::inefficient_to_string)]
#![warn(clippy::integer_division)]
#![warn(clippy::let_unit_value)]
#![warn(clippy::linkedlist)]
#![warn(clippy::lossy_float_literal)]
#![warn(clippy::macro_use_imports)]
#![warn(clippy::manual_assert)]
#![warn(clippy::manual_ok_or)]
#![warn(clippy::many_single_char_names)]
#![warn(clippy::map_err_ignore)]
#![warn(clippy::map_unwrap_or)]
#![warn(clippy::match_bool)]
#![warn(clippy::match_on_vec_items)]
#![warn(clippy::match_same_arms)]
#![warn(clippy::match_wild_err_arm)]
#![warn(clippy::match_wildcard_for_single_variants)]
#![warn(clippy::mem_forget)]
#![warn(clippy::missing_const_for_fn)]
#![warn(clippy::must_use_candidate)]
#![warn(clippy::mut_mut)]
#![warn(clippy::negative_feature_names)]
#![warn(non_ascii_idents)]
#![warn(clippy::option_option)]
#![warn(clippy::redundant_feature_names)]
#![warn(clippy::redundant_pub_crate)]
#![warn(clippy::str_to_string)]
#![warn(clippy::string_to_string)]
#![warn(clippy::trait_duplication_in_bounds)]
#![warn(clippy::unused_async)]
#![warn(clippy::unused_self)]
#![warn(clippy::use_self)]
#![warn(clippy::wildcard_dependencies)]
#![warn(clippy::wildcard_imports)]
#![warn(clippy::zero_sized_map_values)]

mod status;
mod teams;

use status::{get_results, HttpProtocol, ProtocolResult, TeamResult};

use actix_web::{
    get,
    http::{self, header::ContentType},
    web, App, HttpResponse, HttpServer, Responder,
};
use handlebars::handlebars_helper;
use serde::Serialize;
use serde_json::json;
use std::fs;
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
    #[serde(rename = "Invalid certificate")]
    InvalidCertificate,
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
            Self::Correct => "link-success",
            Self::NearlyCorrect => "link-warning",
            Self::Incorrect => "link-danger",
        }
        .to_owned()
    }

    fn to_alt_text(&self) -> String {
        match self {
            Self::Correct => "Correct",
            Self::NearlyCorrect => "Nearly correct",
            Self::Incorrect => "Incorrect",
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

    fn from_result(request_result: ProtocolResult, protocol: HttpProtocol) -> Self {
        match request_result {
            ProtocolResult::Ok(status_code) => Self::new(
                RequestResultResponseTemplate::Ok { status_code },
                match protocol {
                    HttpProtocol::Http => RequestResultStatus::NearlyCorrect,
                    HttpProtocol::Https => RequestResultStatus::Correct,
                },
            ),
            ProtocolResult::NginxDefaultPage(status_code) => Self::new(
                RequestResultResponseTemplate::Ok { status_code },
                RequestResultStatus::NearlyCorrect,
            ),
            ProtocolResult::CorrectRedirect(status_code) => Self::new(
                RequestResultResponseTemplate::Redirect { status_code },
                RequestResultStatus::Correct,
            ),
            ProtocolResult::IncorrectRedirect(status_code) => Self::new(
                RequestResultResponseTemplate::Redirect { status_code },
                RequestResultStatus::NearlyCorrect,
            ),
            ProtocolResult::UnexpectedResponse(status_code) => match status_code {
                418 => Self::new(
                    RequestResultResponseTemplate::Teapot,
                    RequestResultStatus::NearlyCorrect,
                ),
                _ => Self::new(
                    RequestResultResponseTemplate::UnexpectedResponse { status_code },
                    RequestResultStatus::Incorrect,
                ),
            },
            ProtocolResult::Timeout => Self::new(
                RequestResultResponseTemplate::Timeout,
                RequestResultStatus::Incorrect,
            ),
            ProtocolResult::UntrustedCertificate => Self::new(
                RequestResultResponseTemplate::UntrustedCertificate,
                RequestResultStatus::NearlyCorrect,
            ),
            ProtocolResult::InvalidCertificate => Self::new(
                RequestResultResponseTemplate::InvalidCertificate,
                RequestResultStatus::NearlyCorrect,
            ),
            ProtocolResult::FailedConnect => Self::new(
                RequestResultResponseTemplate::FailedConnect,
                RequestResultStatus::Incorrect,
            ),
            ProtocolResult::Error(_) => Self::new(
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
        Self {
            team: team_result.team,
            http: RequestResultTemplate::from_result(team_result.http, HttpProtocol::Http),
            https: RequestResultTemplate::from_result(team_result.https, HttpProtocol::Https),
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

// #[get("/")]
// async fn index(data: web::Data<AppState<'_>>) -> impl Responder {
//     let (TestResultsCache { results, timestamp }, cache_state) = data.get_results().await;
//     let timestamp_template = timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
//     let html = data
//         .handlebars
//         .render(
//             "main",
//             &json!({ "teams": results, "timestamp": timestamp_template }),
//         )
//         .unwrap();

//     let mut response_builder = HttpResponse::Ok();

//     if cache_state == CacheState::Fresh {
//         let expiry: SystemTime = (timestamp + chrono::Duration::seconds(STALE_SECONDS)).into();
//         response_builder.insert_header(("Expires", http::header::Expires(expiry.into())));
//     } else {
//         response_builder
//             .insert_header(("Cache-Control", "public, max-age=1, stale-if-error=86400"));
//     }

//     response_builder
//         .insert_header(("Link", "<https://cdn.jsdelivr.net/npm/bootstrap@5.3.0-alpha1/dist/css/bootstrap.min.css>; rel=preload"))
//         .content_type(ContentType::html())
//         .body(html)
// }

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
