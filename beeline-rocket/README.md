
[![docs.rs](https://docs.rs/beeline-rust/badge.svg)](https://docs.rs/beeline-rust)
[![crates.io](https://img.shields.io/crates/v/beeline-rocket.svg)](https://crates.io/crates/beeline-rocket)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/nlopes/beeline-rust/blob/master/beeline-rocket/LICENSE)
[![Build Status](https://travis-ci.org/nlopes/beeline-rust.svg?branch=master)](https://travis-ci.org/nlopes/beeline-rust)

# beeline-rocket

Honeycomb support for Rocket.

By default, the following fields are added to the trace:
 - `meta.type` (always "http_request")
 - `request.method`
 - `request.path`
 - `request.header.<name>` (name is the same as the original header name but with dashes replaced with underscores)
   - example: `request.header.content_type`
 - `response.status`
 - `response.body.size`

## Usage

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

