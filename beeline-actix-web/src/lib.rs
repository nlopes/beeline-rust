/*! Honeycomb support for actix-web.

By default, the following fields are added to the trace:
 - `meta.type` (always "http_request")
 - `request.method`
 - `request.path`
 - `request.header.<name>` (name is the same as the original header name but with dashes replaced with underscores)
   - example: `request.header.content_type`
 - `response.status`
 - `response.body.size`

# Usage

First add `beeline_actix_web` to your `Cargo.toml`:

```toml
[dependencies]
beeline_actix_web = "0.1"
```

You then instantiate the middleware and pass it to `.wrap()`:

```rust
use actix_web::{web, App, HttpResponse, HttpServer};
use beeline::{init, Config};
use beeline_actix_web::BeelineMiddleware;

fn health() -> HttpResponse {
    HttpResponse::Ok().finish()
}

fn main() -> std::io::Result<()> {
    # if false {
    let client = init(Config::default());
    let beeline = BeelineMiddleware::new(client);
    HttpServer::new(move || {
        App::new()
            .wrap(beeline.clone())
            .service(web::resource("/health").to(health))
    })
    .bind("127.0.0.1:8080")?
    .run();
    # }
    Ok(())
}
```

 */

#![deny(missing_docs)]

use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use actix_service::{Service, Transform};
use actix_web::{
    dev::{BodySize, MessageBody, ResponseBody, ServiceRequest, ServiceResponse},
    http::{Method, StatusCode},
    web::Bytes,
    Error,
};
use beeline::{Client, SafeTrace, Sender};
use futures::{
    future::{ok, Ready},
    task::{Context, Poll},
    Future,
};
use pin_project::{pin_project, pinned_drop};
use serde_json::json;

#[derive(Debug, Clone)]
#[must_use = "must be set up as middleware for actix-web"]
/// By default XXX: talk about the trace that gets sent
pub struct BeelineMiddleware<T>
where
    T: Sender + Clone,
{
    client: Client<T>,
    trace: SafeTrace,
}

impl<T: Sender + Clone> BeelineMiddleware<T> {
    /// Build with already started client
    pub fn new(client: Client<T>) -> Self {
        let trace = client.new_trace(None);
        Self { client, trace }
    }

    fn send(
        &self,
        path: &str,
        method: &Method,
        status: StatusCode,
        clock: SystemTime,
        size: usize,
    ) {
        let trace = self.trace.clone();
        let rs = trace.lock().get_root_span();
        {
            let mut guard = rs.lock();
            {
                guard.add_field("meta.type", json!("http_request"));
                guard.add_field("request.method", json!(method.to_string()));
                guard.add_field("request.path", json!(path));
                if let Ok(elapsed) = clock.elapsed() {
                    let duration = (elapsed.as_secs() as f64)
                        + f64::from(elapsed.subsec_nanos()) / 1_000_000_000_f64;
                    guard.add_field("duration_ms", json!(duration));
                }
                guard.add_field("response.status", json!(status.as_u16()));
                guard.add_field("response.body.size", json!(size));
            }
            let mut span_client = self.client.clone();
            guard.send(&mut span_client)
        }
    }
}

impl<S, B, T> Transform<S> for BeelineMiddleware<T>
where
    B: MessageBody,
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    T: Sender + Clone,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<StreamLog<B, T>>;
    type Error = Error;
    type InitError = ();
    type Transform = BeelineService<S, T>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(BeelineService {
            service,
            inner: Arc::new(self.clone()),
        })
    }
}

#[doc(hidden)]
/// Middleware service for BeelineMiddleware
pub struct BeelineService<S, T: Sender + Clone> {
    service: S,
    inner: Arc<BeelineMiddleware<T>>,
}

impl<T, S, B> Service for BeelineService<S, T>
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    B: MessageBody,
    T: Sender + Clone,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<StreamLog<B, T>>;
    type Error = S::Error;
    type Future = BeelineServiceResponse<S, B, T>;

    fn poll_ready(&mut self, ct: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(ct)
    }

    fn call(&mut self, req: ServiceRequest) -> Self::Future {
        BeelineServiceResponse {
            fut: self.service.call(req),
            clock: SystemTime::now(),
            inner: self.inner.clone(),
            _t: PhantomData,
        }
    }
}

#[doc(hidden)]
#[pin_project::pin_project]
pub struct BeelineServiceResponse<S, B, T>
where
    B: MessageBody,
    S: Service,
    T: Sender + Clone,
{
    #[pin]
    fut: S::Future,
    clock: SystemTime,
    inner: Arc<BeelineMiddleware<T>>,
    _t: PhantomData<(B,)>,
}

