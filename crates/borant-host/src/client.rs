//! A small HTTP client for the local API over the Unix socket.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow};
use borant_core::proto::{ApiError, Reply};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone)]
pub struct LocalClient {
    socket: PathBuf,
    connect_timeout: Duration,
}

/// The outcome of a call: the daemon was unreachable, or it replied.
#[derive(Debug)]
pub enum Outcome<T> {
    Down(anyhow::Error),
    Reply(Result<T, ApiError>),
}

impl LocalClient {
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            connect_timeout: Duration::from_secs(2),
        }
    }

    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> anyhow::Result<Bytes> {
        let stream = tokio::time::timeout(
            self.connect_timeout,
            tokio::net::UnixStream::connect(&self.socket),
        )
        .await
        .map_err(|_| anyhow!("timed out connecting to {}", self.socket.display()))?
        .with_context(|| format!("connect to {}", self.socket.display()))?;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "borant")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))?;
        let resp = tokio::time::timeout(timeout, sender.send_request(req))
            .await
            .map_err(|_| anyhow!("the daemon did not answer in {timeout:?}"))??;
        let bytes = tokio::time::timeout(timeout, resp.into_body().collect())
            .await
            .map_err(|_| anyhow!("the daemon did not finish the reply in {timeout:?}"))??
            .to_bytes();
        Ok(bytes)
    }

    async fn reply<R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Outcome<R> {
        match self.send(method, path, body, timeout).await {
            Err(e) => Outcome::Down(e),
            Ok(bytes) => Outcome::Reply(match serde_json::from_slice::<Reply>(&bytes) {
                Ok(r) => r.parse(),
                Err(e) => Err(ApiError::internal(format!(
                    "bad reply from the daemon: {e}"
                ))),
            }),
        }
    }

    pub async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
        timeout: Duration,
    ) -> Outcome<R> {
        match serde_json::to_vec(body) {
            Ok(b) => self.reply(Method::POST, path, Some(b), timeout).await,
            Err(e) => Outcome::Reply(Err(ApiError::internal(e.to_string()))),
        }
    }

    pub async fn get<R: DeserializeOwned>(&self, path: &str, timeout: Duration) -> Outcome<R> {
        self.reply(Method::GET, path, None, timeout).await
    }
}
