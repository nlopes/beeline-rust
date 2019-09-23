
[![docs.rs](https://docs.rs/beeline-rust/badge.svg)](https://docs.rs/beeline-rust)
[![crates.io](https://img.shields.io/crates/v/beeline-actix-web.svg)](https://crates.io/crates/beeline-actix-web)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/nlopes/beeline-rust/blob/master/beeline-actix-web/LICENSE)
[![Build Status](https://travis-ci.org/nlopes/beeline-rust.svg?branch=master)](https://travis-ci.org/nlopes/beeline-rust)

# beeline-actix-web

Honeycomb support for actix-web.

By default, the following fields are added to the trace:
 - `meta.type` (always "http_request")
 - `request.method`
 - `request.path`
 - `request.header.<name>` (name is the same as the original header name but with dashes replaced with underscores)
   - example: `request.header.content_type`
 - `response.status`
 - `response.body.size`

## Usage

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