impl<S, B, T> Future for BeelineServiceResponse<S, B, T>
where
    B: MessageBody,
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    T: Sender + Clone,
{
    type Output = Result<ServiceResponse<StreamLog<B, T>>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        let res = match futures::ready!(this.fut.poll(cx)) {
            Ok(res) => res,
            Err(e) => return Poll::Ready(Err(e)),
        };

        let req = res.request();
        let inner = this.inner.clone();
        let method = req.method().clone();
        let path = req.path().to_string();
        let headers = req.headers();
        let time = *this.clock;
        let trace = inner.trace.clone();
        let rs = trace.lock().get_root_span();
        {
            let mut guard = rs.lock();
            {
                for (name, value) in headers.iter() {
                    guard.add_field(
                        &format!(
                            "request.header.{}",
                            name.as_str().to_lowercase().replace("-", "_")
                        ),
                        match value.to_str() {
                            Ok(v) => json!(v),
                            _ => json!("<error converting to str>"),
                        },
                    );
                }
            }
        }

        Poll::Ready(Ok(res.map_body(move |head, body| {
            ResponseBody::Body(StreamLog {
                body,
                size: 0,
                clock: time,
                inner,
                status: head.status,
                path: path.clone(),
                method,
            })
        })))
    }
}

#[doc(hidden)]
#[pin_project(PinnedDrop)]
pub struct StreamLog<B, T: Sender + Clone> {
    #[pin]
    body: ResponseBody<B>,
    size: usize,
    clock: SystemTime,
    inner: Arc<BeelineMiddleware<T>>,
    status: StatusCode,
    path: String,
    method: Method,
}

#[pinned_drop]
impl<B, T: Sender + Clone> PinnedDrop for StreamLog<B, T> {
    fn drop(self: Pin<&mut Self>) {
        self.inner
            .send(&self.path, &self.method, self.status, self.clock, self.size);
    }
}

impl<B: MessageBody, T: Sender + Clone> MessageBody for StreamLog<B, T> {
    fn size(&self) -> BodySize {
        self.body.size()
    }

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Error>>> {
        let this = self.project();
        match MessageBody::poll_next(this.body, cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                *this.size += chunk.len();
                Poll::Ready(Some(Ok(chunk)))
            }
            val => val,
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::rt as actix_rt;
    use actix_web::test::{call_service, init_service, read_body, TestRequest};
    use actix_web::{web, App, HttpResponse};
    use beeline::{Client, Config};
    use libhoney::mock::TransmissionMock;

    use super::*;

    fn new_client() -> Client<TransmissionMock> {
        let api_host = &mockito::server_url();
        let _m = mockito::mock(
            "POST",
            mockito::Matcher::Regex(r"/1/batch/(.*)$".to_string()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("[{ \"status\": 202 }]")
        .create();

        let mut config = Config::default();
        config.client_config.options.api_host = api_host.to_string();
        config.client_config.options.api_key = "key".to_string();
        config.service_name = Some("beeline-actix-web-test".to_string());

        beeline::test::init(config)
    }

    #[actix_rt::test]
    async fn middleware_basic() {
        let middleware = BeelineMiddleware::new(new_client());
        let mut app = init_service(
            App::new()
                .wrap(middleware.clone())
                .service(web::resource("/").to(|| HttpResponse::Ok().json(()))),
        )
        .await;

        let res = call_service(
            &mut app,
            TestRequest::with_uri("/")
                .header("content-type", "text/plain")
                .to_request(),
        )
        .await;
        assert!(res.status().is_success());
        assert_eq!(read_body(res).await, Bytes::from_static(b"null"));
        let events = middleware.client.0.write().client.transmission.events();
        // TODO(nlopes): should I expose .fields from Event and also check content?
        assert_eq!(events.len(), 1);
    }

    #[actix_rt::test]
    async fn middleware_basic_failure() {
        let middleware = BeelineMiddleware::new(new_client());
        let mut app = init_service(
            App::new()
                .wrap(middleware.clone())
                .service(web::resource("/").to(HttpResponse::Ok)),
        )
        .await;

        {
            let res = call_service(&mut app, TestRequest::with_uri("/missing").to_request()).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
        }
        let events = middleware.client.0.write().client.transmission.events();
        assert_eq!(events.len(), 1);
    }
}
