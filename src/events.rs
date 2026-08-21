use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;

use crate::state::AppState;

pub(crate) async fn stream_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.events.subscribe();
    let stream = BroadcastStream::new(receiver).filter_map(|result| async move {
        match result {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => Some(Ok(Event::default().data(json))),
                Err(e) => {
                    tracing::warn!("Failed to serialize library event: {e}");
                    None
                }
            },

            Err(_lagged) => None,
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
