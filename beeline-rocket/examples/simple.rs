#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

use beeline::Config;
use beeline_rocket::BeelineMiddleware;

#[get("/")]
fn index() -> &'static str {
    "Hello, world!"
}

#[post("/")]
fn index_post() -> &'static str {
    "Hello, world through a post!"
}

fn main() {
    let mut config = Config::default();
    if let Some(api_key) = option_env!("HONEYCOMB_API_KEY") {
        config.client_config.options.api_key = api_key.to_string();
    }
    if let Some(dataset) = option_env!("HONEYCOMB_DATASET") {
        config.client_config.options.dataset = dataset.to_string();
    }
    config.service_name = Some("beeline-rocket-simple".to_string());

    let client = beeline::init(config);
    let middleware = BeelineMiddleware::new(client);

    rocket::ignite()
        .attach(middleware)
        .mount("/", routes![index, index_post])
        .launch();
}
