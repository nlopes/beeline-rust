/*! Honeycomb support for actix-web.

By default, the following items are added to the trace:
 - meta.type (always "http_request")
 - request.method
 - request.path
 - request.header.<name> (name is the same as the original header name but with dashes replaced with underscores)
 - response.status
 - response.body.size

# Usage

First add `beeline_actix_web` to your `Cargo.toml`:

```toml
[dependencies]
beeline_actix_web = "0.1"
```

You then instantiate the middleware and pass it to `.wrap()`:

```rust
use actix_web::{web, App, HttpResponse, HttpServer};
use beeline_actix_web::BeelineMiddleware;

fn health() -> HttpResponse {
    HttpResponse::Ok().finish()
}

fn main() -> std::io::Result<()> {
    let beeline = BeelineMiddleware::new_with_client(_client_);
    # if false {
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
//#![feature(associated_type_bounds)]

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::SystemTime;

use actix_service::{Service, Transform};
use actix_web::{
    dev::{BodySize, MessageBody, ResponseBody, ServiceRequest, ServiceResponse},
    http::{Method, StatusCode},
    web::Bytes,
    Error,
};
use beeline::{SafeTrace, Sender};
use futures::future::{ok, FutureResult};
use futures::{Async, Future, Poll};
use serde_json::json;

#[derive(Debug, Clone)]
#[must_use = "must be set up as middleware for actix-web"]
/// By default XXX: talk about the trace that gets sent
pub struct BeelineMiddleware<T>
where
    T: Sender + Clone,
{
    client: beeline::Client<T>,
    trace: SafeTrace,
}

impl<T: Sender + Clone> BeelineMiddleware<T> {
    // /// Start the beeline client
    // pub fn new(config: beeline::Config) -> Self {
    //     let client = beeline::init(config);
    //     let trace = client.new_trace(None);
    //     BeelineMiddleware {
    //         client: client,
    //         trace: trace.clone(),
    //     }
    // }

    /// Build with already started client
    pub fn new_with_client(client: beeline::Client<T>) -> Self {
        let trace = client.new_trace(None);
        Self {
            client: client,
            trace: trace.clone(),
        }
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
    type Transform = BeelineInnerMiddleware<S, T>;
    type Future = FutureResult<Self::Transform, Self::InitError>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(BeelineInnerMiddleware {
            service,
            inner: Arc::new(self.clone()),
        })
    }
}

#[doc(hidden)]
/// Middleware service for BeelineMiddleware
pub struct BeelineInnerMiddleware<S, T: Sender + Clone> {
    service: S,
    inner: Arc<BeelineMiddleware<T>>,
}

impl<T, S, B> Service for BeelineInnerMiddleware<S, T>
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: MessageBody,
    T: Sender + Clone,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<StreamLog<B, T>>;
    type Error = S::Error;
    type Future = BeelineServiceResponse<S, B, T>;

    fn poll_ready(&mut self) -> Poll<(), Self::Error> {
        self.service.poll_ready()
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
pub struct BeelineServiceResponse<S, B, T>
where
    B: MessageBody,
    S: Service,
    T: Sender + Clone,
{
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
    type Item = ServiceResponse<StreamLog<B, T>>;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let res = futures::try_ready!(self.fut.poll());

        let req = res.request();
        let inner = self.inner.clone();
        let method = req.method().clone();
        let path = req.path().to_string();
        let headers = req.headers();

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

        Ok(Async::Ready(res.map_body(move |head, body| {
            ResponseBody::Body(StreamLog {
                body,
                size: 0,
                clock: self.clock,
                inner,
                status: head.status,
                path: path.clone(),
                method,
            })
        })))
    }
}

#[doc(hidden)]
pub struct StreamLog<B, T: Sender + Clone> {
    body: ResponseBody<B>,
    size: usize,
    clock: SystemTime,
    inner: Arc<BeelineMiddleware<T>>,
    status: StatusCode,
    path: String,
    method: Method,
}

impl<B, T: Sender + Clone> Drop for StreamLog<B, T> {
    fn drop(&mut self) {
        self.inner
            .send(&self.path, &self.method, self.status, self.clock, self.size);
    }
}

impl<B: MessageBody, T: Sender + Clone> MessageBody for StreamLog<B, T> {
    fn size(&self) -> BodySize {
        self.body.size()
    }

    fn poll_next(&mut self) -> Poll<Option<Bytes>, Error> {
        match self.body.poll_next()? {
            Async::Ready(Some(chunk)) => {
                self.size += chunk.len();
                Ok(Async::Ready(Some(chunk)))
            }
            val => Ok(val),
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::test::{call_service, init_service, read_body, read_response, TestRequest};
    use actix_web::{web, App, HttpResponse};
    use beeline::{Client, Config};
    use libhoney::mock::TransmissionMock;
    use mockito;

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

        let config = Config {
            client_config: libhoney::Config {
                options: libhoney::client::Options {
                    api_host: api_host.to_string(),
                    api_key: "key".to_string(),
                    ..libhoney::client::Options::default()
                },
                transmission_options: libhoney::transmission::Options::default(),
            },
            service_name: Some("beeline-rust-test".to_string()),
        };

        beeline::test::init(config)
    }

    #[test]
    fn middleware_basic() {
        let middleware = BeelineMiddleware::new_with_client(new_client());
        let mut app = init_service(
            App::new()
                .wrap(middleware.clone())
                .service(web::resource("/").to(|| HttpResponse::Ok().json({}))),
        );

        let res = call_service(
            &mut app,
            TestRequest::with_uri("/")
                .header("content-type", "text/plain")
                .to_request(),
        );
        assert!(res.status().is_success());
        assert_eq!(read_body(res), Bytes::from_static(b"null"));
        let events = middleware.client.0.write().client.transmission.events();
        // TODO(nlopes): should I expose .fields from Event and also check content?
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn middleware_basic_failure() {
        let middleware = BeelineMiddleware::new_with_client(new_client());
        let mut app = init_service(
            App::new()
                .wrap(middleware.clone())
                .service(web::resource("/").to(|| HttpResponse::Ok())),
        );

        {
            let res = call_service(&mut app, TestRequest::with_uri("/missing").to_request());
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
        }
        let events = middleware.client.0.write().client.transmission.events();
        assert_eq!(events.len(), 1);
    }
}
