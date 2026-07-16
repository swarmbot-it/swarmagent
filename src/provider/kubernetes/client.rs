//! Minimal in-cluster Kubernetes REST client.
//!
//! The agent needs exactly four API calls (read node, list pods, watch pods,
//! kubelet `stats/summary`), so instead of pulling in the full `kube` crate
//! this module drives the REST API directly with the `reqwest` client that is
//! already a dependency. Authentication uses the mounted ServiceAccount
//! token, which is re-read on every request because bound tokens are rotated
//! by the kubelet.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;

use crate::detect::SERVICEACCOUNT_DIR;

/// Timeout for one-shot API calls (does not apply to watch streams).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// In-cluster Kubernetes API client.
pub struct KubeClient {
	/// `https://<KUBERNETES_SERVICE_HOST>:<KUBERNETES_SERVICE_PORT>`.
	base: String,
	token_path: PathBuf,
	/// Client for the API server (cluster CA pinned).
	http: reqwest::Client,
	/// Client for direct kubelet calls (optionally skips TLS verification —
	/// k3s kubelets serve a self-signed certificate).
	kubelet: reqwest::Client,
}

impl KubeClient {
	/// Builds a client from the standard in-cluster environment:
	/// `KUBERNETES_SERVICE_HOST`/`KUBERNETES_SERVICE_PORT` and the
	/// ServiceAccount files under [`SERVICEACCOUNT_DIR`].
	pub fn in_cluster(kubelet_insecure_tls: bool) -> anyhow::Result<Self> {
		let host = std::env::var("KUBERNETES_SERVICE_HOST")
			.context("KUBERNETES_SERVICE_HOST not set (agent must run inside the cluster)")?;
		let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_string());
		let sa_dir = PathBuf::from(SERVICEACCOUNT_DIR);

		let ca_pem = std::fs::read(sa_dir.join("ca.crt"))
			.context("read ServiceAccount ca.crt (is the token mounted?)")?;
		let ca = reqwest::Certificate::from_pem(&ca_pem).context("parse cluster CA")?;

		let user_agent = concat!("swarmagent/", env!("CARGO_PKG_VERSION"));
		let http = reqwest::Client::builder()
			.add_root_certificate(ca.clone())
			.user_agent(user_agent)
			.build()
			.context("build API server client")?;
		let kubelet = reqwest::Client::builder()
			.add_root_certificate(ca)
			.danger_accept_invalid_certs(kubelet_insecure_tls)
			.user_agent(user_agent)
			.timeout(REQUEST_TIMEOUT)
			.build()
			.context("build kubelet client")?;

		Ok(Self {
			base: format!("https://{host}:{port}"),
			token_path: sa_dir.join("token"),
			http,
			kubelet,
		})
	}

	/// Current ServiceAccount bearer token.
	///
	/// Re-read from disk on every call: bound tokens expire (~1 h) and the
	/// kubelet refreshes the mounted file in place.
	fn token(&self) -> anyhow::Result<String> {
		let raw = std::fs::read_to_string(&self.token_path).context("read ServiceAccount token")?;
		Ok(raw.trim().to_string())
	}

	/// `GET {base}{path}` against the API server; returns the parsed JSON body.
	pub async fn get_json(&self, path: &str) -> anyhow::Result<serde_json::Value> {
		let url = format!("{}{}", self.base, path);
		let resp = self
			.http
			.get(&url)
			.bearer_auth(self.token()?)
			.timeout(REQUEST_TIMEOUT)
			.send()
			.await
			.with_context(|| format!("GET {path}"))?;
		let status = resp.status();
		if !status.is_success() {
			let body = resp.text().await.unwrap_or_default();
			anyhow::bail!("GET {path} returned {status}: {body}");
		}
		resp.json()
			.await
			.with_context(|| format!("parse JSON from {path}"))
	}

	/// `GET {url}` directly against a kubelet (port 10250); returns parsed JSON.
	pub async fn kubelet_get_json(&self, url: &str) -> anyhow::Result<serde_json::Value> {
		let resp = self
			.kubelet
			.get(url)
			.bearer_auth(self.token()?)
			.send()
			.await
			.with_context(|| format!("GET {url}"))?;
		let status = resp.status();
		if !status.is_success() {
			let body = resp.text().await.unwrap_or_default();
			anyhow::bail!("GET {url} returned {status}: {body}");
		}
		resp.json()
			.await
			.with_context(|| format!("parse JSON from {url}"))
	}

	/// Opens a streaming watch request against the API server.
	///
	/// The caller consumes the chunked response body as JSON lines. No
	/// request timeout is applied — watches are long-lived by design.
	pub async fn watch(&self, path: &str) -> anyhow::Result<reqwest::Response> {
		let url = format!("{}{}", self.base, path);
		let resp = self
			.http
			.get(&url)
			.bearer_auth(self.token()?)
			.send()
			.await
			.with_context(|| format!("WATCH {path}"))?;
		let status = resp.status();
		if !status.is_success() {
			let body = resp.text().await.unwrap_or_default();
			anyhow::bail!("WATCH {path} returned {status}: {body}");
		}
		Ok(resp)
	}
}
