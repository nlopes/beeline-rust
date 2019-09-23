/*! Honeycomb support for Rocket.

By default, the following fields are added to the trace:
 - `meta.type` (always "http_request")
 - `request.method`
 - `request.path`
 - `request.header.<name>` (name is the same as the original header name but with dashes replaced with underscores)
   - example: `request.header.content_type`
 - `response.status`
 - `response.body.size`

# Usage

First add `beeline_rocket` to your `Cargo.toml`:

```toml
[dependencies]
beeline_rocket = "0.1"
```

You then instantiate the middleware and pass it to `.wrap()`:

```rust
#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

use beeline::{init, Config};
use beeline_rocket::BeelineMiddleware;

#[get("/")]
fn index() -> &'static str {
    "Hello, world!"
}

fn main() {
    # if false {
    let client = init(Config::default());
    let middleware = BeelineMiddleware::new(client);
    rocket::ignite()
        .attach(middleware)
        .mount("/", routes![index])
        .launch();
    # }
}
```

 */

#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

use rocket::fairing::{Fairing, Info, Kind};
use rocket::{Data, Request, Response, Rocket};
use serde_json::{json, Value};

use beeline::{trace::SafeSpan, trace::SafeTrace, trace::TraceSender, Client, Sender};

#[derive(Debug, Clone)]
pub struct BeelineMiddleware<S: Sender + Send + Sync + Clone> {
    client: Client<S>,
}

impl<S> BeelineMiddleware<S>
where
    S: Sender + Send + Sync + Clone,
{
    pub fn new(client: Client<S>) -> Self {
        Self { client }
    }
}

#[derive(Debug)]
struct InternalTrace {
    trace: Option<SafeTrace>,
    span: Option<SafeSpan>,
}

impl<S> Fairing for BeelineMiddleware<S>
where
    S: Sender + Send + Sync + 'static + Clone,
{
    fn info(&self) -> Info {
        Info {
            name: "Beeline Middleware",
            kind: Kind::Launch | Kind::Request | Kind::Response,
        }
    }

    fn on_launch(&self, _: &Rocket) {
        let mut client = self.client.clone();
        client.add_field("rocket", Value::String("experiment".to_string()));
    }

    fn on_request(&self, request: &mut Request, _: &Data) {
        let mut client = self.client.clone();
        let trace = client.new_trace(None).clone();
        let rs = trace.lock().get_root_span();
        let child = rs.lock().create_child(&mut client);
        if let Some(span) = child.clone() {
            let mut span_guard = span.lock();
            for header in request.headers().iter() {
                span_guard.add_field(
                    &format!(
                        "request.header.{}",
                        header.name.as_str().to_lowercase().replace("-", "_")
                    ),
                    json!(header.value()),
                );
            }
            span_guard.add_field("meta.type", json!("http_request"));
            span_guard.add_field("request.method", json!(request.method().as_str()));
            span_guard.add_field("request.path", json!(request.uri().path()));
        }
        request.local_cache(|| InternalTrace {
            trace: Some(trace.clone()),
            span: child.clone(),
        });
    }

    fn on_response(&self, request: &Request, response: &mut Response) {
        let mut client = self.client.clone();
        let internal_trace: &InternalTrace = request.local_cache(|| InternalTrace {
            trace: None,
            span: None,
        });
        if let Some(span) = &internal_trace.span {
            let mut span_guard = span.lock();
            span_guard.add_field("response.status_code", json!(response.status().code));
            if let Some(b) = response.body() {
                let size = match b {
                    rocket::response::Body::Sized(_, size) => size,
                    rocket::response::Body::Chunked(_, size) => size,
                };
                span_guard.add_field("response.body.size", json!(size));
            }
        }
        if let Some(trace) = &internal_trace.trace {
            trace.send(&mut client);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use beeline::test::TransmissionMock;
    use beeline::Config;
    use rocket::local::Client as RocketClient;
    use rocket::Rocket;

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
        config.service_name = Some("beeline-rocket-test".to_string());

        beeline::test::init(config)
    }

    #[get("/")]
    fn index() -> &'static str {
        "Hello, world!"
    }

    fn setup<S: Clone + Sender + Sync + Send + 'static>(client: Client<S>) -> Rocket {
        let middleware = BeelineMiddleware::new(client);
        rocket::ignite()
            .attach(middleware)
            .mount("/", routes![index])
    }

    #[test]
    fn test_setup() {
        let beeline_client = new_client();
        let client = RocketClient::new(setup(beeline_client.clone())).unwrap();
        let mut response = client.get("/").dispatch();
        assert_eq!(response.body_string(), Some("Hello, world!".into()));

        let events = beeline_client.0.write().client.transmission.events();
        // 2 because of the original trace + the one we create on every call
        assert_eq!(events.len(), 2);
        let _ = client.get("/").dispatch();
        let events = beeline_client.0.write().client.transmission.events();
        assert_eq!(events.len(), 4);
    }
}
