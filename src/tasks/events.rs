//! Docker event collector with automatic reconnection.
//!
//! This task subscribes to `docker events` and forwards each event to
//! Swarmboty via [`Sink::post_event`].  Only event types relevant to
//! Swarmboty are requested (see [`RELEVANT_TYPES`]), which reduces noise
//! from build and plugin activity on busy CI hosts.
//!
//! If the stream closes or errors, the task reconnects with exponential
//! back-off (1 s → 2 s → … → 60 s cap) so the agent survives a Docker
//! daemon restart without manual intervention.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bollard::system::EventsOptions;
use bollard::Docker;
use futures_util::StreamExt;
use tracing::{error, info, warn};

use crate::sink::Sink;

/// Docker event types forwarded to Swarmboty.
///
/// Filtering here reduces noise from build/plugin events in active CI hosts.
const RELEVANT_TYPES: &[&str] = &[
	"container",
	"network",
	"service",
	"node",
	"secret",
	"config",
	"volume",
];

/// Builds the [`EventsOptions`] filter that restricts the stream to
/// [`RELEVANT_TYPES`].
fn events_options() -> EventsOptions<String> {
	let mut filters: HashMap<String, Vec<String>> = HashMap::new();
	filters.insert(
		"type".into(),
		RELEVANT_TYPES.iter().map(|s| s.to_string()).collect(),
	);
	EventsOptions {
		since: None,
		until: None,
		filters,
	}
}

/// Drains one Docker event stream until it errors or the daemon closes it.
///
/// Returns `true` when the stream ended cleanly (EOF without an error),
/// `false` when an error was received from the stream.
async fn drain_stream(docker: &Docker, sink: &Arc<Sink>) -> bool {
	let mut stream = docker.events(Some(events_options()));
	while let Some(item) = stream.next().await {
		match item {
			Ok(msg) => {
				if let Err(e) = sink.post_event("event", &msg).await {
					error!(error = %e, "Event forwarding failed");
				}
			}
			Err(e) => {
				error!(error = %e, "Docker event stream error");
				return false;
			}
		}
	}
	// Stream closed without an error (e.g. daemon restart).
	false
}

/// Runs the event collector indefinitely.
///
/// On each iteration [`drain_stream`] is called; when it returns the task
/// sleeps for `delay` before reconnecting, then doubles `delay` up to a
/// maximum of 60 seconds.
pub async fn run(docker: Docker, sink: Arc<Sink>) {
	let mut delay = Duration::from_secs(1);
	loop {
		info!("Docker event stream starting");
		let clean = drain_stream(&docker, &sink).await;
		if clean {
			warn!("Docker event stream closed; reconnecting in {:?}", delay);
		} else {
			error!("Docker event stream failed; reconnecting in {:?}", delay);
		}
		tokio::time::sleep(delay).await;
		delay = (delay * 2).min(Duration::from_secs(60));
	}
}
