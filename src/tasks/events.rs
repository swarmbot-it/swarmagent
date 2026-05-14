use std::process;
use std::sync::Arc;

use bollard::Docker;
use futures_util::StreamExt;
use tracing::error;

use crate::sink::Sink;

pub async fn run(docker: Docker, sink: Arc<Sink>) {
    let mut stream = docker.events::<String>(None);
    while let Some(item) = stream.next().await {
        match item {
            Ok(msg) => {
                if let Err(e) = sink.post_event("event", &msg).await {
                    error!(error = %e, "Event sending failed");
                }
            }
            Err(e) => {
                error!(error = %e, "Event channel error");
                process::exit(1);
            }
        }
    }
    error!("Event channel closed");
    process::exit(1);
}
