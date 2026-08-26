use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::{
    db::queries::deployments,
    infra::containers::{docker::DockerClient, services::ServiceId},
    state::AppState,
    utils::api_response::ApiError,
};

const DEFAULT_TAIL: i64 = 200;
const MAX_TAIL: i64 = 5000;

#[derive(Deserialize)]
pub struct LogQuery {
    pub tail: Option<i64>,
}

#[derive(Serialize)]
struct ContainerMarker<'a> {
    container: &'a str,
    source: &'a str,
    release: Option<&'a str>,
}

#[derive(Serialize)]
struct StreamEnd<'a> {
    container: &'a str,
    reason: &'a str,
}

fn docker(state: &AppState) -> Result<DockerClient, ApiError> {
    state.docker.clone().ok_or_else(|| {
        ApiError::unavailable("docker is not available on this host, logs cannot be streamed")
    })
}

fn tail_of(query: &LogQuery) -> i64 {
    query.tail.unwrap_or(DEFAULT_TAIL).clamp(0, MAX_TAIL)
}

fn stream_container(
    docker: DockerClient,
    container: String,
    source: String,
    release: Option<String>,
    tail: i64,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let marker = Event::default().event("container").data(
        serde_json::to_string(&ContainerMarker {
            container: &container,
            source: &source,
            release: release.as_deref(),
        })
        .unwrap_or_default(),
    );

    let ended = container.clone();
    let lines = docker.follow_logs(&container, tail).map(move |chunk| {
        Ok(match chunk {
            Ok(text) => Event::default()
                .event("line")
                .data(text.strip_suffix('\n').unwrap_or(&text)),
            Err(e) => Event::default()
                .event("error")
                .data(format!("{}", e.root_cause())),
        })
    });

    let tail_event = futures_util::stream::once(async move {
        Ok(Event::default().event("end").data(
            serde_json::to_string(&StreamEnd {
                container: &ended,
                reason: "the container stopped or was replaced",
            })
            .unwrap_or_default(),
        ))
    });

    let body = futures_util::stream::once(async move { Ok(marker) })
        .chain(lines)
        .chain(tail_event);

    Sse::new(body).keep_alive(KeepAlive::default())
}

pub async fn deployment_logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let docker = docker(&state)?;

    let deployment = deployments::fetch(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("deployment {id} not found")))?;

    let release_id = deployment
        .active_release_id
        .as_ref()
        .or(deployment.desired_release_id.as_ref())
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "{} has no release yet, so there is nothing to stream",
                deployment.name
            ))
        })?;

    let release = deployments::releases(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .find(|r| &r.id == release_id)
        .ok_or_else(|| ApiError::not_found("the deployment's release is missing".to_string()))?;

    let container = format!("arges-{}-{}", deployment.name, release.tag);

    if docker
        .inspect(&container)
        .await
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found(format!(
            "{container} is not running yet, the deployment is {:?}",
            deployment.status
        )));
    }

    Ok(stream_container(
        docker,
        container,
        deployment.name,
        Some(release.tag),
        tail_of(&query),
    ))
}

pub async fn service_logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let docker = docker(&state)?;

    let service = ServiceId::parse(&id)
        .ok_or_else(|| ApiError::not_found(format!("no such service {id}")))?;
    let container = service.container_name().to_string();

    if docker
        .inspect(&container)
        .await
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found(format!(
            "{container} is not running, enable the service first"
        )));
    }

    Ok(stream_container(
        docker,
        container,
        service.as_str().to_string(),
        None,
        tail_of(&query),
    ))
}
